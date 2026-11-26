//! The supervisor's policy: when to restart a worker, when to stop trying, when to kill one,
//! and what its exit means. Pure state machines; time is an argument in milliseconds of a
//! monotonic clock ([`Millis`], any epoch) and randomness (the jitter) is a seeded generator.

use std::collections::VecDeque;

/// Milliseconds on a monotonic clock.
pub type Millis = u64;

/// Exit code of a worker that ran out of memory (its allocator failed, or libav said ENOMEM).
pub const EXIT_OOM: i32 = 86;
/// Exit code of a worker that found the parent's messages malformed.
pub const EXIT_PROTOCOL: i32 = 87;
/// Exit code of a sandboxed process whose set-up failed before it ran (also used by
/// `tb-sandbox` for its own stages).
pub const EXIT_SETUP: i32 = 88;

/// When to restart and when to give up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Delay before the first restart.
    pub initial_backoff_ms: Millis,
    /// Longest delay.
    pub max_backoff_ms: Millis,
    /// Each further consecutive failure multiplies the delay by this.
    pub multiplier: u32,
    /// The delay is drawn uniformly from `[d * (100 - jitter_pct) / 100, d]`.
    pub jitter_pct: u8,
    /// A worker that has been ready this long counts as healthy: the backoff starts over, and
    /// an open circuit closes.
    pub healthy_after_ms: Millis,
    /// The circuit opens after this many failures ...
    pub breaker_failures: u32,
    /// ... within this window.
    pub breaker_window_ms: Millis,
    /// An open circuit waits this long, then allows one trial start.
    pub breaker_cooldown_ms: Millis,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_backoff_ms: 100,
            max_backoff_ms: 10_000,
            multiplier: 2,
            jitter_pct: 20,
            healthy_after_ms: 10_000,
            breaker_failures: 5,
            breaker_window_ms: 60_000,
            breaker_cooldown_ms: 60_000,
        }
    }
}

/// Why the parent killed a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KillReason {
    /// Work was pending and nothing progressed for the stall time.
    Stall,
    /// A ping was not answered in time.
    Heartbeat,
    /// The worker was not ready in time after it was started.
    Startup,
    /// The worker said something that does not validate.
    Violation,
    /// A clean shutdown did not finish in time.
    ShutdownTimeout,
}

/// How a process ended, as `waitid` tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RawExit {
    /// `exit(code)`.
    Code(i32),
    /// Killed by a signal.
    Signal(i32),
}

/// What a worker's end means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitClass {
    /// Exit 0 after the parent asked it to shut down.
    Clean,
    /// An exit code nobody asked for (0 included).
    Code(i32),
    /// It ran out of memory (its address-space limit, or the system's).
    OutOfMemory,
    /// It exceeded its CPU-time limit (`SIGXCPU`).
    CpuLimit,
    /// The system-call filter killed it (`SIGSYS`): it tried something it must not.
    SeccompViolation,
    /// Any other signal (`SIGSEGV`, `SIGABRT`, `SIGBUS`, `SIGILL`, `SIGFPE` ...).
    Crash(i32),
    /// The parent killed it.
    Killed(KillReason),
    /// It never ran: the sandbox's set-up or the worker's initialization failed.
    SetupFailed,
    /// It found the parent's messages malformed (a bug on one side or the other).
    ProtocolError,
}

impl ExitClass {
    /// Whether this end counts as a failure (for backoff and the circuit breaker).
    pub fn is_failure(self) -> bool {
        self != Self::Clean
    }
}

/// Signal numbers (Linux, all architectures this runs on).
const SIGKILL: i32 = 9;
const SIGXCPU: i32 = 24;
const SIGSYS: i32 = 31;

