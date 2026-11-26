# K2 + G2 running notes (decisions, state, open points)

Working log of the last two packages (K2: conversion kernels in image and ABI; G2: capacity
parity and the pipeline harness). Newest entries override the briefs. Started 2026-11-25 15:26.

## How things are run

- Worktree `~/totalblonder-wt/G2`, branch `port/G2`. GPU host `nixark`:
  `NIXARK_DIR=totalblonder-rs-g2 scripts/nixark.sh CMD`; baseline tree (commit ff56264, before
  K2) in `~/totalblonder-rs-g2-base`. The oracle's own tools are built there with
  `./scripts/run.sh make all` in `spikes/cuda-mpeg2` (`build/devenv.sh` copied from an older
  tree to skip `nix print-dev-env`; `media` is a symlink to `~/totalblonder-spike/media`).
- Removed on nixark for space: `~/totalblonder-rs-d1`, `-d1-base`, `-k3-evidence`.

## K2 (done on the CPU side, commit 92ddb15)

- `spikes/cuda-mpeg2/src/pipe_kernels.h` holds the kernels (moved unchanged from `pipe.h`);
  `pipe.h` includes it. All conversion kernels are integer arithmetic: no float rule applies.
- Records `Taps`, `ConvClass`, `ConvJob`, `RectJob`, `CscPar`, `PreJob` are in `tb-kernel-abi`
  (sizes 24 / 144 / 56 / 80 / 52 / 24), in `cbindgen.toml`, the layout test and
  `kernels/abi_layout.cc`; the header was regenerated.
- Seven kernels added to `kernels/tb_kernel_list.h` (24 in the image now) and to
  `tb_device::kernel` (`ConvH`, `ConvV`, `ConvCopy`, `Half`, `FillRect`, `Overlay`, `Csc`) with
  argument kinds `class` (`ConvClassRef`), `rect` (`RectRef`), `csc` (`CscPar`).
- **Foreign device memory (the protocol design G2 needed)**: `tb_device::kernel::DevRef` is a
  place in device memory as plain data (space in bits 63..48: 0 arena, s + 1 foreign slot s;
  offset in bits 47..0). Job records that hold addresses are written by the core in handle
  form (`ConvJobRef`, `PreJobRef`, same layout as the ABI records) and uploaded with
  `Op::UploadRefs { dst, src, kind }`; the adapter resolves the handles (`RefKind::resolve`,
  safe code shared by both adapters) before anything is queued and refuses the plan whole
  (`BadPlan`) when a slot is not bound or an offset is outside its space. After the submit the
  host region is unspecified (the CUDA adapter resolves in place). Binding is the adapter's:
  `CudaDevice::bind_foreign(slot, base, len)` (unsafe: the caller keeps the memory alive until
  it is unbound and every plan submitted meanwhile has completed), `unbind_foreign()`,
  `CudaDevice::context()` (the CUcontext for the decoders); `RefDevice::bind_foreign(slot,
  len) -> &mut [u8]` owns stand-in memory. In recordings the upload is recorded in handle form
  (deterministic); in normal form it is an `upload`.
- `tb_convert::Lowerer`: `ConvOp`s to plan steps (placement of the converter's buffers in a
  region of the device arena, tap tables and job records through a scratch region of the host
  arena, kernel records). `CONV_JOB_BYTES` / `PRE_JOB_BYTES` come from the ABI types.
- `Kernel::index()` is a match on the variant now (was a linear search over names).
- Tests: `crates/tb-device-ref/tests/convert.rs`, 11 tests, every kernel against a Rust model
  written from its definition (exact), through `Converter` + `Lowerer` + `RefDevice`, sources
  in foreign slots and in the arena; `kernel_signatures_agree` covers 24 kernels.

- K2 on the card (2026-11-25, nixark): the oracle built from the tree before K2
  (`~/totalblonder-rs-g2-base`, ff56264) and from the K2 tree: `check-pipe` 46 / 46 both, all
  228 coded streams of the gate byte-identical between the two; `make check` 15 / 15 with the
  K2 tree. (Log: scratch `k2_oracle.log`; the conversion kernels' machine code is untouched:
  the move changed no token of them.)
