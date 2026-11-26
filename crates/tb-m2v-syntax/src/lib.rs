//! MPEG-2 video (ITU-T H.262) syntax that the host writes.
//!
//! The kernels write slices; everything above the slice layer is written here: sequence, GOP and
//! picture headers with their extensions, caption user data, and the all-skipped slices of a
//! repeated picture. The crate also owns the standard's tables in the form the kernels read
//! ([`tb_kernel_abi::Tables`]).
//!
//! Ported from the C++ encoder of the CUDA spike (docs/ARCHITECTURE.md, "History"): `HBits`,
//! `Batch::headers`, `Batch::skip_slices`, the table set-up in `Encoder::Encoder`, and
//! `kernels/common.h`. Section numbers refer to H.262
//! (02/2000). Work package: docs/port/S1-syntax.md.
#![forbid(unsafe_code)]

pub mod bits;
pub mod captions;
pub mod headers;
pub mod profile;
pub mod tables;
pub mod vlc;

pub use bits::BitWriter;
pub use tb_kernel_abi::{PIC_B, PIC_I, PIC_P};

/// picture_coding_type (6.3.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PicType {
    /// Intra.
    I = PIC_I,
    /// Predictive.
    P = PIC_P,
    /// Bidirectionally predictive.
    B = PIC_B,
}

impl PicType {
    /// The value written in the picture header and in `SessPar::pic_type`.
    pub fn code(self) -> u8 {
        self as u8
    }
}
