# Ingest protocols (`tb-rtmp`, `tb-tsin`): design record and validation log

Working log of the package that parses the first two input protocols in Rust, for the
architecture where containers and network protocols are parsed in Rust and only codecs go to
libav, in a sandboxed process that sees nothing but `tb_media::InputEvent`s. Started and
finished 2026-11-26 (one unattended session). If the session is interrupted, read "State"
first.

## How things are run

- Worktree branch `worktree-agent-a28776126b87cee45`, started from `60ca408` (the `tb-media`
  contract).
- `CARGO_BUILD_JOBS=4 nix develop -c cargo nextest run -p tb-rtmp -p tb-tsin`; clippy with
  `--all-targets`, `cargo fmt` before each commit. `nix flake check` was not run (instruction).
- ffmpeg 6.1.6 (`ffmpeg-full`, the user profile on bagel) for the real-client tests; they skip
  with a note on stderr when `ffmpeg`, `ffprobe` or a needed encoder is absent, as in the Nix
  sandbox. Checked: with those tools removed from PATH all 78 tests pass, 13 of them as skips.
- GStreamer 1.28.8 is not in the dev shell. For the two GStreamer tests: `nix build --no-link
  --print-out-paths nixpkgs#gst_all_1.{gstreamer.bin,gstreamer.out,gst-plugins-base,
  gst-plugins-good,gst-plugins-bad,gst-plugins-ugly}`, put `gstreamer-bin/bin` on PATH and
  every `lib/gstreamer-1.0` in `GST_PLUGIN_SYSTEM_PATH_1_0` (the core elements, capsfilter
  among them, are in `gstreamer.out`; without it every caps filter fails to link).

## State

Finished. Commits after `60ca408`:

1. `2929ee6` tb-rtmp core: handshake, chunk layer, AMF0, messages, FLV tags, media assembler,
   server session; unit and session tests;
2. `d9b20be` real ffmpeg over TCP, and a recorded ffmpeg session as a committed fixture;
3. `76110d5` GStreamer `rtmp2sink` and `rtmpsink` (librtmp) as second and third clients;
4. `9f625d3` tb-tsin core: packets and framer, PSI, PES, codecs, demuxer, RTP, UDP input;
5. `b685fb5` tb-tsin against real ffmpeg streams and ffprobe;
6. `5771a18` bounds on held media in both crates;
7. `98cc664` the 2^31 ms wrap of ffmpeg publishers;
8. `4994da9` ffmpeg tests skip unless their encoders exist; then this log.

Not built (see "Open"): the digest handshake, AMF3 values, Enhanced RTMP multitrack, a
re-publish on the same connection, `play`; in TS, 192/204-byte packets, PCR, more than one
program at a time, AAC in LATM. No driver: sockets are the server's business (`tb-ingest`
or the binary); the crates are sans-io.

## tb-rtmp

### Layers

`handshake` → `chunk` → `message` / `amf0` → `server` (session) → `flv` → `media`
(assembler). Everything below `server` is independent of the server role: a future pull
(play) client reuses the handshake (`ClientHandshake` exists and the tests use it), the chunk
encoder and decoder, AMF0, command and control messages, and the FLV parser.

### Decisions

- **Handshake (Adobe RTMP 1.0 §5.2).** Only version 3; 6/8 (RTMPE) and anything ≥ 32 (an
  HTTP request, say) end the session with `Protocol`. The answer is always the simple
  handshake: S0, S1 with a zero second word, S2 = C1's time, our time, C1's random bytes,
  sent together once C0+C1 are in. C2 is read and not checked. A digest ("FP9") C1 gets the
  same answer: a zero version in S1 is how a server says "no digest", and ffmpeg
  (`rtmpproto.c`: validates the server digest only when *playing* and S1's version byte
  ≥ 3) and librtmp (`handshake.h`: clears its FP9 flag when `serversig[4] == 0`, then only
  warns if S2 differs from C1) both fall back. The digest exists to authorize Flash players
  for SWF verification; publishers do not need it. Verified: ffmpeg (digest C1) and librtmp
  (through GStreamer `rtmpsink`) both publish.
