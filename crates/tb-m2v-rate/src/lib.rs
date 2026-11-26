//! The buffer model and rate control of one session.
//!
//! - [`vbv`]: the operating model the encoder enforces and the mux relies on: a queue drained
//!   at the maximum of the session's rates over the last D seconds, with rate changes at picture
//!   times ("lower the encoder first and the mux D later; raise the mux first").
//! - [`model`]: bits as a function of the quantizer, from the histogram the transform kernel
//!   collects (the rho-domain model).
//! - [`control`]: picture targets (Test Model 5 step 1 anchored to the buffer), row quantizers,
//!   the correction passes and the ladder that never lets a picture overflow the buffer.
//!
//! Everything is arithmetic on values the caller passes in: time is the picture's index over the
//! frame rate, statistics are the records the kernels produced. Ported from the C++ encoder of the
//! CUDA spike (docs/ARCHITECTURE.md, "History"): `Vbv`, `RateControl`, `nz_at`, `Batch::choose`,
//! the rate control parts of `Batch::begin`, `quantize`, `retry`, `collect`, `host_repeat`.
//! Floating point: the port reproduces the C++ encoder's `double` arithmetic operation for
//! operation (same order, no fused multiply-add, `ln` where the C++ calls `std::log`), because
//! the golden vectors compare decisions exactly.
#![forbid(unsafe_code)]

pub mod control;
pub mod model;
pub mod vbv;

pub use control::RateControl;
pub use model::RateModel;
pub use vbv::Vbv;
