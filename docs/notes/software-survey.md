# Software survey: reuse versus write

Survey of existing software for the Rust broadcast-mux server, 2026-11-24. Input for
`docs/SPEC.md`, not the spec. Labels as in `design-inputs.md`: [V] verified here (command run,
source read, registry queried today); [K] general knowledge, not checked here. Versions and
dates come from crates.io, GitHub or nixpkgs on 2026-11-24 unless marked [K].

Tools used locally (nothing installed permanently): `nix shell nixpkgs#tsduck` = TSDuck
3.44-4676, `nix shell nixpkgs#ffmpeg-full` = FFmpeg 9.0.2. Upstream TSDuck is at v3.45-4798
(2026-09-12); FFmpeg 9.1 is tagged.

## 1. Summary: build or reuse, per subsystem

| Subsystem | Recommendation | Main reason |
|---|---|---|
| Ingest: RTMP push | Reuse libav (`rtmp` protocol with `listen=1`) through `ffmpeg-the-third`; reopen on each disconnect. Fall back to `rml_rtmp` 0.8.0 (MIT, frozen since 2023) only if we need several publishers on one port or connection-level control. | The producer pushes one FLV stream per connection; libav already demuxes FLV/H.264. |
| Ingest: LL-HLS | Write a small client (reqwest + `m3u8-rs` 6.0.1 + our own EXT-X-PART/PRELOAD-HINT handling), hand fMP4 to libav through a custom AVIO. | No Rust crate or libav handles LL-HLS parts; libav `hls` treats it as ordinary HLS (whole-segment latency). RTMP is the preferred path anyway. |
| Decode | Reuse libav (software H.264). Optional NVDEC/VAAPI decode offload later. | Cheap (~0.1–0.2 core per 1080 stream [K]); robust to timestamp resets once we own the timeline. |
| Encode video, MPEG-2 | Reuse libav `mpeg2video` on the CPU. | Only encoder with interlaced output and A/53 captions; no hardware encoder offers both (section 9). |
| Encode video, H.264 | Reuse libav `libx264` on the CPU. | Only encoder with A/53 captions, CBR filler/HRD, AUD and bitrate changes without forced IDR (`x264_encoder_reconfig`). |
| Encode audio | Reuse libav `ac3` (native float encoder) plus `loudnorm`/`ebur128` filters, or a fixed gain from measured loudness. Silent AC-3 for gaps: pre-encode one silent frame per layout and repeat it. | Mature, LGPL, exposes `dialnorm`. |
| Captions | Reuse `cea708-types` 0.4.1 + `cea608-types` 0.1.5 (MIT) to build `cc_data`; attach as `AV_FRAME_DATA_A53_CC` side data so the libav encoder writes it; `h264-reader` 0.9.0 to pull captions out of ingested SEI if ever needed. | Encoders already insert A/53 user data (MPEG-2 picture user data, H.264 SEI). We only build the cc_data triplets and the caption_service_descriptor. |
| Mux (TS packetization, PCR, CBR, null fill) | Write our own. | FFmpeg's muxer is packet-driven, not clock-driven: no output when an encoder stalls, no ATSC descriptors, no PSIP, whole-PES bursts, cannot change program set without restart (section 3). We need a wall-clock-driven, never-stalling scheduler anyway for robustness and statmux. |
| PSI/PSIP generation (PAT, PMT, MGT, STT, CVCT, EIT, ETT, RRT, descriptors) | Write our own on `bitstream-io` 4.10.0 + `crc` 3.4.0 (`CRC_32_MPEG_2`). | No Rust or C library writes the full set. TSDuck is the only complete writer but is C++ with no stable C API. The tables are small and fully specified in `docs/external/standards/`. |
| Statmux control | Write our own controller (section 5). | No open-source statmux exists that we could reuse. |
| Output pacing (UDP/RTP) | Write our own: one sender thread per NIC, `socket2` + `sendmmsg` (via `nix` or `libc`), `clock_nanosleep` absolute deadlines; optional SO_TXTIME/ETF later. RTP header hand-written (12 bytes). | Simple, and must be tied to our mux clock. |
| Monitoring (RF probe) | Reuse `hdhomerun_config` (libhdhomerun, LGPL) as a subprocess, or the HTTP API; TSDuck `tsp` on the captured stream. | Section 7. |
| Test oracle | Reuse TSDuck (`tsanalyze`, `tstables`, `tsp -P continuity/pcrverify/bitrate_monitor`), plus `mpeg2ts-reader` 0.18.2 in Rust round-trip tests, ffprobe, and libdvbpsi/VLC as second decoders. | TSDuck decodes every ATSC table we emit and checks PCR, CC and repetition rates. |
| First test stream (spike) | Reuse ffmpeg CLI + TSDuck `tsp` (section 2.6). | Gets a CBR MPTS with PSIP to the box today, before any Rust exists. |

## 2. TSDuck

- Repo github.com/tsduck/tsduck, BSD-2-Clause, very active (commit 2026-11-24, release v3.45-4798
  2026-09-12). nixpkgs has 3.44-4676 [V].
- The nixpkgs build lists these plugins [V] (`tsp --list-plugins=names-*`):
  - input: craft dvb file fork hls http ip memory null pcap
  - output: drop file fork hls http ip memory play
  - packet: about 110, including analyze bitrate_monitor continuity eit eitinject filter inject merge
    mux pat pcradjust pcrbitrate pcrverify pmt psi psimerge regulate remap stats tables timeref.
  - No SRT, RIST, Dektec or HiDes plugins in that build (they are optional build features
    upstream [K]). HiDes is irrelevant: the EQAM takes IP.

### 2.1 ATSC tables

- The XML model (`share/tsduck/tsduck.tables.model.xml`) has full serialize/deserialize for MGT,
  TVCT, CVCT, ATSC_EIT, ETT, STT, RRT, DCCT, DCCSCT, cable_emergency_alert_table, plus the
  descriptors we need: ATSC_AC3_audio_stream_descriptor, caption_service_descriptor,
  content_advisory_descriptor, extended_channel_name_descriptor, service_location_descriptor,
  registration_descriptor [V].
- `tstabcomp` compiles XML/JSON to binary sections and `tstabdump`/`tstables --atsc` decode them.
  Tested: a two-channel CVCT (80 bytes), an MGT, an STT and two ATSC EITs compiled and decoded
  correctly [V].
- MGT `number_bytes` must be filled in by hand (TSDuck does not compute it from the referenced
  tables) [V: required attribute].
- EIT generation is DVB only:
  - `eitinject` builds EIT p/f and schedule per ETSI TS 101 211 from event files.
  - `inject --eit-normalization` reorganizes EIT sections, also per TS 101 211.
  - Neither knows ATSC EIT-k slots or source_id. For ATSC, write the EIT-0..3 XML ourselves and
    inject it as static tables [V: help text].
- No plugin generates a live STT. `timeref` updates DVB TDT/TOT only [V: help text]. Workaround
  for the spike: a shell loop rewrites `stt.xml` every second and `inject --poll-files` reloads it
  (section 2.6). This was tested and the STT advanced correctly [V].
- Python (`ts.py`) and Java bindings ship with it. Python reaches the library through about 64
  `tspy*` C functions exported by `libtsduck.so` [V: `nm -D`]. Those are an internal ABI for the
  bindings, not a public C API. The real API is C++ (742 headers). There are no Rust bindings.
  Linking libtsduck from Rust would mean a cxx/C++ shim or the unstable `tspy*` ABI.
- Verdict:
  - Not a runtime component. It is C++, and a `tsp` subprocess per carrier would put ours
    behind its scheduler.
  - Use it as the reference oracle and test tool (CI), and as the spike tool.

### 2.2 Mux / CBR plugins

