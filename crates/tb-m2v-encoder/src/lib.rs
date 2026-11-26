//! The encoder core: sessions, GOP structure, batches, and the state machine of a tick.
//!
//! Sans-io. The core takes frames, commands and the time as values, writes device plans
//! ([`tb_device::Plan`]) and reads completions; it returns coded pictures ([`AccessUnit`]).
//! It never calls a device, reads a clock or starts a thread. One driver loop at the edge (the
//! tick thread) runs:
//!
//! ```text
//! let mut step = enc.begin_tick(now, deadline, host, &mut plan);
//! loop {
//!     match step {
//!         Step::Done => break,
//!         Step::Run => ticket = dev.submit(&plan)?,
//!         Step::Wait => {}
//!     }
//!     let done = dev.wait(ticket, timeout);
//!     step = enc.resume(&done, dev.now(), dev.host_mut(), &mut plan);
//! }
//! while let Some(au) = enc.take_output(session) { ... }
//! ```
//!
//! The whole calling sequence, the plans of a tick and the handling of device errors are in
//! the documentation of [`tick`].
//!
//! Ported from the C++ encoder of the CUDA spike (docs/ARCHITECTURE.md, "History"). Modules and
//! their work packages:
//!
//! | Module | C++ | Work package |
//! |---|---|---|
//! | [`gop`] | `Session::enqueue`, `flush`, `Frame` | E1 |
//! | [`layout`] | `Batch::init` (geometry, buffer layout) | E2 |
//! | [`shed`] | `Encoder::plan` | E3 |
//! | [`tick`] | `Encoder::step`, `Batch::begin` .. `collect`, `host_repeat` | E4 |
#![forbid(unsafe_code)]

pub mod au;
pub mod config;
pub mod golden;
pub mod gop;
pub mod layout;
pub mod shed;
pub mod tick;

pub use au::AccessUnit;
pub use config::{EncoderOptions, SessionConfig};
// What a session's configuration names of the syntax crate.
pub use tb_m2v_syntax::profile::{Color, Profile};
pub use tb_m2v_syntax::{captions, profile};
pub use tick::{
    Encoder, EncoderError, FrameSource, Phase, SessionId, SessionSnapshot, SessionStats, Shown,
    ShownError, Step, TickStats,
};
