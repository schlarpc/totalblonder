//! `tb-engine`: the device-owning loop, shared by the drivers (`tb-m2venc`, `tb-pipe`).
//!
//! | Module | What |
//! |---|---|
//! | [`engine`] | [`Engine`]: an adapter, the encoder and the plan; one tick at a time, waits and times |
//! | [`device`] | [`TickDevice`]: what the loop needs of an adapter; [`Guarded`]: the host-arena rule around any adapter |
//! | [`rng`] | the random sequences of the C++ drivers (`std::mt19937` and libstdc++'s distribution), for the drivers' schedules |
//!
//! Everything that touches the device goes through [`Engine`]; what is above it (inputs,
//! schedules, files, statistics) issues encoder commands between ticks and takes access units.
//! That is the cut along which the loop can move into a child process (docs/ARCHITECTURE.md
//! §4.5, notes/post-port-worklist.md).

pub mod device;
pub mod engine;
pub mod rng;

pub use device::{Guarded, TickDevice};
pub use engine::{Engine, EngineError};
