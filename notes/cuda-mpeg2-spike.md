# CUDA MPEG-2 encoder spike

> **History.** This document records the C++ spike and its measurements as they were; it is
> not kept up to date. The spike's programs were deleted in package X1 (2026-11-25,
> `docs/port/X1-notes.md`) and are in the git history at commit `e79069b`; its result files
> are frozen in `spikes/cuda-mpeg2/results/`. The living documentation: `docs/ARCHITECTURE.md`
> (the Rust encoder and how it is checked), `gate/README.md` (the gate and benchmark tools),
> `golden/README.md` (the golden vectors), `notes/post-port-worklist.md` (what is next).

Work on 2026-11-25 on `nixark` (VM, 8 vCPUs of an EPYC 9124, 31 GB, NVIDIA RTX 2000 Ada 16 GB,
driver 595.71.05, PCIe 4.0 x8). Four rounds so far:

- **First spike** (morning): can the GPU encode MPEG-2 in real time, how many channels, where
  is the bottleneck. Its results that still hold are in §13.
- **Phase A** (this document's main subject): measuring instruments (correctness gate,
  benchmark, hardware-counter profile), the missing functions (strict buffer model, rate
  control, interlace tools, legal rate change, captions, key frames on demand, a per-session
  interface), and a new baseline with those functions on.
- **Phase B** (§0): speed. The encoder now carries 2.3 to 3 times the sessions of phase A on
  the same card, with slightly better pictures at the nominal rates. `notes/cuda-mpeg2-optlog.md` is the log of
  its experiments.