- **Chunk timestamps (§5.3.1.2–5.3.1.3).** Format 0 absolute; 1 and 2 add a delta; a format 3
  chunk that *starts* a message adds the chunk stream's last timestamp field again (as ffmpeg's
  `rtmppkt.c` reads and writes it, including after a format 0); a continuation changes
  nothing. Extended timestamps: when the latest format 0/1/2 header used 0xFFFFFF, every
  format 3 chunk carries the 4-byte field; read always, ignored on continuations, the delta on
  a new message. No "peek and guess" for encoders that omit it: ffmpeg, librtmp and
  GStreamer all write it.
- **Leniency.** A new format 0/1/2 header while a message is in progress drops the partial
  message (as nginx-rtmp) instead of ending the session; a format 1/2/3 chunk on a chunk
  stream that never had a format 0 ends it.
- **Bounds** (`chunk::Limits`, `ServerConfig`): message ≤ 8 MiB (a 1080p intra picture is
  about 1 MiB), ≤ 64 chunk streams, ≤ 16 MiB in partial messages across chunk streams, message
  buffers grow with the bytes that arrive and are never allocated from a declared length;
  command and data messages ≤ 64 KiB before AMF decoding; AMF0 depth ≤ 16 and ≤ 4096 values;
  output queue ≤ 256 KiB (beyond it the peer is not reading: `Local`); ≤ 8 `createStream`.
  Set Chunk Size accepts the whole legal range 1..=2^31−1 (a "chunk-size bomb" costs nothing:
  a chunk's payload is bounded by its message's length). Set Chunk Size and Abort act inside
  the decoder the moment their message completes, since they change how the next chunk is
  read.
- **AMF3.** Command messages of type 17 are accepted when their body is the format byte 0
  followed by AMF0 (what every encoder that uses type 17 sends); an AMF3 value anywhere
  (marker 0x11) ends the session with `Protocol("... AMF3 values are not supported")`. We
  answer `connect` with `objectEncoding` 0.
- **Command flow.** `connect` → Window Ack Size 2.5 MB, Set Peer Bandwidth 2.5 MB dynamic,
  Set Chunk Size 4096, `_result` (`NetConnection.Connect.Success`). `releaseStream`,
  `FCPublish` → `_result` (null, undefined): ffmpeg tracks these transactions and ignores the
  replies, librtmp logs them. `createStream` → `_result` with stream ids 1, 2, ... `publish`
  (on a created stream; the stream key split at '?' into key and query) → User Control
  StreamBegin and `onStatus NetStream.Publish.Start` on csid 5 (ffmpeg sends media only after
  it). `FCUnpublish` → `End(Closed)`, then we wait for `deleteStream` (ffmpeg sends both, in
  that order) and ask to close. `@setDataFrame onMetaData` and a bare `onMetaData` are read.
  Unknown commands are counted and ignored. Ping requests are answered; Acknowledgments are
  sent per the peer's Window Ack Size.
- **Acceptance.** `PublishPolicy::check(&Request)` with `Request::Connect { app, tc_url }`
  and `Request::Publish { app, key, query }`; implemented by `AllowAll`, `AllowList`
  (`allow(app, key)`, `allow_app(app)`) and any `FnMut(&Request) -> bool`. Refusal of the
  application: `_error NetConnection.Connect.Rejected`; refusal of the stream: `onStatus`
  level `error`, `NetStream.Publish.BadName`; then close. ffmpeg reports "Server error: Stream
  not accepted." and exits non-zero.
- **Events.** Nothing before a publish is accepted (a rejected or broken connection emits no
  events; `close_reason()` says why). From then on exactly one input session: `Streams`,
  packets, further `Streams` on a reconfiguration, one `End`. `End` reasons: `Closed`
  (unpublish, deleteStream, or the peer closing the TCP connection: GStreamer's `rtmp2sink`
  does that without `deleteStream`), `Transport` (idle and other timeouts, socket errors),
  `Protocol`, `Local` (output queue). Held packets are released before `End`.
