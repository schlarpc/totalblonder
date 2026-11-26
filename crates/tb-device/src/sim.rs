//! Recording and replaying devices.
//!
//! [`Recorder`] wraps any device and keeps a [`Trace`]: every plan submitted, the bytes its
//! uploads read, the completion, and the bytes its downloads wrote. [`Replay`] is a device made
//! of a trace: it checks that the core submits the same plans with the same bytes, hands back
//! the recorded results, and can be told to misbehave ([`Fault`]). With it the tick state
//! machine, the deadline policy and the error isolation are tested deterministically, with no
//! kernel code and no hardware: record once (on the reference adapter in the sandbox, or on the
//! card), then replay with a completion that arrives late or a phase that fails.
//!
//! Around them:
//! - [`guard`]: the host-arena rule, checked by [`Replay`] and by [`Checked`] around any device;
//! - [`scenario`]: faults placed by tick and phase;
//! - [`normal`]: the normal form of a plan, for comparison with the golden `op` records;
//! - `golden` (feature `golden`): the trace a correct run of a golden case records, and the
//!   recorded plans in normal form.

pub mod guard;
pub mod normal;
pub mod scenario;

#[cfg(any(test, feature = "golden"))]
pub mod golden;

pub use guard::{Checked, HostGuard};
pub use normal::{NormalOp, OpKind, normal_form};
pub use scenario::{Phase, PlanTag, Scenario, ScenarioError, Select};

use crate::plan::{Op, Plan};
use crate::{ArenaSizes, Completion, Device, DeviceError, HostBuf, Nanos, Ticket};

/// One plan and everything that crossed the boundary because of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The plan as submitted.
    pub plan: Plan,
    /// The host regions the plan's uploads read, with their bytes at submission.
    pub uploads: Vec<(HostBuf, Vec<u8>)>,
    /// The completion (`None` while the plan is in flight).
    pub completion: Option<Completion>,
    /// The host regions the plan's downloads wrote, with their bytes at completion.
    pub downloads: Vec<(HostBuf, Vec<u8>)>,
}

/// A recorded run.
#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    /// Arena sizes of the recorded device.
    pub arenas: ArenaSizes,
    /// The plans in submission order.
    pub records: Vec<Record>,
}

fn upload_sources(plan: &Plan) -> impl Iterator<Item = HostBuf> + '_ {
    plan.steps.iter().filter_map(|s| match s.op {
        Op::Upload { src, .. } | Op::Upload2d { src, .. } | Op::UploadRefs { src, .. } => Some(src),
        _ => None,
    })
}

fn download_targets(plan: &Plan) -> impl Iterator<Item = HostBuf> + '_ {
    plan.steps.iter().filter_map(|s| match s.op {
        Op::Download { dst, .. } => Some(dst),
        _ => None,
    })
}

fn snapshot(host: &[u8], bufs: impl Iterator<Item = HostBuf>) -> Vec<(HostBuf, Vec<u8>)> {
    bufs.filter_map(|b| b.of(host).map(|s| (b, s.to_vec())))
        .collect()
}

/// A device that records what passes through another device.
#[derive(Debug)]
pub struct Recorder<D> {
    inner: D,
    trace: Trace,
    /// Ticket of the inner device -> index of its record.
    in_flight: Vec<(Ticket, usize)>,
}

impl<D: Device> Recorder<D> {
    /// Record everything submitted to `inner` from now on.
    pub fn new(inner: D) -> Self {
        let arenas = inner.arenas();
        Self {
            inner,
            trace: Trace {
                arenas,
                records: Vec::new(),
            },
            in_flight: Vec::new(),
        }
    }
    /// The trace so far and the wrapped device.
    pub fn finish(self) -> (Trace, D) {
        (self.trace, self.inner)
    }
}

