# totalblonder — design spec

Status: draft v1, 2026-11-24. Written before any server code exists and before anything has
been sent to the modulator's data ports. §19 lists what has been verified so far. The
hardware spike M0 (§16) has not been run; it settles the items marked [unknown] in §2 and
"pending M0" in §4.

The sections follow the dependencies of the design, starting at the modulator and working
back toward the producers: what the box requires (§2), an overview (§3), what must leave the
host (§4), the clock that drives it (§5), the mux (§6), the tables (§7), the encoders and
rate allocation (§8), ingest and fallback (§9). Then come the parts beside the stream path:
modulator control (§10), monitoring (§11), the failure matrix (§12), the operator interface
(§13), capacity (§14), tests (§15), milestones (§16), code conventions (§17), and the
assumptions and open questions (§18).

How to read this document:

- **[decided]** marks a decision the owner has made. **[proposed]** marks a choice made in this
  draft that the notes did not settle; each can be revised without changing the architecture.
  **[unknown]** marks something nobody has measured or answered; each has an entry in §18.
- **[K]** and **[I]** are carried over from the notes: [K] is general knowledge that was not
  checked against a document or the device; [I] is an inference from cited material.
- References such as (standards-digest §1.2) point at `docs/notes/standards-digest.md` section 1.2.
  The other notes are cited the same way: `software-survey`, `corruption-48`, `device`,
  `manual`, `firmware`, `research`, `design-inputs`. A fact about the box, a
  standard or third-party software that has no reference is an error in this document.

## 1. Scope

A single long-running Rust process, `totalblonder`, that:

1. accepts live content from network producers (first: one RTMP-push producer),
2. decodes it and re-encodes it to a rate-controlled broadcast profile (MPEG-2 or H.264 video,
   AC-3 audio),
3. multiplexes up to six programs into one constant-bitrate MPEG-2 multi-program transport
   stream (MPTS) per QAM carrier, for up to 16 carriers,
4. generates every table a cable-ready TV needs: PAT and PMT (the MPEG "program specific
   information", PSI) and the ATSC "program and system information protocol" tables (PSIP):
   MGT, CVCT, STT, and later EIT and ETT for the program guide,
5. sends each MPTS over UDP or RTP to a Blonder Tongue EQAM-420B-4-64, which modulates what
   arrives (MPTS pass-through mode),
6. configures and supervises the modulator through its web forms, and
7. watches the RF output with an HDHomeRun tuner and checks that every carrier is on air,
   decodable and correctly labeled.

The owner's stated priorities are "high quality, bullshit-resistant" and "extremely robust",
with the full feature set: program guide (EPG), second audio (SAP), MPTS and dynamic bitrate
allocation (design-inputs "Feature candidates").

### Non-goals

- 4K and HEVC. The box documents only MPEG-2 and H.264, and TV tuner paths are not expected to
  decode HEVC [K] (design-inputs "Decisions so far").
- Encryption, on-demand, pay-per-view, channel logos, directed channel change, data broadcast.
  No clear-QAM TV supports them [K] (design-inputs "Not worth pursuing").
- SCTE 65 out-of-band tables. They serve set-top boxes, not TVs (standards-digest §2.9).
- Replacing or permanently patching the modulator's firmware. RAM-only patches are considered
  in §10.4; a rebuilt flash image is not (corruption-48 §5 item 5).
- Changing the modulator's network or login settings, ever (README "Safety rules").
- Distribution of the binary. Linking libx264 makes it GPL, which is acceptable in-house
  (software-survey §10, §11 item 5).
- SMPTE 2022-1 forward error correction. It is not documented for the box and is pointless on
  a clean LAN (software-survey §8).

### Design principles

- **The mux never stops.** Each carrier's output is driven by a clock, not by its inputs. If
  every producer, decoder and encoder fails, the carrier still carries valid tables, PCRs, a
  slate and silence at the exact nominal rate.
- **One timeline, ours.** Every output timestamp comes from the carrier's output clock.
  Producer timestamps are only hints for ordering and pacing. The output never signals a
  discontinuity (standards-digest §1.8).
- **Switch inputs, not bitstreams.** Fallback and source changes happen in front of the
  encoder, on raw frames and samples. The encoder then produces one continuous elementary
  stream and the mux needs no splice logic (standards-digest §1.8).
- **Isolate failure per program.** A dead or misbehaving input degrades one program. A crashed
  encoder degrades one program for the time it takes to restart it. Nothing a producer sends
  can move another program's packets.
- **Measure, then believe.** Every claim about conformance is checked by our own parsers, by
  TSDuck as an independent oracle, and by a real tuner after the modulator. A number in this
  document that was not measured on this hardware is labeled as an estimate.
- **Treat the modulator as fragile.** Its firmware has three confirmed configuration and
  memory defects (corruption-48 §0). Writes are few, whole-form, ordered, verified by reading
  back, and limited to an allow-list.
- **Pure logic apart from I/O.** The mux scheduler, table generators, statmux controller,
  timeline mapper, fallback state machine and supervisor are pure state machines with no
  clocks, sockets or libav calls inside them, so they can be unit-tested.

## 2. Constraints from the modulator

This section collects what the box is known to do with its input. The rest of the design is
built against it. Items marked [unknown] are measured in M0.

| Item | Value | Source |
|---|---|---|
| Mode | MPTS: 16 inputs, one per carrier, passed through. **[decided]** | manual §4.2; design-inputs "Decisions so far" |
| Programs | At most 6 per MPTS, each 1 video and at most 3 audio | manual §4.2 |
| PSI/PSIP in MPTS mode | "Pass thru"; the server must generate all of it | manual §4.3 (Appendix C) |
| Guide tables (EIT/ETT) | Reported working on another unit of this model; not tested on ours [unknown] | design-inputs "Facts from the owner"; research §4 |
| Video | MPEG-2 or H.264. H.264 is documented but untested here | manual §4.2; design-inputs "Codec choice per channel" |
| Audio | AC-3. The FPGA parses audio PES and frame sync even in MPTS mode [I] | research §1 item 3 |
| Transport | Clear UDP or RTP, URL form `UDP://a.b.c.d:port` or `RTP://a.b.c.d:port` | manual §4.1; device §11.4 |
| Addressing | 224.0.0.0–239.255.255.255 is joined as multicast (IGMPv2 now, v3 available); other addresses are unicast. Whether a unicast address means the sender or the box is unresolved [unknown] | manual §4.1; device §4 |
| Duplicates | No two inputs may use the same address and port | manual §4.1 |
| Input ports | Inputs 1–8 arrive on GIGE1, 9–16 on GIGE2 | device §3 "statusedit.cgi" |
| Rate ceiling | 38.8 Mbps per carrier documented. The 256-QAM Annex B payload rate is 38,810,701 bit/s ± 5 ppm (±194 bit/s), set by the modulator's symbol clock | manual §4.2; standards-digest §8 |
| Rate adaptation | The box inserts and deletes null packets and corrects PCR. It does no rate control: excess input "pixelates" | manual §4.2; standards-digest §8 |
| Internal model | Even in MPTS mode the FPGA reschedules packets against decoder buffer models; it is not a bit pipe [I] | research §1 item 3 |
| Packets per datagram, RTP details, jitter tolerance, FEC | Not documented [unknown] | manual §4.1 |
| RF | 16 carriers, all on, 256-QAM Annex B, 40 dBmV, on cable channels 2–5, 10–13, 6/95/96/97 and 140–143 | device §4 |

Consequences:

- The server emits an MPTS that conforms to the cable transport rules (SCTE 54 and its
  decoder buffer model), because the box applies such a model itself (research §1 item 3).
- The server's rate must stay below the modulator's payload rate at every instant, because
  the box can only remove null packets, not content (standards-digest §8).
- Nothing that depends on an [unknown] row is fixed in the design. The datagram format and
  pacing precision are configuration until M0 has run.

## 3. System overview

```
 producers                      totalblonder (one process)
┌──────────┐ RTMP push  ┌───────────────────────────────────────────────────────────────┐
│ producer │───────────►│ ingest ─► decode ─► timeline mapper ─┐   one of these per     │
└──────────┘            │ (libav)   (libav)   (pure)           │   configured input     │
┌──────────┐ LL-HLS     │                                      ▼                        │
│ others   │◄ ─ pull ─ ─│              ┌──────────── program (×≤6 per carrier) ───────┐ │
└──────────┘  (later)   │   standby ──►│ fallback selector ─► frame clock ─► video    │ │
                        │   slate   ──►│ (pure)               (output fps)   encoder  │ │
                        │   silence ──►│                   ─► sample clock ─► AC-3    │ │
                        │              │                      (48 kHz)       encoder  │ │
                        │              └───────────────┬──────────────────────────────┘ │
                        │      rate caps ▲             │ access units (PTS/DTS on the   │
                        │   ┌────────────┴───┐         ▼ carrier clock)                 │
                        │   │ statmux        │   ┌───────────────────────────────────┐  │
                        │   │ controller     │◄──│ mux (one per carrier, pure core)  │  │
                        │   │ (per carrier)  │   │ slot scheduler, PCR, PES/TS       │  │
                        │   └────────────────┘   │ packetizer, T-STD model, nulls    │  │
                        │   ┌────────────────┐   │                                   │  │
                        │   │ PSI/PSIP       │──►│ sections ─► carousel              │  │
                        │   │ generators     │   └──────────────────┬────────────────┘  │
                        │   └───────▲────────┘                      │ slots (188 B)     │
                        │   lineup, │ guide                         ▼                   │
                        │   ┌───────┴────────┐          ┌───────────────────────┐       │
                        │   │ control plane  │          │ pacer (one per NIC)   │       │
                        │   │ axum, SQLite   │          │ 7 packets / datagram  │       │
                        │   └───┬────────┬───┘          └──────────┬────────────┘       │
                        │       │        │                         │                    │
                        │  ┌────▼─────┐ ┌▼──────────┐              │                    │
                        │  │ device   │ │ RF probe  │              │                    │
                        │  │ supervisor│ │ driver    │              │                    │
                        └──┴────┬─────┴─┴────┬──────┴──────────────┼────────────────────┘
                 HTTP forms,    │            │ hdhomerun_config    │ UDP or RTP, 16 streams
                 telnet (few)   ▼            ▼                     ▼
                        ┌───────────────┐  ┌───────────┐   ┌───────────────────────┐
                        │ EQAM-420B     │  │ HDHomeRun │   │ EQAM-420B GIGE1/GIGE2 │
                        │ control port  │  └─────▲─────┘   │ 16 × 256-QAM carriers │
                        └───────────────┘        │ ~30 dB  └───────────┬───────────┘
                                                 └──── attenuator ◄────┤ coax
                                                                       ▼ TVs
```

Runtime model **[decided]**:

- A `tokio` multi-thread runtime runs the control plane, the device supervisor, the probe
  driver and anything that waits on the network without libav.
- Dedicated OS threads run everything that blocks or must not be delayed: one ingest-and-decode
  thread per input, one video-encode and one audio-encode thread per program, one mux thread
  per carrier, one pacer thread per output network interface.
- Threads are joined by bounded channels (`crossbeam`). Producers of data never block: a full
  queue means the item is dropped and a counter is incremented. The one exception is the
  mux-to-pacer ring, described in §4.3, which never blocks either side by construction.
- Shutdown is a `CancellationToken`. A mux and its pacer are the last things to stop.

Terms used below:

| Term | Meaning |
|---|---|
| Carrier | One QAM channel of the modulator, fed by one MPTS. Numbered 1–16; carrier *n* is the box's input slot *n* (device §3 "statusedit.cgi"). |
| Program | One TV channel inside an MPTS: one video stream and one to three audio streams. Numbered 1–6 per carrier. |
| Service | The stable identity of a TV channel across restarts and lineup changes: a name, a virtual channel number and a `source_id`. A service is placed on one program of one carrier. |
| Input | One producer connection (for example one RTMP listener). |
| Slot | The position of one 188-byte TS packet in a carrier's output. Slots are numbered from 0 at process start. |
| R | The nominal TS rate of a carrier, 38,800,000 bit/s. |
| STC | System time clock: the carrier's 27 MHz time base, carried in the stream as PCR. |
| AU | Access unit: one coded picture or one coded audio frame. |
| D | The video decoder-buffer delay, `bufsize / maxrate`, 0.5 s by default (§8.2). |

## 4. What leaves the host
This section fixes the interface between the host and the modulator's GigE ports: what a
datagram contains, where it is sent and when. Everything upstream exists to fill these
datagrams. The items marked "pending M0" stay configurable until the spike has measured what
the box accepts (§2).

### 4.1 Datagram format

**[proposed, pending M0]** Each datagram carries exactly 7 TS packets, 1,316 bytes, the
common convention [K] (standards-digest §8). Two framings are implemented and selected per
carrier:

- UDP: the 1,316 bytes are the UDP payload.
- RTP: a 12-byte header in front, payload type 33, a 90 kHz timestamp taken from the slot
  clock of the first packet, and a sequence number (software-survey §8).

Which the box prefers, and whether it needs exactly 7 packets, is [unknown] (software-survey
§12 item 1). The box's stream URL selects `UDP://` or `RTP://` to match (§2).

### 4.2 Addressing

**[proposed, pending M0]** One multicast group per carrier, `239.10.0.<carrier>:5000`
(carrier 1 matches the spike address in software-survey §2.6). Every stream has a distinct
address and port (§2). Carriers 1–8 are sent on the interface cabled to GIGE1 and 9–16 on the
one cabled to GIGE2 (§2). Sixteen carriers are about 58,000 datagrams/s and about 620 Mbit/s
of payload, too close to one gigabit port's limit, which is another reason to use both
(software-survey §8). Multicast is sent with an explicit outgoing interface and a TTL of 1–4,
and must never reach the box's 10/100 control port (manual §2; software-survey §8).

**[decided 2026-11-26]** Each GigE port is on a VLAN of its own: VLAN 20 `10.42.20.0/24` for
GIGE1 and VLAN 21 `10.42.21.0/24` for GIGE2. `nixark` sends from one SR-IOV VF in each, so
carriers 1–8 go out one interface and 9–16 the other. Each VLAN's only receiver is the box, so
a group reaches it even if the switch floods it. That makes IGMP snooping, an IGMP querier and
ARP irrelevant to the data path. IGMPv2 stays, with no source filters. The box's live addresses
(GIGE1 inside the control port's subnet, GIGE2's corrupted mask and gateway) are replaced by one
`admin.cgi` save (§18 item 2; firmware §4.1).

### 4.3 Pacing

One pacer thread per output interface **[decided]** (software-survey §8):

- A min-heap of per-carrier deadlines. Datagram `k` of a carrier is due at
  `epoch + k · 7 · 1504 / R`, 271.34 µs apart, computed from `k` in integer nanoseconds.
- The thread sleeps with `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)` until about 50 µs
  before the earliest deadline, spins the remainder, then sends everything due within the
  next 100 µs in one `sendmmsg`.
- `SCHED_FIFO` and CPU pinning are options; `SO_TXTIME` with the `fq` qdisc is the upgrade
  path if userspace pacing is not precise enough for the box (software-survey §8). UDP
  segmentation offload is never used, because it sends bursts (software-survey §8).

The ring between mux and pacer is indexed by slot. **[proposed]** The pacer owns time: when a
datagram's deadline arrives and the mux has not filled its seven slots, the pacer sends seven
null packets in their place and advances. The mux sees the pacer's position and skips the
slots it missed. The slot-to-time relation of §5.1, and with it every PCR, therefore stays
exact even when the mux thread is late; only payload is delayed, and the underrun is counted.
The pacer never waits for the mux and the mux never waits for the network.

A send error is counted and logged and the datagram is skipped. The socket is reopened after
`n` consecutive errors. Time is never made up by bursting.

## 5. Clock and timeline

### 5.1 Carrier output clock

**[decided]** Each carrier has one output clock. Every PCR, PTS and DTS on that carrier is
derived from it (standards-digest §1.2 recommendation).

**[proposed]** The clock is a slot counter. A carrier's stream is an endless sequence of
188-byte slots, numbered `n = 0, 1, 2, …` from process start. Slot `n` nominally leaves the
host at

```
t(n) = epoch + n · 1504 / R   seconds        (1504 bits per packet, R = 38,800,000 bit/s)
```

where `epoch` is a `CLOCK_MONOTONIC` instant chosen at start. One slot lasts 38.76 µs; there
are about 25,797.87 slots per second. All conversions use exact integer arithmetic on `n`
(128-bit intermediate), never a running floating-point sum, so there is no cumulative
rounding error.

The carrier's STC at slot `n`, in 27 MHz units, is

```
STC(n) = stc0 + floor((188·n + 10) · 8 · 27,000,000 / R)
```

`stc0` is an arbitrary start value. The `+ 10` is the offset of the byte that holds the last
bit of the PCR base field inside a packet, which is the instant a PCR refers to
(standards-digest §1.2; the offset 10 is [I] from the adaptation-field layout in
standards-digest §1.1). A PCR written into slot `n` is `STC(n)` split into base and extension.
Because the value depends only on the slot index, PCR accuracy against byte position is exact
by construction; the ±500 ns limit (standards-digest §1.2) is met with the only error being
the rounding to one 27 MHz tick (37 ns).

The pacer (§4) sends slot `n` at `t(n)`. The relation between slot index and time therefore
holds on the wire up to pacing jitter, which the box's input buffer and PCR correction absorb
(§2; tolerance [unknown], M0).

Frequency accuracy: the standard requires the 27 MHz clock within ±30 ppm and drift below
0.075 Hz/s (standards-digest §1.2). The clock here is the host's `CLOCK_MONOTONIC`.
**[proposed]** The host runs an NTP client that slews (never steps) the clock; the server
exports the kernel's reported frequency error and alarms above 20 ppm. A free-running host is
assumed to be within ±100 ppm [K] (standards-digest §8), which still keeps R below the
modulator's rate: R is 276 ppm under 38,810,701 bit/s (standards-digest §8).

All 16 carriers read the same host clock but keep separate slot counters and separate `stc0`
values. Programs on one carrier share that carrier's STC, which is permitted (standards-digest
§1.2).

