//! A pure-Rust AC-3 encoder: ATSC A/52:2012, main body (not E-AC-3), 48 kHz.
//!
//! ```text
//! 1536 samples/channel ─► transient detect ─► window + MDCT ─► rematrix (2/0)
//!   ─► exponents (strategy, D15/D25/D45, reuse) ─► parametric bit allocation (decoder-exact)
//!   ─► SNR offset search ─► quantize + group mantissas ─► pack syncframe, CRC1, CRC2
//! ```
//!
//! Sans-io: [`Encoder::encode_frame`] takes samples and returns bytes; there is no clock, file,
//! thread or global state. Each stage is a module with its own tests: [`bits`], [`crc`],
//! [`mdct`], [`transient`], [`exponents`], [`bitalloc`], [`quantize`], [`syntax`], and the
//! orchestration in [`encoder`]. [`parse`] reads frames back strictly and [`decode`] turns them
//! into PCM by the standard's equations, for tests.
//!
//! What the encoder does not use: coupling, delta bit allocation, dynamic range words, 1+1 mode.
//! Constants and rules carry the clause, table and page of A/52:2012 they come from.
//!
//! ```
//! use tb_ac3::{AudioCodingMode, BitRate, Config, Encoder};
//! let cfg = Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps192).unwrap();
//! let mut enc = Encoder::new(cfg);
//! let silence = [0.0f32; 1536];
//! let frame = enc.encode_frame(&[&silence, &silence]).unwrap();
//! assert_eq!(frame.len(), 768);
//! ```
#![forbid(unsafe_code)]

pub mod bitalloc;
pub mod bits;
pub mod config;
pub mod crc;
pub mod decode;
pub mod encoder;
pub mod exponents;
pub mod mdct;
pub mod parse;
pub mod quantize;
pub mod syntax;
pub mod tables;
pub mod transient;

pub use config::{
    AudioCodingMode, AudioProduction, Bandwidth, BandwidthCode, BitRate, BitstreamMode,
    CenterMixLevel, Config, ConfigBuilder, ConfigError, Dialnorm, DolbySurroundMode, MixLevel,
    RoomType, SampleRate, SurroundMixLevel,
};
pub use encoder::{Encoder, ExponentPlan, FrameStats, InputError, silent_frame};
pub use tables::FRAME_SAMPLES;