impl<D: Device> Device for Recorder<D> {
    fn arenas(&self) -> ArenaSizes {
        self.inner.arenas()
    }
    fn host(&self) -> &[u8] {
        self.inner.host()
    }
    fn host_mut(&mut self) -> &mut [u8] {
        self.inner.host_mut()
    }
    fn now(&self) -> Nanos {
        self.inner.now()
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        let uploads = snapshot(self.inner.host(), upload_sources(plan));
        let ticket = self.inner.submit(plan)?;
        self.in_flight.push((ticket, self.trace.records.len()));
        self.trace.records.push(Record {
            plan: plan.clone(),
            uploads,
            completion: None,
            downloads: Vec::new(),
        });
        Ok(ticket)
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        let c = self.inner.wait(ticket, timeout);
        if c.status == Err(DeviceError::Timeout) {
            return c;
        }
        if let Some(i) = self.in_flight.iter().position(|(t, _)| *t == ticket) {
            let (_, r) = self.in_flight.swap_remove(i);
            if let Some(rec) = self.trace.records.get_mut(r) {
                rec.downloads = snapshot(self.inner.host(), download_targets(&rec.plan));
                rec.completion = Some(c.clone());
            }
        }
        c
    }
}

/// Something a [`Replay`] does differently from the recording, at the `submit`-th plan
/// (counted from 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The plan fails with this error; its downloads are not written.
    Fail {
        /// Which plan.
        submit: usize,
        /// The error its completion carries.
        error: DeviceError,
    },
    /// The plan takes this much longer than recorded (contention on the card).
    Delay {
        /// Which plan.
        submit: usize,
        /// Extra time.
        extra: Nanos,
    },
}

/// How a [`Replay`] compares a submitted plan with the recorded one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compare {
    /// Equal plans: every step, buffer and kernel argument.
    #[default]
    Exact,
    /// Equal [normal forms](normal::normal_form) in queue order: the same operations with the
    /// same sizes and launch dimensions; buffers and kernel arguments are not compared. For
    /// traces made from the golden records, which know no more than that.
    NormalForm,
    /// Equal normal forms stream by stream ([`normal::compare_per_stream`]).
    NormalFormPerStream,
}

/// A device that replays a [`Trace`].
///
/// It checks the host-arena rule as [`Checked`] does: a plan whose host regions overlap those
/// of a plan in flight is refused at `submit`, and a plan whose upload or download regions the
/// caller changed before its completion completes with [`DeviceError::BadPlan`] (its downloads
/// are then not written).
#[derive(Clone, Debug)]
pub struct Replay {
    trace: Trace,
    faults: Vec<Fault>,
    compare: Compare,
    guard: HostGuard,
    host: Vec<u8>,
    next: usize,
    clock: Nanos,
    /// Per record: time still to pass before it completes (set at submit).
    remaining: Vec<Option<Nanos>>,
}

impl Replay {
    /// Replay `trace` as recorded.
    pub fn new(trace: Trace) -> Self {
        Self::with_faults(trace, Vec::new())
    }
    /// Replay `trace` with faults.
    pub fn with_faults(trace: Trace, faults: Vec<Fault>) -> Self {
        let host = vec![0; usize::try_from(trace.arenas.host).unwrap_or(0)];
        let remaining = vec![None; trace.records.len()];
        Self {
            trace,
            faults,
            compare: Compare::Exact,
            guard: HostGuard::new(),
            host,
            next: 0,
            clock: Nanos(0),
            remaining,
        }
    }
    /// Compare submitted plans with the recorded ones this way (default [`Compare::Exact`]).
    pub fn compare_by(self, compare: Compare) -> Self {
        Self { compare, ..self }
    }
    /// Whether every recorded plan has been submitted.
    pub fn exhausted(&self) -> bool {
        self.next == self.trace.records.len()
    }
}

