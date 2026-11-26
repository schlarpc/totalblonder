//! MPEG-2 transport stream input (ITU-T H.222.0), sans-io, producing the plain-data input
//! events of `tb_media` (docs/ARCHITECTURE.md §1 rule 1).
//!
//! ```text
//!  UDP datagrams ─► auto-detect ─┬─ raw TS ─────────────────────────┐
//!                                └─ RTP (RFC 3550/2250) ─► reorder ─┴─► demuxer ─► TsEvent
//!  byte stream (file, TCP, SRT) ─► framer (sync acquisition) ─────────┘   (InputEvent +
//!                                                                          loss flag)
//! ```
//!
//! - [`packet`]: transport packets, PIDs, the byte-stream framer;
//! - [`psi`]: sections, CRC, PAT, PMT, descriptors;
//! - [`pes`]: PES headers and 33-bit timestamps;
//! - [`codec`]: stream_type and descriptors to `CodecId`, key frames in the bitstream, ISO 639
//!   languages, Opus framing;
//! - [`demux`]: the demuxer for one program;
//! - [`rtp`]: RTP parsing and reordering;
//! - [`input`]: a UDP input with sessions and `End`.
//!
//! Independent of any TS *writer* in the workspace by design: input parsing faces hostile
//! bytes and must stay small and bounded; nothing here panics on any input.
#![forbid(unsafe_code)]

pub mod codec;
pub mod demux;
pub mod input;
pub mod packet;
pub mod pes;
pub mod psi;
pub mod rtp;
mod unwrap;

pub use demux::{DemuxConfig, DemuxStats, Demuxer, ProgramSelector, TsEvent};
pub use input::{Transport, TsInput, TsInputConfig, TsInputStats};
pub use packet::{InvalidPid, Pid};
pub use rtp::{RtpConfig, RtpReceiver, RtpStats};
pub use unwrap::Unwrap33;
