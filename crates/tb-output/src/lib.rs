//! The output adapter (`docs/SPEC.md` §4): datagram framing ([`frame`], pure), sinks
//! ([`sink`]: UDP unicast or multicast with interface and TTL, file, null) and the pacer
//! ([`pacer`]) that sends each carrier's slots at their times from a thread per interface.
#![forbid(unsafe_code)]

pub mod frame;
pub mod pacer;
pub mod sink;
