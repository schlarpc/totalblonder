//! Sans-io client and supervisor for the Blonder Tongue EQAM-420B web interface
//! (docs/SPEC.md §10; docs/notes/device.md; docs/notes/corruption-48.md).
//!
//! The crate never opens a socket and has no clock. It builds requests and reads responses;
//! a thin adapter in the server sends them through [`Transport`] and passes the time in.
//!
//! ```text
//!   DesiredState ─► Supervisor ──Job──► Client ──Request──► (adapter) ──► device
//!        (config)     ▲   │ Action         │ ▲                  │
//!                     │   ▼ (alarms,       │ └──Response────────┘
//!                     │    metrics)        ▼
//!                     └──────────── ClientEvent (parsed pages, write reports)
//! ```
//!
//! Layers, bottom up:
//!
//! * [`types`]: values that can only hold what the device accepts ([`types::StreamName`],
//!   [`types::StreamUrl`], [`types::Slot`], [`types::FirstChannel`], …).
//! * [`refusal`]: the hard refusal list. [`screen`] is the only constructor of [`Request`],
//!   the only thing a [`Transport`] sends.
//! * [`pages`]: typed parsers for every page read, with golden tests on the saved pages.
//! * [`mod@write`]: the two allowed writes as step machines; the QAM plan exists only as the
//!   four-module sequence.
//! * [`session`]: login, session id, Logged Out, backoff, idle timeout, pacing.
//! * [`client`]: one request at a time; reads retried once; jobs.
//! * [`supervisor`]: polls, drift, report/apply, write limits, reboot and event-log alarms,
//!   defect-D bookkeeping, metrics.
//! * [`controller`]: the supervisor and client wired together.
#![forbid(unsafe_code)]

mod channels;
pub mod client;
pub mod controller;
pub mod html;
pub mod pages;
pub mod plan;
pub mod refusal;
pub mod request;
pub mod session;
pub mod supervisor;
pub mod types;
pub mod write;

pub use refusal::{Refusal, RefusalRule, Request, RequestKind, screen};
pub use request::{
    Method, ReadPage, RequestDraft, Response, Transport, TransportFailure, WriteAuthority,
};