/// What an exit means, given whether the parent killed the worker (and why) and whether it
/// asked for a shutdown.
pub fn classify(raw: RawExit, killed: Option<KillReason>, shutdown_requested: bool) -> ExitClass {
    match (raw, killed) {
        (RawExit::Signal(SIGKILL), Some(r)) => ExitClass::Killed(r),
        (RawExit::Signal(SIGSYS), _) => ExitClass::SeccompViolation,
        (RawExit::Signal(SIGXCPU), _) => ExitClass::CpuLimit,
        (RawExit::Signal(s), _) => ExitClass::Crash(s),
        (RawExit::Code(0), _) if shutdown_requested => ExitClass::Clean,
        (RawExit::Code(EXIT_OOM), _) => ExitClass::OutOfMemory,
        (RawExit::Code(EXIT_SETUP), _) => ExitClass::SetupFailed,
        (RawExit::Code(EXIT_PROTOCOL), _) => ExitClass::ProtocolError,
        (RawExit::Code(c), _) => ExitClass::Code(c),
    }
}

/// Where the supervisor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing running; the next [`Supervisor::poll`] starts one.
    Down,
    /// Started, not ready yet.
    Starting {
        /// When.
        since: Millis,
    },
    /// Ready.
    Running {
        /// Ready since.
        since: Millis,
    },
    /// Waiting to restart.
    Backoff {
        /// Until.
        until: Millis,
    },
    /// Too many failures: no restarts until then.
    Open {
        /// Until.
        until: Millis,
    },
    /// Shut down for good.
    Stopped,
}

/// What [`Supervisor::poll`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start a worker now (then call [`Supervisor::started`] or [`Supervisor::exited`]).
    Spawn,
    /// The worker has been healthy long enough: backoff reset, an open circuit closed.
    Healthy,
}

/// What follows a worker's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A restart at this time.
    RestartAt(Millis),
    /// The circuit is open: no restart before this time.
    CircuitOpen(Millis),
    /// No restart (clean end, or stopped).
    Stopped,
}

/// Counters of the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Workers started.
    pub spawns: u64,
    /// Of which restarts after a failure.
    pub restarts: u64,
    /// Ends by class.
    pub clean: u64,
    /// Unexpected exit codes.
    pub exit_code: u64,
    /// Out of memory.
    pub oom: u64,
    /// CPU limit.
    pub cpu_limit: u64,
    /// Killed by the system-call filter.
    pub seccomp: u64,
    /// Other signals.
    pub crash: u64,
    /// Killed for a stall.
    pub killed_stall: u64,
    /// Killed for a missed heartbeat.
    pub killed_heartbeat: u64,
    /// Killed for a slow start.
    pub killed_startup: u64,
    /// Killed for a protocol violation.
    pub killed_violation: u64,
    /// Killed after a shutdown that did not finish.
    pub killed_shutdown: u64,
    /// Set-up failures.
    pub setup_failed: u64,
    /// The worker found the parent's messages malformed.
    pub protocol_error: u64,
    /// Times the circuit opened.
    pub circuit_opens: u64,
}

impl Counters {
    fn count(&mut self, c: ExitClass) {
        let n = match c {
            ExitClass::Clean => &mut self.clean,
            ExitClass::Code(_) => &mut self.exit_code,
            ExitClass::OutOfMemory => &mut self.oom,
            ExitClass::CpuLimit => &mut self.cpu_limit,
            ExitClass::SeccompViolation => &mut self.seccomp,
            ExitClass::Crash(_) => &mut self.crash,
            ExitClass::Killed(KillReason::Stall) => &mut self.killed_stall,
            ExitClass::Killed(KillReason::Heartbeat) => &mut self.killed_heartbeat,
            ExitClass::Killed(KillReason::Startup) => &mut self.killed_startup,
            ExitClass::Killed(KillReason::Violation) => &mut self.killed_violation,
            ExitClass::Killed(KillReason::ShutdownTimeout) => &mut self.killed_shutdown,
            ExitClass::SetupFailed => &mut self.setup_failed,
            ExitClass::ProtocolError => &mut self.protocol_error,
        };
        *n += 1;
    }
}

/// The restart state machine of one worker slot.
#[derive(Debug, Clone)]
pub struct Supervisor {
    policy: RestartPolicy,
    phase: Phase,
    /// Consecutive failures since the last healthy run.
    attempt: u32,
    /// Times of recent failures (within the breaker's window).
    failures: VecDeque<Millis>,
    /// The current run is a trial after an open circuit.
    trial: bool,
    /// The current run has been reported healthy.
    healthy: bool,
    rng: u64,
    counters: Counters,
}