- `gate/ref-small` failed in `nix flake check` on bagel for a reason outside the change:
  ffmpeg, shown ten processors by the builder, notes "too many threads/slices (10), reducing
  to 9" for a 144-line picture, and the gate counts every decoder message as a fault. The
  gate's decode now ignores that note (`check.py: decode_ffmpeg`).

## G2.1 The churn divergence: traced, a deliberate parity exception

`scripts/churn_trace.py ORACLE RUST` (removable layer). Result on nixark, 2026-11-25:

| Run | Rust vs oracle-naive | Rust vs oracle-reserve | oracle-reserve twice | oracle-reserve vs oracle-naive |
|---|---|---|---|---|
| `--churn 5:6` | 0 of 29 differ | 7 | 0 | 7 |
| `--churn 3:8 --overlap` | 0 of 37 differ | 9 | 0 | 9 |

- The 16 streams are exactly the streams in which the ORACLE differs from ITSELF between its
  two ways of opening a session (`reserve`: batch from the side thread, the session opens in
  the first tick after the thread is ready; `naive`: the batch is made in the tick). The Rust
  port, which has no side thread by decision (ARCHITECTURE 4.1), is byte-identical to the
  oracle's `naive` run with the same seed on all 66 streams.
- Picture by picture, each of the 16: the reserve stream has 1 to 3 pictures fewer (the
  session opened that many of its frame periods later; a pool session's frames depend only
  on how many it has been given), all pictures but the last one or two are byte-identical,
  and the tail differs because the flush at the close (waiting B pictures closed with a P)
  falls on another frame: e.g. `g3.1.c0` 49 against 50 pictures, 48 identical, then `B`
  against `BP`; `g4.1.c0` 192 against 195, 190 identical, `PB` against `PBBPB`.
- Not a bug on either side. On its own terms the Rust behavior passes the gate's `churn`
  case (36 streams of undisturbed sessions byte-identical to a run without churn, churned
  streams start with a sequence header and an I picture, end with sequence_end_code, decode
  strictly, hold their buffer model): G1 ran it 10 of 10; repeated below with the final tree.
- Recorded as parity exception 1: `--churn S:R:reserve` in the port means `naive --arena`.

## G2.2 Encoder capacity (in progress)

- `scripts/bench_capacity.py` (apps `gate-bench`: Rust alone; `gate-bench-compare`: with the
  oracle): interleaved binaries, pmon looked at before every run and sampled during it (use,
  not presence: an idle transcoder context is not counted), disturbed runs discarded and
  repeated right after the other process's burst (`--retries`, `--wait`), every run kept in
  `results/bench/<label>.json`. A binary can carry options (`--bin "name=PATH --flag"`).
- The card was never free: a Plex transcode ran through the whole session (1 to 11 % SM, 2 %
  decoder, in bursts). Most runs are flagged; the comparison rests on interleaving.
- First free-run series (g2-free-1, before the fixes; clean runs only where there were any):
  Rust / oracle 1080i 1.001, 1080p 0.982, 720p 0.990, 480i 0.996, mixed 0.989. Host time per
  tick of the Rust driver: between phases 0.12-0.15 ms (16 x 1080i), 0.30-0.41 ms (96 x 480i),
  0.12-0.14 ms (mixed 8 units); queueing 0.20 / 0.27 / 0.28 ms.
- Finding: with several batches (formats) the port queued a phase only after the host work of
  ALL batches (one plan per phase); the C++ queues each batch's operations before it turns to
  the next (`for (auto &b : batches) b->quantize()`), so the device works on batch 1 while
  the host does batch 2's rate control. Fix: `EncoderOptions::split_plans` + `Step::Queue` +
  `Encoder::queued` (the quantize and finish plans are handed out per batch; same operations
  in the same order on each stream, checked by `split_plans_queue_the_same_operations`);
  driver default, `--one-plan` to compare. Off in replays (plan boundaries differ).
- Fix: `Encoder::recycle(Vec<u8>)`: the driver hands written access-unit bytes back
  (`--no-recycle` to compare).
