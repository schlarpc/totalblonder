//! The host-arena rule: a region named by an upload or download of a plan in flight is not
//! touched until that plan has completed.
//!
//! [`HostGuard`] keeps, per plan in flight, the host regions its uploads read and its downloads
//! write, with their bytes at submission. A plan that names a region overlapping one of a plan
//! in flight is refused; at the wait, bytes that differ from the snapshot are a violation. Both
//! are [`DeviceError::BadPlan`]: a bug in the core. [`Replay`](super::Replay) checks the rule
//! itself; [`Checked`] adds the check to any device.

use crate::plan::{Op, Plan};
use crate::{ArenaSizes, Completion, Device, DeviceError, HostBuf, Nanos, Ticket};

/// Which way a host region is used by a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    /// The device reads it (an upload's source).
    Upload,
    /// The device writes it (a download's destination).
    Download,
}

/// The host regions a plan names, with the index of the step and the use. A 2-D upload names
/// each of its rows (`width` bytes every `src_pitch`), not the gaps between them.
pub fn host_regions(plan: &Plan) -> Vec<(usize, HostBuf, Use)> {
    let mut out = Vec::new();
    for (i, s) in plan.steps.iter().enumerate() {
        match s.op {
            Op::Upload { src, .. } | Op::UploadRefs { src, .. } => {
                out.push((i, src, Use::Upload));
            }
            Op::Upload2d {
                src,
                src_pitch,
                width,
                rows,
                ..
            } => {
                for r in 0..u64::from(rows) {
                    let row = HostBuf::new(src.off + r * u64::from(src_pitch), u64::from(width));
                    out.push((i, row, Use::Upload));
                }
            }
            Op::Download { dst, .. } => out.push((i, dst, Use::Download)),
            Op::Fill { .. } | Op::Copy { .. } | Op::Launch(_) | Op::Record(_) | Op::WaitFor(_) => {}
        }
    }
    out
}

fn overlap(a: HostBuf, b: HostBuf) -> bool {
    a.len > 0 && b.len > 0 && a.off < b.end() && b.off < a.end()
}

/// The violation for a plan that names a region of a plan in flight.
pub const OVERLAP: &str = "host region overlaps one named by a plan still in flight";
/// The violation for an upload source changed while its plan was in flight.
pub const UPLOAD_CHANGED: &str = "host region of an upload changed while its plan was in flight";
/// The violation for a download destination changed while its plan was in flight.
pub const DOWNLOAD_CHANGED: &str = "host region of a download changed while its plan was in flight";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Region {
    step: usize,
    buf: HostBuf,
    what: &'static str,
    /// Bytes at submission; `None`: not compared (a download of an asynchronous device).
    bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Flight {
    ticket: Ticket,
    regions: Vec<Region>,
    /// The first violation seen, kept until the plan leaves (a wait that timed out returns
    /// [`DeviceError::Timeout`]; the violation comes with the completion).
    violation: Option<DeviceError>,
}

/// The host regions of the plans in flight and their bytes at submission.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostGuard {
    flights: Vec<Flight>,
}

impl HostGuard {
    /// No plan in flight.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a plan is tracked.
    pub fn is_empty(&self) -> bool {
        self.flights.is_empty()
    }

    /// Refuse a plan that names a host region overlapping one of a plan in flight.
    pub fn admit(&self, plan: &Plan) -> Result<(), DeviceError> {
        for (step, buf, _) in host_regions(plan) {
            if self
                .flights
                .iter()
                .flat_map(|f| &f.regions)
                .any(|r| overlap(r.buf, buf))
            {
                return Err(DeviceError::BadPlan {
                    step,
                    what: OVERLAP,
                });
            }
        }
        Ok(())
    }

    /// Track a submitted plan: snapshot its regions in `host` now. `downloads`: whether the
    /// download regions are compared too (only for a device that writes them during `submit`
    /// or during `wait`, never in between).
    pub fn track(&mut self, ticket: Ticket, plan: &Plan, host: &[u8], downloads: bool) {
        let regions = host_regions(plan)
            .into_iter()
            .map(|(step, buf, u)| {
                let compare = u == Use::Upload || downloads;
                Region {
                    step,
                    buf,
                    what: match u {
                        Use::Upload => UPLOAD_CHANGED,
                        Use::Download => DOWNLOAD_CHANGED,
                    },
                    bytes: if compare {
                        buf.of(host).map(<[u8]>::to_vec)
                    } else {
                        None
                    },
                }
            })
            .collect();
        self.flights.retain(|f| f.ticket != ticket);
        self.flights.push(Flight {
            ticket,
            regions,
            violation: None,
        });
    }