impl Device for Replay {
    fn arenas(&self) -> ArenaSizes {
        self.trace.arenas
    }
    fn host(&self) -> &[u8] {
        &self.host
    }
    fn host_mut(&mut self) -> &mut [u8] {
        &mut self.host
    }
    fn now(&self) -> Nanos {
        self.clock
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        let differs = |what| Err(DeviceError::BadPlan { step: 0, what });
        let Some(rec) = self.trace.records.get(self.next) else {
            return differs("replay: more plans than were recorded");
        };
        let same = match self.compare {
            Compare::Exact => rec.plan == *plan,
            Compare::NormalForm => {
                normal::compare(&normal_form(&rec.plan), &normal_form(plan)).is_ok()
            }
            Compare::NormalFormPerStream => {
                normal::compare_per_stream(&normal_form(&rec.plan), &normal_form(plan)).is_ok()
            }
        };
        if !same {
            return differs("replay: plan differs from the recording");
        }
        if rec.plan.wait != plan.wait {
            return differs("replay: plan waits on another stream than recorded");
        }
        self.guard.admit(plan)?;
        if rec
            .uploads
            .iter()
            .any(|(b, bytes)| b.of(&self.host) != Some(bytes.as_slice()))
        {
            return differs("replay: uploaded bytes differ from the recording");
        }
        let recorded = rec
            .completion
            .as_ref()
            .map_or(0, |c| c.finished.0.saturating_sub(c.submitted.0));
        let extra: u64 = self
            .faults
            .iter()
            .map(|f| match f {
                Fault::Delay { submit, extra } if *submit == self.next => extra.0,
                _ => 0,
            })
            .sum();
        self.remaining[self.next] = Some(Nanos(recorded + extra));
        let t = Ticket(self.next as u64);
        self.guard.track(t, plan, &self.host, true);
        self.next += 1;
        Ok(t)
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        let submitted = self.clock;
        let done = |status, finished| Completion {
            ticket,
            status,
            submitted,
            finished,
        };
        let i = usize::try_from(ticket.0).unwrap_or(usize::MAX);
        let Some(Some(left)) = self.remaining.get(i).copied() else {
            return done(Err(DeviceError::UnknownTicket), self.clock);
        };
        self.guard.check(ticket, &self.host);
        if let Some(t) = timeout
            && t < left
        {
            self.clock.0 += t.0;
            self.remaining[i] = Some(Nanos(left.0 - t.0));
            return done(Err(DeviceError::Timeout), self.clock);
        }
        self.clock.0 += left.0;
        self.remaining[i] = None;
        if let Some(v) = self.guard.release(ticket) {
            return done(Err(v), self.clock);
        }
        let fail = self.faults.iter().find_map(|f| match f {
            Fault::Fail { submit, error } if *submit == i => Some(error.clone()),
            _ => None,
        });
        if let Some(e) = fail {
            return done(Err(e), self.clock);
        }
        let rec = &self.trace.records[i];
        for (b, bytes) in &rec.downloads {
            if let Some(dst) = b.of_mut(&mut self.host) {
                dst.copy_from_slice(bytes);
            }
        }
        let status = rec.completion.as_ref().map_or(Ok(()), |c| c.status.clone());
        done(status, self.clock)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::selftest;

    /// A hand-made trace of the self-test case, as a correct device would record it.
    pub(crate) fn trace() -> Trace {
        let mut host = vec![0u8; selftest::ARENAS.host as usize];
        selftest::fill_host(&mut host).unwrap();
        let plan = selftest::plan();
        let uploads = snapshot(&host, upload_sources(&plan));
        let (d1, d2) = selftest::expected(&selftest::source());
        let mut targets = download_targets(&plan);
        let downloads = vec![
            (targets.next().unwrap(), d1.clone()),
            (targets.next().unwrap(), d2.clone()),
        ];
        drop(targets);
        let completion = Completion {
            ticket: Ticket(0),
            status: Ok(()),
            submitted: Nanos(1_000),
            finished: Nanos(251_000),
        };
        Trace {
            arenas: selftest::ARENAS,
            records: vec![Record {
                plan,
                uploads,
                completion: Some(completion),
                downloads,
            }],
        }
    }

    #[test]
    fn replay_returns_the_recorded_bytes() {
        let mut dev = Replay::new(trace());
        let out = selftest::run(&mut dev).unwrap();
        assert_eq!((out.d1, out.d2), selftest::expected(&selftest::source()));
        assert_eq!(out.completion.finished, Nanos(250_000));
        assert!(dev.exhausted());
    }

    #[test]
    fn replay_rejects_a_different_plan() {
        let mut dev = Replay::new(trace());
        selftest::fill_host(dev.host_mut()).unwrap();
        let mut p = selftest::plan();
        p.steps.pop();
        assert!(matches!(dev.submit(&p), Err(DeviceError::BadPlan { .. })));
    }

    #[test]
    fn a_failed_phase_is_a_value() {
        let fault = Fault::Fail {
            submit: 0,
            error: DeviceError::Failed { step: 8, code: 700 },
        };
        let mut dev = Replay::with_faults(trace(), vec![fault]);
        assert_eq!(
            selftest::run(&mut dev),
            Err(DeviceError::Failed { step: 8, code: 700 })
        );
    }

    #[test]
    fn a_late_completion_times_out_and_then_arrives() {
        let fault = Fault::Delay {
            submit: 0,
            extra: Nanos(4_000_000),
        };
        let mut dev = Replay::with_faults(trace(), vec![fault]);
        selftest::fill_host(dev.host_mut()).unwrap();
        let t = dev.submit(&selftest::plan()).unwrap();
        let c = dev.wait(t, Some(Nanos(1_000_000)));
        assert_eq!(c.status, Err(DeviceError::Timeout));
        assert_eq!(dev.now(), Nanos(1_000_000));
        let c = dev.wait(t, None);
        assert_eq!(c.status, Ok(()));
        assert_eq!(dev.now(), Nanos(4_250_000));
    }

    #[test]
    fn replay_checks_the_host_arena_rule() {
        // An upload source changed between submit and wait.
        let mut dev = Replay::new(trace());
        selftest::fill_host(dev.host_mut()).unwrap();
        let t = dev.submit(&selftest::plan()).unwrap();
        dev.host_mut()[0] ^= 1;
        let c = dev.wait(t, None);
        assert_eq!(
            c.status,
            Err(DeviceError::BadPlan {
                step: 5,
                what: guard::UPLOAD_CHANGED
            })
        );
        // A download target changed: reported, and the downloads are not written.
        let mut dev = Replay::new(trace());
        selftest::fill_host(dev.host_mut()).unwrap();
        let t = dev.submit(&selftest::plan()).unwrap();
        let (_, d2, _) = guard::host_regions(&selftest::plan())[4];
        d2.of_mut(dev.host_mut()).unwrap()[0] = 0xee;
        assert_eq!(
            dev.wait(t, None).status,
            Err(DeviceError::BadPlan {
                step: 10,
                what: guard::DOWNLOAD_CHANGED
            })
        );
        assert_eq!(d2.of(dev.host()).unwrap()[0], 0xee);
    }

    #[test]
    fn replay_refuses_a_plan_overlapping_one_in_flight() {
        let mut tr = trace();
        tr.records.push(tr.records[0].clone());
        let mut dev = Replay::new(tr);
        selftest::fill_host(dev.host_mut()).unwrap();
        let t = dev.submit(&selftest::plan()).unwrap();
        assert_eq!(
            dev.submit(&selftest::plan()),
            Err(DeviceError::BadPlan {
                step: 1,
                what: guard::OVERLAP
            })
        );
        assert_eq!(dev.wait(t, None).status, Ok(()));
        let t = dev.submit(&selftest::plan()).unwrap();
        assert_eq!(dev.wait(t, None).status, Ok(()));
        assert!(dev.exhausted());
    }

    #[test]
    fn normal_form_replay_ignores_buffers_and_arguments() {
        let mut p = selftest::plan();
        for s in &mut p.steps {
            if let Op::Launch(l) = &mut s.op
                && let crate::Kernel::Pyr(k) = &mut l.kernel
            {
                k.na = 1;
                k.d2 = crate::DevBuf::new(0, 4);
            }
        }
        let run = |cmp, plan: &Plan| {
            let mut dev = Replay::new(trace()).compare_by(cmp);
            selftest::fill_host(dev.host_mut()).unwrap();
            dev.submit(plan).map(|t| dev.wait(t, None).status)
        };
        assert!(run(Compare::Exact, &p).is_err());
        assert_eq!(run(Compare::NormalForm, &p), Ok(Ok(())));
        // Another grid is another normal form.
        let mut q = p.clone();
        if let Op::Launch(l) = &mut q.steps[8].op {
            l.grid = 2;
        }
        assert!(run(Compare::NormalForm, &q).is_err());
        // The upload stream's steps moved to the end: same per stream, not in queue order.
        let mut r = Plan::new();
        r.steps = p
            .steps
            .iter()
            .copied()
            .filter(|s| s.stream == crate::StreamId::MAIN)
            .collect();
        r.steps.extend(
            p.steps
                .iter()
                .copied()
                .filter(|s| s.stream != crate::StreamId::MAIN),
        );
        assert!(run(Compare::NormalForm, &r).is_err());
        assert_eq!(run(Compare::NormalFormPerStream, &r), Ok(Ok(())));
    }
}