| Plugin | What it does [V help text] | Use for us |
|---|---|---|
| `-I null` + `tsp -b` | Generates null packets; the global bitrate declares the TS rate. | CBR skeleton when merging separate SPTS. |
| `regulate` | Slows the packet flow to a bitrate or to PCRs; `--packet-burst` (default 16). Slow-down only. | Pacing in the spike. |
| `merge 'cmd'` | Merges a subprocess TS into stuffing of the main TS; PSI merge (PAT/SDT), PCR restamp (`--incremental-pcr-restamp` for VBR inputs), smoothing by input bitrate. | Alternative spike: one ffmpeg per program. |
| `mux file` | Inserts packets from a file into stuffing, at `--bitrate` or `--inter-packet`. | Not needed. |
| `inject` | Injects XML/JSON/binary tables on a PID, per-file repetition `file=ms`, `--poll-files` reload, `--stuffing`, `--replace`. Takes packets from null stuffing. | PSIP in the spike. |
| `pcradjust` | Rewrites PCR (and optionally PTS/DTS) to a constant bitrate; can add PCR-only packets. | Not needed if ffmpeg `-muxrate` is used. |
| `filter --negate --pid X --stuffing` | Removes a PID and keeps the rate by replacing it with nulls. | Drop ffmpeg's SDT (PID 0x11). |
| `pmt` | Patches PMTs: add registration, `--audio-language`, `--add-pid-registration`, XML patch. | Add ATSC descriptors to ffmpeg PMTs if wanted. |

### 2.3 Analysis

- `tsanalyze --atsc` (= `tsp -P analyze`): services, PIDs, bitrates, PCR-based TS rate, table
  repetition (min/avg/max per table id), CC discontinuities, first/last STT time [V].
- `tsp -P continuity` reports CC errors; `-P pcrverify` reports PCR jitter against packet
  position (default 1 ms; tested down to 14 PCR units) [V].
- `-P bitrate_monitor` gives min/max alarms and periodic JSON.
- `-P tables --pid ...` dumps sections; `-P pes --video-attributes` reports sequence headers.
- `-P stats`, `-P history`, `tslatencymonitor`.
- There is no packaged TR 101 290 report [K], but the priority-1/2 items (sync, CC, PAT/PMT
  presence and interval, PCR interval and jitter, PTS) are all covered by the plugins above.
- Quirk [V]: TSDuck 3.44 reported our 59.94 Hz MPEG-2 stream as "@25 Hz". The sequence header has
  `frame_rate_code` 7 (59.94), and ffprobe agrees. Treat TSDuck's video-attribute line with
  suspicion.

### 2.4 IP output

`-O ip` [V]:
- `--packet-burst` maximum 7 by default (max 128), `--enforce-burst` for exactly 7 per datagram.
- `--rtp` with `--payload-type` (default 33), `--pcr-pid` for RTP timestamps, `--ssrc-identifier`.
- `--local-address` (outgoing interface), `--ttl`, `--tos`, `--disable-multicast-loop`.

### 2.5 Role

1. Spike tool for the first test streams (now).
2. Test oracle in CI: run our mux for N seconds to a file, then `tsanalyze --atsc --json`,
   `tstables --atsc`, continuity, pcrverify; assert on the JSON.
3. Live monitor of HDHomeRun captures (`tsp -I ip` or `-I file` from `hdhomerun_config save`).
4. Not a runtime component.

### 2.6 Spike command lines

The files below were tested in a scratch directory. They are not in the repo yet; copy them to
`tools/spike/` when the spike is run.

`psip.xml` (CVCT, PID 0x1FFB):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <CVCT version="0" transport_stream_id="0x0101">
    <channel short_name="TEST1" major_channel_number="50" minor_channel_number="1"
             modulation_mode="256-QAM" channel_TSID="0x0101" program_number="1"
             service_type="dtv" source_id="1"/>
    <channel short_name="TEST2" major_channel_number="50" minor_channel_number="2"
             modulation_mode="256-QAM" channel_TSID="0x0101" program_number="2"
             service_type="dtv" source_id="2"/>
  </CVCT>
</tsduck>
```

`eit0.xml` (EIT-0, PID 0x1D00 as declared in the MGT):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <ATSC_EIT version="0" source_id="1">
    <event event_id="1" start_time="2026-11-24 00:00:00" ETM_location="0" length_in_seconds="86400">
      <title_text><string language="eng" text="Test pattern one"/></title_text>
    </event>
  </ATSC_EIT>
  <ATSC_EIT version="0" source_id="2">
    <event event_id="1" start_time="2026-11-24 00:00:00" ETM_location="0" length_in_seconds="86400">
      <title_text><string language="eng" text="Test pattern two"/></title_text>
    </event>
  </ATSC_EIT>
</tsduck>
```

`mgt.xml` (`number_bytes` = compiled sizes: `tstabcomp psip.xml` gives 80 bytes, `eit0.xml` 100):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <MGT version="0">
    <table type="CVCT-current" PID="0x1FFB" version_number="0" number_bytes="80"/>
    <table type="EIT-0" PID="0x1D00" version_number="0" number_bytes="100"/>
  </MGT>
</tsduck>
```

`stt-loop.sh` (STT `system_time` is GPS seconds since 1980-01-06; GPS−UTC offset 18 s):

```sh
#!/bin/sh
dir=${1:-.}
while :; do
  gps=$(( $(date -u +%s) - 315964800 + 18 ))
  printf '<?xml version="1.0" encoding="UTF-8"?>\n<tsduck><STT system_time="%s" GPS_UTC_offset="18" DS_status="false"/></tsduck>\n' "$gps" > "$dir/stt.xml.tmp"
  mv "$dir/stt.xml.tmp" "$dir/stt.xml"
  sleep 1
done
```

**(a) Spike: one CBR MPTS, two programs, minimal PSIP, multicast UDP, 7 packets per datagram.**

How it works:
- One ffmpeg process encodes both programs (MPEG-2 720p59.94 + AC-3 each) and writes a CBR TS
  at `-muxrate`. Its byte-count PCR gives the timeline, and null packets fill the gaps.
- `tsp` drops ffmpeg's SDT, injects PSIP into the null packets, paces to the same rate, and
  sends 7×188-byte datagrams.
- Without `-re`, ffmpeg runs ahead and pipe back-pressure from `regulate` throttles it, so there
  is only one clock.
- 37 Mbps leaves margin under the 38.81 Mbps 256-QAM payload; the box adds nulls.
- Replace `GIGE_SIDE_IP` with the host's address on the GigE data network.

```sh
./stt-loop.sh . &
ffmpeg -hide_banner -loglevel warning \
  -f lavfi -i "testsrc2=size=1280x720:rate=60000/1001" \
  -f lavfi -i "sine=frequency=1000:sample_rate=48000,pan=stereo|c0=c0|c1=c0" \
  -f lavfi -i "smptehdbars=size=1280x720:rate=60000/1001" \
  -f lavfi -i "anullsrc=channel_layout=stereo:sample_rate=48000" \
  -map 0:v -map 1:a -map 2:v -map 3:a \
  -c:v mpeg2video -pix_fmt yuv420p -g 30 -bf 2 \
    -b:v 12M -minrate 12M -maxrate 12M -bufsize 7340032 \
  -c:a ac3 -b:a 192k -dialnorm -24 \
  -program program_num=1:title=TEST1:st=0:st=1 \
  -program program_num=2:title=TEST2:st=2:st=3 \
  -streamid 0:0x101 -streamid 1:0x104 -streamid 2:0x201 -streamid 3:0x204 \
  -f mpegts -mpegts_transport_stream_id 0x0101 -mpegts_pmt_start_pid 0x30 \
  -muxrate 37000000 -pcr_period 30 -pat_period 0.1 -sdt_period 3600 -max_delay 500000 \
  pipe:1 \
| tsp --realtime -b 37000000 --max-flushed-packets 7 --max-input-packets 7 \
  -P filter --negate --pid 0x11 --stuffing \
  -P inject --pid 0x1FFB --bitrate 60000 --stuffing --poll-files \
       mgt.xml=150 psip.xml=400 stt.xml=1000 \
  -P inject --pid 0x1D00 --bitrate 20000 --stuffing --poll-files eit0.xml=500 \
  -P regulate --bitrate 37000000 --packet-burst 7 \
  -O ip --packet-burst 7 --enforce-burst --local-address GIGE_SIDE_IP --ttl 4 \
       239.10.0.1:5000