impl Supervisor {
    /// A supervisor with nothing running; `seed` drives the jitter.
    pub fn new(policy: RestartPolicy, seed: u64) -> Self {
        Self {
            policy,
            phase: Phase::Down,
            attempt: 0,
            failures: VecDeque::new(),
            trial: false,
            healthy: false,
            // xorshift must not start at zero
            rng: seed | 1,
            counters: Counters::default(),
        }
    }

    /// The phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The counters.
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// What to do now.
    pub fn poll(&mut self, now: Millis) -> Option<Action> {
        match self.phase {
            Phase::Down => Some(Action::Spawn),
            Phase::Backoff { until } if now >= until => Some(Action::Spawn),
            Phase::Open { until } if now >= until => {
                self.trial = true;
                Some(Action::Spawn)
            }
            Phase::Running { since }
                if !self.healthy && now.saturating_sub(since) >= self.policy.healthy_after_ms =>
            {
                self.healthy = true;
                self.attempt = 0;
                self.trial = false;
                self.failures.clear();
                Some(Action::Healthy)
            }
            _ => None,
        }
    }

    /// The next time [`Supervisor::poll`] may have something to do.
    pub fn deadline(&self) -> Option<Millis> {
        match self.phase {
            Phase::Down => Some(0),
            Phase::Backoff { until } | Phase::Open { until } => Some(until),
            Phase::Running { since } if !self.healthy => {
                Some(since.saturating_add(self.policy.healthy_after_ms))
            }
            _ => None,
        }
    }

    /// A worker was started.
    pub fn started(&mut self, now: Millis) {
        if self.counters.spawns > 0 {
            self.counters.restarts += 1;
        }
        self.counters.spawns += 1;
        self.healthy = false;
        self.phase = Phase::Starting { since: now };
    }

    /// The worker is ready.
    pub fn ready(&mut self, now: Millis) {
        if let Phase::Starting { .. } = self.phase {
            self.phase = Phase::Running { since: now };
        }
    }

    /// No more restarts (shutdown).
    pub fn stop(&mut self) {
        self.phase = Phase::Stopped;
    }

    fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    /// The worker ended (or could not be started: [`ExitClass::SetupFailed`]).
    pub fn exited(&mut self, now: Millis, class: ExitClass) -> Outcome {
        self.counters.count(class);
        if self.phase == Phase::Stopped {
            return Outcome::Stopped;
        }
        if !class.is_failure() {
            self.phase = Phase::Stopped;
            return Outcome::Stopped;
        }
        // a run that was healthy has already reset the backoff in `poll`; one that ended
        // before anyone polled still counts as healthy
        if let Phase::Running { since } = self.phase
            && now.saturating_sub(since) >= self.policy.healthy_after_ms
        {
            self.attempt = 0;
            self.trial = false;
            self.failures.clear();
        }
        self.failures.push_back(now);
        while let Some(&t) = self.failures.front() {
            if now.saturating_sub(t) > self.policy.breaker_window_ms {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        let k = self.policy.breaker_failures.max(1) as usize;
        if self.trial || self.failures.len() >= k {
            self.trial = false;
            self.counters.circuit_opens += 1;
            let until = now.saturating_add(self.policy.breaker_cooldown_ms);
            self.phase = Phase::Open { until };
            return Outcome::CircuitOpen(until);
        }
        let mut d = self.policy.initial_backoff_ms;
        for _ in 0..self.attempt {
            d = d.saturating_mul(u64::from(self.policy.multiplier.max(1)));
            if d >= self.policy.max_backoff_ms {
                break;
            }
        }
        d = d.min(self.policy.max_backoff_ms);
        let j = d * u64::from(self.policy.jitter_pct.min(100)) / 100;
        if j > 0 {
            d -= self.next_random() % (j + 1);
        }
        self.attempt = self.attempt.saturating_add(1);
        let until = now.saturating_add(d);
        self.phase = Phase::Backoff { until };
        Outcome::RestartAt(until)
    }
}

/// The watchdog's times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchdogConfig {
    /// From start to `Ready`.
    pub startup_ms: Millis,
    /// Between pings.
    pub heartbeat_interval_ms: Millis,
    /// A ping not answered within this kills the worker.
    pub heartbeat_timeout_ms: Millis,
    /// With work pending, no progress for this long kills the worker.
    pub stall_ms: Millis,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            startup_ms: 5_000,
            heartbeat_interval_ms: 500,
            heartbeat_timeout_ms: 2_000,
            stall_ms: 2_000,
        }
    }
}

