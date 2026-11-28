# The server: how it is put together

> Using the server (configuration, inputs, the modulator, monitoring) is in
> [`GUIDE.md`](GUIDE.md). This document is how it works inside.

State of 2026-11-26, with the inputs added since (SRT, HLS). `docs/SPEC.md` says what the server must do; `docs/ARCHITECTURE.md`
describes the workspace's rules and the encoder. This document describes the server built on
top of them: the crates of the stream path, the threads and clocks of the process, and where
the untrusted parts are isolated.

## 1. Data flow

```
 producer ───────► ingest thread ──InputEvent──► decoder worker ──frames──► program source
 (RTMP, SRT,       (tb-rtmp / tb-tsin /           (tb-avworker:              (tb-timeline:
  TS over UDP/RTP,  tb-hls: sans-io protocols)     libavcodec in a            mapper, frame
  HLS pull)                                        sandboxed child)           clock, fallback)
                                                                                   │
                     slate (generated) ─────────────────────────────────────────►─┤
                                                                                   ▼
 tables thread ──TableSet──┐                video hub thread: the encoder's device and Engine,
 (tb-psip, every 10 s)     │                one session per program, at each tick: picture of
                           ▼                presentation time T submitted at T − Δ
                   carrier queue ◄──VideoAu (PTS/DTS on the carrier's STC)───────┘
                           ▲
 audio thread ──AudioAu────┘  (tb-audio sample clock → tb-ac3)
                           │
                           ▼
                   mux thread (one per carrier): tb-mux, slot by slot ──Block──► pacer thread
                                                                                (one per output
                                                                                 interface)
                                                                                     │
                                                                           UDP / RTP datagrams,
                                                                           7 packets each, sent
                                                                           at their slot times
```

## 2. Crates of the stream path