```

- For RTP, add `--rtp` to `-O ip` and set the box's stream URL to `RTP://239.10.0.1:5000`.
- Tested [V]: the same pipeline writing to a file (40 s, 186.6 MB), and to unicast
  `127.0.0.1:5000` captured with `tsp -I ip 5000`. Results: TS rate from PCR 36,999,998 b/s,
  two services, 0 CC errors, 2,848 PCRs with 0 jitter, MGT every 146 ms (max 170), CVCT every
  344 ms (max 440), STT advancing.
- Multicast was not sent from this host, to avoid reaching the box.
- What it lacks:
  - ATSC AC-3 descriptor, caption_service_descriptor and the `GA94` registration in the PMTs.
    The PMTs carry only stream type 0x81 + `AC-3` registration, plus ISO 639 if the stream has
    language metadata [V: `mpegtsenc.c` lines 575–604].
  - RRT, ETT.
  - Real content.
  - These are fine for a first "does the box lock and does a TV show 50.1/50.2 with a guide
    entry" test.
- The `title=` metadata only feeds ffmpeg's SDT, which we drop; the TV name comes from the
  CVCT `short_name`.
- Two-process variant (closer to per-program encoders):
  `tsp -b 37000000 -I null -P regulate -P merge "ffmpeg ... prog1 -f mpegts pipe:1" -P merge
  "ffmpeg ... prog2 ..." ...`. Each ffmpeg needs a distinct `-mpegts_service_id`,
  `-mpegts_pmt_start_pid` and `-streamid`, and `merge` merges the PATs. Untested [K].

**(b) Analyze a captured TS** (file from `tsp -I ip`, `hdhomerun_config ... save`, or `-O file`):

```sh
# Whole-stream report: services, PIDs, bitrates, table repetition, CC errors, STT range
tsanalyze --atsc --usa capture.ts > report.txt
tsanalyze --atsc --usa --json capture.ts > report.json      # for automated checks

# All PSIP / PSI tables, decoded (MGT, CVCT, STT, EIT-k from MGT-declared PIDs, PAT, PMT)
tstables --atsc --usa capture.ts
tsp --atsc -I file capture.ts -P tables --pid 0x1FFB --pid 0x1D00 --max-tables 10 -O drop

# Continuity counters and PCR accuracy (jitter vs packet position)
tsp -I file capture.ts -P continuity -P pcrverify -P pcrverify --jitter-max 14 -O drop

# Bitrate over time (alarm below/above), and ffprobe's independent view of programs
tsp -I file capture.ts -P bitrate_monitor --periodic-bitrate 5 --min 36000000 --max 38800000 -O drop
ffprobe -v error -show_programs -show_streams -of compact capture.ts

# Live, from multicast: same plugins with -I ip
tsp -I ip --local-address GIGE_SIDE_IP 239.10.0.1:5000 -P continuity -P pcrverify \
    -P analyze --interval 30 -o live-report.txt -O drop
```

- `pcrverify` on a file checks PCR against byte position (mux accuracy).
- Network-arrival jitter needs `pcrverify --input-synchronous` on a live `-I ip` input [K: option
  exists, semantics not tested].

## 3. FFmpeg / libav

### 3.1 mpegts muxer (FFmpeg 9.0.2, `libavformat/mpegtsenc.c` at tag n9.0.2) [V]

- **Options:**
  - `muxrate` for CBR, `pcr_period` (default 20 ms), `pat_period` (default 0.1 s),
    `sdt_period` (default 0.5 s), `nit_period`, `tables_version`.
  - `mpegts_transport_stream_id`, `mpegts_service_id`, `mpegts_pmt_start_pid`,
    `mpegts_start_pid`, `-streamid` per stream, `-program` for multiple programs.
  - `mpegts_flags`: `system_b`, `resend_headers`, `pat_pmt_at_frames`, `initial_discontinuity`,
    `nit`, `omit_rai`.
- **CBR model:**
  - With `muxrate` set, `get_pcr()` = byte count × 8 / muxrate + `first_pcr`. PCR is therefore
    exact relative to packet position (0 jitter in our test).
  - Null packets are inserted only while `dts − pcr > max_delay` (lines 1534–1567).
  - So CBR padding happens only when the muxer is called with data. If an encoder stalls, the
    muxer emits nothing.
  - If encoders exceed `muxrate`, PCR falls behind DTS and the muxer does not drop, warn or
    clamp. The decoder buffer then underflows.
- **ATSC (System A, the default):**
  - AC-3 is written as stream type 0x81 with a `registration_descriptor('AC-3')` (lines 410–418,
    575–578).
  - No ATSC AC-3 audio descriptor (tag 0x81), no caption_service_descriptor, no `GA94`, no PSIP.
  - SDT is always written. Setting `sdt_period` large reduces it to one copy at start.
  - `system_b` switches to DVB descriptors (0x06 + AC-3 descriptor).
- **Interleave:**
  - Each PES is written in one go (whole access unit, back to back).
  - In an MPTS that means one program's I-frame blocks other programs for its duration.
  - At 38.8 Mbps a 500 kB I-frame is about 100 ms of the whole carrier. That is legal T-STD-wise
    for TB/MB [K], but it is bursty and takes the other programs' PCR spacing to its limit.
    PCR-only packets are inserted, so the PCR interval is kept.
- **Not possible:** changing the program set without closing and reopening the muxer (new
  PAT/PMT version).
- **Verdict:**
  - Use it only for the spike and for test fixtures.
  - Our server uses libav as a codec library only (decode, scale, encode) and writes its own TS
    packetizer, PSI/PSIP and clock-driven CBR scheduler.
  - The PES and adaptation-field formats are small and well specified (`mpeg2ts` 0.6.1 or
    `mpegtsenc.c` are references).

### 3.2 Encoders (options from `ffmpeg -h encoder=...` on 9.0.2) [V]

- **`mpeg2video`** (native, LGPL):
  - Rate control: `b`, `maxrate`, `minrate`, `bufsize` (VBV), `rc_init_occupancy`, `qmin/qmax`,
    `lmin/lmax`, `rc_eq`.
  - Picture options: `-g`, `-bf`, `+cgop` (closed GOP), `sc_threshold`, `intra_vlc`,
    `non_linear_quant`, `alternate_scan`, `seq_disp_ext`, `video_format`, `+ildct+ilme` for
    interlaced.
  - `a53cc` defaults to true: `AV_FRAME_DATA_A53_CC` side data is written as ATSC user data in
    the picture header (`mpeg12enc.c`).
  - Quality is adequate, not broadcast-grade [K]. Use `trellis`, `mbd=rd`, `cmp/subcmp`,
    `intra_vlc=1`, `non_linear_quant=1`, `dc=10` for HD.
  - No filler data in the ES; the TS mux pads.
  - The average bit rate is read once at init (`mpegvideo_enc.c` ~line 581); `ratecontrol.c`
    re-reads `rc_max_rate`/`rc_buffer_size` per frame.
  - So statmux on MPEG-2 means reopening the encoder at a closed-GOP boundary, or a small libav
    patch.
- **`libx264`** (GPL):
  - Options: `nal-hrd` (`none|vbr|cbr`; cbr writes filler), `aud`, `a53cc` (default true),
    `crf` + `crf_max`, `rc-lookahead`, `mbtree`, `forced-idr`, `sc_threshold`, `intra-refresh`,
    `x264-params` for anything else (`repeat-headers=1`, `keyint`, `min-keyint`,
    `scenecut=0`, `open-gop=0`).
  - `libx264.c` `reconfig_encoder()` calls `x264_encoder_reconfig` when
    `bit_rate`/`rc_max_rate`/`rc_buffer_size`/`crf` change between frames, without an IDR [V,
    source read by a research agent; see section 5.1]. VBV must be enabled at open.
  - Under a statmux use `nal-hrd=vbr`, not `cbr`: `cbr` writes filler NALs that the mux should
    replace with null packets.
  - x264's lookahead cost and frame-type statistics are not exposed through libavcodec [K].
  - Capped CRF (`crf` + `maxrate` + `bufsize`) is the natural statmux primitive.