/// What the watchdog asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing.
    Ok,
    /// Send a ping with this number.
    Ping(u64),
    /// Kill the worker.
    Kill(KillReason),
}

/// Heartbeats, stalls and the start-up deadline of one running worker.
#[derive(Debug, Clone)]
pub struct Watchdog {
    cfg: WatchdogConfig,
    started: Millis,
    ready: bool,
    pending: bool,
    last_progress: Millis,
    next_ping: Millis,
    outstanding: Option<(u64, Millis)>,
    seq: u64,
}

impl Watchdog {
    /// A watchdog for a worker started at `now`.
    pub fn new(cfg: WatchdogConfig, now: Millis) -> Self {
        Self {
            cfg,
            started: now,
            ready: false,
            pending: false,
            last_progress: now,
            next_ping: now,
            outstanding: None,
            seq: 0,
        }
    }

    /// The worker is ready: heartbeats begin.
    pub fn ready(&mut self, now: Millis) {
        self.ready = true;
        self.last_progress = now;
        self.next_ping = now.saturating_add(self.cfg.heartbeat_interval_ms);
    }

    /// The worker did something useful (consumed input, returned a frame, finished a drain).
    pub fn progress(&mut self, now: Millis) {
        self.last_progress = now;
    }

    /// Whether the worker has work: packets in the ring, or an `End` not yet answered. Work
    /// that appears after idleness starts the stall clock afresh.
    pub fn set_pending(&mut self, pending: bool, now: Millis) {
        if pending && !self.pending {
            self.last_progress = now;
        }
        self.pending = pending;
    }

    /// An answer to a ping. An answer to no outstanding ping is a violation.
    pub fn pong(&mut self, seq: u64) -> Result<(), crate::pool::Violation> {
        match self.outstanding {
            Some((s, _)) if s == seq => {
                self.outstanding = None;
                Ok(())
            }
            _ => Err(crate::pool::Violation::Pong(seq)),
        }
    }

    /// What to do at `now`.
    pub fn check(&mut self, now: Millis) -> Verdict {
        if !self.ready {
            if now.saturating_sub(self.started) >= self.cfg.startup_ms {
                return Verdict::Kill(KillReason::Startup);
            }
            return Verdict::Ok;
        }
        if let Some((_, sent)) = self.outstanding
            && now.saturating_sub(sent) >= self.cfg.heartbeat_timeout_ms
        {
            return Verdict::Kill(KillReason::Heartbeat);
        }
        if self.pending && now.saturating_sub(self.last_progress) >= self.cfg.stall_ms {
            return Verdict::Kill(KillReason::Stall);
        }
        if self.outstanding.is_none() && now >= self.next_ping {
            self.seq += 1;
            self.outstanding = Some((self.seq, now));
            self.next_ping = now.saturating_add(self.cfg.heartbeat_interval_ms);
            return Verdict::Ping(self.seq);
        }
        Verdict::Ok
    }

