# User guide

This guide explains how to set up and run totalblonder. It covers building, the configuration
file, every input type, channels and audio, the program guide, the modulator, monitoring and
the NixOS module.

The configuration reference is
[`crates/totalblonder/totalblonder.example.toml`](../crates/totalblonder/totalblonder.example.toml).
It lists every key with its default. The design behind each feature is in [`SPEC.md`](SPEC.md).

## Contents

1. [What you need](#1-what-you-need)
2. [Build and run](#2-build-and-run)
3. [How a configuration is organized](#3-how-a-configuration-is-organized)
4. [Channels: carriers, programs and services](#4-channels-carriers-programs-and-services)
5. [Inputs](#5-inputs)
6. [Audio](#6-audio)
7. [The program guide](#7-the-program-guide)
8. [Bit rate: the statmux](#8-bit-rate-the-statmux)
9. [The modulator](#9-the-modulator)
10. [Monitoring](#10-monitoring)
11. [NixOS](#11-nixos)

## 1. What you need

- **A modulator that takes MPEG transport streams over IP.** totalblonder was built for the
  Blonder Tongue EQAM-420B in MPTS mode. Its output is a standard constant-rate multi-program
  transport stream (MPTS) per carrier, over UDP or RTP. Other IP-to-QAM modulators that pass
  an MPTS through unchanged should work, but only the EQAM-420B has been tested, and only it
  has a supervisor (§9).
- **A GPU for video encoding.** An NVIDIA card with CUDA, or an AMD card with HIP. The server
  encodes MPEG-2 video on the GPU. Decoding of live inputs uses the card too when it can.
  Without a GPU, the server runs only on the reference kernels (`video.device = "ref"`), which
  are for tests and are too slow for real time.
- **Linux with user namespaces**, for the decoder sandbox (§5.1).
- **Nix with flakes**, to build.
- **Cable-ready TVs** on the modulator's RF output. They tune the channels as two-part numbers
  (for example 20.1) with names and a guide.

## 2. Build and run

```sh
nix build .#totalblonder .#tb-avworker   # the server and the sandboxed decoder worker
```

Copy the example configuration and edit it (§3). Then check it:

```sh
totalblonder --config my.toml --check
```

`--check` reads and validates the whole configuration and prints what is wrong. It does not
start anything.

To run the server:

```sh
TB_AVWORKER=/path/to/tb-avworker totalblonder --config my.toml
```

`TB_AVWORKER` is needed when any program has a live input. The NixOS module sets it for you.

Other modes:

| Command | What it does |
|---|---|
| `totalblonder --config my.toml --duration 60` | Runs in real time for 60 s and stops |
| `totalblonder --config my.toml --offline 10 --out DIR` | Makes 10 s of output as fast as it can into files in `DIR`, on a virtual clock (no live inputs) |

The server stops cleanly on SIGTERM.

## 3. How a configuration is organized

A configuration is one TOML file. Unknown keys are errors, so a typo cannot be silently
ignored.

| Section | What it sets |
|---|---|
| `[output]` | The carrier rate, framing (UDP or RTP), packets per datagram, timing |
| `[[output.interface]]` | Which local interface sends which carriers (multicast interface, TTL) |
| `[video]` | The GPU: `cuda`, `hip` or `ref` |
| `[psip]` | Table options: GPS offset, daylight-saving rule, guide length, the schedule file |
| `[[carrier]]` | One QAM carrier: its modulator input slot, destination address, programs |
| `[[carrier.program]]` | One TV channel inside a carrier: video format, rates, audio, inputs |
| `[[service]]` | A channel's identity: number, short and long name, source id |
| `[[input]]` | A live source: RTMP, SRT, HLS, MPEG-TS over UDP/RTP, or a file |
| `[statmux]` | Sharing a carrier's bit rate between its programs |
| `[overload]` | Stepping a program down to a smaller format when the GPU falls behind |
| `[device]` | The modulator supervisor |
| `[control]` | The address of the metrics and health API |
| `[telemetry]` | OpenTelemetry export of logs and metrics |
| `[persistence]` | Where table versions are kept across restarts |
| `[[rating_system]]` | Parental-control rating systems of your own |

## 4. Channels: carriers, programs and services

A **carrier** is one QAM channel on the cable, about 38.8 Mbit/s. On the EQAM-420B, carrier
*n* is the modulator's input slot *n* (1 to 16), and the modulator decides which RF channel
it goes out on (§9).

```toml
[[carrier]]
id = 1                                   # the modulator's input slot
destination = "udp://239.10.0.1:5000"    # where its MPTS is sent; also rtp:// or file:/path.ts
```

A **program** is one TV channel inside a carrier. A carrier holds up to six programs.

```toml
  [[carrier.program]]
  number = 1
  service = "wall"                       # a [[service]] id
  video = { format = "720p59.94", max_kbps = 12000, min_kbps = 3000 }
  audio = [ { language = "eng", layout = "stereo", kbps = 192 } ]
  live = "studio"                        # an [[input]] id
  standby = "loop"                       # shown while the live input is away
```

The video formats are 1080i and 1080p at 29.97 frames/s, 720p at 59.94 and 480i at 29.97.
Every source is scaled and fitted to the program's format. Active Format Description and bar
data tell the TV which part of the frame is picture.

Each program falls back on its own: live input, then standby input, then a generated slate
with the channel's name. When the live input comes back, the program switches back to it.

A **service** gives a program its identity on the TV:

```toml
[[service]]
id = "wall"
name = "WALL"            # short name, at most 7 characters
long_name = "Live Wall"
channel = "20.1"         # the number the TV shows; unique across the plant
source_id = 1            # 1..=4095; unique and never reused (the guide is keyed on it)
```

**Captions.** `subtitles = "file.srt"` (SubRip or WebVTT) sends the text as CEA-608 and
CEA-708 captions. `captions = true` announces captions that the source itself carries.

## 5. Inputs

Every input decodes in a sandboxed worker process (§5.1). Each input feeds one program at a
time, as its live input or its standby.

### RTMP (push)

```toml
[[input]]
id = "studio"
kind = "rtmp"
bind = "0.0.0.0:1935"
app = "live"
key = "wall"
```

The producer publishes to `rtmp://HOST:1935/live/wall`. In OBS, set the server to
`rtmp://HOST/live` and the stream key to `wall`. With ffmpeg:

```sh
ffmpeg -re -i show.mp4 -c:v libx264 -c:a aac -f flv rtmp://HOST:1935/live/wall
```

Enhanced RTMP is supported: HEVC, AV1 and VP9 video; AAC, Opus, MP3, AC-3 and E-AC-3 audio; and
several audio tracks in one stream (they reach the program's audio tracks in order).

### SRT (push)

```toml
[[input]]
id = "field"
kind = "srt"
bind = "0.0.0.0:9000"
passphrase = "at least ten characters"   # optional; AES-128
latency_ms = 200                         # how long lost packets may take to be resent
```

SRT carries MPEG-TS and recovers lost packets, so it suits Wi-Fi and internet links. Because
it carries MPEG-TS, several audio streams with their languages and captions come through as
they are. The input serves one caller at a time.

```sh
ffmpeg -re -i show.mp4 -map 0 -c:v libx264 -c:a aac -f mpegts \
  "srt://HOST:9000?passphrase=at%20least%20ten%20characters&pbkeylen=16"
```

Use `-map 0` so that ffmpeg sends every audio stream, not only the first.

### MPEG-TS over UDP or RTP (push)

```toml
[[input]]
id = "encoder"
kind = "ts_udp"
bind = "0.0.0.0:5000"
```

The input accepts raw MPEG-TS datagrams and RTP, and reorders RTP. It has no loss recovery.

### HLS and Low-Latency HLS (pull)

```toml
[[input]]
id = "web"
kind = "hls"
url = "https://example.com/live/master.m3u8"
max_kbps = 8000           # optional: the highest variant bit rate to take
```

The URL can be a multivariant playlist or a media playlist. The input takes the variant with
the highest bandwidth (within `max_kbps`), plus every audio rendition of that variant. Each
rendition's `LANGUAGE` labels its audio, so a program's second audio track can take it (§6).
Segments can be MPEG-TS or fragmented MP4 (CMAF).

- **Live HLS:** the input starts three target durations behind the live edge.
- **Low-Latency HLS:** the input uses parts, preload hints and blocking playlist reloads, and
  starts about a second behind the live edge (`PART-HOLD-BACK`).
- **Scheduled VOD:** see below.

#### A VOD playlist as a 24/7 channel

A VOD playlist (with `EXT-X-ENDLIST`) whose segments carry `EXT-X-PROGRAM-DATE-TIME` plays on
its own schedule. This is meant for generated channels, such as "every episode ever aired",
where a program writes the playlist and decides what airs when.

- The input plays the segment whose air time is now. After a restart it rejoins at the right
  point, in the middle of an episode if need be.
- It fetches each segment at most 8 seconds before its air time, not the whole archive.
- It reads the playlist again every minute, so the generator may change or regenerate it. The
  position is kept by air time, not by segment number.
- Past the last segment, it waits and reads the playlist again.
- Separately encoded episodes joined with `EXT-X-DISCONTINUITY` play one after another without
  a break: the input moves every packet onto the air-time clock.

What the generator writes:

```
#EXTM3U
#EXT-X-TARGETDURATION:6
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-PROGRAM-DATE-TIME:2026-11-28T20:00:00Z
#EXT-X-DATERANGE:ID="ep1",CLASS="totalblonder.program",START-DATE="2026-11-28T20:00:00Z",DURATION=1320,X-TITLE="Cowboy Bebop",X-DESCRIPTION="Asteroid Blues",X-RATING="TV-14"
#EXTINF:6,
ep1/0.ts
…
#EXT-X-DISCONTINUITY
#EXT-X-PROGRAM-DATE-TIME:2026-11-28T20:22:00Z
…
#EXT-X-ENDLIST
```

The `EXT-X-DATERANGE` lines become the program guide (§7.2).

### Files

```toml
[[input]]
id = "loop"
kind = "file"
path = "/srv/totalblonder/standby.ts"
```

An MPEG-TS file is played in a loop, paced by its own time stamps. This is useful as a
standby source.

### 5.1 The decoder sandbox

totalblonder does not link libav (ffmpeg's libraries) into the server. Decoding runs in a
separate worker process, `tb-avworker`, inside user, mount, PID, network, IPC and UTS
namespaces. The worker also runs under Landlock, a seccomp filter that kills it on any
unexpected system call, and resource limits. Packets and frames cross through shared memory.
If a worker crashes, it is restarted with back-off and the program falls back meanwhile.

## 6. Audio

A program has one to three audio tracks. They are encoded to AC-3 with loudness normalized to
−24 LKFS and true peaks limited to −2 dBTP.

```toml
audio = [
  { language = "eng", layout = "auto", kbps = 384 },
  { language = "spa", layout = "stereo", kbps = 192 },
]
```

**Layouts.**

| `layout` | What is sent |
|---|---|
| `mono` | 1/0 |
| `stereo` | 2/0 (the default) |
| `5.1` | 3/2 with LFE |
| `auto` | Follows the source: mono stays mono, stereo stays stereo, anything wider becomes 5.1. The slate is stereo. |

With `auto`, `kbps` is the rate for 5.1; stereo uses at most 192 kbit/s and mono at most 96.
With the statmux on (§8), what an `auto` track does not use goes to video.

**Second audio (SAP).** Each track takes the input's audio stream in its own language. If the
input has no stream in that language, the track carries the main audio, as a station without a
second language does. Streams without a language label fill the tracks in order.
`input_track = N` picks the input's *N*-th audio stream instead.

| Input | Where the language comes from |
|---|---|
| MPEG-TS (UDP, RTP, SRT, file, HLS TS segments) | The ISO 639 descriptor of each audio stream |
| HLS | Each audio rendition's `LANGUAGE`, or the fMP4 track's language |
| RTMP | Nothing: tracks are taken in order |

## 7. The program guide

TVs show a guide of up to 16 days. Each program's events come from one of two places. If the
program's input publishes events (an HLS input, §7.2), those win; otherwise the schedule file
is used. Without either, the guide shows the channel name in each three-hour window
(`[psip] placeholders`).

### 7.1 The schedule file

```toml
[psip]
schedule = "/var/lib/totalblonder/schedule.json"
eit_windows = 8          # how far ahead the guide reaches, in 3-hour windows (8 = 24 hours)
```

The server reads the file again every 10 seconds. If the file has an error, the server keeps
the last good version and logs why.

```json
{
  "wall": [
    { "start": "2026-11-26T20:00:00Z", "minutes": 60,
      "title": "Evening News", "description": "Local news and weather", "rating": "TV-PG" }
  ]
}
```

- Keys are service ids. `start` is RFC 3339; `minutes` or `seconds` gives the length.
- `rating` is a US TV or MPAA rating (`TV-G`, `TV-14-DLSV`, `MPAA-PG-13`, …).
- `ratings` rates the event in a rating system of your own (`[[rating_system]]`); see the
  comments in `src/schedule.rs`.
- Events must not overlap, and two events may not start in the same minute.

### 7.2 From an HLS playlist

An HLS input's `EXT-X-DATERANGE` spans become the guide of every program that airs it:

| Attribute | Meaning |
|---|---|
| `CLASS="totalblonder.program"` | Required. Date ranges of any other class are ignored, so a third-party stream's ad breaks or chapter marks never reach the guide. |
| `START-DATE`, and `DURATION` or `END-DATE` | When the program airs |
| `X-TITLE` | The title (required) |
| `X-DESCRIPTION` | A description |
| `X-RATING` | A US rating, as in the schedule file |
| `X-GUIDE="NO"` | Keeps the span out of the guide |

Spans shorter than a minute (bumpers, promos) are left out, and so are spans marked
`X-GUIDE="NO"`. The program before such a gap is stretched to the next one across gaps of up
to five minutes, so the guide has no holes.

### 7.3 Ratings

The PMT and the guide carry each event's rating, so a TV's parental controls work. US ratings
are limited to the combinations CTA-766 permits. `[[rating_system]]` adds a system of your own
(sent as a Rating Region Table). See the comments in the example configuration.

## 8. Bit rate: the statmux

Without the statmux, every program gets its `max_kbps`. With it, each carrier's video bit
rate is shared between its programs by how hard each is to encode at the moment, between each
program's `min_kbps` and `max_kbps`:

```toml
[statmux]
enabled = true
```

A program showing its slate is held at its floor. The sum of the programs' rates never
exceeds the carrier: a program's rate is lowered before another's is raised. `weight` in a
program's `video` table gives it a larger or smaller share.

**Overload.** When the GPU cannot keep up, the lowest-priority program (`priority` in `video`;
lower numbers step down first) steps down to a smaller format (by default 480i), and steps back up after five minutes without
loss. `[overload]` sets the thresholds; `ladder` in a program sets the formats it may use.

## 9. The modulator

### 9.1 Network

The EQAM-420B takes carriers 1–8 on its GIGE1 port and carriers 9–16 on GIGE2. Give each
port an address on a network of its own, and send each carrier from the matching interface:

```toml
[[output.interface]]
name = "gige1"
address = "192.0.2.1"        # this host's address on GIGE1's network
carriers = [1, 2]
multicast_ttl = 1

[[output.interface]]
name = "gige2"
address = "198.51.100.1"     # this host's address on GIGE2's network
carriers = [9]
multicast_ttl = 1
```

Multicast with TTL 1 on a network where the box is the only receiver reaches it whether or not
the switch handles IGMP.

### 9.2 Settings the box needs

Make these changes once, by hand, in the box's web interface:

1. **MPTS mode.** The box passes tables through only in MPTS mode, so channel numbers, names
   and the guide come from totalblonder.
2. **Configuration mode: Manual.** In Automatic mode, the box rebuilds its stream table from a
   flash file at boot, which this firmware does incorrectly (defect B in
   [`notes/corruption-48.md`](notes/corruption-48.md)). Manual mode avoids that.
3. **GigE addresses**, as in §9.1.

### 9.3 The supervisor

The `[device]` section turns on a supervisor that logs in to the box's web interface, reads
its configuration and status, and compares them with the configuration file.

```toml
[device]
base_url = "http://192.0.2.50"          # the box's management address
credentials_file = "/run/credentials/totalblonder/eqam"
reconcile = "report"                    # or "apply"
level_dbmv = 40

  [[device.rf_module]]                  # exactly four, as the box should be set
  module = 1
  first_channel = 2
  …
```

- **The credentials file** holds the Administrator's user name and password on two lines. A
  read-only account (the box's Guest) may follow on lines 3 and 4.
- **`report`** (the default) reads and alarms but never writes. With a read-only account in
  the file, the supervisor polls as that account, so it never logs a person out of the web
  interface.
- **`apply`** also rewrites the stream table and the RF plan when they drift from the file. It
  needs the Administrator account and writes only after it has seen Manual mode. Writes are
  rate-limited, and stop with an alarm if the same drift keeps coming back.
- **Alarms:** unreachable, not in MPTS mode, not in Manual mode, drift, reboot, an input
  absent, an RF module not locked, oversubscription, rising TS sync errors, programs that
  differ from what is sent, event-log lines that matter, and the time since the box's
  tables were last reset (`defect_d_limit_h`, defect D in
  [`notes/corruption-48.md`](notes/corruption-48.md)).

The supervisor can only send a fixed list of requests. It cannot reach the page that changes
network settings, passwords, reboots or deletes, except for one bare read of that page
(`notes/device.md`).

To keep the box's own log, point its syslog at your log store in its admin page. Logging
starts at the box's next boot.

## 10. Monitoring

**Control API** (`[control] bind`, read-only):

| Path | What |
|---|---|
| `/healthz` | `ok` |
| `/api/metrics` | Prometheus text: per carrier, program, input, audio track and the modulator |
| `/api/state` | The same values as JSON |

**Built-in stream checker.** With `[output] self_check = true` (the default), an independent
checker (`tb-ts-verify`) reads every carrier's output as it is sent. Its findings appear in the
metrics and the log. The same checker runs on files:

```sh
tb-ts-verify capture.ts
```

**OpenTelemetry.** `[telemetry]` sends log lines, structured events and every metric to an
OTLP/HTTP endpoint (for example VictoriaLogs and VictoriaMetrics). [`SERVER.md`](SERVER.md)
lists the events.

## 11. NixOS

The flake provides a NixOS module:

```nix
{
  imports = [ totalblonder.nixosModules.default ];

  services.totalblonder = {
    enable = true;
    deviceCredentialsFile = "/run/secrets/eqam";   # passed as a systemd credential
    gpuVendor = "nvidia";                          # or "amd"
    telemetry.logs = "https://logs.example/insert/opentelemetry/v1/logs";
    settings = {
      video.device = "cuda";
      statmux.enabled = true;
      # … the rest of the configuration, as in the TOML file
    };
  };
}
```

`settings` is the configuration file as Nix values; `configFile` uses a file instead. The
module runs the server as a confined systemd service with access to the GPU's device nodes,
and sets `TB_AVWORKER`. `openFirewall` opens the ports of the configured RTMP, SRT and
MPEG-TS inputs; the control API is never opened, as it has no authentication.