- **Streams rule** (the producer may be video only). Indices are fixed: video 0, audio 1. A
  track is *known* at its sequence header (AVC, HEVC, AV1, VP9, AAC, Opus) or first frame
  (MP3, PCM, AC-3, E-AC-3). Packets of a known track are held until `Streams`; frames of an
  unknown track are dropped and counted. `Streams` goes out when one track is known and every
  other is known or *absent*. Absent means: `onMetaData` names the other track's codec id and
  not this one's (ffmpeg's FLV muxer and OBS write exactly the ids of the streams they have;
  metadata with neither id says nothing); or its codec is not carried; or the wait is over:
  held packets span 1 s of media time, or 2 s of wall time have passed since the first media
  message, or the held packets reach 8 MiB (each counted with 128 bytes of overhead, so that a
  flood of empty frames is bounded too). The producer (video only, with metadata) gets `Streams`
  at its sequence header with no wait; measured with ffmpeg video-only. A changed sequence
  header (codec or bytes) or a track that becomes known later re-announces; an identical one
  is counted and ignored.
- **Malformed FLV tags** (a truncated AVC header, an unknown packet type) are dropped and
  counted: the framing is intact, so ending the session for one bad tag would cost a
  reconnect for nothing. Chunk-layer and command violations end the session.
- **Timestamps.** One unwrapper per connection, shared by audio and video. Video
  `pts = dts + CompositionTime`. **Finding:** ffmpeg's FLV muxer masks the timestamp's top
  byte with 0x7F (`flvenc.c`, `put_timestamp`: "timestamps are 32 bits _signed_") and its RTMP
  client takes message timestamps from those tags, so ffmpeg publishers, the producer among them,
  wrap at 2^31 ms (24.8 days), not 2^32. Measured with `-output_ts_offset 2147483`:
  `..., 2147482967, 2147483000, 219, 252`. A plain 32-bit unwrap reads that as a 24.8-day step
  back, which the timeline mapper would take as a discontinuity once per 24.8 days of one
  connection. `MsUnwrapper` takes the step on the 2^31 circle when both values are below 2^31,
  the 32-bit step is ≥ 2^30 ms, the 31-bit step is < 2^20 ms, and the continuous time has
  already reached the wrap zone (the last condition because ffmpeg sends the sequence header
  at 0 before frames at a large offset). Note for `tb_timeline::Unwrapper::rtmp_ms`: it has the
  plain 32-bit rule; fed raw ffmpeg RTMP timestamps it would see the same backwards step.
  `tb-rtmp` already hands out continuous values.
- **Timeouts** (driven by `now` arguments): handshake 10 s, publish 15 s from accept, idle
  10 s with no bytes. We send no keepalive pings of our own.

### Validation (tb-rtmp: 50 tests, plus one `#[ignore]`d fixture recorder)

- Unit: the spec's two chunk examples (§5.3.2.1, §5.3.2.2) decode to the stated timestamps
  and lengths; encoder ↔ decoder round trip over chunk stream ids 3, 63, 64, 319, 320, 65599,
  all formats, zero-length messages, a timestamp going back and a new message stream (forcing
  format 0), byte by byte; extended timestamps repeated in format 3 continuations; Set Chunk
  Size taking effect on the next chunk; Abort; every limit; AMF0 round trip and the spec's
  byte layout; bounded AMF decoding (depth, a 4-billion-element array with no elements, a 4 GiB
  long string, value count, AMF3 marker, ECMA array without its end marker); FLV legacy and
  enhanced tags (ModEx prefix, command frames, CodedFramesX); AudioSpecificConfig.
