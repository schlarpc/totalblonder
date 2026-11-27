# Status: what is built and what is needed

As of 2026-11-26, morning.

The goal is a server that takes content pushed over the network, makes compliant MPEG-2
transport streams from it, and feeds a Blonder Tongue EQAM-420B so that cable-ready TVs show
numbered channels with a guide (`docs/SPEC.md`).

**Where it stands.** The server exists and runs (`docs/SERVER.md`): carriers of constant-rate
MPTS with exact PCR, PAT/PMT and the cable PSIP tables (channel map, clock, guide, ratings),
MPEG-2 video from the GPU encoder, AC-3 audio from our own encoder with loudness control,
captions from subtitle files, UDP/RTP output paced to the slot clock, a supervisor of the
modulator's web forms, metrics over HTTP, and inputs over RTMP and MPEG-TS/UDP/RTP whose
decoding runs in a sandboxed child process. It has run in real time on nixark's card: three
carriers and seven programs with audio, subtitles and the statmux; and an RTMP producer
switched in and out of a program, picture and sound, with the independent checker finding
nothing at any severity (`docs/SERVER.md` §7). **Nothing has been sent to the modulator yet**:
its GigE input port is not reachable from the hosts (`docs/FIRST-AIR.md`).

Terms used below:

- **Elementary stream:** the coded video (or audio) of one program, with no container.
- **Transport stream:** the container a modulator and a TV expect.
- **Mux:** the component that builds a transport stream.
- **Gate:** our automated test run (`tools/gate/`), and `nix flake check` for everything without a GPU.

## 1. Built

### 1.1 Video encode (MPEG-2, on the GPU)

Crates: `tb-m2v-syntax`, `tb-m2v-rate`, `tb-m2v-encoder`, `tb-device`, `tb-device-gpu`,
`tb-device-ref`, `tb-kernel-abi`, `tb-engine`, `tb-m2venc`; kernels in `crates/tb-kernel-abi/kernels/`. Unchanged
tonight apart from captions from text (`--cc-text`).

| Capability | State | Evidence |
|---|---|---|
| Formats | 1080i and 1080p at 29.97, 720p at 59.94, 704×480i at 29.97; others refused at open | tests and gate |
| Coding tools | I, P and B pictures, field and frame prediction, adaptive quantization | `docs/ARCHITECTURE.md` |
| Rate control | Constant rate, and a ceiling with a variable rate under it; the decoder buffer limit is never broken; rate changes mid-stream under one sequence | Gate torture cases |
| Film mode | 24 fps sources coded as film frames with display flags (not yet used by the server) | tests and gate |
| Captions | Caption bytes per frame validated and written; text captions from SubRip/WebVTT through `tb-captions` | Gate caption case |
| Capacity, card to itself | 67 × 1080i, 75 × 1080p, 90 × 720p, 400 × 480i | `docs/SPEC.md` §14.x |
| AMD cards (HIP) | Same kernels, same output: golden cases and float parity identical on an RX 7900 XTX; a kernel fault is `Lost` as on CUDA (core files turned off for that); capacity not measured | `docs/ARCHITECTURE.md` §5.2, §7.3 |

### 1.2 Input pipeline (on the GPU) and timeline

Crates: `tb-decode`, `tb-convert`, `tb-timeline`, `tb-pipe` (harness). The timeline's mapper,
frame clock and fallback selector are used by the server (`source.rs`).

### 1.3 Transport stream, tables, output

| Crate | What | Evidence |
|---|---|---|
| `tb-ts` | Packets, adaptation fields, PCR and 33-bit stamps, PES headers and cursor, sections with CRC-32/MPEG-2, packetizer and reassembler (H.222.0) | unit tests |
| `tb-psip` | PAT, PMT, CVCT, MGT, STT, EIT, ETT and descriptors (AC-3, ISO 639, caption service, content advisory, service location, extended channel name, AVC) from one validated lineup; guide windows; versions that change only with content; US ratings limited to CTA-766's permitted messages; GPS time and US DST | TSDuck decodes every table as intended |
| `tb-mux` | Constant-rate MPTS per carrier: slot clock with exact PCR, T-STD admission (transport, system, smoothing, AC-3 buffers), video by `DTS − D` through token buckets, audio by PTS, late-AU policy, table carousel with STT | 20 s simulations; TSDuck: 0 PCR over 1 µs jitter, rate exactly 38.8 Mbit/s, 0 continuity errors |
| `tb-output` | UDP and RTP framing, UDP (multicast with interface and TTL) and file sinks, the pacer that owns time and covers a late mux with nulls | real time on nixark: 99.8 % of datagrams within 50 µs |
| `tb-ts-verify` | Independent checker (147 rules): TR 101 290, cable table rules, PSIP cross-checks, T-STD; library and CLI | agrees with TSDuck; finds nothing in our streams, offline or real time |
| `tb-statmux` | A carrier's video pool by complexity, lower before raise, never overcommitted | simulation tests |