- **`ac3`** (native float):
  - Options: `b:a`, `dialnorm` (−31..−1; **default −31**, i.e. "no attenuation"), `dsur_mode`,
    `dmix_mode`, mix levels, `room_type`, `per_frame_metadata`, `channel_coupling`,
    `stereo_rematrixing`.
  - Recommended: normalize to −24 LKFS (ATSC A/85) with `loudnorm` or `ebur128`-measured gain
    and set `dialnorm=-24`.
  - Rates: 192 kb/s stereo, 384 kb/s 5.1 [K].
  - E-AC-3 (`eac3`) is irrelevant: TVs on clear QAM expect AC-3, and the box documents AC-3
    only.
- **Loudness:**
  - `loudnorm` (EBU R128 / two-pass or live one-pass with dynamic mode; adds up to 3 s lookahead
    latency and upsamples to 192 kHz internally [K]).
  - `ebur128` (measurement only, can inject metadata).
  - For live ingest, prefer measuring with `ebur128` and applying a slow gain, or `loudnorm`
    one-pass with `linear=true` when the measured values are known [K].
- **Hardware encoders:** see section 9. Summary: none fits MPEG-2. NVENC is a possible
  progressive-H.264 offload.

### 3.3 `ffmpeg-the-third`

- 6.0.0+ffmpeg-9.0, released 2026-08-09, WTFPL, repo shssoichiro/ffmpeg-the-third. The producer
  already pins it with `default-features = false, features = ["codec", "format",
  "software-scaling"]` [V].
- **Exposed** [V, crate source]:
  - `encoder::video::Video::open_with(Dictionary)`, so every private AVOption above (x264-params,
    nal-hrd, a53cc, dialnorm, …) is reachable as strings.
  - `set_bit_rate`, `set_max_bit_rate`, `set_gop`, `set_max_b_frames`, `set_qmin/qmax`.
  - Codec `Flags::INTERLACED_DCT/INTERLACED_ME`, `set_field_order`.
  - `frame::Frame::new_side_data(side_data::Type::A53CC, n)`; `Video::set_kind` (picture type,
    to force IDR).
- **Not exposed** [V]: `rc_buffer_size`, `rc_min_rate`, `rc_initial_buffer_occupancy`. Write them
  through `as_mut_ptr()` in a named `unsafe` module.
- Mid-stream reconfiguration (x264) means writing `bit_rate`/`rc_max_rate`/`rc_buffer_size` on
  the open context before `send_frame`. That works with `set_bit_rate`/`set_max_bit_rate` plus
  one raw write.
- Filters (`loudnorm`, `aresample`, `scale`) need the `filter` feature.

## 4. Rust crates and other PSIP implementations

Checked on crates.io and GitHub on 2026-11-24 by a research agent; values as reported (crates
API, repo API).

### 4.1 TS / PSI

| Crate | Version, release | License | Verdict |
|---|---|---|---|
| `mpeg2ts` (sile) | 0.6.1, 2026-08-11 | MIT | Reference only. Writer has PAT/PMT/PES/null/raw section, single-packet tables, no PCR/CBR scheduling. |
| `mpeg2ts-reader` (dholroyd) | 0.18.2, 2025-01-29 (commit 2026-05) | MIT/Apache-2.0 | Use as dev-dependency to re-parse our output. Decode only. |
| `mpegts` | 0.1.2, 2018 | MIT | Avoid (dead). |
| `va-ts` | 0.0.4, 2021 | MIT | Avoid. |
| `ts-analyzer` | 0.3.0, 2026-03 | MIT | Avoid (tiny). |
| `mpeg-ts`, `dvb-si`, `dvb-conformance` (fishloa/rust-broadcast) | dvb-si 16.0.0, 2026-11-23 | MIT OR Apache-2.0 | Reference only. New, fast-moving (60 versions, major 16), DVB only, no PSIP tables. `dvb-conformance` (TR 101 290) may be a useful extra check. |
| `libmpegts` (cesbo) | 0.8.2, 2026-11-09 | MIT | Reference only (DVB PSI writer, no ATSC). |
| `gstreamer-mpegts` | 0.25.2 | MIT/Apache (C lib LGPL) | Avoid (builds only MGT/STT/RRT). |
| `crc` | 3.4.0 | MIT OR Apache-2.0 | Use: `crc::CRC_32_MPEG_2` (poly 0x04C11DB7, init 0xFFFFFFFF, no reflect, check 0x0376E6E7). |

No crate writes ATSC A/65 PSIP.

### 4.2 Transport and ingest

| Crate | Version, release | License | Verdict |
|---|---|---|---|
| `rml_rtmp` | 0.8.0, 2023-04-29 | MIT | Use only if libav `listen` is not enough. Sans-IO, widely used, frozen. |
| `xiu` / `rtmp` (harlanc) | 0.6.6, 2026-10-16 | MIT | Reference (full server, heavy). |
| `rtmp-rs`, `scuffle-rtmp` | 0.6.0 / 0.2.3 | MIT / MIT-Apache | Watch. |
| `m3u8-rs` | 6.0.1, 2026-07-30 | MIT | Use for playlists; no typed LL-HLS tags. |
| `hls_m3u8` | 0.7.0 | MIT/Apache | Avoid (no LL-HLS). |
| `mp4-atom` | 0.17.1, 2026-11-04 | MIT OR Apache-2.0 | Use if we demux fMP4 in Rust. |
| `mp4` | 0.14.0, 2023 | MIT | Avoid (stale). |
| `srt-tokio` | 0.4.4, 2024-05 | Apache-2.0 | Avoid; use libav's `srt://` (libsrt) if ever needed. |
| `rtp` (webrtc-rs) | 0.17.3 | MIT/Apache | Not needed; RFC 2250 header is about 30 lines. |

### 4.3 Bitstream, codec, captions

| Crate | Version | License | Verdict |
|---|---|---|---|
| `bitstream-io` | 4.10.0 | MIT/Apache-2.0 | Use (section writer). |
| `deku` | 0.20.3 | MIT/Apache | Optional; awkward with computed lengths/CRC. |
| `binrw`, `modular-bitfield` | 0.15.2 / 0.13.1 | MIT / MIT-Apache | Avoid for PSIP. |
| `h264-reader` | 0.9.0 | MIT/Apache | Use for SEI/SPS inspection of ingest. |
| `cea708-types` (Matthew Waters, Centricular) | 0.4.1 | MIT | Use: cc_data + DTVCC service blocks, has a writer. |
| `cea608-types` | 0.1.5 | MIT | Use. |
| `scte35-reader` / `scte35` | 0.16.0 / 0.2.0 | MIT-Apache / MIT | Optional (not needed now). |
| AC-3 and MPEG-2 parser crates | 0.0.x, 2026 | various | Avoid; libav suffices. |

### 4.4 GStreamer

- `gstreamer` crate 0.25.4 (MIT/Apache).
- gst-plugins-rs has no Rust TS muxer; `mpegtslivesrc` is a live TS source with PCR clock
  recovery.
- `mpegtsmux` and `atscmux` are C (gst-plugins-bad, LGPL). `atscmux` adds AC-3 descriptors and
  empty MGT/STT/RRT, but no VCT/EIT/ETT and no STT clock update.
- `libgstmpegts` can build only MGT/STT/RRT.
- The closed-caption plugin (`cea708mux`, `cea608tocea708`, `tttocea708`, MPL-2.0) is good
  reference code for caption muxing.
- Verdict: avoid as a dependency.

### 4.5 PSIP implementations elsewhere

