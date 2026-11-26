//! A sans-io RTMP server for publishers (Adobe RTMP specification 1.0, 2012), producing the
//! plain-data input events of `tb_media` (docs/SPEC.md §9.1; docs/ARCHITECTURE.md §1 rule 1).
//!
//! ```text
//!  bytes ─► handshake ─► chunk decoder ─► messages ─► server session ─► commands, metadata
//!  (§5.2)    (§5.3, limits)               (§5.4, §6, §7)  │
//!                                                         └─ audio/video ─► FLV tag parser ─►
//!                                              media assembler ─► tb_media::InputEvent
//! ```
//!
//! Layers, each usable on its own (a future pull/play client reuses all but [`server`]):
//! - [`handshake`]: C0/C1/C2 ↔ S0/S1/S2, simple handshake, both roles;
//! - [`chunk`]: chunk stream decoding and encoding, formats 0–3, extended timestamps, Set
//!   Chunk Size and Abort, with bounds on everything a peer can make it hold;
//! - [`amf0`]: AMF0 values, bounded decoding;
//! - [`message`]: protocol control, user control and command messages;
//! - [`flv`]: FLV audio/video tag bodies, legacy and Enhanced RTMP;
//! - [`media`]: tags into `Streams` / `Packet`s, the rule for announcing streams;
//! - [`server`]: the session state machine of one publishing connection, the acceptance
//!   policy, timeouts, the bounded output queue.
//!
//! The peer is hostile: no input panics, every allocation is bounded by a limit or by bytes
//! actually received, and violations end the session with `End(Protocol(..))`.
#![forbid(unsafe_code)]

pub mod amf0;
pub mod chunk;
pub mod flv;
pub mod handshake;
pub mod media;
pub mod message;
pub mod server;
mod types;
mod unwrap;

pub use server::{
    AllowAll, AllowList, CloseReason, PublishPolicy, Request, ServerConfig, ServerSession,
    SessionStats,
};
pub use types::{
    ChunkStreamId, InvalidChunkStreamId, Message, MessageStreamId, MessageType, RtmpTime,
};
pub use unwrap::MsUnwrapper;