- `Kernel::index()` precomputed (K2 commit).
- `WaitFor(FRAMES)` instead of the Sync plan: built as `EncoderOptions::frames_event`
  (driver `--frames-event`), off by default.
- perf on the mixed line-up (6 s runs, whole process, 6 kHz): Rust 4011 samples, oracle 3567;
  libcuda 61.9 % / 64.9 %, own code 9.0 % / 10.1 % (the oracle's includes the cudart
  wrappers), memmove 6.4 % / 3.9 %, mutex lock + unlock 10.7 % / 10.1 %. The memmove
  excess was the launch marshaling: `Kernel::args()` built a 16-entry list of full-size
  values (2.8 KB) twice per launch (validate, queue) and `resolve` a second one (2.4 KB after
  K2 added `ConvClass`). Fix: `Kernel::resolve` / `Kernel::bufs_fit` without the list, each
  by-value record once (`Resolved` is under 800 bytes).
- Series g2-free-3 (R2: split plans + recycle; 4 rounds, all disturbed), Rust / oracle by
  round (median): 1080i 1.011, 1080p 0.996, 480i 0.995, 720p 0.983, mixed 0.985.
- Series g2-free-4 (variants against the default, mixed, 4 rounds): `--one-plan` 0.967 by
  round (0.892..0.995); host time in waits per tick 3.795 ms against 3.644 ms (medians);
  `--no-recycle` 1.000 (0.948..1.004), host time between phases 0.151 against 0.155 ms: no
  measurable effect of the pooled access-unit bytes at this size.
- Series g2-free-5 / 6 (R4: + compact launch arguments; R3 = before them; 5 rounds each, all
  159 + 40 runs disturbed, spreads 3 to 27 %), by round against the oracle (median, range):

  | Format | Rust (R4) | Rust before the argument fix (R3) | Rust `--frames-event` |
  |---|---|---|---|
  | 720p | 0.987 (0.974..1.022) | 0.994 (0.947..1.056) | 1.005 (0.902..1.063) |
  | 1080i | 0.991 (0.982..0.998) | 0.983 (0.973..0.995) | 1.000 (0.973..1.009) |
  | 480i | 0.994 (0.983..1.001) | 0.994 (0.991..1.014) | 0.999 (0.955..1.010) |
  | 1080p | 0.997 (0.497..1.006) | 0.994 (0.325..1.009) | 1.013 (0.970..1.017) |
  | mixed | 0.989 (0.938..1.304) | 0.991 (0.955..1.311) | 0.996 (0.908..1.322) |

  Reading: the default was about 1 % behind in every format and the run without the Sync
  plan was level with the oracle. Cause found in the adapter, not in the protocol: the Sync
  plan (a plan without steps that waits on UPLOAD) recorded a completion event on the idle
  upload stream and polled it; an event on an idle stream reads as complete only after a
  round trip to the device, about 30 us (0.5 % of a 6.7 ms tick, 1.2 % of a 2.6 ms one), where
  the C++ `cudaStreamSynchronize` returns at once. Fix: `tb-device-cuda` waits for the stream
  itself (`cuStreamQuery` / `cuStreamSynchronize`) when a plan has no steps. Protocol and
  operation sequence unchanged. (R5 has it; measured below.)
- Series g2-free-7 (R5: + the stream wait in the adapter; 4 rounds, **all 60 runs clean**,
  the first clean series), by round against the oracle (median, range):

  | Format | Rust (R5) | Rust `--frames-event` |
  |---|---|---|
  | 1080i | 0.997 (0.996..0.998) | 1.006 (1.001..1.010) |
  | 1080p | 0.992 (0.987..0.996) | 1.007 (1.003..1.013) |
  | 720p | 0.993 (0.984..0.995) | 1.002 (0.985..1.004) |
  | 480i | 0.992 (0.982..1.006) | 1.003 (0.993..1.010) |
  | mixed | 0.988 (0.986..0.992) | 0.996 (0.987..1.001) |

  So the stream wait alone did not close the gap, and the run without the Sync plan is at or
  above the oracle. Second cause, in the adapter's hybrid wait: the wait for the upload
  stream is not a phase of stable length (what is left of the frame upload when the next
  tick starts varies), and a nap of 85 % of its average overshoots. Fix: no nap when the
  wait is for a stream (R6). And the event dependency is worth having where the upload is
  long: the host prepares the tick while the upload finishes.