| Project | License | Writes PSIP? | Use |
|---|---|---|---|
| TSDuck | BSD-2-Clause | Yes, all tables | Oracle, spike |
| libdvbpsi (VideoLAN) | LGPL-2.1 [K] | No; ATSC MGT/VCT/EIT/ETT/STT decode only. Last release 1.3.3 (2019), commit 2024-11. | Second decoder in tests |
| VLC `modules/demux/mpeg/ts_psip.c` | GPL-2.0 | No (decode via libdvbpsi into EPG) | Receiver test |
| MythTV `atsctables.cpp` | GPL-2.0 | No | Reference |
| tvheadend `epggrab/module/psip.c` | GPL-3.0 | No | Reference |
| OpenCaster (dvbobjects, Python) | GPL-2.0 | MGT, TVCT, ATSC EIT, ETT; no STT/CVCT/RRT. Last commit 2014. | Layout reference only |
| libdvbtee | LGPL-2.1 | No (decode, ATSC multiple_string_structure) | Reference |
| GNU Radio gr-dtv | GPL-3.0 | No (8-VSB PHY only) | Irrelevant |
| GitHub "psip_generator" projects | none | Wrappers around TSDuck, or abandoned | Irrelevant |

Conclusion: write PSIP in-house. Check against TSDuck in CI, and spot-check with libdvbpsi, VLC
and a real TV.

## 5. Statistical multiplexing

Researched by an agent on 2026-11-24 from FFmpeg master `5abfe3a` and x264 source.

### 5.1 What the encoders allow

- **libx264 through libavcodec** (`libavcodec/libx264.c`):
  - `reconfig_encoder()` (L201–246) runs on every frame, via `reconfig_encoder_from_frame`.
  - It calls `x264_encoder_reconfig` when any of these change:
    - `rc_buffer_size` or `rc_max_rate` (become VBV bufsize/maxrate)
    - `bit_rate` (ABR mode only)
    - the `crf`, `cqp` or `crf_max` options
    - SAR, field order
  - Writing those `AVCodecContext` fields between `send_frame` calls works on FFmpeg 9.0 today.
  - FFmpeg 9.1 adds `avcodec_encode_reconfigure()` + `AV_CODEC_CAP_ENCODER_RECONF` (APIchanges
    2026-08-13, lavc 63.8.101). `ffmpeg-the-third` 6.0.0 targets 9.0 and does not wrap it.
- **x264** (`encoder/encoder.c` `encoder_try_reconfig`):
  - Maxrate, bufsize and bitrate can change only if VBV was enabled at open (old and new values
    above 0).
  - A change takes effect on "whichever frame is encoded next", i.e. about lookahead + threads
    frames later.
- **x264 HRD mode:** use `nal-hrd=vbr` (capped, no filler) under a statmux. `nal-hrd=cbr` spends
  bits on filler NALs that the mux should replace with null packets. This corrects the
  `nal-hrd=cbr` suggestion in section 3.2 for the statmux case.
- **mpeg2video:**
  - `rc_max_rate`, `rc_min_rate` and `rc_buffer_size` are re-read per frame (`ratecontrol.c`
    `modify_qscale` L172–175, `ff_vbv_update` L736–738).
  - `bit_rate` is latched at init (`mpegvideo_enc.c` L581), and the ABR target is cumulative
    (`wanted_bits = bit_rate * pts / fps`).
  - The sequence header carries `rc_max_rate` and the VBV size (`mpeg12enc.c` L192–210).
  - Recommended: close and reopen the encoder at a closed-GOP boundary when the allocation
    changes, with `rc_initial_buffer_occupancy` set to the tracked VBV fullness (otherwise the
    default is 3/4 full).
  - Alternative: a small libav patch that re-reads `bit_rate`.
- **NVENC:** a reconfig forces an IDR (section 9). QSV flushes and resets. VAAPI and AMF cannot
  reconfigure.
- **Complexity statistics:**
  - No encoder exposes its lookahead costs through libavcodec.
  - Usable inputs: per-packet bits and `AV_PKT_DATA_QUALITY_STATS` (QP), and our own cheap
    pre-analysis.

### 5.2 Prior art

- **No open-source statmux exists** (searches on GitHub; the field is patents and commercial
  products).
- **OBE/obe-rt** (GPL-2.0): "statmux pools" were only ever planned (2014 wiki).
  - Its muxer `kierank/libmpegts` (GPL-2.0, pushed 2026-11-18) has `TS_TYPE_ATSC`, CBR null
    padding and a T-STD model, but no VCT/MGT.
  - This is a different project from cesbo's Rust `libmpegts`.
- **Upipe** (MIT core, upipe-ts LGPL-2.1+, pushed 2026-10-26): `upipe_ts_mux` has CBR and capped
  VBR modes, ATSC PAT/PMT timing conformance and a T-STD model, but no statmux and no PSIP. It
  is the best design reference for a clock-driven TS mux.
- **Literature:**
  - Perkins & Arnstein, "Statistical Multiplexing of Multiple MPEG-2 Video Programs in a Single
    Channel", SMPTE J. 104(9), 1995.
  - Böröczky et al. (IBM), "Statistical multiplexing with look-ahead using MPEG-2 video encoding".
  - Nokia Bell Labs, "Joint Encoder and Buffer Control for Statistical Multiplexing".
  - US 6859496 / 6956901 / 7085322 / 6643327, which change rates at GOP boundaries or on scene
    change.
  - The TM5 complexity measure X = bits × average quantizer [K].

### 5.3 Recommended design

1. **Carrier budget.**
   - 256-QAM Annex B payload is 38.8107 Mb/s; the HDHomeRun reports `bps=38807712`.
   - Choose a fixed output rate a little below it (e.g. 38.0 Mb/s; the box adds nulls).
   - Subtract fixed overhead:
     - PAT/PMTs (about 15 kb/s per PID at 10 Hz)
     - PSIP (about 100–200 kb/s for MGT, CVCT, STT, EIT-0..3, ETT)
     - AC-3 rates × about 1.05
     - PCR-only packets
     - 2–3% headroom
   - The rest is the video pool. Allow about 3% of it for TS/PES packing.
2. **Encoders.** One per program, VBV-capped, no filler (x264 `nal-hrd=vbr`, mpeg2video
   `maxrate`/`bufsize`).
   - Keep the VBV delay constant: `bufsize_i = maxrate_i × D` (D ≈ 0.5 s, within the level limit),
     so the mux's PCR→DTS offset stays fixed when rates change.
3. **Controller.** Runs per GOP (fixed GOP cadence, all programs aligned or not) and on scene
   cuts.
   - Complexity C_i = EWMA of (bits × QP) over the last GOP, optionally refined by a cheap
     pre-analysis of decoded input (downscaled SAD/variance, at the cost of about 0.5 s delay).
   - Allocation: R_i = Rmin_i + (Rpool − ΣRmin) · w_i·C_i^α / Σ w_j·C_j^α, with α ≈ 0.7–1 and
     priority weights w_i.
   - Clamp to [Rmin_i, Rmax_i], limit slew to ±15–20% per GOP, add hysteresis.
4. **Make-before-break.**
   - Decrease: lower the encoder cap first, and lower the mux's delivery rate for that program
     only after the change has taken effect plus one VBV delay (D + x264 lookahead).
   - Increase: raise the mux rate first, then the encoder.
   - The sum of mux delivery rates never exceeds the pool. Increases are funded only from
     decreases that have completed.
5. **Mux.**
   - A wall-clock-driven scheduler emits one packet per slot at the fixed TS rate. Each program's
     ES queue drains at its allocated rate under a DTS deadline. Audio and PSI are scheduled
     just in time, at higher priority.
   - Empty slots carry null packets. PCR is stamped from the output slot clock, so it is exact by
     construction.
   - An encoder stall never stops the mux: its program gets nulls, then the slate.
