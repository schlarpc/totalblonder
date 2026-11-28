//! The device-owning loop: an adapter, the encoder and the plan between them.
//!
//! Everything that touches the device goes through [`Engine`]: the calling sequence of
//! `tb_m2v_encoder::tick` with a real adapter, real waits and the time each phase took. What
//! is above it (the run loops of `tb-m2venc` and `tb-pipe`: inputs, schedule, files, statistics) talks to
//! the encoder through commands and reads access units, and never sees a plan. That is the cut
//! along which this loop can move into a child process (docs/ARCHITECTURE.md §4.5): commands
//! and access units become messages, the host arena shared memory.

use std::time::{Duration, Instant};

use tb_device::{Completion, DeviceError, Nanos, Plan, StreamId, Ticket};
use tb_m2v_encoder::{Encoder, EncoderOptions, Phase, Step};

use crate::device::TickDevice;

/// Number of wait classes the engine keeps times for (`Phase::class` and the correction
/// passes).
pub const CLASSES: usize = 7;

/// Wait class of the plans queued ahead of a phase's last plan (split plans): they have
/// completed when they are waited for.
pub const CLASS_QUEUED: usize = 6;

/// Names of the wait classes, for the stage profile.
pub const CLASS_NAMES: [&str; CLASSES] = [
    "begin: search+transform",
    "quant+entropy",
    "finish: scatter+recon+download",
    "wait for the upload stream",
    "settle",
    "quant+entropy, correction passes",
    "parts queued ahead of a phase's last plan",
];

/// The tick cannot go on.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum EngineError {
    /// A plan did not complete within the limit; it is still in flight and nothing more can be
    /// queued behind it.
    #[error("the device did not finish a plan of phase {phase:?} within {seconds} s")]
    Hung {
        /// The phase of the plan.
        phase: Option<Phase>,
        /// The limit.
        seconds: f64,
    },
    /// The encoder asked for a wait with no plan in flight (a bug in the core).
    #[error("the encoder asked to wait with no plan in flight")]
    NoTicket,
}

/// Where the time of one tick went, in milliseconds (the C++ `tk_*` fields).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickTimes {
    /// Waiting for the previous tick's upload (the `Sync` plan).
    pub upwait: f64,
    /// From the end of that wait to the completion of `Begin`, of the last `Quant` plan, and
    /// of `Finish`.
    pub phase: [f64; 3],
    /// Building and queueing the `Begin` plan.
    pub begin: f64,
}

/// Sums over a run, per wait class.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClassTimes {
    /// Plans submitted.
    pub plans: u64,
    /// Host time in `submit`, seconds.
    pub submit: f64,
    /// Host time in `wait`, seconds.
    pub wait: f64,
    /// Device-side time, seconds (when timing is on).
    pub device: f64,
}

/// The adapter, the encoder and the plan.
#[derive(Debug)]
pub struct Engine<D> {
    dev: D,
    enc: Encoder,
    plan: Plan,
    /// How long one `wait` call may block.
    slice: Nanos,
    /// How long a plan may take in all.
    limit: Duration,
    /// Completions that carried an error other than a timeout, since the start.
    pub errors: u64,
    /// The first of them.
    pub first_error: Option<DeviceError>,
    /// Times per wait class since the start.
    pub classes: [ClassTimes; CLASSES],
    /// Host time between a completion and the next submit (the encoder's work between
    /// phases), seconds since the start.
    pub host_between: f64,
    /// Times of the last tick.
    pub times: TickTimes,
}

impl<D: TickDevice> Engine<D> {
    /// An encoder on this device.
    pub fn new(dev: D, opt: EncoderOptions, device_timeout: f64) -> Self {
        let blocks = dev.xf_blocks();
        Self::with_xf_blocks(dev, opt, device_timeout, blocks)
    }

    /// An encoder on this device whose transform kernel runs in thread blocks of `xf_blocks`
    /// 8x8 blocks (`EncoderOptions::xf_blocks`; [`crate::calibrate`] finds the fastest).
    pub fn with_xf_blocks(
        dev: D,
        mut opt: EncoderOptions,
        device_timeout: f64,
        xf_blocks: u32,
    ) -> Self {
        opt.xf_blocks = xf_blocks;
        let enc = Encoder::new(opt, dev.arenas());
        Self {
            dev,
            enc,
            plan: Plan::new(),
            slice: Nanos(1_000_000_000),
            limit: Duration::from_secs_f64(device_timeout.clamp(0.001, 86_400.0)),
            errors: 0,
            first_error: None,
            classes: [ClassTimes::default(); CLASSES],
            host_between: 0.0,
            times: TickTimes::default(),
        }
    }

