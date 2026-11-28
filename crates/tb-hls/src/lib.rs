//! Sans-io HLS input (SPEC §9.2): what an HLS or Low-Latency HLS pull needs short of the HTTP
//! client itself.
//!
//! - [`playlist`]: multivariant and media playlists (RFC 8216bis), including the LL-HLS tags
//!   `EXT-X-PART`, `EXT-X-PRELOAD-HINT`, `EXT-X-SERVER-CONTROL` and `EXT-X-PART-INF`.
//! - [`fmp4`]: a fragmented-MP4 (CMAF) demuxer: the init segment's tracks and codec
//!   configurations, and each fragment's samples.
//! - [`planner`]: which part, segment or playlist to request next, from where to start at the
//!   live edge, and when to fall back from parts to whole segments.
//!
//! MPEG-TS segments are left to `tb-tsin`. Decoding stays in the sandboxed worker: nothing
//! here touches libav.
#![forbid(unsafe_code)]

pub mod fmp4;
pub mod planner;
pub mod playlist;