6. **T-STD.**
   - Video TB leak is 1.2 × Rmax (MP@HL 96 Mb/s), so bursts at 38.8 Mb/s are harmless.
   - AC-3 has a small main buffer (about 2.5 kB in ATSC [K]), so audio may lead its PTS by only
     about 50 ms.
   - PCR interval ≤ 40 ms (TR 101 290 limit 100 ms) and PCR accuracy within ±500 ns.
7. **Start simple.** Milestone 1 is static per-program caps (no statmux). Milestone 2 is the GOP
   controller on x264. Milestone 3 is MPEG-2 via reopen-at-GOP.

## 6. Whole-system alternatives

| Tool | MPTS CBR | ATSC PSIP | Statmux | License, status |
|---|---|---|---|---|
| TSDuck `tsp` | Yes: `merge`, `regulate`, `inject`, `-O ip --rtp` | Yes (static XML tables; no live STT, no ATSC EIT generator) | No | BSD-2-Clause; v3.45 2026-09 |
| Upipe | Yes (CBR, capped VBR, T-STD) | ATSC timing only, no tables | No | MIT + LGPL; active 2026-10 |
| OBE / libmpegts (kierank) | Yes, ATSC type | No VCT/MGT | Planned only | GPL-2.0; mixed activity |
| GStreamer `mpegtsmux` / `atscmux` | `bitrate` pads with nulls; `pcr-interval` | `atscmux`: empty MGT/STT/RRT only | No | LGPL; gitlab issue #1491 reports CBR overshoot |
| FFmpeg mpegts | `-muxrate`, multiple programs | No (writes DVB SDT) | No | LGPL; trac #8048 reports CBR overshoot |
| dvblast | Demux/remux of DVB inputs (ATSC input mode) | No | No | GPL-2.0; pushed 2026-11 |
| multicat / aggregartp / ingests / offsets | Record and replay with 27 MHz timing, RTP aggregation | – | – | GPL-2.0; pushed 2026-06 |
| OpenCaster | `tscbrmuxer`, null shaper, PSI generators | DVB; partial ATSC (MGT/TVCT/EIT/ETT) | No | GPL-2.0; dormant |
| MediaMTX, srt-live-server | Relays only | No | No | MIT; SLS stale |
| ffplayout | Single-channel playout | No | No | GPL-3.0; v2.9.0 2026-11 |
| Thor H-HERMES-64 (appliance) | IP-in, H.264→MPEG-2 transcode, QAM-out | Yes [K] | Unknown | Commercial |

Nothing open does MPTS + ATSC PSIP + statmux. The closest pieces are TSDuck (tables, pacing)
and Upipe (mux design). This confirms "write the mux, PSIP and controller; reuse codecs".

## 7. Monitoring and probe side

### 7.1 HDHomeRun

- **libhdhomerun:** github.com/Silicondust/libhdhomerun, LGPL-2.1, last commit 2026-11-24.
  - Latest tag is `20361107` (sic, published 2026-11-14), previous `20260313`.
  - nixpkgs `libhdhomerun` is 20250506 and provides `hdhomerun_config`.
  - No Rust crate exists on crates.io.
  - The control protocol is small (TCP 65001, GETSET_REQ 0x0004, tags 0x03 name / 0x04 value,
    CRC32; `hdhomerun_pkt.h` L111–129), so a native client is feasible later.
- **Commands** (info.hdhomerun.com/info/hdhomerun_config):
  - `hdhomerun_config <id> set /tuner0/channelmap us-cable` (stored in NVRAM; also `us-hrc`,
    `us-irc`).
  - `set /tuner0/channel auto:<freq_hz>` or `auto:<ch>`. Explicit modulation (`qam256:<freq>`)
    is allowed; `get /sys/features` lists the accepted modulations.
  - `get /tuner0/status`, e.g. `ch=qam:33 lock=qam256 ss=83 snq=90 seq=100 bps=38807712 pps=0`:
    - `ss`: signal strength (80 ≈ −12 dBmV).
    - `snq`: signal-to-noise quality (library colors it green ≥ 70, yellow ≥ 50).
    - `seq`: symbol error quality from uncorrectable errors; 100 means clean
      (`hdhomerun_device.c` L639–655).
    - `bps`: raw channel rate.
    - `pps`: packets per second to the network.
  - `get /tuner0/streaminfo` lists `prog: major.minor name` from our VCT. This is a direct PSIP
    check from the receiver's point of view.
  - `set /tuner0/program N`.
  - `set /tuner0/target udp://ip:port` (or `rtp://`).
  - `save /tuner0 capture.ts` (or `-` for stdout). At exit it prints overflow, network, transport
    (TEI) and sequence (CC) error counts.
- **HTTP API** (info.hdhomerun.com/info/http_api):
  - `/discover.json`, `/lineup.json`.
  - Streaming: `http://IP:5004/auto/v50.1`, by frequency `/auto/ch<freq_hz>`, one program
    `/auto/ch<freq>-<prog>`. Replace `auto` with `tuner0..3` to pin a tuner.
  - `?duration=N`. `?transcode=` exists on EXTEND models only.
  - Check the purchased model before relying on the frequency form.
- **Plan:**
  - A monitor task cycles a tuner across the 16 carriers.
  - Per carrier: `status` (lock, ss/snq/seq), `streaminfo` (VCT as received), a 10–30 s capture
    piped to TSDuck (`tsp -I file - -P continuity -P pcrverify -P analyze --json`).
  - Export to Prometheus.
  - Start by shelling out to `hdhomerun_config`; write a native client only if needed.

### 7.2 TS analysis tools

- **TSDuck:** primary tool (section 2.3), full ATSC support.
- **ffprobe:** PAT/PMT/SDT, `-show_programs`, CC warnings. No PSIP [K].
- **libdvbpsi `dvbinfo`:** independent ATSC MGT/VCT/EIT/ETT/STT decoder.
- **DVB Inspector** (GPL-3.0, 1.21.0 2025-10): good PCR and bitrate graphs. Its repo tree has no
  ATSC PSIP classes.
- **TR 101 290 open tools:**
  - perfect-streamer-toolkit `ts_analyze` (Apache-2.0, 2026-11; source availability to check).
  - `streamtop` (MIT).
  - Cinegy TsAnalyser (Apache-2.0, last release 2017).
  - `dvb-conformance` crate (section 4.1).
  - `tsduck-prometheus` (MIT, 2023).
- **Real TVs and VLC:** final acceptance, including guide display and caption decoding.

## 8. UDP/RTP output on Linux

- **Load:**
  - One carrier at 38 Mb/s with 1316-byte payloads is about 3,600 datagrams/s (one every
    ~277 µs).
  - Sixteen carriers is about 58k datagrams/s and about 620 Mb/s, within one GigE port but close
    enough to its limit that the two GigE ports should each take 8 carriers.
- **Userspace pacing first** (what multicat `util.c` and TSDuck `regulate` do):
  - One dedicated thread (optionally SCHED_FIFO) with a min-heap of per-carrier deadlines.
  - `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)` about 50 µs early, then a short spin.
  - One `sendmmsg` for everything due within about 100 µs. Destinations may differ per message.
  - The mux produces packets ahead of time into a bounded ring. The pacer never blocks the mux;
    the mux never waits on the network.
- **SO_TXTIME upgrade:**
  - With `CLOCK_MONOTONIC` it needs no capability (`net/core/sock.c` L1634–1641) and is honored
    in software by the `fq` qdisc on any NIC.
  - Installing fq needs CAP_NET_ADMIN once (`tc qdisc replace dev X root fq` or the
    `net.core.default_qdisc=fq` sysctl, set in the NixOS module).
  - Watch fq `flow_limit` (100 packets) and `horizon` [K].
  - ETF with hardware launch time (Intel i210/i225 [K]) is overkill.
- **Avoid UDP GSO** (`quinn-udp` uses UDP_SEGMENT): one transmit time per super-packet means
  bursts.
