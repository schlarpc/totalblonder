//! Audio between the decoder and the AC-3 encoder (docs/SPEC.md §5.4, §8.3, §9.5).
//!
//! The decoder hands over `f32` planar audio at any rate and channel count
//! (`tb_media::AudioFrameDesc`); the AC-3 encoder takes 1536 samples per channel at 48 kHz in
//! A/52 channel order, one frame every 32 ms, forever. Everything in between is here, as pure
//! state machines with time and samples as arguments (no clock, file, thread or global state):
//!
//! - [`resample`]: any rate from 8 to 192 kHz to 48 kHz, with a ratio trimmed continuously by up
//!   to ±500 ppm (Kaiser-windowed sinc at a 64.64 fixed-point position; better than −115 dB
//!   THD+N on sines; bit-transparent at 48 → 48 kHz with zero trim);
//! - [`layout`]: input layouts, the output layouts we encode (2/0 and 3/2 + LFE) in A/52 order
//!   (A/52 Table 5.8), and the mapping (A/52 §7.8.2 LoRo downmix, no synthetic upmix);
//! - [`loudness`]: an ITU-R BS.1770-4 meter at 48 kHz (momentary, short-term, gated integrated,
//!   4× true peak), validated against EBU Tech 3341 and ffmpeg;
//! - [`normalize`]: the live normalizer toward −24 LKFS (A/85 §6), a slow gain with no
//!   look-ahead;
//! - [`limiter`]: a look-ahead true-peak limiter at −2 dBTP;
//! - [`clock`]: the sample clock: blocks with output times (late, early, missing, overlapping)
//!   to exact 1536-sample frames at `P0 + 2880·j`, with 10 ms fades at the edges of silence,
//!   20 ms crossfades between sources, counters, and the buffer depth and drift that trim the
//!   resampler;
//! - [`pipeline`]: [`pipeline::TrackPipeline`], one output track composing all of the above for
//!   a live and a standby source, configured by a validated [`pipeline::TrackConfig`];
//! - [`signal`]: test signals and their analysis (tones, program-like noise, sine fit).
//!
//! Time is the 27 MHz [`tb_timeline::Ticks27`] of the timeline crate, converted exactly to
//! 48 kHz sample indices.
#![forbid(unsafe_code)]

pub mod clock;
mod fir;
pub mod layout;
pub mod limiter;
pub mod loudness;
pub mod normalize;
pub mod pipeline;
pub mod resample;
pub mod signal;

#[cfg(test)]
pub(crate) use signal as testsig;