- Session (synthetic publisher built from the crate's own client pieces): the replies
  (WAS, SPB, SCS before `_result`; createStream returns 1; StreamBegin; Publish.Start); the
  events (90 video, key frames at 0/30/60, pts = dts + cts, AAC hints from the ASC);
  timestamps across 2^32 with extended fields; **every split point** of a small session
  (every two-way split, and byte by byte) gives identical events; 50 random partitions of a
  larger one; rejection by allow list (wrong key, wrong app) and by closure (sees the query);
  ping answered, acknowledgments sent; handshake, publish and idle timeouts; output bound
  under a 60 000-ping flood; oversized declared length after a chunk size of 2^31−1;
  oversized command; HTTP instead of RTMP; publish before createStream; the type-17 wrapper;
  300 rounds of random bytes / random corruption / truncation, split at random: no panic, one
  `End` at the end of any session.
- Recorded fixture (`tests/fixtures/ffmpeg-h264-aac.rtmp`, 8 889 bytes, ffmpeg 6.1.6:
  64x36 H.264 with B-frames, key every 10, mono AAC 22.05 kHz, 1 s), so that the sandbox
  replays real client bytes: events checked (avcC version 1, NAL lengths fill each packet,
  key frames 0/10/20); **every two-way split point** (8 890) and fixed chunkings of 1, 2, 3,
  7, 127, 128, 129, 1536, 1537, 4096 give the same events; every truncation ends cleanly.
- Real clients over TCP (`tests/clients.rs`), session driven by a test loop over a socket:

  | Client | Media | Result |
  |---|---|---|
  | ffmpeg 6.1.6, `-re ... -c:v libx264 -g 30 -c:a aac -t 5` | H.264 + AAC | 150 video (key frames 0, 30, ..., 120, pts 0, 1000, ... ms), 217 AAC, monotonic DTS, B-frames, `End(Closed)` via deleteStream; 50 random re-splits of the 575 640 received bytes give the same events |
  | ffmpeg, `-c:v libx265` (Enhanced RTMP `hvc1`) | HEVC + AAC | 90 video; key frames at pts 0/1000/2000 (decode positions 0, 26, 58: x265's open GOPs put the CRA before its leading pictures; ffprobe agrees) |
  | ffmpeg, `libsvtav1` / `libvpx-vp9` | AV1 (`av01`), VP9 (`vp09`) | 30 video each, key first |
  | ffmpeg, video only | H.264 | `Streams` with one track at once (metadata names only video) |
  | ffmpeg, wrong key | | `onStatus` error; ffmpeg: "Server error: Stream not accepted.", exit ≠ 0; no events |
  | ffmpeg, `-output_ts_offset 16776.5` and `2147483`, 640x360 | H.264 | DTS continuous across 0xFFFFFF (extended timestamps, multi-chunk key frames) and across ffmpeg's 2^31 wrap |
  | GStreamer 1.28.8 `rtmp2sink` | H.264 + AAC | 90 video, 130 audio; closes TCP without deleteStream → `End(Closed)` |
  | GStreamer 1.28.8 `rtmpsink` (librtmp, as OBS) | H.264 + AAC | 90 video, 130 audio, `Finished` |

  For both GStreamer runs flvmux's output was teed to a file and our per-packet
  `(dts, pts, key)` equals ffprobe's reading of the same tags. **Finding:** GStreamer's
  flvmux writes B-frame H.264 with a repeated first DTS (0, 0, 23, 69, ...) and some
  pts < dts. That is the producer's timeline, not a parse error; the decoder and the mapper
  must tolerate equal DTS.

## tb-tsin

### Decisions

- **Independent of any TS writer** in the workspace (instruction); its own small PSI and PES
  code. Tests carry their own naive TS writer.
- **Framing.** Datagrams and RTP payloads are packet-aligned (`push_packets`); byte streams
  (`push_bytes`) go through a framer that acquires sync on three sync bytes 188 apart, drops
  sync at the first packet without one, and holds at most two packets' worth between calls.
  Only 188-byte packets (no M2TS 192 or RS 204).
- **Packets dropped** and counted: transport_error_indicator, scrambled, adaptation_field_control
  00, adaptation field longer than allowed (§2.4.3.5: ≤ 182 with payload, 183 without), null
  PID.
- **PSI.** Sections are reassembled with pointer_field (§2.4.4.1–2), several sections per
  packet and stuffing; CRC-32 (Annex A) checked on every long-form section (known vector: the
  PAT ffmpeg writes ends `2A B1 04 B2`); ≤ 1024 bytes. current_next_indicator 0 is ignored.
  Multi-section PATs are merged when all sections of a version are present.
- **Program.** `ProgramSelector::First` (first listed in the PAT) or `Number(n)`; a PAT
  version that moves the PMT is followed. One program per demuxer (several demuxers on the
  same input for several programs).
- **Stream indices are PIDs** (13 bits fit `StreamIndex(u16)`): a PID keeps its index across
  PMT versions, so the decoder sees the same index for the same stream.
- **Codecs** (Table 2-34 and registered private values): 0x01/0x02 MPEG video, 0x03/0x04 MPEG
  audio, 0x0F AAC ADTS, 0x1B H.264, 0x24 HEVC, 0x81 AC-3, 0x87 E-AC-3 (ATSC); 0x06 with the
  DVB AC-3 (0x6A) / enhanced AC-3 (0x7A) descriptor or registration "AC-3"/"EAC3" (stream or
  program level), "Opus", "HEVC". **0x11 (AAC in LATM) is refused**: `CodecId` has no LATM
  framing and libav would need its LATM parser in the worker; counted in `skipped_streams`.
  Subtitles, teletext, SCTE-35, metadata are left out of `Streams`.
- **Opus.** The PES payload is split at each `opus_control_header` (prefix 0x7FE0..., AU size
  as a run of 0xFF bytes, optional trims and extension) into one packet per access unit, the
  first with the PES's PTS and each next one later by the previous one's TOC duration
  (RFC 6716 §3.1). `OpusHead` extradata is built from the DVB extension descriptor (tag 0x7F,
  extension 0x80) for mono and stereo only (mapping family 0); multichannel gets none (the
  decoder then assumes stereo). Which DVB/ETSI document defines the TS framing I did not
  confirm; it is implemented as ffmpeg reads and writes it and checked against ffprobe.
- **Languages**: the first entry of the ISO 639 descriptor (§2.6.18), ASCII letters only.
- **PES** (§2.4.3.6–7): reassembled per PID from payload_unit_start to the next one, or to
  PES_packet_length when it is non-zero (so audio is not held for one PES). PTS/DTS 33 bits,
  marker bits not checked; `dts = pts` when only PTS is present; one 33-bit unwrapper per
  program (half-range rule of `tb_timeline::Unwrapper`, mirrored in 90 kHz so the crate
  depends only on `tb-media`). Key frames: random_access_indicator, or the bitstream (H.264
  IDR, HEVC IRAP 16–23, MPEG-2 sequence header); audio always.
- **Loss** (§2.4.3.3): per PID, a repeated CC with payload is a duplicate (dropped); any other
  gap drops the partial PES and flags the next packet of that PID; discontinuity_indicator
  allows a jump. Packets without payload are not checked. An RTP gap is not acted on as such:
  it shows up as CC gaps on the PIDs it hit.
- **The loss flag is not in `tb_media::Packet`.** The contract has no field for it, and
  changing a contract other crates are being built against was not mine to do overnight. The
  demuxer emits `TsEvent { event: InputEvent, after_loss: bool }`. Proposal for the owner: a
  `discontinuity: bool` (or `corrupt`) on `tb_media::Packet`, which RTMP would always leave
  false and the decoder worker would map to `AV_PKT_FLAG_CORRUPT` / error concealment.
- **Bounds**: a PES ≤ 8 MiB, all partial PES ≤ 16 MiB, ≤ 16 elementary streams, sections
  ≤ 1024 bytes, RTP window ≤ 256 datagrams (64 by default).
- **RTP** (RFC 3550 §5.1, §A.1; RFC 2250 §2): version 2, CSRCs, extension, padding validated;
  payload type 33 by default (configurable for a dynamic type). Reordering through a window
  of 64 datagrams; a packet is released when everything before it has arrived, when the
  window is full, or after 50 ms (`max_delay`); gaps released past are lost, later arrivals
  late. A new SSRC, or a jump beyond MAX_DROPOUT 3000 ahead / MAX_MISORDER 100 behind that a
  second in-sequence packet confirms, starts a new session. The first packet received defines
  the start of a session: an earlier one arriving after it is late (no start-up delay is spent
  waiting; PSI repeats).
- **Transport detection** (`Transport::Auto`): first byte 0x47 and a length that is a multiple
  of 188 → raw TS; RTP version 2 with the payload type → RTP (its first byte is 0x80–0xBF, so
  the two cannot be confused). The first datagram decides; the other kind is then counted and
  dropped.
- **Sessions and End.** A session starts at `Streams` and ends with one `End`:
  `Transport("no data for 5s")` on idle, `Transport("RTP source changed")` on a new RTP
  session (the demuxer resets and waits for PAT/PMT), `Closed` from `finish`. Partial PES are
  released before `End`.

### Validation (tb-tsin: 28 tests)

- Unit: header and adaptation field parsing, the framer (acquisition, a broken packet, resync,
  splits of 1/7/188/1000), the CRC vector, sections across packets with pointer_field and
  stuffing, a corrupted section counted, PMT with descriptors, 33-bit timestamps round trip
  (0 to 2^33−1), stream type table, key frame detection, Opus framing and durations, RTP
  header fields (CSRC, extension, padding, bad padding, version), reordering, release on time,
  window overflow across the 16-bit wrap, SSRC change and confirmed jump.
- Hand-built streams: streams, indices, language, exact payloads, key frames; **every split
  point** of a stream (6 frames) gives identical events; timestamps across 2^33; a dropped
  packet drops exactly that frame and flags the next; a duplicated packet is dropped; program
  selection (first, by number) and a PMT version change re-announcing (AAC → AC-3 by DVB
  descriptor, LATM skipped); RTP through `TsInput` with pairwise swapped datagrams across the
  sequence wrap equals the byte-stream result, then `End` on idle; a new SSRC gives
  `Streams, End(Transport), Streams, End(Closed)`; raw UDP with an RTP datagram rejected;
  oversized PES dropped and the next flagged, the total bound; 400 rounds of random bytes,
  random corruption and random datagrams (raw and RTP): no panic, packets only of announced
  streams, one `End` per session.
- Real ffmpeg 6.1.6 (2 s, 640x360 at 30 fps, 48 kHz sine), demuxed from 1000-byte pieces with
  zero CC errors, malformed packets or bad sections; video `(pts, dts, key)` of every packet
  **equal to ffprobe's reading of the same bytes**:

  | Stream | Result |
  |---|---|
  | `-f mpegts`, libx264 `-g 30` + AAC | H.264 + AAC; 60 video, keys 0 and 30; 7 audio PES holding 95 ADTS frames (ffmpeg packs several frames per PES) |
  | libx265 | HEVC; 60 packets as ffprobe |
  | mpeg2video `-g 15 -bf 2` + AC-3, `language=spa` | MPEG-2 + AC-3, language `spa`, every AC-3 PES starts 0B 77 |
  | libx264 + eac3 | E-AC-3 (ffmpeg writes 0x06 + the DVB descriptor) |
  | libx264 + libopus | Opus, `OpusHead`; 101 access units of 1800 ticks (libopus adds one for its pre-skip), pts and durations equal to ffprobe's |
  | `-re -f rtp_mpegts rtp://` to a UDP socket | RTP detected, 182 datagrams, none lost or reordered on loopback; 60 video, keys at 0 and 30 |
  | `-re -f mpegts udp://...?pkt_size=1316` | raw UDP detected, 221 datagrams; same |

## Open

- Digest handshake: not implemented (not needed by any publisher tested; documented above).
- OBS itself was not run; librtmp (which OBS uses) was, through GStreamer `rtmpsink`.
- Enhanced RTMP audio (v2: Opus, AC-3, E-AC-3, FLAC FourCCs) and ModEx are parsed and unit
  tested but no available client sends them (ffmpeg 6.1 sends enhanced video only). Multitrack
  is reported as unsupported and ignored.
- No re-publish on the same connection, no `play`, no server-initiated keepalive pings.
- TS: no PCR use (the worker does not need it; the timeline mapper uses arrival times), no
  192/204-byte packets, LATM refused, multichannel Opus without extradata; `VideoHint` /
  `AudioHint` are not filled for TS (no parsing of SPS or ADTS headers here; the decoder
  reports them).
- Documents not changed, to stay out of other packages' way: SPEC §9.1 still says "libav's
  `rtmp` protocol with `listen=1`", and ARCHITECTURE §9 lists ingest as the `tb-ingest`
  adapter only. Both are superseded by the decision that protocols are parsed in Rust; the
  crate map should gain `tb-rtmp` and `tb-tsin` (pure) feeding the decoder worker.
- `tb_media::Packet` has no loss/corrupt flag (see "The loss flag").