- **Crates:**
  - `socket2` 0.6.5 (MIT/Apache): `set_multicast_if_v4`, `set_multicast_ttl_v4`,
    `set_multicast_loop_v4`, `set_tos_v4`, `set_send_buffer_size`. `bind_device` needs feature
    `all`.
  - `nix` 0.32.0 (MIT): `sendmmsg`/`MultiHeaders`, `sockopt::TxTime`, `ControlMessage::TxTime`.
  - `io-uring` 0.7.16: an option, not needed.
- **Multicast:**
  - Set the outgoing interface explicitly (the GigE data NIC, never the control port: manual p.12
    forbids multicast reaching the 10/100 control port) and TTL 1–4.
  - IGMP snooping on the switch is the box's concern; it joins with IGMPv2 or v3.
- **RTP:**
  - 12-byte header, PT 33 (MP2T, RFC 3551), 90 kHz timestamp (RFC 2250), sequence number for
    loss detection, 7 × 188 = 1316-byte payload (1328 bytes of UDP payload).
  - Hand-written, about 20–30 lines.
  - Whether the box prefers RTP or UDP, and whether it requires exactly 7 packets per datagram,
    is still to be measured.
- **SMPTE 2022-1 FEC** (successor of Pro-MPEG CoP3; XOR row/column on ports +2/+4) is not
  documented for the box and is pointless on a clean LAN. Skip it.

## 9. Hardware encode offload

Researched by an agent on 2026-11-24 against vendor docs and FFmpeg master `5abfe3a`
(2026-11-24).

**Summary:**
- No hardware encoder on sale today does MPEG-2 encode with A/53 captions inside libavcodec.
- None of the dedicated transcode cards encodes MPEG-2 or interlaced video.
- MPEG-2 belongs on the CPU, and it is cheap there.
- For H.264, libx264 on the CPU is the only path that meets every requirement. NVENC is a
  workable offload for progressive H.264 only.

### 9.1 Dedicated transcode cards

| Card | Encode codecs | MPEG-2 enc / interlaced | Density (1080p30) | Status, price | Software |
|---|---|---|---|---|---|
| AMD/Xilinx Alveo U30 (EC2 VT1) | H.264, HEVC | No / not listed | 16 per card | End of life: "All new designs should migrate to MA35D" (xilinx.github.io/video-sdk/v3.0). The VT1 page is still up with no successor named. | Forked FFmpeg/GStreamer, XRT driver |
| AMD Alveo MA35D | H.264, HEVC, AV1 | No / progressive only ("Progressive support for H.264, H.265 and AV1", amd.github.io/ama-sdk/v1.1.2/specs_and_features.html) | 32–64 | About $1,400–1,600 (AMD list, DigiKey) | Pre-compiled forked FFmpeg; out-of-tree kernel driver, Ubuntu/RHEL targets [K] |
| NETINT Quadra (T1U etc.) | H.264, HEVC, AV1 | No / progressive only | 32 (T1U) | Quote-only, not sold to the general public (ffmpeg-devel 2025-07 thread) | libxcoder + patched FFmpeg; upstream patch series not merged (no `quadra`/`libxcoder` in FFmpeg master `configure`) |
| NETINT T408 | H.264, HEVC | No; interlaced decode/deinterlace on host CPU (NETINT APPS062) | 4×1080p60 per module | Legacy, cheap used [K] | libxcoder + patched FFmpeg |

- NixOS: AMD needs an out-of-tree module plus a vendor FFmpeg fork. NETINT appears as NVMe [K]
  but still needs a patched libav under `ffmpeg-the-third`.
- Verdict: none can produce our MPEG-2 or 1080i outputs. For progressive H.264 they add a forked
  libav for no gain over NVENC.

### 9.2 GPUs

**NVIDIA NVENC:**
- No MPEG-2 encode (support matrix: MPEG-2 is NVDEC only).
- Consumer cards: at most 12 concurrent encode sessions per system across all GPUs (NVENC
  Application Note 13.1). Qualified pro GPUs have no fixed cap.
- Field (interlaced) H.264 encode is supported only on Blackwell (RTX 50 series and Blackwell
  pro); not on Turing, Ampere or Ada.
- `nvenc.c`:
  - `cbr_padding` sets filler data; `aud`; SPS/PPS repeated in band unless GLOBAL_HEADER.
  - `strict_gop`, `forced-idr`, `no-scenecut`, `rc-lookahead`, `multipass`.
  - A/53 CC SEI from frame side data (`a53cc` defaults on).
  - `reconfig_encoder()` runs per frame and calls `nvEncReconfigureEncoder` on changes to
    `bit_rate`, `rc_max_rate` or `rc_buffer_size`, with `resetEncoder = 1; forceIDR = 1`. Every
    rate change therefore forces an IDR, so change rates only at GOP boundaries.
- NVDEC can decode the H.264 inputs.

**Intel QSV / VAAPI:**
- MPEG-2 encode appears only in the "PAK + shader/VME" table of intel/media-driver
  `docs/media_features.md`: BDW, SKL, KBLx, ICL, TGLx, DG1/SG1.
- It is absent on DG2/Arc, MTL, LNL, BMG, PTL and later. ADL/RPL presumably count as TGLx [K].
- `vaapi_encode_mpeg2.c` hard-codes progressive and has no A/53.
- `vaapi_encode_h264.c` has A/53 SEI but no interlace and no runtime RC reconfig.
- `qsvenc.c`:
  - Interlaced (`MFX_PICSTRUCT_FIELD_TFF`), `NalHrdConformance`, `aud`.
  - No A/53 at all.
  - Bitrate change flushes the encoder and calls `MFXVideoENCODE_Reset`.
- No hard session cap [K].

**AMD VCN (AMF/VAAPI):**
- No MPEG-2 encode.
- `amfenc_h264.c` has `enforce_hrd`, `filler_data`, `aud`, but no A/53 and no runtime reconfig.
- H.264 quality historically behind Intel/NVIDIA.

### 9.3 Broadcast constraints by option

| Requirement | mpeg2video (CPU) | libx264 (CPU) | NVENC H.264 | QSV | VAAPI | AMF | Cards |
|---|---|---|---|---|---|---|---|
| VBV/HRD CBR | VBV; mux pads | `nal-hrd=cbr` + filler | `cbr` + `cbr_padding` | NalHrdConformance | basic | `enforce_hrd`, `filler_data` | listed |
| Fixed GOP/IDR | `-g`, `+cgop`, `sc_threshold` | yes | `strict_gop`, `no-scenecut` | yes | yes | yes | yes |
| AUD, in-band SPS/PPS | n/a | yes | yes | `aud` | yes | `aud` | [K] |
| A/53 captions from side data | yes | yes | yes | no | H.264 only | no | undocumented |
| Interlaced | yes | yes | Blackwell only | yes | no | no | no |
| Runtime bitrate change | no (reopen at closed GOP, or patch) | yes, no IDR | yes, forces IDR | yes, flush + reset | no | no | undocumented |
| Complexity stats exposed | no | no (x264 API only) | no | no | no | no | no |
| H.264 decode offload | – | – | NVDEC | yes | yes | yes | yes |

### 9.4 CPU baseline

Measured on an AMD Ryzen 7 5800X (8C/16T), `-threads 1`, 5 s of `testsrc2` to `-f null`. Values
are cores per channel (user CPU-seconds per second of video). Input decode and scaling are
excluded.

| Encode | 480i/p29.97 | 720p59.94 | 1080i29.97 (`+ildct+ilme`) | 1080p29.97 |
|---|---|---|---|---|
| mpeg2video, bf=2 | 0.057 | 0.17 | 0.59 | 0.30 |
| libx264 veryfast | 0.10 | 0.44 | 0.58 | 0.55 |
| libx264 medium | 0.22 | 0.87 | 1.14 | 1.14 |

- testsrc2 is far easier than real content. Budget about 2× for real content, plus about
  0.1–0.2 core per 1080 H.264 decode [K].
- Rough sizing:
  - 96 SD MPEG-2 channels: about 11 cores.
  - 48 × 1080i MPEG-2: about 57 cores.
  - 1080 H.264 veryfast: about 1–1.2 cores per channel on real content.