| Crate | Kind | What |
|---|---|---|
| `tb-media` | pure | The contract between receivers, the decoder worker and the server: codecs, streams, packets, decoded-frame descriptors |
| `tb-rtmp` | pure | RTMP server for publishers: handshake, chunk streams, AMF0, the publish flow, FLV tags (legacy and Enhanced RTMP) into `tb-media` events |
| `tb-tsin` | pure | MPEG-TS input over UDP or RTP: reordering, PAT/PMT/PES demuxing into `tb-media` events (also used for SRT and for HLS's TS segments) |
| `tb-hls` | pure | HLS and LL-HLS: playlists (parts, preload hints, date ranges), a fragmented-MP4 demuxer, the fetch planner; the HTTP side is the server's `hls_input.rs` |
| `tb-ts` | pure | Transport stream primitives from H.222.0: PIDs, continuity counters, PCR and 33-bit time stamps, packets with adaptation fields, PES headers and a packet cursor, sections with CRC-32/MPEG-2, section packetizer and reassembler |
| `tb-psip` | pure | PAT, PMT, CVCT, MGT, STT, EIT, ETT and their descriptors, built from one validated lineup; guide windows; versions that change only with content; US rating messages limited to those CTA-766 permits |
| `tb-mux` | pure | One carrier's constant-rate multiplexer: slot clock with exact PCR, T-STD admission, table carousel, video by `DTS − D` through token buckets, audio by PTS, late-AU policy |
| `tb-output` | adapter | Datagram framing (UDP, RTP), sinks (UDP unicast/multicast with interface and TTL, file), the pacer |
| `tb-audio` | pure | Resampling with a trimmed ratio, channel mapping, BS.1770 loudness, the −24 LKFS normalizer and −2 dBTP limiter, the AC-3 sample clock |
| `tb-ac3` | pure | AC-3 encoder (A/52), with a strict parser and a reference decoder for tests |
| `tb-captions` | pure | SubRip/WebVTT cues to CEA-608 and CEA-708 bytes, per-picture `cc_data()` |
| `tb-ts-verify` | pure + CLI | Independent transport stream checker (TR 101 290, cable table rules, PSIP cross-checks, T-STD) |
| `tb-eqam` | pure | The modulator's web forms: refusal screen, typed pages, whole-form writes, supervisor |
| `tb-sandbox`, `tb-decoder-proto`, `tb-avworker` | adapter / pure / binary | The decoder worker: a confined child process running libavcodec, its wire protocol and shared-memory rings, and supervision |
| `totalblonder` | binary | Configuration, threads, clocks, inputs, slate, video hub, control API |

The encoder crates (`tb-m2v-*`, `tb-device*`, `tb-engine`, `tb-convert`, `tb-timeline`) are
described in `docs/ARCHITECTURE.md`.

## 3. Time

- **Process time** is 27 MHz ticks since the process epoch (`time.rs`). Every carrier's STC is
  its own constant offset (`stc0`) plus process time, and its pacer sends slot `n` at
  `epoch + n · 1504 / R`. So one clock drives every carrier, every PCR is exact by construction,
  and the relation of slot to STC never changes (SPEC §5.1).
- **Program cadence**: frame `k` is presented at `T_k = P0 + k · period`; the hub submits it at
  `T_k − Δ` (Δ = 1.1 s by default: encode budget, decoder buffer delay, margin). The encoder's
  90 kHz clock is `T_k / 300`, so its PTS/DTS move onto a carrier's STC by adding `stc0`.
- **Inputs**: a decoded frame that arrived at `r` is given to the mapper as if it had arrived at
  `r + Δ`, so the jitter buffer `J` is measured on the presentation timeline.
- **Two clocks** implement this: `RealClock` (monotonic) and `VirtualClock`, which advances only
  when the offline driver asks and every worker is waiting. With the virtual clock the whole
  server runs as fast as its work allows and writes each carrier to a file: deterministic, and
  usable on machines without the card (`totalblonder --offline SECONDS --out DIR`).

## 4. Threads

| Thread | Count | Blocks on | Owns |
|---|---|---|---|
| video hub | 1 | the device (GPU waits) | `Engine` (CUDA or reference adapter), sessions, frame providers |
| audio | 1 | nothing | per-track `tb-audio` pipelines and `tb-ac3` encoders |
| tables | 1 | nothing | lineups, version state |
| mux | 1 per carrier | nothing (1 ms sleep) | `Mux`, the carrier's queue, its gauges, the self-check (`tb-ts-verify` on every packet, SPEC §11.2) |
| pacer | 1 per interface | the deadline of the next datagram | sockets |
| ingest | 1 per input | the socket | the protocol session |
| control | 1 | the listener | reads atomics only |

Queues are bounded and never block a producer: a full queue drops and counts. The pacer never
waits for the mux: a datagram whose block has not arrived is sent as nulls, and the mux skips
the slots it missed (SPEC §4.3).

## 5. Isolation of libav

libav is not linked into the server. The decoder runs in `tb-avworker`, a child process
started by `tb-sandbox` in its own user, mount, PID, network, IPC and UTS namespaces, with a
read-only minimal root, no network, rlimits, Landlock and a seccomp allow-list installed after
libav has initialized. It receives `tb-media` events over a `SOCK_SEQPACKET` channel and returns
frames in sealed `memfd` rings; the parent validates every descriptor before touching a byte and
kills a worker that lies. A crash, hang or protocol violation becomes a typed event and a restart
with backoff; the program falls to its slate meanwhile. Containers and network protocols are
parsed in Rust, so the worker only ever sees elementary stream packets.

## 6. Running it

```
totalblonder --config FILE --check                     # validate only
totalblonder --config FILE                             # real time, until killed
totalblonder --config FILE --duration 60               # real time, for a minute
totalblonder --config FILE --offline 10 --out DIR      # 10 s of stream into DIR/carrier-N.ts
tb-ts-verify --rate 38800000 DIR/carrier-1.ts          # the independent checker on the result
```

Live inputs need the decoder worker: `TB_AVWORKER=/path/to/tb-avworker` (the NixOS module
sets it), or `tb-avworker` beside the `totalblonder` binary.

`crates/totalblonder/totalblonder.example.toml` lists every key. The control API (default
`127.0.0.1:8090`) serves `/healthz`, `/api/metrics` (Prometheus) and `/api/state`.

### Telemetry

`[telemetry]` sends OpenTelemetry over OTLP/HTTP (protobuf), from `tb-telemetry` (the
OpenTelemetry SDK; reqwest over rustls for https). Each signal has its own whole URL and is off
until it has one:

| Signal | What | VictoriaLogs / VictoriaMetrics path |
|---|---|---|
| `logs` | log lines at `log_level` and above, and every wide event | `/insert/opentelemetry/v1/logs` |
| `metrics` | every `/api/metrics` series, every `metrics_interval_s` | `/opentelemetry/v1/metrics` |

A series whose name ends in `_total` is a counter, any other a gauge; labels become attributes.
Resource attributes are `service.name`, `service.instance.id` and `host.name` (the host's name
unless `instance` is set), and those in `[telemetry.resource]`. Exporting never blocks the
stream: records go to a bounded queue that drops when the collector is behind.

A **wide event** is one record per unit of work, with every field that describes it and the
fields of the context it ran in (`carrier`, `service`, `program`, `channel`, `input`, `role`,
`peer`). Each thread and program has a context span, so ordinary log lines carry the same
fields. Code makes one with `tb_telemetry::wide!("noun.verb", key = value, …)`, fills it with
`set`, `add` and `error`, and it is reported when dropped, with `duration_ms`; `emit()`
reports an instant one.

| Event | When | Notable fields |
|---|---|---|
| `server.start`, `server.run` | start; exit | config path, mode, carriers, programs, video device; `stop` (signal, duration), `outcome` |
| `input.session` | an input's `Streams` … `End` | `stream.N.codec`, `.size`, `.fps`, `.sample_rate`; packets, keyframes, bytes; `outcome` (closed, transport, protocol, local) and `reason` |
| `rtmp.connection` | an RTMP publisher's connection | `peer`, `rtmp.app`, `rtmp.close` |
| `decoder.worker` | a sandboxed decoder's life | generation, confinement, pictures, audio blocks, `exit` |
| `decoder.circuit_open`, `decoder.violation` | the worker keeps failing; it lies | pause, what |
| `source.switch` | a program changes source | `from`, `to` (live, standby, slate), `held_s`, health of each feed; WARN going down |
| `video.step` | the overload governor steps a program | direction, from and to format, rung, starting rate |
| `device.request` | one HTTP exchange with the modulator | method, request kind, status, `outcome` |
| `device.alarm`, `device.alarm_cleared`, `device.write` | the supervisor's actions | alarm, detail |
| `tables.cycle` | each table rebuild (10 s) | schedule reloaded, sets delivered, queue full, failed, versions saved |
| `carrier.interval`, `service.interval`, `input.interval`, `process.interval` | every `summary_interval_s` | every series of that entity: a counter's gain over the interval (name without `_total`), a gauge's value; `service.interval` adds its `format` and `source`, `process.interval` the CPU share of each thread |

The interval events are on by default only when logs are exported. Without a logs URL, wide
events are written to stderr as one line each; with one, only those at WARN or worse are.

In VictoriaLogs every field is queryable, for example:

```
event.name:service.interval | stats by (service) sum(encoder_shed) shed, sum(late_au) late
event.name:process.interval | stats max(thread_cpu_pct.video-hub) hub, max(process_resident_bytes) rss
event.name:source.switch to:slate | fields _time, service, from, held_s
event.name:input.session -outcome:closed | fields _time, input, peer, outcome, reason, duration_ms
```

## 7. Measured

- Real time on nixark, RTX 2000 Ada, 3 carriers and 7 programs (720p, 1080i, 1080p, 480i)
  for 20 s: 0 late or dropped access units, 0 pacer underruns, PCR every 30.0 ms, every access
  unit delivered at least 408 ms before its decoding time, 99.8 % of datagrams within 50 µs of
  their slot time (worst 568 µs). TSDuck: PCR-derived rate exactly 38,800,000 bit/s, 0 PCR with
  jitter over 1 µs, 0 continuity errors; ffmpeg decodes every program without error (the last
  picture of a file cut by the stop is the only damage). The UDP carrier, captured by TSDuck on
  the same host, is byte-exact in rate and continuity.
- Live input on nixark, real time: ffmpeg published 1080p30 H.264 + AAC over RTMP into a
  720p program next to a 480i slate program. The sandboxed decoder started with all 13
  confinement layers; the program went slate → live → slate (`program_source` 3 → 1 → 3);
  sound followed (silence, the 440 Hz tone at −26 dB RMS after loudness control, silence);
  0 dropped access units; ffmpeg decodes the file without an error; **`tb-ts-verify` finds
  nothing at any severity** (PCR accuracy 19 ns, rate exactly 38.8 Mbit/s, every table interval,
  the T-STD buffers).
- Film mode on nixark: a 23.976 fps RTMP source into a 1080i program with `film = "auto"`
  is coded as pictures of three and two fields in strict alternation (`repeat_first_field`);
  both video streams pass `tb-m2v-verify`, `tb-ts-verify` finds nothing, ffmpeg decodes
  without error (end of file included: the stop is clean).
- Fallback chain on nixark: a program with a live RTMP input and a looping file as standby
  went slate → standby → live → standby, picture and sound; `tb-ts-verify` finds nothing.
- With audio, subtitles and the statmux on (3 carriers, 7 programs, 20 s): 0 dropped video
  or audio, every track with its language and layout, PCR every 30 ms; the in-process
  self-check reports 0 findings at every severity on all three carriers; the whole server uses
  about 15 % of one core (the video encode is on the card).
- Offline on bagel with the reference adapter (one 480i program with a stereo and a 5.1 track,
  subtitles, a schedule): the stream ends on a whole GOP with `sequence_end_code`;
  `tb-m2v-verify` and `tb-ts-verify` find nothing; ffmpeg and CCExtractor read the captions back;
  TSDuck decodes the guide, the rating in the PMT and the ETT.
