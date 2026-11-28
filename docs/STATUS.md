# Status: what is built and what is needed

As of 2026-11-28.

The goal is a server that takes video from the network, makes compliant MPEG-2 transport
streams from it, and feeds a Blonder Tongue EQAM-420B so that cable-ready TVs show numbered
channels with a guide ([`SPEC.md`](SPEC.md)). How to use it is in [`GUIDE.md`](GUIDE.md).

**Where it stands.** The server runs in production on one plant: two carriers into an
EQAM-420B (one on each GigE port), with the statmux, `auto` audio and the modulator
supervisor in report mode. Every input type has been run end to end on the plant's GPU host
against real or simulated sources. What has not been checked yet is the picture on real TVs
and the RF output with a tuner: that waits for an HDHomeRun.

Terms used below:

- **Elementary stream:** the coded video (or audio) of one program, with no container.
- **Transport stream:** the container a modulator and a TV expect.
- **Mux:** the component that builds a transport stream.
- **Gate:** our automated test run (`tools/gate/`), and `nix flake check` for everything without a GPU.
- **nixark:** the plant's host, with an NVIDIA RTX 2000 Ada; most real-time measurements are
  from it. **bagel:** a development machine without a GPU.

## 1. Built

### 1.1 Video encode (MPEG-2, on the GPU)

Crates: `tb-m2v-syntax`, `tb-m2v-rate`, `tb-m2v-encoder`, `tb-device`, `tb-device-gpu`,
`tb-device-ref`, `tb-kernel-abi`, `tb-engine`, `tb-m2venc`; kernels in `crates/tb-kernel-abi/kernels/`.

| Capability | State | Evidence |
|---|---|---|
| Formats | 1080i and 1080p at 29.97, 720p at 59.94, 704×480i at 29.97; others refused at open | tests and gate |
| Coding tools | I, P and B pictures, field and frame prediction, adaptive quantization | `docs/ARCHITECTURE.md` |
| Rate control | Constant rate, and a ceiling with a variable rate under it; the decoder buffer limit is never broken; rate changes mid-stream under one sequence | Gate torture cases |
| Film mode | 24 fps sources coded as film frames with display flags (not yet used by the server) | tests and gate |
| Captions | Caption bytes per frame validated and written; text captions from SubRip/WebVTT through `tb-captions` | Gate caption case |
| Active Format Description and bar data | `DTG1` in every picture and `bar_data()` with every boxed one, from the rectangle the converter fitted the source into, numbered per SMPTE ST 2016-1 Table 2 (SPEC §7.7) | ST 2016-1 Annex B examples as tests; on nixark 4:3, 16:9 and 2.39:1 sources give 1001, 1010 and 0100 with their bars, `tb-m2v-verify` clean |
| Capacity, card to itself | 67 × 1080i, 75 × 1080p, 90 × 720p, 400 × 480i | `docs/SPEC.md` §14.x |
| AMD cards (HIP) | Same kernels, same output: golden cases and float parity identical on an RX 7900 XTX; a kernel fault is `Lost` as on CUDA (core files turned off for that); capacity not measured | `docs/ARCHITECTURE.md` §5.2, §7.3 |

### 1.2 Input pipeline (on the GPU) and timeline

Crates: `tb-decode`, `tb-convert`, `tb-timeline`, `tb-pipe` (harness). The timeline's mapper,
frame clock and fallback selector are used by the server (`source.rs`).

### 1.3 Transport stream, tables, output

| Crate | What | Evidence |
|---|---|---|
| `tb-ts` | Packets, adaptation fields, PCR and 33-bit stamps, PES headers and cursor, sections with CRC-32/MPEG-2, packetizer and reassembler (H.222.0) | unit tests |
| `tb-psip` | PAT, PMT, CVCT, MGT, STT, EIT, ETT, RRT and descriptors (AC-3, ISO 639, caption service, content advisory, service location, extended channel name, AVC) from one validated lineup; guide windows; versions that change only with content; US ratings limited to CTA-766's permitted messages, plus rating systems of our own sent as RRTs (SPEC §7.7); GPS time and US DST | TSDuck decodes every table as intended; the RRT and multi-region advisories checked by `tb-ts-verify` |
| `tb-mux` | Constant-rate MPTS per carrier: slot clock with exact PCR, T-STD admission (transport, system, smoothing, AC-3 buffers), video by `DTS − D` through token buckets, audio by PTS, late-AU policy, table carousel with STT | 20 s simulations; TSDuck: 0 PCR over 1 µs jitter, rate exactly 38.8 Mbit/s, 0 continuity errors |
| `tb-output` | UDP and RTP framing, UDP (multicast with interface and TTL) and file sinks, the pacer that owns time and covers a late mux with nulls | real time on nixark: 99.8 % of datagrams within 50 µs |
| `tb-ts-verify` | Independent checker (147 rules): TR 101 290, cable table rules, PSIP cross-checks, T-STD; library and CLI | agrees with TSDuck; finds nothing in our streams, offline or real time |
| `tb-statmux` | A carrier's video pool by complexity, lower before raise, never overcommitted; the pool moves for `auto` audio tracks (a smaller pool only once the mux fits it) | simulation tests; nixark, three live programs, 2026-11-28 |
| Step-down (`overload.rs`, `video.rs`) | Under sustained overload the lowest-priority program steps down its ladder (default to 480i): new session opened first, the old one ended with `sequence_end_code`, a gap of the new buffer delay, the mux and statmux reconfigured with the new stream, the decoder worker re-specced; back up after 5 min without loss (SPEC §12.1) | 96 × 1080i on nixark: steps every window, the switched stream conformant at the switch |

### 1.4 Audio