### 9.5 Broadcast encoder appliances

- Baseband-input encoders need a decode-to-HDMI/SDI box per channel in a network-ingest design.
  Examples:
  - Blonder Tongue HDE-2H-QAM / HDE-8C-QAM / HDE-CHV-QAM (MPEG-2).
  - BT HD264-2S-IP (H.264, no QAM).
  - Thor H-xSDI-QAM.
- Costs of that design: double lossy generation, HDMI does not carry 608/708 captions, HDCP, and
  no software statmux control.
- IP-input transcoding gateways exist: Thor H-HERMES-64 takes 64 IP inputs, transcodes H.264 to
  MPEG-2 and outputs up to 32 QAM. It is a black box that would replace most of this project;
  caption and statmux behavior are unknown [K].
- Used Harmonic Electra and Ericsson encoders take IP input but carry licensing and support
  costs [K].

### 9.6 Recommendation for 16–96 channels

1. **MPEG-2 on the CPU (libavcodec `mpeg2video`).** It is the only path with interlaced output
   and A/53 user data.
   - Statmux by reopening at closed-GOP boundaries, or a small patch that re-reads
     `avctx->bit_rate`.
2. **H.264 on the CPU (libx264 veryfast/faster).** Settings: `nal-hrd`, `aud`, `a53cc`, fixed
   keyint, `scenecut=0`.
   - Statmux through `x264_encoder_reconfig` (no forced IDR).
3. **Decode offload (NVDEC/QSV)** is a cheap win if CPU is tight. Software decode is fine to start.
4. **Hosts:**
   - 16–30 mixed HD channels: one 16–32-core host.
   - 96 mostly-HD channels: about 100–130 cores (dual 64-core EPYC or two hosts).
   - SD-heavy line-ups need far less.
5. **Optional NVENC** for progressive H.264. Use a pro or Blackwell card beyond 12 sessions or
   for field encoding, and align rate changes to GOPs.
6. **Avoid** U30 (EOL), MA35D and NETINT (no MPEG-2, no interlace, forked libav), QSV (no A/53;
   MPEG-2 only on pre-Arc parts) and AMF (no A/53, no MPEG-2).

## 10. Proposed dependencies

Linked into the server:

| Dependency | Version | License | Role |
|---|---|---|---|
| FFmpeg (libavcodec/format/filter/swscale/swresample) | 9.0.x (nixpkgs 9.0.2) | LGPL-2.1+; GPL with libx264 | Decode, encode, filters |
| libx264 | nixpkgs [K version] | GPL-2.0+ | H.264 encode. **Makes the binary GPL.** Acceptable for an in-house server; note it. |
| `ffmpeg-the-third` | 6.0.0+ffmpeg-9.0 | WTFPL | libav bindings (as the producer) |
| `crc` | 3.4.0 | MIT OR Apache-2.0 | `CRC_32_MPEG_2` |
| `bitstream-io` | 4.10.0 | MIT/Apache-2.0 | PSI/PSIP/PES writers |
| `cea708-types` | 0.4.1 | MIT | cc_data / DTVCC |
| `cea608-types` | 0.1.5 | MIT | 608 pairs |
| `h264-reader` | 0.9.0 | MIT/Apache-2.0 | Ingest SPS/SEI inspection |
| `m3u8-rs` | 6.0.1 | MIT | LL-HLS playlists (if HLS ingest) |
| `mp4-atom` | 0.17.1 | MIT OR Apache-2.0 | Only if fMP4 is demuxed in Rust |
| `rml_rtmp` | 0.8.0 | MIT | Only if libav RTMP listen is insufficient |
| `socket2` | 0.6.5 | MIT OR Apache-2.0 | Multicast sockets |
| `nix` | 0.32.0 | MIT | `sendmmsg`, SO_TXTIME, `clock_nanosleep` |
| tokio, tokio-util, crossbeam-channel, serde, toml, humantime-serde, anyhow, thiserror, tracing | as the producer | MIT/Apache | Conventions |

Dev and test only:

| Dependency | Version | License | Role |
|---|---|---|---|
| `mpeg2ts-reader` | 0.18.2 | MIT/Apache-2.0 | Round-trip parse of our TS |
| TSDuck (external) | 3.44 nixpkgs / 3.45 upstream | BSD-2-Clause | Oracle in CI (`nix flake check`), spike |
| libdvbpsi `dvbinfo`, VLC (external) | 1.3.3 / current | LGPL / GPL | Second decoders, not linked |
| libhdhomerun `hdhomerun_config` (external) | 20250506 nixpkgs | LGPL-2.1 | RF probe control |

## 11. Risks

1. **Own mux correctness.** PCR, T-STD buffer and PSIP bugs show up only on some TVs.
   Mitigation:
   - TSDuck and mpeg2ts-reader in CI from the first commit.
   - HDHomeRun capture analysis.
   - Several TV models.
2. **MPEG-2 statmux.** libav `mpeg2video` cannot move its average target mid-stream. Reopening
   at closed GOPs must keep the VBV model continuous (`rc_initial_buffer_occupancy`) or the
   decoder buffer underflows at the splice. Possibly needs a libav patch, which would mean
   building FFmpeg in our flake.
3. **x264 reconfig lag.** A rate change lands lookahead + threads frames later. The
   make-before-break ordering must use the measured lag, not the request time.
4. **CPU budget.** 1080i MPEG-2 is about 0.6 core and 1080 x264 veryfast about 0.55 core on
   synthetic input, probably about 2× on real content. 96 HD channels need about 100+ cores.
   Measure on real output from the producer before committing to a channel plan.
5. **GPL.** Linking libx264 makes the server binary GPL. Fine in-house; matters only if
   distributed.
6. **Clock domains.** Live inputs run on the producer's clock, the output on ours. We must
   resample audio and drop or repeat video frames to hold the output timeline. The producer's
   timestamp resets and idle gaps make this mandatory.
7. **Box tolerances unknown:** packets per datagram, RTP vs UDP, burst and jitter tolerance,
   behavior above 38.8 Mb/s. The spike measures these before the mux design is fixed.
8. **TSDuck quirks.** Its video-attribute line misreported 59.94 Hz MPEG-2 as 25 Hz in 3.44 [V].
   Cross-check oracle output with ffprobe or libdvbpsi before trusting a failure.
9. **Ingest library gaps.** libav RTMP listen handles one publisher per open, and libav has no
   LL-HLS. Reconnect and multiple-producer handling is ours to write.
10. **New, fast-moving crates.** Several 2026 TS/SI crates (fishloa/rust-broadcast family) look
    machine-generated with unstable APIs. Do not depend on them.

## 12. Open questions

1. Does the EQAM accept RTP and plain UDP equally? Does it need exactly 7 TS packets per
   datagram? How much burst and jitter does it tolerate? (Spike.)
2. Does a real TV on this plant pick up our CVCT names and EIT titles from the spike stream, and
   does it require RRT or ETT? (Spike plus TV.)
3. Which TVs decode H.264 over QAM? This decides the MPEG-2 to H.264 mix and the CPU budget.
4. Build FFmpeg ourselves (to patch mpeg2video rate control or move to 9.1's
   `avcodec_encode_reconfigure`), or stay on nixpkgs libav and reopen encoders?
5. Is 1080i MPEG-2 needed, or is 720p60/480i enough? 1080i costs about 3.5× a 720p channel on
   the CPU.
6. Is a statmux worth it at 6 programs per carrier, or do fixed per-program caps tuned per
   channel suffice at first? Measure quality at, say, 6 × 6 Mb/s.
7. What exact HDHomeRun model was bought (HTTP frequency tuning form, number of tuners)?
8. Should the producer side add an MPEG-TS/UDP or SRT sink to avoid RTMP entirely?
9. Is the perfect-streamer-toolkit `ts_analyze` actually open source, and is it worth adding as
   a TR 101 290 checker?