- Series g2-free-8 (R6: + no nap in stream waits; R5 in the same rounds; 4 rounds, 80 runs,
  none disturbed), by round against the oracle (median, range):

  | Format | Rust (R6, final) | R5 | R6 `--frames-event` |
  |---|---|---|---|
  | 1080i | 1.005 (1.004..1.009) | 0.989 (0.984..0.993) | 1.007 (0.998..1.010) |
  | 1080p | 1.006 (1.004..1.008) | 0.997 (0.996..1.001) | 1.010 (1.000..1.014) |
  | 720p | 0.999 (0.983..1.009) | 0.988 (0.979..0.993) | 1.001 (0.994..1.005) |
  | 480i | 1.004 (0.989..1.010) | 0.995 (0.986..0.997) | 1.002 (1.001..1.007) |
  | mixed | 0.995 (0.986..1.003) | 0.985 (0.980..0.994) | 0.997 (0.993..1.004) |

  Parity. Absolute (M mb/s, oracle / Rust): 1080i 19.35 / 19.47, 1080p 21.29 / 21.42, 720p
  21.99 / 21.93, 480i 17.32 / 17.35, mixed 20.21 / 20.10. Rust host per tick: between phases
  0.126 / 0.143 / 0.084 / 0.337 / 0.153 ms, queueing 0.194 / 0.203 / 0.117 / 0.274 / 0.297 ms.
- Paced (g2-paced-1, R4; g2-paced-2, R5, 67 x 1080i again): both binaries hold the oracle's
  capacity points and two steps above (table in the spike notes R.2).
- Contention (300 x 480i; `results/contend/g2-{rust,oracle}-480i300`): ratios 1.11 / 1.25 /
  1.02 (Rust) and 1.12 / 1.24 / 1.01 (oracle) next to trivial kernels, the unthrottled and
  the real-time Plex pipeline; no late tick, no isolated slow tick.
- Diagnostic without GPU work to speak of (16 x 176x144, three rounds, noisy next to Plex):
  tick 0.291 / 0.321 / 0.291 ms oracle, 0.285 / 0.282 / 0.297 ms Rust (R3): the host's own
  cost per tick is the same; Rust host time 0.022 ms between phases + 0.100 ms queueing.

## G2.3 The pipeline harness (in progress)

- Crates: `tb-timeline::{select, ingest, captions, admission}` (sans-io, by a sub-agent, 44
  tests), `tb-decode` (adapter: libav through `ffmpeg-the-third` 6.0.0 for FFmpeg 9.0, NVDEC
  via the `cuda` hwaccel in OUR context, software fallback with swscale; by a sub-agent, 29
  tests in the sandbox), `tb-pipe` (binary + library: options of m2vpipe, run loop, report).
- Protocol additions: `Op::Copy` (device to device: the slate, device-resident clips),
  `Op::UploadRefs` + `DevRef` + foreign slots (K2 section), `CudaDevice::aux_stream()`
  (non-blocking stream for libav's copies), `CudaDevice::context()`, `mem_info()`,
  `Encoder::reserve_device`, `Converter::adopt` (classes made before the tick loop).
- Lifetime rule of decoder surfaces: bound per tick (`PipeDevice::bind_surfaces`), the frames
  are held in the tick's queue until the conversion plan (submitted ahead of the encoder's
  tick on MAIN, waited for after it) has completed.
- First run on the card (2026-11-25): `h264_1080p30 -> 1080i`, offline, 1 s: coded stream,
  converted frames (`--dump-conv`) and pts log byte-identical to the C++ m2vpipe.