    /// Gives the device back. Nothing may be in flight ([`Engine::settle`]).
    pub fn into_dev(self) -> D {
        self.dev
    }
    /// The encoder, for reading.
    pub fn enc(&self) -> &Encoder {
        &self.enc
    }
    /// The encoder, for commands between ticks.
    pub fn enc_mut(&mut self) -> &mut Encoder {
        &mut self.enc
    }
    /// The device.
    pub fn dev(&self) -> &D {
        &self.dev
    }
    /// The device.
    pub fn dev_mut(&mut self) -> &mut D {
        &mut self.dev
    }
    /// The host arena, for the caller's frames between ticks.
    pub fn host_mut(&mut self) -> &mut [u8] {
        self.dev.host_mut()
    }

    /// The C++ `Encoder::finish`: wait for the upload stream (with the overlapped upload),
    /// then for the kernel stream; the second plan carries the set-up of new batches.
    pub fn settle(&mut self) -> Result<(), EngineError> {
        let first = if self.enc.options().overlap {
            StreamId::UPLOAD
        } else {
            StreamId::MAIN
        };
        for st in [first, StreamId::MAIN] {
            let step = self.enc.settle(st, self.dev.host_mut(), &mut self.plan);
            self.drive(step, None)?;
        }
        Ok(())
    }

    /// One tick. `deadline`: when its output must exist, as an instant of the wall clock
    /// (ignored on a device whose clock is not the wall clock).
    pub fn tick(&mut self, deadline: Option<Instant>) -> Result<(), EngineError> {
        let now = self.dev.now();
        let deadline = deadline.filter(|_| self.dev.real_time()).map(|d| {
            let left = d.saturating_duration_since(Instant::now());
            Nanos(
                now.0
                    .saturating_add(u64::try_from(left.as_nanos()).unwrap_or(u64::MAX)),
            )
        });
        self.times = TickTimes::default();
        let t0 = Instant::now();
        let step = self
            .enc
            .begin_tick(now, deadline, self.dev.host_mut(), &mut self.plan);
        self.drive(step, Some(t0))
    }

    /// One tick whose deadline is `after` on the device's own clock from its start (a test
    /// switch: on the reference adapter the clock counts executed steps, so the deadline policy
    /// decides the same way in every run).
    pub fn tick_within(&mut self, after: Nanos) -> Result<(), EngineError> {
        let now = self.dev.now();
        let deadline = Some(Nanos(now.0.saturating_add(after.0)));
        self.times = TickTimes::default();
        let t0 = Instant::now();
        let step = self
            .enc
            .begin_tick(now, deadline, self.dev.host_mut(), &mut self.plan);
        self.drive(step, Some(t0))
    }