### 1.4 Audio

| Crate | What | Evidence |
|---|---|---|
| `tb-ac3` | AC-3 encoder (A/52) in Rust: block switching, rematrixing, exact bit allocation; 1/0 to 3/2+LFE, 32–448 kbit/s; silent frame | 7,392 frames decoded by ffmpeg (strictest) and liba52 without error |
| `tb-audio` | Resampler with trimmed ratio, A/52 channel order and downmix, BS.1770-4 meter, −24 LKFS normalizer, −2 dBTP limiter, the sample clock of exact 1536-sample frames | inputs at −10/−24/−35 LKFS out at −24.0 |

### 1.5 Inputs and decoding

| Crate | What | Evidence |
|---|---|---|
| `tb-rtmp` | RTMP server for publishers (handshake, chunks, AMF0, publish flow, FLV and Enhanced RTMP: H.264, HEVC, AV1, VP9, AAC) | ffmpeg, GStreamer and librtmp publishers |
| `tb-tsin` | MPEG-TS over UDP or RTP: reordering, PAT/PMT/PES demux | ffmpeg streams checked against ffprobe |
| `tb-sandbox`, `tb-decoder-proto`, `tb-avworker`, `tb-decoder-client` | libavcodec in a confined child process (user, mount, PID, network, IPC, UTS namespaces; Landlock; seccomp in kill mode; rlimits), shared-memory rings, supervision, restarts and a circuit breaker; scaling and resampling inside the worker; NVDEC inside the sandbox | 13 of 13 layers on bagel and nixark; H.264/AAC bit-exact against ffmpeg; every injected fault classified |

### 1.6 The server (`totalblonder`)

Configuration (validated TOML), carriers, video hub on the CUDA or reference device, audio
thread, table builder with version persistence and a schedule file for the guide, slate,
live and standby sources through mapper/frame clock/fallback (live → standby → slate; MPEG-TS
files as looping standby inputs), film mode per program (24p sources as film pictures with
3:2 display flags; a steady 3,2 cadence measured from an RTMP source), a clean stop (the last
GOP whole, every queued access unit sent, then the output), RTMP and TS/UDP listeners, statmux, the
modulator supervisor (report by default), the continuous self-check (`tb-ts-verify` on every
carrier, findings in the metrics), control API (`/healthz`, `/api/metrics`, `/api/state`), offline mode on a virtual clock, clean stop on SIGTERM, a NixOS module.
Measured: `docs/SERVER.md` §7.

### 1.7 The modulator

| Item | State |
|---|---|
| `tb-eqam` | Typed client of the web forms: refusal screen enforced by types, parsers for every page, whole-form writes with read-back, four-module RF saves, supervisor with drift, write limits, reboot and defect-D bookkeeping |
| Live check | Every page of the live box parsed (2026-11-26); the supervisor run in report mode: drift and the boot alarm reported, nothing written |
| RF probe (`tb-probe`) | HDHomeRun replies parsed and judged; the adapter waits for the hardware |

## 2. Not built

| Area | What is needed |
|---|---|
| First air | Network, Manual mode and re-addressing done; next the first stream (`docs/FIRST-AIR.md` §2) |
| The first producer | The live path is proven with ffmpeg standing in for it (H.264 + AAC over RTMP, with the same ffmpeg RTMP client the producer uses); the producer itself has not been run against the server |
| RF probe adapter | `hdhomerun_config` driver and captures, when the HDHomeRun arrives |
| Emergency alerts | Not designed beyond the digest |
| H.264 output, flexible placement | Not designed |

## 3. Open decisions and unknowns

| Item | Needed from |
|---|---|
| How the host reaches the GigE ports; multicast or unicast (`docs/FIRST-AIR.md` §1) | Owner |
| Configuration Mode Manual before the first write (defect B) | Owner |
| Line-up: channels, formats, carriers | Owner |
| Where guide titles come from (the schedule file exists) | Owner |
| TV models; H.264 support | Owner |
| What the modulator tolerates (datagrams, jitter, rate ceiling, EIT pass-through) | First air |
| Caption field order on real TVs (ffmpeg's automatic mode misses ours) | First TV test |
| Film mode, captions and channel changes on TVs | First TV test |