- `gate/pipe-ref-small` (flake check): tb-pipe on the reference adapter with software inputs.
- Apps: `gate-pipe` (pipe_check.py with tb-m2venc + tb-pipe alone), `gate-pipe-compare`
  (with the oracle's m2vpipe from `ORACLE_PIPE`, default `spikes/cuda-mpeg2/build/m2vpipe`).
- Pipeline gate, first full run (2026-11-25, nixark; `gate-pipe-compare`, oracle `m2vpipe`
  built from the K2 tree): oracle 46 / 46; Rust 45 / 46 (the one failing case is `devin`:
  `tb-m2venc --input-mode dev` was still refused at that point). Streams: 228 of the oracle;
  every `pipe-*` stream (26), every shed / hold stream and the undisturbed churn streams are
  byte-identical; different are the 16 churn-reserve streams (parity exception 1) and the 23
  `devin-*-dev` streams the Rust driver did not write yet. The nine `geom-*` cases (analytic
  pattern, no coded stream compared) pass with the same figures as the oracle.
- Pipeline gate, second full run with `--input-mode dev` in the driver (tools
  `kc7znb1…-gate-tools`, commit of the compact launch arguments): oracle 46 / 46, **Rust
  46 / 46**; 228 streams each, **212 byte-identical, 16 different = the churn-reserve
  exception**. Identical among them: 26 `pipe-*` streams, 46 `devin-*` (host and device
  input), shed, hold, the 131 other churn streams. Result file:
  `spikes/cuda-mpeg2/results/check/rust-pipe.txt`.
- `nix flake check` at that commit: 223 checks, all green.
- Not ported: `@cuvid` / `@deint` inputs (refused with a message; the C++ measured them worse).

## Pipeline on the card: line-ups, features, sustained (2026-11-25)

- Line-ups (a) to (e) at the oracle's counts, `tb-pipe` and `m2vpipe` one after the other,
  card otherwise idle, two passes: all hold with both (table in the spike notes R.3;
  `results/pipe/g2-rust.jsonl`, `g2-oracle.jsonl`, `g2f-*.jsonl`). Decoder at 89 to 96 %
  ((e) 74 to 78 %); host 0.35 / 0.40 / 0.49 / 0.39 / 0.36 core for (a) (b) (c) (d) (e).
- Features (`results/pipe/g2-features.txt`): inputs appearing at run time, a stalled input,
  admission control (refusal and routing to software decode), the software path;
  `--degrade` / `--resync` next to three heavy transcodes, `tb-pipe` and `m2vpipe` in the same
  scenario (`results/contend/g2-pipe-a26-*`, `g2-oracle-pipe-a26-*`).
- Found there: after the tick loop `tb-pipe` spent longer than `m2vpipe` before it read the
  decoders' counters (they went on decoding: 13 frames more per input and frames dropped
  from full queues in the report). The counters are read at the end of the run now; the
  report prints the time of the flush.
- The device-memory figure of `nvidia-smi dmon` for PCIe traffic (`pci rx` in the line-up
  lines) jumps between runs of either binary (37 to 301 MB/s); not a property of the port.

## Parity exceptions (deliberate differences from the C++), running list

1. `--churn S:R:reserve`: the port has no side thread for batches; a session opens in the tick
   it is asked for (= the oracle's `naive`). 16 streams of the gate differ from the oracle's
   `reserve` runs and equal its `naive` runs (G2.1).
2. `tb-pipe`: `@cuvid` and `@deint=` inputs are refused (the cuvid decoders are not ported).
3. `tb-pipe` makes the tap tables of the conversions its opened inputs will most likely need
   before the first tick in paced runs (the C++ makes each class in the tick that first uses
   it); offline runs make them when first used, as the C++ does, so that the operation order
   of the gate cases is the oracle's. The tables' contents are the same either way.
4. `tb-pipe` passes captions (A53 side data of the inputs) to the encoder when an input has
   any (`--no-cc` turns it off); the C++ harness passes none. The gate's inputs have none.
5. Software inputs (`@sw`): the scaled frame is copied from the decode thread's buffer into
   the session's frame region on the tick thread (the C++ scales into pinned memory and
   uploads from there): one more copy of the frame on the host per channel.
