//! The clock-driven multiplexer of one carrier (`docs/SPEC.md` §5.1, §6, §7.6).
//!
//! The output is a sequence of slots, one 188-byte packet each, at a constant rate; slot `n`
//! has a fixed STC ([`clock`]), so every PCR is exact by construction and the output never
//! stops or speeds up, whatever the inputs do: an empty slot is a null packet. Access units
//! arrive with PTS/DTS already on the carrier's STC; the mux decides only *when* their bytes
//! go, within the decoder buffer models ([`bucket`]) and the deadlines their time stamps imply.
//! Tables come from `tb-psip` and are repeated by the [`carousel`].
//!
//! Pure: time is the slot index; nothing here reads a clock or does I/O. A pacer (adapter)
//! sends slot `n` at `epoch + clock.nanos(n)`.
#![forbid(unsafe_code)]

pub mod bucket;
pub mod carousel;
pub mod clock;
mod mux;

pub use carousel::{Periods, SttConfig, WallClock};
pub use clock::{QAM256_PAYLOAD, SlotClock, TsRate};
pub use mux::{
    AudioAu, AudioConfig, ConfigError, Mux, MuxConfig, MuxStats, ProgramConfig, ProgramStats,
    PushError, VideoAu, VideoConfig, VideoReconfig, ts_rate_for,
};