    /// The next time [`Watchdog::check`] may have something to do.
    pub fn deadline(&self) -> Millis {
        if !self.ready {
            return self.started.saturating_add(self.cfg.startup_ms);
        }
        let mut d = match self.outstanding {
            Some((_, sent)) => sent.saturating_add(self.cfg.heartbeat_timeout_ms),
            None => self.next_ping,
        };
        if self.pending {
            d = d.min(self.last_progress.saturating_add(self.cfg.stall_ms));
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RestartPolicy {
        RestartPolicy {
            initial_backoff_ms: 100,
            max_backoff_ms: 1_000,
            multiplier: 2,
            jitter_pct: 0,
            healthy_after_ms: 5_000,
            breaker_failures: 4,
            breaker_window_ms: 10_000,
            breaker_cooldown_ms: 30_000,
        }
    }

    #[test]
    fn classification() {
        use ExitClass::*;
        assert_eq!(classify(RawExit::Code(0), None, true), Clean);
        assert_eq!(classify(RawExit::Code(0), None, false), Code(0));
        assert_eq!(classify(RawExit::Code(EXIT_OOM), None, false), OutOfMemory);
        assert_eq!(
            classify(RawExit::Code(EXIT_SETUP), None, false),
            SetupFailed
        );
        assert_eq!(
            classify(RawExit::Code(EXIT_PROTOCOL), None, false),
            ProtocolError
        );
        assert_eq!(classify(RawExit::Signal(31), None, false), SeccompViolation);
        assert_eq!(classify(RawExit::Signal(24), None, false), CpuLimit);
        assert_eq!(classify(RawExit::Signal(11), None, false), Crash(11));
        assert_eq!(
            classify(RawExit::Signal(9), Some(KillReason::Stall), false),
            Killed(KillReason::Stall)
        );
        // a SIGKILL nobody here sent (the OOM killer, an operator)
        assert_eq!(classify(RawExit::Signal(9), None, false), Crash(9));
    }

    #[test]
    fn exponential_backoff_resets_after_a_healthy_run() {
        let mut s = Supervisor::new(policy(), 1);
        assert_eq!(s.poll(0), Some(Action::Spawn));
        s.started(0);
        s.ready(10);
        assert_eq!(s.exited(20, ExitClass::Crash(11)), Outcome::RestartAt(120));
        assert_eq!(s.poll(119), None);
        assert_eq!(s.poll(120), Some(Action::Spawn));
        s.started(120);
        assert_eq!(s.exited(130, ExitClass::Crash(11)), Outcome::RestartAt(330));
        s.started(330);
        assert_eq!(s.exited(340, ExitClass::Crash(11)), Outcome::RestartAt(740));
        s.started(740);
        s.ready(750);
        // healthy after 5 s of running
        assert_eq!(s.poll(5_749), None);
        assert_eq!(s.poll(5_750), Some(Action::Healthy));
        assert_eq!(
            s.exited(6_000, ExitClass::Crash(6)),
            Outcome::RestartAt(6_100)
        );
        let c = s.counters();
        assert_eq!((c.spawns, c.restarts, c.crash), (4, 3, 4));
    }

    #[test]
    fn backoff_is_capped() {
        let mut p = policy();
        p.breaker_failures = 100;
        let mut s = Supervisor::new(p, 1);
        let mut now = 0;
        let mut last = 0;
        for _ in 0..20 {
            s.started(now);
            match s.exited(now, ExitClass::Code(1)) {
                Outcome::RestartAt(t) => {
                    last = t - now;
                    now = t;
                }
                o => panic!("{o:?}"),
            }
        }
        assert_eq!(last, 1_000);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let mut p = policy();
        p.jitter_pct = 50;
        p.breaker_failures = 1000;
        let mut s = Supervisor::new(p, 99);
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..200u64 {
            s.started(i);
            // reset the attempt count through a healthy run so every delay is the first one
            s.ready(i);
            let _ = s.poll(i + 5_000);
            if let Outcome::RestartAt(t) = s.exited(i + 5_000, ExitClass::Code(1)) {
                let d = t - (i + 5_000);
                assert!((50..=100).contains(&d), "{d}");
                seen.insert(d);
            }
        }
        assert!(seen.len() > 10);
    }

    #[test]
    fn circuit_breaker_opens_and_half_opens() {
        let mut s = Supervisor::new(policy(), 1);
        let mut now = 0;
        for i in 0..3 {
            s.started(now);
            let o = s.exited(now + 1, ExitClass::Crash(11));
            assert!(matches!(o, Outcome::RestartAt(_)), "{i}");
            now += 1_000;
        }
        s.started(now);
        assert_eq!(
            s.exited(now + 1, ExitClass::Crash(11)),
            Outcome::CircuitOpen(now + 1 + 30_000)
        );
        assert_eq!(s.poll(now + 30_000), None);
        assert_eq!(s.poll(now + 30_001), Some(Action::Spawn));
        // the trial fails at once: open again, without waiting for k failures
        s.started(now + 30_001);
        assert!(matches!(
            s.exited(now + 30_002, ExitClass::Crash(11)),
            Outcome::CircuitOpen(_)
        ));
        assert_eq!(s.counters().circuit_opens, 2);
        // a trial that becomes healthy closes the circuit
        let t = now + 100_000;
        assert_eq!(s.poll(t), Some(Action::Spawn));
        s.started(t);
        s.ready(t);
        assert_eq!(s.poll(t + 5_000), Some(Action::Healthy));
        assert_eq!(
            s.exited(t + 5_001, ExitClass::Crash(11)),
            Outcome::RestartAt(t + 5_101)
        );
    }

    #[test]
    fn failures_outside_the_window_do_not_count() {
        let mut s = Supervisor::new(policy(), 1);
        for i in 0..10u64 {
            s.started(i * 6_000);
            assert!(matches!(
                s.exited(i * 6_000 + 1, ExitClass::Crash(11)),
                Outcome::RestartAt(_)
            ));
        }
    }

    #[test]
    fn clean_exit_and_stop() {
        let mut s = Supervisor::new(policy(), 1);
        s.started(0);
        assert_eq!(s.exited(1, ExitClass::Clean), Outcome::Stopped);
        assert_eq!(s.poll(10_000), None);
        let mut s = Supervisor::new(policy(), 1);
        s.started(0);
        s.stop();
        assert_eq!(s.exited(1, ExitClass::Crash(11)), Outcome::Stopped);
        assert_eq!(s.counters().crash, 1);
    }

    fn wd() -> Watchdog {
        Watchdog::new(
            WatchdogConfig {
                startup_ms: 1_000,
                heartbeat_interval_ms: 100,
                heartbeat_timeout_ms: 300,
                stall_ms: 500,
            },
            0,
        )
    }

    #[test]
    fn startup_deadline() {
        let mut w = wd();
        assert_eq!(w.check(999), Verdict::Ok);
        assert_eq!(w.deadline(), 1_000);
        assert_eq!(w.check(1_000), Verdict::Kill(KillReason::Startup));
    }

    #[test]
    fn heartbeats() {
        let mut w = wd();
        w.ready(0);
        assert_eq!(w.check(99), Verdict::Ok);
        assert_eq!(w.check(100), Verdict::Ping(1));
        // one ping at a time
        assert_eq!(w.check(250), Verdict::Ok);
        assert!(w.pong(2).is_err());
        w.pong(1).unwrap();
        assert!(w.pong(1).is_err());
        assert_eq!(w.check(250), Verdict::Ping(2));
        assert_eq!(w.deadline(), 550);
        assert_eq!(w.check(549), Verdict::Ok);
        assert_eq!(w.check(550), Verdict::Kill(KillReason::Heartbeat));
    }

    #[test]
    fn stalls_only_with_work_pending() {
        let mut w = wd();
        w.ready(0);
        let mut now = 0;
        // idle for a long time, answering pings: fine
        while now < 5_000 {
            if let Verdict::Ping(s) = w.check(now) {
                w.pong(s).unwrap();
            }
            now += 50;
        }
        // work arrives; the stall clock starts now
        w.set_pending(true, now);
        let start = now;
        while now < start + 450 {
            if let Verdict::Ping(s) = w.check(now) {
                w.pong(s).unwrap();
            }
            now += 50;
        }
        w.progress(now);
        let p = now;
        loop {
            match w.check(now) {
                Verdict::Ping(s) => w.pong(s).unwrap(),
                Verdict::Kill(r) => {
                    assert_eq!(r, KillReason::Stall);
                    assert_eq!(now, p + 500);
                    break;
                }
                Verdict::Ok => {}
            }
            now += 50;
        }
    }
}
