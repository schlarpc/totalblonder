//! MPEG-2 transport stream primitives, from ITU-T H.222.0 (06/2012), "Rec." below.
//!
//! Page numbers are the printed ones (the PDF page is about 14 higher), as in
//! `docs/notes/standards-digest.md` §1, which this crate implements.
//!
//! - [`Pid`], [`Cc`]: identifiers and continuity counters that cannot hold an illegal value;
//! - [`Stc`], [`Pcr`], [`Ts90k`]: the 27 MHz system clock, its PCR encoding and the 33-bit
//!   90 kHz time stamps;
//! - [`crc32_mpeg2`]: the section CRC of Annex A;
//! - [`packet`]: writing a 188-byte packet with an adaptation field (PCR, flags, stuffing) and
//!   reading one back;
//! - [`pes`]: PES headers (PTS/DTS) and a cursor that cuts one PES into packets;
//! - [`section`]: long-form sections with their CRC, the packetizer of a section PID
//!   (`pointer_field`, 0xFF stuffing) and a reassembler.
//!
//! Pure: no I/O, no clock. Writing never allocates beyond the section and PES buffers the
//! caller hands over.
#![forbid(unsafe_code)]

mod crc;
pub mod packet;
pub mod pes;
mod pid;
pub mod section;
mod time;

pub use crc::crc32_mpeg2;
pub use packet::{PACKET_LEN, Packet};
pub use pid::{Cc, Pid, PidError};
pub use time::{PCR_MODULUS, Pcr, STC_HZ, Stc, TS_MODULUS, Ts90k};
