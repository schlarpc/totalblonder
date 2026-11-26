//! The device as a protocol.
//!
//! The encoder core never calls CUDA. For each phase of a tick it writes a [`Plan`]: an ordered
//! list of operations on named buffers (fills, copies between the pinned host arena and the
//! device arena, kernel launches with typed argument records, events that order one stream
//! after another) and the stream whose completion ends the phase. An adapter executes the plan
//! and returns a [`Completion`]: success or a [`DeviceError`], and when it was submitted and
//! finished on the adapter's clock. Results (row statistics, picture sizes, coded pictures) come
//! back as bytes in the host arena at the places the plan's downloads named.
//!
//! Three adapters execute plans:
//! - `tb-device-cuda`: the CUDA driver API on a real card;
//! - `tb-device-ref`: the same kernel source compiled for the host, synchronous, bit-exact
//!   integer arithmetic, runs in the Nix sandbox;
//! - [`sim::Replay`] in this crate: replays a recorded trace and injects faults (late
//!   completion, device error), with no kernel code at all.
//!
//! What is static and what is dynamic:
//! - static: the two arenas (sizes fixed when the adapter is made), the set of kernels and their
//!   signatures ([`kernel`]), the streams and events ([`StreamId`], [`EventId`]), the order of
//!   operations inside a phase (straight-line code in the core, like the C++ `launch_begin`);
//! - dynamic: buffer placement inside the arenas ([`mem::ArenaAlloc`], pure arithmetic), which
//!   sessions take part (list buffers), counts, grid sizes.
//!
//! A plan is rebuilt every phase into a [`Plan`] that the caller keeps and clears, so the hot
//! path allocates nothing once the vectors have grown to their working size.
#![forbid(unsafe_code)]

pub mod kernel;
pub mod mem;
pub mod plan;
pub mod selftest;
pub mod sim;

pub use kernel::{Kernel, Launch};
pub use mem::{ArenaAlloc, DevBuf, HostBuf};
pub use plan::{EventId, Op, Plan, Step, StreamId};

/// A point on the adapter's monotonic clock, in nanoseconds. The origin is arbitrary; only
/// differences mean anything. Time is always an input to the core, never read by it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(pub u64);

/// Identifies one submitted plan until its completion has been taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(pub u64);

/// Sizes of the two arenas an adapter owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArenaSizes {
    /// Bytes of device memory.
    pub device: u64,
    /// Bytes of (pinned) host memory.
    pub host: u64,
}

/// Why a plan failed. Errors are values: the core turns one into a failure of the sessions or
/// batches the plan served (they repeat pictures on the host and are set up again), never into
/// an abort.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    /// The plan names memory outside an arena, a kernel the image does not have, or a stream or
    /// event the adapter does not have. A bug in the core; nothing was queued.
    #[error("invalid plan at step {step}: {what}")]
    BadPlan {
        /// Index of the offending step.
        step: usize,
        /// What is wrong with it.
        what: &'static str,
    },
    /// The device reported an error for an operation of this plan. `code` is the adapter's
    /// native code (a `CUresult` for CUDA). Later operations of the plan may not have run.
    #[error("device operation failed at step {step} (code {code})")]
    Failed {
        /// Index of the step that failed, or of the first step after an asynchronous failure
        /// was noticed.
        step: usize,
        /// The adapter's error code.
        code: i32,
    },
    /// The device context is gone (fallen off the bus, reset, uncorrectable error). Every buffer
    /// on the device is lost; the adapter must be made again.
    #[error("device lost (code {code})")]
    Lost {
        /// The adapter's error code.
        code: i32,
    },
    /// The plan had not finished when the wait gave up. It is still in flight: the caller waits
    /// again with the same ticket, and plans queued after it stay behind it.
    #[error("not finished within the wait")]
    Timeout,
    /// The ticket does not name a plan in flight.
    #[error("unknown ticket")]
    UnknownTicket,
}

/// The result of one plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// The plan this is about.
    pub ticket: Ticket,
    /// Whether every operation ran.
    pub status: Result<(), DeviceError>,
    /// When the adapter finished queueing the plan.
    pub submitted: Nanos,
    /// When the adapter saw the plan's wait stream finish (or gave up waiting).
    pub finished: Nanos,
}

/// What executes plans. Implemented by the adapters; the core never sees it. The driver at the
/// edge runs the loop: core writes a plan, `submit`, `wait`, core reads the completion.
///
/// Rule for the host arena: a region named by an upload or download of a plan in flight is not
/// touched through [`Device::host_mut`] until that plan has completed. Adapters do not check
/// the rule; [`sim::Replay`] does, and [`sim::Checked`] adds the check to any device.
pub trait Device {
    /// Sizes of the arenas.
    fn arenas(&self) -> ArenaSizes;
    /// The host arena, for reading results after a completion.
    fn host(&self) -> &[u8];
    /// The host arena, for writing parameters and frames before a submit.
    fn host_mut(&mut self) -> &mut [u8];
    /// The adapter's clock.
    fn now(&self) -> Nanos;
    /// Queue every operation of the plan. Does not wait for the device. A plan that fails
    /// validation is rejected whole.
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError>;
    /// Wait until the plan's wait stream has finished, or until `timeout` has passed
    /// (`None`: no limit). A completion with [`DeviceError::Timeout`] leaves the plan in flight.
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion;
}