### 5.2 Program output cadence

Each program produces output on a fixed cadence tied to its carrier's STC, whether or not it
has input:

- Video: one frame every frame period of the configured output format (for example 1001/60000
  s for 720p59.94). Frame `k` has presentation time `PTS_v(k) = P0 + k · period`, computed in
  27 MHz units and rounded to 90 kHz when written.
- Audio: one AC-3 frame of 1536 samples every 32 ms; frame `j` has `PTS_a(j) = P0 + j · 2880`
  in 90 kHz units (standards-digest §5.3). Because 2880 ticks per 1536 samples is exact, the
  48 kHz sample clock is locked to the 27 MHz clock in the required ratio (standards-digest
  §1.2) as long as exactly 1536 samples are produced per frame, which the sample clock in
  §5.4 guarantees.

`P0` is the program's start time on the STC plus a fixed pipeline delay `Δ`:

```
Δ = encode budget + D + mux margin        (defaults: 0.5 s + 0.5 s + 0.1 s)   [proposed]
```

A frame with presentation time `T` is handed to the encoder when the STC reads about `T − Δ`.
The encoder may take up to the encode budget to return it; the mux then has `D` plus the
margin to deliver it before its decode time. `Δ` is constant for the life of the process, so
the PCR-to-DTS relation never moves, including across rate changes (software-survey §5.3
item 2).

Every PES header carries a PTS, and a DTS when it differs (standards-digest §1.7). Neither
ever jumps: there is no time-base discontinuity on the output and `discontinuity_indicator` is
never set (standards-digest §1.8).

### 5.3 Mapping input timestamps

Producers run on their own clocks and their timestamps reset. The producer restarts each RTMP
connection at a keyframe with timestamps from zero, sends nothing while idle, and jumps its
timestamps forward across idle periods.

**[proposed]** The timeline mapper is a pure state machine per input. It holds one *session*
at a time. A session is a run of input with consistent timestamps, described by an affine map

```
out_time = offset + in_time · (1 + skew)
```

from input timestamps to carrier STC time.

- **Session start.** The first decodable frame of a session (a keyframe) is mapped so that it
  is due for presentation `J` after its arrival, where `J` is the jitter-buffer target
  (default 1 s). `skew` starts at 0.
- **Skew estimate.** The mapper tracks the minimum of `arrival_time − in_time` over a sliding
  window (default 30 s). The slope of that minimum over time is the producer's clock error
  relative to ours. `skew` follows the estimate through a
  slow filter and is clamped to ±500 ppm. It moves audio and video together, so it never
  changes their relative timing. The estimate corrects the slope of the buffer depth only, so
  a weak depth term is added to it: `(J − depth) / 10 min`, at most 50 ppm, with the depth
  measured on the fastest frame of the newest window bucket. Without it the estimation noise
  integrates into a random walk of the depth (tens of ms in a month); with it the depth stays
  within a few ms of `J` (`crates/tb-timeline/tests/long_run.rs`).
- **Discontinuity.** A session ends, and the next packet starts a new one, when any of these
  holds: the transport connection closed; a timestamp goes backwards; the difference between
  the input timestamp step and the arrival-time step exceeds `reset_threshold` (default
  500 ms) for more than a few packets; or the buffer depth leaves `[0, 2·J]` for longer than
  `resync_after` (default 5 s) despite the skew correction.
- **Gap.** If no frame is due and none arrives, the mapper reports "no frame". It does not
  end the session until one of the discontinuity rules fires or `live_timeout` (default 2 s
  beyond the buffer) passes.

The mapper outputs frames and samples labeled with output times. It never changes the output
cadence; that belongs to the next stage.

### 5.4 Frame clock and sample clock

These two components convert "frames and samples with output times, possibly missing" into
"exactly one frame per frame period and exactly 1536 samples per 32 ms, forever".

Video frame clock **[proposed]**: at each output frame tick with presentation time `T`, take
the newest mapped frame whose output time is ≤ `T`. If it is the frame already shown, it is
repeated. If more than one new frame is due, the older ones are dropped. This is the only
place where frames are dropped or repeated, and both are counted. An input at 30.000 fps
feeding a 29.97 fps output therefore drops one frame in 1001; a producer running 100 ppm slow
repeats one frame in 10,000. Scaling and pixel-format conversion to the output format happen
here, after selection, so dropped frames cost nothing.

Audio sample clock **[proposed]**: input audio is resampled to 48 kHz by libswresample with
its ratio trimmed by the session's `skew` plus a small proportional correction on the audio
buffer depth, clamped to ±500 ppm in total. If the buffer still runs dry, the missing samples
are silence with a 10 ms fade at each edge; if it overfills past `2·J`, the mapper ends the
session (§5.3). When an input has no audio at all, as with the producer today, the sample clock produces digital silence.

A/V sync: video and audio of a session share one `offset` and `skew`, so their relative
timing is the producer's. The target is audio within ±1 AC-3 frame of video, checked with a
sync clip (standards-digest §5.5).

### 5.5 Wall-clock time

The STT (§7.4) needs UTC. The mux converts a slot time to UTC through a
`CLOCK_MONOTONIC`-to-`CLOCK_REALTIME` offset sampled once a second. If the host clock is
unsynchronized the STT is still sent, and the condition is exported and alarmed, because TVs
set their clock and place guide entries from it [K] (design-inputs "Feature candidates").

## 6. Mux

The mux is written in-house **[decided]**. FFmpeg's muxer emits nothing when an encoder
stalls, writes whole access units back to back, has no ATSC descriptors or PSIP and cannot
change the program set without a restart (software-survey §3.1). Upipe's `upipe_ts_mux` is the
design reference for a clock-driven TS mux (software-survey §5.2).

### 6.1 Structure

One mux per carrier, on its own thread. Its core is a pure function
`next_packet(state, slot_index) -> [u8; 188]` plus queue inputs. The thread keeps the
mux-to-pacer ring filled `lead` ahead of real time (default 20 ms, about 516 slots).

Inputs per program: a queue of video AUs, a queue of audio AUs per audio stream, and the
program's current delivery rate `r_p` from the statmux controller (§8.4). Inputs per carrier:
the current set of PSI/PSIP sections with their repetition periods (§7).

### 6.2 Packetization

- PES: one video frame per PES; `PES_packet_length` 0 for video; `data_alignment_indicator`
  1; PTS on every PES and DTS when different; AC-3 in `stream_id` 0xBD with one AC-3 frame per
  PES **[proposed]** (standards-digest §1.7).
- A PES starts at the start of a TS packet with `payload_unit_start_indicator` set. The last
  packet of a PES is padded with adaptation-field stuffing, the only stuffing allowed in
  packets carrying PES (standards-digest §1.1).
- Continuity counters are kept per PID by the mux and never jump, whatever the inputs do
  (standards-digest §1.4).
- PCR is carried on each program's video PID (standards-digest §1.2 recommendation). If a PCR
  is due and the video queue has nothing to send, the mux emits an adaptation-only packet
  (which does not advance the continuity counter). PAT and PMT packets never carry an
  adaptation field (standards-digest §1.1).
- Null packets are `47 1F FF 10` followed by 184 bytes of 0xFF (standards-digest §1.5).

Audio overhead: a 192 kbit/s AC-3 frame is 768 bytes; with a 14-byte PES header it needs five
TS packets, so the stream occupies 235 kbit/s of the multiplex, 22 % above the elementary
rate. At 384 kbit/s the figure is 423 kbit/s. [I, arithmetic from standards-digest §5.3 and
§1.1.] The carrier budget uses these exact figures, not the "× 1.05" rule of thumb in
software-survey §5.3.

### 6.3 Slot scheduling **[proposed]**

For each slot the scheduler picks the first of the following that has something to send. A
later class never delays an earlier one, and every class is bounded so that it cannot starve
the ones below it.

1. **PCR due.** For each program, a PCR is due `pcr_interval` (default 30 ms) after the last
   one. The required maximum is 100 ms (standards-digest §1.2); the margin allows for the
   box's own retiming. The PCR goes on the next video packet of that program, or on an
   adaptation-only packet.
2. **Tables.** The carousel (§7.6) offers at most one section packet per slot, earliest
   deadline first, subject to the per-PID buffer models in §6.4.
3. **Audio.** Each audio stream is delivered as a smooth constant-rate flow timed so that the
   first byte of a frame arrives `audio_lead` (default 40 ms) before its PTS. The AC-3 decoder
   buffer is only 2,592 bytes (standards-digest §1.6), so audio cannot be sent far ahead: the
   usable lead is about 50 ms [K] (software-survey §5.3 item 6).
4. **Video.** Each program's video queue drains at its delivery rate `r_p` through a token
   bucket with a depth of two packets. Among programs with a token and data, the one whose
   head AU has the earliest decode time goes first. A program's video packets are also spaced
   by its transport-buffer model (§6.4).
