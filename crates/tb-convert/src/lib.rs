//! Conversion planning: decoder surfaces (NV12 / P010) to the encoder's padded planar 4:2:0
//! input planes, on the device.
//!
//! One separable polyphase resampler does every geometric job (size change with antialiasing,
//! chroma siting, field lattices for interlaced sources and outputs, crop, destination
//! rectangle); what differs is a table of taps per axis. This crate computes the taps
//! ([`make_taps`]) and color matrices ([`make_csc`]), and decides which kernel launches a tick
//! needs ([`Converter`]); it emits them as [`ConvOp`]s, which go in front of the encoder's
//! steps in a device plan.
//!
//! Ported from the conversion path of the C++ CUDA spike (docs/ARCHITECTURE.md, "History"):
//! the host half (`pp_kernel`, `make_taps`, `ConvKey`, `ConvClassHost`, `Converter`,
//! `make_csc`) and `make_fit` of the pipeline harness. The kernels (`crates/tb-kernel-abi/kernels/pipe_kernels.h`)
//! are part of the device image and the host reference library (K2); [`Lowerer`] turns
//! [`ConvOp`]s into plan steps with the kernel records of `tb_device`.
#![forbid(unsafe_code)]

mod csc;
mod fit;
pub mod golden;
mod lower;
mod plan;
mod taps;

pub use csc::{Csc, make_csc};
pub use fit::{Fit, FitMode, make_fit};
pub use lower::{LowerError, Lowerer};
pub use plan::{
    Axis, BLOCK, CONV_JOB_BYTES, ClassId, ClassTaps, ConvBuf, ConvClass, ConvJob, ConvKey,
    ConvLaunch, ConvOp, Converter, Dest, DevAddr, OverlayPlanes, PRE_BLOCK_JOBS, PRE_JOB_BYTES,
    Plane, PreId, PreJob, RectJob, Source,
};
pub use taps::{Filter, Rows, TapSpec, Taps, make_taps, pp_kernel, pp_support};

/// What the conversion path refuses (the C++ ends the process in these cases).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    /// The filter's taps, rounded up to a multiple of four, outnumber the source lattice.
    #[error("resampler: source of {n_lat} samples is smaller than the filter ({nt} taps)")]
    SourceTooSmall {
        /// Samples of the source lattice.
        n_lat: u32,
        /// Taps per output.
        nt: u32,
    },
    /// A tap spec on which the C++ is undefined: a zero step, an empty lattice, a negative
    /// output count, a count beyond 2^24, or an output with no taps.
    #[error("tap spec out of range")]
    BadSpec,
    /// A conversion rectangle whose width is not a multiple of 8 or whose height, crop origin
    /// or crop size is odd.
    #[error("conversion rectangle {dw}x{dh}: width must be a multiple of 8, the rest even")]
    BadRect {
        /// Destination width.
        dw: u32,
        /// Destination height.
        dh: u32,
    },
    /// A source plane or pitch that is not a multiple of four bytes.
    #[error("source surface is not 4-byte aligned")]
    Unaligned,
}
