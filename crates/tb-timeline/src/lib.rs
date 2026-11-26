//! The input timeline (docs/SPEC.md §5.2 to §5.4, §9.5): placing a producer's frames on the
//! carrier's clock, choosing the frame for each output tick, and choosing the source.
//!
//! Pure state machines, evaluated by the caller with times as inputs (no function here reads a
//! clock):
//! - [`Mapper`]: input timestamps -> output times (affine map with skew estimate, sessions that
//!   end on discontinuities), SPEC §5.3;
//! - [`FrameClock`]: at each output tick, the newest mapped frame that is due: new, repeat, or
//!   nothing; drops and repeats are counted here and nowhere else, SPEC §5.4. It carries the
//!   hysteresis for equal-rate sources whose phase sits on a tick boundary;
//! - [`Fallback`]: live -> standby -> slate, with hold and promotion times, cuts on GOP starts,
//!   SPEC §9.5;
//! - helpers for the output cadence and input time bases: [`Cadence`] (exact frame times,
//!   SPEC §5.2), [`Divider`] (a format's clock as a divisor and phase of the field-rate tick, as
//!   in the C++ harness), [`Unwrapper`] (33-bit PTS and 32-bit RTMP timestamps made continuous).
//!
//! The pipeline harness's decisions, ported from the C++ spike operation for operation (their
//! times are seconds in `f64` as in the C++, because offline runs must make the same decisions):
//! - [`select`]: which input frames and rows make each output frame of a channel (pulldown,
//!   weave, bob, repeat, slate), from the C++ harness `m2vpipe`;
//! - [`ingest`]: a decode thread's pacing gate (degrade, resynchronize), frame times and queue
//!   rules, from the C++ decoder.
//!
//! New with the port (the spike has neither):
//! - [`film`]: film mode, the selections per output field grouped into pictures that say how
//!   long they are shown (two or three fields; one, two or three frame periods), for the
//!   encoder to flag instead of coding the pulldown;
//! - [`captions`]: CEA-608/708 caption data carried through frame selection;
//! - [`admission`]: the hardware decoder's budget and the choice between hardware decode,
//!   software decode and refusal.
//!
//! Arithmetic: every time is an integer count of 27 MHz ticks ([`Ticks27`]); maps and cadences
//! use exact integer or rational arithmetic with 128-bit intermediates, so nothing drifts over
//! days. Floating point is used in one place only, the skew *estimate* of the mapper (a least
//! squares slope and a first-order filter, see `skew.rs`); its result is quantized to an integer
//! parts-per-billion value before it touches a time, and it is recomputed from a bounded window,
//! never integrated, so float rounding cannot accumulate.
//!
//! Work package: docs/port/T1-timeline.md.
#![forbid(unsafe_code)]

pub mod admission;
mod cadence;
pub mod captions;
mod fallback;
pub mod film;
mod frame_clock;
pub mod ingest;
mod mapper;
pub mod select;
mod skew;

pub use cadence::{Cadence, CadenceError, Divider, Unwrapper, to_pts90};
pub use fallback::Fallback;
pub use frame_clock::{FrameClock, MAX_PENDING};
pub use mapper::{Mapped, Mapper, MapperConfig};

/// 27 MHz ticks per second.
pub const HZ: i64 = 27_000_000;

/// Time on a 27 MHz clock (the carrier's STC, or an input's time base converted to it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticks27(pub i64);

impl Ticks27 {
    /// `ms` milliseconds.
    pub const fn from_ms(ms: i64) -> Self {
        Self(ms.saturating_mul(HZ / 1000))
    }
    /// `s` seconds.
    pub const fn from_secs(s: i64) -> Self {
        Self(s.saturating_mul(HZ))
    }
}

/// Identifies a decoded frame held by the caller (the core never holds pictures).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(pub u64);

/// What the frame clock chose for one output tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// A frame not shown before. Frames pushed before it that were never shown are dropped; the
    /// caller may release them.
    New(FrameId),
    /// The frame shown at the previous tick again.
    Repeat(FrameId),
    /// Nothing has ever been due (start, or after [`FrameClock::reset`]).
    Nothing,
}

/// Why the mapper ended a session (SPEC §5.3 "Discontinuity").
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SessionEnd {
    /// The transport connection closed.
    #[error("connection closed")]
    Closed,
    /// A timestamp went backwards.
    #[error("timestamp went backwards")]
    Backwards,
    /// Input timestamps and arrival times disagree by more than the reset threshold.
    #[error("timestamp step differs from arrival step")]
    Jump,
    /// The buffer depth left its range for longer than the resynchronization time.
    #[error("buffer depth out of range")]
    Depth,
    /// Nothing arrived for the live timeout.
    #[error("input stalled")]
    Stalled,
}

/// The sources of a program, best first (SPEC §9.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// The program's configured input.
    Live,
    /// A second input or a looping file.
    Standby,
    /// A still and silence; always available.
    Slate,
}

/// Saturate a 128-bit value into `i64`.
pub(crate) fn sat(v: i128) -> i64 {
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}
