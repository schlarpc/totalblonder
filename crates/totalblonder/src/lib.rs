//! `totalblonder`: the server (docs/SPEC.md, docs/SERVER.md).
//!
//! | Module | What |
//! |---|---|
//! | [`config`] | The TOML file and its validation into a [`config::Plant`] |
//! | [`time`] | Process time: real, or virtual for offline runs |
//! | [`audio`] | The audio thread: sample clock, loudness, AC-3 per track |
//! | [`carrier`] | A carrier's mux thread and its queue |
//! | [`tables`] | Rebuilding every carrier's PSI/PSIP on a period |
//! | [`video`] | The video hub: the encoder device, one session per program |
//! | [`schedule`] | The guide's schedule file: events, descriptions, ratings |
//! | [`slate`] | The generated slate |
//! | [`source`] | A program's live input through mapper, frame clock and fallback |
//! | [`metrics`] | Counters |
//! | [`ingest`] | Input sockets: RTMP publishers, MPEG-TS over UDP/RTP |
//! | [`device`] | The modulator's supervisor over HTTP (report by default) |
//! | [`live`] | A live input's decoder thread: sandboxed decoder, mapper, to the program |
//! | [`control`] | Health, Prometheus metrics and state over HTTP |
//! | [`runtime`] | Threads, queues and clocks from a plant, real-time or offline |
#![forbid(unsafe_code)]

pub mod audio;
pub mod carrier;
pub mod config;
pub mod control;
pub mod device;
pub mod ingest;
pub mod live;
pub mod metrics;
pub mod runtime;
pub mod schedule;
pub mod slate;
pub mod source;
pub mod tables;
pub mod time;
pub mod video;