5. **Null.**

Deadlines: the mux delivers the last byte of every AU before its DTS and starts no AU more
than 1 s (MPEG-2 video, audio) or 10 s (H.264) before its DTS (standards-digest §1.6). With
`Δ` and `D` as in §5.2, a video AU normally completes about `D` minus the encoder's current
buffer occupancy ahead of its DTS, which is how a buffer-compliant stream behaves when
delivered at its declared rate.

Why this respects the encoder's buffer model: an encoder configured with `max_bitrate = r`
and `vbv_bits = r·D` guarantees that a decoder fed at constant rate `r` after an initial delay
`D` neither underflows nor overflows. The mux provides exactly that channel: a constant
`r_p` (in TS terms, with about 3 % allowed for PES and TS packing, software-survey §5.3
item 1) and a constant `Δ`. §8.4 explains how `r_p` and the encoder's cap change together
without breaking the guarantee.

### 6.4 Decoder buffer models

The mux runs the transport-level decoder model (T-STD) for its own output as it schedules,
and refuses any packet that would break it; the packet waits one slot. The quantities
(standards-digest §1.6):

| Buffer | Size | Drain rate | Scheduler rule |
|---|---|---|---|
| Transport buffer per PID | 512 bytes | Video: 1.2 × the profile's maximum rate; audio: 2 Mbit/s; system: 1 Mbit/s | Never more than 512 bytes of one PID ahead of its drain. For audio and tables this forces spacing between packets of the same PID |
| System buffer (PAT plus one program's PMT) | 1,536 bytes | 80 kbit/s | PAT and each PMT are scheduled within this |
| PSIP PID smoothing buffer (0x1FFB, each EIT PID, each ETT PID) | 1,024 bytes | 250 kbit/s | Large sections are spread across slots, never sent back to back (standards-digest §2.3) |
| AC-3 audio main buffer | 2,592 bytes | Decoder removes one frame per 32 ms | Bounds `audio_lead` |
| Video elementary buffer | The encoder's `vbv_bits` | Decoder removes one AU at each DTS | Guaranteed by the encoder if delivery is at `r_p`; the mux models it and counts violations |

For MPEG-2 Main Profile at High Level the transport buffer drains far faster than the carrier
rate, so video bursts are harmless (software-survey §5.3 item 6). The token bucket in §6.3
still limits bursts, because the box runs its own buffer model and its tolerance is [unknown]
(research §1 item 3).

### 6.5 Carrier budget

Worked example for six programs with one stereo track each, using §6.2's figures. The video
pool is what the statmux divides.

| Component | kbit/s | Basis |
|---|---|---|
| Nominal rate R | 38,800 | §2 |
| Null floor, 2 % | 776 | standards-digest §8 recommendation (2–3 %) |
| PAT every 90 ms | 17 | one packet per cycle |
| Six PMTs every 300 ms | 30 | one packet each per cycle |
| PSIP base PID (MGT, CVCT, STT) | 25 | standards-digest §2.3 (Annex G.8 estimate) |
| EIT and ETT, when enabled | up to 200 | software-survey §5.3 item 1 (estimate) |
| Six AC-3 streams at 192 kbit/s | 1,410 | §6.2 |
| **Video pool, TS level** | **about 36,300** | remainder |
| Video pool, elementary level | about 35,200 | minus 3 % packing |

That is about 5.9 Mbit/s of video per program at six programs. The vendor's own density
examples are 2–3 HD MPEG-2 programs, or 5–6 HD H.264 programs, or 6 SD MPEG-2 programs per
carrier (research §3), so six HD MPEG-2 programs on one carrier should not be expected to
look good.

### 6.6 When an encoder does not deliver

If a program's video queue is empty at the time an AU should start, the mux sends nulls in
that program's share and keeps its PCRs going on adaptation-only packets. The decoder shows
its last picture. If the queue is still empty after `encoder_timeout` (default 2 s), the
program supervisor restarts the encoder thread; the new encoder starts with a keyframe on the
existing cadence and the existing PIDs and counters. For audio, the mux substitutes the
pre-encoded silent AC-3 frame for each missing frame at once, because a silent frame is
always valid in place of any other (software-survey §1).

If an AU arrives too late to be delivered before its DTS, the mux drops queued video up to
the next keyframe and counts it. The viewer sees a freeze of at most one GOP.

## 7. Tables

The generators are written in-house on `bitstream-io` and `crc` (`CRC_32_MPEG_2`) **[decided]**
(software-survey §1, §4.1). No Rust or C library writes the full ATSC set (software-survey
§4.5). Each generator is a pure function from a lineup description to section bytes.

### 7.1 Identifier plan

| Identifier | Rule | Source |
|---|---|---|
| `transport_stream_id` (TSID) | One per carrier, unique across the plant, never changed. Default `0x0100 + carrier` (carrier 1 is 0x0101, as in the spike) **[proposed]** | standards-digest §2.7, §10.3; software-survey §2.6 |
| `program_number` | 1–6 by program position on the carrier. Never 0 | standards-digest §1.3 |
| PMT PID | `0x0030 + 0x10 · (program − 1)`: 0x30, 0x40, … 0x80 | standards-digest §2.2; manual §4.3 |
| Video PID | PMT PID + 1. Also the PCR PID | standards-digest §2.2 |
| Audio PIDs | PMT PID + 4, + 5, + 6 | standards-digest §2.2 |
| PSIP base PID | 0x1FFB: MGT, CVCT, STT | standards-digest §2.2 |
| EIT PIDs | 0x1D00–0x1D7F, one per 3-hour window (§7.5) **[proposed]** | standards-digest §2.2 (range); standards-digest §3.5 |
| Event ETT PIDs | 0x1E00–0x1E7F, paired with the EIT PIDs; channel ETT on 0x1E80 | standards-digest §2.2 |
| `source_id` | One per service, 0x0001–0x0FFF, unique across all 16 carriers, allocated once, stored in SQLite and never reused | standards-digest §2.7 |
| Virtual channel | Two-part `major.minor`, both below 1000, unique across the plant, configured per service | standards-digest §2.6 |

PIDs depend only on the program position, and the TSID only on the carrier, so both stay the
same when a service's content changes. Receivers cache TSID-to-frequency and PIDs per channel
(standards-digest §10.3); moving a service to another carrier or position is a lineup change
that may need a rescan on the TV [I] (standards-digest §10.3).

Each carrier's CVCT lists only that carrier's own programs **[proposed]**, for the reasons in
standards-digest §2.8.

### 7.2 Table set

| Table | PID | Our cycle | Required maximum | Increment |
|---|---|---|---|---|
| PAT | 0x0000 | 90 ms | 100 ms | Minimum |
| PMT, one per program | §7.1 | 300 ms | 400 ms | Minimum |
| MGT | 0x1FFB | 100 ms | 150 ms | Minimum |
| CVCT | 0x1FFB | 250 ms | 400 ms | Minimum |
| STT | 0x1FFB | 1 s | 10 s on cable, 1 s terrestrial | Minimum |
| EIT-0 | window PID | 500 ms | 500 ms recommended | Guide |
| EIT-1 to EIT-3 | window PIDs | 3 s | none on cable | Guide |
| EIT-4 and later | window PIDs | 30 s | none | Guide |
| Event ETT | window PIDs | 10–60 s | none | Guide |
| Channel ETT | 0x1E80 | 60 s | none | Guide |
| RRT | — | not sent | optional for US region 1 | — |

Maxima are from standards-digest §2.3; our cycles follow its recommendation and leave margin
because the box may shift packets. The loopback run of the spike pipeline measured MGT gaps up
to 170 ms and CVCT gaps up to 440 ms with injection periods set at the limits
(software-survey §2.6), which is why ours are set well inside them.

**[decided]** The minimum set (PAT, PMT, MGT, CVCT, STT) comes first. EIT and ETT, ratings,
captions and multi-audio follow as independent increments, each of which only adds sections
or descriptors.

### 7.3 PAT, PMT and descriptors

PAT: the carrier's TSID and its programs. PMT per program (standards-digest §4.2):

- Program loop: registration descriptor `GA94`; `content_advisory_descriptor` when the current
  event is rated (ratings increment).
- Video: `stream_type` 0x02 (MPEG-2) or 0x1B (H.264); `data_stream_alignment_descriptor`
  (required); `caption_service_descriptor` when captions are present (captions increment);
  `AVC_video_descriptor` for H.264.
- Each audio stream: `stream_type` 0x81; `AC-3_audio_stream_descriptor` in its full form
  through the language field (required); registration descriptor `AC-3`;
  `ISO_639_language_descriptor`; `component_name_descriptor` when two tracks share language
  and service type (standards-digest §4.3, §4.4).

A changed table gets `version_number + 1 mod 32`. Version counters are stored in SQLite so
they keep incrementing across restarts (standards-digest §1.3).

### 7.4 MGT, CVCT, STT

- MGT (standards-digest §3.2): starts a TS packet with `pointer_field` 0; lists the CVCT and,
  when the guide is enabled, every EIT, event ETT and the channel ETT with PID, version and
  exact byte count. Whether a CVCT-only MGT is acceptable is an open point in the standard
  (standards-digest "Open questions" 2); until the guide increment the MGT lists the CVCT and
  an EIT-0 containing empty instances **[proposed]**.
- CVCT (standards-digest §3.3, §10.1): one entry per program with `short_name` (up to 7
  UTF-16 units), two-part number, `modulation_mode` 0x03, `carrier_frequency` 0,
  `channel_TSID` equal to the PAT's TSID, `service_type` 0x02, `source_id`, and the optional
  `service_location` and `extended_channel_name` descriptors. The service-location descriptor
  must equal the PMT, so both are generated from one structure.
- STT (standards-digest §3.1): GPS seconds, `GPS_UTC_offset` from configuration (18 s [K]),
  daylight-saving fields from a configured rule. The value in each STT is computed for the
  slot in which its last byte is sent, so it is exact to well under the ±1 s requirement.

### 7.5 Guide: EIT and ETT

- Windows are 3 hours on UTC boundaries; EIT-0 is the window containing now (standards-digest
  §3.5). **[proposed]** Window number `w` (hours since the GPS epoch divided by 3) uses EIT
  PID `0x1D00 + (w mod 128)` and ETT PID `0x1E00 + (w mod 128)`. At a boundary only the MGT
  changes: each table type moves to the next window's PID, which is how the standard expects
  the roll to happen (standards-digest §3.5). Version numbers are tracked per PID
  (standards-digest §3.2).
- Default depth is EIT-0 to EIT-7 (24 hours), configurable from 4 to 56 windows
  (standards-digest §3.5 recommendation).
- Every service has an instance in every window, empty if it has no events. An event that
  spans windows appears in each with the same `event_id`. Events are sorted and do not overlap
  (standards-digest §3.5).
- Event source: the control plane holds a schedule per service (§13.1). A service with no
  schedule gets one generated event per window titled with the service name, so the guide is
  never blank **[proposed]**.
- Text is one string, language `eng`, uncompressed Latin-1 (standards-digest §3.8).
- `ETT_table_id_extension`: the standard requires unique values but warns that non-zero values
  break some receivers (standards-digest §3.6). It is a configuration switch, default unique,
  to be settled on real TVs [unknown].

### 7.6 Carousel

The carousel is a pure scheduler: each section has a period and a next-due slot. Each slot it
offers the most overdue section packet that the PID's buffer model (§6.4) admits. Sections on
one PID are sent whole and in order. A table update replaces the section set atomically at a
section boundary.

### 7.7 Later increments

- Ratings: `content_advisory_descriptor` for region 0x01 in the PMT and EIT, restricted to
  the 54 permitted combinations; no RRT (standards-digest §3.4).
- Captions: CEA-608 and CEA-708 bytes built with `cea608-types` and `cea708-types`, attached
  to frames as A/53 side data so the encoder writes them, at the fixed counts per frame rate;
  `caption_service_descriptor` in the PMT and EIT (standards-digest §6; software-survey §1).
  Where caption text comes from is [unknown].
- Multi-audio: SAP and described-video signaling per standards-digest §4.3, including the
  unresolved labeling conflict noted there.
- AFD: always "full frame" unless we letterbox (standards-digest §7). Low priority.
- Emergency alert sections (SCTE 18) are not planned; standards-digest §9.3 gives the reasons
  and recommends program substitution.

## 8. Encode and rate allocation
Each program has one video encoder and one audio encoder per track. Both run on the carrier's
output cadence (§5.2) and never see a gap or a timestamp jump, whatever the input does. This
section defines the encoder interface, the coding constraints, and how a carrier's bit rate
is divided between its programs.

### 8.1 Video encoder trait **[decided]**

All video encoders sit behind one trait. The mux and the statmux controller know only the
trait.

```rust
/// One coded picture, timestamps already on the carrier clock.
struct AccessUnit { data: Bytes, pts: Stc90k, dts: Stc90k, keyframe: bool, avg_qp: Option<f32> }

struct RateCap { max_bitrate: u32, vbv_bits: u32 }   // vbv_bits = max_bitrate · D

enum RateChange {
    /// Takes effect at a frame the encoder identifies; no keyframe is forced (libx264).
    Seamless,
    /// Takes effect at the next GOP boundary (mpeg2video reopen; NVENC, which forces an IDR).
    AtNextGop,
}

struct EncoderCaps {
    rate_change: RateChange,
    strict_vbv: bool,          // output never violates the configured buffer model
    fixed_gop: bool,           // keyframes exactly every N frames, closed GOP
    a53_captions: bool,        // writes caption side data into the bitstream
    interlaced: bool,
}

trait VideoEncoder: Send {
    fn caps(&self) -> EncoderCaps;
    /// Queue one frame. `frame.pts` is the output presentation time; captions ride as side data.
    fn submit(&mut self, frame: &VideoFrame) -> Result<(), EncodeError>;
    /// Next finished access unit, if any. Never blocks longer than `deadline`.
    fn poll(&mut self, deadline: Instant) -> Result<Option<AccessUnit>, EncodeError>;
    /// Request a new cap. Returns the PTS of the first frame coded under it, once known.
    fn set_rate(&mut self, cap: RateCap) -> Result<RateTicket, EncodeError>;
}
```

The four statmux requirements on any implementation **[decided]**: mid-stream rate change,
strict VBV/HRD compliance, a fixed IDR (keyframe) cadence, and caption side data. An encoder
that cannot change rate at all may still be used with a static cap; an encoder that violates
its buffer model may not be used.

| Implementation | Status | Rate change | Captions | Interlace | Source |
|---|---|---|---|---|---|
| `mpeg2video` (libav, CPU) | Reference, default codec **[decided]** | Close and reopen at a closed-GOP boundary with the tracked buffer fullness as `rc_initial_buffer_occupancy`; `bit_rate` is latched at init | Yes (`a53cc`) | Yes | software-survey §3.2, §5.1 |
| `libx264` (CPU) | Reference **[decided]** | `x264_encoder_reconfig` through libav when `rc_max_rate`/`rc_buffer_size` change; no IDR; lands about lookahead + threads frames later | Yes | Yes | software-survey §3.2, §5.1 |
| NVENC H.264 (RTX 2000 Ada) | Later candidate | Each change forces an IDR, so only at GOP boundaries | Yes | No on Ada | software-survey §9.2 |
| Friend's CUDA MPEG-2 encoder | Later candidate, unseen [unknown] | Unknown | Unknown | Unknown | design-inputs "Facts from the owner" |

`ffmpeg-the-third` does not expose `rc_buffer_size`, `rc_min_rate` or
`rc_initial_buffer_occupancy`; they are written through the raw context pointer in one named
`unsafe` module (software-survey §3.3).

### 8.2 Video profiles

Common to both codecs **[proposed]**, each from the cited rule:

- Closed GOP of 0.5 s (30 frames at 59.94 fps, 15 at 29.97 fps), scene-cut keyframes
  disabled, with a sequence header (MPEG-2) or SPS/PPS (H.264) at every keyframe. There is no
  normative GOP rule for MPEG-2 on cable; 0.5 s is the informative suggestion and satisfies
  the ≤ 1 s rule for H.264 (standards-digest §5.1, §4.6).
- Constant buffer delay `D = vbv_bits / max_bitrate = 0.5 s` for every program, so the
  PCR-to-DTS offset is the same at every rate (software-survey §5.3 item 2).
- No filler: MPEG-2 has none, and x264 runs with `nal-hrd=vbr`. The mux pads with null packets
  (software-survey §5.1).
- Output frame rates are the 1000/1001 family by default (59.94, 29.97). Integer rates (60,
  30) are legal (standards-digest §5.1) and configurable per program; whether the TVs treat
  them equally is [unknown].

MPEG-2 (standards-digest §5.1): Main Profile at High Level; formats limited to the table in
that section (1920×1080 at up to 30 fps progressive or interlaced, 1280×720 at up to 60 fps,
704×480 and 640×480); `vbv_buffer_size_value` ≤ 488, that is 7,995,392 bits; `vbv_delay` ≤
0.5 s; color description signaled explicitly. With `D` = 0.5 s the buffer limit caps a
program at about 15.99 Mbit/s [I, from the two limits]. Encoder options start from the HD set
in software-survey §3.2 (`trellis`, `mbd=rd`, `intra_vlc`, `non_linear_quant`, `dc=10`), whose
quality is "adequate, not broadcast-grade" [K].

H.264 (standards-digest §4.6, §5.2): High Profile, Level 4.0 (4.2 for 1080p60); access unit
delimiter in every AU; SPS and PPS in-band at every keyframe; VUI with timing information;
at most 30 Mbit/s. The TS packet that starts a keyframe's PES sets `random_access_indicator`,
and the packet with its first slice sets `elementary_stream_priority_indicator`.

The producer delivers 1920×1080 at 30 fps progressive.
MPEG-2 allows 1080p at 29.97 or 30 fps, so no interlacing is needed for it (standards-digest
§5.1).

### 8.3 Audio **[decided]**

- Codec: AC-3, 48 kHz, 192 kbit/s stereo or 384 kbit/s 5.1 for main tracks, at most
  448 kbit/s (software-survey §3.2; standards-digest §5.3). **[changed 2026-11-26]** The
  encoder is our own pure-Rust `tb-ac3`, not libav's (the owner does not trust FFmpeg's).
  Its dialnorm defaults to 24, so the libav
  default of −31 below no longer applies.
