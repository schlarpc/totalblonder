//! The pipeline harness (work package G2): inputs decoded on the device or in software, frame
//! selection per output channel, conversion on the device, encoder sessions; the counterpart
//! of the C++ `m2vpipe` of the CUDA spike (docs/ARCHITECTURE.md, "History").
//!
//! The logic is in the sans-io crates: which frame and which rows a channel shows
//! (`tb_timeline::select`), what the decode thread does with a packet (`tb_timeline::ingest`),
//! captions through drops and repeats (`tb_timeline::captions`), whether an input may use the
//! hardware decoder (`tb_timeline::admission`), which conversions a tick needs and their
//! plan steps (`tb_convert`), and the encoder (`tb_m2v_encoder`). The adapters are
//! `tb_decode` (libav), `tb_device_gpu` and `tb_device_ref`. This crate is the edge that
//! joins them: the command line ([`opts`]), the run loop ([`run`]), the report ([`report`]),
//! the pictures the harness makes itself ([`pattern`]) and the hand-over of decoder surfaces
//! to the device adapter ([`dev`]).
//!
//! Device memory outside the arena: a decoded frame is a decoder surface. For the tick in
//! which a conversion reads it, its two planes are bound to the adapter as foreign regions
//! (`tb_device::kernel::DevRef::foreign`), the job records name them by slot, and the frame is
//! held until that tick's conversion plan has completed.

pub mod dev;
pub mod opts;
pub mod pattern;
pub mod report;
pub mod run;

pub use opts::{DeviceKind, Opts};
pub use run::{RunError, arena_sizes, run};
