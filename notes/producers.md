# Producers: what feeds the server

Survey of the first producer, a separate RTMP source, on 2026-11-24 (read-only). Paths are relative to its source tree.

## What the producer emits today

- **Video only.** H.264 High, yuv420p, default 1920x1080 at 30 fps. No audio at all; audio is
  its unfinished milestone M7 (`docs/SPEC.md:387-397`).
- **Encode** is in-process through `ffmpeg-the-third` (libavcodec), `libx264 veryfast` by
  default, `h264_vaapi` supported (`src/video/encoder.rs`, `src/video/hw.rs`).
- **GOP**: an IDR exactly every 2 s, up to 2 B-frames, SPS/PPS in extradata (global header).
- **Rate control**: CRF 23 by default. `bitrate_kbps` sets only an average. Nothing sets
  `maxrate`, `bufsize` or HRD, so the output is never rate-capped (`encoder.rs:107-140`).
- **Outputs** (`src/output/`):
  - LL-HLS over fMP4 from memory: `/live/index.m3u8`, 10 segments, 0.5 s parts,
    `PROGRAM-DATE-TIME` stamped at push time, not capture time.
  - RTMP push (FLV). Each connection starts at a keyframe with timestamps reset to 0, drops
    when its queue is full, and reconnects with backoff.
  - MPEG-TS over UDP and SRT are only a "later" line (`docs/SPEC.md:385`).
- **Timeline**: PTS is a tick counter that jumps forward across idle periods. While idle,
  no packets are sent. After a wake, real content returns about 15 s later.
- **Latency** is not a goal there: 15 s presentation delay, `tune=film`, B-frames.

## Demand

The producer sleeps when nobody watches (`src/demand.rs:65-79`). It stays awake if any of these
holds: `timing.idle_after = "0s"`; its RTMP sink is connected; an HLS request arrived in the
last 30 s. There is no explicit demand API.

## Consequences for the server

- Every feed from the producer must be re-encoded: it is not rate-capped and has no audio.
- The server must own the output timeline and survive timestamp resets and gaps.
- The server must supply audio (silent AC-3 at minimum) and a slate while a feed is absent.
- Preferred ingest: accept the producer's RTMP push and hold the connection. That is a push, it
  reconnects on its own, and it counts as demand for as long as we are connected.
- Cheapest change on the producer side, if wanted: a third sink that copies `rtmp.rs` with
  format `mpegts` to `udp://` or `srt://`.

## Conventions to reuse

- Template: `github.com/schlarpc/rust-flake` via cruft. Crane build, checks for build, clippy
  `--deny warnings`, fmt, nextest, llvm-cov. A hardened `nixosModules.default`.
- Runtime: tokio with `CancellationToken`; dedicated OS threads for blocking libav work,
  joined by bounded crossbeam channels; producers never block (`try_send`, count drops).
- Config: serde + toml with `deny_unknown_fields`, humantime durations, hot reload of a safe
  subset, an example file listing every key.
- Errors and logs: anyhow in `main`, thiserror in modules, tracing.
- Metrics: per-component atomic counters rendered by hand as Prometheus text.
- Lints: no `unwrap` outside tests; `unsafe` confined to named modules with SAFETY comments.
- Pure state machines separated from drivers so they can be unit-tested.
- `docs/SPEC.md`: numbered sections, milestones marked "(done YYYY-MM-DD)" with measured
  results, a spike log, an open-questions section.