    /// Submit, wait, resume until the encoder is done. `tick`: when the tick began (`None`:
    /// outside a tick).
    fn drive(&mut self, mut step: Step, tick: Option<Instant>) -> Result<(), EngineError> {
        let ms = |a: Instant, b: Instant| 1e3 * b.duration_since(a).as_secs_f64();
        let mut ticket: Option<Ticket> = None;
        // Plans queued ahead of the one in `ticket` (split plans), and the first failure
        // among them.
        let mut ahead: [Option<Ticket>; 16] = [None; 16];
        let mut ahead_n = 0usize;
        let mut ahead_err: Option<DeviceError> = None;
        // The start of the tick's main part (after the wait for the upload stream), and the
        // end of the phase before the one in flight.
        let mut mark = tick;
        let mut last = tick.unwrap_or_else(Instant::now);
        let mut phase = None;
        let mut class = 0usize;
        let mut waiting_since = last;
        loop {
            match step {
                Step::Done => {
                    self.host_between += last.elapsed().as_secs_f64();
                    return Ok(());
                }
                Step::Run => {
                    phase = self.enc.phase();
                    class = usize::from(self.enc.wait_class().unwrap_or(0)).min(CLASSES - 1);
                    let t = Instant::now();
                    self.host_between += t.duration_since(last).as_secs_f64();
                    let r = self.dev.submit_class(&self.plan, class as u8);
                    let t1 = Instant::now();
                    let c = &mut self.classes[class];
                    c.plans += 1;
                    c.submit += t1.duration_since(t).as_secs_f64();
                    if phase == Some(Phase::Begin)
                        && let Some(m) = mark
                    {
                        self.times.begin = ms(m, t1);
                    }
                    last = t1;
                    waiting_since = t1;
                    match r {
                        Ok(t) => ticket = Some(t),
                        Err(e) => {
                            // A plan the adapter refuses is a failed completion to the core.
                            self.note(&e, phase);
                            let now = self.dev.now();
                            let done = Completion {
                                ticket: Ticket(u64::MAX),
                                status: Err(e),
                                submitted: now,
                                finished: now,
                            };
                            ticket = None;
                            step = self
                                .enc
                                .resume(&done, now, self.dev.host_mut(), &mut self.plan);
                            continue;
                        }
                    }
                }
                Step::Queue => {
                    phase = self.enc.phase();
                    let t = Instant::now();
                    self.host_between += t.duration_since(last).as_secs_f64();
                    let r = self.dev.submit_class(&self.plan, CLASS_QUEUED as u8);
                    let t1 = Instant::now();
                    let c = &mut self.classes[CLASS_QUEUED];
                    c.plans += 1;
                    c.submit += t1.duration_since(t).as_secs_f64();
                    last = t1;
                    match r {
                        Ok(t) if ahead_n < ahead.len() => {
                            ahead[ahead_n] = Some(t);
                            ahead_n += 1;
                        }
                        // More parts than the driver keeps tickets for: wait for this one now.
                        Ok(t) => {
                            let done = self.dev.wait(t, None);
                            if let Err(e) = done.status {
                                ahead_err.get_or_insert(e);
                            }
                        }
                        Err(e) => {
                            self.note(&e, phase);
                            let now = self.dev.now();
                            let done = Completion {
                                ticket: Ticket(u64::MAX),
                                status: Err(e),
                                submitted: now,
                                finished: now,
                            };
                            for t in ahead.iter_mut().take(ahead_n).filter_map(Option::take) {
                                let _ = self.dev.wait(t, None);
                            }
                            ahead_n = 0;
                            ahead_err = None;
                            step = self
                                .enc
                                .resume(&done, now, self.dev.host_mut(), &mut self.plan);
                            continue;
                        }
                    }
                    step = self.enc.queued(self.dev.host_mut(), &mut self.plan);
                    continue;
                }
                Step::Wait => {}
            }
            let Some(t) = ticket else {
                return Err(EngineError::NoTicket);
            };
            let w0 = Instant::now();
            let mut done = self.dev.wait(t, Some(self.slice));
            if done.status != Err(DeviceError::Timeout) {
                // The parts queued ahead ran before it on the same stream: they are complete.
                for t in ahead.iter_mut().take(ahead_n).filter_map(Option::take) {
                    let c = self.dev.wait(t, None);
                    if let Err(e) = c.status {
                        ahead_err.get_or_insert(e);
                    }
                }
                ahead_n = 0;
                if let Some(e) = ahead_err.take()
                    && done.status.is_ok()
                {
                    done.status = Err(e);
                }
            }
            let w1 = Instant::now();
            self.classes[class].wait += w1.duration_since(w0).as_secs_f64();
            match &done.status {
                Ok(()) => {
                    if let Some(d) = self.dev.device_time(t) {
                        self.classes[class].device += d.0 as f64 * 1e-9;
                    }
                }
                Err(DeviceError::Timeout) => {
                    if w1.duration_since(waiting_since) > self.limit {
                        return Err(EngineError::Hung {
                            phase,
                            seconds: self.limit.as_secs_f64(),
                        });
                    }
                }
                Err(e) => self.note(e, phase),
            }
            if done.status != Err(DeviceError::Timeout) {
                match (phase, mark) {
                    (Some(Phase::Sync), Some(m)) => {
                        self.times.upwait = ms(m, w1);
                        mark = Some(w1);
                    }
                    (Some(Phase::Begin), Some(m)) => {
                        self.times.phase[0] = ms(m, w1);
                        mark = Some(w1);
                    }
                    (Some(Phase::Quant), Some(m)) => {
                        // correction passes add to the same figure
                        self.times.phase[1] += ms(m, w1);
                        mark = Some(w1);
                    }
                    (Some(Phase::Finish), Some(m)) => self.times.phase[2] = ms(m, w1),
                    _ => {}
                }
                last = w1;
            }
            let now = self.dev.now();
            step = self
                .enc
                .resume(&done, now, self.dev.host_mut(), &mut self.plan);
        }
    }

    /// Count a failed plan of the caller's own (queued beside the tick's plans) with the
    /// tick's failures.
    pub fn device_error(&mut self, e: &DeviceError) {
        self.note(e, None);
    }

    fn note(&mut self, e: &DeviceError, phase: Option<Phase>) {
        self.errors += 1;
        if self.first_error.is_none() {
            self.first_error = Some(e.clone());
        }
        tracing::warn!(error = %e, ?phase, "a plan failed");
    }
}
