# totalblonder

totalblonder turns network video into digital cable channels. It is a Rust server that feeds a
Blonder Tongue EQAM-420B edge QAM modulator, so that ordinary cable-ready TVs can tune your
streams as numbered channels, with names, a program guide, second-language audio, closed
captions and parental ratings.

It takes live pushes (RTMP, SRT, MPEG-TS), pulls HLS, and can run a generated VOD playlist as
a 24/7 scheduled channel. It encodes MPEG-2 video on the GPU and AC-3 audio, builds one
constant-rate transport stream per QAM carrier with all the tables a TV needs, and supervises
the modulator through its web interface.

The repository also holds the reverse engineering of the EQAM-420B: its firmware, its web
interface and its defects.

## Features

- **Inputs:** RTMP and Enhanced RTMP (H.264, HEVC, AV1, VP9; AAC, Opus, MP3, AC-3, E-AC-3;
  several audio tracks), SRT with optional encryption, MPEG-TS over UDP or RTP, HLS and
  Low-Latency HLS, and looping files. Decoding runs in a sandboxed child process.
- **Scheduled channels from HLS:** a VOD playlist with `EXT-X-PROGRAM-DATE-TIME` plays on its
  own schedule, rejoins at the right point after a restart, and its `EXT-X-DATERANGE`s become
  the program guide.
- **Fallback:** each channel has a live input, an optional standby input and a generated
  slate, and switches between them on its own.
- **Video:** MPEG-2 encoded on an NVIDIA (CUDA) or AMD (HIP) GPU, 480i to 1080i/1080p, with
  Active Format Description and bar data, film mode for 24p sources, and captions from
  SubRip or WebVTT files.
- **Audio:** AC-3 in mono, stereo, 5.1 or following the source, normalized to −24 LKFS, up
  to three tracks per channel picked from the input by language (SAP).
- **Bit rate:** a statistical multiplexer shares each carrier between its channels by how hard
  each is to encode, audio included, and never overfills the carrier.
- **Tables:** channel numbers and names, the clock, a guide of up to 16 days from a schedule
  file or from HLS, and US or custom content ratings.
- **The modulator:** a supervisor that reads the box's configuration and status, alarms on
  drift and faults, and can rewrite its stream table and RF plan, through a request screen
  that cannot reach the dangerous pages.
- **Checked output:** an independent transport stream checker in the repository reads every
  carrier as it is sent.
- **Operations:** Prometheus metrics, OpenTelemetry logs and metrics, and a NixOS module.

## Status

totalblonder runs in production on one plant: two carriers into an EQAM-420B. Every input type
has been run end to end on that plant's GPU host. Not yet checked: the picture on a range of
real TVs, and the RF output with a tuner. [`docs/STATUS.md`](docs/STATUS.md) has the details.

## Requirements

- An NVIDIA GPU with CUDA, or an AMD GPU with HIP, for the encoder.
- Linux with user namespaces (for the decoder sandbox).
- Nix with flakes, to build.
- An EQAM-420B, or another IP-to-QAM modulator that passes an MPTS through (untested).

## Quick start

```sh
nix build .#totalblonder .#tb-avworker
cp crates/totalblonder/totalblonder.example.toml my.toml   # every key, with its default
totalblonder --config my.toml --check                      # validate
TB_AVWORKER=$PWD/result-1/bin/tb-avworker totalblonder --config my.toml
```

On NixOS, use the flake's module (`services.totalblonder`). [`docs/GUIDE.md`](docs/GUIDE.md)
explains every part of the configuration, with examples.

To build and test everything that needs no GPU:

```sh
nix flake check
```

## Documentation

| Document | For |
|---|---|
| [`docs/GUIDE.md`](docs/GUIDE.md) | Setting up and running the server: inputs, channels, audio, guide, modulator, monitoring, NixOS |
| [`docs/STATUS.md`](docs/STATUS.md) | What is built, how it was checked, and what is still missing |
| [`docs/EQAM-420B.md`](docs/EQAM-420B.md) | The modulator in short: what it is, what it accepts, its defects, how not to break it |
| [`docs/SERVER.md`](docs/SERVER.md) | How the server is put together: data flow, threads, clocks, the libav sandbox, telemetry |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | The Rust workspace: crates, the sans-io rule, the GPU device protocol, the Nix build, how the encoder is checked |
| [`docs/SPEC.md`](docs/SPEC.md) | The design specification and its decisions |
| [`docs/notes/`](docs/notes/) | Reference notes: the modulator's firmware and defects, the standards, conformance rules, the software survey, the first plant's log |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Working on the code |

## Repository layout

| Path | Content |
|---|---|
| `crates/totalblonder/` | The server |
| `crates/tb-*/` | The libraries: encoder, mux, tables, audio, captions, inputs, the decoder sandbox, the modulator client, the stream checkers |
| `crates/tb-kernel-abi/kernels/` | The GPU kernels |
| `nix/`, `flake.nix` | The build (with [breakbulk](https://github.com/schlarpc/breakbulk)), the NixOS module |
| `tools/gate/` | The GPU test gate and benchmarks |
| `tools/` | Firmware extractors, a Nios II module for Ghidra, device scripts |
| `docs/external/` | Not in the repository: vendor manuals, firmware and standards; how to fetch them |

## License

MIT, see [`LICENSE`](LICENSE). Vendor firmware, manuals and standards are not included;
[`docs/external/README.md`](docs/external/README.md) says where they come from.
