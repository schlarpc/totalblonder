//! The sans-io core of the sandboxed decoder (`docs/ARCHITECTURE.md` §5.5).
//!
//! ```text
//!  server thread ── tb-decoder-client ──(SOCK_SEQPACKET: wire messages)──► tb-avworker
//!                       │   ▲                                               (libav, sandboxed
//!                       │   └──── output pools (memfd, fixed slots) ◄──────── child process)
//!                       └──────► input ring (memfd, packet bytes) ─────────►
//! ```
//!
//! Everything both sides must agree on, and every decision that can be made without I/O, is
//! here as plain data and pure functions:
//!
//! - [`wire`]: the versioned, length-checked binary messages of the control channel, both
//!   directions; decoding never trusts a length or a count it has not checked.
//! - [`ring`]: the byte ring that carries compressed packets to the worker, with credit
//!   (the worker's `Consumed`) as the only way space comes back.
//! - [`pool`]: the output pools of fixed slots and who owns each slot; the validation of every
//!   frame descriptor the worker sends against the slot it names. A [`pool::ValidSlot`] can only
//!   be obtained by that validation and is consumed when the slot is given back.
//! - [`supervise`]: the restart policy (exponential backoff with jitter, circuit breaker), the
//!   watchdog (heartbeats, stalls with work pending, start-up) and the classification of a
//!   worker's exit. Time is an argument ([`supervise::Millis`]).
//!
//! The worker is not trusted: anything it says that does not validate is a
//! [`pool::Violation`], and the client kills a worker that commits one.
#![forbid(unsafe_code)]

pub mod pool;
pub mod ring;
pub mod supervise;
pub mod wire;