- Loudness: every input is normalized to −24 LKFS, with a true-peak limit of −2 dBTP, and
  every track carries `dialnorm` −24, including silence (standards-digest §5.4). The libav
  default of −31 must be overridden (software-survey §3.2). **[proposed]** For live input the
  normalizer is a slow gain driven by an `ebur128` measurement, not one-pass `loudnorm`, which
  adds up to 3 s of latency [K] (software-survey §3.2).
- The audio encoder runs on the sample clock at all times, encoding silence when there is no
  input. A pre-encoded silent AC-3 frame per channel layout is kept for the case where the
  encoder itself has failed (§6.6; software-survey §1).
- Up to three audio streams per program (manual §4.2). All tracks of a program are encoded
  frame-synchronously with identical PTS (standards-digest §4.3).

### 8.4 Statmux control loop

Statistical multiplexing here means dividing one carrier's fixed video pool (§6.5) between
its programs according to how hard each is to encode at the moment. No open-source statmux
exists to reuse (software-survey §5.2). **[decided]** The controller is ours; milestones
start with static caps and add the controller later (software-survey §5.3 item 7).


One controller per carrier, a pure function run on a fixed tick.

| Aspect | Design | Source |
|---|---|---|
| Cadence | Once per GOP period (0.5 s). **[proposed]** Programs' keyframes are staggered across the GOP period so their I-frames do not coincide | software-survey §5.3 item 3 |
| Inputs per program | Bits and average quantizer of each AU of the last GOP (`AV_PKT_DATA_QUALITY_STATS`), the configured weight `w`, floor `Rmin` and ceiling `Rmax`, and whether the program is showing the slate | software-survey §5.1 |
| Complexity | `C` = exponentially weighted mean of (bits × average quantizer) per GOP. Encoders do not expose look-ahead costs through libav | software-survey §5.1, §5.3 item 3 |
| Allocation | `R_i = Rmin_i + (pool − ΣRmin) · w_i·C_i^α / Σ w_j·C_j^α`, α between 0.7 and 1 | software-survey §5.3 item 3 |
| Limits | Clamp to `[Rmin_i, Rmax_i]`; change at most 15–20 % per tick; ignore changes below a hysteresis threshold | software-survey §5.3 item 3 |
| Slate | A program on its slate is pinned to `Rmin` **[proposed]** | — |
| Invariant | The sum of mux delivery rates `Σ r_p` never exceeds the pool | software-survey §5.3 item 4 |

`Rmax` for an MPEG-2 program is at most 15.99 Mbit/s with `D` = 0.5 s (§8.2).

### 8.5 Lower before raise **[decided]**

A rate change has two halves, the encoder's cap and the mux's delivery rate `r_p`, and their
order differs by direction (software-survey §5.3 item 4):

- **Decrease.** Lower the encoder's cap first. Lower `r_p` only after the change has taken
  effect in the encoder *and* one buffer delay `D` has passed, because AUs coded under the old
  cap are still in the queue and need the old rate.
- **Increase.** Raise `r_p` first, then the encoder's cap.
- **Funding.** An increase may use only rate released by decreases that have completed. In
  between, the released rate is sent as nulls.

### 8.6 Encoder lag

"Has taken effect" is measured, not assumed (software-survey §11 item 3). `set_rate` returns a
ticket; the encoder wrapper resolves it to the PTS of the first frame coded under the new cap:

- libx264: the change applies to "whichever frame is encoded next", about look-ahead plus
  thread delay after the frame being submitted (software-survey §5.1). The wrapper resolves
  the ticket conservatively to the frame submitted at the time of the call; the decrease is
  complete when that frame's AU has left the encoder plus `D`.
- mpeg2video: the encoder is closed and reopened at the next closed-GOP boundary with
  `rc_initial_buffer_occupancy` set from the wrapper's own model of buffer fullness, so the
  buffer model is continuous across the reopen (software-survey §5.1, §11 item 2). The ticket
  resolves to the first frame of the new GOP. This is the riskiest part of the statmux and
  has its own milestone. Whether a small libav patch that re-reads `bit_rate` is preferable
  is open (software-survey §12 item 4).
  **Correction (2026-11-25):** a reopened encoder that writes a different `bit_rate` in its
  sequence header violates H.262 §6.1.1.6, which requires repeated sequence headers to be
  identical apart from quantizer matrices. The legal
  mechanism is one continuous sequence whose header declares the program's ceiling rate, with
  `vbv_delay` 0xFFFF in every picture (variable-rate semantics), while the mux varies delivery
  below the ceiling. For libav `mpeg2video` this means keeping `rc_max_rate` fixed at the
  ceiling across reopens (or ending the sequence with `-seq_end 1`, which is legal but risky
  at junctions). The CUDA encoder spike implements the continuous-sequence method and states
  what the mux must guarantee in return (`tb_m2v_rate::vbv`).
- NVENC: as mpeg2video in timing, because each change forces an IDR (software-survey §9.2).

## 9. Ingest and fallback
Ingest turns a producer's connection into decoded frames and samples with the producer's
timestamps. The timeline mapper (§5.3) places them on the output clock, and the fallback
chain decides, frame by frame, what each program's encoders are fed.

### 9.1 RTMP push **[decided]**

The first ingest path accepts the producer's RTMP push (design-inputs "Feature candidates"). Holding the connection also keeps the producer awake,
because a connected RTMP sink counts as demand there.

- Implementation: **superseded (2026-11-26)**: the RTMP server is our own sans-io `tb-rtmp`,
  so libav never faces the network; only codecs go to the sandboxed decoder
  (`docs/SERVER.md` §5). Originally: libav's `rtmp` protocol with `listen=1`, through
  `ffmpeg-the-third`, one listener per configured input, reopened after every disconnect
  (software-survey §1). libav
  accepts one publisher per open (software-survey §11 item 9), which matches one producer
  instance per input. The fallback, if several publishers must share a port or connection-level
  control is needed, is the `rml_rtmp` crate (software-survey §1, §4.2).
- Each accepted connection is a new mapper session (§5.3). The producer starts every connection at
  a keyframe with timestamps from zero and reconnects on its own with backoff.
- After a wake, the producer's real content returns about 15 s later. The fallback chain (§9.5) covers that interval.
- The listener binds to a configured address. There is no authentication in RTMP as used
  here; the bind address and the host firewall are the access control. **[proposed]**

### 9.2 LL-HLS pull (later) **[decided: later]**

A small client using `reqwest` and `m3u8-rs`, with our own handling of `EXT-X-PART` and
`PRELOAD-HINT`, feeding fragmented MP4 to libav through a custom AVIO context. libav's own
`hls` demuxer treats LL-HLS as ordinary HLS (software-survey §1). The producer stamps
`PROGRAM-DATE-TIME` at push time, not capture time, so
it is not a better clock than arrival time. An HLS request also counts as demand for 30 s.

### 9.3 Decode

Software H.264 decode through libav, one thread per input, costing roughly 0.1–0.2 core per
1080-line stream [K] (software-survey §1). Decoding starts at the first keyframe of a session.
A decode error burst (default 5 errors in 2 s) ends the session, so the fallback chain takes
over instead of showing corruption. NVDEC offload is a later optimization (software-survey
§9.6 item 3).

Decoded frames go to the mapper in pooled buffers through a bounded queue. If the queue is
full the decoder drops the frame and counts it; it never waits.

### 9.4 Pass-through (later) **[decided: later]**

Inputs that already meet the output constraints (rate-capped, suitable GOP, AC-3 audio) may
later bypass the encoders (design-inputs "Decisions so far"). That path needs compressed-domain
switching at access points with the rules in standards-digest §1.8 and is deliberately outside
the first design. The producer cannot use it: its output is not rate-capped and has no audio.

### 9.5 Fallback chain **[decided]**

Every program has an ordered chain of sources: live, then standby, then slate with silence
(design-inputs "Robustness"). The selector is a pure state machine evaluated once per output
frame tick.

| Level | Source | Selected when |
|---|---|---|
| 1 Live | The program's configured input | Its mapper session has delivered frames continuously for `promote_after` (default 3 s) |
| 2 Standby | An optional second input, or a looping local file **[proposed]** | Level 1 is unavailable and this source is healthy |
| 3 Slate | A still image (or generated card with the service name) and digital silence | Always available; generated in-process with no dependency on libav I/O |

Rules **[proposed]**:

- Demotion is immediate in effect: when the selected source has no frame due, the frame clock
  repeats the last frame (§5.4). After `hold` (default 1 s) of repeats the selector moves down
  the chain. Short gaps therefore show a freeze, not a slate.
- Promotion requires the higher source to be healthy for `promote_after`, so a flapping input
  does not flap the picture.