6. `tb-m2venc --input-mode dev` holds the clip on the device in the padded layout and copies
   whole padded planes (the C++ copies w x h with a 2-D device copy): same pictures, a few
   rows more per copy.
7. The first quantization plan and the finish plan are submitted per batch (`split_plans`),
   one plan per phase in replays; a tick has more `submit` calls and the same waits.

8. Frame times of a pass of a looping input count from the pass's first PACKET, not from
   the first frame the decoder returns (`tb_timeline::ingest::FrameTimes::packet_pts`). The
   C++ rule shifts an input for good when a pass begins while the thread skips to a key
   frame; found in the sustained runs with `--resync 600`, in both harnesses: `tb-pipe`
   (before the fix) 24 x 1080p30, 120 s: 4 inputs resynchronized at a loop restart and
   stayed 2.17 s late, their channels on the slate (16.7 % of all output frames); `m2vpipe`,
   the same run: 5 inputs, 2.17 s, 20.8 %; 7 mixed units, 150 s: more than half of the
   inputs. Ordinary passes (the first frame returned is the pass's first) get the same
   times as in the C++, so the gate's streams are unchanged.

## Hazards found on the way

- A batch made at run time is cleared by the encoder's next plan that waits on MAIN. A caller
  that writes a `FrameSource::Device` frame for a session of such a batch with a plan of its
  own BEFORE that (as `--input-mode dev` and `tb-pipe` do) would have its frame wiped.
  Guarded now: `Encoder::frame_target` refuses (`EncoderError::State`) while the session's
  batch is not set up; the callers settle first (`tb-pipe` after opening its sessions,
  `tb-m2venc` in dev mode after a tick with pool operations). A channel added to a running
  pipeline therefore costs one `settle` (a wait for MAIN between ticks) before its first
  device frame; folding the set-up into the caller's own plan would avoid that. Not done.

## Before the C++ can be deleted (checklist)

Nothing below is a port; every item is a move, a re-point or a decision.

1. **Kernel sources out of the spike directory**: `spikes/cuda-mpeg2/src/{kernels.h,
   entropy.h, common.h, vlc_tables.h, pipe_kernels.h, tb_kernel_abi.h}` to `kernels/`;
   `nix/kernels.nix` (`kernelFiles`, `includes`, `abiHeaderFresh`), `nix run .#abi-header` and
   `crates/tb-kernel-abi` docs point there. `common.h` / `vlc_tables.h` still hold the table
   VALUES the oracle uses; the kernels need only what they read (check which statics remain).
2. **The gate's machinery out of the spike directory** (it is shared today and runs against
   the Rust binaries alone): `scripts/{check.py, formats.py, vbv_check.py, pipe_check.py,
   bench.py (Sampler, line-up definitions), gpumon.py, pipe_lineup.py, contend.sh,
   contend_report.py, contend_late.py, prep_inputs.sh, prep_media.sh, capped.sh, run.sh}`,
   the baselines `results/check/{baseline,ref}.json`, and the result files worth keeping.
   `nix/gate.nix`, `scripts/gate_small.py`, `scripts/pipe_small.py`,
   `scripts/bench_capacity.py` and `scripts/gate_compare.sh` import from there
   (`GATE_SCRIPTS`). The defaults `./build/m2venc` / `./build/m2vpipe` in those scripts
   become the Rust binaries (today: `M2VENC`, `M2VPIPE`).
3. **Remove the comparison layer**: apps `gate-compare`, `gate-pipe-compare`,
   `gate-bench-compare`, `gate-oracle`; `scripts/gate_compare.sh`, `scripts/churn_trace.py`,
   `scripts/golden_au_diff.py`; the derivations `oracle`, `tapsdump`, `taps`, checks
   `kernels/oracle`, `kernels/taps`, `kernels/taps-fresh` (`nix/kernels.nix`).
