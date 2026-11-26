# totalblonder

totalblonder turns network video into digital cable channels. It is a Rust server that feeds a
Blonder Tongue EQAM-420B edge QAM modulator, so that an ordinary cable-ready TV can tune the
streams as numbered channels with names and a program guide. The repository also holds the
reverse engineering of that modulator: its firmware, its web interface and its defects.

## What it does

- **Takes live inputs**: RTMP pushes (tested with ffmpeg, GStreamer and librtmp publishers), MPEG-TS over UDP or
  RTP, and MPEG-TS files played in a loop. Each channel can have a live input, a standby
  input and a generated slate, and switches between them on its own when an input fails.
- **Encodes for cable TVs**: MPEG-2 video on an NVIDIA GPU (480i up to 1080i/1080p), AC-3
  audio with loudness control, and closed captions from SubRip or WebVTT files.
- **Builds the transport streams**: one constant-rate multi-program stream per QAM carrier,
  with exact clock references and the cable tables a TV needs for channel numbers, names, the
  clock, the guide and content ratings. An independent checker in this repository verifies
  every output packet while the server runs.
- **Supervises the modulator**: reads its configuration and status through its web forms,
  reports drift and, when allowed, writes the stream table and RF plan.

The whole plant is set up in one TOML file
(`crates/totalblonder/totalblonder.example.toml` lists every key). The guide comes from a JSON
file that the server reads again every 10 seconds.

## Status

As of 2026-11-26, the server runs in real time and its output passes the repository's own
checker and TSDuck, but no stream has reached the modulator yet. `docs/STATUS.md` says what
is built and what is still missing; `docs/FIRST-AIR.md` is the plan for the first broadcast.

## Building and running

The build uses Nix with flakes. The video encoder needs an NVIDIA GPU with CUDA; everything
else, the tests included, also runs without one.

```sh
nix flake check                                   # build and test everything that needs no GPU
nix develop                                       # a shell with the Rust toolchain and tools
nix build .#totalblonder                          # the server
totalblonder --config FILE --check                # validate a configuration
totalblonder --config FILE                        # run
totalblonder --config FILE --offline 10 --out DIR # 10 s of output into files, no real time
```

`docs/SERVER.md` explains how the server is built and run; `nix/module.nix` is a NixOS module
for it (`services.totalblonder`).

## License

MIT, see `LICENSE`. Vendor firmware, manuals and standards documents are not included:
`docs/external/README.md` says where they come from.

## Where things are

| Path | Content |
|---|---|
| `docs/STATUS.md` | What is built and what is still needed, with the order of work |
| `docs/SPEC.md` | Design specification for the streaming server: output contract, clocking, mux, tables, encode, ingest, device control, monitoring, milestones, open questions |
| `docs/ARCHITECTURE.md` | The Rust workspace: crate map, sans-io rule, device protocol, kernel ABI, Nix build, how the encoder is checked, history |
| `crates/`, `nix/`, `flake.nix` | The workspace (built by `github:schlarpc/breakbulk`): `nix flake check`, `nix develop`, `nix run .#gpu-plumbing` on nixark |
| `kernels/` | The CUDA kernels (one copy, built by Nix into the device image and for the host) |
| `gate/` | The gate and the benchmarks: decoders, buffer models, conformance verifier, capacity, line-ups, contention, corpus recipes (`gate/README.md`) |
| `golden/` | Golden vectors: the encoder's decisions and device operations for ten small cases, written by the Rust recorder, and how to regenerate them |
| `scripts/` | `nixark.sh` (run a command of the tree on the GPU host), `capped.sh` (the memory cap) |
| `docs/port/` | Completed history: the work packages of the port of the CUDA MPEG-2 encoder's host side to Rust and of the C++'s retirement (X1) |
| `spikes/cuda-mpeg2/results/` | Frozen history: the measurements of the C++ spike and of the port while it was compared with it |
| `notes/post-port-worklist.md` | The encoder's work queue now that the C++ is retired |
| `notes/cuda-mpeg2-spike.md`, `notes/cuda-mpeg2-optlog.md` | History: the C++ spike's measurements and optimization log |
| `notes/m2v-conformance-findings.md`, `notes/m2v-conformance-rules.md` | What the conformance verifier finds in our streams; its rule catalog |
| `notes/breakbulk-feedback.md` | What was missing in breakbulk when the workspace was set up |
| `notes/firmware.md` | Flash layout, CPU/OS, rootfs inventory, config storage, Ghidra setup |
| `notes/device.md` | Live device surface: web forms, FTP/telnet, current state, dangerous URLs |
| `notes/manual.md` | Digest of the 420B manual with page references |
| `notes/research.md` | Vendor changelogs, sister products, standards, documents obtained and missing |
| `notes/producers.md` | What the first producer emits, its demand model, conventions to reuse |
| `notes/design-inputs.md` | Decisions, owner-supplied facts, feature candidates, research still to do |
| `notes/standards-digest.md` | Implementer's digest of MPEG-TS, PSIP, SCTE cable, captions, AC-3, EAS, conformance tests |
| `notes/corruption-48.md` | The ">48 channels corrupts config" investigation |
| `notes/software-survey.md` | Reuse-versus-write survey: TSDuck, libav, crates, statmux, hardware encode, spike commands |
| `docs/external/` | Gitignored: vendor manuals, firmware images, spec sheets and standards (fetch lists and checksums for the standards are tracked) |
| `tools/` | Firmware extractors, Nios II Ghidra module, device login/crawl/mirror scripts |
| `work/` | Gitignored: extracted firmware, decompilation, device captures, flash dump |