- Video cuts between sources take effect on the first frame of the next output GOP, which is
  at most one GOP (0.5 s) away, so every cut lands on an I-frame without disturbing the fixed
  keyframe cadence. Audio crosses over with a 20 ms fade at the same instant.
- Below level 3 there is one more layer, inside the mux, for encoder failure: §6.6.

## 10. Modulator control and firmware defects

The modulator is managed through CGI forms on a Boa web server, with root FTP and telnet on
the same credential (device §1). Its live configuration is an 8 KB EEPROM image; a text
"config file" on flash is a second copy that the firmware reconciles at boot (firmware §0;
corruption-48 §2).

### 10.1 Client **[decided]**

A typed client: one Rust type per form, parsed from the page and serialized back.

- Session: `GET login.cgi` for a `session_id`, `POST` the login form, then pass
  `?session_id=` on every request. A second Admin login invalidates the first, and the idle
  timeout is 15 minutes (device §2). The client detects the "Logged Out" page and logs in
  again with backoff. `POST /api/device/pause` (§13.1) stops the supervisor so a person can use
  the web UI without being logged out.
- One request at a time, at least 1 s apart, with a 30 s timeout and one retry for reads. The
  box has a 125 MHz soft CPU, and one page fetch was seen to time out once (device §1, §2).
- Writes are whole forms. The client fetches the form, checks that its set of field names is
  exactly the expected one, changes only the intended values, posts every field back, then
  fetches the page again and compares. `statusedit.cgi` zeroes any row missing from a POST
  (corruption-48 §3, "POST truncation" row; §5 item 4). An unexpected field set aborts the
  write.
- Credentials come from a file or systemd credential, never from the TOML file.

Allowed writes **[proposed]**: exactly two forms.

| Form | Used for | Rules |
|---|---|---|
| `statusedit.cgi` | Stream URL and name for input slots 1–16 | All 16 rows in every POST. Names are 1–10 alphanumeric characters, never empty and never containing `,` `-` `.` (manual §4.2; corruption-48 §2 "Triggers and damage") |
| `qamconfig.cgi` | First channel, carrier on/off, QAM mode, plan and level per RF module | Only as the four-module sequence of §10.3 |

Hard refusal list **[decided]**, enforced in one function that every request passes through
and covered by unit tests that need no network:

| Refused | Why |
|---|---|
| Any request to `admin.cgi` or `admin_redir.cgi`, by any method | Holds reboot, flush, delete-config, passwords and every editable network setting (device §11.3) |
| Any URL whose query contains a parameter starting with `btn` | The CGIs read action fields from the query string; `btnReboot`, `btnFlush` and `btnDeleteConfig` act on a GET (device §9) |
| Any change to network or login settings, through any channel | Owner's rule (design-inputs "Facts from the owner") |
| `config.cgi` upload | The restore parser has unbounded copies and index bugs (corruption-48 §3 row L4, §5 item 3) |
| `mode.cgi` writes | The unit is already in MPTS mode; a switch to SPTS would retune RF module 2 from stale bytes (corruption-48 §1 "Knock-on") |
| `eas.cgi`, `license.cgi`, `time.cgi`, `network.cgi`, `errorlog.cgi`, `diagnostic.cgi`, `debug.cgi` writes | Not needed in MPTS mode; EAS actions replace every program (manual §4.5) |
| Telnet commands outside a fixed list of exact strings; all FTP at run time | Telnet and FTP are root (device §8) |

Reads use bare GETs with only `session_id`, which were all exercised safely during the survey
(device §3, §11.3).

### 10.2 Supervisor

A pure state machine fed by polled pages. Desired state comes from the configuration file
(§13.2): TS mode MPTS, the RF plan, and one stream URL and name per carrier.

| Poll | Interval **[proposed]** | What is read |
|---|---|---|
| `status.cgi` | 30 s | Per carrier: stream URL, input present, bitrate, RF channel and on/off, module lock; page header: mode, uptime, temperature (device §3; manual §5) |
| `qamconfig.cgi?slotNumber=1..4` | 5 min | The RF plan as stored |
| `streaminfo.cgi` | 5 min | PMT PIDs, program numbers and stream types the box detects in each MPTS, compared with what we emit (manual §4.3) |
| `diagnostic.cgi`, `debug.cgi` | 5 min | Oversubscription, TS sync errors, buffer overflows, 100 ms timeouts, bitrates (device §3, §11.3) |
| `errorlog.cgi` | 5 min | Event log lines; the log is lost on power loss (manual §5) |

The RF plan is read from the rendered QAM pages, not from the configuration export, because
the export is known to disagree with the modulators on this unit (device §5).

Drift is any difference between desired and observed state. The supervisor has two modes:
`report` (default until M4 has passed) and `apply`, which rewrites the affected form. Writes
are rate-limited (one reconcile per 10 minutes, three per day). If the same drift returns
after three re-applies, the supervisor stops writing and raises an alarm, because something
else is changing the configuration. A drop in uptime means the box rebooted; that triggers an
immediate full comparison.

### 10.3 Firmware defects A and B

Defect A: a QAM Config save writes RF-channel bytes through an index that is right for only
one of the two tables. In MPTS mode a save for module *m* writes the MPTS table correctly and
overwrites SPTS-table records of other modules. `mux_sync` later retunes modulators from the
table of the current mode. Confirmed in disassembly and by the bytes in this unit's EEPROM
(corruption-48 §1, §8).

Mitigation **[decided: save order]**, as implemented by the client:

1. Verify the page header says MPTS. Never write `mode.cgi`.
2. Save QAM Config for all four RF modules, 1 to 4, in one operation, every time any of them
   changes. A single-module save does not exist in the client's API. Saving all four in the
   current mode also repairs stale RF-channel bytes (corruption-48 §1 "Aftermath"; §5 item 2).
   On this unit the stored MPTS RF channels for modules 2 and 3 are zero while the modulators
   are tuned (corruption-48 §1 "Live fingerprint"), so this sequence is also the first
   supervised write.
3. Read all four pages back and compare; then compare the status page's RF column.
4. In MPTS mode the stray bytes land only in the unused SPTS table, so Unit Name and Location
   are not at risk; they would be if module 4 were saved in SPTS mode (corruption-48 §1).

Defect B: at boot, in Automatic configuration mode, `update_config` parses the flash config
file and overwrites every stream record with what it parsed, zero-filling records that have no
line. This unit is in Automatic mode and holds a 48-line file in a format the firmware
mis-parses (corruption-48 §2). What exactly it wrote at the last boot is not fully explained
(corruption-48 §2 "Open discrepancy").

Mitigation **[decided]**: Configuration Mode is set to Manual before the first configuration
write (design-inputs "Decisions so far"; corruption-48 §5 item 1). That setting lives on
`admin.cgi`, which the client refuses, so the owner makes this one change by hand. What a
save of that form writes is now known (firmware §4.1): only the fields in the POST are
applied, so the save carries Manual mode together with the GigE re-addressing (§18 item 2).
Still unresolved (§18): the supervisor needs a way to confirm Manual mode without requesting
`admin.cgi`. Independently, the supervisor treats the
event-log line "Difference detected in Config File" as an alarm and re-checks everything after
any reboot.

### 10.4 Firmware defect D