    /// Compare the regions of a plan in flight with `host`. Returns the plan's violation (the
    /// first one seen, now or at an earlier check), or `None`.
    pub fn check(&mut self, ticket: Ticket, host: &[u8]) -> Option<DeviceError> {
        let f = self.flights.iter_mut().find(|f| f.ticket == ticket)?;
        if f.violation.is_none() {
            f.violation = f.regions.iter().find_map(|r| {
                let now = r.buf.of(host);
                match &r.bytes {
                    Some(b) if now != Some(b.as_slice()) => Some(DeviceError::BadPlan {
                        step: r.step,
                        what: r.what,
                    }),
                    _ => None,
                }
            });
        }
        f.violation.clone()
    }

    /// Stop tracking a plan (it completed). Returns its violation, if any.
    pub fn release(&mut self, ticket: Ticket) -> Option<DeviceError> {
        let i = self.flights.iter().position(|f| f.ticket == ticket)?;
        self.flights.swap_remove(i).violation
    }
}

/// A device that checks the host-arena rule around another device.
///
/// `submit` refuses a plan whose host regions overlap those of a plan in flight; `wait` compares
/// the regions with their bytes at submission before it waits, and a completion whose regions
/// were changed carries [`DeviceError::BadPlan`] instead of its status (the inner plan is still
/// waited for, so that it leaves the inner device).
#[derive(Debug)]
pub struct Checked<D> {
    inner: D,
    guard: HostGuard,
    downloads: bool,
}