4. **Golden vectors**: `golden/*.jsonl` and the `.sha256` digests stay as recorded history
   of the oracle; `golden/generate.sh` goes (it runs the oracle). From then on the vectors
   are regression data: a deliberate change of behavior regenerates them from the Rust
   encoder (`tb-m2venc --golden` writes `au` records only today; the replay tests of
   `tb-device::sim::golden` and `tb-m2v-encoder/tests/tick_golden.rs` need the `op`, `par`,
   `rowstat` ... records, so either they are frozen with the vectors or a Rust recorder of
   the full format is written first). Decide which.
5. **Options that exist only for comparison**: `--one-plan`, `--no-recycle`,
   `--frames-event`, `--input-copy` (keep as test switches or drop); `Compare::NormalForm`
   replays stay useful.
6. **Things the C++ harness had that were not ported** (decide: port or drop): `@cuvid` /
   `@deint` inputs; `--spin`; the nine-stage `--profile` of `m2venc` (the port reports times
   per plan); `decbench`, `convbench` (decoder and conversion micro-benchmarks), `gpucomp`
   (the trivial-kernel competitor of the contention runs: a few lines of CUDA, needed by
   `contend.sh`), `scripts/ncu_profile.sh` / `nsys_*.sh` (they take any binary).
7. **Known open points that the deletion does not depend on**: admission control is static (table of
   measured decoder rates), not fed by the card's actual decoder load; captions end to end
   are tested in pieces (decoder side data, carrier, encoder user data), not through one
   gate case with a captioned input; `Engine` lives in `tb-m2venc`'s library and is used by
   `tb-pipe` (move it to a crate of its own when the server binary appears).

## State at the end (2026-11-25, about 18:20 PST)

- Final binaries: gate tools `p5yxdzj0…-gate-tools` (tb-m2venc `6d1319pd…`, tb-pipe
  `16my76wx…`), built on bagel from the branch and copied to nixark.
- Gates with them, on the card: `gate-small --device cuda --golden` 5 / 5; `check-full`
  oracle 54 / 54, Rust 54 / 54, 74 streams, 0 different; `check-pipe` oracle 46 / 46, Rust
  46 / 46, 228 streams, 16 different (exception 1). `nix run .#gpu-plumbing` (the CUDA
  adapter's tests on the card, float parity among them): 12 / 12 with the final adapter.
- `nix flake check` on bagel: 223 checks green at the commit of the frame-time fix; only
  documents and result files were committed after it.
- Sustained with the final binaries: spike notes R.3.
- nixark at the end: no process of ours, `systemctl --failed` empty (system and user), no OOM
  line in the kernel log, power limit 70 W, compute policy Default, 59 GB free; scratch
  streams removed; `~/totalblonder-rs-g2` (tree + results) and `~/totalblonder-rs-g2-base`
  (the tree before K2 with the oracle built from it) left in place.
- Things on nixark to know about: (1) `perf record` at 6 kHz made the kernel lower
  `kernel.perf_event_max_sample_rate` from its default to 20000 by itself (kernel log
  00:27 UTC); it stays so until the next boot and limits only perf's own sampling rate.
  (2) Two NVRM Xid 31 lines (00:56 and about 02:20 UTC, process `device_fault_is…`): the CUDA adapter's own GPU
  test that provokes a fault on purpose to check that it is reported as `Lost`
  (`nix run .#gpu-plumbing`); its process only. (3) `perf` (from the flake's nixpkgs) was
  copied into the Nix store of nixark with `nix copy`; nothing was installed.

## Not done

- `@cuvid` / `@deint` inputs; the hybrid line-ups (`a_hyb`, `c_hyb`) and `--dec-ctx own` were
  not measured with `tb-pipe` (the options exist).
- The paced capacity search stopped two steps above the oracle's points (both binaries still
  held there); the true cap99 of today's card and cooling was not searched.
- Captions through the pipeline: no gate case with a captioned input (no such input in the
  corpus); pieces are tested (decoder side data round trip, carrier, encoder).
- Admission control works from the static table of spike notes C.2; it does not look at the
  decoder's actual load or at Plex.
- A channel added to a running pipeline (run-time open with device input) is not exercised
  by `tb-pipe` (all sessions open at start); the encoder refuses `frame_target` until the
  batch is set up, see "Hazards".
- ncu / nsys were not run.