Defect D: `mux_sync` rebuilds a per-stream program table every 30 s but clears only a few
bytes of it and appends entries without limit. With input streaming, each program's write
position advances 8 bytes per elementary stream per pass, through the following records and
then past the end of the heap block into other memory; the CPU has no memory protection. The
defect is proven in disassembly. Its consequences are inferred and have not been observed,
because this unit has never had input (corruption-48 §2a). In MPTS mode the note estimates
about 111 KB between the last stream's records and the end of the block, and "one to several
days" to cross it, less if a counter is overwritten on the way (corruption-48 §2a
"Consequences"). At 8 bytes per elementary stream per pass, a program with four streams
advances 32 bytes per pass, which gives roughly 29 hours for 111 KB [I, arithmetic on the
note's figures]. This is the defect that best fits the friend's report of corruption in MPTS
mode (corruption-48 §0).

Nothing in the configuration prevents it (corruption-48 §2a "Workarounds"). Three operational
mitigations are possible. All three are **untested**.

| Option | What it does | For | Against |
|---|---|---|---|
| 1. Restart `mux_sync` periodically from telnet (`kill`, then start it again) | Resets the table without a reboot | No carrier outage if the daemon restarts cleanly | `mux_sync` is the main control daemon that programs the FPGAs and modulators (firmware §3 inventory). What a restart does to RF output is unknown. Starting a daemon from a telnet shell and leaving it running is unproven on this shell |
| 2. RAM-only binary patch after each boot | One instruction changed so the whole table is cleared each pass; the patched file replaces `/bin/mux_sync` in the RAM filesystem and the daemon is restarted (corruption-48 §2a "Workarounds") | Removes the defect instead of racing it; nothing persistent is changed | The patch is untested. It needs the restart of option 1 to work, an upload over FTP after every boot, and the owner's approval to run a modified binary |
| 3. Monitor and reboot on a schedule | Reboot the box (telnet `epcs -c`, a documented method, manual §7) at a quiet time, at an interval shorter than the time to overrun | Uses only vendor-documented behavior | Every carrier is off the air for one boot time (not yet measured). Each reboot runs defect B unless Manual mode is set. The safe interval is not known |

Recommendation **[proposed]**:

- From the first day input is applied: option 3, with a daily reboot at a configured time and
  only after Manual mode is confirmed. A day is inside the 29-hour estimate above but that
  estimate is weak, so M0 also watches for the symptoms (garbage on the Stream Info page,
  unexpected reboots, event-log errors) with a stream left running.
- In parallel, offline: run `mux_sync` under an emulator with fake FPGA registers
  (corruption-48 §7) to measure the real growth rate and to validate the one-instruction
  patch.
- In M4, with the owner present and the probe watching: test whether killing and restarting
  `mux_sync` disturbs the RF output. If it does not, adopt option 2 (patch applied by the
  supervisor after each detected boot), with option 1 as its unpatched fallback, and drop the
  daily reboot. If it does, stay on option 3 and tune the interval from the emulator result.

Whichever is active, the supervisor exports the box's uptime and the time since the last
table reset, and alarms when the configured limit is exceeded.

## 11. Monitoring
Three layers watch the output, from nearest to furthest: a checker on each mux's own output
before it is sent, the modulator's status pages (§10.2), and a tuner on the RF output. A
fault is located by which layers see it.

### 11.1 RF probe

An HDHomeRun with clear-QAM support, fed from the modulator's output through about 30 dB of
attenuation **[decided]**. The box delivers about +40 dBmV per carrier and a tuner wants
roughly 0 to +10 dBmV; with 16 carriers the total is about 12 dB above one carrier
(design-inputs "Monitoring probe"). The unit has been bought and has not arrived; its model
and tuner count are [unknown].

Control is by running `hdhomerun_config` as a subprocess (software-survey §7.1). The probe
driver rotates one tuner through the configured carriers:

| Step | Command | Checked |
|---|---|---|
| Tune | `set /tunerN/channel auto:<frequency_hz>` (or `qam256:<frequency_hz>`) | — |
| Status | `get /tunerN/status` | `lock=qam256`; `ss`, `snq`, `seq` (100 means no uncorrectable errors); `bps` |
| Lineup as received | `get /tunerN/streaminfo` | Each program's `major.minor name` equals the configured service |
| Capture | `save /tunerN <file>` for 10–30 s, into a memory-backed directory | The tool's own overflow, transport-error and sequence-error counts |
| Analysis | Our parsers, then TSDuck (`continuity`, `pcrverify`, `analyze`, `tables`) on the same capture | Continuity, table cycles and CRCs, PSIP cross-checks of §15, PCR interval |

Carrier frequencies come from the RF plan (device §4 lists the current channels and center
frequencies). One rotation of 16 carriers is estimated at 5–8 minutes with one tuner; extra
tuners shorten it. Captures are deleted after analysis unless a check failed, in which case
the capture is kept (bounded count) as evidence.

The probe sees the stream after the modulator's null handling and PCR correction, so PCR
accuracy there measures the box, not our mux; our mux is checked before the pacer (§15). What
the box rewrites is [unknown] until M0.

Probe loss never affects output. It raises an alarm, because with the probe gone the system
is running without independent verification.

### 11.2 Continuous self-checks

The stream checker of §15 (layer 3) runs in production on a tap of every carrier's slot
stream, taken after the mux and before the pacer. It evaluates the transport indicators and
PSIP cross-checks of standards-digest §11.1 and §11.2 continuously: sync, continuity per PID,
PAT, PMT and PSIP table intervals, section CRCs, PCR interval and PCR value against slot
index, PTS interval, the decoder buffer models, unreferenced PIDs, and the TS rate and null
share over one-second windows. Findings are counters (§11.3) and log lines with the carrier,
PID and slot index. The checker is a separate implementation from the mux's own scheduler
model, so that one error cannot hide in both **[proposed]**. It costs one pass over about
25,800 packets per second per carrier.

### 11.3 Metrics and logs

`tracing` with spans per carrier, program and input. Metrics are atomic counters and gauges
per component, rendered by hand as Prometheus text at `/api/metrics`. The names below are the first set.

| Area | Metrics (labels) |
|---|---|
| Carrier | `ts_packets_total{carrier,class}` with class = video, audio, psi, psip, pcr_only, null; `null_share{carrier}`; `mux_underrun_slots_total{carrier}`; `mux_lead_seconds{carrier}` |
| Conformance (self-check) | `cc_errors_total{carrier}`; `pcr_interval_seconds_max{carrier,program}`; `table_interval_seconds_max{carrier,table}`; `tstd_violations_total{carrier,buffer}`; `late_au_total{carrier,program}`; `section_crc_errors_total{carrier}` |
| Pacer | `datagrams_sent_total{iface}`; `send_errors_total{iface}`; `send_lateness_seconds{iface}` (histogram); `null_datagrams_total{carrier}` |
| Program | `program_source{carrier,program}` (1 live, 2 standby, 3 slate); `source_switches_total`; `frames_repeated_total`; `frames_dropped_total`; `encoder_restarts_total`; `encode_seconds` (histogram); `video_queue_seconds`; `audio_queue_seconds` |
| Timeline | `timeline_resets_total{input}`; `skew_ppm{input}`; `jitter_buffer_seconds{input}`; `decode_errors_total{input}`; `input_connected{input}` |
| Statmux | `rate_cap_bps{carrier,program}`; `delivery_rate_bps{carrier,program}`; `complexity{carrier,program}`; `avg_qp{carrier,program}`; `rate_changes_total`; `pool_unallocated_bps{carrier}` |
| Audio | `loudness_lkfs{carrier,program,track}`; `gain_db{…}`; `silence_frames_total{…}` |
| Clock | `clock_frequency_error_ppm`; `clock_synchronized`; `stt_utc_error_seconds` |
| Device | `device_reachable`; `device_uptime_seconds`; `device_drift_items`; `device_writes_total{form}`; `device_refused_total{rule}`; `device_input_present{carrier}`; `device_bitrate_bps{carrier}`; `device_overflow_total{carrier}`; `device_rf_locked{module}`; `device_seconds_since_table_reset` |
| Probe | `probe_reachable`; `probe_lock{carrier}`; `probe_ss{carrier}`; `probe_snq{carrier}`; `probe_seq{carrier}`; `probe_lineup_match{carrier}`; `probe_cc_errors_total{carrier}`; `probe_last_scan_age_seconds{carrier}` |
| Process | CPU seconds per thread class; RSS; uptime |

## 12. Failure matrix

"Viewer" is what a TV on the affected channel shows.

| Failure | Viewer sees | System does | Detected by |
|---|---|---|---|
| Producer disconnects or goes idle | Freeze for `hold` (1 s), then standby or slate with silence. Other channels unaffected | Mapper session ends; selector moves down the chain at the next GOP; listener reopens; statmux pins the program to `Rmin` | Ingest state, `program_source` metric |
| Producer returns | Slate until the input has been healthy for `promote_after` (3 s), then a cut at a GOP boundary | New session, new offset; selector moves up | Same |
| Timestamp reset or jump, connection kept | Nothing if the buffer covers it; otherwise a freeze of under 1 s | Mapper starts a new session and re-anchors; output timestamps are unaffected | `timeline_resets_total` |
| Producer clock drift | One repeated or dropped frame at long intervals; audio unaffected | Skew estimate trims the audio resampler; frame clock repeats or drops | `frames_repeated_total`, `frames_dropped_total`, `skew_ppm` |
| Corrupt input | Freeze, then slate | Decode error burst ends the session | `decode_errors_total` |
| Video encoder stalls | Freeze | Mux sends nulls and PCR-only packets for that program; encoder restarted after `encoder_timeout` (2 s), resuming with a keyframe | `encoder_restarts_total`, mux queue-empty counter |
| Encoder thread panics or libav returns a fatal error | Freeze of a few seconds | Same restart path. A crash of the whole process inside libav is systemd's to restart; see next row | Same |
| Process crash or restart | All carriers lose input for the restart time; TVs show their no-signal behavior or a frozen picture [unknown, M0] | systemd restarts the service; slot counters and STC start fresh (a time-base jump the TV sees as a new stream); table versions and `source_id`s continue from SQLite | systemd, uptime metric |
| CPU overload, encoders late | Occasional freeze of up to one GOP on the affected programs | Late AUs dropped to the next keyframe; if sustained, the program supervisor steps that program down a configured ladder (lower resolution or frame rate) **[proposed]** | `late_au_total`, encode time histogram |
| CPU overload, mux thread late | Brief glitch on that carrier | Pacer sends null datagrams for the missed slots; PCRs stay exact; mux skips ahead | `mux_underrun_slots_total` |
| Output socket error | Carrier content missing for the duration | Datagram skipped, socket reopened after repeated errors, timing unchanged | `send_errors_total` |
| Network path to the box lost | Carrier carries no programs; the box reports "assigned but not streaming" (device §11.4) | Nothing changes on our side; supervisor and probe alarm | Supervisor status poll, probe |
| Modulator reboots | All 16 carriers off for the boot time [unknown] | Supervisor sees uptime drop, runs a full comparison, re-applies if in `apply` mode, re-applies the defect D mitigation | Supervisor, probe |
| Modulator configuration lost or changed | Depends on what changed: wrong or missing channels | Drift reported; re-applied in `apply` mode within the write limits; alarm if it recurs | Supervisor |
| Modulator web UI unreachable | Nothing | Output continues; alarm | Supervisor |
| Probe lost | Nothing | Output continues; alarm for loss of verification | Probe driver |
| Host clock unsynchronized | TV clock and guide placement drift over time | STT still sent; alarm | Clock metrics |
| Control plane or SQLite failure | Nothing | Mux, encoders and pacer do not depend on either after start; guide data stops updating | Health endpoint |

## 13. Operator interface and stored state

### 13.1 Control API

`axum` on a configurable address, default `127.0.0.1:8090` **[proposed]**. A JSON API, a small
embedded status page and Prometheus metrics. No authentication in the first version; the bind
address is the access control.

```
GET   /api/state                         everything below in one document
GET   /api/carriers                      per carrier: rate, null share, programs, pacer stats
GET   /api/carriers/{c}/programs/{p}     source in use, encoder, rate cap, queue depths
POST  /api/carriers/{c}/programs/{p}/source   { "force": "live" | "standby" | "slate" | null }
GET   /api/services                      lineup: name, channel, source_id, placement
GET   /api/services/{id}/events          guide schedule for one service
PUT   /api/services/{id}/events          replace the schedule (title, start, length, text, rating)
POST  /api/services/{id}/now             "now/next" update from a live producer
GET   /api/statmux/{c}                   complexities, allocations, pending rate changes
GET   /api/device                        desired vs observed modulator state, drift, last writes
POST  /api/device/pause                  { "for": "30m" }   stop polling and writes
POST  /api/device/reconcile              run one comparison now; apply only if mode = "apply"
GET   /api/probe                         last result per carrier
POST  /api/probe/scan                    start a rotation now
POST  /api/config/reload
GET   /api/metrics                       Prometheus text format
GET   /healthz
```

### 13.2 Configuration

One TOML file, path from `--config` or `TOTALBLONDER_CONFIG`. `serde` with
`deny_unknown_fields`, humantime durations, and an example file `totalblonder.example.toml`
listing every key. A safe subset reloads without a restart:
guide data, fallback timings, statmux weights and limits, probe and supervisor intervals.
Anything that changes a carrier's program set, PIDs, formats or addresses needs a restart in
the first version.

Sketch:

```toml
[control]
bind = "127.0.0.1:8090"

[persistence]
sqlite = "/var/lib/totalblonder/state.db"

[output]
ts_rate = 38_800_000            # bit/s per carrier; validated < 38_810_701
null_floor = 0.02
packets_per_datagram = 7
framing = "udp"                 # udp | rtp
pcr_interval = "30ms"
mux_lead = "20ms"

[[output.interface]]            # one pacer thread each
name = "gige1"
device = "enp5s0"
carriers = [1, 2, 3, 4, 5, 6, 7, 8]
multicast_ttl = 1
realtime = false                # SCHED_FIFO

[psip]
gps_utc_offset = 18
dst_rule = "us"
eit_windows = 8
ett_unique_extension = true

[[carrier]]
id = 1
tsid = 0x0101
destination = "239.10.0.1:5000"

  [[carrier.program]]
  number = 1
  service = "wall"
  video = { codec = "mpeg2video", width = 1280, height = 720, fps = "60000/1001" }
  rate = { min_kbps = 3000, max_kbps = 12000, weight = 1.0 }   # static: min = max
  audio = [ { language = "eng", layout = "stereo", kbps = 192 } ]
  source = { live = "studio", standby = "none", slate = "default" }

[[service]]
id = "wall"
name = "WALL"                   # short_name, at most 7 characters
long_name = "Live Wall"
channel = "20.1"                # two-part, unique across the plant
# source_id is allocated on first run and stored; set it here only to pin it

[[input]]
id = "studio"
kind = "rtmp_listen"
bind = "127.0.0.1:1935"
path = "/live/wall"

[timeline]
jitter_buffer = "1s"            # J
reset_threshold = "500ms"
max_skew_ppm = 500

[fallback]
hold = "1s"
promote_after = "3s"
encoder_timeout = "2s"

[statmux]
enabled = false
alpha = 0.85
max_step = 0.15

[device]
base_url = "http://10.42.0.184"
credentials_file = "/run/credentials/totalblonder/eqam"
reconcile = "report"            # report | apply
status_interval = "30s"
defect_d = "reboot"             # none | reboot | restart_mux_sync | patch
defect_d_at = "04:30"

  [[device.rf_module]]          # exactly four
  module = 1
  first_channel = 2
  qam = "256B"
  plan = "STD"
  carriers_on = [true, true, true, true]

[probe]
enabled = true
device_id = "auto"
tuner = 0
capture = "15s"
```

Validation at load, all fatal: unique TSIDs, unique channel numbers, unique destinations, at
most 6 programs per carrier and 3 audio streams per program, PIDs inside 0x0030–0x1FEF, the
sum of `min_kbps` plus audio, tables and the null floor within `ts_rate`, video formats legal
for the codec (§8.2), and `max_kbps` within the codec's buffer limit.

### 13.3 Persistence

SQLite through `rusqlite` (bundled), one file, WAL mode. It holds only what
must survive a restart:

- `services(id, source_id, name, …)`: `source_id` allocation, never reused (§7.1).
- `table_versions(carrier, pid, table_id, extension, version, content_hash)`: so versions
  increment across restarts and stay unchanged when content is unchanged (§7.3).
- `events(service_id, event_id, start, length, title, text, rating)`: the guide schedule;
  `event_id` is allocated per service and stable for the event's life (standards-digest §3.5).
- `device_log(time, action, form, before, after)`: every write sent to the modulator.
- `probe_results(time, carrier, …)`: recent history, bounded.

The TOML file is the source of the lineup and of all settings. SQLite never overrides it. No
media is written to disk except failed probe captures (§11.1).

## 14. Capacity estimates

**Every figure in this section is an estimate.** The per-channel costs were measured on
`bagel` (Ryzen 7 5800X), one thread, on synthetic test patterns, with decode and scaling
excluded (software-survey §9.4). `nixark` has different cores, its 8 CPUs may be threads
rather than cores, and real content is harder. M3 and M6 replace this table with
measurements.

Cost model: cores per channel = 2 × the synthetic encode figure (the survey's allowance for
real content) + 0.15 for decoding a 1080-line H.264 input [K] (software-survey §9.4). Scaling,
AC-3 encode and loudness measurement are not included and not yet measured. 1.5 cores are
reserved for muxes, pacers, audio and control **[proposed]**, leaving 6.5, 14.5 and 22.5.

| Output | Synthetic encode (cores) | Estimated cores per channel | Channels on 8 vCPUs | on 16 | on 24 |
|---|---|---|---|---|---|
| MPEG-2 480-line, 29.97 | 0.057 | 0.26 | 25 | 55 | 86 |
| MPEG-2 720p59.94 | 0.17 | 0.49 | 13 | 29 | 45 |
| MPEG-2 1080p29.97 | 0.30 | 0.75 | 8 | 19 | 30 |
| MPEG-2 1080i29.97 | 0.59 | 1.33 | 4 | 10 | 16 |
| H.264 x264 veryfast 480-line | 0.10 | 0.35 | 18 | 41 | 64 |
| H.264 x264 veryfast 720p59.94 | 0.44 | 1.03 | 6 | 14 | 21 |
| H.264 x264 veryfast 1080p29.97 | 0.55 | 1.25 | 5 | 11 | 18 |
| H.264 x264 medium 1080p29.97 | 1.14 | 2.43 | 2 | 5 | 9 |

Reading the table against the bandwidth budget (§6.5): a full plant of 96 programs fits in
CPU only as standard definition, and 96 mostly-HD channels need on the order of 100–130 cores
(software-survey §9.6). The first milestones need 2–4 programs, which fit on 8 vCPUs in every
row.

GPU (RTX 2000 Ada):

| Use | Possible | Limits | Source |
|---|---|---|---|
| MPEG-2 encode with NVENC | No | NVENC has no MPEG-2 encode | software-survey §9.2 |
| H.264 encode with NVENC | Yes, progressive only | No interlaced encode on Ada; every rate change forces an IDR; consumer cards are limited to 12 sessions, qualified professional cards are not, and whether this card qualifies is [unknown]; capacity per card not measured | software-survey §9.2 |
| H.264 decode with NVDEC | Yes | Saves the 0.1–0.2 core per input [K] | software-survey §9.2, §9.6 |
| Friend's CUDA MPEG-2 encoder | Unknown | Not seen: rate control, buffer compliance, interlace and quality all unknown | design-inputs "Facts from the owner" |

Other resources, estimated:

| Resource | Estimate | Basis |
|---|---|---|
| Network out | 16 carriers ≈ 58,000 datagrams/s, ≈ 620 Mbit/s payload, split over two ports | software-survey §8 |
| Threads | About 3 per program plus 1 per input, 1 per carrier and 1 per interface: about 400 at 96 programs | §3 |
| RAM | Dominated by raw-frame queues: a 1080-line 4:2:0 frame is about 3 MB; with queues bounded at 8 frames per stage, well under 100 MB per HD program | arithmetic |

### 14.x Our own CUDA MPEG-2 encoder (added 2026-11-25) [measured]

A CUDA MPEG-2 encoder was built and optimized as a C++ spike and ported to Rust (`docs/ARCHITECTURE.md`; the C++ was retired once
the Rust held its gate and capacity). It is separate from the friend's encoder, which is
still unseen. It changes the capacity picture: video encode moves off the 8 vCPUs.

Sessions held at ≥ 99 % of ticks on time on the RTX 2000 Ada in `nixark`, hard content, all
coding tools on, staggered GOPs, frames uploaded from host memory, card otherwise idle:

| Format | Card to itself | Shared, typical | Shared, worst case |
|---|---|---|---|
| 1080i29.97 | 67 | 65 | 58 |
| 1080p29.97 | 75 | 70 | 62 |
| 720p59.94 | 90 (86–88 sustained, estimated) | 80 | 71 |
| 704×480i29.97 | 400 | 365 | 325 |
| Mixed, units of 1×1080i + 1×720p + 4×480i | 24 | 23 | 21 |

**[decided 2026-11-25]** The card is shared with Plex and Jellyfin. The owner's stated load is
1–3 simultaneous transcodes, some from 2160p sources, tone mapping under 10 % of traffic
(design-inputs). "Typical" budgets the encoder's tick at 85 % of the frame period (up to
three real-time transcodes, no tone mapping; worst measured tick ratio 1.13). "Worst case"
budgets 75 % (a tone-mapped 2160p stream in the mix; 1.18–1.33). Source: the spike's
measurements on the card. An earlier version of this table divided by 1.6; most of
that slowdown was an ordering fault in the encoder's own upload, since fixed.