impl<D: Device> Checked<D> {
    /// Check uploads and downloads. For devices that write download regions only inside
    /// `submit` or `wait` (the reference adapter, [`Replay`](super::Replay)).
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            guard: HostGuard::new(),
            downloads: true,
        }
    }
    /// Check uploads, and downloads only for overlap: for devices that write download regions
    /// at any time while the plan is in flight (the CUDA adapter).
    pub fn uploads_only(inner: D) -> Self {
        Self {
            downloads: false,
            ..Self::new(inner)
        }
    }
    /// The wrapped device.
    pub fn inner(&self) -> &D {
        &self.inner
    }
    /// The wrapped device.
    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: Device> Device for Checked<D> {
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
        self.guard.admit(plan)?;
        let ticket = self.inner.submit(plan)?;
        // After the inner submit: a synchronous device has written its downloads by now.
        self.guard
            .track(ticket, plan, self.inner.host(), self.downloads);
        Ok(ticket)
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        self.guard.check(ticket, self.inner.host());
        let mut c = self.inner.wait(ticket, timeout);
        if c.status == Err(DeviceError::Timeout) {
            return c;
        }
        if let Some(v) = self.guard.release(ticket) {
            c.status = Err(v);
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::DevBuf;
    use crate::plan::StreamId;

    /// A synchronous device that copies uploads into a device array at submit and downloads at
    /// wait, with no kernels.
    struct Mem {
        host: Vec<u8>,
        dev: Vec<u8>,
        pending: Vec<(Ticket, Plan)>,
        next: u64,
    }

    impl Mem {
        fn new() -> Self {
            Self {
                host: vec![0; 256],
                dev: vec![0; 256],
                pending: Vec::new(),
                next: 0,
            }
        }
    }

    impl Device for Mem {
        fn arenas(&self) -> ArenaSizes {
            ArenaSizes {
                device: 256,
                host: 256,
            }
        }
        fn host(&self) -> &[u8] {
            &self.host
        }
        fn host_mut(&mut self) -> &mut [u8] {
            &mut self.host
        }
        fn now(&self) -> Nanos {
            Nanos(0)
        }
        fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
            plan.validate(self.arenas())?;
            for s in &plan.steps {
                if let Op::Upload { dst, src } = s.op {
                    let (d, n) = (dst.off as usize, dst.len as usize);
                    let from = src.of(&self.host).unwrap_or_default().to_vec();
                    self.dev[d..d + n].copy_from_slice(&from);
                }
            }
            let t = Ticket(self.next);
            self.next += 1;
            self.pending.push((t, plan.clone()));
            Ok(t)
        }
        fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
            let done = |status| Completion {
                ticket,
                status,
                submitted: Nanos(0),
                finished: Nanos(0),
            };
            if timeout == Some(Nanos(0)) {
                return done(Err(DeviceError::Timeout));
            }
            let Some(i) = self.pending.iter().position(|(t, _)| *t == ticket) else {
                return done(Err(DeviceError::UnknownTicket));
            };
            let (_, plan) = self.pending.swap_remove(i);
            for s in &plan.steps {
                if let Op::Download { dst, src } = s.op {
                    let (d, n) = (src.off as usize, src.len as usize);
                    let v = self.dev[d..d + n].to_vec();
                    if let Some(h) = dst.of_mut(&mut self.host) {
                        h.copy_from_slice(&v);
                    }
                }
            }
            done(Ok(()))
        }
    }

    /// Upload host 0..16 to device 0..16, download device 0..16 to host 64..80.
    fn copy_plan(up: u64, down: u64) -> Plan {
        let mut p = Plan::new();
        p.main(Op::Upload {
            dst: DevBuf::new(0, 16),
            src: HostBuf::new(up, 16),
        });
        p.main(Op::Download {
            dst: HostBuf::new(down, 16),
            src: DevBuf::new(0, 16),
        });
        p
    }

    #[test]
    fn a_correct_caller_passes() {
        let mut d = Checked::new(Mem::new());
        d.host_mut()[..16].copy_from_slice(&[7; 16]);
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        assert_eq!(d.wait(t, None).status, Ok(()));
        assert_eq!(&d.host()[64..80], &[7; 16]);
        // After the completion the regions are free again.
        d.host_mut()[..16].copy_from_slice(&[9; 16]);
        d.host_mut()[64] = 1;
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        assert_eq!(d.wait(t, None).status, Ok(()));
        assert_eq!(&d.host()[64..80], &[9; 16]);
        assert!(d.guard.is_empty());
    }

    #[test]
    fn changing_an_upload_source_in_flight_is_a_violation() {
        let mut d = Checked::new(Mem::new());
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        d.host_mut()[15] = 1;
        let c = d.wait(t, None);
        assert_eq!(
            c.status,
            Err(DeviceError::BadPlan {
                step: 0,
                what: UPLOAD_CHANGED
            })
        );
        assert!(d.guard.is_empty());
        assert!(
            d.inner().pending.is_empty(),
            "the inner plan was waited for"
        );
    }

    #[test]
    fn changing_a_download_target_in_flight_is_a_violation() {
        let mut d = Checked::new(Mem::new());
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        d.host_mut()[70] = 1;
        assert_eq!(
            d.wait(t, None).status,
            Err(DeviceError::BadPlan {
                step: 1,
                what: DOWNLOAD_CHANGED
            })
        );
        // The asynchronous variant does not compare download regions.
        let mut d = Checked::uploads_only(Mem::new());
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        d.host_mut()[70] = 1;
        assert_eq!(d.wait(t, None).status, Ok(()));
    }

    #[test]
    fn a_change_before_a_timeout_is_reported_with_the_completion() {
        let mut d = Checked::new(Mem::new());
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        d.host_mut()[0] = 1;
        assert_eq!(d.wait(t, Some(Nanos(0))).status, Err(DeviceError::Timeout));
        // Putting the byte back does not undo the violation.
        d.host_mut()[0] = 0;
        assert_eq!(
            d.wait(t, None).status,
            Err(DeviceError::BadPlan {
                step: 0,
                what: UPLOAD_CHANGED
            })
        );
    }

    #[test]
    fn a_plan_overlapping_one_in_flight_is_refused() {
        let mut d = Checked::new(Mem::new());
        let t = d.submit(&copy_plan(0, 64)).unwrap();
        // Its upload reads where the first plan downloads to.
        assert_eq!(
            d.submit(&copy_plan(72, 128)),
            Err(DeviceError::BadPlan {
                step: 0,
                what: OVERLAP
            })
        );
        // Its download writes where the first plan uploads from.
        assert_eq!(
            d.submit(&copy_plan(32, 15)),
            Err(DeviceError::BadPlan {
                step: 1,
                what: OVERLAP
            })
        );
        // Disjoint regions are fine; adjacent ones do not overlap.
        let u = d.submit(&copy_plan(16, 80)).unwrap();
        assert_eq!(d.wait(t, None).status, Ok(()));
        assert_eq!(d.wait(u, None).status, Ok(()));
    }

    #[test]
    fn a_2d_upload_names_its_rows_only() {
        let mut p = Plan::new();
        p.push(
            StreamId::UPLOAD,
            Op::Upload2d {
                dst: DevBuf::new(0, 64),
                dst_pitch: 16,
                src: HostBuf::new(100, 28),
                src_pitch: 10,
                width: 8,
                rows: 3,
            },
        );
        let r: Vec<_> = host_regions(&p).into_iter().map(|(_, b, _)| b).collect();
        assert_eq!(
            r,
            [
                HostBuf::new(100, 8),
                HostBuf::new(110, 8),
                HostBuf::new(120, 8)
            ]
        );
        let mut d = Checked::new(Mem::new());
        let t = d.submit(&p).unwrap();
        d.host_mut()[108] = 1; // between two rows: not read by the device
        assert_eq!(d.wait(t, None).status, Ok(()));
    }
}