## What the box is

- An IP-to-QAM modulator, not an encoder. It takes MPEG-2 transport streams over UDP or RTP
  on two GigE ports and modulates them onto 16 QAM carriers (4 RF modules × 4 adjacent
  carriers). It does not transcode.
- An Altera FPGA design with a Nios II soft CPU running uClinux 2.6.26. The stream path is in
  FPGA; Linux only does configuration, IGMP joins and the web UI.
- Software 1.1.13, the latest and only published build. The device's files are byte-identical
  to `docs/external/eqam-420b.zip`.
- Management is Boa CGI forms, plus root FTP and telnet on one shared credential. Firmware
  update has no signature or checksum, so patched firmware is mechanically possible.

### Input contract

| | SPTS mode | MPTS mode |
|---|---|---|
| Inputs | 64 single-program streams, 4 per carrier | 16 multi-program streams, 1 per carrier |
| Programs | 1 video + at most 1 audio each | At most 6 per carrier, 1 video + at most 3 audio each |
| PAT/PMT, PIDs | Regenerated and remapped by the box | Passed through |
| PSIP (channel numbers, names) | Generated by the box from its web UI | Passed through; we generate it |
| EPG | Not supported | Passes through; we generate it (reported working on another unit, not yet tested on ours) |
| SAP / second audio | Not possible | Possible |

Common to both: at most 38.8 Mbps per carrier (256-QAM Annex B), MPEG-2 or H.264 video, AC-3
audio, multicast with IGMP v2/v3 or unicast. The box inserts and deletes null packets and
corrects PCR, but does no rate control: overflow pixelates.

Not documented and still to measure: TS packets per datagram, jitter tolerance, FEC, unicast
behavior.

## Trajectory

The target is a Rust server, `totalblonder`, that owns everything above the modulator. The
box runs in MPTS mode and only modulates what arrives. The server re-encodes network inputs
(first an RTMP-push producer), builds one constant-bitrate MPTS per carrier with its own tables,
divides each carrier's bit rate between its programs, keeps every carrier on air when inputs
fail, supervises the modulator's configuration and checks the RF output with a tuner.

The design, the milestones (starting with a hardware spike, M0) and the open questions are
in `docs/SPEC.md`.

## Safety rules for the device

- Never change network or login settings. Never submit `admin.cgi`.
- These GET URLs are destructive: `admin.cgi` with `btnReboot`, `btnFlush` or
  `btnDeleteConfig` in the query. `tools/eqam_get.sh` refuses any URL containing `btn`.
- Recovery path: hold the front-panel IP RESET button about 10 s, power-cycle, then reach the
  box at 172.16.70.1 on the 10/100 control port with the vendor default logins.
- A wrong or ASCII-mode firmware upload can brick the unit.
- Safety copies: `work/device/flash/mtd0.bin` (2 MB config flash),
  `work/device/eeprom.bin` (8 KB EEPROM) and `work/device/config_file.dat` (web export).

## Open questions

- Which of the three confirmed config/memory defects is the reported ">48" corruption
  (`notes/corruption-48.md`): the `mux_sync` table overrun, the `qamconfig.cgi` shared
  offset, or the `update_config` boot reconcile. None is dimensioned 48.
- Who wrote the table at EEPROM 0xF10 during the 2026-09-07 setup (`notes/corruption-48.md`
  section 8).
- Feed tolerances listed under "Input contract".
- EAS trigger packet format on UDP 5050.
- Which TVs on the plant decode H.264 over QAM; MPEG-2 is the safe default.