Not covered by either budget: a transcoder running unthrottled (a burst while it fills its
buffer, typically seconds). One unthrottled 2160p transcode takes about half the card
(ratio about 2.0), and every tick is late for the duration. Budgeting for that would halve
capacity, so the encoder needs a deadline-miss policy instead: repeat the previous picture
and count the event **[proposed]**. At the typical planning point, the real-time worst case
makes 2–4 % of pictures repeat, in runs shorter than a second (measured). The 720p, 1080p
and mixed shared figures are estimates; 1080i and 480i were measured next to competitors.

Limits of these figures: runs of at most three minutes; the card is thermally limited at
capacity (90–91 °C, clock reduced); quality is 0.54 dB luma PSNR below ffmpeg `mpeg2video`
on average at equal bitrate, of which 0.48 dB is adaptive quantization by design; nothing has
passed through a transport stream, the modulator or a TV; host CPU for decode, scaling, audio
and mux is not included and is now the binding resource on 8 vCPUs. The encoder enforces the
buffer model strictly and changes rate under a constant sequence header (§8.5).

## 15. Test strategy

Four layers, from fastest to slowest. The first two run in `nix flake check`.

1. **Unit tests on pure state machines.** The slot scheduler, carousel, buffer models,
   timeline mapper, frame and sample clocks, fallback selector, statmux controller (including
   the invariant `Σ r_p ≤ pool` under random rate requests and lags), device supervisor and
   refusal function, EIT window roll. Property tests where the input space is large: random
   input timestamp sequences with resets and gaps must always produce a frame per tick and
   monotonic output timestamps.
2. **Golden-table tests against TSDuck.** Each generator's sections are decoded by TSDuck
   (`tstabdump`, `tstables --atsc`) and the decoded form compared with the expected lineup.
   TSDuck has full serialize and deserialize for every table and descriptor we emit
   (software-survey §2.1). TSDuck is a test oracle only and never a runtime dependency
   **[decided]** (software-survey §2.5). `mpeg2ts-reader` re-parses our packets in Rust tests;
   `ffprobe` and libdvbpsi's `dvbinfo` are second opinions, because TSDuck has at least one
   known misreport (software-survey §2.3, §11 item 8).
3. **Stream self-checks, modeled on ETSI TR 101 290.** A checker module consumes a TS (the
   mux output in tests and continuously in production on a tap before the pacer; probe
   captures after the modulator) and evaluates the indicators of standards-digest §11.1 with
   the cable thresholds, plus the PSIP cross-checks of standards-digest §11.2. In CI the mux
   runs for 60 s of stream time against synthetic encoders with no real-time pacing and must
   produce zero findings; the same file then goes through `tsanalyze --atsc --json`, `tsp -P
   continuity -P pcrverify`, and the JSON is asserted on (software-survey §2.5, §2.6 b).
4. **Soak on hardware with the probe.** Each milestone names a duration. Pass conditions are
   counters, not impressions: continuity errors, underruns, late AUs, table intervals, probe
   lock and `seq`, lineup match, memory slope.

Fault injection is part of layers 1 and 4: input kills, timestamp resets, stalled encoders
(a test encoder behind the trait that sleeps), a mux thread paused with `SIGSTOP`, a pulled
network cable, a modulator reboot.

Final acceptance for anything a viewer sees (guide, captions, audio selection, ratings) is on
real TVs, recorded by model (software-survey §7.2).

## 16. Milestones

Each milestone ends in something observable on a TV or on the probe and is useful on its own.
The order retires the largest unknowns first: what the box tolerates (M0), whether our own
mux satisfies real TVs (M2), whether the timeline survives real producers (M3), and whether
the box can be kept configured (M4). Every exit criterion is a check that can be run and
recorded; "the M0 tolerance" in later milestones means the figure measured in M0 question 4.

### M0 — spike on real hardware

No Rust. The ffmpeg-plus-`tsp` pipeline of software-survey §2.6 sends one CBR MPTS with two
programs and minimal PSIP to carrier 1. Helper files (`psip.xml`, `eit0.xml`, `mgt.xml`,
`stt-loop.sh`, a `run.sh` with the command line, an `analyze.sh` with the checks of
software-survey §2.6 b, and a `README.md` with the procedure) go in `tools/spike/`. Injection
periods are tightened to MGT 100 ms and CVCT 250 ms, since the loopback run exceeded the
limits at 150 and 400 (§7.2).

Preconditions, all needing the owner: Configuration Mode set to Manual (§10.3); the data
network decided (§4.2); slot 1's stream URL entered by hand on the Status Edit page; a TV or
the HDHomeRun connected through attenuation.

Instruments: the box's own pages by bare GET (`status.cgi`, `streaminfo.cgi`,
`diagnostic.cgi`, `debug.cgi`, `errorlog.cgi`), the HDHomeRun if it has arrived, a TV, and
`tcpdump` on the sending interface.

| # | Question | How it is measured | Decision that depends on it |
|---|---|---|---|
| 1 | UDP or RTP? | Same stream sent each way with the matching URL; input-present flag and bitrate on `status.cgi`, error counters on `debug.cgi`, probe lock and continuity | Default `framing` (§4.1) |
| 2 | Packets per datagram | `tsp -O ip --packet-burst` at 7 (enforced), 4 and 1 | Whether 7 is required, and whether short datagrams are allowed |
| 3 | Unicast or multicast; what a unicast URL means | Multicast group with `tcpdump` watching for the box's IGMP reports; unicast to the GigE address; both readings of the unicast URL (manual §4.1) | Addressing plan (§4.2); whether the data network needs an IGMP querier |
| 4 | Jitter tolerance | `regulate --packet-burst` raised from 7 to 16, 64 and 256 at the same average rate; then `tc netem` delay jitter on the interface; overflow and timeout counters on `debug.cgi`, continuity on the probe | Pacer precision target; whether `SCHED_FIFO` or `SO_TXTIME` is needed (§4.3) |
| 5 | Behavior near the rate ceiling | Mux rate stepped through 37.0, 38.0, 38.5, 38.80, 38.81, 38.82 and 39.0 Mbit/s; Oversubscription column, overflow counters, probe continuity and null share | Confirms 38.80 Mbit/s or lowers it; the null floor (§6.5) |
| 6 | Do EIT sections pass in MPTS mode? | Probe capture decoded with `tstables --atsc`; guide entry on a TV | Whether the guide (M5) is possible on this unit |
| 7 | What does the box rewrite? | Bytes sent (tee to a file) compared per PID with the probe capture: table sections, continuity counters, PCR values, adaptation-field flags, null share | Whether probe checks may compare section bytes; how PCR checks after the modulator are interpreted (§11.1) |
| 8 | Defect D with live input | Stream left running at least 48 h; every 10 minutes record uptime, `streaminfo.cgi`, event log, `status.cgi`; look for garbage rows, reboots, errors | Which mitigation and what interval (§10.4) |
| 9 | Loss and recovery | Stream stopped for 1 s, 10 s, 60 s and restarted; time for the box and the TV to recover; what the TV shows | Process-restart behavior (§12); whether a restart needs anything special |
| 10 | Does a TV show the channels? | Scan on at least one TV: numbers 50.1 and 50.2, names, clock, guide entry | Whether the minimum PSIP set is enough; whether RRT or ETT is needed (software-survey §12 item 2) |
| 11 | Reboot time of the box | With the owner's approval, one telnet `epcs -c`; time until carriers lock again; check that settings survived | Cost of defect D option 3 |
| 12 | H.264 through the box (optional) | The same pipeline with `libx264` | Early evidence for the codec mix (M7) |

Exit criteria: every question has a recorded answer, or a recorded reason it could not be
answered and who is blocked; at least one receiver (TV or probe) shows both programs with the
names from our CVCT; the stream ran 48 h; results are written into §2, §4 and §19.

### M1 — probe

The probe driver of §11.1 as the first Rust code: flake from the `rust-flake` template,
configuration, metrics, the TS checker module of §15 in its first form. It is built against
the M0 spike stream. If the HDHomeRun has not arrived, M1 and M2 swap, and M2's hardware
criteria are re-run when it does.

Exit criteria: one rotation covers all 16 carriers in under 10 minutes; lock, `ss`, `snq` and
`seq` are exported for each; the lineup read from `streaminfo` matches the spike's CVCT;
stopping the spike stream is reported within one rotation; our checker and TSDuck agree on
continuity errors and table intervals for 20 captures; 24 h unattended with no driver failure.

### M2 — one carrier from our own mux

In-process test sources (color bars with a frame counter, a tone) feed `mpeg2video` and
`ac3`; the mux, the PSI and minimum PSIP generators, the carousel and the pacer are ours. Two
programs on carrier 1 at static rates. CI with TSDuck from the first commit.

Exit criteria, in CI on a 60 s stream: tables decode in TSDuck to the configured lineup; 0
continuity errors; PCR interval ≤ 40 ms; PCR error against byte position ≤ 1 tick of 27 MHz;
table cycles within §7.2; TS rate 38,800,000 bit/s within one packet per second; 0 buffer-model
violations. On hardware: a TV scan finds both channels with the right numbers and names and
sets its clock; the probe's `streaminfo` lists both; over a 24 h soak the mux reports 0
continuity errors and 0 underrun slots, pacer lateness at the 99.9th percentile is inside the
tolerance found in M0 question 4, and every probe sample shows lock with `seq` = 100.

### M3 — a producer on a TV

RTMP ingest, decode, timeline mapper, frame and sample clocks, fallback chain and slate.
The producer appears as a TV channel with silent audio.

Exit criteria: the producer's picture on a TV for 24 h; each of these injected 20 times: the producer
killed, the producer restarted (timestamp reset), the producer stopped for 30 s with `SIGSTOP`,
the producer idle then woken. Every injection leaves 0 continuity errors, no PCR gap over 40 ms,
0 underrun slots and no discontinuity flag; the picture reaches the slate within 1.6 s (hold
plus one GOP) and returns within 3.6 s of the input being healthy; other programs on the
carrier show 0 errors throughout. Measured and recorded: skew in ppm, frames repeated and
dropped per hour, and cores per program on `nixark`, which replaces the first row of §14 that
applies.

### M4 — modulator client and supervisor

The typed client, refusal function, supervisor and the mitigations of §10.3–10.4.

Exit criteria: every saved page in `work/device/web/` parses into its type (golden tests);
unit tests show each refused request is rejected before a socket is opened; with the owner
present, the client saves the RF plan (four modules) and 16 stream URLs and reads back
identical values; a stream URL changed by hand is reported within 60 s and, in `apply` mode,
corrected within 120 s; a reboot is detected within one poll and the configuration is
verified afterwards; the `mux_sync` restart test of §10.4 has been run with the probe
watching and its result recorded; the chosen defect D mitigation has run for 7 days with input
on at least 4 carriers and no configuration drift.

### M5 — program guide

EIT, ETT, the schedule API and placeholder events.

Exit criteria: a TV's guide shows titles and descriptions for the current event and at least
12 hours ahead on every channel of the carrier; `tstables` decodes every EIT and ETT from a
probe capture; each PSIP PID stays under 250 kbit/s with 0 smoothing-buffer violations; a
7-day soak crosses 56 window boundaries with 0 section CRC errors and the guide still correct
on the TV afterwards; both settings of `ett_unique_extension` have been tried on each TV
model and the result recorded.

### M6 — all carriers