- **Work package C** (§C): the path in front of the encoder. Inputs are decoded on the card
  (NVDEC through libavcodec, in the encoder's context), converted, scaled and composed by CUDA
  kernels straight into the encoder's input planes; sessions open and close at run time; a tick
  that would be late repeats pictures instead. The decoder, not the encoder, is now what limits
  the box: 27 channels of 1080p30 H.264 → 1080i, 48 channels of the mixed line-up.

- **The Rust port** (§R, written at the end of port packages K2 and G2): the host side of the
  encoder and of the pipeline exists in Rust (`crates/`), passes the same gates, and its
  coded streams are byte-identical to this C++'s except for one traced difference. §R has
  the parity and capacity measurements; `docs/ARCHITECTURE.md` describes the Rust workspace.

Whoever continues should start from §C (the pipeline and what limits it), §0 (the encoder:
state, profile, remaining targets) and §2 (how to work). `notes/cuda-mpeg2-optimization-ideas.md` is the desk research that preceded phase B;
§0.2 and §0.8 say which of its ideas were built and which lost.

Source, scripts and result files are in `spikes/cuda-mpeg2/`. Section numbers of the form
§7.6.3 refer to ITU-T H.262 (02/2000), `docs/external/standards/2026-11-24-itu-atsc-2/`.

Labels: **[M]** measured here; **[E]** estimated from measurements; **[K]** general knowledge,
not tested.

## R. The Rust port against this C++ (2026-11-25, port packages K2 and G2)

Work of 2026-11-25 after 15:26 PST on the same host and card. The card was **never free
during this work**: one to three Plex transcodes ran throughout (0 to 24 % SM in bursts, the
decoder up to 81 % and the encoder up to 86 % for minutes at a time). Every timing below was
taken with the two binaries interleaved run by run, `nvidia-smi pmon` sampled during each run,
and says how many of its runs were disturbed; read differences under about 2 % as noise.
All [M] unless marked. Binaries: `tb-m2venc` (the encoder's driver) and `tb-pipe` (the
pipeline harness), built by Nix from the Rust workspace; "oracle" is this C++ (`m2venc`,
`m2vpipe`) built from the same tree.

### R.1 Correctness

| Gate | Rust binaries alone | Streams against the oracle's |
|---|---|---|
| `check` (15 cases) / `check-full` (54) | 15 / 15, 54 / 54 (G1) | 23 of 23, 74 of 74 byte-identical |
| `check-pipe` (46 cases: device input, forced repeats, holds, run-time sessions, 9 geometry cases against analytic patterns, 26 conversion cases) | **46 / 46** (`results/check/rust-pipe.txt`) | 228 streams, **212 byte-identical**, 16 different |
| Kernels after the conversion kernels moved into the device image (K2) | oracle `check-pipe` 46 / 46 before and after | 228 of 228 identical between the oracle before and after |

The 16 streams are one difference, traced (`scripts/churn_trace.py`,
`results/check/rust-churn-trace.txt`): in the two `--churn …:reserve` runs this C++ opens a
run-time session when its side thread has made the batch, one to three pictures later than
`--churn …:naive` does, and the Rust port, which has no side thread (batches are laid out in
the arena at once, §C.6's "arena" way), opens like `naive`. The oracle's `reserve` run
differs from its own `naive` run on exactly these 16 streams (each 1 to 3 pictures shorter,
identical up to the last one or two pictures, where the flush of the close falls on another
frame); the Rust encoder equals the oracle's `naive` run on 66 of 66 streams. The oracle's
`reserve` runs repeated identically here (the side thread was ready in the same ticks twice).

The conversion path (`tb-pipe`): for the first input tried (`h264_1080p30 → 1080i`) the coded
stream, the converted frames (`--dump-conv`) and the pts log were byte-identical to
`m2vpipe`'s at the first run; the gate then showed the same for all 26 conversion cases
(same-size copies, Lanczos up and down, the half stage, field → field, bob, 3:2 from 24p,
weaving from 50/60p with the low-pass, 10-bit sources, BT.601 ↔ 709, letterbox and crop,
mosaic with overlay, the slate; H.264, HEVC, MPEG-2, VP9, AV1 through NVDEC in the encoder's
context). Decode is libavcodec with the `cuda` hwaccel wrapping the adapter's context, through
the `ffmpeg-the-third` crate (§C.2's [E] confirmed: one hand-declared struct, checked against
the header at build time).

### R.1a The upload fence next to competitors, with the Rust driver

`scripts/contend.sh` as in §0.7 (experiment 41), 300 × 480i paced, 10 s per competitor, no
Plex activity in the windows (`results/contend/g2-rust-480i300`, `g2-oracle-480i300`):

| Next to | Rust: tick median alone → with, ratio, late ticks, isolated slow ticks | Oracle |
|---|---|---|
| back-to-back trivial kernels of another process (`cuda_tinycont`) | 21.92 → 24.35 ms, **1.11**, 0 %, 0 | 22.41 → 25.14 ms, 1.12, 0 %, 0 |
| Plex's pipeline unthrottled (`scale_max`, 677 fps) | 22.02 → 27.60 ms, **1.25**, 0 %, 0 | 22.57 → 28.08 ms, 1.24, 0 %, 0 |
| Plex's pipeline in real time (`scale_re`) | 22.10 → 22.45 ms, 1.02, 0 %, 0 | 22.58 → 22.71 ms, 1.01, 0 %, 0 |

The fixed behavior of experiment 41 (1.14 and 1.23 there; 1.6 to 1.8 and 41 to 43 isolated
slow ticks before the fence) is what the Rust driver shows: the fence is the same two plan
steps (`Record(PARAMS)` on the kernel stream, `WaitFor(PARAMS)` at the head of the upload).

### R.2 Capacity of the encoder alone (frames from host memory, `--stagger --overlap`)

`scripts/bench_capacity.py` (app `gate-bench`; `gate-bench-compare` adds the oracle): the
binaries run interleaved, `nvidia-smi pmon` is looked at before every run and sampled during
it (use, not presence), a disturbed run is discarded and repeated right after the other
process's burst, and what is still disturbed after the retries is kept and marked. Free runs
are 5 s of video at the reference line-ups of §2, paced runs 20 s.

**Paced, at the capacity points of §0.1 and two steps above** (`results/bench/g2-paced-1.json`;
on time = a tick's output exists before the next is due; tick mean / p99 in ms; 22 of the
run attempts were discarded and repeated):

| Line-up | Oracle: on time, tick | Rust: on time, tick |
|---|---|---|
| 66 × 1080i | 100 %, 27.94 / 29.86 | 100 %, 28.02 / 29.74 |
| 67 × 1080i | 100 %, 27.85 / 29.82; again 100 %, 27.74 / 28.61 | (first run disturbed: 51 %); again 100 %, 28.02 / 28.83 |
| 68 × 1080i | 100 %, 28.74 / 30.78 | 100 %, 28.85 / 30.67 |
| 69 × 1080i | 99.82 %, 29.46 / 31.41 | 100 %, 29.57 / 31.45 |
| 75 × 1080p | 100 %, 28.75 / 30.48 | 100 %, 28.83 / 30.93 |
| 76 × 1080p | 100 %, 28.88 / 29.63 | 100 %, 28.94 / 29.72 |
| 77 × 1080p | 100 %, 29.42 / 30.19 | 100 %, 29.48 / 30.36 |
| 90 × 720p | 100 %, 15.23 / 15.65 | 100 %, 15.27 / 15.87 |
| 91 × 720p | 100 %, 15.52 / 15.87 | 100 %, 15.51 / 15.88 |
| 92 × 720p | 100 %, 15.67 / 16.03 | 99.82 %, 15.72 / 16.23 |
| 400 × 480i | 100 %, 30.60 / 31.27 | 100 %, 30.68 / 31.88 |
| 404 × 480i | 100 %, 31.35 / 32.48 | 100 %, 31.19 / 32.29 |
| 408 × 480i | 100 %, 31.26 / 31.93 | 100 %, 31.34 / 32.23 |
| 24 mixed units | 100 %, 12.61 / 13.45 | 100 %, 12.62 / 13.40 |
| 25 mixed units | 99.91 %, 13.18 / 14.81 | 100 %, 13.19 / 14.77 |
| 26 mixed units | 99.91 %, 13.74 / 14.79 | 99.65 %, 13.74 / 14.77 |

Both binaries hold every capacity point of §0.1 and the two counts above it with at least
99 % of ticks on time (99.9 % except where the table says otherwise); the search was not
taken further up. Mean ticks differ by −0.5 to +0.4 %; host CPU at these loads is 0.16 to
0.21 core for both (Rust 0.162 to 0.211, oracle 0.159 to 0.212). The card ran at 83 to 91 °C
and 62 to 68 W in these runs, as in §0.3.

**Free-running**, final binaries, a window without any other process on the card (series
`g2-free-8`: 4 rounds, 80 runs, none disturbed; million macroblocks per second, mean and
spread = (max − min) / mean; ratio by round = of neighboring runs, median and range):

| Line-up | Oracle | Rust | Rust / oracle by round | Host CPU, cores: oracle / Rust |
|---|---|---|---|---|
| 16 × 1080i | 19.35 (0.97 %) | 19.47 (0.46 %) | **1.005** (1.004 to 1.009) | 0.225 / 0.228 |
| 16 × 1080p | 21.29 (0.61 %) | 21.42 (0.81 %) | **1.006** (1.004 to 1.008) | 0.231 / 0.239 |
| 16 × 720p | 21.99 (0.24 %) | 21.93 (2.52 %) | **0.999** (0.983 to 1.009) | 0.269 / 0.283 |
| 96 × 480i | 17.32 (0.34 %) | 17.35 (2.17 %) | **1.004** (0.989 to 1.010) | 0.254 / 0.260 |
| mixed, 8 units | 20.21 (1.13 %) | 20.10 (1.33 %) | **0.995** (0.986 to 1.003) | 0.279 / 0.300 |

Within the run-to-run spread in every format. The series before the last two fixes (same
method; the earlier ones next to Plex transcodes, `g2-free-7` clean) had the Rust driver at
0.983 to 0.997 of the oracle by round, consistently about 1 % behind.

**What was found on the host side, and what each fix did** (details and the series in
`docs/port/G2-notes.md`):

- *The tick's wait for the upload stream* (the plan without steps at the start of a tick
  with the overlapped upload; this C++ calls `cudaStreamSynchronize`) was the 1 %. Two
  things in the CUDA adapter, found one after the other by running the same binary with the
  wait replaced by an event dependency (`--frames-event`: level with or ahead of the oracle
  in every series, so the wait was the place): (1) the port recorded an event on the
  upload stream and polled it, which costs a round trip to the device even when the stream
  is idle; now a plan without steps asks the stream itself (`cuStreamQuery`). That alone
  changed nothing measurable (clean series `g2-free-7`: 0.988 to 0.997 by round). (2) The
  hybrid wait slept 85 % of the average of earlier waits before polling; that suits a phase
  of stable length, not what happens to be left of the frame upload when the next tick
  starts, and the nap overshot. Without the nap for stream waits: 0.995 to 1.006 (table
  above; the previous binary in the same rounds: 0.985 to 0.997).
- *One plan per phase for all formats*: with several batches the device idled while the host
  did the rate control of the later batches; this C++ queues batch by batch. Fixed
  (`split_plans`): on the mixed line-up 0.05 ms less waiting per tick in the quietest runs
  (3.62 against 3.67 ms; 0.15 ms by the medians of four disturbed rounds), 0.5 to 3 % in
  throughput by round against the same binary with one plan per phase.
- *Launch arguments copied as a list of full-size values* (about 8 KB per launch; `memmove`
  6.4 % of the driver's CPU samples against 3.9 % in this C++): now under 1 KB. No change in
  throughput beyond noise (0.991 against 0.983 by round on 16 × 1080i, 0.987 against 0.994
  on 720p).
- *Access-unit bytes pooled* (`Encoder::recycle`): no measurable effect (host time between
  phases 0.155 against 0.151 ms on the mixed line-up).
- *An event dependency instead of the host's wait for the upload stream* (`--frames-event`):
  built and measured in every series; after the adapter fixes it has nothing left to save
  (`g2-free-8`: 0.997 to 1.010 by round against 0.995 to 1.006 for the default), and it stays
  off so that the operation sequence is the one this C++ records.
- Host time of the Rust driver per tick (medians of the series): 16 × 1080i 0.13 ms between
  phases and 0.20 ms queueing; 16 × 720p 0.07 and 0.11; 96 × 480i 0.32 and 0.28; mixed 8
  units 0.16 and 0.29 (budget of `docs/ARCHITECTURE.md` §8.1: 0.15 ms and 0.35 ms between
  phases at 16 × 1080i and 96 × 480i). With pictures of 176 × 144 (no GPU work to speak of)
  a tick of 16 sessions takes 0.28 to 0.30 ms with either binary.

### R.3 The pipeline end to end with the Rust harness

`scripts/pipe_lineup.py` with `M2VPIPE` = `tb-pipe`, the line-ups and criteria of §C.4 (paced,
22 s, the first 2.5 s left out; holds = at least 99 % of ticks on time and at most 1 % of
decoded frames later than the lead), each at the count §C.4 found, `tb-pipe` and `m2vpipe`
one after the other with no other process using the card (`results/pipe/g2-rust.jsonl`,
`g2-oracle.jsonl`):

| Line-up at §C.4's count | `tb-pipe`: ticks on time; tick mean / p99; decoded frames late; host CPU (tick thread / decode threads); NVDEC, SM, W | `m2vpipe` in the same minutes |
|---|---|---|
| (a) 27 × 1080p30 H.264 → 1080i | 100 %; 6.10 / 7.20 ms; 0.00 %; 0.35 core (0.08 / 0.26); 95 %, 36 %, 51 W | 100 %; 6.07 / 7.19 ms; 0.01 %; 0.36 (0.08 / 0.27); 96 %, 37 %, 52 W |
| (b) 27 × 720p60 → 720p | 100 %; 5.11 / 5.91 ms; 0.05 %; 0.40 (0.08 / 0.31); 96 %, 30 %, 47 W | 100 %; 5.08 / 5.77 ms; 0.22 %; 0.39 (0.07 / 0.30); 97 %, 31 %, 47 W |
| (c) 8 mixed units (48 channels, nine kinds of input) | 100 %; 4.85 / 6.03 ms; 0.39 %; 0.49 (0.08 / 0.39); 89 %, 27 %, 45 W | 100 %; 4.81 / 5.92 ms; 0.14 %; 0.48 (0.08 / 0.38); 92 %, 29 %, 45 W |
| (d) 24 × 2160p / 1080p sources → 480i | 100 %; 3.37 / 5.31 ms; 0.67 %; 0.39 (0.06 / 0.31); 93 %, 21 %, 43 W | 100 %; 3.33 / 4.94 ms; 0.36 %; 0.37 (0.06 / 0.30); 96 %, 22 %, 43 W |
| (e) 4 mosaic channels of 3 × 3 with overlay (36 inputs) | 100 %; 1.92 / 2.86 ms; 0.00 %; 0.36 (0.05 / 0.30); 74 %, 11 %, 35 W | 100 %; 1.90 / 2.77 ms; 0.00 %; 0.37 (0.05 / 0.31); 76 %, 12 %, 34 W |

(Second of two passes; the first, a build earlier, gave the same picture: ticks 6.06 / 4.94 /
5.11 / 3.34 / 1.97 ms against 6.06 / 4.89 / 5.04 / 3.34 / 1.89, late frames at most 0.86 %.
Frame-clock counts differ between any two runs by what the decoder's momentary lateness makes
them: e.g. (a) 33 repeats and 45 drops against 54 and 70 of 15,768 output frames.)

All five hold, as they do with `m2vpipe`. The bottleneck is the same: the card's decoder at
90 to 96 % in (a) to (d) (78 % in (e), whose limit is at 5 channels); the encoder's tick is at
12 to 36 % of its period, the SMs at 11 to 35 %. Host CPU per channel on the device path:
0.012 to 0.015 core (tick thread 0.05 to 0.09 core in total, the rest the decode threads);
(e) 0.10 core per channel of nine inputs, 0.011 per input.

Sustained, with the deadline policy and `--resync 600`:

| Run | Result |
|---|---|
| `tb-pipe`, 24 × 1080p30 → 1080i, 120 s | every tick on time (5.39 / 6.49 ms), no late frame (worst 167 ms after its time), no repeat, no drop, no slate; NVDEC 82 %, SM 32 %, 47 W, 78 °C, 0.23 core |
| `tb-pipe`, 7 mixed units (42 channels), 150 s | every tick on time (4.30 / 5.38 ms), 1.88 % of decoded frames late (the moments when all 42 files loop at once, as in §C.4), 6 repeats and 6 drops of about 215,000 output frames, no slate; NVDEC 79 %, SM 24 %, 42 W, 78 °C, 0.34 core |
| `tb-m2venc` alone, 66 × 1080i from host memory, 120 s | every tick on time (27.62 / 28.60 ms), 72 → 90 °C, 65 W, 0.157 core |
| `tb-m2venc` alone, 24 mixed units, 120 s | 99.92 % of ticks on time (12.65 / 13.55 ms), 80 → 91 °C, 66 W, 0.172 core |

**A fault of this C++ found by these runs** (and of the port's first version, which copied
it): with `--resync` and looping files an input can stay late for good. `m2vpipe` today, the
same two runs: 24 × 1080p30: 20.6 % of decoded frames 2.17 s late, 17,605 output frames
(20.8 %) on the slate; 7 mixed units: 47.6 % late (2.22 s), 83,987 on the slate. Mechanism:
a decode thread that is more than `--resync` behind when its file ends skips to the next key
frame across the restart; `dec.h` takes the first frame the decoder RETURNS in a pass as
frame 0 of the pass (`pts0`), so the pass, and every later one, is timed early by the skipped
part, the frames then come "2 s late" for the frame clock and the channel shows the slate.
The Rust harness times a pass from its first packet instead
(`tb_timeline::ingest::FrameTimes::packet_pts`); ordinary passes get the same times. (The
sustained runs of §C.4 did not show it; whether a thread is 600 ms behind at the moment its
file ends is a matter of load at that instant.)

Features, each a 10 to 21 s paced run with `tb-pipe` [M]:

| What | Result |
|---|---|
| Inputs that appear while 20 channels run (a 2160p HEVC at 4 s, a 720p60 H.264 at 6 s; decoder opened and tap tables made on a side thread) | decoder ready after 1.5 and 14.1 ms; longest tick in the 1.5 s after 7.62 and 7.22 ms (p99 of the run 7.40 ms); every tick on time |
| An input that stalls for 4 s among 12 channels (`--stall`) | its channel: 29 repeated frames, then the slate, then the input again; every tick on time (§C.7's figure) |
| Admission control, 30 × 1080p30 asked at 90 % of the decoder (`--nvdec-share 0.9`) | 27 inputs admitted, 3 refused (their channels show the slate); every tick on time, no decoded frame late (without it §C.4: 2.7 % late at 28) |
| Admission control, 9 mixed units at 75 % with 2 host cores for software decode | 6 SD inputs decoded in software (host path), 5 inputs refused; every tick on time, no frame late; host 1.03 cores |
| Software decode path, 4 × 1080p30 (`@sw`) | 0.253 core per channel (0.229 in §C.4: the port copies the scaled frame once more on the host), 361 MB/s uploaded |
| 26 × 1080p30 next to two 2160p transcodes and a tone-mapped one in real time, 10 s of a 21 s run (`contend.sh pipe:a 26`), deadline policy only | `tb-pipe`: 25.2 % of output frames repeated their input, 40.8 % of decoded frames late, worst 631 ms; `m2vpipe` in the same scenario: 22.8 %, 39.2 %, 598 ms |
| the same with `--degrade 250 --resync 600` | `tb-pipe`: 1.2 % repeated, 26.9 % slate (resynchronizations meeting the inputs' 2 s GOPs, §C.5), 34 suspensions, 7 resynchronizations; `m2vpipe`: 1.9 %, 19.2 %, 38, 5; both worst frame 2297 ms |

The overload behavior is the C++'s (the scenario is not repeatable frame for frame, so the
two columns agree only in kind and size). Not measured again: the hybrid line-ups of §C.4,
decoders in a context of their own (`--dec-ctx own` exists).

What the Rust harness does that this C++ did not (§C.9): the arena is used throughout (the
adapter owns one device and one pinned host block; sessions, slates, overlay planes, tap
tables, intermediate planes and half-size frames are regions of it); the tap tables of the
conversions the opened inputs will need are made before the first tick (0 classes made
inside a tick in every line-up above; 18.7 ms of stalls at start in §C.9) and those of a late
input on its side thread; admission control for the decoder (a static budget from §C.2's
table: hardware, software for inputs that are cheap on the CPU, or refused); captions are
carried from the decoder's side data through frame selection to the encoder (every decoded
frame's `cc_data`, also of frames the frame clock drops; re-packed for the output rate;
tested with synthetic 608/708 payloads, not yet by a gate case with a captioned input).

## C. Work package C: input on the device, run-time sessions, deadline policy (the current state)

Work of 2026-11-25, 10:03 to 12:45 PST, same host and card. The record of the experiments,
with the mistakes, is `notes/cuda-mpeg2-optlog.md`, experiments 42 to 55. This section is the
state at the end; where §0 and later sections differ from it, this section is current. The
encoder's own speed did not change in this package (streams are byte-identical to the binary of
experiment 41 on 74 streams).

What exists now, in `spikes/cuda-mpeg2/`:

| Piece | File | What it does |
|---|---|---|
| Inputs | `src/dec.h` | libavcodec with the `cuda` hwaccel (or `*_cuvid`), one thread per input, frames as device pointers in **the encoder's CUDA context** |
| Conversion | `src/pipe.h` | One separable polyphase resampler in two kernels plus copy, fill, color matrix, overlay; writes into the encoder's input planes |
| Encoder | `src/enc.h` | `submit_dev` / `submit_again`, sessions opened and closed at run time, batches from an arena, repeats made on the host, `step(deadline)` |
| Harness | `src/m2vpipe.cu` | Looping paced inputs → frame clock per channel → conversion → sessions; slate, mosaic, inputs that appear or stall |
| Gate | `scripts/pipe_check.py` | 46 cases (`make check-pipe`), §C.8 |

### C.1 The answers in short

- **What the real pipeline holds on this box** [M]: 27 channels of 1080p30 H.264 → 1080i MPEG-2,
  29 with a mix of 1080-line sources, 27 of 720p60 → 720p, 8 mixed units (48 channels: 8 ×
  1080i + 8 × 720p + 32 × 480i from nine kinds of input), 24 channels of 2160p/1080p sources
  scaled down to 480i, 4 mosaic channels of 3 × 3 inputs. Every one of these is limited by
  **the card's single NVDEC at 87 to 95 %**; the encoder's tick is at 12 to 40 % of its period
  and the host at 0.3 to 0.6 core in total. The encoder alone would hold 67 × 1080i (§0.1).
- **Host copies of decoded frames are gone** [M]: per channel the host spends 0.011 to 0.013
  core (0.23 core with software decode and upload) and the bus carries 14 to 31 MB/s for 8 to
  24 channels of 1080 (808 to 2068 MB/s before). What still crosses the bus: the compressed
  inputs going in, the coded pictures coming out, per-tick parameters, row statistics and
  quantizers (§C.4).
- **Does a new channel need a restart?** No [M]. Opening a session in a free slot takes 2 µs,
  closing less than 1 µs, a batch for a new format or more slots 0.2 ms on average (0.4 ms at
  most per tick) when the memory comes from an arena taken at start; the other sessions'
  streams are byte-identical to a run without the operations, and no tick was late at 69 % load.
  A new decoder is ready 3 to 32 ms after the request and lengthens no tick beyond 9.2 ms of the
  16.7 ms period (§C.6).
- **A late tick costs repeated pictures, not a growing delay** [M]: next to an unthrottled
  2160p transcode a line-up at 85 % of the period repeats 48 % of its pictures while the burst
  lasts, is never more than 17 ms behind, and repeats nothing from the first tick after it;
  without the policy it was 3.6 s behind after 10 s (§C.7).
- **The shared decoder cuts both ways** [M]: with 24 of our 1080p decodes running, a Plex
  transcode that wants to run ahead gets 38 to 65 fps instead of 400 to 460, and two 2160p
  real-time transcodes plus a tone-mapped one fall to 19–20 fps; past 88 % of the decoder our
  own inputs starve as well (47 % of output frames repeat their input), unless their decoders
  drop to reference pictures while they are behind (then 3 %) (§C.5, §0.7).

### C.2 Decoder capacity (NVDEC, measured on this card)

`build/decbench` through `scripts/decbench.py`: N looping streams of one file, decode only,
frames left on the device and released at once; 7 s unpaced per stream count, then 10 s paced
at three counts around the limit. `results/dec/a1.jsonl` (rows marked * were measured again
without another process on the card, `a2.jsonl`). All [M].

| Input (files of `scripts/prep_inputs.sh`) | Mbit/s | Frames per second in total at 1 / 2 / 4 / 8 / 16 streams (2160p: up to 8) | NVDEC utilization at those, % | Real-time streams (best total / source rate) | Paced: streams: ms a frame leaves the decoder after its time, in the second second → in the last second (40 streams was the most tried, 12 for 2160p) | Host CPU per real-time stream, cores | Device memory per stream, MB | Software decode: cores per real-time stream |
|---|---|---|---|---|---|---|---|---|
| `av1_1080p30.mkv` 1920x1080 nv12 | 4.3 | 1661 / 1816 / 1807 / 1802 / 1806 | 89 / 100 / 100 / 100 / 100 | 60.6 | 40: 53 → 16; 40: 51 → 16; 40: 47 → 16 | 0.007 | 101 | 0.18 |
| `h264_1080i.mkv` 1920x1080 nv12 interlaced | 10.1 | 834 / 831 / 831 / 825 / 840 | 100 / 100 / 100 / 100 / 100 | 28.0 | 22: 3 → 1; 26: 142 → 1; 30: 358 → 961 | 0.006 | 65 | 0.23 |
| `h264_1080p24.mkv` 1920x1080 nv12 | 8.2 | 907 / 903 / 902 / 902 / 901 | 100 / 100 / 100 / 100 / 100 | 37.8 | 30: 16 → 1; 35: 185 → 1; 40: 387 → 812 | 0.005 | 65 | 0.18 |
| `h264_1080p25.mkv` 1920x1080 nv12 | 8.1 | 912 / 909 / 907 / 907 / 906 | 100 / 100 / 100 / 100 / 100 | 36.5 | 29: 14 → 1; 34: 167 → 1; 40: 441 → 1126 | 0.005 | 65 | 0.17 |
| `h264_1080p30.mkv` 1920x1080 nv12 | 8.1 | 925 / 922 / 921 / 921 / 919 | 100 / 100 / 100 / 100 / 100 | 30.9 | 24: 2 → -2; 29: 155 → -1; 33: 369 → 876 | 0.005 | 65 | 0.19 |
| `h264_1080p60.mkv` 1920x1080 nv12 | 12.1 | 937 / 934 / 933 / 931 / 932 | 100 / 100 / 100 / 100 / 100 | 15.6 | 12: 1 → 0; 14: 11 → 1; 17: 274 → 948 | 0.009 | 65 | 0.32 |
| `h264_1366x768p30.mkv` 1366x768 nv12 | 3.1 | 1674 / 1671 / 1669 / 1667 / 1665 | 100 / 100 / 100 / 100 / 100 | 55.9 | 40: 8 → -1; 40: 11 → -1; 40: 7 → -1 | 0.004 | 35 | 0.08 |
| `h264_480i.mkv` 704x480 nv12 interlaced | 2.6 | 3302 / 3285 / 3284 / 3280 / 3278 | 100 / 100 / 100 / 100 / 100 | 110.2 | 40: 1 → 1; 40: 1 → 1; 40: 1 → 1 | 0.004 | 12 | 0.05 |
| `h264_720p50.mkv` 1280x720 nv12 | 6.0 | 1816 / 1811 / 1808 / 1809 / 1804 | 100 / 100 / 100 / 100 / 100 | 36.3 | 29: 6 → 1; 34: 153 → 1; 39: 386 → 967 | 0.007 | 35 | 0.13 |
| `h264_720p60.mkv` 1280x720 nv12 | 6.1 | 1839 / 1832 / 1829 / 1830 / 1826 | 100 / 100 / 100 / 100 / 100 | 30.7 | 24: 1 → 1; 29: 120 → 1; 33: 334 → 928 | 0.008 | 35 | 0.15 |
| `h264_854x480p30.mkv` 854x480 nv12 | 1.5 | 3179 / 3167 / 3167 / 3161 / 3161 | 100 / 100 / 100 / 100 / 100 | 106.1 | 40: 1 → -2; 40: 1 → -2; 40: 1 → -2 | 0.004 | 14 | 0.03 |
| `hevc_1080p30.mkv`* 1920x1080 nv12 | 5.1 | 1983 / 2077 / 2061 / 2059 / 2058 | 92 / 100 / 100 / 100 / 100 | 69.3 | 40: 6 → 2; 40: 7 → 2; 40: 6 → 2 | 0.007 | 75 | 0.24 |
| `hevc_1080p30_10.mkv`* 1920x1080 p010le | 6.4 | 1838 / 1879 / 1876 / 1875 / 1879 | 94 / 100 / 100 / 100 / 100 | 62.7 | 40: 11 → 1; 40: 11 → 1; 40: 10 → 1 | 0.006 | 121 | 0.30 |
| `hevc_2160p24_10.mkv` 3840x2160 p010le | 32.6 | 461 / 469 / 471 / 470 | 96 / 100 / 100 / 100 | 19.6 | 12: 4 → 2; 12: 4 → 2; 12: 4 → 2 | 0.010 | 431 | 1.37 |
| `hevc_2160p30.mkv`* 3840x2160 nv12 | 27.0 | 529 / 537 / 537 / 538 | 97 / 100 / 100 / 100 | 18.0 | 12: 4 → 1; 12: 4 → 1; 12: 4 → 1 | 0.013 | 235 | 1.05 |
| `mpeg2_1080i.ts`* 1920x1080 nv12 interlaced | 15.0 | 810 / 870 / 865 / 864 / 863 | 92 / 100 / 100 / 100 / 100 | 29.0 | 23: 9 → 10; 27: 100 → 14; 31: 305 → 864 | 0.008 | 42 | 0.07 |
| `mpeg2_480i.ts`* 704x480 nv12 interlaced | 4.0 | 2801 / 3235 / 3231 / 3222 / 3219 | 85 / 100 / 100 / 100 / 100 | 107.9 | 40: 5 → 5; 40: 5 → 5; 40: 4 → 5 | 0.005 | 10 | 0.01 |
| `vp9_1080p30.mkv`* 1920x1080 nv12 | 6.1 | 1430 / 1678 / 1675 / 1670 / 1673 | 84 / 100 / 100 / 100 / 100 | 56.0 | 40: 180 → 14; 40: 139 → 14; 40: 125 → 14 | 0.010 | 114 | 0.16 |

- **The engine has one throughput per kind of stream, whatever the number of sessions**: H.264
  1080p 900–935 fps from 1 to 16 streams. It scales with pixels and bit rate: about 1.9
  gigapixels per second for H.264 at 1080p, 1.7 at 720p, 1.1 at SD (more bits per pixel there).
  HEVC is 2.2 times as fast as H.264 at 1080p; a 2160p HEVC stream costs as much as four 1080p
  HEVC ones or two 1080p H.264 ones.
- One stream alone does not always fill the engine (MPEG-2, VP9, AV1 and HEVC reach 84 to 97 %
  of it with one decode thread); from two streams on the totals are flat. (In the first pass
  MPEG-2 and VP9 seemed to grow with the number of streams up to 16; that was a Plex transcode
  on the card. The rows marked * are the repeat without it.)
- **At the limit nothing is dropped**: frames leave the decoder later and later (H.264 1080p30:
  24 streams hold; 29 start 155 ms behind and recover; 33 are 0.9 s behind after 10 s and
  growing). Whoever feeds a frame clock from it must bound that delay himself (§C.9).
- **Host CPU** per real-time stream: 0.004 to 0.013 core (demux, bitstream parsing in
  libavcodec, driver calls).
- **Software decode of the same streams** (last column; one thread, cores needed for real time
  on this host; the frame then also has to be uploaded, 93 MB/s per 1080 stream): H.264 1080p30
  0.19, 1080i 0.23, 1080p60 0.32, 720p60 0.15, SD 0.03 to 0.05; HEVC 1080p30 0.24 (10-bit 0.30);
  HEVC 2160p 1.05 and 1.37 (one thread does not reach real time: 28 and 17 fps); MPEG-2 1080i
  0.07, 480i 0.011; AV1 0.18; VP9 0.16. On the card the same stream costs the host 2 (MPEG-2
  SD) to 100 (HEVC 2160p) times less; 38 times less for H.264 1080p30.
- **Which inputs to hand to the CPU first when the decoder is full** follows from the two
  columns: MPEG-2 (1080i: 3.4 % of the decoder for 0.07 core; 480i 0.9 % for 0.01 core) and
  H.264 SD (0.9 % for 0.03–0.05 core) are cheap for the CPU; H.264 1080p30 is 3.2 % for 0.19
  core; HEVC and anything at 2160p should stay on the card (5.6 % for more than a core).
- **Device memory** per decode session: 65 MB (H.264 1080p), 35 MB (720p), 12–14 MB (SD),
  73 MB (HEVC 1080p), 123 MB (HEVC 1080p 10-bit), 235 MB (HEVC 2160p), 431 MB (HEVC 2160p
  10-bit). On top of that the frames a frame clock keeps waiting (lead × rate per input, 3 MB
  per 1080p frame).
- No session limit was met (40 decoders at once, plus the encoder).

Variants at 4 streams (`results/dec/a1-var.jsonl`):

| Input, 4 streams unpaced | hwaccel, copy into a pool frame (default) | hwaccel, decoder surfaces (`unsafe_output`) | hwaccel, decoders in a context of their own | `*_cuvid` decoder | cuvid adaptive deinterlace, one frame per frame | adaptive, one per field | cuvid bob, one per field |
|---|---|---|---|---|---|---|---|
| H.264 1080p30 | 921 fps, SM 3 %, 123 µs CPU/frame, 65 MB/stream | 923, 2 %, 99 µs, 93 MB | 923, 3 %, 118 µs, 65 MB | 922, 3 %, 215 µs, 53 MB | – | – | – |
| HEVC 2160p | 537 fps, SM 12 %, 250 µs, 235 MB | 549, 7 %, 194 µs, 347 MB | 538, 12 %, 227 µs, 235 MB | 535, 12 %, 366 µs, 221 MB | – | – | – |
| HEVC 2160p 10-bit | 471 fps, SM 26 %, 303 µs, 431 MB | 495, 13 %, 255 µs, 623 MB | 471, 25 %, 300 µs, 431 MB | 464, 28 %, 353 µs, 407 MB | – | – | – |
| H.264 1080i | 831 fps, SM 3 %, 109 µs, 65 MB | 831, 2 %, 120 µs, 93 MB | 831, 3 %, 142 µs, 65 MB | 828, 3 %, 177 µs, 50 MB | 819 fps, **SM 34 %**, 197 µs, 85 MB | 1603 fps out, **SM 63 %**, 87 MB | 1655 fps out, SM 4 %, 53 MB |
| MPEG-2 1080i | 865 fps, SM 3 %, 154 µs, 42 MB | 872, 2 %, 122 µs, 57 MB | 868, 3 %, 143 µs, 42 MB | 871, 3 %, 198 µs, 45 MB | 847 fps, SM 35 %, 222 µs, 79 MB | 1660 fps out, SM 64 %, 79 MB | 1721 fps out, SM 5 %, 45 MB |
| H.264 480i | 3284 fps, SM 3 %, 66 µs, 12 MB | 3287, 1 %, 56 µs, 19 MB | 3290, 3 %, 71 µs, 12 MB | 3293, 3 %, 98 µs, 10 MB | 3280 fps, SM 26 %, 133 µs, 16 MB | 6525 fps out, SM 48 %, 18 MB | 6559 fps out, SM 7 %, 11 MB |

(All 18 inputs: `results/dec/a1-table.md`.)

- None of the routes changes the engine's rate. They differ in host CPU (the cuvid decoders'
  parser path costs 1.5 to 2 times the hwaccel's), device memory, and what they do on the SMs.
- **Decoding is not free on the SMs.** The driver turns every decoded picture from NVDEC's
  internal layout into NV12 / P016 with a CUDA kernel (`ConvertNV12BLtoNV12`,
  `ConvertP016BLtoP016` in a timeline), and the hwaccel then copies it into a pool frame. For
  1080-line H.264 that is 3 % SM utilization at 920 fps; for 2160p 10-bit it is 26 % at 471 fps,
  half of it the pool copy (13 % with decoder surfaces). In the pipeline's kernel time the
  driver's conversion is 6 % in line-ups (a) and (c) and 26 % in the downscale-heavy (d)
  (`results/nsys/pipe-*-kern.txt`). This is the per-frame cost of a transcode of §0.7, seen
  from inside the context.
- **Interlaced sources** decode like progressive ones (H.264 MBAFF 1080i 831 fps against 921,
  frames come out woven with the field flags set). cuvid's adaptive deinterlacer leaves the
  decode rate alone but runs on the SMs: 34 % utilization at 819 frames per second, 63 % when it
  returns a frame per field, about 0.4 ms of the compute engine per output frame [E]; its bob
  costs nearly nothing.

**Route chosen: libavcodec with the `cuda` hwaccel, in our context** (`src/dec.h`). Reasons [M
unless marked]:

- It works as documented: `av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_CUDA)`, the device context's
  `cuda_ctx` set to the context the CUDA runtime uses (`cuCtxGetCurrent`), `stream` set to a
  non-blocking stream of its own, `av_hwdevice_ctx_init`. Frames are `AV_PIX_FMT_CUDA`; their
  `data[0]` / `data[1]` are device pointers our kernels read (NV12, or P010 for 10-bit).
  ffmpeg-the-third exposes the same structures, so the Rust product can do the same with one
  small `unsafe` block for the two fields [E].
- **Decoders in a second context cost 8 to 10 % of every tick** (7.98 against 7.27 ms at 28
  channels, 7.61 against 7.02 ms at 27): two contexts time-slice even inside one process, the
  §0.7 mechanism. In our context the driver's decode work is queued with our kernels.
- The hwaccel copies each decoded picture from the decoder's surface into a pool frame (a device
  copy on libav's stream, no host memory). Handing out the decoder's own surfaces
  (`AV_HWACCEL_FLAG_UNSAFE_OUTPUT`) saves that copy (visible at 2160p, table above) but costs
  45 % more device memory per decoder and bounds how many frames may be held; in the pipeline
  at the limit it changed nothing that mattered (28 channels: 9 % of frames late against 18 %,
  both fail). The default stays; worth a second look for 2160p inputs.
- The `*_cuvid` decoders (NVIDIA's parser) were worse in the pipeline (at 28 channels 84 % of
  frames late against 18 %) and cost more host CPU. They would only be needed for their built-in
  deinterlacer, which costs four to six times the pipeline's bob in compute (§C.3). The Video Codec SDK used directly would give the same engine
  with our own parser glue; nothing measured asks for it.
- Traps: for AV1 `avcodec_find_decoder` returns libdav1d, which decodes on the CPU without a
  word (one core per stream); the native `av1` decoder must be named. A frame not on the device
  is detected and reported. Matroska timestamps are rounded to milliseconds; the harness takes
  frame times from the frame count for constant-rate inputs.

### C.3 Convert, scale, compose (`src/pipe.h`)

One resampler does every geometric job. A conversion "class" (source size and sample size, crop,
destination rectangle, which rows, filter) gets four tap tables made on the host when it is
first used (start index and Q14 weights per output sample; 3.7 ms per class on the tick thread,
§C.9); two kernels then
run for all jobs of a class in one launch each: `k_conv_h` (source rows → 8-bit intermediate,
splits the interleaved chroma, rounds 10-bit to 8) and `k_conv_v` (intermediate → the session's
planes in the encoder's input ring, inside the destination rectangle). Four output samples per
thread, 32-bit loads and stores; `k_conv_copy` replaces both when nothing is resampled. For
ratios of 3 and more in both directions (2160p into 720p or SD) `k_half` first halves the source
(luma 2 × 2 box, chroma [1 4 3]/8 across and the mean of two lines down, which keeps the siting),
and the resampler works on a quarter of the samples (`--exact-down` turns that off). The
encoder then fills the padding of the coded size itself (`Batch::pad_dev`).

Decisions:

- **Filter**: Lanczos-3 by default, stretched by the scale factor on downscale (so it is the
  antialias filter as well); Catmull-Rom bicubic and bilinear as options. Cost and quality below.
- **Chroma siting**: horizontally co-sited with the left luma sample (H.264/HEVC default and
  MPEG-2's rule), vertically centered. In an interlaced frame the chroma line j of either field
  is at luma line 2j + 0.5 in frame coordinates, as for a progressive frame, so one tap
  generator serves both; what changes is which lines exist.
- **Rows as a lattice**: all rows or one field, on either side. Frame → frame; field → field
  (an interlaced source at another size, each field resampled on its own with the phase its
  lines really have); field → frame (bob); frame → field.
- **10-bit → 8-bit**: rounded, no dither (inside the horizontal pass). HDR sources are not tone
  mapped: a PQ or HLG source would come out flat. Not built.
- **Color**: BT.709 for sources of 720 lines and more unless the stream says otherwise, BT.601
  below; the output is BT.709 for HD and SMPTE 170M for SD (as the sequence display extension
  says, §4). When the two differ, or the source is full range, `k_csc` applies the 3 × 3 matrix
  and range in place; the luma correction uses the nearest chroma sample (of the same field
  in an interlaced frame).
- **Aspect**: `fit=box` (default: all of the source visible, bars filled by `k_fill`), `crop`
  (destination filled, source cropped), `stretch`. 704 × 480 is taken as 4:3 with 10:11 samples
  (or 16:9 with 40:33 when the channel says `dar=16:9`, which also sets
  `aspect_ratio_information`). Rectangles are multiples of 8 samples wide and of 2 (interlaced:
  4) lines high, so a 16:9 picture in a 4:3 SD frame is 704 × 360 at line 60.
- **Frame rate and scan** (SPEC §5.4, the frame clock is the only place frames are dropped or
  repeated; both are counted per channel):
  - progressive output: the newest input frame due at the tick; no new one → `submit_again`
    (the encoder codes the previous frame again from the same ring slot: no conversion, no bus);
  - interlaced output: **one selection per field**. Both fields from the same source frame → one
    job, `progressive_frame` = 1 in the picture (legal in an interlaced sequence, §6.3.10; the
    encoder keeps its field tools). Different frames → each field from its own frame. This gives
    3:2 pulldown from 24p (53 of 89 frames progressive, 36 split, in the gate case), 2:2 with an
    occasional repeat from 25p, and fields of successive frames from 50/60p without a special
    case;
  - 50/60p sources into interlaced output are filtered vertically with **[1 2 1]/4 across output
    lines** before every second line is taken (the usual measure against interline twitter; it
    is ffmpeg's `interlace=lowpass=linear`, and the tap generator folds it into the vertical
    taps). 24/25/30p sources are not filtered (film-style);
  - a source at the output's frame rate gets one selection per frame: with one per field, half
    of all source phases split every frame between two source frames (found in the first runs);
  - interlaced sources: same size → rows copied, fields kept; other size, interlaced output →
    field → field; progressive output → bob, at 59.94p both fields in turn.
- **Deinterlacer: bob by default** [M] (`scripts/deint_quality.sh`, `results/pipe/deint.txt`:
  the fields of `crowd_run` 1080i against the true frames of the 1080p50 original, to
  720p59.94, luma PSNR / SSIM): pipeline bob with Lanczos 34.78 dB / 0.9722 (bilinear 32.67 /
  0.9630); cuvid's adaptive mode 34.66 / 0.9742 and cuvid's bob 34.41 / 0.9719 (both through
  the ffmpeg program; the adaptive mode returns its pictures one frame late); ffmpeg yadif on
  the CPU 33.12 / 0.9667; ffmpeg bwdif 35.48 / 0.9766. On this clip, which moves everywhere,
  cuvid's adaptive mode is no better than a bob and costs four to six times as much compute
  (§C.2) plus the cuvid decoder; the bob costs 62 µs per 720p frame and nothing in the decoder.
  Static material, where an adaptive deinterlacer keeps full vertical resolution and a bob
  does not, was not measured. A kernel of the bwdif class would gain 0.7 dB here; not built.
- **Composition**: a channel may be a grid of inputs (`mosaic=CxR`): `k_fill`, then one job per
  tile (all tiles of one class in one launch), then `k_overlay` for a plane with alpha (logo,
  banner or slate text: Y, U, V and alpha at picture size, rendered on the host and uploaded
  when it changes, never per frame). A mosaic is re-rendered when any tile has a new frame.
- **Slate**: one frame per format held on the device in the encoder's layout. A channel whose
  input has had no frame for `hold` (1 s; before that it repeats) gets it by one device copy and
  then by `submit_again`: no host traffic and no copy per frame. With `Session::hold` set it
  would not even cost GPU time, but then no key frames are coded either.

Cost per output frame [M] (`build/convbench`, 16 jobs per launch back to back, nothing else on
the card; `results/pipe/convbench.txt`). For scale: one 1080i picture costs the encoder about
440 µs at capacity, one 720p picture 330 µs, one 480i picture 75 µs.

| Conversion | Lanczos-3 | Bicubic | Bilinear |
|---|---|---|---|
| 1080p → 1080 (copy, chroma split) | 30 µs | | |
| 720p → 720 (copy) | 8 µs | | |
| 1080p → 720p | 99 µs | 77 | 63 |
| 720p → 1080p | 107 µs | 87 | 84 |
| 2160p → 1080p (10-bit) | 300 µs (401) | 259 (370) | 184 (207) |
| 2160p → 720p (10-bit), source halved first | 179 µs (232); without the half stage 264 (336) | 161 (213) | 140 (197) |
| 2160p → 704 × 360 letterbox (10-bit), source halved first | 143 µs (199); without 240 (359) | 124 (181) | 114 (172) |
| 1080p → 704 × 360 letterbox | 70 µs | 51 | 41 |
| 1080p → 704 × 480, cropped to 4:3 | 112 µs | 53 | 42 |
| 1080p60 → 1080i (two frames, low-pass) | 228 µs | 176 | 176 |
| 720p60 → 1080i (two frames, low-pass) | 175 µs | 143 | 129 |
| 1080i → 480i, field by field | 64 µs | 52 | 42 |
| 480i → 1080i, field by field | 83 µs | 74 | 69 |
| 1080i → 720p, bob | 62 µs | 50 | 39 |
| mosaic into 1080 from 720p sources: 2 × 2 / 3 × 3 / 4 × 4 | 160 / 291 / 773 µs | | 117 / 170 / 323 |
| mosaic into 1080 from 1080p sources: 2 × 2 / 3 × 3 / 4 × 4 | 288 / 586 / 1249 µs | | 171 / 331 / 523 |
| mosaic into 720 from 720p sources: 2 × 2 / 3 × 3 / 4 × 4 | 123 / 249 / 538 µs | | 75 / 144 / 223 |
| fill / color matrix / overlay at 1080 (at 704 × 480) | 15 / 30 / 34 µs (3.5 / 3.1 / 5.1) | | |

A same-size transcode costs 7 % of its encode; a 2160p source into a 1080 channel 70 to 90 %;
a 2160p source into a 480i channel two to three times its encode; a 3 × 3 mosaic channel about
one and a half encodes. In the kernel time of whole line-ups (Nsight Systems,
`scripts/nsys_pipe.sh`, `results/nsys/pipe-*-kern.txt`) the conversion kernels are 6.5 % in
(a) (all of it `k_conv_copy`), 10.9 % in the mixed line-up (c) and 44 % in the downscale-heavy
(d) (`k_conv_h` 22 %, `k_half` 14 %, `k_conv_v` 4.5 %), where the driver's own surface
conversion for the 2160p decodes takes another 26 % and the encoder's search 10 %. Only one
round of optimization was done (the half stage, 30 to 45 % off the 2160p conversions), because
in every line-up measured the decoder is exhausted first (SM utilization 12 to 40 % at the
limit). Next if compute ever limits: `k_half` with packed chroma loads, a second half stage
for ratios above 6, and a horizontal pass that skips the identity case of same-width weaving.

Quality [M]. Against an analytic pattern (sinusoids along both axes in luma and chroma, so that
every output sample has a known value at the position the mapping assigns to it; the pattern is
coded with H.264 at qp 2 and decoded by NVDEC, `geom-*` cases of the gate): 47.8 to 55.5 dB in
nine conversions, among them field → field (52.1 and 48.7 dB; ffmpeg's `scale=interl=1` on the
same source: 27.8 and 34.7 dB, it puts both fields at the same phase) and bob at 59.94p over
both field parities (worst frame 50.2 dB: no line jitter between fields), and 2160p sources
through the half stage (51.4 dB into 720p, 49.2 dB into 480i). Against zimg's
Lanczos-3 on the CPU with real content (`results/pipe/quality.txt`; ffmpeg's `zscale` cannot be
the reference for interlaced material, it scales an interlaced frame as a progressive one):

| Conversion | Lanczos-3: Y / U / V, dB (SSIM) | Bicubic | Bilinear |
|---|---|---|---|
| 2160p 10-bit → 1080p | 55.3 / 55.5 / 55.4 (0.9986) | 49.2 / 54.0 / 54.1 | 40.0 / 49.0 / 49.5 |
| 2160p → 720p, source halved first (the default) | 49.5 / 53.7 / 54.1 (0.9983) | 42.8 / 51.0 / 51.4 | 36.3 / 46.7 / 47.0 |
| 2160p → 720p, `--exact-down` | 57.5 / 58.1 / 58.4 (0.9994) | 46.2 / 54.2 / 54.6 | 38.7 / 48.5 / 48.7 |
| 2160p → 704 × 360 in 480i, source halved first (the default) | 50.3 / 54.7 / 54.9 (0.9987) | 43.1 / 51.6 / 51.8 | 36.8 / 46.8 / 46.9 |
| 2160p → 704 × 360 in 480i, `--exact-down` | 51.1 / 55.3 / 55.5 (0.9988) | 43.9 / 52.4 / 52.6 | 36.7 / 46.9 / 46.9 |
| 1080p → 720p | 56.1 / 57.8 / 58.3 (0.9992) | 46.7 / 56.6 / 56.9 | 38.3 / 52.5 / 52.3 |
| 1080p → 704 × 360 in 480i, BT.709 → 601 | 52.2 / 55.4 / 55.8 (0.9988) | 45.2 / 53.7 / 53.9 | 38.2 / 49.4 / 49.3 |
| 720p → 1080p | 55.4 / 57.8 / 58.5 (0.9989) | 46.1 / 57.0 / 57.3 | 39.0 / 53.9 / 53.6 |
| 854 × 480 → 1080i, BT.601 → 709 | 53.2 / 54.6 / 55.5 (0.9979) | 45.6 / 54.1 / 54.6 | 39.0 / 52.2 / 51.8 |
| 1080p60 → 1080i, fields woven with the low-pass (against `interlace=lowpass=linear`) | 54.9 / 57.7 / 58.2 (0.9991) | the same | the same |
| HEVC 10-bit 1080p → 8-bit, same size | 57.2 / 57.3 / 56.6 (0.9991) | | |

The Lanczos column is two implementations of the same filter (what is left is rounding of the
8-bit intermediate); the other columns show what the cheaper filters lose against it. Same-size
paths are bit-exact for H.264 and AV1; MPEG-2 through NVDEC against libavcodec's decoder is
61 dB (MPEG-2's inverse transform is not bit-exact between decoders).

### C.4 End to end: line-ups at real time

`build/m2vpipe` through `scripts/pipe_lineup.py`. Inputs loop (10 s files), paced by their own
time stamps like live sources, each on a thread of its own; frames are due 300 ms after their
packets (the lead: it covers picture reordering and decode latency; a real mapper has its
jitter-buffer target here, SPEC §5.3). A frame's time is its index (from its time stamp) over
the input's rate. The tick is the field rate, 59.94 Hz; 29.97 Hz channels
are spread over its two phases. A line-up "holds" when at least 99 % of ticks deliver before the
next is due **and** at most 1 % of decoded frames come later than the lead, in a 20 s run
(the first 2.5 s left out). Outputs at the nominal rates of §3. All [M]; "clean" = no other
process on the card during the run.

| Line-up | Holds | First count that fails | NVDEC | Tick mean / p99 at the limit (period 16.7 ms) | SM | W | Host CPU: total (tick thread / decode threads) | Device memory | Run |
|---|---|---|---|---|---|---|---|---|---|
| (a) 1080p30 H.264 → 1080i | **27** | 28: 2.7 % of frames late | 91 % | 6.1 / 7.2 ms | 35 % | 50 | 0.37 (0.08 / 0.28) | 4.5 GB | clean |
| (a) with four kinds of 1080 source (H.264 p30, H.264 1080i, HEVC p30, MPEG-2 1080i) | **29** | 30: 3.2 % | 89 % | 6.6 / 7.5 ms | 38 % | 54 | 0.40 (0.09 / 0.31) | 4.7 GB | clean |
| (b) 720p60 H.264 → 720p | **27** | 28: 10.8 % | 97 % | 5.1 / 6.3 ms | 31 % | 48 | 0.39 (0.07 / 0.31) | 2.8 GB | next to a Plex transcode at 1 % of the decoder; 26 and 24 in two earlier runs next to a busier one |
| (c) mixed unit: 1 × 1080i + 1 × 720p + 4 × 480i from nine kinds of input | **8 units (48 channels)** | 9: 44 % | 87 % | 4.9 / 6.2 ms | 26 % | 46 | 0.57 (0.09 / 0.47) | 3.4 GB | clean |
| (d) 2160p and 1080p sources → 480i (one 2160p in four inputs) | **24** | 25: 26 % | 92 % | 3.8 / 6.0 ms | 22 % | 45 | 0.35 (0.06 / 0.28) | 5.2 GB | next to a Plex transcode at 1 % of the decoder |
| (e) 3 × 3 mosaic into 1080i with overlay, nine decoders per channel | **4 (36 inputs)** | 5: 8 % | 75 % (93 % at 5) | 2.0 / 2.8 ms | 12 % | 35 | 0.36 (0.05 / 0.30) | 2.7 GB | clean |

- In every run that holds, all ticks were on time (100 %), so the counts are the same for
  99.9 % of ticks; what ends a line-up is always the share of decoded frames that come late.
- **The bottleneck is the decoder in every line-up.** The encoder's tick is at 12 to 40 % of its
  period, the SMs at 12 to 38 %, the host below 0.6 core, the card at 35 to 54 W and 73 to
  82 °C (no throttling). The encoder's own limit (67 × 1080i, 24 mixed units, §0.1) is two to
  three times further out.
- What a channel takes from the decoder follows from §C.2: an H.264 1080p30 input 3.3 % (100 /
  30.9), 1080p60 6.4 %, 720p60 3.3 %, an SD input 0.9 %, HEVC 1080p30 1.5 %, HEVC 2160p30 5.6 %
  (10-bit at 24 fps 5.1 %), MPEG-2 1080i 3.9 %. In the pipeline the engine gives about 9 % less
  than alone (840 against 920 fps of H.264 1080p at "95 %"), so plan with 90 % of the table.
- **Hybrid** [M, next to a Plex transcode at 1–2 % of the decoder]: with 22 inputs on NVDEC and
  the rest decoded in software (one thread each, swscale, upload), 30 channels of 1080 held at
  2.1 cores and 34 at 3.0 cores (99.6 % of ticks); 38 did not (90 %; 4.0 cores, the tick thread
  no longer gets the CPU in time on 8 vCPUs shared with the other services). The mixed unit with
  its four SD inputs in software: 9 units (54 channels) at 2.5 cores held 98.9 and 99.6 % of
  ticks in two runs (borderline). Software decode of SD inputs is the cheapest way to buy decoder capacity (0.07 core for
  0.9 % of NVDEC), but the 8 vCPUs do not carry it far.
- Decoded frames never touch the host (`all_on_device` is checked per input in every run).
- **At the encoder's own limit the device path is not free** [M]: with the clip held on the
  device and one device copy per frame into the input planes (`m2venc --input-mode dev`), 67 ×
  1080i held 98.75 % of ticks at a mean tick of 29.95 ms, 70 held 92.4 % (32.0 ms); from host
  memory with the overlapped upload both held 100 % (28.1 and 29.8 ms). The copy or conversion
  of a device frame is compute work inside the tick (2 ms for 67 frames, the 30 µs per frame of
  §C.3), while the host upload ran beside the kernels on the copy engine. So an encoder fed
  from the device ends about 7 % earlier, at 62 to 63 × 1080i [E]. It does not matter while the
  decoder ends at 27. Removing it would mean letting the encoder's kernels read the decoder's
  NV12 surface directly; not built.

Sustained [M], with the deadline policy and `--resync 600`, no other process on the card
(`results/pipe/sustain.jsonl`): 7 mixed units (42 channels) for 180 s: every tick on time (mean
4.2 ms, p99 5.3 ms), NVDEC 78 %, SM 24 %, 42 W, 78 °C, 0.31 core; 174 of about 260,000 output
frames repeated their input and 150 input frames were dropped, at the moments when all 42 input
files loop at once (their decoders restart together and for a moment their frames are more
than 300 ms late: 5.8 % of the decoded frames over the run; an artifact of looping files, not
seen with live inputs [E]). 24 × 1080p30 → 1080i for 120 s: every tick on time (5.4 / 6.4 ms),
no late frame, no repeat, no drop, NVDEC 84 %, SM 33 %, 50 W, 82 °C, 0.22 core. The card does
not throttle at these loads (it did at the encoder's own limit, §0.3).

Host CPU and bus, device path against host path (software decode in one thread per input,
swscale to the output size, upload from pinned memory; `results/pipe/host2.jsonl`, a Plex
transcode at 1–2 % of the decoder on the card):

| Line-up | Path | Host CPU, cores (per channel) | PCIe to the card, MB/s | Tick mean, ms | Ticks on time | W |
|---|---|---|---|---|---|---|
| 8 × 1080p30 → 1080i | host | 1.83 (0.229) | 808 | 3.23 | 100 % | 23 |
| | device | 0.10 (0.013) | 14 | 1.90 | 100 % | 26 |
| 16 × | host | 3.62 (0.226) | 1389 | 6.34 | 99.33 % | 28 |
| | device | 0.20 (0.013) | 23 | 3.92 | 100 % | 34 |
| 20 × | host | 4.56 (0.228) | 2068 | 9.11 | 93.99 % | 32 |
| 24 × | device | 0.27 (0.011) | 31 | 5.43 | 100 % | 47 |
| 2 mixed units (12 channels) | host | 1.35 (0.113) | 474 | 2.14 | 100 % | 21 |
| | device | 0.13 (0.011) | 39 | 1.66 | 100 % | 23 |
| 4 mixed units (24 channels) | host | 2.56 (0.107) | 955 | 5.09 | 100 % | 24 |
| | device | 0.28 (0.012) | 28 | 2.92 | 100 % | 28 |

The device path needs a seventeenth of the host CPU (0.012 against 0.23 core per 1080 channel)
and a fiftieth of the bus traffic, and its tick is 20 to 40 % shorter at equal line-up (no
upload). It draws 2 to 6 W more (the decoder). Temperatures were the same within 2 °C. The host path
ends at about 16 channels of 1080 on this VM.

What crosses the bus on the device path, per channel of 1080 at nominal rates: about 1 MB/s of
compressed input in, 1.9 MB/s of coded pictures out, and per tick the session parameters, the
row statistics down and the row quantizers up (kilobytes). `nvidia-smi dmon` shows 1.3 to
1.8 MB/s per channel received by the card and 2.5 MB/s sent.

Timing as the mux will see it: output frame k of a channel has PTS k × frame period (90 kHz,
exact); `--ptslog` records per output frame the tick, the PTS, what was done (N new frame,
W two source frames in one picture, A the input repeated, S slate, H host frame) and the input
frame behind each field. Repeats and drops are counted per channel and reported.

### C.5 Sharing the card, now that decode shares it too

§0.7 described what a Plex or Jellyfin transcode costs the encoder's kernels. With the inputs on
NVDEC there is a second shared resource, and it behaves differently: **the decoder is shared by
the picture, and a session gets what it asks for until the total is used up; who asks for more
than is left gets the remainder.** Measured with `scripts/contend.sh` (competitors: ffmpeg
commands like Plex's, `h264 nvdec → scale_cuda → h264_nvenc`, a 1080p H.264 and a 2160p HEVC
10-bit source, real time or unthrottled; alone they run at 460, 400, 899 fps), our pipeline with
the deadline policy on, 10 s per competitor, [M]:

| Our line-up (share of NVDEC it uses) | Next to | Our ticks late | Our output frames that had to repeat their input | The competitors |
|---|---|---|---|---|
| 24 × 1080p30 → 1080i (80 %) | 1080p + 2160p + scaled 1080p transcodes, real time | 0 % | 0 % | 25, 25, 25 fps: fine |
| | two 2160p + one tone-mapped 2160p, real time | 0 % | 0 % | 20, 20, 19 fps: **below real time** |
| | 2160p transcode, unthrottled | 0 % | 0 % | 38 fps (alone 400) |
| | 1080p transcode, unthrottled | 0 % | 0 % | 65 fps (alone 460) |
| 26 × (88 %) | one 1080p transcode, real time | 0 % | 0 % | 25 fps |
| | 1080p + 2160p + scaled 1080p, real time | 0 % | 0 % | 25, **19**, 25 fps |
| | two 2160p + tone-mapped, real time | 0.1 % | **46.8 %**, and 58 % in the 5 s after | 18, 18, 18 fps |
| | 2160p transcode, unthrottled | 0 % | 2.9 % | 20 fps |
| 7 mixed units, 42 channels (76 %) | 1080p + 2160p + scaled 1080p, real time | 0 % | 0 % | 25, 25, 25 fps |
| | two 2160p + tone-mapped, real time | 0 % | 21.7 % | 25, 25, 25 fps |
| | 2160p transcode, unthrottled | 0 % | 0 % | 51 fps |

- **What 1 to 3 transcodes take from us**: their decode rate. A 1080p24 H.264 transcode in
  real time is 2.6 % of the decoder, a 2160p24 HEVC one 5.1 %; three of them, two from 2160p,
  13 %. A transcode that runs ahead takes whatever is free and no more: it did not starve
  our inputs at 80 %.
- **What we take from them**: everything above their real-time need. Plex's bursts ahead of
  real time (§0.7: 1 to 3 s every 10 to 20 s on 1080p sources) shrink from 400–460 fps to
  38–65 fps when we use 80 % of the decoder, so they last correspondingly longer; a real-time
  2160p transcode falls below real time when we use 88 %, and with two 2160p and a tone-mapped
  one already at 80 %. Their viewers then see buffering. During the work a real Plex transcode
  that started on the card took 80 % of the decoder for some seconds and made 9.7 % of our
  output frames repeat their input in that window.
- **Past the total both lose, and a decoder that fell behind stays behind** by what it missed;
  it catches up only as fast as capacity is free (58 % repeats in the five seconds after the
  competitors had gone). `--resync ms` bounds that: a decode thread that is that far behind its
  packets gives up the rest of the GOP and continues at the input's next key frame. With 600 ms
  the same case gave 32 % repeated input during the three heavy transcodes (47 % without), 17 %
  in the 2.5 s after (58 %), the worst frame 592 ms late (866 ms); 7 resynchronizations, 410
  packets skipped (`results/contend/pc-a26-resync`). It bounds the delay; it does not create
  decoder capacity.
- **Better: a decoder that falls behind stops decoding the pictures nothing refers to**
  (`--degrade ms`: `skip_frame = AVDISCARD_NONREF` while the decode thread is that far behind
  its packets, back to normal when it has caught up; libavcodec then does not send those
  pictures to NVDEC at all). With 250 ms, the same 26 channels next to the same three heavy
  transcodes: **3.0 % of output frames repeated their input** (46.8 % without anything, 32 %
  with the resynchronization alone), no frame later than the lead, nothing left over in the
  gap after it, 36 suspensions of a fraction of a second, no resynchronization needed
  (`results/contend/pc-a26-degrade`) [M]. An input in that state runs at the rate of its
  reference pictures (a third of its frame rate for two B pictures between anchors) and at
  that share of its decoder load. The competitors gained nothing from it (19–20 fps). Both
  options are off by default in the harness so that capacity figures mean "everything
  decoded"; a product should run with both. Neither rescues a line-up that is over the decoder's
  capacity for good: 29 channels of 1080p30 with both on still had 9 % of frames late, and
  10 % of output frames showed the slate when resynchronizations met the 2 s GOPs of the inputs
  (`results/pipe/final.jsonl`).

Planning figures, replacing the table of §0.7 for line-ups whose inputs are decoded on the card
(the §0.7 table still holds for the encoder's kernels, which are not the limit here) [E from
the measurements above]:

| Case | Decoder share to plan for our inputs | (a) 1080p30 H.264 → 1080i | (a) mixed 1080 sources | (b) 720p60 | (c) mixed units | Why |
|---|---|---|---|---|---|---|
| Alone on the card | 90 % | 27 | 29 | 27 | 8 (48 channels) | measured (§C.4) |
| **Typical**: up to 3 real-time transcodes, 1080p or 2160p, no tone mapping | **75 %** | 22 | 24 | 22 | 6–7 (36–42 channels) | 13 % for their real-time need; at 80 % (24 channels) three real-time transcodes held 25 fps and our inputs were not touched, at 88 % a 2160p one fell to 19 fps |
| **Worst case**: two 2160p and a tone-mapped 2160p stream in real time | **65 %** | 19–20 | 21 | 19 | 5–6 | at 76–80 % either they fell to 19–20 fps or a fifth of our frames repeated |
| Their bursts ahead of real time | not planned for | | | | | they get what is free: a tenth of their alone rate at our 80 % [M], more below it [E] |

The encoder's side of sharing is unchanged (§0.7): at these counts its tick is at 25 to 40 % of
the period, so even a tick ratio of 2 next to an unthrottled 2160p transcode leaves it on time;
the deadline policy (§C.7) is the safety net, not the plan. If inputs come in as HEVC, or more
than a third are SD, the counts rise as §C.2 says; if the 8 vCPUs are free, software decode of
SD inputs buys decoder share at 0.07 core per input.

### C.6 Sessions at run time

The owner's question: *does the encoder support dropping streams in and out, or does a new
channel need a restart?* It does now; nothing is restarted and the others are not disturbed.

How it works (`src/enc.h`):

- A batch has `g.n` slots; `sess[slot]` is a session or free. All kernels take lists of slot
  indices, so slots may be sparse and sessions never move.
- `Encoder::open(config)` → a free slot of a batch of that format; no device work at all (the
  first picture is an I picture, so nothing of the slot's old content is read). If there is no
  free slot a new batch of `config.capacity` slots is laid out.
- **Batches come from an arena** (`EncoderOptions::arena_mb`): device memory and pinned host
  memory taken from the driver once at start; a batch adds up what it needs, takes one block of
  each, and clears it with one fill on the stream. Freeing gives the blocks back. Without the
  arena every buffer is allocated from the driver, which is where the stall is (below).
  `reserve()` (the same on a side thread, adopted at the start of a tick) exists and is the
  weaker answer.
- `close(session)` flushes the session (waiting B pictures are closed with a P picture); its
  last pictures come out in the next ticks, the last one flagged `last`, then the slot is free
  and the caller ends the stream with `sequence_end_code`. `release()` drops the object.
- A format change is close + open: the new session starts with a sequence header and an I
  picture (closed GOP). Batches that stay empty are reaped (`reap(after_ticks)`).
- More slots for a format = another batch of that format (several batches per format were
  already possible). Nothing is reallocated or copied. Cost: each batch has its own kernel
  launches; merging two half-empty batches (moving a session at a key frame) is not built.

Proof (`churn` case of the gate): 3 × 1080i, 2 × 720p and 4 × 480i sessions run while pools of
4 × 1080i, 3 × 720p, 6 × 480i and 2 × 1080p sessions are opened, closed and reopened with another
rate at random, 6 to 8 operations per second, in four ways (batches from the driver in the
tick, on the side thread, with overlapped upload, from the arena). **All 36 streams of the
undisturbed sessions are byte-identical to a run without the operations**, and the 93 streams
of the churned sessions each start with a sequence header and an I picture, end with
`sequence_end_code`, decode strictly in ffmpeg and hold their buffer model.

Cost under load [M] (`scripts/churn_bench.sh`: 48 × 1080i stable plus pools of 8 × 1080i, 6 ×
720p, 16 × 480i and 4 × 1080p at 2 operations per second; mean tick 11.4 ms of 16.7 ms = 69 %;
24 s; `results/churn/churn48-*.txt`):

| Operation | On the tick thread | Lands inside a tick? |
|---|---|---|
| open in a free slot | 2 µs (max 9 µs) | yes |
| close | < 1 µs (max 2 µs) | yes |
| new batch of 4 slots from the arena (more slots, or a new format) | 0.23 ms mean; all operations of one tick together 0.41 ms at most (the 48-slot batch at start, 2.9 GB: 1.85 ms) | yes: no late tick in 1440, ticks with an operation p99 13.1 ms against 13.7 ms without |
| new batch of 4 slots allocated from the driver in the tick | 5.3 ms mean; all operations of one tick together 9.4 ms at most (the 48-slot batch at start: 73 to 86 ms) | no: 1 late tick, ticks with an operation up to 19.7 ms |
| new batch allocated on a side thread | 0.06 ms to start, 2 µs to adopt; the thread needs 7.9 ms (max 10.5 ms), an open waits 5.5 ms (max 33 ms) | no: 3 late ticks; the tick thread's CUDA calls wait behind the driver's allocation lock |
| a new input: decoder created on a side thread while 20 channels run | ready after 32, 3 and 15 ms (1080p H.264, 2160p HEVC, 720p H.264) | longest tick in the 1.5 s after: 5.8, 9.2, 8.8 ms (p99 of the run 6.8 ms); no late tick |

Memory: a slot costs 61.5 MB (1080), 27.1 MB (720p), 10.0 MB (480i) of the arena whether it is
used or not; a decoder 12 to 431 MB (§C.2). The arena has to be sized for the line-up the box is
meant to carry; when it is full, batches fall back to the driver (with the stall).

### C.7 Deadline-miss policy

`Encoder::step(deadline)`; all of it is in `Encoder::plan`, `Batch::host_repeat` and
`Batch::skip_slices`.

- **Before** the tick's work is queued (kernels cannot be taken back), the macroblocks that fit
  into the time to the deadline are estimated from the throughput of the last ticks. If the
  tick's pictures do not fit, sessions give up theirs for this tick: **the sessions with the
  lower `priority` first; among equals B pictures first, then P, key frames last, in rotation.** A tick
  that starts after its deadline gives up everything, so the output is never more than about a
  tick behind.
- **A given-up picture is a repeat made on the host in microseconds**: per slice the first and
  last macroblock coded with a zero vector and no coefficients, the rest skipped (a B picture is
  318 bytes at 480i, 513 at 720p, 902 at 1080i). No kernel runs for it.
  - B position: a B picture that repeats the anchor before it. Nothing else changes (one frozen
    frame).
  - P position: a P picture that repeats the previous anchor. The encoder keeps its reference
    slot (the decoder's new anchor has the same content) and marks the session; the B pictures
    that lie before this P in display order are then repeats as well, so the viewer sees three
    frozen frames, not a step back in time.
  - **Key frame position**: a repeat P without GOP header (`AccessUnit::key_missed`). The next
    frame submitted is typed I (`off_grid`), and **the cadence stays on its grid**: the next key
    frame after that is where it would have been. If the make-up attempt is given up too, the
    session waits for the grid (prediction simply continues; nothing is wrong with the picture,
    only tune-in takes a GOP longer).
  - The first picture of a stream is never a repeat.
- **Buffer model**: a repeat is a coded picture like any other for both models (it always fits:
  `min_rate()` of §5.3 reserves twice its size per frame period). The gate checks both models on
  streams with 10, 50 and 90 % random repeats; no violation.
- **A late or failed input** is the caller's side: the frame clock repeats the input
  (`submit_again`: an exact repeat, coded normally) and after `hold` switches to the slate.
  `Session::hold = true` makes every picture of a session a host repeat (no GPU time at all);
  on release it starts with a key frame. One session's input never touches another session:
  shown with an input stalled for 4 s among 23 channels (29 repeated frames, then the slate,
  then the input again; every tick on time).
- **Per-session errors**: `open()` returns nullptr with `last_error` instead of ending the
  process; a session can be closed or held at any time. A CUDA error is still fatal for all
  (it is the context's, not a session's).
- Counted per session: `repeats`, `shed_count`, `keys_missed`; per access unit: `repeat`,
  `key_missed`, `off_grid`.

Demonstration [M] (`contend.sh` with `--deadline`, 65 × 1080i from host memory: the "typical"
planning load of §0.7, 82 % of the period alone; 10 s per competitor, 5 s gaps;
`results/contend/dl-on-1080i65`, `dl-off-1080i65`; no other process on the card):

| Next to | Ticks late | Pictures repeated | Furthest behind | Without the policy |
|---|---|---|---|---|
| nothing (the gaps, each starting a second after a burst) | 0 % | 0 % | 0 ms | |
| one 2160p transcode, unthrottled (the burst case) | 3.3 % | **47.9 %** | **17 ms** | 100 % late, **3646 ms** behind after 10 s, still 2.8 s behind 15 s later |
| one 1080p transcode with `scale_cuda`, unthrottled | 1.0 % | 13.3 % | 1 ms | 95 % late, 3 s behind (after the above) |
| two 2160p transcodes and a tone-mapped one, real time | 7.9 % | 10.8 % | 14 ms | |
| 1080p, 2160p and scaled 1080p transcodes, real time | 2.3 % | 0.7 % | 11 ms | |

"Late" ticks end a few milliseconds after the next one was due and do not accumulate. The share
of repeats is what §0.7 predicted from the tick ratios (a third to a half for a 2160p burst).

Two earlier versions of the estimate failed and are in the log (experiment 50): a cost per
macroblock rode the deadline (half the ticks marginally late), and with a fixed cost unknown to
it plus chained key-frame retries it locked into repeating 98 % of all pictures after a burst.
The current rule has no state that can stick: the estimate is the best throughput of the last
three ticks, every tick that ends early raises it by time available over time used, and one
picture is always coded when there is time.

**Scheduler shape** (as the harness does it, and as the product's encoder thread would):

- One tick thread at the field rate (59.94 Hz). A format's clock is a divisor and a phase of
  it (29.97 Hz channels alternate between the two phases; 720p uses every tick).
- Per tick: frame clock for every channel due (select, or repeat, or slate) → conversion jobs
  queued on the encoder's stream → `step(deadline = the next tick)` (plan, queue, three waits) →
  access units out. Decode threads (one per input, nice 10) and allocations never run on it.
- The deadline is the next tick in the harness. The mux's real deadline is later (SPEC §5.2:
  0.5 s encode budget), so a tick that ends milliseconds late is harmless; what the policy
  guarantees is that lateness does not add up.
- Shedding order is by priority, then picture class. Measured with 33 sessions of priority 1
  and 32 of priority 0 next to an unthrottled 2160p transcode (`results/contend/prio-1080i`): the
  first group repeated 0.06 % of its pictures, the second 39.6 %; 7.3 % of ticks late, never more
  than 16 ms behind. Measurement supports nothing
  finer: the cost of a picture is its macroblocks, whatever its type (§0.8).

### C.8 Correctness

`make check` and `make check-full` (the encoder's gate, §7) are unchanged and pass:
54 of 54 at the end of this package, quality identical to the baseline. `make check-pipe` (`scripts/pipe_check.py`, 46 cases, none failed, `results/check/pipe.txt`):

| Cases | What is compared | Result |
|---|---|---|
| `devin` | streams from frames submitted on the device against the same frames from host memory, four upload shapes | 23 streams identical |
| `shed10/50/90`, `hold` (9) | random repeats and held sessions: strict decode in ffmpeg and libmpeg2, frame count, normative and operating buffer model, the encoder's reconstruction of every picture it really coded against the decoder | no message, no violation, drift 63.2 dB or better; missed key frames followed by one within a GOP |
| `churn` | §C.6 | 36 streams identical, 93 churned streams strict |
| `geom-*` (9) | converted frames against the analytic pattern, among them two with the half stage (2160p → 720p, 2160p → 480i) | 47.8–55.5 dB; tolerance 44 to 48 dB per case |
| `pipe-*` with a reference (15) | converted frames against the same conversion on the CPU (zimg Lanczos, `interlace`, `pad`, `crop`), tolerance 38 to 55 dB per case, bit-exact for same-size H.264 and AV1 | 51.6–57.6 dB; 4 bit-exact; MPEG-2 60.9 dB |
| `pipe-*` structure (11) | 24p → 1080i (3:2), 25p → 720p, 50p → 1080i, bob, field → field, VP9, mosaic with overlay, slate | every coded stream decodes strictly in two decoders and holds its buffer model; frame counts as submitted |

Every `pipe-*` case also checks that the input was decoded on the device and that the coded
stream passes the strict decode, the second decoder and the normative buffer check.

Not proven by the gate: the paced path's timing (it is measured, not gated); quality of the
coded pictures after conversion (the encoder's own gate covers coding); captions through the
pipeline (the harness passes none); bottom-field-first sources.

### C.9 What is not production-shaped (in addition to §0.10 and §12)

- **The decoder is the limit and nothing manages it.** There is no admission control (a line-up
  that needs 105 % of NVDEC is accepted and every input degrades), no choice between NVDEC and
  software decode per input, and no reaction to Plex taking its share. The figures of §C.2 and
  §C.5 are what such a manager would work from.
- **A decoder that falls behind** drops to its reference pictures (`--degrade`) and, if that is
  not enough, to its input's next key frame (`--resync`); both are off by default and have one
  test each (§C.5). Streams without non-reference pictures (no B pictures, or B pyramids where
  most B pictures are references) get nothing from the first.
- **Decoded frames wait on the device** for the lead (9 frames per 1080p30 input at 300 ms,
  18 at 60 fps; 3 MB each at 1080p, 12 to 25 MB at 2160p). With the SPEC's 1 s jitter buffer that
  is 0.7 GB for one 2160p 10-bit input. The jitter buffer should hold packets, not pictures,
  and decode only the lead ahead [E]. Not built.
- **Timestamps.** The harness derives frame times from frame indices (constant-rate inputs);
  a real mapper works from arrival times with skew estimation (SPEC §5.3). Equal-rate sources
  whose phase sits on a tick boundary will alternate between repeat and drop without
  hysteresis; not handled.
- **HDR, bottom field first, 4:2:2 / 4:4:4, odd crops.** PQ/HLG sources are not tone mapped;
  interlaced sources are assumed top field first; only 4:2:0 surfaces (NV12, P010) are
  accepted; rectangles are multiples of 8 × 2.
- **Deinterlacing is a bob.** A motion-adaptive kernel would gain about 0.7 dB (§C.3).
- **The arena is fixed at start**; a line-up that outgrows it stalls ticks by 10 to 90 ms per new
  batch. Half-empty batches of one format are not merged.
- **The deadline estimate** assumes a tick's time scales with its macroblocks. With host uploads
  (not the product's path) part of the tick does not shrink when pictures are given up, and the
  policy then sheds more than needed. A late tick is tolerated a few percent of the time during
  bursts (it ends milliseconds late).
- **Repeats are freezes.** A repeated P picture freezes three frames; nothing smarter (coding at
  lower quality instead of repeating) was tried.
- **CUDA errors are fatal** for the process. A decode error is counted and the input carries
  on; the error-burst rule of SPEC §9.3 (end the input's session) is not implemented. No decode
  error occurred in any run.
- **A new kind of conversion stalls its first tick**: the tap tables of a class are made and
  uploaded on the tick thread when the class is first used, 3.7 ms each (18.7 ms for the five
  classes of the mixed line-up, at start) [M]. They belong in the arena, or on a side thread
  before the channel starts.
- **The arena is not used by the harness** (`m2vpipe` opens all its sessions at start); it was
  measured with `m2venc --arena`.
- **The harness is not the product**: one process, inputs are files, no audio, no mux.
  The 8 vCPUs were never the limit on the device path (0.6 core at most); audio and mux are
  still unmeasured.
- Conversions had one round of optimization (§C.3). Nsight Compute was not run on them, only
  Nsight Systems' kernel tables (the SMs are at 12 to 40 % when the decoder is exhausted).


## 0. Phase B: optimization (the encoder's current state)

Work of 2026-11-25, 04:35 to about 08:25 PST, on the same host and card. The running record
of every experiment, kept or not, with its numbers is `notes/cuda-mpeg2-optlog.md`; this
section is the state at the end. Speed figures in §1 and §8 to §11 are those of the end of
phase A and are kept for the history; where they differ from this section, this section is
current.

### 0.1 Result

Capacity: sessions that hold real time (99 % of ticks on time in a 25 s paced run), realistic
workload (all tools on, staggered GOPs, overlapped upload), `crowd_run`, GPU warm at 86 to
91 °C [M]:

| Format | Phase A | Milestone 1 | Now | Ratio to phase A | Macroblocks/s at that point | Where the "now" figure comes from |
|---|---|---|---|---|---|---|
| 1080i29.97, 15 Mbit/s | 29 | 47 | **67** | 2.3 | 16.4 M | bisection with a Plex transcoder at 2 % decoder load (67: 100 %, 68: 93.5 % in a disturbed run); three minutes without any other process: 100 %; with the final binary 67 held 99.86 to 100 % in four runs next to an active transcode and 68 held 95.7 to 99.0 % |
| 1080p29.97, 15 Mbit/s | 31 | 52 | **75** | 2.4 | 18.3 M | walk without other GPU activity (72 to 75: 100 %); 77 held twice with the transcoder present but idle |
| 720p59.94, 12 Mbit/s | 34 | 59 | **90** | 2.6 | 19.4 M | bisection with the transcoder at 2 % (90: 100 %, 91: 98.2 %); over three minutes on the hot card 90 gives 98.3 % (§0.3) |
| 480i29.97 (704 wide), 4 Mbit/s | 176 | 292 | **400** | 2.3 | 15.8 M | walk without other GPU activity (400: 100 %, 420: 0 %, steps of 20); not re-measured cleanly after experiment 41 (a transcode was busy in every attempt; next to it 400 sessions held 86–95 % with the new binary, 41–75 % with the old) |
| mixed, units of 1 × 1080i + 1 × 720p + 4 × 480i | about 8 | 18 | **24** | 3 | 14.8 M | three minutes without any other process: 99.92 %; 25 and 26 units held 99 % in two of three runs each next to the transcoder |

At 99.9 %: 67 × 1080i, 75 × 1080p, 90 × 720p (25 s), 400 × 480i; the mixed line-up reached
99.93 % at 24 units in 25 s and 99.92 % over three minutes.

How these were obtained matters. A Plex playback session with a hardware transcode was open on
the card for most of the last two hours, and a transcode takes capacity away in bursts (§0.7).
The bisection of the final milestone (`final-realistic.json`, table below) ran next to it and
is marked contaminated in every run; its 1080i and 720p points fell into quiet stretches
(transcoder decoder at 2 %) and are limited by the encoder (mean tick 88 and 95 % of the
period); its 1080p, 480i and mixed points fell into busy ones and were too low. Those were
measured again with a walk that repeats every run during which another process used the
decoder or encoder (`scripts/capclean.py`, `final-clean.json`), which found clean runs for
480i and 1080p before the transcoder came back; the mixed figure rests on the clean
three-minute run. The figures are for the binary before the last change (experiment 37,
+1.6 to +2.1 % free-running), so they are slightly low. The disturbed bisection, for the
record:

| Format | cap99 before | cap99 now | gain | cap999 before | cap999 now | Mmb/s at cap99 now | Mean / p99 tick at cap99, ms | GPU busy | W | °C start→max | Clock min, MHz | Host CPU, cores | Tested N: on time |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1080i | 29 | **67** | +131 % | 29 | 67 | 16.39 | 29.3 / 30.2 | 86 % | 65 | 82→89 | 1890 | 0.16 | 62: 100.00 %, 65: 99.86 %, 66: 100.00 %, 67: 100.00 %, 68: 93.47 %, 74: 75.69 %, 86: 0.00 % |
| 1080p | 31 | **72** | +132 % | 31 | 71 | 17.61 | 28.9 / 33.2 | 81 % | 64 | 85→91 | 1920 | 0.17 | 68: 100.00 %, 71: 100.00 %, 72: 99.17 %, 73: 96.67 %, 74: 93.75 %, 80: 1.39 %, 93: 0.00 % |
| 720p | 34 | **90** | +165 % | 32 | 90 | 19.42 | 15.8 / 16.3 | 92 % | 68 | 84→91 | 1950 | 0.18 | 78: 100.00 %, 85: 100.00 %, 88: 100.00 %, 90: 100.00 %, 91: 98.19 %, 92: 84.36 %, 107: 0.00 % |
| 480i | 176 | **264** | +50 % | 152 | 260 | 10.44 | 21.1 / 33.0 | 59 % | 59 | 81→88 | 2145 | 0.14 | 216: 100.00 %, 244: 100.00 %, 256: 100.00 %, 260: 100.00 %, 264: 99.72 %, 268: 98.89 %, 272: 97.50 %, 340: 95.83 % |
| mixed | – | **24** | – | – | 23 | 14.85 | 14.4 / 16.2 | 84 % | 66 | 83→91 | 2100 | 0.18 | 20: 99.86 %, 22: 100.00 %, 23: 99.93 %, 24: 99.79 %, 25: 89.92 % |

- "mixed" is the line-up added to the benchmark in this phase: N units of 1 × 1080i +
  1 × 720p59.94 + 4 × 480i in one process on a 59.94 Hz tick (one unit is 618,579 macroblocks
  per second). Phase A measured it by hand: 9 + 7 + 32 sessions held 99.65 %, 10 + 8 + 36 did
  not, about 8 units.
- Milestone 1 (`m1-realistic.json`) was measured a third of the way through the phase with
  nothing else on the card: 47 × 1080i, 52 × 1080p, 59 × 720p, 292 × 480i, 18 mixed units.
- Free-running throughput (16 sessions; 96 for 480i; 8 units mixed; mean of three runs) [M]:

  | Run | 1080i, Mmb/s | 1080p, Mmb/s | 720p, Mmb/s | 480i, Mmb/s | mixed, Mmb/s | °C at start |
  |---|---|---|---|---|---|---|
  | `p0-realistic` | 7.75 | 8.08 | 7.80 | 7.71 | – | 48–67 |
  | `m1-realistic` | 12.34 | 12.76 | 12.73 | 11.99 | 11.67 | 56–84 |
  | `final-realistic` | 18.72 | 20.33 | 20.86 | 16.51 | 18.76 | 61–85 |
  | `final2-realistic` | 19.00 | 21.14 | 21.95 | 17.28 | 19.31 | 66–78 |
  | `e41-fence1/2` (upload fence, experiment 41; mean of two series) | 18.84 | – | 21.78 | 16.85 | 20.08 | 65–80 |

  `p0-realistic` is the phase-A binary measured cold at the start of this phase (its warm
  figures in §8.2 are 3 to 5 % lower; the mixed line-up with that binary: 6.77, `p0-mixed`);
  `m1-realistic` is the milestone after experiments 1 to 6, `final-realistic` the binary the
  capacity was measured with, `final2-realistic` the final binary (with experiment 37; a Plex
  process was present during both final runs). `e41-fence` is the current binary: the upload
  fence of experiment 41 changed nothing free-running (old binary in the same session, A/B/A/B:
  −0.8 % 1080i, −0.6 % 720p, +1.0 % 480i, +0.3 % mixed, all within the scatter). Free-running, the final binary does 2.2 to
  2.9 times the macroblocks of phase A.
- Quality at the nominal rates went up slightly (§0.5): +0.06 dB luma PSNR on average against
  phase A, every one of the 22 standard cases better, `mobcal` +0.45 dB. The buffer model and
  the rate change mechanism pass every gate case as before (54 of 54).
- The same line-up now loads the card less than half as much (§0.6): 16 × 1080i took 47 % of
  the SMs and 48 W, now 19 to 20 % and 27 to 31 W; 8 mixed units 67 % and 56 W, now 25 % and
  30 W.
- Incidents on `nixark`: none. No out-of-memory event, no failed unit, no process left running, nothing changed outside
  `~/totalblonder-spike` (checked at the end: `systemctl --failed` empty, no OOM line in the
  kernel log since the start). The power limit of the card was lowered for the experiments of
  §0.6 and is back at its default of 70 W (read back). One thing to know: from about 06:30 a
  Plex playback session was transcoding on the same card while benchmarks ran. The benchmarks
  were disturbed by it (§0.7); whether the playback was disturbed by the benchmarks was not
  checked (no service or log of Plex was touched). Experiment 41 (upload fence, several
  transcodes at once): no incident either; the same checks at its end were clean (no process
  left, no failed unit, no OOM line, power limit 70 W, timeslice Default). Up to three ffmpeg
  competitors ran at once inside the 10 GB cap; two live Plex transcodes were on the card during
  much of it.

### 0.2 What was changed, with the gain of each change

Measured free-running at 16 sessions (96 for 480i), each against the build just before it,
A/B/A where the gain is small. "Identical" means the coded streams are byte-identical to the
build before (or to the phase-A binary), so quality cannot have changed. The numbers refer to
the experiments in the log.

| # | Change | 1080i | 720p | 480i | Output |
|---|---|---|---|---|---|
| 1 | `k_me`: frame search on packed pixels (32-bit loads, `__vsadu4`, `__vavgu4`, exact packed four-sample average), no per-thread pixel arrays | +32.2 % | +35.7 % | +33.2 % | identical |
| 2 | `k_me` launched in 64-thread blocks (occupancy is limited by registers in whole blocks) | +4 % | +2.5 % | – | identical |
| 5 | Slice state per macroblock instead of one thread per slice (`k_ent_resolve`, `k_ent_pack` walk left) | +3.7 % | – | +0.6 % | identical |
| 6 | Upload straight into padded planes (`k_pad` gone); all batches go through each phase together with three waits per tick; one memset and one download per batch | +14.5 % | +15.0 % | +16.7 % | identical |
| 8 | `k_mbstat` and `k_down2` on packed pixels | +5.6 % | +7.5 % | +5.2 % | identical |
| 9 | `k_xf`: prediction from packed words | +8.3 % | +9.8 % | +4.5 % | identical |
| 10 | Sparse levels: only non-zero levels stored, with a position mask that `k_ent_pack` and `k_recon` iterate | +14.4 % | +17.9 % | +11.8 % | identical |
| 13 | Sparse coefficients behind a per-picture quantizer floor (`k_xf` stores only what can survive; the transform is repeated when the rate control goes below the floor, 1 to 2 % of pictures) | +5.6 % | +1.4 % | +1.8 % | identical |
| 17 | Temporal candidate in the motion search (`--tpred`) | −1.3 % | −1.7 % | −1.3 % | +0.067 dB on average (§0.5) |
| 19 | Hybrid wait for the GPU (sleep most of the expected time, then poll) | +1.6 % | +4.2 % | +1.7 % | identical; 0.15 host core more |
| 20 | Field search helpers without variable word indices (no local-memory arrays) | +5.1 % | – | +4.2 % | identical |
| 22 | One exact half-sample fetch for the smoothing pass and the bidirectional field SAD | +0.7 % | +1.2 % | +0.7 % | −0.006 dB on average |
| 24 | `f_code` sized for the scaled field seed (bug at `--range` above 9; the default was not affected) | 0 | 0 | 0 | default streams identical |
| 27 | 32-bit index arithmetic in the block kernels | +0.4 % | +1.2 % | 0 | identical |
| 29 | `k_recon` with one thread per block, packed rows | +1.7 % | +1.2 % | +0.8 % | identical |
| 30 | Row statistics and slice prefix sum with one block per row (warp shuffles) | +1.6 % | +2.3 % | +1.2 % | identical |
| 37 | `k_ent_resolve` writes a 24-byte record instead of 40 bytes; both pyramid levels in one kernel (`k_pyr`) | +1.8 % | +2.1 % | +1.6 % | identical |

For the mixed line-up the structural changes count most: experiments 2 and 5 together
+12.8 %, experiment 6 +17.9 %, experiments 29 and 30 together +4.3 % [M]. The gains do not
simply multiply to the totals of §0.1: they were measured at different temperatures over
three hours, and the temporal candidate costs 1.3 to 1.7 %.

Why the mixed line-up was a quarter slower than a single format in phase A, as traced with
Nsight Systems (`results/nsys/p2-mixed-tick.txt`) [M]: (a) one thread per slice in
`k_ent_resolve` took 0.2 to 0.7 ms per batch whatever the batch held, 17 % of the kernel time;
(b) each batch synchronized three times, 10 waits and 0.9 ms of idle GPU per 9.5 ms tick;
(c) `k_pad` competed with the search; (d) the benchmark counted the two ticks after the end of
the input, in which the 29.97 Hz sessions flush in every 59.94 Hz tick. With (a) to (c)
removed and (d) left out of the statistics, a tick costs 0.29 ms plus 0.052 ms per 1000
macroblocks whatever their format and picture type (fit over a mixed run, experiment 16).

### 0.3 Sustained load at the capacity point

One paced run of three minutes per format family at the capacity found above, with the
benchmark of this phase; the first row is phase A's sustained run for comparison [M]:

| Run | Line-up | Seconds | On time | Mean / p99 / p99.9 tick, ms | W mean (max) | °C start→max | Clock min, MHz | GPU busy | Host CPU, cores | Other GPU process |
|---|---|---|---|---|---|---|---|---|---|---|
| `sustain-1080i` | 29 × 1080i | 180 | 99.94 % | 32.2 / 33.0 / 33.2 | 68 (69) | 81→91 | 1980 | 94 % | 0.02 | no |
| `final-sustain-1080i` | 67 × 1080i | 180 | 100.00 % | 29.1 / 30.3 / 30.7 | 66 (67) | 62→90 | 1890 | 85 % | 0.16 | no |
| `final-sustain-720p` | 90 × 720p | 180 | 98.30 % | 15.9 / 16.5 / 19.4 | 68 (69) | 83→91 | 1965 | 92 % | 0.19 | no |
| `final-sustain-480i` | 264 × 480i | 180 | 100.00 % | 20.0 / 21.0 / 21.9 | 59 (60) | 84→90 | 2070 | 56 % | 0.14 | no |
| `final-sustain-mixed` | mixed × 24 (24 + 24 + 96) | 180 | 99.92 % | 14.3 / 15.1 / 15.4 | 67 (67) | 81→91 | 2040 | 84 % | 0.18 | no |

- 1080i: all 5,365 ticks of three minutes on time with 67 sessions, at 66 W and 90 °C, the
  clock down to 1890 MHz. Phase A's run held 29 sessions at 68 W and 91 °C.
- 720p: 90 sessions held 100 % in the 25 s bisection run and 98.3 % over three minutes (182
  late ticks of 10,730): the
  mean tick is 95 % of the period, and when the clock sinks to 1965 MHz at 91 °C the 99th
  percentile (16.5 ms) reaches the period (16.7 ms). The sustained 720p capacity on this
  card as it is cooled is a few sessions lower, about 86 to 88 [E]; not bisected.
- 480i: 264 sessions use 60 % of the tick period (see §0.1).
- Mixed: 24 units (24 × 1080i + 24 × 720p + 96 × 480i, 14.8 M macroblocks/s): 9 late ticks in
  10,730.
- No other process was on the card during these four runs (`pmon` recorded none).
- Host CPU at capacity is 0.14 to 0.19 core (0.02 to 0.06 in phase A): the hybrid wait polls
  for the last part of each wait (`--blocking-wait`: back to about 0.07, 2 to 4 % slower).
- The card is at its thermal limit in every one of these runs (90–91 °C, 59 to 68 W of 70 W).

### 0.4 Where the time goes now

Stage times, `final2-realistic` (ms per tick, profile mode: synchronized after every stage, the upload inside its own stage; N = 16 × 1080i, 16 × 1080p, 16 × 720p, 96 × 480i, 8 × mixed):

| Stage | 1080i | share | 1080p | share | 720p | share | 480i | share | mixed | share |
|---|---|---|---|---|---|---|---|---|---|---|
| pyramid+ME+mbstat | 2.58 | 40.2 % | 2.04 | 34.9 % | 0.96 | 35.3 % | 2.44 | 34.5 % | 1.76 | 35.2 % |
| pred+dct+stats | 2.04 | 31.8 % | 2.03 | 34.6 % | 0.91 | 33.3 % | 2.12 | 29.9 % | 1.38 | 27.8 % |
| rate control (host) | 0.04 | 0.7 % | 0.04 | 0.8 % | 0.03 | 1.2 % | 0.14 | 2.0 % | 0.11 | 2.3 % |
| quant+entropy | 0.88 | 13.7 % | 0.88 | 15.0 % | 0.43 | 15.9 % | 1.29 | 18.3 % | 0.73 | 14.6 % |
| scatter | 0.13 | 2.0 % | 0.13 | 2.2 % | 0.08 | 2.8 % | 0.15 | 2.1 % | 0.27 | 5.4 % |
| recon+pyr | 0.52 | 8.1 % | 0.51 | 8.7 % | 0.20 | 7.2 % | 0.58 | 8.3 % | 0.46 | 9.2 % |
| download | 0.14 | 2.1 % | 0.13 | 2.3 % | 0.09 | 3.1 % | 0.18 | 2.5 % | 0.20 | 4.1 % |
| host | 0.09 | 1.4 % | 0.09 | 1.5 % | 0.03 | 1.2 % | 0.18 | 2.5 % | 0.07 | 1.5 % |
| total without the upload | 6.43 | | 5.85 | | 2.72 | | 7.08 | | 4.98 | |
| upload (overlapped in normal operation) | 3.83 | | 3.83 | | 1.74 | | 3.80 | | 2.53 | |

One tick of 16 × 1080i on the timeline (`results/nsys/final-1080i-tick.txt`, final binary)
[M]: 6.88 ms (median 6.80), kernels busy 93 %, 0.37 ms idle. In a median tick: `k_xf` 2.03 ms
(plus 0.14 for one repeated transform), `k_me` 1.92, `k_mvsmooth` 0.44, `k_quant` 0.41,
`k_recon` 0.35, `k_pyr` 0.28 in two launches, `k_ent_pack` 0.26, `k_mbstat` 0.19,
`k_ent_resolve` 0.08, `k_rowstat` 0.08, `k_ent_scatter` 0.06. In phase A the same tick was
17.7 ms with `k_me` 6.5 ms, `k_xf` 2.7, `k_quant` 1.3, `k_pad` 1.25, `k_ent_pack` 0.9,
`k_ent_resolve` 0.7 (§8.4). The mixed line-up at 8 units: 4.28 ms per tick (median 4.13),
kernels busy 88 %, 0.23 ms idle (`final-mixed-tick.txt`); it was 12.05 ms.

Hardware counters (Nsight Compute, 4 × 1080i, staggered, 80 launches from the steady part;
`results/ncu/final-*`; the binary before experiment 37, so with `k_down2` instead of
`k_pyr`) [M]:

Per kernel, `final` (averages per launch; kernels with at least 2 % of the profiled kernel time):

| Metric | `k_me` | `k_xf` | `k_mvsmooth` | `k_recon` | `k_quant` | `k_down2` | `k_ent_pack` | `k_ent_resolve` | `k_mbstat` |
|---|---|---|---|---|---|---|---|---|---|
| Launches profiled | 4 | 6 | 4 | 4 | 4 | 16 | 4 | 4 | 4 |
| Time per launch, ms | 0.533 | 0.348 | 0.109 | 0.100 | 0.098 | 0.021 | 0.082 | 0.053 | 0.044 |
| Share of kernel time | 31.3 % | 30.7 % | 6.4 % | 5.9 % | 5.8 % | 4.9 % | 4.8 % | 3.1 % | 2.6 % |
| Registers per thread | 168 | 54 | 80 | 118 | 37 | 22 | 48 | 36 | 48 |
| Achieved occupancy | 23.2 % | 63.1 % | 40.9 % | 26.1 % | 71.8 % | 83.4 % | 51.3 % | 61.6 % | 76.1 % |
| Issue slots busy | 45.8 % | 61.4 % | 26.5 % | 44.7 % | 34.6 % | 27.3 % | 31.4 % | 3.1 % | 10.4 % |
| Warp instructions, millions | 43.87 | 40.51 | 4.85 | 7.65 | 5.59 | 1.00 | 3.47 | 0.26 | 0.76 |
| Warp cycles per issued instruction | 6.16 | 12.66 | 19.28 | 7.33 | 26.78 | 37.81 | 21.58 | 245.55 | 88.66 |
| Threads per warp instruction | 29.6 | 27.8 | 10.4 | 14.2 | 19.5 | 32.0 | 9.3 | 27.5 | 32.0 |
| Not predicated off | 29.2 | 24.9 | 9.9 | 12.5 | 18.1 | 29.0 | 8.3 | 25.4 | 29.9 |
| Branch efficiency | 83.5 % | 70.9 % | 58.0 % | 85.3 % | 75.9 % | 100.0 % | 75.5 % | 77.8 % | 100.0 % |
| Divergent branches | 1306 | 6539 | 920 | 163 | 799 | 0 | 760 | 28 | 0 |
| Eligible warps per scheduler | 0.75 | 1.48 | 0.39 | 0.89 | 0.55 | 0.56 | 0.63 | 0.08 | 0.17 |
| Global loads, M | 2.85 | 1.17 | 0.32 | 0.11 | 0.18 | 0.05 | 0.18 | 0.01 | 0.07 |
| Global stores, M | 0.01 | 0.24 | 0.01 | 0.05 | 0.05 | 0.01 | 0.02 | 0.02 | 0.00 |
| Local memory instr., M | 0.12 | 0.16 | 0.00 | 0.00 | 0.00 | 0.00 | 0.01 | 0.00 | 0.03 |
| Shared memory instr., M | 0.00 | 0.37 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 | 0.00 |
| Memory throughput | 62.1 % | 54.5 % | 68.7 % | 52.6 % | 61.7 % | 78.8 % | 35.8 % | 71.6 % | 88.1 % |
| L1 hit | 82.7 % | 66.2 % | 81.9 % | 73.7 % | 51.2 % | 52.7 % | 81.5 % | 90.5 % | 68.1 % |
| L2 hit | 89.8 % | 74.5 % | 30.9 % | 58.5 % | 49.8 % | 21.4 % | 45.5 % | 94.9 % | 32.9 % |
| DRAM read, MB | 28.32 | 28.17 | 15.89 | 9.71 | 10.68 | 3.76 | 5.44 | 1.01 | 8.37 |
| DRAM written, MB | 3.60 | 9.91 | 0.34 | 2.38 | 2.43 | 0.07 | 0.93 | 0.00 | 0.00 |

Warp states, `final`: average number of warps in each state per cycle in which an instruction is issued. The column sum is the warp cycles per issued instruction; in brackets the share of PC samples in that state.

| State | `k_me` | `k_xf` | `k_mvsmooth` | `k_recon` | `k_quant` | `k_down2` | `k_ent_pack` | `k_ent_resolve` | `k_mbstat` |
|---|---|---|---|---|---|---|---|---|---|
| Long scoreboard (waiting for an L1TEX result: global or local memory) | 2.20 (36.1 %) | 4.32 (30.7 %) | 15.13 (78.4 %) | 3.22 (45.0 %) | 20.34 (75.1 %) | 29.21 (80.6 %) | 13.45 (65.7 %) | 52.23 (17.9 %) | 71.36 (84.9 %) |
| Short scoreboard (shared memory, other short-latency units) | 0.01 (0.1 %) | 0.28 (2.5 %) | 0.02 (0.1 %) | 0.08 (1.1 %) | 0.66 (2.7 %) | 0.55 (1.2 %) | 0.15 (0.8 %) | 0.43 (0.1 %) | 0.13 (0.1 %) |
| LG throttle (local/global memory queue full) | 0.16 (2.6 %) | 0.20 (0.7 %) | 0.02 (0.1 %) | 0.04 (0.5 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 1.05 (2.2 %) | 170.49 (71.0 %) | 12.87 (8.5 %) |
| MIO throttle (shared-memory and other MIO queue full) | 0.00 (0.0 %) | 0.46 (3.0 %) | 0.00 (0.0 %) | 0.00 (0.1 %) | 0.11 (0.3 %) | 0.27 (0.5 %) | 0.02 (0.1 %) | 0.48 (0.1 %) | 0.12 (0.1 %) |
| Math pipe throttle | 0.60 (9.1 %) | 0.62 (5.4 %) | 0.43 (2.0 %) | 0.73 (9.0 %) | 0.41 (1.2 %) | 1.01 (2.1 %) | 0.72 (2.3 %) | 0.93 (0.2 %) | 0.32 (0.3 %) |
| Wait (fixed-latency dependency) | 1.11 (18.5 %) | 1.76 (15.2 %) | 1.40 (7.3 %) | 0.90 (13.0 %) | 2.69 (11.0 %) | 2.21 (5.6 %) | 2.55 (13.0 %) | 2.26 (0.7 %) | 1.34 (2.0 %) |
| Not selected (eligible, another warp issued) | 0.64 (10.0 %) | 1.40 (11.9 %) | 0.46 (2.3 %) | 1.00 (12.8 %) | 0.61 (1.8 %) | 1.07 (2.3 %) | 1.01 (3.2 %) | 1.67 (0.5 %) | 0.66 (0.6 %) |
| Selected (issuing) | 1.00 (16.6 %) | 1.00 (8.5 %) | 1.00 (5.5 %) | 1.00 (14.1 %) | 1.00 (4.2 %) | 1.00 (2.9 %) | 1.00 (5.2 %) | 1.00 (0.3 %) | 1.00 (1.8 %) |
| No instruction (fetch) | 0.22 (3.5 %) | 0.57 (5.2 %) | 0.19 (0.9 %) | 0.05 (0.5 %) | 0.16 (0.6 %) | 0.20 (0.6 %) | 0.44 (1.9 %) | 0.41 (0.1 %) | 0.12 (0.1 %) |
| Branch resolving | 0.10 (1.8 %) | 0.78 (6.9 %) | 0.45 (2.4 %) | 0.08 (1.1 %) | 0.48 (2.2 %) | 0.06 (0.3 %) | 0.66 (3.5 %) | 0.43 (0.2 %) | 0.01 (0.1 %) |
| Barrier (`__syncthreads`) | 0.00 (0.0 %) | 1.01 (7.9 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) |
| Dispatch stall | 0.08 (1.2 %) | 0.16 (1.3 %) | 0.04 (0.2 %) | 0.08 (1.2 %) | 0.13 (0.4 %) | 0.46 (1.2 %) | 0.20 (0.8 %) | 0.27 (0.1 %) | 0.11 (0.2 %) |

Reading:

- **`k_me`, 31 %**: 0.53 ms per launch for 4 sessions (2.01 in phase A): 44 M warp instructions
  (130 M), 0.12 M of them on local memory (14.3 M); 168 registers, 23 % occupancy, issue slots
  46 % busy; 36 % of its warp samples wait for loads, 18 % for fixed-latency results.
- **`k_xf`, 31 %**: 0.35 ms on average over six launches, two of which are repeated transforms
  of single sessions; a full launch is about 0.5 ms (0.76 in phase A). 28 MB read and 10 MB
  written per launch on average (33 and 24); 31 % of
  samples wait for loads, 8 % at the one remaining barrier, 7 % resolving branches (71 % branch
  efficiency: the four blocks of a warp have different modes).
- **`k_mvsmooth`, 6 %**: 78 % waiting for loads, 10.4 of 32 threads active.
- **`k_recon`, 6 %** (anchors only): 118 registers, 7.3 cycles per instruction.
- **`k_quant`, 6 %**: 0.10 ms (0.29): 75 % waiting for loads; reads 10.7 MB (37.7).
- **`k_down2`, 5 %** in 16 small launches: 81 % waiting for loads.
- **`k_ent_pack`, 5 %**: 0.08 ms (0.34). **`k_ent_resolve`, 3 %**: 0.05 ms (0.79, and it no
  longer depends on the row length); 71 % of its samples are throttled on the memory queue:
  it stored a 40-byte record per macroblock (24 bytes since experiment 37, which came after
  this profile, as did the single pyramid kernel that replaces `k_down2`).
- Everything that is left waits for memory. Nothing is limited by arithmetic or by barriers.

### 0.5 Quality

Gate corpus, 22 standard cases at the nominal rates: luma PSNR and SSIM of phase A (the gate
baseline of that phase, `results/check/baseline-phaseA.json`), of the final binary
(`results/check/final.json`) and of ffmpeg `mpeg2video` at the same rate [M]:

| Case | Mbit/s | PSNR Y phase A | PSNR Y now | change | SSIM now (change) | ffmpeg PSNR Y / SSIM | now − ffmpeg, PSNR Y | now − ffmpeg, SSIM | phase A − ffmpeg |
|---|---|---|---|---|---|---|---|---|---|
| 1080i-crowd_run | 15.23 | 26.91 | 26.94 | +0.04 | 0.7808 (+0.0012) | 27.30 / 0.7908 | -0.35 | -0.0100 | -0.39 |
| 1080i-pedestrian_area | 15.23 | 37.97 | 38.02 | +0.05 | 0.9444 (+0.0005) | 38.76 / 0.9517 | -0.74 | -0.0072 | -0.79 |
| 1080i-park_joy | 15.24 | 24.38 | 24.43 | +0.04 | 0.7120 (+0.0016) | 24.84 / 0.7203 | -0.41 | -0.0083 | -0.46 |
| 1080i-fourpeople | 15.23 | 42.91 | 42.92 | +0.01 | 0.9782 (+0.0000) | 43.24 / 0.9787 | -0.32 | -0.0005 | -0.33 |
| 1080i-cuts | 15.23 | 29.42 | 29.45 | +0.03 | 0.8700 (+0.0007) | 30.78 / 0.8889 | -1.33 | -0.0190 | -1.36 |
| 1080i-fade | 15.23 | 26.79 | 26.84 | +0.04 | 0.7979 (+0.0012) | 27.13 / 0.8060 | -0.29 | -0.0080 | -0.34 |
| 1080i-grain | 15.23 | 33.63 | 33.65 | +0.02 | 0.7976 (+0.0004) | 33.87 / 0.8042 | -0.23 | -0.0066 | -0.24 |
| 480i-crowd_run | 4.05 | 26.37 | 26.41 | +0.04 | 0.8151 (+0.0014) | 26.79 / 0.8230 | -0.38 | -0.0079 | -0.42 |
| 480i-football | 4.06 | 34.96 | 35.02 | +0.06 | 0.9347 (+0.0006) | 35.63 / 0.9384 | -0.60 | -0.0037 | -0.67 |
| 480i-mobcal | 4.06 | 26.72 | 27.17 | +0.45 | 0.8971 (+0.0080) | 28.04 / 0.9007 | -0.86 | -0.0037 | -1.31 |
| 480i-cuts | 4.05 | 29.48 | 29.52 | +0.04 | 0.8945 (+0.0013) | 30.45 / 0.9055 | -0.93 | -0.0110 | -0.96 |
| 480i-testsrc2 | 4.05 | 45.60 | 45.62 | +0.02 | 0.9950 (-0.0000) | 44.93 / 0.9925 | +0.69 | +0.0025 | +0.66 |
| 480i-park_joy | 4.06 | 25.28 | 25.36 | +0.08 | 0.7830 (+0.0026) | 25.79 / 0.7933 | -0.44 | -0.0102 | -0.51 |
| 720p-crowd_run | 12.18 | 27.72 | 27.79 | +0.07 | 0.8282 (+0.0020) | 28.38 / 0.8400 | -0.60 | -0.0118 | -0.66 |
| 720p-fourpeople | 12.17 | 41.68 | 41.69 | +0.01 | 0.9763 (+0.0001) | 42.68 / 0.9785 | -0.99 | -0.0022 | -1.00 |
| 720p-park_joy | 12.18 | 26.03 | 26.12 | +0.09 | 0.7907 (+0.0028) | 26.91 / 0.8075 | -0.79 | -0.0167 | -0.88 |
| 720p-cuts | 12.18 | 30.88 | 30.94 | +0.05 | 0.9041 (+0.0007) | 31.64 / 0.9138 | -0.71 | -0.0097 | -0.76 |
| 720p-fade | 12.18 | 28.19 | 28.27 | +0.09 | 0.8508 (+0.0022) | 28.92 / 0.8629 | -0.65 | -0.0121 | -0.74 |
| 720p-still | 9.88 | 33.79 | 33.82 | +0.03 | 0.9504 (+0.0002) | 33.63 / 0.9476 | +0.19 | +0.0028 | +0.16 |
| 1080p-crowd_run | 15.22 | 28.63 | 28.67 | +0.04 | 0.8222 (+0.0010) | 29.15 / 0.8324 | -0.48 | -0.0101 | -0.52 |
| 1080p-pedestrian_area | 15.23 | 40.11 | 40.12 | +0.02 | 0.9565 (+0.0001) | 40.43 / 0.9593 | -0.30 | -0.0028 | -0.32 |
| 1080p-cuts | 15.23 | 31.31 | 31.34 | +0.03 | 0.8983 (+0.0004) | 32.63 / 0.9108 | -1.29 | -0.0125 | -1.32 |
| mean of 22 | | | | +0.061 | | | -0.54 | | -0.60 |

- torture cases (19): luma PSNR change against phase A +0.057 dB on average, from -0.12 (1080i-cuts-quarter) to +0.85 (720p-cuts-quarter); outside the gate's tolerance against the phase-A baseline (0.10 dB in any plane, 0.002 SSIM): 1080i-cuts-quarter, 720p-cuts-quarter, 1080p-noise-quarter.
- feature cases (12): luma PSNR change against phase A +0.031 dB on average, from -0.05 (720p-cuts-ratewalk) to +0.06 (720p-crowd_run-forcekey); outside the gate's tolerance against the phase-A baseline (0.10 dB in any plane, 0.002 SSIM): none.

- Against phase A every standard case is better; the gain comes from the temporal candidate
  (experiment 17: +0.067 dB) less experiment 22 (−0.006 dB). All other kept changes leave the
  streams byte-identical.
- Against ffmpeg the gap is now 0.2 to 1.3 dB. Adaptive quantization accounts for 0.48 dB of
  it on average by design (`--aq 0`: +0.48 dB PSNR, SSIM the same; ffmpeg's default has
  none). `mobcal` improved from −1.31 to about −0.86 dB. The `cuts` cases stay 0.7 to 1.3 dB
  behind for a structural reason: ffmpeg puts a key frame at a scene cut and restarts its
  GOP, while this encoder keeps a fixed GOP (SPEC §8.2) and therefore codes an intra P picture
  at the cut and the scheduled I picture soon after.
- The torture cases at a quarter of the nominal rate are coded on the buffer ladder (pictures
  without coefficients), and their PSNR moves with any change of the vectors: in the final
  binary between −0.12 and +0.85 dB of luma against phase A (list above; `cuts` 720p at
  3 Mbit/s gains 0.85 dB of luma and loses 0.25 dB in one chroma plane). No case loses more
  than 0.2 dB of luma. With the temporal candidate alone, before experiment 22, `cuts` 480i at
  1 Mbit/s had lost 0.50 dB (log, experiment 17); `--tpred 0` gives the old search.
- The gate baseline was updated to the final binary at the end (`baseline.json`); the phase-A
  baseline is kept as `baseline-phaseA.json`. `scripts/qdiff.py` prints the difference of a
  gate run to the baseline.

Speed and quality knobs (experiment 24; paired free runs at 16 × 1080i; 22 standard cases) [M]:

| Setting (default) | Luma PSNR, mean of 22 cases against the default | Worst case against phase A | Throughput |
|---|---|---|---|
| `--tpred 0` (1) | −0.067 dB (`mobcal` −0.45) | −0.13 dB (`still` 720p) | +1.3 % |
| `--smooth 0` (1) | −0.025 dB | −0.07 dB (`testsrc2` 480i) | +6.1 % |
| `--field-pred 0` (1) | −0.084 dB | −0.28 dB (`testsrc2` 480i) | +10 % |
| `--range 4`, ±19 pixels (8, ±35) | −0.052 dB | −0.97 dB (`testsrc2` 480i) | +1 %, inside the noise |
| `--range 2`, ±11 pixels | −0.53 dB | −3.5 dB (`pedestrian_area` 1080i) | about +4 % |
| `--range 12`, ±51 pixels | 0.00 dB | −0.04 dB (`crowd_run` 1080i) | −5.5 % |
| `--aq 0` (1) | +0.48 dB PSNR, SSIM +0.0000 | +0.22 dB | 0 |
| `--blocking-wait` (hybrid wait) | none | none | −2 to −4 %; 0.15 host core less |
| `--spin` | none | none | +1 %; one host core |

The search range is not a cheap knob: ±11 pixels cannot follow P pictures three frames apart.
Phase A's "±11 pixels cost no PSNR" (§13.3) came from the first spike's two clips and does not
hold on this corpus.

### 0.6 Load on the card at a fixed line-up, and the power cap

Paced at real time for 60 s, realistic workload, the phase-A binary against the binary of
this phase; `nvidia-smi dmon` once per second (`results/bench/load-*.json`) [M]:

| Line-up | Binary | Mean tick, ms (period) | GPU utilization | Power, W | Temperature start → max, °C | Clock, MHz |
|---|---|---|---|---|---|---|
| 16 × 1080i | phase A | 16.30 (33.4) | 46.8 % | 48.1 | 61 → 76 | 2130 |
| 16 × 1080i | after experiment 22 | 6.97 | 20.0 % | 27.3 | 53 → 62 | 2130 |
| 16 × 1080i | the same, right after the phase-A run | 6.99 | 20.4 % | 29.4 | 76 → 76 | 2130 |
| 16 × 1080i | final | 6.72 | 19.3 % | 30.6 | 84 → 84 (hot from the sustained runs) | 2130 |
| mixed × 8 (8 + 8 + 32) | phase A | 11.98 (16.7) | 67.2 % | 56.4 | 65 → 83 | 2430 |
| mixed × 8 | after experiment 22 | 4.47 | 26.1 % | 29.9 | 66 → 68 | 2130 |
| mixed × 8 | final (a Plex transcode active on the card) | 4.25 | 24.7 % | 32.2 | 74 → 78 | 2130 |

All ticks were on time in all seven runs; no other GPU process in the first six. An idle card
draws 6.5 W: the same 16 × 1080i now costs 21 to 24 W above idle where it cost 42 W, and 8
mixed units 23 W where they cost 50 W. In its minute the phase-A binary heated the card by
15 and 18 °C, the new one by 9 and 2 °C. The busy share of the SMs fell by a factor of 2.3 to
2.7; the watts above idle by 1.8 to 2.2 (a card that works less also clocks down less
efficiently: 2130 MHz in both cases).

Power cap (`sudo nvidia-smi -pl`, default 70 W, range 30 to 70 W; the limit was read before,
restored after each series and read back: 70 W) [M]:

| Cap | Free run, 16 × 1080i, Mmb/s (two series) | Clock, MHz | Fixed line-up 16 × 1080i paced: tick ms, W |
|---|---|---|---|
| 70 W (default) | 18.05, 18.70 | 2040–2070 | 6.73 ms, 30.7 W |
| 60 W | 16.91, 17.55 (−6 %) | 1830–1860 | – |
| 50 W | 15.21, 15.68 (−16 %) | 1515–1575 | 6.87 ms, 29.4 W |
| 40 W | 10.05 (−44 %) | 855–870 | 6.86 ms, 29.3 W |

- A cap lowers the clock and with it the peak throughput, by more than it lowers the power:
  free-running the encoder draws about 51 W uncapped, yet a 60 W cap already costs 6 %.
- At a fixed line-up below the cap it changes nothing: the power follows the work (30.7, 29.4
  and 29.3 W under caps of 70, 50 and 40 W, the clock unchanged at 2130 MHz).
- So a cap is of no use here; less load on the card comes from doing less work per macroblock.
  (A Plex session was open during these runs; the transcoder was idle in all free runs but two
  of the 40 W series and active in the three fixed-line-up runs, whose watts include it. The
  watt figures of the 6 s free runs are from one or two samples each and are not given.)
- Not tested: whether a cap near the sustained draw (66 to 68 W at capacity) would keep the
  card under 90 °C and its clock steadier over minutes; the runs it needs were not possible
  next to the transcode.

### 0.7 Next to a Plex transcode

(Since work package C the inputs are decoded on the card as well, and the shared decoder, not
the kernels, decides how many channels fit next to Plex: §C.5 has the measurements and the
planning table for that case. What follows is about the encoder's kernels and still holds;
mitigation 3 below, the deadline-miss handling, is built: §C.7.)

Phase B saw the encoder share the card with Plex from about 06:30 on: a playback session with a
hardware transcode (decoder and encoder of the card, a CUDA context of 165 to 290 MB), later two.
Three experiments looked at what that costs: 32 (observation), 40 (synthetic competitors, one at a
time) and 41 (a fix, and up to three competitors at once). State after experiment 41 [M unless
marked]:

- **A real-time transcode costs little**: 0 to 6 % of the encoder's time per transcode without
  tone mapping, 12 % for a 2160p tone-mapped one. Three at once cost about the sum.
- **An unthrottled transcode costs a share of the time**: 23 to 27 % for 1080p sources, up to
  half (tick ratio 1.93 to 2.07) for a 2160p source. Plex runs its transcoder faster than real
  time in bursts (1 to 3 s every 10 to 20 s on 1080p sources, experiment 32), so these bursts
  are the case to plan for.
- **The 60 % slowdown of experiment 32 at 300 × 480i is gone.** Most of it was the encoder's own
  upload being serialized with its kernels (below). Fixed in experiment 41: next to the same
  competitors, 300 × 480i went from ratios 1.6–1.8 to 1.1–1.4, the isolated slow ticks
  disappeared, and 480i now behaves like 1080i.

#### Why sharing hurts

From experiments 40 and 41 (`scripts/contend.sh`, `contend_report.py`, `contend_table.py`,
`contend_late.py`, `src/gpucomp.cu`, `results/contend/`). The Plex session's command is
`h264 nvdec → scale_cuda → h264_nvenc`: a CUDA context with a compute kernel per frame, not only
fixed-function blocks.

- **Time-slicing of the compute engine between processes: the base mechanism.** Kernels of two
  processes never run at the same time, however small (NVIDIA, MPS documentation: work "from
  work queues belonging to different CUDA contexts cannot execute concurrently"). A second
  process whose back-to-back kernels use a fraction of one SM makes the ticks 1.11 to 1.14 times
  as long (1.46 to 1.93 with 1 ms kernels, experiment 40). The slice is about 2 ms. An idle
  context costs nothing, 60 tiny kernels per second 0 to 4 %.
- **Real compute work of the transcoder, in proportion to its frame rate.** The driver's side of
  NVDEC and NVENC runs in a CUDA context, so every frame a transcoder moves costs the encoder
  time: about 0.15 ms per 1080p decode, 0.25 ms per 1080p decode + `scale_cuda` + encode, 1.2 to
  2 ms per 2160p decode + scale + encode, 3.9 ms per 2160p tone-mapped frame [E, from ratio and
  frame rate]. At real time these add up across transcodes (three 1080p: 2–3 %; one 1080p and
  two 2160p: 12–13 %; two 2160p and one tone-mapped: 18–28 %); no extra cost per additional
  context was seen.
- **Unthrottled transcodes share the video engines, so their cost does not add up.** Three
  unthrottled 1080p transcodes run at a third of the rate of one each and cost the same 1.26 as
  one. The ratio is set by the heaviest pipeline present; the worst seen is a single 2160p
  transcode, about 2.0 (the encoder gets half the time). Mixing in 1080p ones lowers it (2 ×
  1080p + 1 × 2160p 1.47; 1 × 1080p + 2 × 2160p 1.77). Tone mapping unthrottled: 1.41–1.43
  alone, 1.6–2.0 with others.
- **Our own upload, serialized: was most of the 60 %; fixed (experiment 41).** The overlapped
  upload of the next tick's frames (one copy per session) and the tick's parameter copies (on
  the kernel stream, queued first) share the copy engine. Next to a busy second context, and
  only above 256 copies or with one large copy, the copy engine ran the whole upload first; the
  first kernel waited for its parameters, i.e. for the upload (12 ms at 300 × 480i). Fewer
  copies are *not* the cure: one 152 MB copy per batch was serialized the same way (ratio 1.78),
  and the batched-copy API changed nothing (1.59). The cure is the order: the upload stream now
  waits on an event recorded after the parameter copies. Next to back-to-back kernels 300 ×
  480i went from 1.62 to 1.14, next to Plex's pipeline unthrottled from 1.69 to 1.23 and in real
  time from 1.07 (76 % of ticks on time) to 1.01 (100 %); tone mapping unthrottled 1.77 → 1.41,
  real time 1.29 (64 %) → 1.12 (100 %). Single slow ticks: 41–43 per run before, none after.
  The timeline (`results/nsys/contend-fence-tinycont300-tick.txt`) shows `k_pyr` 0.5 ms into the
  tick with the copies beside the kernels. Free-running without competitors: no change. Why the
  copy engine reorders and why at 256 is not documented by NVIDIA (the driver API's
  synchronization page only says calls may wait on "internal resources"; forum reports describe
  similar cross-stream copy stalls); the fix does not depend on it.
- **Power: a small part, near capacity only.** At half load the clock rises with a competitor;
  near capacity the pair reaches 65–68 W and the clock falls 5 to 12 %, the difference between
  the half-load and near-capacity ratios (1.19 → 1.27 for Plex's pipeline). No thermal
  violation in any run.
- **Memory bandwidth, PCIe: no** (a 2 GB/s copy stream from a second process: 0 to 7 %).

Competitors kept real time in every combination tried (three at once, including two 2160p
transcodes and a tone-mapped one: 24–26 fps each); unthrottled, their total rate stays that of
one (1080p 620–730 fps). No NVDEC/NVENC session limit was met.

#### How much headroom to budget

Planning figures for the owner's load (one to three simultaneous transcodes, some from 2160p
sources, tone mapping under 10 % of traffic). "Budget" is the encoder's mean tick alone as a
share of the frame period; the session counts are for this card as cooled (§0.3) and assume
the tick grows in proportion to the sessions [E for 720p and 1080p, which were not run next to
competitors; the ratios are shares of time and should carry over]:

| Case | Worst ratio measured | Budget | 1080i | 1080p | 720p | 480i | mixed units |
|---|---|---|---|---|---|---|---|
| Alone (capacity, §0.1) | – | 86–95 % | 67 | 72–75 | 90 | 400 | 24 |
| **Typical**: up to 3 real-time transcodes, 1080p or 2160p sources, no tone mapping | 1.13 | **85 %** | 65 | 70 | 80 | 365 | 23–24 |
| **Worst case**: a real-time tone-mapped 2160p stream with two 2160p transcodes | 1.18–1.33 | **75 %** | 58 | 62 | 71 | 325 | 21 |
| Bursts of an unthrottled 2160p transcode, kept on time | 2.0 | 50 % | 39 | 41 | 47 | 220 | 14–16 |

The first two rows keep every tick on time for steady real-time transcodes (at 83 %, the
real-time worst case gave late runs under a second and 2–4 % of pictures to repeat; three
real-time 1080p transcodes cost 3 %). Bursts are not covered: at the typical budget, while an
unthrottled 1080p transcode runs the tick is 1.05–1.08 of the period (a few percent of
pictures late), while an unthrottled 2160p one runs every tick is late and a quarter to a third
of the pictures would have to be repeated (measured at 83 %: 1 × 2160p 31–36 %, 2 × 2160p + tone
map 24–26 %, the paced encoder 2.3–5 s behind after 5.5 s). Budgeting 50 % to cover those is the
only way to keep every tick, and it halves capacity for a case that lasts seconds. The better
answer is the deadline-miss handling below: at the typical budget it would repeat about a third
of the pictures for the length of a 2160p burst (Plex's 1080p bursts lasted 1–3 s; 2160p ones
were not observed) and nothing otherwise.

Not proven: clean capacity with the new binary (a transcode was busy in every attempt); 720p
and 1080p next to competitors; Plex's own tone mapping and Jellyfin's `tonemap_cuda` (stock
`tonemap_opencl` with a host round trip stood in); how long and how often Plex bursts on 2160p
sources; a run with no other context at all.

Mitigations, ranked:

1. **Upload fence: done** (experiment 41, above).
2. **Headroom**: 85 % of the period for the typical load, 75 % when tone-mapped 2160p streams may
   run (table above).
3. **Deadline-miss handling.** A tick that is late should cost one repeated picture (a skipped
   P or B picture is a few hundred bits and legal), not a growing delay. The ladder already has
   the skipped picture; a wall-clock trigger for it is not built. With it, unthrottled bursts
   at the typical budget cost a share of pictures for seconds instead of a backlog.
4. **What to avoid or change on the media servers**, in order of cost: unthrottled transcoding of
   2160p sources (half the card while it runs); HDR tone mapping on the GPU (3.9 ms of our time
   per frame; 12 % in real time); 2160p sources at all (1.2–2 ms per frame); a large Plex
   "throttle buffer" (longer bursts). Direct play costs nothing. Real-time 1080p transcodes are
   harmless (1 % each).
5. **`nvidia-smi compute-policy --set-timeslice`: no effect** (experiment 40: Short, Medium and
   Long gave the Default ratios). Not worth setting.
6. **MPS: not applicable.** It would let both contexts run concurrently, but the server accepts
   clients of one user (or all users without isolation in multi-user mode, started as root),
   every client must be started under it, the Codec SDK under MPS is not clearly supported, and
   Plex is a separate service [K, NVIDIA MPS documentation; not tested].
7. **A lower power limit: no** (§0.6); it would add to the clock loss near capacity.

### 0.8 Tried and rejected

| # | Experiment | Measured | Why it lost |
|---|---|---|---|
| 3, 26 | `-maxrregcount` on `k_me` (128 / 104 / 88 early; 144 / 128 on the final code) | −3 to −6 % early; −3 % and −0.5 % late | spills to local memory cost what the extra warps give |
| 7 | Spinning wait (`--spin`) | +2 % for a whole host core | kept as an option; the hybrid wait (19) gets most of it for 0.15 core |
| 11 | Motion search split into four kernels with 48 to 128 registers each | −7.5 % 1080i, −4.2 % 720p, −7.8 % 480i | each kernel fetches the current rows and the reference window again; the single kernel has them in the caches |
| 14 | Scan and matrix tables as packed 64-bit constants | −1.2 % 1080i, −4.8 % 720p | 64-bit shifts cost more than cached table loads |
| 15 | Exact skip of the field search where it cannot win | −3.8 to −11.6 % | threads that skip wait for their warp; the search had to move away from the rows it shares with the full-pel step |
| 16 | GOP phases balanced modulo 3 for the mixed line-up | no change (p99/mean 1.42 → 1.40) | the cost of a tick depends on its macroblocks, hardly on their picture types; the long ticks were the flush ticks at the end of a run |
| 18 | B pictures search the quarter-resolution level only around zero and the temporal candidate | no speed gain; `pedestrian_area` 1080i −0.20 dB | 183 registers instead of 168 (10 instead of 12 warps per SM) |
| 21 | Both reference searches written out (results in registers) | −4.5 % in all three formats | 177 registers and twice the code; not explained by occupancy alone (see 33) |
| 33 | The two coarse search levels in a kernel of their own (they read planes the rest never touches); `k_me` then has 137 registers, 7 blocks per SM | −2.6 % 1080i, −3.4 % 720p | more warps do not make the search faster, even without repeated fetches |
| 26 | Half-sample steps word by word (fewer live values in the source) | 0 % | the register count stayed at 168 |
| 28 | `k_xf` with one thread per block (no shared memory, no barriers, coalesced stores) | 0 % (transform stage 2.27 against 2.14–2.26 ms) | the kernel is bound by the 1.3 kB per macroblock it moves, not by its thread arrangement |
| 31 | Smoothing pass scored on every second row | +1.5 %; −0.043 dB on average, `still` 720p −0.30 dB | not worth the quality |
| – | A bit-cost penalty on the temporal candidate (2, 6, 12 λ) | +0.064 / +0.056 / +0.063 dB against +0.067 without | no help on the torture cases either |
| – | Search range ±11 or ±19 pixels | +4 % / +1 %; −0.53 / −0.05 dB on average, single clips −3.5 / −1.0 dB | P pictures are three frames apart |
| 25 | Power cap below 70 W | −6 % at 60 W, −16 % at 50 W, −44 % at 40 W free-running; no change in watts at a fixed line-up | the clock falls with the cap |

Three things the rejected experiments say about this card and this code: per-thread early
exits and per-thread adaptive work do not pay (the warp waits for its slowest thread);
occupancy is not the lever for the search (experiments 3, 11, 21, 26 and 33: more warps per
SM never made it faster, whether by compiler flag, by splitting the kernel, or by splitting
off the part that shares no data); and `k_xf` is at the limit of what its data flow allows.

### 0.9 Remaining targets, ranked

The yardstick is the tick of 16 × 1080i, now 6.8 to 6.9 ms (§0.4). Estimates are [E]; none of
them is as large as the changes already made.

| # | Target | Evidence | Estimated gain |
|---|---|---|---|
| 0 | **Budget for other GPU users.** (The deadline-miss handling is built, §C.7; with inputs on NVDEC the decoder's share is the budget that binds, §C.5.) | Real-time transcodes cost 1 to 13 % (tone mapping up to 33 %), unthrottled bursts 25 % to half (§0.7) | 85 % / 75 % of the period (§0.7 table) for the kernels; 75 % / 65 % of the decoder (§C.5) |
| 1 | **Cooling.** | At sustained capacity the card sits at 90–91 °C and its clock falls from 2430 to 1890–2040 MHz (§0.3) | up to +10 to +15 % sustained capacity; not a code change |
| 2 | **Quantization inside `k_xf` with a predicted quantizer**, `k_quant` only for sessions whose prediction missed. Removes the coefficient round trip (the stores cost about 0.4 ms after experiment 13, `k_quant` 0.42 ms) | Experiment 13 and §0.4 | +6 to +10 %; needs a rate-control study, the exact-size check and the ladder stay |
| 3 | **Rate control on the device** (quantizer choice in a kernel): one wait per tick less | 0.3 ms idle per 6.8 ms tick; the hybrid wait gained 4 % at 720p | +2 % at 1080i, +3 to +4 % at 720p and mixed |
| 4 | **`k_mvsmooth`** (0.44 ms): 80 registers, 12.7 of 32 threads active, 83 % waiting for loads; compact the candidates or get it to 64 registers | profile | +2 to +3 % |
| 5 | **Entropy** (`k_ent_resolve` + `k_ent_pack` 0.34 ms): one kernel, levels read once | timeline | +1 to +2 % |
| 6 | **The search itself** (`k_me` 1.9 ms: quarter-resolution level 0.48 ms, field search about 0.5 ms, half-sample ring 0.25 ms): fewer instructions per candidate, not more warps | timeline; experiments on occupancy all lost | +2 to +4 % |
| 7 | **Quality at scene cuts** (0.7 to 1.3 dB behind ffmpeg): a key frame at the cut would need the fixed-GOP rule of SPEC §8.2 relaxed; within the rule, spend the scheduled I picture's bits elsewhere when an intra P picture has just been coded | §0.5 | 0.3 to 0.8 dB on cuts; no speed |
| 8 | **Mode decision by estimated bits** instead of SAD plus fixed bit costs; chroma in the motion decision | gap to ffmpeg without adaptive quantization about −0.1 to −0.5 dB | 0.1 to 0.3 dB |

What the measurements say is not worth doing: any further rearrangement of `k_xf`'s threads
(experiment 28); per-thread early exits (15); anything for occupancy (3, 11, 21, 26, 33); a
smaller search range (24); a power cap for throughput.

### 0.10 What is still not production-shaped

In addition to §12 (which still holds, except where noted there):

- **The wait.** `Encoder::wait()` sleeps for a share of the last ticks' duration and then polls;
  it falls back to a blocking wait when a phase takes more than twice as long as expected. It
  has not been tested next to a Plex transcode or under host CPU pressure.
- **The quantizer floor.** A picture whose quantizer falls more than three codes below the last
  one of its type repeats its transform (1 to 2 % of pictures on `crowd_run`, 7 to 11 % on a
  still and on a rate walk). That is a cost, not an error, but a statmux that moves rates
  every GOP will see it more often than the benchmark does.
- **The temporal candidate** helps at nominal rates and moves the quarter-rate torture cases by
  −0.1 to +0.9 dB (§0.5). Its effect on pictures has not been looked at.
- **Picture sizes.** Rows longer than 256 macroblocks (4096 samples) are refused. A picture
  whose width is not a multiple of 16 takes the `cudaMemcpy2DAsync` path and `k_pad_cols`,
  tested on one cropped clip only (identical to the phase-A binary).
- **End of stream.** When the input ends, sessions of half the tick rate flush in every tick; the
  benchmark leaves those ticks out, a real mux would see one or two late pictures at shutdown.
- **`--range` above 9 with field prediction** wrapped field vectors until experiment 24; fixed,
  and the gate passes at ±51 pixels, but only the default range has been run on the whole gate.
- **Benchmark content.** Capacity is measured on `crowd_run`; `pedestrian_area` and `fourpeople`
  run 2 to 6 % faster free-running at 16 × 1080i (experiment 13), other content was not timed.
- **Other GPU users.** See §0.7: a transcode on the same card takes capacity away, most in its
  unthrottled bursts. Since work package C the encoder reacts: `step(deadline)` repeats
  pictures instead of running late (§C.7). The upload no longer serializes with the kernels
  next to another context (experiment 41). The shared decoder is the open problem (§C.5, §C.9).

## 1. Summary of phase A

(History. The speed figures here are those of the end of phase A; §0 has the current ones.
The statements about correctness, the buffer model and the rate change still hold.)

Capacity: sessions that hold real time (99 % of ticks on time, 25 s paced runs on `crowd_run`,
GPU warm) [M]:

| Format | First spike's encoder, serial | First spike's encoder, overlapped upload | End of phase, all tools, serial, GOPs in phase | End of phase, all tools, staggered GOPs, overlapped upload |
|---|---|---|---|---|
| 1080i29.97, 15 Mbit/s | 28 | 31 | 23 | **29** |
| 1080p29.97, 15 Mbit/s | 27 | 33 | 24 | **31** |
| 720p59.94, 12 Mbit/s | 26 | 29 | 27 | **34** |
| 480i29.97 (704 wide), 4 Mbit/s | 160 | 192 | 144 | **176** |

- The last column is the workload to optimize ("realistic"): strict buffer model with ladder,
  row-level rate control, adaptive quantization, field prediction for interlaced formats,
  vector smoothing, per-session GOP phase. It is 7.0 to 7.6 million macroblocks per second in
  every format. One sustained run: 29 × 1080i for 180 s, 99.94 % of ticks on time, 68 W, 91 °C.
- A line-up of three formats in one process holds less: about 5.0 million macroblocks per
  second (§8.5).
- What the tools cost, free-running (§8.2): the new pipeline with the tools off −2 to −9 %,
  adaptive quantization under 1 %, field prediction −7 %, vector smoothing −4 %; all together
  −11 to −18 %. Staggering the GOP phase costs nothing and removes the I-picture tick that set
  the 720p limit.
- Where the time goes now (16 × 1080i, realistic, 17.7 ms per tick): motion search stage
  55 % (`k_me` alone 37 %), transform and statistics 19 %, quantization and entropy coding
  18 %, reconstruction 5 % (§8.3). `k_me` runs at 16 % occupancy with 255 registers per
  thread and two warps per scheduler; it is starved of warps and full of stack-array traffic
  (§10). §11 ranks the targets; the first one alone should be worth about a fifth more
  capacity [E].
- Correctness (§7): 54 gate cases, no failure. No decoder message, no drift, **no buffer
  violation on any content at any rate tried**, including a quarter of the nominal rate on
  the hardest clips and random noise, in two independent simulations. The first spike's
  encoder failed 19 of the 41 cases it could run; ffmpeg `mpeg2video` itself underflows at
  the quarter rates.
- Rate change is legal now: one sequence, `bit_rate` = the ceiling, `vbv_delay` = 0xFFFF, the
  buffer model follows the delivery rate (§5.1). The mux's side of the contract is written
  down there.
- Quality at equal rate (§7.1): 0.2 to 1.0 dB below ffmpeg's default `mpeg2video` in luma
  PSNR on most clips with adaptive quantization on (it costs 0.3 to 0.5 dB of PSNR by
  design), 1.3 to 1.4 dB below on scene cuts and on `mobcal`. Against the first spike's encoder it is 0.0 dB on average, with
  the buffer model now holding.
- The card is a limit in a second way: it sits at 90 to 91 °C during capacity runs and
  throttles (§8.5).

Nothing went wrong on `nixark` in this phase: no out-of-memory event other than the two
deliberate cgroup tests of the memory cap, no failed unit, no process left running (§2 has the
rules, §13.4 the history).

## 2. How to work on it

All commands run on `nixark` in `~/totalblonder-spike` (a copy of `spikes/cuda-mpeg2/`; edit in
the repository on `bagel` and copy). `spikes/cuda-mpeg2/README.md` has the option lists.

```sh
# on bagel: copy sources over (never the other way for src/ and scripts/)
rsync -a --exclude build --exclude media --exclude out --exclude results spikes/cuda-mpeg2/ nixark:totalblonder-spike/
# on nixark (ssh nixark; the login shell is fish: run script files or pipe to `bash -s`)
cd ~/totalblonder-spike
./scripts/run.sh make                       # build/m2venc
./scripts/run.sh make check                 # quick gate: 15 cases, about 35 s; after every change
./scripts/run.sh make check-full            # 54 cases, about 100 s; at milestones
./scripts/run.sh make check-pipe            # 46 cases, about 4 min: device input, repeats, run-time sessions, conversions (§C.8)
./scripts/run.sh ./scripts/prep_inputs.sh   # once: compressed inputs for the pipeline under media/in/ (250 MB)
./scripts/run.sh python3 -I scripts/pipe_lineup.py --label try --lineup c --n 6   # one paced pipeline line-up (§C.4); --cap LO:HI bisects
./scripts/run.sh python3 scripts/qdiff.py -v   # after check-full: PSNR and SSIM against the baseline, per standard case
./scripts/run.sh make bench LABEL=try1r ARGS="--enc-arg=--stagger --enc-arg=--overlap"   # the realistic workload: free-run
                                            # macroblocks/s x3 + stage times for 1080i, 720p, 480i and the mixed line-up, about 70 s
./scripts/run.sh python3 scripts/bench_diff.py final-realistic try1r   # final-realistic: the final binary of phase B
./scripts/run.sh make bench-cap LABEL=m1 ARGS="--enc-arg=--stagger --enc-arg=--overlap"  # capacity bisection, five formats, ~20 min
SKIP=150 COUNT=80 ./scripts/ncu_profile.sh try1 ./build/m2venc --stagger --frames 14    # sudo ncu, full metric set, steady ticks
./scripts/nsys_profile.sh try1 ./build/m2venc --stagger --overlap     # timeline of a tick (MIXED=8 for the mixed line-up)
# on bagel: fetch results, then commit
rsync -a nixark:totalblonder-spike/results/ spikes/cuda-mpeg2/results/
```

Rules on `nixark` (it is shared with Plex, Jellyfin and nzbget, and the first spike ran it
out of memory, §13.4):

- Everything goes through `scripts/run.sh`: a transient systemd scope with `MemoryMax=10G`,
  `MemorySwapMax=0`, and a refusal to start under 12 GiB available. Proven by
  `scripts/capped_selftest.sh`: a process that allocates 2 GB under a 1 GiB limit is killed by
  the cgroup OOM killer, alone (`constraint=CONSTRAINT_MEMCG` in the kernel log) [M]. The user
  service manager has the memory controller, so no sudo is needed for this.
- sudo only for Nsight Compute (`ncu_profile.sh`). Nsight Systems needs no root; it needs
  `TMPDIR` set to a directory you own (`/tmp/nvidia` belongs to root), which the script does.
- A process started over ssh dies when the ssh session ends. Keep the session open.
- No many-process runs. One encoder process pins `--max-in-frames` frames of input (the
  benchmark uses 48 frames, 150 MB for 1080).
- For a gain under about 5 %, do not trust two `make bench` results taken minutes apart: the
  card is 3 to 4 % faster cold than warm and single runs scatter by 1 to 3 %. Phase B kept the
  binary of each step and ran old / new / old back to back, two runs each (the log shows the
  pairs). A change that should not alter decisions was checked by comparing the coded streams
  byte for byte with the previous binary, on three single-session clips and on a mixed
  three-format run, before any timing.
- `nvcc -Xptxas -v` prints the registers per kernel. Do not spend time on occupancy for
  `k_me` (168 registers): five experiments with more warps per SM all lost (§0.8).
- Before a timing run look at `nvidia-smi pmon`: a Plex transcode on the card slows every
  tick by 1 to 27 %, a 2160p one up to half, while it works (§0.7). `bench.py` marks such runs as contaminated;
  `scripts/capclean.py` measures capacity points and repeats disturbed runs.
- Benchmarks are short on purpose. `make bench` is the iteration metric; capacity bisection
  only at milestones; no soak runs. Each run records temperature, power and clock; a number is
  "cold" below about 70 °C at the start and "warm" above 80 °C. The difference matters (§9.5).

Benchmark definitions (fixed; `scripts/bench.py` has them in its header):

- Tick: one frame period of the format. In a tick each session takes one frame and codes one
  picture.
- Free-run: macroblocks per second with no pacing, 16 sessions (96 for 480i), 5 s of video,
  three runs; plus one run with `--profile` for stage times (the encoder synchronizes after
  every stage and, since phase B, waits for the overlapped upload inside its own stage, so that
  run is slower than a free run and its other stages are free of the upload).
- Mixed: the format `mixed` is N units of 1 × 1080i + 1 × 720p59.94 + 4 × 480i in one process
  on a 59.94 Hz tick; free-run at N = 8 (8 + 8 + 32 sessions); capacity in units.
- On time: in a paced run the tick's output exists before the next tick is due; the first
  second is left out, and since phase B the ticks after the end of the input as well (the
  sessions flush there; log, experiment 16).
- `cap99`: largest session count with ≥ 99 % of ticks on time in a 25 s paced run on
  `crowd_run`, by bisection. `cap999`: largest tested count with ≥ 99.9 %.

The gate's baseline (`results/check/baseline.json`) holds PSNR and SSIM per case; a change
fails the gate when a case loses more than 0.10 dB or 0.002 SSIM, when any decoder complains,
when a buffer model is violated, or when the encoder's reconstruction drifts from the decoder.
After an intended quality change, `make check-full ARGS="--update-baseline"`.

## 3. Formats and test corpus

| Format | Size, rate | GOP | Nominal rate, buffer | Macroblocks/s per channel |
|---|---|---|---|---|
| 1080i | 1920×1080 interlaced (coded 1920×1088), 29.97 | 15, M = 3 | 15 Mbit/s, 7,487,488 bits | 244,555 |
| 1080p | 1920×1080 progressive, 29.97 | 15, M = 3 | 15 Mbit/s, 7,487,488 bits | 244,555 |
| 720p | 1280×720, 59.94 | 30, M = 3 | 12 Mbit/s, 5,996,544 bits | 215,784 |
| 480i | 704×480 interlaced, 29.97 | 15, M = 3 | 4 Mbit/s, 1,835,008 bits | 39,560 |

- **SD is 704×480, not 720×480.** ATSC A/53 Part 4 Table 6.2 allows 704 or 640 samples per
  line at 480 lines; SCTE 43 Table 3 (§7.1.2) allows 720 and 704. 704 is legal under both.
  The first spike used 720 (40,500 macroblocks/s per channel, 2 % more).
- 1080p29.97 was added because it is what the producer delivers (SPEC §8.2).
- The buffer is 0.5 s of the nominal rate in units of 16,384 bits, except 480i, where Main
  Level limits it to 1,835,008 bits (0.459 s at 4 Mbit/s).

Corpus (`scripts/prep_media.sh`, `scripts/derive_media.py`; 20 GB under `media/` on
`nixark`, of which 5.4 GB are downloads from the Xiph derf collection; not in git):

| Clip | Formats | What it exercises |
|---|---|---|
| `crowd_run` (derf, 1080p50) | all | Hard: dense texture, camera motion. The benchmark clip |
| `pedestrian_area` (derf, 1080p25) | all | Moderate motion close to the camera |
| `park_joy` (derf, 1080p50) | all | High-detail pan with grain; harder than `crowd_run` for the buffer |
| `fourpeople` (derf, 720p60) | all | Talking heads, static camera |
| `football`, `mobcal` (derf `*_422_ntsc`, 720×486) | 480i | Native interlaced: sport; slow pan over fine detail. Bottom field first at the source (ffmpeg `idet`: 121 of 121 frames) [M]; cropped 3 lines to make top field first, and a `_bff` variant cropped 4 lines |
| `cuts` | all | A scene cut every 0.37–0.8 s, not aligned to the GOP |
| `fade` | all | `crowd_run` fades to black, `park_joy` fades in |
| `grain` | all | `pedestrian_area` plus temporal noise |
| `noise` | all | 2 s of uniform random bytes, then `crowd_run` |
| `still` | all | 1 s of motion, 3 s of one repeated frame, 1 s of black |
| `testsrc2` | 1080i, 720p, 480i | Synthetic (the SPEC §14 estimate used it) |

Interlaced clips from progressive sources take alternate fields from successive source frames
(ffmpeg `interlace`, no low-pass), so the fields of a frame are different instants. Clips are
5 s (150 or 300 frames), except the three from the first spike.

## 4. What the encoder implements

Checked by the gate unless marked.

- **Stream syntax.** Sequence header, sequence extension and sequence display extension
  (color description: BT.709 for HD, SMPTE 170M for SD, as A/53 Part 4 §6.1.4 asks) at every
  key frame; GOP header (closed GOP); picture header and coding extension; picture user data;
  one slice per macroblock row (§6.2). Main Profile at Main Level for 480i, High Level
  otherwise. `q_scale_type` = 1 (non-linear quantizer scale, 1 to 112, Table 7-6) in every
  picture; `intra_vlc_format` = 1; alternate scan for interlaced formats.
- **Pictures.** I, P and B frame pictures; closed GOPs; key frames at a fixed distance, at a
  per-session phase, and on demand; a B picture never precedes a key frame in display order
  (the last one is turned into a P picture).
- **Motion.** Per macroblock: intra, forward, backward or bidirectional; frame-based or
  field-based prediction (`frame_motion_type`, two vectors per direction, each with
  `motion_vertical_field_select`; §6.3.17, §7.6.3, §7.6.4); frame or field DCT (`dct_type`).
  Vector prediction for field vectors in frame pictures (vertical predictor halved, §7.6.3.1),
  predictor update rules of Table 7-9, skipped macroblocks in P and B pictures (§7.6.6.2,
  §7.6.6.4), chroma vectors by division with truncation (§7.6.3.7).
- **Interlace signaling.** `progressive_sequence` = 0, `progressive_frame` = 0,
  `frame_pred_frame_dct` = 0, `top_field_first` = 1 or 0 (`--bff`), `repeat_first_field` = 0,
  `chroma_420_type` = 0 for interlaced formats; the progressive formats set
  `progressive_sequence` = 1, `progressive_frame` = 1, `frame_pred_frame_dct` = 1.
- **Quantization.** Default matrices; a quantizer per macroblock (`macroblock_quant`,
  Tables B.2 to B.4) from a base code per row and the macroblock's activity; inverse
  quantization, saturation and mismatch control for the reconstruction (§7.4).
- **Rate control and buffer.** §5.2 and §5.3: picture targets, row quantizers from a
  pre-pass, corrective passes, the ladder past quantizer 112, a buffer model that is never
  violated.
- **Rate change.** §5.1: any picture or the next key frame, constant sequence header.
- **Captions.** `cc_data()` triplets per input frame, written as ATSC picture user data
  (`GA94`, type 3; A/53 Part 4 §6.2) with the picture they belong to, so they travel in
  decode order and a decoder returns them in display order.
- **Interface.** `open` / `submit` / `set_rate` / `force_keyframe` / `step` / `receive`
  (`src/enc.h`); access units carry PTS, DTS, type, key-frame flag, mean quantizer, bits,
  complexity (bits × quantizer), the buffer state and which ladder step was used. Sessions of
  different formats, frame rates, GOP phases and rates run in one process.

Not implemented: field pictures and dual prime (dual prime is not allowed with B pictures,
§7.6.1; field pictures were not evaluated, the research note advises against them);
`repeat_first_field` (3:2 pulldown is done in the frame clock by repeating fields into frames,
§C.3, not by the flag); custom quantizer matrices; `intra_dc_precision` above 8 bits;
scene-cut key frames (SPEC §8.2 disables them); anything outside the video elementary stream.
The CPU entropy coder and the `--on-gpu` input mode of the first spike were removed; since work
package C a session takes frames written on the device (`submit_dev`), input from NVDEC and
scaling exist in front of the encoder (§C), sessions open and close at run time, and a picture
may be flagged `progressive_frame` = 1 inside an interlaced sequence.

Against the encoder trait of SPEC §8.1:

| Trait item | State | Gap |
|---|---|---|
| `submit(frame)`, captions as side data | `Session::submit(frame, pts, cc, n)`; frame is raw 4:2:0 at output size in pinned host memory. Or `submit_dev(pts, cc, n, progressive)`: the session's input planes on the device, written by the caller's kernels; `submit_again(pts)`: the previous frame once more | Scaling, color conversion and NVDEC input sit in front of the encoder (`pipe.h`, `dec.h`, §C); captions are not carried through the harness |
| `poll(deadline)` | `Encoder::step(deadline)` codes one picture for every session with work, or a repeat for those that would not fit (§C.7), then `Session::receive()` | The scheduler thread exists only in the harness (§C.7). A CUDA error ends the process; `open` reports a bad configuration instead of exiting |
| `set_rate(cap)` → ticket | `set_rate(bps, at_gop)` returns a ticket; the first access unit under the new rate carries it and the rate | The cap's `vbv_bits` is fixed at `open` (it defines D); only the rate changes |
| `RateChange` | Both: at the next coded picture (`Seamless`) or at the next key frame (`AtNextGop`) | – |
| `strict_vbv` | True in every test (§7) | The guarantee needs the rate above a floor (§5.3) and the mux contract of §5.1 |
| `fixed_gop` | True; per-session phase; `force_keyframe()` | A forced key frame restarts the cadence from that frame. A key frame replaced by the last ladder step (§5.3) is not repeated. A key frame given up to the deadline policy is flagged (`key_missed`) and made up once off the grid; the grid itself does not move (§C.7) |
| `a53_captions` | True | The caller builds valid `cc_data` (count per frame rate, 608 pairs first); the encoder only wraps and orders it |
| `interlaced` | True, frame pictures | – |
| `avg_qp`, statistics | `avg_q`, `bits`, `complexity`, `intra_mbs`, `field_mbs`, buffer state per access unit | – |
| Timestamps | PTS from the caller; DTS = first PTS + (coded index − 1) frame periods | – |

## 5. Design decisions

### 5.1 Rate change and the buffer model

The first spike changed `bit_rate` in a repeated sequence header. §6.1.1.6 forbids that:
"In repeat sequence headers all of the data elements with the permitted exception of those
defining the quantization matrices … shall have the same values as in the first sequence
header", and likewise for the sequence extension.

What the encoder does now:

- `bit_rate` is the session's ceiling `max_bitrate` and never changes; `vbv_buffer_size` is
  `vbv_bits` and never changes; `vbv_delay` is 0xFFFF in every picture. §6.3.3 defines
  `bit_rate` as an upper bound on the rate into the VBV, and §6.3.9 allows 0xFFFF if every
  picture of the sequence carries it. A/53 Part 4 §6.1.5 and SCTE 43 §7.1.5 accept 0xFFFF.
- One sequence that never ends. No `sequence_end_code`, no discontinuity for a TV to see.
- The research note's alternative, ending the sequence and starting a new one at a closed
  GOP, is legal but puts a sequence junction in front of the decoder at every statmux tick;
  it was not built.

Two buffer models follow from that, and `scripts/vbv_check.py` simulates both:

1. **The normative model, Annex C.3.2.** Data enters the VBV at `bit_rate` whenever the
   buffer is not full; decoding starts when it is full; pictures are removed at the frame
   rate. Overflow cannot happen by definition. The only requirement is no underflow (C.6).
   This is what a conformance checker would test.
2. **The operating model**, which is what the encoder enforces and what the mux must match:
   - D = `vbv_bits` / `max_bitrate` (0.5 s in SPEC §8.2). It is fixed at `open`.
   - Coded picture n enters the mux queue at e(n) = n × T (T = frame period) and is removed
     from the decoder buffer at e(n) + D. DTS and PCR in the mux realize that delay.
   - ρ(t) is the session's rate: the value given to `open`, changed by `set_rate` at the
     time of the first picture coded under the new value.
   - **The mux sends the session's queued data at a rate of at least
     ρ̄(t) = max{ρ(s) : t − D ≤ s ≤ t}, and at most `max_bitrate`.** In words: after a decrease
     the mux keeps the old rate for D more seconds; before an increase reaches the encoder,
     the mux is already sending at the new rate. That is SPEC §8.5 ("lower before raise").
   - The encoder guarantees: every picture has left the mux queue completely by e(n) + D,
     so the decoder never underflows; and since at most `max_bitrate` × D = `vbv_bits` can
     arrive within D, the decoder buffer never overflows. The encoder writes no stuffing;
     when it has nothing to send the mux sends null packets.

   A stream that satisfies the operating model also satisfies C.3.2, because C.3.2 delivers
   every bit at least as early.

What the encoder computes (`struct Vbv` in `enc.h`): the queue E after each picture, drained
between pictures at ρ̄; for picture n the limit E + bits(n) ≤ ∫ ρ̄ over [e(n), e(n) + D], where
only rate changes that have already happened count. After a decrease from r1 to r2 the limit
falls from r1 × D to r2 × D linearly over D; after an increase it rises at once.

What the mux must guarantee in return, exactly:

1. Delay: the last byte of picture n is in the decoder's buffer no later than its DTS, given
   that the encoder obeys the limit above; equivalently the mux's delivery rate for this
   elementary stream is never below ρ̄(t) while data is queued.
2. It never delivers faster than `max_bitrate`, and keeps the T-STD buffers of H.222.0 in
   order itself (the encoder's model is the elementary stream buffer only).
3. Ordering of a change: the mux learns the picture at which a new rate applies from the
   access unit that carries the ticket (`rate_ticket`, `rate`). For an increase it must already
   deliver at the new rate when that access unit is queued, so it raises first and calls
   `set_rate` after; for a decrease it may lower its rate D after that access unit's coding
   time, not before.

The statmux consequence: during the D after a decrease the program still occupies its old
rate in the pool (§8.5 says so already). The encoder aims the GOP budget at the new rate from
the first GOP, but does not degrade a picture below what the real buffer demands, so a GOP
right after a large decrease can use more than rate × duration (§7.3 has the numbers).

### 5.2 Rate control

Per session, per picture (`Batch::code` in `enc.h`):

1. **Target** (Test Model 5 step 1): the GOP budget R is split by the complexities X of the
   three picture types with K_P = 1, K_B = 1.4. Two changes from TM5:
   - R at a key frame = rate × GOP duration + (0.15 × rate × D − queue). Every GOP is planned
     to end with the mux queue at 15 % of the buffer. This ties the budget to the buffer model
     and makes a rate change take effect in the first GOP.
   - X of the picture being coded comes from its own statistics (below), not from the last
     picture of its type. Without this a scene cut left the allocation with a stale figure
     and the following pictures were starved (measured on `cuts`: +0.4 dB) [M].
   - A P picture that is more than half intra (a scene cut) gets an I picture's share, taken
     from the buffer, not from the rest of the GOP.
2. **Statistics before quantizing.** `k_xf` transforms every block once and builds, per
   block, a 16-bin histogram of the quantiser_scale at which each coefficient becomes zero
   (bins at half-octave steps from 1 to 192, normalized by the macroblock's activity factor).
   Two small kernels sum it per macroblock row; 72 bytes per row come back to the host.
3. **Row quantizers.** Model: bits = θ × (non-zero levels at the chosen quantizers) + H, with
   θ (bits per non-zero level, 5 to 8 in practice) and H (everything that is not a run/level
   code) tracked per picture type from the last coded picture (the ρ-domain model of He and
   Mitra, cited in the research note). The host finds the first code whose prediction meets
   the target and gives a fraction of the rows the next finer code so that the prediction
   lands on the target. The per-row code is the slice's `quantiser_scale_code`.
4. **Adaptive quantization** (TM5 step 3): macroblock quantizer = row quantizer ×
   (s × act + avg) / (act + s × avg), act = 1 + the smallest variance of the macroblock's 8×8
   luminance blocks (frame- and field-organized), avg = the previous picture's mean, s = 1.5
   (TM5 uses 2). Sent with `macroblock_quant` when it changes. Not applied at code 31.
5. **Exact size, then correction.** After `k_quant` and the packing kernels the host knows
   the exact picture size before anything is written. A picture over its limit is always
   requantized (§5.3). A picture more than 30 % off its target is corrected once, with θ and
   H re-estimated from the pass just measured. Only the affected sessions repeat `k_quant`
   and packing; the transform is not repeated.

Measured: 0.001 to 0.007 extra passes per picture at the nominal rates on `crowd_run`
(the first spike needed 0.05 to 0.11 full requantizations) [M]; up to 0.3 on the torture
cases (§7.2).

### 5.3 Never violating the buffer: the ladder

When the model says even code 31 (quantiser_scale 112) does not fit, or a packed picture is
over its limit, the session goes up a ladder, one or more steps per pass:

| Step | Action |
|---|---|
| codes 1–31 | quantiser_scale 1 to 112, mixed per row |
| 32–35 | code 31 and only the first 12, 6, 3, 1 coefficients of each block in scan order |
| 36 | no coefficients at all; intra blocks keep their DC |
| 37 | P or B picture: every macroblock predicted with a zero vector and nothing coded, so all but the first and last of each row are skipped (under 1 kB at 1080). An I picture is replaced by such a P picture, without GOP header |

- The limit of a picture is the room in the buffer model. A picture that is over its target
  may not use a reserve at the end of the buffer: 5 % for an I picture; for P and B pictures
  30 %, or the smallest possible I picture (the part of the last one that did not depend on
  the quantizer) plus 5 %, whichever is larger, up to 60 %. That keeps room for the next key
  frame.
- The next picture of the same type starts on the step the last one ended on, or one lower
  if that had room to spare. So a long hard passage costs about one extra pass per picture,
  not six.
- **Why it cannot fail.** Before any picture the room is at least one frame period of
  delivery (rate × T), because the queue drained that much since the last picture met its
  limit. `Session::min_rate()` requires rate × T to be at least twice the size of a step-37
  picture with all headers and the largest caption payload (0.4 Mbit/s for 1080i, 0.2 Mbit/s
  for 480i). `open` also requires that a DC-only I picture (about 100 bits per macroblock)
  fits the buffer at the initial rate, since the first picture of a stream cannot be
  replaced (1.6 Mbit/s for 1080 with D = 0.5 s). `set_rate` clamps to that floor and to
  `max_bitrate`.
- An I picture replaced by a P picture means no random access point at that position. With
  the reserve rule above it does not happen in any gate case (§7.2). If it happens the
  encoder does not schedule a replacement key frame; a caller that needs one can call
  `force_keyframe()`.
- No stuffing is ever written. In the first spike the encoder padded pictures with zero bytes
  to keep a constant-rate VBV from overflowing; with C.3.2 signaling that is not needed and
  SPEC §8.2 wants no filler.

### 5.4 Interlace

Frame pictures with, per macroblock, frame or field prediction and frame or field DCT.

- Field candidates come almost free from the frame search: each of the nine full-sample frame
  candidates is scored separately on the even and the odd lines of the macroblock; a frame
  candidate with vertical offset dy predicts the top field from the reference field of parity
  dy & 1. That gives nine candidates per field.
- A second seed per field: the reference field of the other parity is one field period nearer
  or farther, so for steady motion its vector is the frame vector scaled by the ratio of the
  field distances. Nine positions around that, then one half-sample step on the winner.
- The new search code (`field_seed`, `field_refine`, `field_bi_sad`, `frame_pred_sad`) is in
  the packed style the research note recommends: 32-bit loads, `__funnelshift_r`, `__vsadu4`,
  `__vavgu4`, no per-thread pixel arrays. The frame search was the first spike's byte loop
  until phase B rewrote it in the same style (§0.2).
- Since phase B the frame search has one more full-pel candidate per reference: the vector of
  the co-located macroblock of the session's previous coded picture, scaled by the temporal
  distances (`--tpred`; +0.067 dB on average, +0.45 dB on `mobcal`; §0.5).
- A field candidate is charged 12 bits × λ (λ ≈ quantiser_scale / 2 of the previous picture).
- `k_mvsmooth`: after the search a macroblock adopts its left neighbor's mode and vectors
  when that costs less SAD than the bits it saves. Independent searches leave the vector
  field noisy; MPEG-2 pays for every vector difference and skips a B macroblock only if it
  repeats its neighbor.

Measured gain of field prediction at equal rate (no adaptive quantization, luma PSNR) [M]:
`crowd_run` 1080i +0.30 dB, `crowd_run` 480i +0.13, `pedestrian_area` 1080i +0.05, `football`
−0.06, `mobcal` −0.01. For comparison ffmpeg gains +0.35, +0.14, +0.21, +0.02 and 0.00 from
`+ilme` on the same clips, and on `football` and `mobcal` ffmpeg is 0.12 to 0.15 dB *better*
with no interlace tools at all. The smoothing pass is worth +0.5 dB on `mobcal` and 0.05 to
0.1 dB elsewhere [M]. So the tools work and are on a par with ffmpeg's on the hard clip; this
corpus has no clip where field prediction is worth more than 0.35 dB to either encoder.

### 5.5 Smaller decisions

- **Bit costs in the motion decisions.** λ = quantiser_scale / 2 of the session's previous
  picture, in SAD units per bit. The zero vector is preferred when within 16 + 6 λ; the
  bidirectional mode pays 8 + 6 λ, field prediction 12 λ, intra 512 + 40 λ. The first spike
  used constants tuned for one quantizer; at high quantizers its P and B pictures were mostly
  vector bits.
- **Quality at equal rate** moved as follows against the first spike (luma PSNR, standard
  cases, §7.1): +0.2 to +1.0 dB without adaptive quantization; adaptive quantization then
  costs 0.3 to 0.5 dB of PSNR at s = 1.5 (0.8 dB at TM5's s = 2) and changes SSIM by less than
  0.003 either way [M]. It is on by default because the brief asks for it and its purpose is
  visual; nobody has looked at pictures. `--aq 0` turns it off.
- **Reconstruction for the drift check.** `--recon` writes the encoder's reconstruction of
  every picture, B pictures included, in display order; the gate compares it with ffmpeg's
  decode. This is what catches a prediction that the encoder and the syntax disagree on,
  which no decoder reports as an error.

## 6. Interface

`src/enc.h`, about 950 lines. `Encoder` owns the CUDA streams and the batches; `Batch` is one
format (geometry, device buffers, kernels' arguments); `Session` is one program.

- (Run-time opening and closing, the arena, `reserve`, `close`, `release`, `reap`: §C.6.)
- `open(SessionConfig)` puts the session in a batch of its format with a free slot, or makes
  a new batch with `capacity` slots. Device memory per session (phase A; after phase B 61.5,
  27.1 and 10.0 MB): 63.7 MB for 1080, 28.1 MB for
  720p, 10.3 MB for 480i [M].
- `submit` types the frame (I, P or B) by the session's own GOP state and queues it in coded
  order. `step` uploads the new frames and codes, for every session, the next picture in
  coded order whose frame is on the device. With `--overlap` the upload runs on a second
  stream during the coding of the previous frames (one frame more delay).
- All per-session differences inside a batch travel in `SessPar` (picture type, reference
  slots, ladder state, activity average, λ, field distances), uploaded every tick. Kernels
  take a list of the sessions that take part, so a retry or a tick with few active sessions
  costs only those sessions.
- A process with several frame rates ticks at the highest; the driver lets 29.97 Hz sessions
  alternate between the two phases of a 59.94 Hz tick.
- One synchronization point per tick was added (statistics down, quantizers up); there are
  three now. Since phase B they are three per tick for all batches together (`Encoder::step()`
  runs `begin` / `quantize` / `retry` / `finish_gpu` / `collect` for every batch in turn), and
  the host waits by sleeping and polling (`Encoder::wait`). Host CPU at capacity was 0.02 to
  0.06 core for all sessions together in phase A and is 0.14 to 0.19 core now [M].

## 7. Correctness

Final gate run: `results/check/full.json`, 54 cases, none failed, 94 to 119 s for the full set and 32 s for the quick set of 15. Every case: ffmpeg decodes with
`-xerror -err_detect explode` and no message; libmpeg2 agrees with ffmpeg to 64 dB or better;
the encoder's reconstruction of every picture agrees with ffmpeg's decode to 64 dB or better
(float against integer IDCT); frame count, picture types and stream fields are as intended.

### 7.1 Standard cases

| Case | Mbit/s | PSNR Y / U / V, dB | SSIM | ffmpeg Mbit/s | ffmpeg PSNR Y | ffmpeg SSIM | ΔPSNR Y | ΔSSIM | Old encoder PSNR Y | Drift, dB | 2nd decoder, dB | Result |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1080i-crowd_run | 15.23 | 26.91 / 32.39 / 32.78 | 0.7796 | 15.11 | 27.30 | 0.7908 | -0.39 | -0.0111 | 26.93 | 67.6 | 68.9 | ok |
| 1080i-pedestrian_area | 15.23 | 37.97 / 41.75 / 42.47 | 0.9439 | 15.08 | 38.76 | 0.9517 | -0.79 | -0.0078 | 38.21 | 68.7 | 69.9 | ok |
| 1080i-park_joy | 15.24 | 24.38 / 29.41 / 34.02 | 0.7104 | 15.62 | 24.84 | 0.7203 | -0.46 | -0.0099 | failed | 69.0 | 69.6 | ok |
| 1080i-fourpeople | 15.24 | 42.91 / 46.89 / 48.08 | 0.9782 | 15.08 | 43.24 | 0.9787 | -0.33 | -0.0005 | 42.34 | 65.8 | 66.8 | ok |
| 1080i-cuts | 15.23 | 29.42 / 34.45 / 36.20 | 0.8693 | 15.22 | 30.78 | 0.8889 | -1.36 | -0.0196 | 29.59 | 68.1 | 69.0 | ok |
| 1080i-fade | 15.23 | 26.79 / 32.22 / 34.99 | 0.7967 | 15.28 | 27.13 | 0.8060 | -0.34 | -0.0093 | failed | 68.7 | 70.1 | ok |
| 1080i-grain | 15.23 | 33.63 / 34.67 / 34.83 | 0.7972 | 15.07 | 33.87 | 0.8042 | -0.24 | -0.0069 | 33.61 | 68.6 | 69.9 | ok |
| 480i-crowd_run | 4.05 | 26.37 / 32.46 / 32.62 | 0.8137 | 4.06 | 26.79 | 0.8230 | -0.42 | -0.0092 | 26.50 | 66.0 | 67.3 | ok |
| 480i-football | 4.06 | 34.96 / 38.74 / 39.14 | 0.9341 | 4.05 | 35.63 | 0.9384 | -0.67 | -0.0043 | 35.12 | 65.5 | 66.7 | ok |
| 480i-mobcal | 4.05 | 26.72 / 34.69 / 35.46 | 0.8891 | 4.05 | 28.04 | 0.9007 | -1.31 | -0.0116 | 26.70 | 65.8 | 66.7 | ok |
| 480i-cuts | 4.05 | 29.48 / 35.01 / 36.40 | 0.8932 | 4.10 | 30.45 | 0.9055 | -0.96 | -0.0123 | 29.10 | 66.6 | 68.0 | ok |
| 480i-testsrc2 | 4.05 | 45.60 / 44.24 / 44.93 | 0.9950 | 4.07 | 44.93 | 0.9925 | +0.66 | +0.0025 | 46.24 | 71.4 | 72.9 | ok |
| 480i-park_joy | 4.06 | 25.28 / 30.99 / 36.07 | 0.7804 | 4.07 | 25.79 | 0.7933 | -0.51 | -0.0129 | 25.40 | 66.8 | 68.3 | ok |
| 720p-crowd_run | 12.18 | 27.72 / 33.19 / 33.55 | 0.8262 | 12.12 | 28.38 | 0.8400 | -0.66 | -0.0138 | 28.02 | 65.5 | 67.5 | ok |
| 720p-fourpeople | 12.19 | 41.68 / 45.77 / 46.97 | 0.9762 | 12.20 | 42.68 | 0.9785 | -1.00 | -0.0023 | 41.65 | 65.1 | 67.0 | ok |
| 720p-park_joy | 12.18 | 26.03 / 30.95 / 35.51 | 0.7879 | 12.12 | 26.91 | 0.8075 | -0.88 | -0.0196 | 26.17 | 66.3 | 68.2 | ok |
| 720p-cuts | 12.18 | 30.88 / 35.86 / 37.43 | 0.9034 | 12.21 | 31.64 | 0.9138 | -0.76 | -0.0104 | 30.80 | 66.3 | 68.1 | ok |
| 720p-fade | 12.18 | 28.19 / 33.43 / 36.07 | 0.8486 | 12.18 | 28.92 | 0.8629 | -0.74 | -0.0143 | 28.36 | 66.4 | 68.7 | ok |
| 720p-still | 9.89 | 33.79 / 38.37 / 38.70 | 0.9502 | 11.70 | 33.63 | 0.9476 | +0.16 | +0.0027 | 32.10 | 66.3 | 67.9 | ok |
| 1080p-crowd_run | 15.22 | 28.63 / 33.15 / 33.64 | 0.8212 | 15.11 | 29.15 | 0.8324 | -0.52 | -0.0112 | 28.97 | 66.9 | 68.4 | ok |
| 1080p-pedestrian_area | 15.23 | 40.11 / 43.74 / 45.02 | 0.9564 | 15.05 | 40.43 | 0.9593 | -0.32 | -0.0029 | 40.33 | 67.9 | 69.3 | ok |
| 1080p-cuts | 15.23 | 31.31 / 35.65 / 37.24 | 0.8979 | 15.20 | 32.63 | 0.9108 | -1.32 | -0.0129 | 31.12 | 67.7 | 68.8 | ok |

- "Old encoder" is the first spike's binary on the same clips (`results/check/baseline-run.txt`);
  it failed `park_joy` 1080i (45 buffer underflows) and overshot `fade` 1080i.
- Against ffmpeg the gap is −0.2 to −1.0 dB of luma PSNR on most clips with adaptive
  quantization on, and −0.3 to +0.2 dB with it off on the six interlaced clips tried
  (`mobcal` −0.8). The largest gaps are `cuts` (ffmpeg
  spends more on the pictures after a cut) and `mobcal` (fine detail in slow motion: the
  search's half-sample step and the hierarchical search are the suspects; not investigated).
- ffmpeg's rate is 0.5 to 4 % above its target on some clips; this encoder's mean rate over
  5 s is 1.5 % above target because the stream starts with an empty mux queue and fills it to
  its operating point (15 % of the buffer).

### 7.2 Torture: the buffer model

| Case | Target, Mbit/s | Got | PSNR Y | Normative model: least occupancy | Operating model: least delivery margin, ms | Extra passes per picture | Ladder pictures | All-skipped | I replaced by P | ffmpeg produced, Mbit/s | Result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1080i-crowd_run-half | 7.50 | 7.62 | 24.45 | 35.0 % | 174.2 | 0.00 | 0 | 0 | 0 | 10.69 | ok |
| 1080i-crowd_run-quarter | 3.75 | 3.96 | 20.08 | 8.5 % | 42.6 | 0.41 | 140 | 29 | 0 | 10.42 | ok |
| 1080i-noise | 15.00 | 15.37 | 12.21 | 5.8 % | 29.1 | 0.26 | 59 | 3 | 0 | 129.69 | ok |
| 1080i-noise-quarter | 3.75 | 3.99 | 12.00 | 9.4 % | 46.6 | 0.53 | 90 | 7 | 0 | failed | ok |
| 1080i-cuts-quarter | 3.75 | 3.96 | 18.27 | 6.1 % | 30.2 | 0.56 | 116 | 35 | 0 | 7.90 | ok |
| 720p-crowd_run-half | 6.00 | 6.09 | 25.20 | 68.0 % | 339.7 | 0.00 | 0 | 0 | 0 | 6.54 | ok |
| 720p-crowd_run-quarter | 3.00 | 3.19 | 21.51 | 15.3 % | 76.1 | 0.21 | 253 | 20 | 0 | 6.30 | ok |
| 720p-noise | 12.00 | 12.30 | 12.29 | 15.9 % | 79.5 | 0.18 | 118 | 0 | 0 | 114.12 | ok |
| 720p-noise-quarter | 3.00 | 3.32 | 11.96 | 8.9 % | 44.4 | 0.43 | 179 | 9 | 0 | failed | ok |
| 720p-cuts-quarter | 3.00 | 3.20 | 21.56 | 17.0 % | 84.2 | 0.31 | 172 | 27 | 0 | 4.76 | ok |
| 480i-crowd_run-half | 2.00 | 2.03 | 24.10 | 43.7 % | 218.3 | 0.01 | 0 | 0 | 0 | 2.07 | ok |
| 480i-crowd_run-quarter | 1.00 | 1.05 | 22.23 | 24.0 % | 118.2 | 0.03 | 9 | 1 | 0 | 1.94 | ok |
| 480i-noise | 4.00 | 4.09 | 12.66 | 17.8 % | 81.9 | 0.28 | 57 | 0 | 0 | 21.43 | ok |
| 480i-noise-quarter | 1.00 | 1.09 | 12.04 | 8.6 % | 42.1 | 0.33 | 73 | 3 | 0 | failed | ok |
| 480i-cuts-quarter | 1.00 | 1.06 | 23.36 | 18.9 % | 92.8 | 0.24 | 49 | 4 | 0 | 1.51 | ok |
| 1080i-park_joy-quarter | 3.75 | 3.96 | 18.14 | 6.4 % | 31.9 | 0.48 | 140 | 37 | 0 | 14.42 | ok |
| 1080i-still | 15.00 | 12.41 | 32.44 | 8.9 % | 44.4 | 0.26 | 0 | 0 | 0 | 14.63 | ok |
| 480i-still | 4.00 | 3.33 | 32.11 | 8.8 % | 40.3 | 0.33 | 0 | 0 | 0 | 3.91 | ok |
| 1080p-noise-quarter | 3.75 | 4.02 | 12.04 | 14.5 % | 72.1 | 0.52 | 90 | 5 | 0 | failed | ok |

- **No violation in any case, in either model**, and none reported by the encoder's own
  accounting. The first spike's encoder fails 17 of these 19 (`baseline-run.txt`), with up to
  287 underflows in 300 pictures.
- ffmpeg `mpeg2video` cannot do this: at the quarter rates it prints "rc buffer underflow"
  and produces two to three times the target rate (column "ffmpeg produced"), so its PSNR in
  `results/check/ref.json` for these cases is not at equal rate.
- The price is quality: at a quarter of the nominal rate `crowd_run` 1080i is coded mostly on
  the ladder (no coefficients, some all-skipped pictures). An I picture cannot go below about
  80 bits per macroblock (six DC differences and End of Block codes); at 3.75 Mbit/s that is
  five frame periods of delivery, so the pictures after it starve. This is the content's
  floor in MPEG-2, not a rate-control accident.
- Extra quantization passes per picture: none at half rate, 0.2 to 0.3 on noise at the
  nominal rate, 0.2 to 0.6 at a quarter rate. Each is one `k_quant` and packing pass for that
  session only.
- No key frame was replaced in any case. Before the reserve for the next key frame was made
  to depend on the smallest possible I picture (§5.3), 7 of the 10 key frames of
  `crowd_run` 1080i at a quarter rate were replaced by all-skipped P pictures; the rule costs
  0.5 to 0.9 dB on the 1080i quarter-rate cases (more all-skipped P and B pictures instead).

### 7.3 Features

| Case | What is checked beyond the standard checks | Detail | Result |
|---|---|---|---|
| 1080i-crowd_run-ratewalk | rate changes, operating buffer model, constant sequence header | set → used per GOP, Mbit/s: 12.0 → 13.8, 3.4 → 6.2, 11.6 → 13.4, 3.6 → 5.8, 7.5 → 8.6, 6.1 → 6.3, 3.1 → 6.0, 3.2 → 4.7, 4.5 → 5.8, 3.5 → 3.5; 10 sequence headers, 0 differ; least delivery margin 44 ms | ok |
| 720p-cuts-ratewalk | rate changes, operating buffer model, constant sequence header | set → used per GOP, Mbit/s: 9.6 → 13.1, 2.7 → 2.0, 9.3 → 10.7, 2.9 → 4.2, 6.0 → 6.9, 4.8 → 4.9, 2.5 → 3.5, 2.6 → 4.2, 3.6 → 3.1, 2.8 → 4.0; 10 sequence headers, 0 differ; least delivery margin 186 ms | ok |
| 480i-football-ratewalk | rate changes, operating buffer model, constant sequence header | set → used per GOP, Mbit/s: 3.2 → 3.7, 1.1 → 1.1, 3.2 → 3.6, 1.2 → 1.2, 2.2 → 2.5, 1.8 → 1.9, 1.0 → 1.2, 1.1 → 1.2, 1.4 → 1.5, 1.1 → 1.2; 10 sequence headers, 0 differ; least delivery margin 260 ms | ok |
| 1080i-cuts-ratestep | rate changes, operating buffer model, constant sequence header | set → used per GOP, Mbit/s: 15.0 → 20.7, 3.8 → 3.3, 3.8 → 6.0, 15.0 → 15.1, 15.0 → 15.0, 5.0 → 6.5, 15.0 → 17.4, 4.0 → 7.7, 12.0 → 13.8, 12.0 → 12.0; 10 sequence headers, 0 differ; least delivery margin 207 ms | ok |
| 720p-crowd_run-ratenow | rate changes, operating buffer model, constant sequence header | 7 changes at arbitrary frames, 2.5 to 12.0 Mbit/s; 10 sequence headers, 0 differ; least delivery margin 342 ms | ok |
| 1080i-pedestrian_area-cc | cc_data extracted by ffmpeg, per frame, display order | captions in display order: ok | ok |
| 720p-fourpeople-cc | cc_data extracted by ffmpeg, per frame, display order | captions in display order: ok | ok |
| 480i-crowd_run-forcekey | key frames on demand |  | ok |
| 720p-crowd_run-forcekey | key frames on demand |  | ok |
| 480i-football_bff-bff | top_field_first = 0 |  | ok |
| 1080i-crowd_run-stagger4 | every session's stream, GOP phase per session | 4 streams decoded and buffer-checked | ok |
| 480i-cuts-stagger7-overlap | every session's stream, GOP phase per session | 7 streams decoded and buffer-checked | ok |
| mixed-formats | 1080i × 2, 720p × 2, 480i × 4 in one process, staggered, rate walks | 8 streams decoded and buffer-checked | ok |

- Rate changes: 10 key frames, each with a new rate between 20 % and 100 % of the ceiling;
  all ten sequence headers are byte-identical; both buffer models hold. A GOP after an
  increase uses up to 1.1 Mbit more than rate × duration (it fills the queue to its operating
  point). A GOP after a large decrease can use more than the new rate, funded by the old rate
  the mux still delivers during D (§5.1); on content that fits the new rate (`football`) the
  GOPs land within 10 %, on content that does not (`crowd_run` 1080i at 3.4 Mbit/s) the first
  GOP is 60 to 95 % over and the ladder takes over as the buffer fills.
- `--rate-now` (`720p-crowd_run-ratenow`): seven changes at arbitrary frames, between 2.5 and
  12 Mbit/s, effective at the next coded picture; both models hold.
- Captions: ffmpeg returns the `cc_data` of every frame, 20 triplets per frame at 29.97 and
  10 at 59.94, in display order (the synthetic payload carries the display index).
- Key frames on demand, including two in consecutive frames: types as planned, GOP headers
  closed.
- Sessions at different GOP phases (4 × 1080i; 7 × 480i with overlapped upload) and three
  formats in one process: every stream decodes and passes the normative buffer check.

### 7.4 What the gate does not prove

No stream was put in a transport stream, through the modulator or on a TV. No formal MPEG-2
conformance verifier was run; the evidence is two software decoders and my own buffer
simulations. The operating model is checked against the rate schedule the encoder itself
reports. PSNR and SSIM are the only quality measures.

## 8. Capacity at the end of phase A

(History; §0.1 and §0.3 have the current figures.)

### 8.1 Paced capacity

`cap99` and `cap999` as defined in §2, `crowd_run`, 25 s per run (749 ticks at 29.97 Hz, so
99.9 % means no late tick; cap999 is the largest *tested* count that had none). `baseline*` is the first
spike's encoder (SD at 704 wide, same harness); `features-serial` is the end-of-phase encoder
with every tool on, the upload inside the tick and all sessions in the same GOP phase, which
is the like-for-like comparison with `baseline`; `realistic` adds staggered GOP phases and the
overlapped upload, and is the workload to optimize. [M]

| Run | Format | cap99 | cap999 | Mean tick at cap99, ms | 99th pct, ms | On time | GPU busy | W | °C start→max | Host CPU, cores | Tested N: on-time fraction |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `baseline` | 1080i | 28 | 25 | 30.1 | 33.0 | 99.86 % | 92 % | 62 | 73→82 | 0.012 | 25: 100.0 %, 28: 99.9 %, 29: 84.2 %, 30: 35.5 %, 34: 0.0 % |
| `baseline` | 1080p | 27 | 27 | 29.7 | 33.1 | 100.00 % | 78 % | 61 | 82→90 | 0.013 | 24: 100.0 %, 26: 100.0 %, 27: 100.0 %, 28: 73.1 %, 33: 0.0 % |
| `baseline` | 720p | 26 | – | 12.6 | 16.4 | 99.51 % | 74 % | 58 | 85→85 | 0.023 | 26: 99.5 %, 27: 96.3 %, 28: 93.0 %, 31: 78.1 %, 36: 0.0 % |
| `baseline` | 480i | 160 | 160 | 29.1 | 33.1 | 100.00 % | 77 % | 61 | 82→90 | 0.037 | 148: 100.0 %, 160: 100.0 %, 164: 91.4 %, 168: 85.7 %, 176: 30.3 %, 200: 0.0 % |
| `baseline-overlap` | 1080i | 31 | 31 | 28.4 | 32.1 | 100.00 % | 87 % | 64 | 83→90 | 0.015 | 29: 100.0 %, 30: 100.0 %, 31: 100.0 %, 32: 98.6 %, 34: 44.3 %, 39: 0.0 % |
| `baseline-overlap` | 1080p | 33 | 32 | 29.4 | 33.0 | 99.72 % | 74 % | 64 | 83→90 | 0.013 | 29: 100.0 %, 32: 100.0 %, 33: 99.7 %, 34: 72.6 %, 40: 0.0 % |
| `baseline-overlap` | 720p | 29 | 28 | 11.6 | 15.9 | 99.65 % | 66 % | 62 | 81→89 | 0.025 | 25: 100.0 %, 28: 100.0 %, 29: 99.7 %, 30: 98.9 %, 31: 95.3 % |
| `baseline-overlap` | 480i | 192 | 192 | 28.5 | 32.7 | 100.00 % | 88 % | 64 | 82→91 | 0.043 | 176: 100.0 %, 192: 100.0 %, 196: 96.1 %, 200: 84.0 %, 208: 59.3 %, 244: 0.0 % |
| `features-serial` | 1080i | 23 | 23 | 29.5 | 32.6 | 100.00 % | 77 % | 63 | 77→87 | 0.021 | 21: 100.0 %, 23: 100.0 %, 24: 83.8 %, 25: 44.9 %, 29: 0.0 % |
| `features-serial` | 1080p | 24 | 23 | 29.9 | 32.3 | 99.45 % | 90 % | 62 | 83→90 | 0.022 | 21: 100.0 %, 23: 100.0 %, 24: 99.4 %, 25: 95.8 %, 29: 0.0 % |
| `features-serial` | 720p | 27 | 23 | 15.5 | 16.5 | 99.65 % | 91 % | 63 | 82→90 | 0.035 | 23: 99.9 %, 26: 99.9 %, 27: 99.7 %, 28: 58.7 %, 32: 0.0 % |
| `features-serial` | 480i | 144 | 144 | 30.6 | 33.2 | 100.00 % | 91 % | 62 | 83→91 | 0.053 | 124: 100.0 %, 136: 100.0 %, 144: 100.0 %, 148: 50.4 %, 172: 0.0 % |
| `realistic` | 1080i | 29 | 29 | 32.4 | 33.0 | 100.00 % | 93 % | 68 | 86→91 | 0.023 | 24: 100.0 %, 28: 100.0 %, 29: 100.0 %, 30: 0.0 %, 33: 0.0 % |
| `realistic` | 1080p | 31 | 31 | 31.9 | 32.6 | 100.00 % | 94 % | 67 | 83→91 | 0.025 | 26: 100.0 %, 30: 100.0 %, 31: 100.0 %, 32: 3.9 %, 35: 0.0 % |
| `realistic` | 720p | 34 | 32 | 15.7 | 16.2 | 99.65 % | 91 % | 67 | 83→91 | 0.041 | 27: 100.0 %, 32: 100.0 %, 34: 99.7 %, 35: 98.1 %, 36: 0.0 %, 37: 0.0 % |
| `realistic` | 480i | 176 | 152 | 31.4 | 32.3 | 99.72 % | 91 % | 66 | 82→90 | 0.064 | 152: 100.0 %, 168: 99.4 %, 176: 99.7 %, 180: 98.3 %, 208: 0.0 % |

- With every tool on, the like-for-like capacity fell by 18 % (1080i), 11 % (1080p) and 10 %
  (480i), and rose by one channel for 720p.
- Staggering the GOP phase is worth much more than it costs: with it the 99th percentile of
  the tick is within 2 to 3 % of the mean, where the in-phase runs had 6 to 30 % between
  them (the I-picture tick). 720p gains most: 29 → 34 with the overlapped upload.
- End of phase against the first spike with overlapped upload: 1080i 29 against 31, 1080p 31
  against 33, 720p 34 against 29, 480i 176 against 192. In macroblocks per second at cap99:
  7.1 M (1080i), 7.6 M (1080p), 7.3 M (720p), 7.0 M (480i).
- All the capacity runs are warm (82 to 91 °C) and at 58 to 68 W of the card's 70 W.

### 8.2 Free-running throughput and the cost of each tool

Macroblocks per second at a fixed session count (16; 96 for 480i), three runs, spread =
(max − min) / mean. Each row differs from `f-min` by one switch, so the differences between
rows are the tools' costs. [M]

| Run | 1080i: Mmb/s (spread) | vs `baseline` | 720p: Mmb/s (spread) | vs `baseline` | 480i: Mmb/s (spread) | vs `baseline` |
|---|---|---|---|---|---|---|
| `baseline`  | 7.56 (0.6 %) | +0.0 % | 7.07 (0.1 %) | +0.0 % | 7.16 (1.5 %) | +0.0 % |
| `f-min` --aq 0 --field-pred 0 --smooth 0 | 6.92 (0.4 %) | -8.5 % | 6.56 (0.0 %) | -7.2 % | 6.99 (0.4 %) | -2.4 % |
| `f-aq` --aq 1 --field-pred 0 --smooth 0 | 6.88 (0.3 %) | -9.0 % | 6.56 (0.3 %) | -7.2 % | 6.92 (0.1 %) | -3.3 % |
| `f-field` --aq 0 --field-pred 1 --smooth 0 | 6.43 (0.3 %) | -14.9 % | 6.53 (0.3 %) | -7.7 % | 6.52 (0.1 %) | -8.9 % |
| `f-smooth` --aq 0 --field-pred 0 --smooth 1 | 6.63 (0.1 %) | -12.3 % | 6.18 (7.2 %) | -12.5 % | 6.70 (0.3 %) | -6.4 % |
| `f-all`  | 6.23 (0.3 %) | -17.6 % | 6.30 (0.2 %) | -10.9 % | 6.31 (0.8 %) | -11.8 % |
| `f-all-stagger` --stagger | 6.26 (0.2 %) | -17.1 % | 6.25 (1.0 %) | -11.7 % | 6.26 (0.5 %) | -12.6 % |
| `baseline-overlap` --overlap | 8.55 (0.1 %) | +13.1 % | 8.33 (0.6 %) | +17.8 % | 8.62 (0.1 %) | +20.4 % |
| `f-all-stagger-overlap` --stagger --overlap | 7.25 (0.7 %) | -4.1 % | 7.39 (0.3 %) | +4.6 % | 7.38 (0.4 %) | +3.1 % |
| `end-serial`  | 6.35 (1.1 %) | -16.0 % | 6.39 (0.4 %) | -9.6 % | 6.40 (0.4 %) | -10.6 % |
| `end-realistic` --stagger --overlap | 7.40 (0.3 %) | -2.0 % | 7.49 (0.3 %) | +6.0 % | 7.47 (0.3 %) | +4.3 % |

Cost per tool, from those rows:

| Tool | 1080i | 720p | 480i | Where the time goes |
|---|---|---|---|---|
| New pipeline with every tool off (`f-min`): transform pre-pass with statistics, separate quantization kernel, per-session parameters, third synchronization, buffer ladder | −8.5 % | −7.2 % | −2.4 % | `quant+entropy` 2.95 ms where the old `entropy` was 1.80 (16 × 1080i); the transform itself got 0.25 ms cheaper |
| Adaptive quantization | −0.6 % | 0.0 % | −0.9 % | `k_mbstat` (which also decides the DCT type) |
| Field prediction (two seeds, half-sample step, bidirectional field SAD) | −7.0 % | – | −6.6 % | `k_me`: +1.29 ms on 6.50 ms per tick |
| Vector smoothing pass | −4.2 % | −5.7 % (noisy) | −4.1 % | `k_mvsmooth`: +0.7 ms per tick |
| All tools (`f-all`) | −17.6 % | −10.9 % | −11.8 % | |
| Staggered GOP phase | +0.5 % | −0.9 % | −0.9 % | Within the noise free-running; the gain is in the paced percentiles |
| Overlapped upload, on top of all tools | +15.7 % | +18.4 % | +18.0 % | The upload and `k_pad` leave the tick's critical path |

End of phase, final binary: `end-serial` 6.35 / 6.39 / 6.40 and `end-realistic` 7.40 / 7.49 / 7.47 million macroblocks per second for 1080i / 720p / 480i (last two rows of the table; these are the labels to diff the next change against) [M].

### 8.3 Stage times

`--profile` synchronizes after every stage. [M]

Stage times, `baseline`  (ms per tick; N = 16 × 1080i, 16 × 1080p, 16 × 720p, 96 × 480i):

| Stage | 1080i | share | 1080p | share | 720p | share | 480i | share |
|---|---|---|---|---|---|---|---|---|
| upload+pad | 4.84 | 27.9 % | 4.92 | 27.2 % | 2.22 | 26.6 % | 4.93 | 27.6 % |
| pyramid+ME | 6.41 | 37.0 % | 6.49 | 35.9 % | 2.83 | 34.0 % | 6.63 | 37.1 % |
| pred+dct+q | 3.30 | 19.1 % | 3.72 | 20.6 % | 1.70 | 20.4 % | 3.33 | 18.6 % |
| entropy | 1.80 | 10.4 % | 1.99 | 11.0 % | 1.19 | 14.3 % | 1.87 | 10.4 % |
| recon+pyr | 0.80 | 4.6 % | 0.81 | 4.5 % | 0.30 | 3.6 % | 0.81 | 4.5 % |
| download | 0.15 | 0.9 % | 0.15 | 0.9 % | 0.10 | 1.1 % | 0.30 | 1.7 % |
| host | 0.00 | 0.0 % | 0.00 | 0.0 % | 0.00 | 0.0 % | 0.00 | 0.0 % |
| total | 17.31 | | 18.08 | | 8.33 | | 17.87 | |

Stage times, `features-serial`  (ms per tick; N = 16 × 1080i, 16 × 1080p, 16 × 720p, 96 × 480i):

| Stage | 1080i | share | 1080p | share | 720p | share | 480i | share |
|---|---|---|---|---|---|---|---|---|
| upload+pad | 4.95 | 24.5 % | 5.05 | 24.7 % | 2.27 | 24.4 % | 5.05 | 24.6 % |
| pyramid+ME+mbstat | 8.00 | 39.5 % | 7.62 | 37.3 % | 3.40 | 36.5 % | 8.24 | 40.0 % |
| pred+dct+stats | 3.01 | 14.9 % | 3.27 | 16.0 % | 1.40 | 15.1 % | 2.67 | 13.0 % |
| rate control (host) | 0.04 | 0.2 % | 0.04 | 0.2 % | 0.03 | 0.4 % | 0.15 | 0.7 % |
| quant+entropy | 2.97 | 14.7 % | 3.15 | 15.4 % | 1.62 | 17.4 % | 2.67 | 13.0 % |
| scatter | 0.17 | 0.8 % | 0.17 | 0.9 % | 0.11 | 1.2 % | 0.35 | 1.7 % |
| recon+pyr | 0.85 | 4.2 % | 0.86 | 4.2 % | 0.31 | 3.3 % | 0.86 | 4.2 % |
| download | 0.15 | 0.8 % | 0.16 | 0.8 % | 0.09 | 1.0 % | 0.31 | 1.5 % |
| host | 0.10 | 0.5 % | 0.11 | 0.5 % | 0.06 | 0.6 % | 0.28 | 1.4 % |
| total | 20.25 | | 20.44 | | 9.31 | | 20.59 | |

Stage times, `realistic` --stagger --overlap (ms per tick; N = 16 × 1080i, 16 × 1080p, 16 × 720p, 96 × 480i):

| Stage | 1080i | share | 1080p | share | 720p | share | 480i | share |
|---|---|---|---|---|---|---|---|---|
| upload+pad | 0.09 | 0.5 % | 0.09 | 0.5 % | 0.06 | 0.8 % | 0.24 | 1.4 % |
| pyramid+ME+mbstat | 9.79 | 55.2 % | 8.69 | 52.0 % | 3.96 | 50.1 % | 9.36 | 54.8 % |
| pred+dct+stats | 3.41 | 19.2 % | 3.40 | 20.3 % | 1.52 | 19.3 % | 2.80 | 16.4 % |
| rate control (host) | 0.05 | 0.3 % | 0.05 | 0.3 % | 0.03 | 0.4 % | 0.15 | 0.9 % |
| quant+entropy | 3.15 | 17.8 % | 3.21 | 19.2 % | 1.72 | 21.8 % | 2.72 | 16.0 % |
| scatter | 0.17 | 1.0 % | 0.18 | 1.1 % | 0.12 | 1.5 % | 0.35 | 2.1 % |
| recon+pyr | 0.82 | 4.6 % | 0.82 | 4.9 % | 0.33 | 4.2 % | 0.82 | 4.8 % |
| download | 0.16 | 0.9 % | 0.16 | 1.0 % | 0.10 | 1.3 % | 0.33 | 1.9 % |
| host | 0.10 | 0.6 % | 0.11 | 0.7 % | 0.04 | 0.5 % | 0.29 | 1.7 % |
| total | 17.73 | | 16.71 | | 7.89 | | 17.07 | |

Read with the overlapped upload in mind: in `realistic` the upload's `k_pad` kernels run on
the second stream at the same time as the motion search, which is why that stage is 1.3 to
1.8 ms longer there than in `features-serial`. The time has not gone away; it moved.

### 8.4 One tick on the timeline

Nsight Systems, 16 × 1080i, free-running (`results/nsys/`). [M]

End-of-phase encoder, staggered, serial upload (`new-stagger-tick.txt`): tick 20.2 ms, GPU busy 19.6 ms (96.8 %), idle 0.65 ms. First spike's encoder (`baseline-tick.txt`): tick 17.1 ms, busy 98.1 %, idle 0.33 ms. Median tick of the new encoder:

```
timeline of a median tick (20.235 ms): start offset ms, duration ms, idle gap before ms, activity
     0.010    3.776    0.010  memcpy H2D x18
     3.787    1.252    0.002  k_pad x3
     5.041    0.005    0.002  memcpy H2D x3
     5.047    0.003    0.001  k_set_mode
     5.051    0.000    0.002  memcpy H2D
     5.052    0.320    0.001  k_down2 x2
     5.373    6.522    0.001  k_me
    11.896    0.474    0.001  k_mvsmooth
    12.370    0.508    0.001  k_mbstat
    12.879    2.689    0.001  k_xf
    15.569    0.154    0.001  k_rowstat_part
    15.723    0.042    0.001  k_rowstat
    15.768    0.007    0.002  memcpy D2H
    15.998    0.006    0.223  memcpy H2D x2
    16.021    1.273    0.017  k_quant
    17.295    0.693    0.001  k_ent_resolve
    17.988    0.885    0.001  k_ent_pack
    18.874    0.036    0.001  k_ent_rowsum
    18.910    0.005    0.001  k_ent_picsum
    18.918    0.001    0.003  memcpy D2H
    19.030    0.050    0.111  memset x16
    19.085    0.070    0.004  k_ent_scatter
    19.156    0.000    0.002  memcpy H2D
    19.157    0.639    0.001  k_recon
    19.797    0.144    0.001  k_down2 x2
    19.946    0.121    0.005  memcpy D2H x16
```

- The GPU is busy 97 to 98 % of a serial tick. Idle time is 0.3 to 0.65 ms per tick: about
  0.22 ms while the host computes quantizers between the two halves, 0.11 ms before the 16
  per-session `memset` calls, the rest spread over small copies.
- Launch overhead is small at this session count: 21 kernel launches per tick at 5 µs of host
  time each, 43 `cudaMemcpyAsync` at 3.7 µs, 16 `cudaMemsetAsync`; a kernel starts 15 µs
  (median) after the API call when the GPU is idle. At 480i session counts the per-session
  calls (one upload copy, one memset, one download copy per session) multiply by ten; that
  was not traced.
- The upload is 3.8 ms of copies (13 MB per ms, 13 GB/s, on the PCIe 4.0 x8 link) plus
  1.25 ms of `k_pad` for 16 × 1080i.

### 8.5 Temperature, power, sustained load

- The card throttles. `nvidia-smi -q` reports a cumulative "SW Thermal Slowdown" time; it read
  12.9 s after the baseline capacity runs (not recorded before them), 28.3 s after the 20
  minutes of the two end-of-phase capacity runs, and 74.3 s after the 3-minute sustained run
  and the last benchmarks, all at 90 to 91 °C [M].
  The graphics clock moves between 2445 MHz (cold, 60 °C), 2070 to 2130 MHz (hot) and
  1980 MHz at the lowest in the sustained run, 13 to 19 % lower [M].
- Free-running, the end-of-phase encoder did 6.42 M macroblocks/s for 16 × 1080i started at
  58 °C, 6.35 M at 63 °C and 6.23 M at 78 °C (three benchmark runs with the same switches;
  the rate control differed slightly between them) [M]: about 3 % between cold and warm, as
  in the first spike (4 %). What the throttling costs at capacity was not isolated. A
  capacity number carries its temperature.
- Power at capacity is 58 to 68 W of 70 W.
- Sustained run: 29 × 1080i, realistic workload, 180 s (`sustain-1080i`): 99.94 % of 5,365 ticks on time, mean tick 32.2 ms, 99th percentile 33.0 ms, 99.9th 33.2 ms of a 33.4 ms period; 68 W mean, 69 W peak; 81 → 91 °C; clock down to 1980 MHz; host CPU 0.022 core; no other GPU process [M]. The margin at cap99 is thin: the mean tick is 97 % of the period.
- Mixed formats, paced: three formats in one process on a 59.94 Hz tick, staggered, overlapped upload, 20 s each (`mixed-*.txt`) [M]: 8 × 1080i + 6 × 720p + 28 × 480i (4.35 M macroblocks/s) 99.1 % on time, mean tick 11.2 of 16.7 ms; 9 + 7 + 32 (4.96 M) 99.65 %, mean 12.0 ms; 10 + 8 + 36 (5.58 M) 57 %, mean 14.4 ms, 99th percentile 18.9 ms (that run was at 89 °C). So a mixed line-up holds about 5.0 M macroblocks/s where one format holds 7.0 to 7.6 M. Three batches run one after the other inside a 16.7 ms tick, each with its own synchronizations and its own kernels that take 0.7 ms however little they have to do (`k_ent_resolve`); the next phase should measure this case as well as the single-format one.

## 9. Hardware-counter profile of the baseline (first spike's encoder)

Nsight Compute 2026.1.1 under sudo, `--set full` plus the warp-state sampling section;
4 sessions of 1080i `crowd_run`, 70 kernel launches covering P and B pictures and an anchor
reconstruction; text export of every section of every launch in
`results/ncu/baseline-details.txt`, per-kernel summary in `baseline-summary.txt`, all metrics
in `baseline-raw.csv`. [M]

Per kernel, `baseline` (averages per launch; kernels with at least 2 % of the profiled kernel time):

| Metric | `k_me` | `k_xq` | `k_ent_resolve` | `k_ent_pack` | `k_pad` | `k_dct_type` |
|---|---|---|---|---|---|---|
| Launches profiled | 4 | 6 | 6 | 6 | 17 | 4 |
| Time per launch, ms | 1.620 | 0.820 | 0.572 | 0.314 | 0.091 | 0.140 |
| Share of kernel time | 32.6 % | 24.8 % | 17.3 % | 9.5 % | 7.8 % | 2.8 % |
| Registers per thread | 255 | 48 | 40 | 38 | 16 | 34 |
| Achieved occupancy | 16.4 % | 82.2 % | 13.5 % | 55.0 % | 83.3 % | 74.3 % |
| Issue slots busy | 37.9 % | 30.1 % | 6.9 % | 17.6 % | 64.3 % | 9.8 % |
| Warp instructions, millions | 109.20 | 46.09 | 0.33 | 7.94 | 10.81 | 2.37 |
| Warp cycles per issued instruction | 5.23 | 32.73 | 28.51 | 42.97 | 15.34 | 90.47 |
| Threads per warp instruction | 31.5 | 28.9 | 15.0 | 8.3 | 32.0 | 32.0 |
| Not predicated off | 31.3 | 27.2 | 14.3 | 7.8 | 27.6 | 31.8 |
| Branch efficiency | 92.6 % | 95.8 % | 76.8 % | 87.0 % | 100.0 % | 100.0 % |
| Divergent branches | 1289 | 379 | 57 | 1669 | 0 | 0 |
| Eligible warps per scheduler | 0.46 | 0.69 | 0.07 | 0.24 | 1.50 | 0.17 |
| Global loads, M | 9.39 | 3.24 | 0.02 | 0.59 | 0.12 | 0.26 |
| Global stores, M | 0.00 | 0.56 | 0.04 | 0.05 | 0.12 | 0.00 |
| Local memory instr., M | 12.82 | 0.56 | 0.00 | 0.00 | 0.00 | 0.00 |
| Shared memory instr., M | 0.00 | 0.64 | 0.00 | 0.00 | 0.00 | 0.00 |
| Memory throughput | 58.3 % | 79.9 % | 8.2 % | 28.1 % | 19.6 % | 60.6 % |
| L1 hit | 92.5 % | 83.1 % | 61.2 % | 48.7 % | 31.6 % | 7.7 % |
| L2 hit | 94.0 % | 88.3 % | 97.1 % | 33.8 % | 51.4 % | 93.0 % |
| DRAM read, MB | 27.53 | 39.90 | 0.78 | 18.10 | 3.91 | 8.36 |
| DRAM written, MB | 1.43 | 31.69 | 0.00 | 1.23 | 0.01 | 0.43 |

Warp states, `baseline`: average number of warps in each state per cycle in which an instruction is issued. The column sum is the warp cycles per issued instruction; in brackets the share of PC samples in that state.

| State | `k_me` | `k_xq` | `k_ent_resolve` | `k_ent_pack` | `k_pad` | `k_dct_type` |
|---|---|---|---|---|---|---|
| Long scoreboard (waiting for an L1TEX result: global or local memory) | 1.54 (29.9 %) | 10.33 (31.6 %) | 18.68 (63.8 %) | 36.80 (85.7 %) | 6.94 (48.9 %) | 34.37 (47.6 %) |
| Short scoreboard (shared memory, other short-latency units) | 0.00 (0.0 %) | 0.92 (2.9 %) | 0.00 (0.0 %) | 0.01 (0.0 %) | 0.95 (5.8 %) | 0.03 (0.1 %) |
| LG throttle (local/global memory queue full) | 0.91 (16.9 %) | 8.12 (24.4 %) | 6.32 (18.2 %) | 0.01 (0.0 %) | 0.00 (0.0 %) | 52.89 (48.0 %) |
| MIO throttle (shared-memory and other MIO queue full) | 0.00 (0.0 %) | 4.41 (13.2 %) | 0.00 (0.0 %) | 0.01 (0.0 %) | 0.11 (0.6 %) | 0.05 (0.0 %) |
| Math pipe throttle | 0.12 (2.2 %) | 0.37 (1.1 %) | 0.03 (0.1 %) | 0.22 (0.4 %) | 0.80 (4.7 %) | 0.15 (0.2 %) |
| Wait (fixed-latency dependency) | 1.29 (24.7 %) | 1.42 (4.4 %) | 1.65 (8.5 %) | 2.74 (6.9 %) | 3.04 (18.9 %) | 0.77 (1.2 %) |
| Not selected (eligible, another warp issued) | 0.22 (4.1 %) | 1.28 (3.9 %) | 0.07 (0.3 %) | 0.37 (0.6 %) | 1.33 (7.8 %) | 0.74 (0.7 %) |
| Selected (issuing) | 1.00 (19.2 %) | 1.00 (3.1 %) | 1.00 (5.3 %) | 1.00 (2.4 %) | 1.00 (6.2 %) | 1.00 (1.5 %) |
| No instruction (fetch) | 0.04 (0.8 %) | 0.06 (0.2 %) | 0.10 (0.5 %) | 0.31 (0.7 %) | 0.13 (0.6 %) | 0.05 (0.0 %) |
| Branch resolving | 0.08 (1.5 %) | 0.16 (0.5 %) | 0.62 (3.0 %) | 1.08 (2.5 %) | 0.23 (1.5 %) | 0.02 (0.0 %) |
| Barrier (`__syncthreads`) | 0.00 (0.0 %) | 4.29 (13.4 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) |
| Dispatch stall | 0.02 (0.4 %) | 0.18 (0.5 %) | 0.01 (0.0 %) | 0.16 (0.3 %) | 0.43 (2.5 %) | 0.06 (0.1 %) |

Reading:

- `k_me` (33 % of kernel time here): 255 registers per thread, 16 % occupancy, 2 warps per
  scheduler of which 0.46 is eligible; 5.2 warp cycles per instruction, split between
  issuing (1.0), waiting for loads (1.5), fixed-latency waits (1.3) and a full local/global
  memory queue (0.9); 12.8 of its 109 million warp instructions per launch touch its stack
  arrays. Threads rarely diverge (31.5 of 32 active, 93 % uniform branches).
- `k_xq` (25 %): 33 cycles per instruction at 82 % occupancy; a third waiting for loads, a
  quarter throttled on the global memory queue, an eighth each on the shared-memory queue and
  at barriers. 72 MB of DRAM traffic per launch.
- `k_ent_resolve` (17 %) and `k_ent_rowsum`: one thread per slice; 7 % and 2 % of issue slots
  busy. They take their time whatever the session count.
- `k_ent_pack` (9.5 %): 86 % of samples waiting for loads, 8.3 of 32 threads active.
- `k_dct_type`: 90 cycles per instruction, the worst of the kernels with real work: byte
  loads down the columns of a macroblock.

## 10. Hardware-counter profile at the end of phase A

(§0.4 has the profile of the current binary.)

Same tool and workload with every tool on and staggered GOP phases (the four sessions are at
different picture types in each launch), 80 launches from the steady part of the run;
`results/ncu/features-*`. [M]

Per kernel, `features` (averages per launch; kernels with at least 2 % of the profiled kernel time):

| Metric | `k_me` | `k_ent_resolve` | `k_xf` | `k_ent_pack` | `k_pad` | `k_quant` | `k_recon` | `k_mbstat` | `k_down2` | `k_mvsmooth` |
|---|---|---|---|---|---|---|---|---|---|---|
| Launches profiled | 4 | 4 | 4 | 4 | 12 | 4 | 4 | 4 | 16 | 4 |
| Time per launch, ms | 2.011 | 0.791 | 0.762 | 0.342 | 0.106 | 0.285 | 0.161 | 0.159 | 0.032 | 0.122 |
| Share of kernel time | 38.5 % | 15.1 % | 14.6 % | 6.6 % | 6.1 % | 5.5 % | 3.1 % | 3.1 % | 2.4 % | 2.3 % |
| Registers per thread | 255 | 64 | 56 | 44 | 18 | 96 | 39 | 40 | 20 | 77 |
| Achieved occupancy | 16.4 % | 13.2 % | 65.5 % | 48.2 % | 83.6 % | 27.6 % | 90.3 % | 82.9 % | 85.0 % | 39.8 % |
| Issue slots busy | 37.3 % | 4.9 % | 40.1 % | 15.6 % | 69.6 % | 19.5 % | 47.1 % | 12.7 % | 67.9 % | 21.6 % |
| Warp instructions, millions | 129.86 | 0.33 | 56.97 | 6.33 | 13.71 | 9.64 | 13.53 | 3.54 | 3.97 | 4.26 |
| Warp cycles per issued instruction | 5.32 | 37.84 | 19.58 | 40.24 | 14.23 | 17.90 | 23.21 | 78.13 | 14.94 | 23.25 |
| Threads per warp instruction | 31.2 | 20.5 | 28.0 | 10.5 | 32.0 | 27.8 | 25.8 | 32.0 | 32.0 | 12.7 |
| Not predicated off | 31.0 | 19.2 | 25.6 | 9.9 | 28.4 | 26.7 | 23.9 | 31.6 | 29.0 | 11.9 |
| Branch efficiency | 89.9 % | 63.4 % | 69.4 % | 88.0 % | 100.0 % | 92.6 % | 84.3 % | 100.0 % | 100.0 % | 40.8 % |
| Divergent branches | 2039 | 81 | 4982 | 990 | 0 | 200 | 895 | 0 | 0 | 998 |
| Eligible warps per scheduler | 0.46 | 0.05 | 0.85 | 0.20 | 1.86 | 0.26 | 1.04 | 0.22 | 2.26 | 0.26 |
| Global loads, M | 10.83 | 0.02 | 3.52 | 0.45 | 0.39 | 0.53 | 0.65 | 0.26 | 0.21 | 0.55 |
| Global stores, M | 0.01 | 0.04 | 0.53 | 0.03 | 0.13 | 0.28 | 0.26 | 0.00 | 0.03 | 0.01 |
| Local memory instr., M | 14.32 | 0.00 | 0.21 | 0.00 | 0.00 | 0.00 | 0.00 | 0.28 | 0.00 | 0.00 |
| Shared memory instr., M | 0.00 | 0.00 | 0.93 | 0.00 | 0.00 | 0.00 | 0.20 | 0.00 | 0.00 | 0.00 |
| Memory throughput | 56.6 % | 8.1 % | 83.0 % | 25.3 % | 26.1 % | 82.9 % | 51.7 % | 66.2 % | 51.2 % | 61.9 % |
| L1 hit | 91.7 % | 67.9 % | 89.1 % | 50.8 % | 63.1 % | 16.0 % | 71.8 % | 13.2 % | 62.9 % | 87.7 % |
| L2 hit | 96.1 % | 96.5 % | 84.3 % | 26.8 % | 52.3 % | 35.2 % | 48.8 % | 94.6 % | 21.8 % | 34.6 % |
| DRAM read, MB | 27.65 | 1.04 | 33.12 | 17.68 | 4.15 | 37.71 | 16.66 | 8.36 | 3.76 | 16.03 |
| DRAM written, MB | 5.66 | 0.00 | 24.02 | 1.01 | 0.02 | 13.63 | 1.93 | 0.00 | 0.00 | 0.36 |

Warp states, `features`: average number of warps in each state per cycle in which an instruction is issued. The column sum is the warp cycles per issued instruction; in brackets the share of PC samples in that state.

| State | `k_me` | `k_ent_resolve` | `k_xf` | `k_ent_pack` | `k_pad` | `k_quant` | `k_recon` | `k_mbstat` | `k_down2` | `k_mvsmooth` |
|---|---|---|---|---|---|---|---|---|---|---|
| Long scoreboard (waiting for an L1TEX result: global or local memory) | 1.59 (30.4 %) | 22.31 (58.5 %) | 6.40 (32.9 %) | 34.61 (86.5 %) | 5.54 (44.6 %) | 12.58 (70.7 %) | 12.79 (56.1 %) | 40.49 (57.6 %) | 5.09 (36.3 %) | 19.24 (82.8 %) |
| Short scoreboard (shared memory, other short-latency units) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.75 (3.9 %) | 0.01 (0.0 %) | 0.77 (4.7 %) | 0.21 (1.2 %) | 0.39 (1.6 %) | 0.03 (0.0 %) | 0.54 (3.5 %) | 0.02 (0.1 %) |
| LG throttle (local/global memory queue full) | 0.86 (15.8 %) | 12.27 (27.1 %) | 2.85 (14.2 %) | 0.01 (0.0 %) | 0.00 (0.0 %) | 1.98 (9.9 %) | 0.00 (0.0 %) | 34.04 (37.0 %) | 0.00 (0.0 %) | 0.05 (0.1 %) |
| MIO throttle (shared-memory and other MIO queue full) | 0.00 (0.0 %) | 0.00 (0.0 %) | 2.40 (11.9 %) | 0.01 (0.0 %) | 0.11 (0.7 %) | 0.03 (0.1 %) | 0.13 (0.6 %) | 0.04 (0.1 %) | 0.11 (0.7 %) | 0.01 (0.0 %) |
| Math pipe throttle | 0.14 (2.7 %) | 0.03 (0.0 %) | 0.50 (2.5 %) | 0.17 (0.3 %) | 1.04 (6.7 %) | 0.21 (1.0 %) | 0.78 (3.1 %) | 0.13 (0.2 %) | 1.64 (10.7 %) | 0.17 (0.6 %) |
| Wait (fixed-latency dependency) | 1.32 (24.9 %) | 1.49 (6.2 %) | 1.50 (7.9 %) | 2.67 (6.5 %) | 2.99 (19.3 %) | 1.13 (6.9 %) | 2.23 (9.7 %) | 1.17 (1.9 %) | 2.60 (17.5 %) | 1.63 (7.3 %) |
| Not selected (eligible, another warp issued) | 0.23 (4.2 %) | 0.07 (0.1 %) | 1.11 (5.5 %) | 0.29 (0.5 %) | 1.67 (10.2 %) | 0.34 (1.6 %) | 1.20 (4.7 %) | 0.74 (0.9 %) | 2.33 (15.3 %) | 0.23 (0.8 %) |
| Selected (issuing) | 1.00 (18.8 %) | 1.00 (4.1 %) | 1.00 (5.1 %) | 1.00 (2.6 %) | 1.00 (6.6 %) | 1.00 (5.8 %) | 1.00 (4.4 %) | 1.00 (1.8 %) | 1.00 (6.5 %) | 1.00 (4.4 %) |
| No instruction (fetch) | 0.05 (0.9 %) | 0.20 (0.8 %) | 0.17 (0.9 %) | 0.31 (0.7 %) | 0.11 (0.6 %) | 0.06 (0.4 %) | 0.17 (0.7 %) | 0.04 (0.1 %) | 0.13 (0.7 %) | 0.18 (0.8 %) |
| Branch resolving | 0.08 (1.7 %) | 0.69 (2.9 %) | 0.36 (2.0 %) | 0.97 (2.3 %) | 0.19 (1.4 %) | 0.20 (1.4 %) | 0.37 (1.6 %) | 0.02 (0.0 %) | 0.16 (1.2 %) | 0.52 (2.3 %) |
| Barrier (`__syncthreads`) | 0.00 (0.0 %) | 0.00 (0.0 %) | 2.19 (11.3 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 3.67 (15.3 %) | 0.00 (0.0 %) | 0.00 (0.0 %) | 0.00 (0.0 %) |
| Dispatch stall | 0.02 (0.5 %) | 0.00 (0.0 %) | 0.20 (1.0 %) | 0.12 (0.2 %) | 0.50 (3.1 %) | 0.04 (0.2 %) | 0.40 (1.8 %) | 0.03 (0.0 %) | 0.79 (5.1 %) | 0.04 (0.2 %) |

Reading, kernel by kernel (shares are of kernel time in this 4-session profile; at 16 or more
sessions the two kernels that are serial per slice shrink in share, see the stage times):

- **`k_me`, 38 %.** It is short of warps, not of anything to compute. 255 registers per
  thread leave 8 warps per SM, 2 per scheduler; 0.46 of them is eligible on average, and the
  issue slots are busy 37 % of the time. A warp needs 5.3 cycles per instruction: 1.0 issuing,
  1.6 waiting for a load (long scoreboard), 1.3 on fixed-latency dependencies, 0.9 because the
  local/global memory queue is full. 14.3 million of its 130 million warp instructions per
  launch are loads and stores to its own stack arrays (`c`, `c1`, `c2`, `win`, `pf`, `pb`),
  10.8 million are global loads. Divergence is small (31.2 of 32 threads active, 90 % of
  branches uniform). More warps cannot be had without removing the arrays, and removing the
  arrays (packed pixels in registers) also removes most of the instructions. The field search
  added in this phase is already in that style and costs 20 % of the kernel for two extra
  searches per field and reference.
- **`k_xf`, 15 %.** 19.6 cycles per instruction at 66 % occupancy: 6.4 waiting for loads,
  2.9 and 2.4 on full memory queues (global and shared), 2.2 at the two barriers. Branch
  efficiency is 69 % with 5,000 divergent branches per scheduler: the eight threads of a block
  agree, but the four blocks in a warp can be intra, forward, backward, bidirectional, frame-
  or field-predicted. It reads 33 MB and writes 24 MB of DRAM per launch (1.0 and 0.74 kB per
  macroblock).
- **`k_quant`, 5.5 %.** 96 registers (its 64-entry output array) hold occupancy to 28 %; 71 %
  of its warp samples wait for loads: each thread reads 64 coefficients one by one from 64
  planes. It moves 38 MB in and 14 MB out per launch for 0.29 ms, so it is DRAM-bound as
  well.
- **`k_ent_resolve`, 15 %.** 0.79 ms per launch at any session count: one thread per slice
  walks 120 macroblocks, 0.33 million instructions in total, 38 cycles per instruction, issue
  slots 5 % busy. It is a latency chain, not work. It sets a floor of about 0.7 ms under the
  tick.
- **`k_ent_pack`, 6.6 %.** 86 % of warp samples wait for loads; L1 hit rate 51 %, L2 27 %:
  it reads levels scattered over 384 planes. Only 10.5 of 32 threads are active on average
  (skipped macroblocks and loops of unequal length).
- **`k_mbstat`, 3.1 %.** 78 cycles per instruction, half waiting for loads and half throttled:
  it walks each macroblock column by column (my loop order) with byte loads; L1 hit rate 13 %.
- **`k_mvsmooth`, 2.3 %.** 12.7 of 32 threads active: most macroblocks have nothing to test.
- **`k_pad`, 6.1 %** (12 launches) and **`k_down2`, 2.4 %**: byte-wise copies at 70 % issue
  slot use; cheap per launch, many launches.
- **`k_recon`, 3.1 %.** 12.8 cycles on loads, 3.7 at barriers; runs for anchors only.

## 11. Optimization targets as ranked at the end of phase A

(History. Phase B did items 1, 3, 5 and most of 7 and 8, tried item 2 and parts of 4 and 6;
§0.2 and §0.8 have what each gave, §0.9 the current ranking. The estimate for item 1 was +18 to
+26 %; it measured +32 to +36 %. Items 2 and 6 as written here did not pay.)

The yardstick is the realistic workload: 16 × 1080i staggered with overlapped upload takes
17.7 ms of GPU work per tick (§8.3): motion search stage 9.8 ms, of which `k_me` 6.5,
`k_pad` running alongside about 1.3, `k_mvsmooth` 0.5, `k_mbstat` 0.5, pyramids 0.3;
transform and statistics 3.4; quantize and pack 3.15 (`k_quant` 1.3, `k_ent_resolve` 0.7,
`k_ent_pack` 0.9); reconstruction 0.8; the rest 0.6. Capacity scales as the inverse of that
sum. Gains are [E] unless marked; items refer to the research note (`RN`) where it has the
mechanism.

| # | Target | Evidence in the profile | Expected effect on the 17.7 ms tick | Capacity |
|---|---|---|---|---|
| 1 | **Packed-pixel frame search in `k_me`** (RN §2.3): 32-bit loads, `__vsadu4`, `__vavgu4`, no stack arrays, forward and backward as separate threads | 255 registers, 16 % occupancy, 2 warps per scheduler, 14 M local-memory instructions per launch, 5.3 cycles per instruction. The packed field code does two searches per field for 20 % of the kernel | Frame part 5.2 ms → 1.5–2.5 ms: −2.7 to −3.7 ms | +18 to +26 % |
| 2 | **Fewer candidates** (RN §2.1, §2.2): search window around predictors; B pictures seeded from the anchors' vectors | B pictures are two thirds of the pictures and search two references; first spike: ±11 pixels cost no PSNR on two clips [M] | After item 1 the frame search is small; a further −0.5 to −1 ms | +3 to +6 % |
| 3 | **Drop `k_pad`**: `cudaMemcpy2DAsync` straight into the padded planes; a small kernel for the bottom rows of 1080 | `k_pad` is 1.25 ms per tick at 16 × 1080i [M, nsys] and competes with the search when the upload is overlapped (the search stage is 1.3–1.8 ms longer there) | −1.2 ms | +7 % |
| 4 | **Quantization**: fold it into `k_xf`'s second pass with a predicted quantizer and keep `k_quant` for the sessions whose prediction missed (the row statistics are still produced and decide that), or give `k_quant` eight threads per block | `k_quant` 1.3 ms: 96 registers, 28 % occupancy, 71 % of samples waiting for loads, 52 MB of DRAM traffic per 4 sessions | −0.8 to −1.1 ms | +5 to +6 % |
| 5 | **Entropy without the serial pass** (RN §2.5): warp scans for slice state, compact run/level lists written at quantization time | `k_ent_resolve` 0.7 ms at 5 % issue slots; `k_ent_pack` 0.9 ms at 86 % load waits and 10.5 of 32 threads active | −0.8 to −1.1 ms | +5 to +6 % |
| 6 | **`k_xf` traffic and divergence**: sort or group blocks by mode so that a warp is uniform; store coefficients only for blocks that can be non-zero at the finest quantizer in use | 69 % branch efficiency, 5,000 divergent branches per scheduler; 57 MB of DRAM traffic per launch | −0.5 to −1 ms | +3 to +6 % |
| 7 | **`k_mbstat` and `k_mvsmooth`**: row-major packed loads; compute activity inside `k_xf` pass 1 (it has the pixels); compact the smoothing candidates | 78 cycles per instruction and 13 % L1 hits; 12.7 of 32 threads active | −0.6 ms | +3 % |
| 8 | **Synchronization, launches, batches**: quantizer choice on the device (the model is a few hundred operations per session), one memset and one download for all sessions, CUDA graphs; let the batches of different formats overlap on separate streams | 0.65 ms idle per tick at 16 sessions [M]; grows with the session count at 480i (not traced). Mixed formats hold 5.0 M macroblocks/s against 7.0–7.6 M for one format (§8.5) [M] | −0.4 ms at 16 × 1080i; more at 160 × 480i; most for mixed line-ups | +2 % (1080i); up to +30 % for mixed line-ups |
| 9 | **Cooling and clock** | Clock 13–20 % lower hot than cold; 61 s of thermal slowdown accumulated in this phase's capacity and sustained runs [M] | Not a code change | +3 % measured free-running; more at sustained capacity, not isolated |

Together items 1 to 8 would bring the tick from 17.7 ms to about 9 to 10 ms, that is 50 to
55 × 1080i or 60 to 65 × 720p on this card [E]. They overlap less than the research note's
list because they are taken from disjoint parts of the measured tick; the estimate for item 1
is the uncertain one and should be measured first.

What the profile says is *not* worth doing: raising occupancy with `-maxrregcount` (first
spike: slower); integer DCT (the transform kernels wait on memory, not on arithmetic);
anything for upload throughput beyond item 3 (it is already off the critical path).

Order of work suggested: 1, 3, 4, 5, then measure again before 2, 6, 7, 8. After each step:
`make check`, `make bench LABEL=…` with the realistic switches, `bench_diff.py`, and the
profile of the kernel that was changed.

## 12. Unfinished and not verified

(Written at the end of phase A; still true after phase B unless marked. §0.10 adds the items of
phase B.)

Unfinished in this phase:

- The interface has no scheduler thread, deadline or per-session error path (§4 table).
- A key frame replaced by an all-skipped P picture is not rescheduled; a forced key frame
  restarts the cadence.
- `repeat_first_field` and film pulldown; field pictures were not evaluated.
- The capacity cost of each tool was measured free-running (§8.2); the paced capacity was
  measured for the baseline, for all tools together and for the realistic workload, not tool
  by tool.
- `cap999` is the largest *tested* session count with 99.9 %, which the bisection does not
  aim at (`bench.py` now walks down from cap99 for it; the tables of §8 were made before
  that, those of §0 after).
- The timeline was traced for 16 × 1080i only; the profile for 4 sessions only. (Phase B traced
  the mixed line-up as well; the hardware-counter profile is still for 4 × 1080i.)
- The mux contract of §5.1 is a specification; no mux exists yet to hold up its side.

Not verified:

- Any TV, the modulator, a transport stream, a formal conformance verifier.
- That adaptive quantization, the smoothing pass and the ladder look acceptable. All
  judgments here are PSNR and SSIM.
- Rate control over minutes; all clips are 5 s. Drift of the long-run mean rate was not
  measured beyond the 5 s cases (about +1.5 %, the initial queue fill).
- Whether deployed decoders treat `vbv_delay` = 0xFFFF differently from real values (SCTE 43
  §7.1.5's note says they ignore the field; not tested).
- The encoder next to Plex or Jellyfin transcodes: no other GPU process appeared in any
  benchmark run (`pmon` recorded none), so the interference is unmeasured. (Phase B met a
  Plex transcode and measured it: §0.7.)
- Operation above 91 °C or for longer than three minutes at capacity.

## 13. First spike: what still holds, and what went wrong then

### 13.1 Profiling counters

`nix develop -c ./build/saxpy` proved the toolchain (`cudaPackages_13_2`: nvcc 13.2.86, Nsight
Compute 2026.1.1.2, pinned because the driver reports CUDA 13.2). Hardware counters work only
as root on this host (`ERR_NVGPUCTRPERM` for the user); under sudo they work, GPU passthrough
does not break them [M]. To allow profiling without sudo the owner could add
`boot.extraModprobeConfig = "options nvidia NVreg_RestrictProfilingToAdminUsers=0";` and
reboot (NVIDIA's page for the error; `modinfo nvidia` lists the parameter [M]; not applied).

### 13.2 CPU baseline

ffmpeg 8.1 `mpeg2video`, one thread, raw 4:2:0 input, CBR, `-bf 2`, closed GOP of 0.5 s,
`+ildct+ilme` for interlaced, 720×480 SD; cores per channel = CPU seconds per second of video
[M]:

| Format | testsrc2 | crowd_run | pedestrian_area |
|---|---|---|---|
| 480i, 4 Mbit/s | 0.068 | 0.119 | 0.121 |
| 720p59.94, 12 Mbit/s | 0.175 | 0.287 | 0.320 |
| 1080i, 15 Mbit/s | 0.434 | 0.788 | 0.852 |

With the quality options of SPEC §8.2 (`-mbd rd -trellis 2 -cmp 2 -subcmp 2 -intra_vlc 1
-non_linear_quant 1 -qmax 28 -dc 10`) the cost is 5 to 7 times higher for at most 0.4 dB [M].
So 8 vCPUs carry about 8 channels of 1080i or 20 of 720p with default options, before
anything else runs.

### 13.3 Other measurements of the first spike

- Several processes instead of one: 2 to 8 processes change throughput by at most 2 % and
  raise host CPU per session sevenfold [M]. One process, one context.
- Entropy coding on the CPU: 0.036, 0.19 and 0.20–0.22 core per 480i, 720p and 1080i channel,
  plus 33 to 205 MB/s of download per channel [M]. On 8 vCPUs the bit packing has to stay on
  the GPU. (That code path was removed in phase A.)
- Source frames already on the device (no upload) gave the same throughput as the overlapped
  upload [M]: NVDEC input would save host CPU and PCIe traffic, not add channels.
- Search range: ±11 pixels gave the same PSNR as ±35 on `crowd_run` and `pedestrian_area` and
  10 % more throughput; ±67 cost 20 % [M].
- `__usad` on bytes made `k_me` 15 % slower; `-maxrregcount` 96 and 64 a further 6 % and
  57 % [M].
- Quality of the first spike against ffmpeg: 0.3 to 0.65 dB below at equal rate on two clips
  [M]. Its buffer model failed whenever the quantizer saturated (§7.2, column "old encoder").

### 13.4 What went wrong on nixark in the first spike

- **Out of memory.** A multi-process comparison started 24 encoder processes, each pinning
  0.78 GB of input. At 09:28:46 UTC the kernel OOM killer killed Plex (restarted by systemd),
  Jellyfin (restarted), the user service manager, and nzbget, which systemd did not restart.
  At the start of phase A `systemctl --failed` listed no failed unit [M]; someone had
  restarted it. Since then everything runs inside `scripts/run.sh` (§2).
- One `sudo chown` on a profiler report, outside the permission "sudo for the profiler only".