| Crate | What | Evidence |
|---|---|---|
| `tb-ac3` | AC-3 encoder (A/52) in Rust: block switching, rematrixing, exact bit allocation; 1/0 to 3/2+LFE, 32–448 kbit/s; silent frame | 7,392 frames decoded by ffmpeg (strictest) and liba52 without error |
| `tb-audio` | Resampler with trimmed ratio, A/52 channel order and downmix (to 5.1, stereo or mono), BS.1770-4 meter, −24 LKFS normalizer, −2 dBTP limiter, the sample clock of exact 1536-sample frames | inputs at −10/−24/−35 LKFS out at −24.0 |
| Second audio (`audio_routes`, `live.rs`) | Each track takes the input's audio stream in its language (or `input_track`), falling back to the main audio; one decode per stream, fanned out to the tracks it feeds | nixark, 2026-11-28: a TS input with `spa` 880 Hz listed before `eng` 440 Hz gave eng 440, spa 880, and a `fra` mono track the English audio; self-check silent |

### 1.5 Inputs and decoding

| Crate | What | Evidence |
|---|---|---|
| `tb-hls`, `hls_input.rs` | HLS and LL-HLS pull: playlists with parts, preload hints and date ranges, an fMP4 demuxer, the fetch planner (blocking reloads, live-edge start, whole-segment fallback, scheduled VOD by air time), audio renditions by language, the guide from `DATERANGE` | unit tests; nixark against a simulated live LL-HLS origin with a separate audio rendition, ffmpeg's live HLS (TS and fMP4), and a generated two-episode VOD channel |
| `tb-rtmp` | RTMP server for publishers (handshake, chunks, AMF0, publish flow, FLV and Enhanced RTMP v2: H.264, HEVC, AV1, VP9, AAC, Opus, MP3, AC-3, E-AC-3, multitrack audio as one stream per track) | ffmpeg, GStreamer and librtmp publishers; multitrack audio by unit tests (no local producer sends it) |
| `tb-tsin` | MPEG-TS over UDP or RTP: reordering, PAT/PMT/PES demux | ffmpeg streams checked against ffprobe |
| SRT (`ingest.rs`, `srt-tokio`) | MPEG-TS over SRT, one caller at a time, optional AES-128 passphrase; through the same demuxer as UDP | ffmpeg as an encrypted caller |
| `tb-sandbox`, `tb-decoder-proto`, `tb-avworker`, `tb-decoder-client` | libavcodec in a confined child process (user, mount, PID, network, IPC, UTS namespaces; Landlock; seccomp in kill mode; rlimits), shared-memory rings, supervision, restarts and a circuit breaker; scaling and resampling inside the worker; NVDEC inside the sandbox | 13 of 13 layers on bagel and nixark; H.264/AAC bit-exact against ffmpeg; every injected fault classified |

### 1.6 The server (`totalblonder`)

Configuration (validated TOML), carriers, video hub on the CUDA, HIP or reference device, audio
thread, table builder with version persistence, the guide from a schedule file or from HLS
date ranges, slate, live and standby sources through mapper, frame clock and fallback (live →
standby → slate), film mode per program, statmux and step-down under overload, the modulator
supervisor, the continuous self-check (`tb-ts-verify` on every carrier, findings in the
metrics), control API (`/healthz`, `/api/metrics`, `/api/state`), OpenTelemetry export,
offline mode on a virtual clock, a clean stop on SIGTERM (the last GOP whole, every queued
access unit sent), and a NixOS module. Measured: [`SERVER.md`](SERVER.md) §7.

Inputs: RTMP and Enhanced RTMP (multitrack audio included), SRT, MPEG-TS over UDP or RTP,
HLS and Low-Latency HLS (multivariant with audio renditions; scheduled VOD playlists that play
on their `PROGRAM-DATE-TIME` and give the guide from `DATERANGE`), and looping files.

### 1.7 The modulator

| Item | State |
|---|---|
| `tb-eqam` | Typed client of the web forms: refusal screen enforced by types (one bare admin read allowed), parsers for every page, whole-form writes with read-back, four-module RF saves, supervisor with drift, write limits, reboot, Manual-mode and defect-D bookkeeping |
| On the plant | Supervisor in report mode, polling as the box's read-only Guest; carriers 1 (GIGE1) and 9 (GIGE2) on air; the box's syslog sent to the log store; slot-to-FPGA mapping confirmed for both input FPGAs |
| Monitoring | Stream Info compared with each carrier's lineup; Diagnostics oversubscription and TS sync errors; absent inputs; every device metric exported (per-carrier bitrate, timestamp and TS sync errors, RF lock, temperature, active alarms); the event log forwarded as structured events |
| RF probe (`tb-probe`) | HDHomeRun replies parsed and judged; the adapter waits for the hardware |

## 2. Not built

| Area | What is needed |
|---|---|
| RF probe adapter | `hdhomerun_config` driver and captures, when the HDHomeRun arrives |
| Defect D mitigation | Being measured on the plant first (`EQAM-420B.md`) |
| Emergency alerts | Not designed beyond the digest |
| H.264 output | Not designed |
| E-AC-3 audio | Not planned: clear-QAM TVs need only decode AC-3 |

## 3. Open questions

| Item | Answered by |
|---|---|
| Defect D: what happens when its table overruns, and when | Measurement on the plant |
| Why the second input FPGA reports no bitrate (`notes/device.md` §11.3) | The RF probe: does slot 9's program reach its RF channel? |
| What the modulator tolerates (datagram sizes, jitter) | Measurement |
| Captions, film mode, channel changes, 5.1 ↔ 2.0 switches and rate changes on real TVs | First TV test |
| Which TVs on a plant decode H.264 over QAM | Per plant |