Sixteen carriers on both GigE ports, a slate wherever there is no content, the NixOS module
(a hardened systemd unit), and real capacity numbers.

Exit criteria: 16 carriers for 72 h with 0 continuity errors and 0 underrun slots per
carrier; pacer lateness at the 99.9th percentile inside the M0 tolerance at about 58,000
datagrams/s; a probe rotation shows 16 of 16 locked with the expected lineup; a service
restart brings every carrier back within 10 s with table versions continuing; §14 is rewritten
from measurements with real content, including AC-3 and scaling costs.

### M7 — H.264 output

The `libx264` implementation of the encoder trait on one test carrier, with the H.264 PMT
descriptors and transport flags of §8.2.

Exit criteria: our checker and `ffprobe` confirm an access unit delimiter in every AU, SPS
and PPS at every IDR, an IDR every 0.5 s, and the random-access and priority flags; each TV
model on the plant is recorded as decoding H.264 or not; the default codec per channel is
decided from that record.

### M8 — statmux

The controller of §8.4, first on `libx264`, then on `mpeg2video` by reopening at GOP
boundaries.

Exit criteria: on a carrier with 4–6 programs of mixed difficulty, over 24 h: `Σ r_p ≤ pool`
at every tick, 0 buffer-model violations, 0 late AUs; a step change in one program's content
is followed by a completed reallocation within the measured encoder lag + D + one tick; for
the same content and pool, the worst program's average quantizer is lower than with an equal
static split, by a margin that is recorded; for MPEG-2, 1,000 consecutive reopen rate changes
with 0 buffer-model violations and no visible fault on a TV.

### M9 — audio features

Needs a producer with audio (the producer's own audio milestone or a test input). Loudness
normalization, a second audio track with language signaling.

Exit criteria: for test inputs at −10, −24 and −35 LKFS, output measured over 10 minutes is
−24 ± 2 LKFS with `dialnorm` 24 in the bitstream; audio stays within one AC-3 frame (32 ms)
of video on a sync clip, with no trend over 24 h; a TV's audio menu offers both languages and
switches between them.

### M10 — ratings and captions

Exit criteria: a TV's parental lock blocks a test event rated TV-MA and passes one rated
TV-G; CEA-608 CC1 and CEA-708 service 1 text from a test source appears on a TV; captured
streams carry the per-frame caption counts of standards-digest §6.3 and the caption service
descriptor in PMT and EIT.

### M11 — alternative encoders

NVENC H.264 and the friend's CUDA MPEG-2 encoder behind the trait, each evaluated against the
CPU reference with the probe and TSDuck **[decided]**.

Exit criteria: for each candidate, a recorded table of the M2 and M8 conformance checks
(buffer compliance, fixed GOP, rate change behavior, captions), quality at equal rate
against the reference on recorded clips, and cores and GPU load per channel; a recorded
decision to adopt, restrict or reject.

### M12 — hardening and later inputs

Pass-through for compliant inputs, LL-HLS pull, and a long soak.

Exit criteria: 30 days with 0 unplanned process restarts, memory slope under 1 MB per day
after the first day, 0 continuity errors at the mux output, and every carrier locked in at
least 99.9 % of probe samples outside planned modulator restarts.

## 17. Code conventions and dependencies

The layout of the Rust workspace (crates, the sans-io rule, the device protocol, the Nix build)
is in `docs/ARCHITECTURE.md`; where that document and this section differ on build tooling
(breakbulk instead of Crane), that document is current.

Linked into the server (versions and licenses in software-survey §10): `ffmpeg-the-third`
(features `codec`, `format`, `software-scaling`, `filter`) over FFmpeg 9.0.x with libx264;
`tokio`, `tokio-util`, `crossbeam-channel`; `axum`; `serde`, `toml`, `humantime-serde`;
`anyhow` in `main`, `thiserror` in modules; `tracing`, `tracing-subscriber`; `bytes`;
`bitstream-io`, `crc`; `socket2`, `nix`; `rusqlite` (bundled); `reqwest` (device client);
later `cea708-types`, `cea608-types`, `h264-reader`, `m3u8-rs`. Dev-dependencies:
`mpeg2ts-reader`, `proptest`.

External tools, never linked: TSDuck (CI oracle and M0), `hdhomerun_config` from
`libhdhomerun` (probe), `ffprobe` and `dvbinfo` (second opinions).

Not used, with reasons in software-survey §4 and §6: FFmpeg's mpegts muxer at run time,
GStreamer, the recent DVB crate families, `srt-tokio`, UDP segmentation offload.

Nix: the `rust-flake` template through cruft, with Crane, and checks for build, clippy with
`--deny warnings`, fmt, nextest and coverage. FFmpeg with
x264, `pkg-config` and `libclang` for bindgen as build inputs; TSDuck in the check
derivation's inputs. `nixosModules.default` provides `services.totalblonder` as a hardened
systemd unit, sets the `fq` qdisc if `SO_TXTIME` is adopted, and grants real-time scheduling
only if M0 shows it is needed.

Lints and layout: no `unwrap` outside tests; `unsafe` only in named modules with SAFETY
comments, expected to be `codec::ffi` (raw libav context fields, §8.1), `ingest::avio` and
`output::sys` (`sendmmsg`, `clock_nanosleep`). Pure modules (`mux`, `psip`, `statmux`,
`timeline`, `fallback`, `device::supervisor`) take no clock or I/O handles.

## 18. Assumptions and open questions

### 18.1 Assumptions the owner has not confirmed

All of these are **[proposed]** working assumptions. Changing any of them changes sizing, not
structure.

| Assumption | Basis | What changes if wrong |
|---|---|---|
| The host is the VM `nixark` with 8 vCPUs, 31 GB RAM and an RTX 2000 Ada (16 GB). The vCPU count can be raised; the physical host has 2 × 16 cores (64 threads), and its vCPUs are already over-allocated to other VMs. | design-inputs "Host" (verified 2026-11-24) | §14 |
| Target lineup size and resolutions are not known. The design supports 16 carriers × 6 programs; the first milestones are sized for 1 carrier with 2–4 programs. | manual §4.2 (limits) | §14, milestone sizes |
| CPU cost per channel is taken from measurements on another machine (`bagel`, Ryzen 7 5800X) with synthetic input. It must be re-measured on `nixark` with real content. | software-survey §9.4; design-inputs "Host" | §14 |
| The TV models and their H.264 support are not known. MPEG-2 is the default output codec. | design-inputs "Codec choice per channel" | codec mix, §14 |

### 18.2 Open questions

For the owner:

1. **Data network. [decided 2026-11-26]** Network settings may now be changed (the earlier
   rule applied only during the investigation). GIGE1 is on the rack switch's sfp-sfpplus14
   and GIGE2 on sfp-sfpplus8, each module with its speed forced (neither links under
   auto-negotiation). Each GigE port gets a VLAN of its own, with no router interface:
   VLAN 20 `10.42.20.0/24` for GIGE1 and VLAN 21 `10.42.21.0/24` for GIGE2. `nixark` gets
   one SR-IOV VF in each, and the owner's VF policy pins each VF's VLAN. Each data VLAN has
   one receiver, so multicast reaches the box even if no switch acts on its IGMP joins. GIGE1
   answers ARP, so unicast is the fallback.
2. **Manual mode. [analyzed]** What an `admin.cgi` save writes is now known (firmware §4.1).
   Only the fields present in the POST are applied, and a changed GigE address reboots the box
   right after the write. The plan is one POST, made by hand with the owner present, carrying
   only `configMode=1` and the six GigE fields. The box then reboots into Manual mode with both
   GigE ports re-addressed and GIGE2's mask and gateway overwritten. Take a fresh EEPROM dump
   before the POST and compare after. Deleting the flash config file stays an optional extra.
3. **Confirming Manual mode at run time.** A bare GET of `admin.cgi` shows the mode and was
   done safely once (device §11.3), but the refusal list bans the page outright. Allow that
   one read, or rely on a recorded manual check?
4. **Defect D.** Is a daily reboot of the modulator acceptable as the first mitigation, and
   at what time of day? May a restart of `mux_sync` be tested, and may a RAM-only patched
   `mux_sync` be run if it validates (§10.4)?
5. **Host.** `nixark` is a VM with 8 threads (design-inputs "Host"). Should its vCPU count be raised, and to what, given contention with the other VMs?
6. **Lineup.** How many channels, at what resolutions, and how many carriers at first?
7. **TVs.** Which models are on the plant? Are integer frame rates and H.264 worth testing
   on them early?
8. **Guide content.** Where do event titles come from: a schedule the owner writes, a
   "now/next" call from producers, or placeholders only?
9. **Caption and audio sources.** No current producer has audio or captions. Which comes
   first?
10. **Supervisor login.** The supervisor's Admin session logs a human out of the web UI and
    the reverse. Is the Guest account (read-only, untested here) acceptable for polling?
11. **Syslog.** The box sends syslog to 10.42.0.53 with all categories disabled (device §4,
    §11.3). Is that host available to us, given that enabling categories is an `admin.cgi`
    change?
12. **1080i.** Is interlaced MPEG-2 output wanted at all? It costs about 3.5 times a 720p
    channel (software-survey §12 item 5).

For the friend (design-inputs "Questions for the friend"): what set this unit up on
2026-09-07 and wrote the table at EEPROM 0xF10 (corruption-48 §8); what "corrupted" looked
like and how long it took to appear; what generates her streams and guide data; whether her
CUDA MPEG-2 encoder's source is available and what rate control it has.

To be measured (M0 unless noted): every [unknown] row of §2; the items in the M0 table;
whether the box passes every PID and flag untouched (standards-digest "Open questions" 10);
boot time of the box; the HDHomeRun's model, tuner count and control details when it arrives
(software-survey §12 item 7).

Standards and receiver behavior, to settle on real TVs: the ETT extension value; whether a
CVCT-only MGT is acceptable; the trailing bits of the caption service descriptor; described
video labeling; how many EIT windows TVs use; STT cadence; audio-only channels
(standards-digest "Open questions" 1, 2, 4, 5, 8, 9, 15).

Design points left open in this draft: whether to build FFmpeg ourselves to patch
`mpeg2video` rate control or adopt FFmpeg 9.1's reconfigure API (software-survey §12 item 4);
whether statmux is worth its complexity at six programs per carrier against tuned static caps
(software-survey §12 item 6); whether the producer should gain an MPEG-TS or SRT sink
(software-survey §12 item 8); the EAS trigger format on UDP 5050, which is not decoded
(device §11.4) and not needed by this design.

## 19. Verified so far

What has actually been done and verified. All dates are 2026-11-24. Nothing here involved
sending a stream to the modulator or writing its configuration.

- **Firmware on the device equals the published image.** All 96 non-empty files pulled from
  the running unit over FTP are byte-identical to the root filesystem extracted from
  `EPCS_1_1113.bin`, except the generated configuration export (device §1). The vendor's
  published files have the same SHA-256 as `docs/external/eqam-420b.zip`, and 1.1.13 is the only
  published build (research §1).
- **Flash safety copy.** The 2 MB configuration flash (`mtd0`) was read twice through a
  temporary copy in the device's RAM; both reads are identical (SHA-256 `85054757…f76df9`),
  saved as `work/device/flash/mtd0.bin` (device §11.1). The web configuration export is saved
  as `work/device/config_file.dat` (device §5).
- **EEPROM safety copy.** A 260-byte read-only program (`tools/eedump/`) was uploaded to the
  device's RAM, run twice and removed; both 8 KB dumps are identical (SHA-256
  `2f27ba03…5f55`), saved as `work/device/eeprom.bin` and not committed because it holds
  credentials (corruption-48 §8).
- **Defect A confirmed on this unit.** The dump holds exactly the bytes predicted for QAM
  saves of modules 2 and 3 made in SPTS mode: 10, 11, 12, 13 at 0xA91… and 6, 95, 96, 97 at
  0xC91… (corruption-48 §8). Defects B and D are proven in disassembly; their effects on this
  unit are not observed (corruption-48 §0).
- **Unexplained table in the EEPROM.** Offsets 0xF10–0x108F hold a structured table matching
  the 48-line flash file; no analyzed 1.1.13 program writes it, and it overlays GIGE2's mask
  and gateway and the stored channel-table settings (corruption-48 §8).
- **Live state recorded.** MPTS mode, IGMPv2, 16 carriers on and locked at 256-QAM, no inputs
  configured, Configuration Mode Automatic, uptime about 6 d 18 h (device §4;
  corruption-48 §8).
- **Spike pipeline run in loopback.** The M0 command line was run to a file (40 s, 186.6 MB)
  and to `127.0.0.1:5000`, captured with `tsp`. Measured: TS rate from PCR 36,999,998 bit/s;
  two services; 0 continuity errors; 2,848 PCRs with 0 jitter; MGT every 146 ms (maximum
  170); CVCT every 344 ms (maximum 440); STT advancing. Multicast was not sent
  (software-survey §2.6).
- **TSDuck as oracle.** A two-channel CVCT, an MGT, an STT and two ATSC EITs compiled and
  decoded correctly; the MGT byte counts must be supplied by hand; TSDuck 3.44 misreported a
  59.94 Hz MPEG-2 stream as 25 Hz (software-survey §2.1, §2.3).
- **Encode cost baseline.** Measured on `bagel`, not on the target host, with synthetic input:
  the first column of §14 (software-survey §9.4).
- **Target host.** `nixark` reports 8 CPUs (AMD EPYC 9124), 31 GB RAM and NVIDIA driver
  595.71.05 (design-inputs "Host").
