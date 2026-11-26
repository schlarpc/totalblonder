//! `tb-m2venc`: the command-line driver of the Rust encoder, as a library.
//!
//! The counterpart of the C++ `m2venc` of the CUDA spike (docs/ARCHITECTURE.md, "History"). The
//! encoder core is sans-io; this crate is its edge: the clock, the files, the choice of adapter and
//! the loop that runs ticks on it.
//!
//! | Module | What |
//! |---|---|
//! | [`opts`] | the command line of the C++ driver |
//! | [`run`] | the run: clips, sessions, the schedule of every frame, pacing, files |
//! | [`report`] | statistics, the text report, the `--json` record |
//! | [`clip`] | the deterministic test clips of the golden vectors |
//!
//! The binary (`main.rs`) parses, makes the adapter and calls [`run::run`]; tests call the
//! same function with the reference adapter.

pub mod clip;
pub mod opts;
pub mod report;
pub mod run;

pub use opts::{DeviceKind, Opts};
pub use run::{Outcome, RunError, Status, arena_sizes, run};
