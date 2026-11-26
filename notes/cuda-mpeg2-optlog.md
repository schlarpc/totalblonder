# CUDA MPEG-2 encoder: optimization log (phase B)

> **History.** This document records the C++ spike and its measurements as they were; it is
> not kept up to date. The spike's programs were deleted in package X1 (2026-11-25,
> `docs/port/X1-notes.md`) and are in the git history at commit `e79069b`; its result files
> are frozen in `spikes/cuda-mpeg2/results/`. The living documentation: `docs/ARCHITECTURE.md`
> (the Rust encoder and how it is checked), `gate/README.md` (the gate and benchmark tools),
> `golden/README.md` (the golden vectors), `notes/post-port-worklist.md` (what is next).

Running log, appended after every experiment (kept or reverted). Host `nixark`, RTX 2000 Ada,
code in `spikes/cuda-mpeg2/`. Start: 2026-11-25 04:35 PST (12:35 UTC). Budget about five hours.
Times in the headings are PST, taken from the commits.

Conventions:

- "realistic" = `--stagger --overlap`, all coding tools on. Free-run numbers are million
  macroblocks per second (Mmb/s) at 16 sessions (96 for 480i), mean of 3 runs, `crowd_run`.
- Each number carries the GPU temperature at the start of the run; the card is about 3–4 %
  faster cold (below 70 °C) than warm (above 80 °C), so differences under about 2 % between
  runs at different temperatures are noise.
- Quality: luma PSNR / SSIM from the gate (`make check`, 15 cases; `check-full`, 54 cases)
  against `results/check/baseline.json`.
- [M] measured, [E] estimated.

## 0. Baseline of this phase (binary of commit a337f45)

`results/bench/p0-realistic.json`, GPU cold (48 °C at start, 62–75 °C during), no other GPU
process, load 0.3 [M]:

| Format | Mmb/s | tick ms | stage ms: ME / transform / quant+entropy / recon |
|---|---|---|---|
| 1080i × 16 | 7.75 | 16.5 | 9.09 / 3.17 / 3.05 / 0.79 |
| 720p × 16 | 7.80 | 7.3 | 3.66 / 1.40 / 1.65 / 0.33 |
| 480i × 96 | 7.71 | 16.1 | 8.79 / 2.65 / 2.68 / 0.79 |
| 1080p × 16 | 8.07 | 15.9 | – |

The previous phase's `end-realistic` (7.40 / 7.49 / 7.47) was taken warm; the same binary is
3–5 % faster cold. Power limit of the card: 70 W (default 70 W).

## 1. Packed-pixel frame search in `k_me` — KEPT (04:42)

- Why: `k_me` was 37 % of the tick: 255 registers, 16 % occupancy, byte loads, six per-thread
  pixel arrays (profile of phase A).
- What: all three pyramid levels, the half-sample ring, the zero-vector test, the
  bidirectional SAD and the activity now read 32-bit words straight from the planes
  (`__funnelshift_r`, `__vsadu4`, `__vavgu4`); no per-thread pixel arrays, no prediction
  copies. The diagonal half-sample candidates use an exact packed four-sample average
  (`avg4x`), so the decisions are the same as before.
- Check: output streams byte-identical to the baseline binary on three clips (1080i, 720p,
  480i, 60 frames each); quick gate 15/15, quality delta 0.00 dB.
- Measured (realistic, cold 51–70 °C; `p0-realistic` → `p1-me-packed`) [M]:

| Format | Mmb/s before | after | gain | ME stage ms before → after |
|---|---|---|---|---|
| 1080i × 16 | 7.75 | 10.24 | +32.2 % | 9.09 → 4.20 |
| 720p × 16 | 7.80 | 10.58 | +35.7 % | 3.66 → 1.48 |
| 480i × 96 | 7.71 | 10.27 | +33.2 % | 8.79 → 3.95 |

Re-profile after experiment 1 (`results/ncu/p1-*`, 4 × 1080i) [M]: `k_me` 0.80 ms per launch
(was 2.01), 139 registers, 45 M warp instructions (was 130 M), 16 % occupancy (a 256-thread
block with 144 registers per thread uses 36,864 of the SM's 65,536 registers, so only one block
fits); 47 % of its warp samples now wait for loads. `k_ent_resolve` 0.79 ms (flat), `k_xf` 0.76,
`k_ent_pack` 0.34, `k_pad` 0.32 (3 launches), `k_quant` 0.29, `k_recon` 0.16, `k_mbstat` 0.16.

## 2. `k_me` in 64-thread blocks — KEPT (04:47)

- Why: occupancy is limited by whole blocks; 64-thread blocks let 7 blocks (14 warps) share
  an SM instead of one block of 8 warps.
- Measured, A/B/A at 67–75 °C, 2 runs each [M]: 1080i 10.19 / 9.99 (before, first and last)
  → 10.54 (64) / 10.60 (32) / 10.48 (128); 720p 10.55 / 10.37 → 10.73 / 10.73 / 10.67.
  About +4 % 1080i, +2.5 % 720p. 64 chosen. Output unchanged (same kernel code).

## 3. `-maxrregcount` 128 / 104 / 88 on the packed `k_me` — REJECTED (04:50)

- Why: more warps per SM if the kernel fits fewer registers.
- Measured (bracketed by the 64-thread build at 10.51 before, 10.28 after) [M]: 1080i 9.98 /
  9.92 / 9.84; 720p 10.19 / 10.16 / 10.08 (before 10.73, after 10.53). 3–6 % slower: the
  compiler spills to local memory. Fewer registers must come from the code's structure.

## 4. Mixed line-up added to the benchmark (04:52)

- `bench.py` format `mixed`: N units of (1 × 1080i + 1 × 720p59.94 + 4 × 480i) in one process
  on a 59.94 Hz tick; reference N = 8 (8 + 8 + 32 sessions, 4.95 M macroblocks/s of real-time
  load). Reported with the single formats from now on (`make bench`, `make bench-cap`).
- Baseline binary (phase A) [M]: 6.77 Mmb/s free-running (tick mean 12.05 ms, p99 16.6 ms),
  13 % below its single-format 7.75. After experiments 1–2: 9.15–9.2 Mmb/s (12 % below 10.5).
- Timeline of the mixed tick after experiments 1–2 (`results/nsys/p2-mixed-tick.txt`) [M]:
  tick 9.5 ms, GPU busy 90.6 %, idle 0.9 ms; 10 synchronizations, 61 launches, 116 copies and
  memsets per tick. Kernel time: `k_xf` 19.8 %, **`k_ent_resolve` 17.1 %** (0.70 + 0.52 +
  0.22 ms: its serial pass costs the same for 4 sessions as for 16, and each batch pays it),
  `k_me` 14.9 %, `k_pad` 11.6 %, `k_quant` 10.1 %, `k_ent_pack` 8.7 %. Idle: 0.42 ms before
  the first copies of a batch (host between batches), 0.26 ms before the per-session memsets.
  So the mixed deficit is (a) the serial slice pass per batch, (b) idle gaps at the three
  synchronizations per batch, (c) the tick-to-tick variation (p99 37 % above the mean).

## 5. Slice state without the serial pass — KEPT (04:57)

- Why: `k_ent_resolve` walked every macroblock row in one thread: 0.79 ms per launch whatever
  the session count (5 % issue-slot use), 17 % of kernel time in the mixed line-up.
- What: `k_ent_resolve` is one thread per macroblock: DC predictors and P skipping are local
  or left-neighbor only; a B macroblock that may be skipped walks left to the nearest
  macroblock that sets each predictor. `k_ent_pack` walks left over the flags for the address
  increment, the quantizer to signal and the vector predictors (the nearest macroblock that is
  not skipped and uses the direction). `resolve_slice` is gone.
- First attempt treated a skipped B macroblock as setting PMV[1][s] too; a 1080i stream came
  out 70 bytes different (a field-predicted macroblock after a skipped one). Fixed: skipped
  macroblocks leave the predictors alone (7.6.6.4), so the walk passes over them.
- Check: streams byte-identical to the baseline binary on three clips; full gate 54/54, 0.00 dB.
- Measured (realistic; `p1-me-packed` → `p3-resolve`, which also contains experiment 2; 59–79 °C) [M]:

| Format | Mmb/s before | after (exp. 2 + 5) | of which exp. 5 alone (A/B/A, 69–73 °C) |
|---|---|---|---|
| 1080i × 16 | 10.24 | 10.66 (+4.1 %; first of the 3 runs disturbed by the gate's CPU load, the others 10.69, 10.84) | 10.52 → 10.91, +3.7 % |
| 720p × 16 | 10.58 | 11.50 (+8.7 %) | – |
| 480i × 96 | 10.27 | 10.69 (+4.1 %) | 10.66 → 10.73, +0.6 % (44-macroblock rows: the serial pass was short) |
| mixed × 8 | 9.17 (`p2-mixed`) | 10.35 (+12.8 %) | – |

Cumulative against the phase-A binary: 1080i +38 %, 720p +47 %, 480i +39 %, mixed +53 %.

## 6. Upload into the padded planes, batches phase by phase, one memset and one download — KEPT (05:05)

- Why: `k_pad` was 0.32 ms per launch group at 4 sessions (1.27 ms per tick at 16 × 1080i) and
  ran next to the search; the mixed tick had 10 synchronizations and 0.9 ms of idle GPU; every
  session cost a memset and a download call.
- What:
  1. The ring of source frames is one padded 4:2:0 frame per slot and session (Y, U, V
     contiguous, `Bufs::ring_stride`). `upload()` copies a frame straight into it: one
     `cudaMemcpyAsync` when the picture size is the coded size (720p, 480i), three when only
     the height differs (1080: 8 padding rows, filled by `k_pad_rows`), `cudaMemcpy2DAsync`
     plus `k_pad_cols` when the width differs. `k_pad` and the staging buffer are gone
     (3.1 MB of device memory per 1080 session less).
  2. `Batch::code()` is split into `begin` / `quantize` / `retry` / `finish_gpu` / `collect`;
     `Encoder::step()` runs each phase for all batches and synchronizes three times per tick
     in total (plus once per retry round), not three times per batch. With `--overlap` the
     upload calls are issued after the first phase is queued, so the GPU already works while
     the host issues them.
  3. The coded pictures of all sessions of a batch are packed one after the other in one
     buffer (`outpos`): one `cudaMemsetAsync` and one download per batch and tick.
- `--profile` now waits for the overlapped upload inside its own stage, so the other stage
  times are free of it (the profile run is therefore slower than a free run with `--overlap`).
- Check: 11 streams (3 formats in one process, staggered, overlapped, a 696×468 picture that
  needs both padding kernels) byte-identical to the phase-A binary; quick gate 15/15, 0.00 dB.
- Measured (realistic; `p3-resolve` → `p4-direct-upload`; 61–78 °C) [M]:

| Format | Mmb/s before | after | gain |
|---|---|---|---|
| 1080i × 16 | 10.66 | 12.20 | +14.5 % |
| 720p × 16 | 11.50 | 13.22 | +15.0 % |
| 480i × 96 | 10.69 | 12.48 | +16.7 % |
| mixed × 8 | 10.35 | 12.20 | +17.9 % |

- Timeline after (`results/nsys/p4-*-tick.txt`) [M]: 16 × 1080i: tick 10.37 ms, kernels busy
  95.0 %, idle 0.43 ms, 4 synchronizations, 22 launches, 59 copies. Mixed × 8: tick mean
  6.8 ms, kernels busy 90.3 %, idle 0.46 ms, 56 launches; **tick length 5.4 to 10.2 ms**
  (median 6.6): the load per tick depends on how many sessions code a B picture.
- Stage times, 16 × 1080i, ms: search 3.29, transform 2.98, quantize + entropy 2.51,
  reconstruction 0.80, scatter 0.16, download 0.15, host 0.13; sum 10.0 (free-run tick 10.3–10.5).
  Kernels in a tick (timeline): `k_xf` 2.92, `k_me` 2.15, `k_quant` 1.26, `k_ent_pack` 0.96,
  `k_recon` 0.55, `k_mbstat` 0.54, `k_mvsmooth` 0.48, `k_down2` 0.47 (4 launches),
  `k_ent_resolve` 0.18, `k_rowstat_part` 0.17.

## 7. Spin-wait synchronization (`--spin`, existing option) — KNOB, default off (05:06)

- Idle before the host's next call after each synchronization is about 0.1 ms (blocking wait).
- Measured A/B/A [M]: 1080i 12.42 / 12.21 (blocking, before and after) against 12.55 (spin);
  480i 12.64 / 12.54 against 12.83. About +2 % for one host core spinning at 100 %. Not made
  the default on a shared 8-vCPU host.

## Milestone 1: paced capacity after experiments 1–6 (05:07–05:23)

`results/bench/m1-realistic.json` (binary of commit 06d3af5; realistic workload; 25 s paced runs
on `crowd_run`; GPU warm, 79–91 °C, 66–68 W; no other GPU process; 16 minutes for five
formats) [M]:

| Format | cap99 phase A | cap99 now | cap999 now | Mmb/s at cap99 | mean / p99 tick, ms | tested N: on time |
|---|---|---|---|---|---|---|
| 1080i | 29 | **47** | 47 | 11.49 | 31.7 / 32.6 | 45: 100 %, 47: 100 %, 48: 94.3 %, 49: 90.7 % |
| 1080p | 31 | **52** | 50 | 12.72 | 32.5 / 33.1 | 50: 100 %, 51: 99.7 %, 52: 99.6 %, 53: 98.5 % |
| 720p | 34 | **59** | 58 | 12.73 | 16.0 / 16.4 | 57: 99.4 %, 58: 99.9 %, 59: 99.7 %, 60: 96.8 % |
| 480i | 176 | **292** | 288 | 11.55 | 31.4 / 32.3 | 288: 100 %, 292: 99.6 %, 296: 98.5 % |
| mixed (units of 1 × 1080i + 1 × 720p + 4 × 480i) | about 8 (9 + 7 + 32 held 99.65 %, 10 + 8 + 36 did not) | **18** (18 + 18 + 72) | none (15: 99.7 %, 16: 99.8 %) | 11.13 | 15.1 / 15.8 | 17: 99.4 %, 18: 99.4 %, 19: 75 % |

The mixed line-up now carries about as many macroblocks per second as a single format, but no
tested count reached 99.9 %: its ticks are uneven (experiment 6's timeline).

## 8. Packed `k_mbstat` and `k_down2` — KEPT (05:24)

- Why: `k_mbstat` 0.54 ms per tick at 16 × 1080i (78 cycles per instruction, byte loads down
  the columns), `k_down2` 0.47 ms in four launches (one thread per output byte).
- What: `k_mbstat` reads 16 rows of four words; sums with `__vsadu4`, sums of squares with
  `__dp4a` (dot product of four bytes), line differences with `__vsadu4` of two rows.
  `k_down2`: one thread per four output pixels, the 2×2 averages in 16-bit lanes of a word.
- Check: 14 streams byte-identical to the phase-A binary.
- Measured A/B/A, 2 runs each, 75–81 °C [M]: 1080i 12.23 / 12.05 → 12.82 (+5.6 %);
  720p 13.03 / 12.97 → 13.98 (+7.5 %); 480i 12.46 / 12.32 → 13.04 (+5.2 %).

## 9. Packed prediction in `k_xf` — KEPT (05:25)

- Why: `k_xf` was the largest kernel (2.9 ms per tick at 16 × 1080i); its first pass formed
  the prediction with 32 byte loads and 16 clamps per row and direction.
- What: `mc_row8`: three aligned words per source row, funnel shifts, `__vavgu4` for the
  horizontal or vertical half-sample case and the exact packed four-sample average for the
  diagonal case; the bidirectional average is `__vavgu4`; the current row is two words; the
  prediction is stored as the two words. No clamp on x: vectors of the search stay inside
  the picture (the old clamps never acted).
- Check: 14 streams byte-identical to the phase-A binary.
- Measured A/B/A, 78–80 °C [M]: 1080i 12.91 / 12.74 → 13.89 (+8.3 %); 720p 14.07 / 13.89 →
  15.35 (+9.8 %); 480i 13.13 / 12.99 → 13.64 (+4.5 %).

## 10. Sparse levels: skip zero blocks, store and read only non-zero levels — KEPT (05:27)

- Why: `k_quant` (1.26 ms per tick) was bound by DRAM traffic: it read 64 coefficients and
  wrote 64 levels per block through a 64-entry register array (96 registers, 28 % occupancy);
  `k_ent_pack` (0.96 ms) read all 64 levels of every coded block from 64 different planes
  (87 % of its warp samples waiting for loads); `k_recon` likewise.
- What:
  1. `k_xf` records the largest coefficient magnitude of each block (`bmax`, 2 bytes). A
     non-intra block whose largest coefficient quantizes to zero is not read by `k_quant`.
  2. `k_quant` writes only the non-zero levels and a 64-bit mask of their positions
     (`nzmask`); no local array (40 registers instead of 96).
  3. `put_block_coefs` iterates over the set bits of the mask (count trailing zeros), so
     `k_ent_pack` reads only non-zero levels; `k_recon` reads a level only where the mask
     has a bit.
- Check: 14 streams byte-identical to the phase-A binary; full gate 54/54, 0.00 dB.
- Measured A/B/A, 79–81 °C [M]: 1080i 14.00 / 13.83 → 15.91 (+14.4 %); 720p 15.43 / 15.34 →
  18.14 (+17.9 %); 480i 13.65 / 13.61 → 15.24 (+11.8 %).

## 11. Motion search split into four kernels for occupancy — REJECTED (05:30)

- Why: `k_me` needs 168 registers per thread (12 warps per SM in 64-thread blocks) and waits
  for loads 47 % of the time; four kernels (quarter/half resolution; full-pel + half-pel;
  field search per field; mode decision) need 48 / 128 / 80 / 93 registers.
- Check: 14 streams byte-identical (same decisions).
- Measured A/B/A, 70–74 °C [M]: 1080i 16.18 / 15.93 → 14.85 (−7.5 %); 720p 18.39 / 18.25 →
  17.56 (−4.2 %); 480i 15.37 / 15.23 → 14.11 (−7.8 %). Slower: each kernel fetches the
  current rows and the reference window again, which the single kernel has in the caches from
  its previous stage. The search is limited by cache misses, not by the number of warps.
  Reverted (the patch is not in the tree).

Re-profile after experiment 10 (`results/ncu/p5-*`, 4 × 1080i, per launch) [M]: `k_me` 0.60 ms
(31 % of kernel time; 168 registers, 23 % occupancy, 44 % of warp samples waiting for loads),
`k_xf` 0.50 (25 %; 34 MB read and 24 MB written per launch, 116 GB/s), `k_quant` 0.18,
`k_recon` 0.14, `k_mvsmooth` 0.12, `k_down2` 0.08 (4 launches), `k_ent_pack` 0.08,
`k_ent_resolve` 0.05, `k_mbstat` 0.04. Stage times at 16 × 1080i: search 2.87 ms, transform
2.34, quantize + entropy 1.30, reconstruction 0.65, rest 0.4; sum 7.6 ms.

## 12. 32-bit index arithmetic in `k_xf`, `k_recon`, `k_quant`, `k_down2` — KEPT (05:33)

(Correction at 06:35: this change was measured as written here but never reached the
repository. The commit that names it holds only result files, because its `git add` failed on
a path that did not exist yet, and a later `git checkout` of `kernels.h`, meant to drop the
measurement switches, removed the edit. Experiments 13 to 26 ran without it. Re-applied and
re-measured in experiment 27.)

- Why: thread-to-block mapping used 64-bit division and modulo, which the GPU emulates.
- Check: 14 streams byte-identical.
- Measured A/B/A, 72–74 °C [M]: 1080i 16.17 / 16.01 → 16.21 (+0.8 %); 720p 18.38 / 18.24 →
  18.56 (+1.3 %); 480i 15.38 / 15.20 → 15.43 (+0.9 %). Inside the run-to-run noise but
  positive in all three; kept because it is free.

Where the search and the transform spend their time (16 × 1080i, stage times with one part
left out; measurement-only builds, not kept) [M]:

- Search stage 2.87 ms: quarter-resolution full search 0.48 (`--range 2`: 0.17), field search
  0.80, vector smoothing 0.43, half-sample ring 0.25, the rest (half- and full-resolution
  levels, zero-vector test, bidirectional SADs, activity, pyramids, `k_mbstat`) about 0.9.
- Transform stage 2.32 ms: storing the coefficients 0.94, forming the prediction 0.74,
  histogram 0.15, the rest (current rows, DCT, shared memory, row statistics) 0.5.

## 13. Sparse coefficients behind a quantizer floor — KEPT (05:46)

- Why: the coefficient stores were 40 % of `k_xf` and `k_quant` read every coefficient of
  every block that was not all zero.
- What: each picture gets a floor for its quantizer codes: three codes below the lowest row
  code of the last picture of its type (`SessPar::qfloor`; a fixed quantizer is its own
  floor; 1 without history). `k_xf` stores only coefficients that are not zero at the floor
  quantizer of their macroblock (the same float expressions as `k_quant`, so "zero at the
  floor" implies "zero at any coarser quantizer" exactly) and a 64-bit mask per block
  (`cmask`, combined over the block's eight threads with warp shuffles, which also replaced
  the shared-memory reduction of the histogram and one barrier). `k_quant` reads only the
  masked positions. If the rate control then chooses a row code below the floor,
  `launch_qent()` repeats the transform for that session with floor 1 (counted as "repeated
  transforms" in the report). `bmax` of experiment 10 is gone.
- Check: 16 streams byte-identical to the phase-A binary, among them a rate walk (11 % of
  its pictures repeated the transform) and a still (7 %); full gate 54/54, 0.00 dB.
- Measured A/B/A, 70–76 °C [M]: 1080i 16.17 / 16.13 → 17.05 (+5.6 %); 720p 18.50 / 18.49 →
  18.75 (+1.4 %; an earlier pair of runs showed +5 %); 480i 15.35 / 15.28 → 15.60 (+1.8 %).
  Repeated transforms on the benchmark clip: 0.7 to 1.8 % of pictures at a margin of 3 codes
  (4.2 % at 2, 0.6 % at 5; throughput the same within 0.5 % for 2, 3; 5 was 0.5–2 % slower).
  Transform stage 2.32 → 2.14 ms, quantize + entropy 1.30 → 1.02 ms.
- Smaller than the 0.94 ms the stores cost: the mask logic costs about 0.4 ms and the
  remaining stores about 0.4 ms. A first version (64-bit mask shifts, float test per
  coefficient) gained 1 % less.

## 14. Scan and matrix tables as packed 64-bit constants in `k_xf` and `k_recon` — REJECTED (05:42)

- Why: eight byte loads of the scan table per thread.
- Measured against the build without it [M]: 1080i 17.02 → 16.81 (−1.2 %), 720p 19.30 → 18.38
  (−4.8 %), 480i 15.39 → 15.48 (+0.6 %). The 64-bit shifts cost more than the cached table
  loads. (A local `const` array in a device function is built on the stack at every call;
  it has to be `static const`.) Reverted.

Experiment 13 on easier content, 16 × 1080i [M]: `pedestrian_area` (quantizer about 17 to 26)
16.05 → 17.38 Mmb/s (+8.3 %); `fourpeople` (7 to 9) 16.73 → 17.43 (+4.2 %). Throughput
depends little on the content: 16.4 to 17.4 Mmb/s on all three clips.

## 15. Skip the field search where it cannot win — REJECTED (05:47)

- Why: the field search is 0.80 ms of the 2.87 ms search stage. Every field-based candidate
  carries at least the field penalty (12 λ), so when the frame SAD of a reference is at most
  that penalty the field search for it cannot change the decision: an exact skip.
- Check: 16 streams byte-identical.
- Measured A/B/A [M]: 1080i `crowd_run` 17.12 / 17.08 → 16.45 (−3.8 %); `pedestrian_area`
  17.42 / 17.41 → 16.69 (−4.2 %); `fourpeople` 17.43 / 17.43 → 15.41 (−11.6 %); 480i
  `crowd_run` 15.61 / 15.33 → 15.12; `football` 15.57 / 15.58 → 15.00 (−3.7 %).
  Slower everywhere, most where most macroblocks skip: threads of one warp that skip wait for
  those that do not, and the search had to move behind the half-sample step, away from the
  rows it shares with the full-pel step. Per-thread early exits do not pay in this kernel.
  Reverted.

## 16. Even GOP phases for the mixed line-up (`--balance`) — REJECTED, and a benchmark correction (05:52)

- Why: no tested count of the mixed line-up reached 99.9 % on time in milestone 1; the
  timeline showed ticks of 5.4 to 10.2 ms. Hypothesis: uneven share of B pictures per tick.
- Measured with a per-tick log (`--ticklog`, new) [M]: tick time fits 0.29 ms + 0.052 ms per
  1000 macroblocks coded in the tick, with nearly the same slope for I, P and B pictures
  (0.047 / 0.052 / 0.053); with `--stagger` the number of macroblocks per tick is constant
  at 20 units (206 k every tick). A phase assignment that balances the residues modulo 3
  changed nothing (p99/mean 1.42 → 1.40). Removed again.
- The long ticks were the **last two ticks of a run**: when the input ends, every session
  flushes its waiting pictures, and the 29.97 Hz sessions then code in every 59.94 Hz tick.
  Two late ticks in 1,500 are 0.13 %, which alone denies 99.9 %. `m2venc` now leaves the
  ticks after the end of the input out of the timing statistics (they are still coded).
  Paced, 20 units (20 + 20 + 80 sessions, 12.3 M macroblocks/s), 20 s, with the binary of
  experiment 13 [M]: mean tick 12.4 ms, p99 13.5, p99.9 15.0 of 16.7 ms; the only two ticks
  over 14 ms were the flush ticks.

## 17. Temporal candidate in the motion search (`--tpred`, default on) — KEPT as a quality gain with a small speed cost (05:55–06:08)

- Why: the hierarchical search starts every macroblock from scratch at quarter resolution
  and can lock on the wrong match in fine repetitive detail (`mobcal` was 1.3 dB below
  ffmpeg; the smoothing pass of phase A had already shown +0.5 dB there).
- What: the final decisions of a session's previous coded picture are still in the `MbInfo`
  buffer when `k_me` runs. The co-located macroblock's vector, scaled by the ratio of the
  temporal distances (`SessPar::pdist_f/b`), gives one more full-pel candidate per reference
  (16 rows, scored on even and odd lines like the other nine); the half-sample step starts
  from the better one.
- Quality against the gate baseline, 54 cases (`scripts/qdiff.py`, new) [M]:
  - 22 standard cases (nominal rate): luma PSNR **+0.067 dB on average, every case better**
    (+0.010 `fourpeople` 1080i … +0.453 `mobcal`), SSIM +0.0014 on average, same rate.
  - 19 buffer torture cases: +0.018 dB on average, between −0.50 (`cuts` 480i at a quarter
    of the rate) and +0.59 (`cuts` 720p at a quarter rate; its chroma −0.19); `still` 1080i
    −0.10, `still` 480i +0.21. 12 feature cases: +0.019 on average, `cuts` 1080i with rate
    steps −0.14.
  - These quarter-rate cases are coded on the buffer ladder (pictures without coefficients);
    their PSNR moves by ±0.5 dB with any change of the vectors. **This breaks the 0.2 dB
    per-clip rule on one torture clip** and is kept on the strength of the nominal-rate
    results; `--tpred 0` restores the old search exactly (16 streams byte-identical to the
    phase-A binary with it).
  - A bit-cost penalty on the candidate (2, 6, 12 λ) did not help: standard cases +0.064 /
    +0.056 / +0.063 dB, torture cases still scattered. Not kept.
- Speed, A/B/A, 62–78 °C [M]: 1080i 17.06 / 16.96 → 16.79 (−1.3 %); 720p 19.55 / 19.46 →
  19.18 (−1.7 %); 480i 15.62 / 15.55 → 15.38 (−1.3 %).

## 18. Seeded quarter-resolution search for B pictures — REJECTED (06:00)

- Why: the quarter-resolution full search (289 candidates) is 0.48 ms of the search stage and
  B pictures do it twice; with the temporal candidate at hand a B picture could search 3×3
  around zero and 5×5 around the candidate.
- Measured [M]: standard cases +0.040 dB on average (against +0.067 for the candidate
  alone) but `pedestrian_area` 1080i −0.20 dB; no speed gain outside the noise (1080i 16.2
  against 16.3–16.8; 720p 19.70 against 18.9–19.6): the extra code raised `k_me` from 168 to
  183 registers (10 instead of 12 warps per SM). Removed.

## 19. Hybrid wait for the GPU — KEPT, default on (06:08)

- Why: a blocking `cudaStreamSynchronize` wakes the host about 0.1 ms after the GPU has
  finished, four times per tick (0.43 ms idle of a 10.4 ms tick in the timeline of experiment
  6); spinning (experiment 7) costs a whole core.
- What: `Encoder::wait()` sleeps for 85 % of the time the same wait took in the last ticks
  (less 0.1 ms) and polls `cudaStreamQuery` for the rest; falls back to the blocking call if
  the wait takes more than twice as long as expected. `--blocking-wait` restores the old way.
- Measured, blocking / hybrid / spin / blocking / hybrid, one run each, 16 sessions (96 for
  480i) [M]: 1080i 17.05 / 17.30 / 17.42 / 16.99 / 17.27 Mmb/s (+1.6 %; spin +2.3 %); 720p
  19.44 / 20.32 / 20.44 / 19.36 / 20.09 (+4.2 %; spin +5.4 %); 480i 15.56 / 15.87 / 15.92 /
  15.49 / 15.71 (+1.7 %; spin +2.5 %). Host CPU free-running: 0.06–0.10 core blocking,
  0.22–0.26 hybrid, 0.99 spin. Output unchanged.

## 20. Field search helpers without variable word indices — KEPT (06:13)

- Why: `field_seed`, `field_refine` and `hp16x` shifted words out of a small array with an
  index that depends on the pixel alignment (`W[i + (o >> 5)]`); the compiler keeps such an
  array in local memory (1.7 M local-memory instructions per `k_me` launch in the profile,
  320 bytes of stack). The field search is 0.80 ms of the 2.87 ms search stage.
- What: normalize each row once with a variable funnel shift at fixed indices, then shift by
  constants (as the frame search of experiment 1 does); `field_seed` slides three current
  lines instead of holding eight. Stack 320 → 176 bytes.
- Check: 16 streams byte-identical to the phase-A binary with `--tpred 0`, and three streams
  identical to the previous build with the temporal candidate on.
- Measured A/B/A, 70–72 °C [M]: 1080i 17.37 / 17.18 → 18.17 (+5.1 %); 480i 15.95 / 15.85 →
  16.57 (+4.2 %). 720p has no field search.

## 21. Registers of `k_me` — two REJECTED attempts and what they show (06:14–06:16)

- Writing the two reference searches out instead of looping over them (results in registers,
  stack 176 → 112 bytes): `k_me` went from 168 to 177 registers, i.e. from 6 to 5 blocks of
  64 threads per SM. A/B/A [M]: 1080i 18.24 / 17.97 → 17.30 (−4.5 %); 720p 20.65 / 20.53 →
  19.66 (−4.5 %); 480i 16.55 / 16.46 → 15.78 (−4.4 %). Reverted. One block of two warps per
  SM is worth 4.5 %, so the kernel is sensitive to occupancy after all, as long as the stages
  stay in one kernel (experiment 11).
- Looking for the opposite step (compile only, register count from `ptxas -v`): the count
  moves in plateaus: 128 when any one of the half-sample ring, the full-pel loop or the field
  and bidirectional part is left out, 142 without the half-resolution level, 168 for the
  whole kernel whatever is unrolled (four variants) and with a leaner half-resolution level.
  At 64 threads per block 146 registers would give 7 blocks and 128 would give 8 (+33 %
  warps). Not reached in the time; it is the first thing to try next (est. +5 to +10 % [E]).

## 22. One exact half-sample fetch for all prediction SADs — KEPT (06:16)

- What: `hp16x` (exact averages, five words per source row) replaces `hp16` / `row16` /
  `wsh` in the smoothing pass and the bidirectional field SAD (5 to 10 loads per row
  instead of 5 to 20; the diagonal case was a nested average there).
- Measured A/B/A/B [M]: 1080i 18.38 / 18.38 → 18.54 / 18.47 (+0.7 %); 720p 20.57 / 20.80 →
  20.98 / 20.88 (+1.2 %); 480i 16.63 / 16.64 → 16.75 / 16.74 (+0.7 %).
- Quality, 22 standard cases against the phase-A baseline [M]: +0.061 dB on average (it was
  +0.067 before this change), every case still above the baseline (worst +0.007). The
  decisions of the smoothing pass change slightly; −0.006 dB on average for +1 % speed and
  three helper functions less.

## 23. Load on the card at a fixed line-up, before and after (06:17–06:22)

`results/bench/load-*.json`: paced at real time for 60 s, realistic workload, phase-A binary
against the binary of experiment 22; `nvidia-smi dmon` once per second; no other GPU
process [M]:

| Line-up | Binary | Mean tick, ms (period) | GPU utilization | Power, W | Temperature start → max, °C | Clock, MHz | On time |
|---|---|---|---|---|---|---|---|
| 16 × 1080i | phase A | 16.30 (33.4) | 46.8 % | 48.1 | 62 → 76 | 2130 | 100 % |
| 16 × 1080i | now | 6.97 | 20.0 % | 27.3 | 53 → 62 | 2130 | 100 % |
| 16 × 1080i | now, run again right after the phase-A run | 6.99 | 20.4 % | 29.4 | 76 → 76 (cooling) | 2130 | 100 % |
| mixed × 8 (8 × 1080i + 8 × 720p + 32 × 480i) | phase A | 11.98 (16.7) | 67.2 % | 56.4 | 68 → 83 | 2430 | 100 % |
| mixed × 8 | now | 4.47 | 26.1 % | 29.9 | 62 → 68 | 2130 | 100 % |

The same line-ups now load the card 2.3 to 2.6 times less: 19 to 26 W less, and the card
stays 14 to 15 °C cooler.

## 24. Knob table, and a vector-range bug found by it — FIXED (06:22–06:30)

- `scripts/check.py` takes extra encoder options from `M2V_EXTRA`; with it the 22 standard
  cases were run for each knob (quality against the phase-A baseline; the default is +0.061 dB)
  and paired free runs at 16 × 1080i for the speed [M]:

| Setting | Luma PSNR, mean of 22 cases against the default | Worst case against the phase-A baseline | Throughput vs default |
|---|---|---|---|
| `--tpred 0` | −0.067 dB (`mobcal` −0.45) | −0.13 dB (`still` 720p) | +1.3 % |
| `--smooth 0` | −0.025 dB | −0.07 dB (`testsrc2` 480i) | +6.1 % |
| `--field-pred 0` | −0.084 dB | −0.28 dB (`testsrc2` 480i) | +10 % |
| `--range 4` | −0.052 dB | −0.97 dB (`testsrc2` 480i) | +1 % (two pairs: −5 % and +3 %; noise) |
| `--range 2` | −0.53 dB | −3.5 dB (`pedestrian_area` 1080i) | about +4 % (stage time 0.31 ms of 7.6) |
| `--range 12` (after the fix below) | −0.002 dB | −0.04 dB (`crowd_run` 1080i) | −5.5 % |
| `--aq 0` | +0.48 dB (SSIM +0.0000) | +0.22 dB | 0 |

- So the search range is no cheap knob: ±11 pixels cannot follow P pictures three frames
  apart (phase A's "no PSNR loss" came from the first spike's two clips). The default ±35
  stays.
- The first run of `--range 12` lost 2.4 dB on `pedestrian_area` 1080i. Cause: with field
  prediction the second field seed scales the frame vector by up to 3/2, and `f_code` was
  sized for the frame range only; at ranges above 9 a field vector could leave the coded
  range and wrap. (Phase-A code; the default range 8 is inside the range, so no stream so far
  was affected.) `Batch::init` now sizes `f_code` for the scaled seed. Default streams
  byte-identical before and after; `--range 12` then passes the gate (22/22).

## 25. Power cap, first series (06:30; repeated in experiment 35)

- Default limit 70 W (range 30–70 W, read before the test). `sudo nvidia-smi -pl` 70 / 60 /
  50 / 40, each with three free runs of 16 × 1080i; **limit restored to 70 W and read back**.
- During all four a Plex transcode used the card's decoder and encoder (12–40 % decoder, 3–10 %
  encoder, 0–1 % of the SMs, 165 MB), so the numbers are not trusted [M, contaminated]:
  70 W: 18.05 Mmb/s at 51–54 W, clock 2070 MHz; 60 W: 16.91 (−6 %), 1845 MHz; 50 W: 15.21
  (−16 %), 1560 MHz; 40 W: 10.05 (−44 %), 870 MHz.

## 26. `k_me`: half-sample steps word by word, and register caps again — REJECTED (06:33)

- Why: after experiment 21, fewer registers should mean more warps. The half-sample ring and
  `field_refine` were rewritten to handle a row word by word and to keep the previous row as
  five words instead of twelve.
- Compile only: still 168 registers (the count does not follow the number of values alive in
  the source). Streams identical. A/B, two runs each, 1080i / 720p [M]: 18.29, 18.20 / 20.66,
  20.63 before; 18.26, 18.12 / 20.62, 20.61 word by word (no change);
  `-maxrregcount=144`: 17.95, 17.19 / 20.35, 20.30 (−3 %); `-maxrregcount=128`: 18.23, 18.12 /
  20.50, 20.43 (−0.5 %): 16 warps instead of 12 buy back what the spills cost, no more.
  Reverted. (A Plex process was present on the card during these runs, idle most of the time.)

## 27. 32-bit index arithmetic, again — KEPT (06:36)

- The change of experiment 12, found missing from the tree (see the correction there) while
  reading `k_xf`, applied again: `blk_geo`, `k_quant`, `k_down2`.
- Check: three streams identical to the build before it.
- Measured A/B/A/B [M]: 1080i 18.30 / 18.24 → 18.35 / 18.33 (+0.4 %); 720p 20.70 / 20.62 →
  20.91 / 20.89 (+1.2 %); 480i 16.63 / 15.79 → 16.62 / 16.56 (no change; one reference run
  disturbed).

## 28. `k_xf` with one thread per block — REJECTED (06:38)

- Why: `k_xf` is the largest kernel again (2.1 to 2.3 ms of a 7.0 ms tick at 16 × 1080i). With
  eight threads per block it pays two barriers, a shared-memory round trip, 18 warp shuffles
  per thread, eight copies of the per-block set-up, and its coefficient stores touch eight
  cache lines per instruction. One thread per block (rows into a per-thread array, then
  columns) has none of that, and a warp works on 32 blocks at once.
- Check: three streams identical.
- Measured, transform stage of the profile, two runs each [M]: 2.26, 2.14 ms now; 2.28, 2.27
  with one thread per block in 256-thread blocks; 2.32 in 64- and 128-thread blocks. Free run
  1080i 18.18 / 18.23 → 18.19 / 17.40; 720p 20.53 / 20.70 → 20.46 / 20.58. No gain.
- Reading: the kernel's time does not depend on how its threads are arranged; it is bound by
  the memory it has to move (current and reference rows in, prediction, coefficients, mask
  and histogram out: about 1.3 kB per macroblock). Removed.

## 29. `k_recon` with one thread per block — KEPT (06:42)

- Why: the reconstruction (0.6 ms of the tick, anchors only) read the prediction and wrote
  the result as bytes down the columns, through shared memory and two barriers.
- What: one thread per block in 64-thread blocks: visits only the non-zero levels (mask),
  transforms rows then columns in registers (118 per thread), reads the prediction and writes
  the result as two words per row. Same arithmetic in the same order.
- Check: 16 streams identical to the build before it (reconstruction and streams).
- Measured: reconstruction stage 0.595 → 0.52 ms; free run A/B/A/B [M]: 1080i 18.41 / 18.40 →
  18.76 / 18.67 (+1.7 %); 720p 20.96 / 20.93 → 21.21 / 21.20 (+1.2 %); 480i 16.73 / 16.71 →
  16.83 / 16.86 (+0.8 %). (Unlike `k_xf`, experiment 28, whose byte traffic was already packed.)

## 30. Row statistics and the slice prefix sum with one block per row — KEPT (06:45)

- Why: `k_rowstat_part`, `k_rowstat` and `k_ent_rowsum` walked a macroblock row in one thread
  (0.045 + 0.034 + 0.040 ms per launch whatever the session count, 20 to 96 cycles per
  instruction). At 16 × 1080i that is 2 % of the tick, but every batch pays it: about 0.3 ms of
  a 4.3 ms mixed tick.
- What: one CUDA block per (session, row), one thread per macroblock. `k_rowstat` adds the
  histograms of the thread's six blocks, reduces 19 sums inside each warp with shuffles and
  across warps through shared memory (`k_rowstat_part` and its buffer are gone).
  `k_ent_rowsum` scans the bit counts with shuffles inside a warp and adds the warp totals.
  Rows longer than 256 macroblocks (4096 samples) are refused at `open`.
- Check: 16 streams identical to the build before it.
- Measured against the build of experiment 27, which lacks experiment 29 too, A/B/A/B [M]:
  1080i 18.42 / 18.39 → 19.05 / 19.01 (+3.4 %; +1.6 % of it from this change); 720p 20.89 /
  20.88 → 21.68 / 21.61 (+3.6 %; +2.3 %); 480i 16.69 / 16.70 → 17.09 / 16.98 (+2.0 %; +1.2 %);
  mixed × 8 19.02 / 18.98 → 19.83 / 19.79 (+4.3 %).

Timeline after experiment 30 (`results/nsys/p9-*-tick.txt`) [M]: 16 × 1080i: tick 6.78 ms,
kernels busy 94.2 %, idle 0.30 ms; in a median tick `k_xf` 2.02 ms (plus 0.15 for one
repeated transform), `k_me` 1.83, `k_mvsmooth` 0.44, `k_recon` 0.43, `k_quant` 0.42,
`k_down2` 0.33 (four launches), `k_ent_pack` 0.32, `k_mbstat` 0.19, `k_ent_resolve` 0.18,
`k_rowstat` 0.08, `k_ent_scatter` 0.08, `k_ent_rowsum` 0.007. Mixed × 8: tick 4.33 ms (median
4.18), kernels busy 88 %, idle 0.23 ms; `k_xf` and `k_me` 29.5 % of kernel time each.

## 31. Row-subsampled SAD in the smoothing pass — REJECTED (06:47)

- Why: `k_mvsmooth` is 0.44 ms of the tick and waits for loads; scoring the left neighbor's
  vectors on every second row halves its loads.
- Measured A/B/A/B [M]: 1080i 19.00 / 18.97 → 19.26 / 19.29 (+1.5 %); 720p 21.68 / 21.64 →
  22.02 / 21.98 (+1.6 %). Quality, 22 standard cases: +0.018 dB against the phase-A baseline
  where the full SAD has +0.061 (−0.043 dB), `still` 720p −0.30 dB. Not worth it; reverted.

## Milestone 2: paced capacity of the final binary — measured next to a Plex transcode (06:48–07:03)

`results/bench/final-realistic.json` (binary after experiment 30; realistic workload; 25 s
paced runs; GPU warm, 81–91 °C). **Every run of it is marked contaminated**: a Plex transcode
session was open on the card the whole time (process present with 165 MB; its decoder between
0 and 80 %, its encoder between 0 and 22 % depending on the run). The numbers are therefore
lower bounds [M, contaminated]:

| Format | cap99 phase A | milestone 1 | now, next to Plex | cap999 | Mmb/s at cap99 | mean / p99 tick, ms | Plex decoder during the run at cap99 | tested N: on time |
|---|---|---|---|---|---|---|---|---|
| 1080i | 29 | 47 | **67** | 67 | 16.39 | 29.3 / 30.2 | 2 % | 65: 99.86 %, 66: 100 %, 67: 100 %, 68: 93.5 % (decoder 42 %), 74: 75.7 % |
| 1080p | 31 | 52 | **72** | 71 | 17.61 | 28.9 / 33.2 | 60 % | 71: 100 %, 72: 99.2 %, 73: 96.7 %, 74: 93.8 %, 80: 1.4 % |
| 720p | 34 | 59 | **90** | 90 | 19.42 | 15.8 / 16.3 | 2 % | 88: 100 %, 90: 100 %, 91: 98.2 %, 92: 84.4 % |
| 480i | 176 | 292 | **264** | 260 | 10.44 | 21.1 / 33.0 | 77 % | 260: 100 % (decoder 0 %), 264: 99.7 %, 268: 98.9 %, 272: 97.5 %, 340: 95.8 % |
| mixed, units | about 8 | 18 | **24** | 23 | 14.85 | 14.4 / 16.2 | 73 % | 22: 100 %, 23: 99.93 %, 24: 99.79 %, 25: 89.9 % |

1080i and 720p ran while the transcode was nearly idle and are limited by the mean tick, as
before. 480i is not: its mean tick at 264 sessions is 21 ms of 33 ms and the on-time
fraction hardly depends on the session count (95.8 % at 340). See the next entry.

## 32. What a Plex transcode on the same card does to the ticks (07:03–07:10)

Paced 480i with the per-tick log (now with the time of each phase, the upload wait and the
retry counts) and `nvidia-smi pmon` once per second [M]:

- 300 sessions, tick normally 22–24 ms of 33.4 ms. While the transcoder's decoder shows 15 to
  26 % (bursts of two to three seconds), **every tick takes 34 to 38 ms, about 60 % longer**,
  and is late. Two contexts on one GPU are time-sliced; the transcode's share is small but
  our kernels wait for its slices.
- Between the visible bursts there are groups of single slow ticks (every third tick, three
  to five in a row, every four to six seconds; some 20-second runs have none): in them the
  first phase takes exactly the upload time longer (300 sessions: 14.4 → 25.9 ms; 260
  sessions: 12.5 → 22.4 ms), i.e. the overlapped upload and the kernels ran one after the
  other. No retry round, no repeated transform, no host stall in those ticks (upload calls
  0.6 ms, launch calls 0.1 ms). 200 and 100 sessions of 480i and 48 × 1080i showed none in
  20 s each. Milestone 1 ran 288 to 292 sessions of 480i at a mean tick of 31.4 ms with 99.6
  to 100 % on time when no transcode was open, so this did not exist then; it is attributed to
  the open transcode session [E], not proven.
- Consequence: next to an active Plex transcode the encoder loses about 40 % of its capacity
  while the transcoder decodes, and real-time operation near capacity is not safe on a shared
  card. This was the open question of §12 ("the encoder next to Plex or Jellyfin").

## 33. Coarse search levels in a kernel of their own — REJECTED (07:43)

- Why: the quarter- and half-resolution levels read planes that the rest of `k_me` never
  touches, so splitting them off repeats no fetch (unlike experiment 11), and `k_me` then
  compiles to 137 registers (7 blocks of 64 threads per SM instead of 6); the coarse kernel
  needs 53.
- Check: 16 streams identical.
- Measured, two rounds of old / split / split with `-maxrregcount=128` / split with 64-thread
  coarse blocks [M]: 1080i 19.07, 18.91 / 18.56, 18.44 / 18.59, 18.37 / 18.51, 18.40 (−2.6 %);
  720p 21.57, 21.62 / 20.88, 20.83 / 20.88, 20.87 / 20.84, 20.79 (−3.4 %); 480i 17.04, 16.29 /
  16.58, 16.52 / 16.71, 16.64 / 16.61, 16.57 (inside the noise; a Plex process became active
  during the 480i runs).
- Reading: more warps do not make this search faster, with or without repeated fetches. With
  experiments 3, 11, 21 and 26 that closes the occupancy line: the loss of experiment 21 (177
  registers) cannot have been only its lost block, and the estimate in that entry ("128
  registers would give +5 to +10 %") is withdrawn. Reverted.

## 34. Sustained runs at the capacity points (07:11–07:22)

Three minutes paced each, final binary, at the counts of milestone 2; no other process on the
card during any of the four (`results/bench/final-sustain-*.json`) [M]:

| Line-up | On time | Late ticks | Mean / p99 / p99.9 tick, ms | W | °C start → max | Clock min, MHz | Host CPU, cores |
|---|---|---|---|---|---|---|---|
| 67 × 1080i | 100 % | 0 of 5,365 | 29.1 / 30.3 / 30.7 (33.4) | 66 | 62 → 90 | 1890 | 0.16 |
| 90 × 720p | 98.30 % | 182 of 10,730 | 15.9 / 16.5 / 19.4 (16.7) | 68 | 83 → 91 | 1965 | 0.19 |
| 264 × 480i | 100 % | 0 of 5,365 | 20.0 / 21.0 / 21.9 (33.4) | 59 | 84 → 90 | 2070 | 0.14 |
| mixed × 24 (24 + 24 + 96) | 99.92 % | 9 of 10,730 | 14.3 / 15.1 / 15.4 (16.7) | 67 | 81 → 91 | 2040 | 0.18 |

- 720p at 90 is over the sustained limit of the hot card (it held 100 % in the 25 s run).
- 480i at 264 uses 60 % of the period: the bisection of milestone 2 stopped there because of
  the Plex bursts, not because of the encoder. By the mean tick its capacity is about 400
  sessions [E].

## 35. Load at the fixed line-ups with the final binary; power cap, second and third series (07:22–07:50)

- Fixed line-ups, 60 s paced, final binary [M]: 16 × 1080i: mean tick 6.72 ms, SMs 19.3 % busy,
  30.6 W on a card still at 84 °C from the sustained runs (experiment 23: 6.97 ms, 20.0 %,
  27.3 W from 53 °C; phase A 16.30 ms, 46.8 %, 48.1 W). Mixed × 8: 4.25 ms, 24.7 %, 32.2 W with
  a Plex transcode active (experiment 23: 4.47 ms, 26.1 %, 29.9 W; phase A 11.98 ms, 67.2 %,
  56.4 W).
- Power cap, free run of 16 × 1080i, second series (the Plex process present but idle in all
  runs but one) [M]: 70 W 18.70 Mmb/s at 2040–2055 MHz; 60 W 17.55 (−6.2 %) at 1830; 50 W
  15.68 (−16.1 %) at 1515–1530. With the first series (experiment 25: 18.05 / 16.91 / 15.21 /
  10.05 at 40 W; the transcoder was idle in those runs too, except in two of the 40 W runs):
  −6 % at 60 W, −16 % at 50 W, −44 % at 40 W, reproducibly.
- Power cap at the fixed line-up 16 × 1080i, 45 s paced (a Plex transcode active in all three)
  [M]: 70 W: tick 6.73 ms, 30.7 W; 50 W: 6.87 ms, 29.4 W; 40 W: 6.86 ms, 29.3 W; clock
  2130 MHz in all three. Below the cap nothing changes.
- The limit was restored to 70 W after each series and read back (default 70 W).
- Conclusion: a cap does not improve throughput per watt here; it only takes the peak away.
  Whether a cap just under the sustained draw keeps the card cooler at capacity over minutes
  was not tested (it needs runs at capacity without the transcode).

## 36. Capacity points without the transcode (07:43–08:10)

`scripts/capclean.py` walks session counts upward and repeats a run during which another
process used the decoder or encoder of the card. Three attempts between 07:43 and 07:53 met
transcoder activity in every run (with it, 300 × 480i is 89 to 97 % on time and 330 is 75 to
84 %). At 07:53 the transcoder process had gone, and a fourth walk got clean runs for two
formats before it came back (`results/bench/final-clean.json`, binary before experiment 37) [M]:

| Format | Clean runs: N: on time (mean / p99 tick, ms) | Runs next to the transcoder |
|---|---|---|
| 480i | 340: 100 % (26.1 / 27.1), 360: 100 % (28.0 / 28.7), 380: 100 % (29.8 / 30.5), **400: 100 % (31.5 / 32.2)**, 420: 0 % (33.6 / 34.4), 440: 0 % | – |
| 1080p | 72: 100 % (28.3 / 29.0), 73: 100 %, 74: 100 %, **75: 100 % (29.7 / 30.5)** | 76: 57.8 %, 89.2 %, 94.4 %; 77: 100 %, 100 %, 85.0 % |
| mixed | 24: 99.93 % (transcoder present, idle) | 24: 99.93 %, 99.86 %; 25: 99.37 %, 98.68 %, 99.72 %; 26: 99.37 %, 99.24 %, 94.51 %; 27: 11 to 25 % |
| 1080i | – | 67: 92.4 %, 93.6 %, 92.4 %; 68: 96.4 % |
| 720p | – | 89: 74.0 %, 85.5 %, 90.8 %; 90: 75.3 %, 88.7 %, 49.7 % |

- 480i holds 400 sessions (15.8 M macroblocks/s) at 99.9 %; 420 are over the period. The 264
  of milestone 2 was the transcode.
- 1080p holds at least 75 (clean) and held 77 twice while the transcoder was present but idle.
- 1080i and 720p could not be repeated cleanly; for them milestone 2 (67 and 90, measured
  with the transcoder at 2 %) and experiment 34 stand. The same counts are 74 to 94 % on time
  when the transcoder works.

## 37. Smaller per-macroblock record, and both pyramid levels in one kernel — KEPT (08:10)

- Why: `k_ent_resolve` stored a 40-byte `MbCode` per macroblock, 16 bytes of it predictors
  that `k_ent_pack` has computed itself since experiment 5 (71 % of the kernel's warp samples
  were throttled on the memory queue); `k_down2` ran twice per pyramid, the second launch
  reading back what the first had written (0.33 ms per tick in four launches).
- What: `k_ent_resolve` writes a 24-byte `MbRes`; `k_ent_pack` builds its `MbCode` from it.
  `k_pyr` makes the half- and the quarter-resolution plane in one pass (one thread per four
  quarter-resolution pixels: 16 words in, 5 out); `k_down2` is gone.
- Check: 16 streams identical to the build before; full gate 54/54, quality change 0.000 dB.
- Measured together, three pairs old / new [M]: 1080i 18.55, 18.53, 18.54 → 18.94, 18.84,
  18.83 (+1.8 %); 720p 21.12, 21.09, 21.08 → 21.54, 21.59, 21.47 (+2.1 %); 480i 16.72, 16.66,
  16.73 → 16.99, 16.97, 16.94 (+1.6 %). Not measured separately.
- The capacity points of milestone 2 and of experiments 34 and 36 were measured before this
  change.

## 38. Final state (08:13–08:20)

- Final binary (after experiment 37), `results/bench/final2-realistic.json`, free run, mean
  of three, a Plex process present (one mixed run disturbed, spread 8.9 %) [M]:

| | 1080i × 16 | 1080p × 16 | 720p × 16 | 480i × 96 | mixed × 8 |
|---|---|---|---|---|---|
| Phase-A binary, cold (`p0-realistic`, `p0-mixed`) | 7.75 | 8.08 | 7.80 | 7.71 | 6.77 |
| Final binary, Mmb/s | 19.00 | 21.14 | 21.95 | 17.28 | 19.31 |
| Ratio | 2.45 | 2.62 | 2.81 | 2.24 | 2.85 |

- Timeline (`results/nsys/final-1080i-tick.txt`, `final-mixed-tick.txt`) [M]: 16 × 1080i
  6.88 ms per tick (median 6.80), kernels busy 93.3 %, idle 0.37 ms; `k_xf` 2.03 (+0.14 for a
  repeated transform), `k_me` 1.92, `k_mvsmooth` 0.44, `k_quant` 0.41, `k_recon` 0.35, `k_pyr`
  0.28, `k_ent_pack` 0.26, `k_mbstat` 0.19, `k_ent_resolve` 0.08, `k_rowstat` 0.08. Mixed × 8:
  4.28 ms (median 4.13), kernels busy 88 %.
- Gate: 54 of 54 with the baseline of the final binary (`results/check/baseline.json`; the
  phase-A baseline is `baseline-phaseA.json`, the final run against it `final.json`).
- Capacity: milestone 2 and experiments 34 and 36, measured before experiment 37.
- Host: no failed unit, no OOM line in the kernel log since 12:30 UTC, Plex, Jellyfin and
  nzbget active, no encoder or benchmark process left, power limit 70 W (default 70 W).
  Disk use under `~/totalblonder-spike`: `build` 1.2 GB (profiler reports), `out` 0.4 GB.

## Summary of the kept changes

1 packed frame search; 2 small blocks for `k_me`; 5 slice state per macroblock; 6 direct
upload, phases across batches, one memset and download per batch; 8 packed `k_mbstat` and
`k_down2`; 9 packed prediction in `k_xf`; 10 sparse levels; 13 sparse coefficients behind a
quantizer floor; 17 temporal candidate (quality); 19 hybrid wait; 20 field helpers without
local arrays; 22 one exact half-sample fetch; 24 `f_code` fix; 27 32-bit index arithmetic;
29 `k_recon` per block; 30 row kernels per row block; 37 smaller macroblock record and one
pyramid kernel.

Rejected: 3 and 26 register caps; 7 spin wait (option); 11 four-kernel search; 14 packed
tables; 15 exact field-search skip; 16 balanced GOP phases; 18 seeded B search; 21 written-out
reference searches; 26 word-wise half-sample steps; 28 `k_xf` per block; 31 subsampled
smoothing SAD; 33 coarse levels in their own kernel; power cap (25, 35).

## 39. Last attempt at clean 1080i and 720p capacity points (08:18–08:24) — stopped

Final binary, `capclean.py` with four tries per count; the Plex transcoder was active in every
run, so the walk was stopped after the 1080i part [M, contaminated]: 67 × 1080i 100 %, 100 %,
100 %, 99.86 % on time (mean tick 27.6 to 29.5 ms, p99 31.4 to 33.0 ms); 68: 99.0 %, 97.4 %,
96.3 %, 95.7 %; 69: 91.8 to 96.1 %; 88 × 720p: 94.3 % (one run). So 67 × 1080i hold even next
to this transcode; one or two more do not. No result file was kept.

## 40. Why a transcode on the same card slows the ticks: synthetic competitors (08:29–09:00)

Question from experiment 32: a Plex hardware transcode made every tick of 300 × 480i about
60 % longer; is that time-slicing, real compute work of the transcoder, the power limit, memory
or PCIe, or something in our own pipeline? Method: one paced encoder run with the per-tick log
(`scripts/contend.sh`), and during it one competitor after the other on the same card, 8 s each
with 3 s of the encoder alone between them; each competitor is compared with the ticks right
before and after it. Competitors: ffmpeg 9.0.2 (NVDEC, NVENC, `scale_cuda`, `tonemap_opencl`)
on a 1080p H.264 file and a 2160p HEVC 10-bit PQ file, throttled to real time (`-re`, 24 fps)
and unthrottled; and `src/gpucomp.cu`, a second CUDA process with no video engine use.
`nvidia-smi dmon`/`pmon` at 1 s. Evaluation: `scripts/contend_report.py`; data in
`results/contend/*/` (`report.json` per run).

Contamination, stated first: **a real Plex transcode was open on the card during the whole
experiment** (`h264 nvdec → scale_cuda 720x404 → h264_nvenc`, bursts of 1 to 3 s every 15 s or
so, decoder 10 to 60 % in them). Seconds in which pmon shows activity of that process are
left out of both sides of every comparison; some cells lost most of their ticks that way (marked
"few"). There was therefore never a run with no second context at all.

Tick-time ratio, competitor running / encoder alone (median tick; alone: 150 × 480i 11.4 ms,
300 × 480i 22.4 ms, 32 × 1080i 12.9 ms, 60 × 1080i 24.8 ms; period 33.4 ms) [M]:

| Competitor | 150 × 480i | 300 × 480i | 32 × 1080i | 60 × 1080i | its own rate |
|---|---|---|---|---|---|
| NVDEC H.264 1080p, frames stay on device, real time | – (few) | 1.15 ¹ | 1.03 | 1.01 | 24 fps |
| the same, unthrottled | 1.10 | **1.64** | 1.12 | 1.17 | 660–810 fps |
| NVDEC HEVC 2160p 10-bit, real time | 1.02 | 1.07 ¹ | – | – | 24 fps |
| the same, unthrottled | 1.42 | **1.99** | – | – | 385–430 fps |
| the same, unthrottled, without ffmpeg's per-frame copy (`unsafe_output`) | – | **1.79** | 1.33 | 1.36 | 400–425 fps |
| NVDEC H.264 + download to host, real time | 1.01 | 1.02 ¹ | – | – | 24 fps |
| the same, unthrottled | 1.07 | **1.63** | – | – | 560–785 fps |
| NVDEC → NVENC, real time | 1.01 | – (few) | – | – | 24 fps |
| the same, unthrottled | 1.09 | **1.67** | 1.17 | 1.21 | 380–440 fps |
| NVDEC → `scale_cuda` 720x404 → NVENC (Plex's own command), real time | 1.02 | 1.04 ¹ | 1.02 | 1.01 | 24 fps |
| the same, unthrottled | 1.14 | **1.68** | 1.19 | 1.27 | 640–780 fps |
| NVDEC HEVC 2160p → `tonemap_opencl` → NVENC, real time | 1.03 | 1.29 ¹ | 1.05 | 1.14 | 24 fps |
| the same, unthrottled | 1.20 | **1.50** ¹ | 1.25 | 1.41 | 62–71 fps |
| NVENC only, 1080p frames from host memory, real time | 1.00 | 1.00 | – | – | 30 fps |
| the same, unthrottled | 1.08 ² | **1.67** (few) | 1.14 | 1.17 | 360–430 fps |
| CUDA: context only, no work | 0.98 | 1.01 | 1.04 | 1.00 | – |
| CUDA: kernel of 0.05 ms, 60 per second | 1.00 | 1.04 ¹ | 1.03 | 1.02 | round trip p99 2.2–2.5 ms |
| CUDA: kernel of 1 ms, 60 per second (6 % duty) | – (few) | 1.27 ¹ | – | – | |
| CUDA: kernel of 4 ms, 60 per second (24 % duty) | 1.11 | 1.37 ¹ | 1.08 | 1.34 | |
| CUDA: kernels of 1 ms back to back | 1.46 | **1.93** | 1.52 | 1.48 | |
| CUDA: kernels of 0.02 ms back to back | 1.06 | **1.56** | 1.09 | 1.07 | 23,000–80,000 per second |
| CUDA: 32 MB pinned host-to-device copy, 60 per second (2 GB/s) | 1.07 | 1.00 | 1.04 | 1.02 ² | |

¹ a mixture: most ticks near 1.0, a share of them longer by the upload time (see below); the
median understates it (p95 33 to 39 ms). ² against the run's ticks alone, the gaps next to it
were lost. All `gpucomp` kernels are one block of 256 threads: they occupy a fraction of one SM.

What the table and the per-phase times say [M]:

1. **The compute engine is time-sliced between processes, exclusively.** A competitor whose
   kernels fit in a fraction of one SM but run back to back takes a third to a half of the
   encoder's time (1.46 to 1.93). Its work could run next to ours on idle SMs; it does not.
   NVIDIA documents this: "Work launched to the compute engine from work queues belonging to
   different CUDA contexts cannot execute concurrently", and the GPU "has a time sliced
   scheduler" between contexts (docs.nvidia.com/deploy/mps/architecture.html). A 0.05 ms kernel
   of the second process waits up to 2.2–2.9 ms next to the encoder (alone: 0.09 ms median,
   0.17 ms worst), i.e. the slice is about 2 ms; no NVIDIA document states the length, a
   measurement in the literature gives the same 2 ms (arXiv 2110.00459).
2. **What costs is the other context's busy time, not its existence.** An idle second context
   costs nothing (0.98 to 1.04), 60 tiny kernels per second 0 to 4 %. Outside the special case
   of item 4 the ratio is about the same at half load and near capacity (1.12 / 1.17,
   1.19 / 1.27, 1.52 / 1.48): the slowdown is a share of the time, not a fixed delay per tick.
   In all three phases of the tick alike (32 × 1080i next to unthrottled `scale_cuda`: 9.6 →
   11.5, 1.67 → 1.96, 1.46 → 1.74 ms).
3. **"Just NVDEC/NVENC" is not free, because the path to and from the fixed-function blocks
   runs through a CUDA context.** Decode only, encode only and decode → encode with no filter
   all slow the encoder when they run unthrottled, by 8 to 21 % (1080p H.264) and 33 to 42 %
   (2160p HEVC), although the SMs do no codec work. Per frame of the competitor that is about
   0.15 ms of our time for a 1080p decode, 0.33 ms for a 1080p NVENC frame from host memory,
   0.25 ms for decode + scale + encode, 0.8 ms for a 2160p 10-bit decode, 3.9 ms for a 2160p
   tone-mapped frame [E, from ratio and frame rate at 32 × 1080i]. ffmpeg's own copy per decoded
   frame is a small part of it (2160p: 1.99 with, 1.79 without). At 24 fps these add up to 0.4
   to 2 % (measured 1.00 to 1.03), tone mapping to 9 % (measured 1.03 to 1.05 at half load, 1.14
   at 60 × 1080i). The cost appears when the transcoder runs faster than real time, which is
   what Plex does in its bursts: it fills its segment buffer at several hundred frames per
   second, then sleeps.
4. **Above 256 sessions the encoder's own overlapped upload is serialized with its kernels
   whenever a second context is busy; this is most of the 60 % of experiment 32.** At
   300 × 480i every busy competitor, including the 0.02 ms kernels with no memory traffic, adds
   13 to 15 ms, and all but about 10 % of it sits in the first phase of the tick (14.5 → 27.9 ms;
   phases two and three 4.33 → 4.80 and 2.62 → 3.07 ms). The Nsight Systems timeline shows it
   directly (`results/nsys/contend-alone300-tick.txt`, `contend-tinycont300-tick.txt`): alone,
   the 302 host-to-device copies of the next tick's frames run next to `k_pyr`, `k_me`,
   `k_mvsmooth` and `k_xf`; with the competitor all 302 copies run first (12.2 ms) and `k_pyr`
   starts after the last one, the kernels themselves no slower (`k_me` 5.46 → 5.30 ms, `k_xf`
   6.38 → 6.71 ms). The threshold is sharp (480i next to the 0.02 ms kernels): 200 sessions
   1.12, 240 1.10, 252 1.10, 256 1.09, **260 1.59**, 280 1.62. The upload is one
   `cudaMemcpyAsync` per session on its own stream, so the step is at more than 256 queued
   copies. 60 × 1080i (60 copies, 187 MB) does not show it; 300 × 480i (152 MB) does, so it is
   the number of copies, not the bytes. The isolated slow ticks of experiment 32 (first phase
   longer by exactly the upload time) are the same thing: they appear at 260, 280 and 300
   sessions with only the idle or lightly active Plex context present (up to 23 per run, excess
   10 to 12 ms, all of it in the first phase) and never at 256 or below. Why the driver does
   this is not known [E: a limit of 256 on something per stream or channel].
5. **Power is a second-order term, and only near capacity.** At half load no competitor causes
   a power violation and the clock goes up, not down (2130 MHz alone, 2430 MHz with a busy
   competitor, 10 to 25 W more). Near capacity the card is already at 58 to 64 W alone; the
   video competitors push it to 65–68 W, `pviol` reads 30 to 100 % and the clock falls from
   2100–2290 to 1890–2070 MHz. That explains the difference between the half-load and the
   near-capacity ratio (1.19 → 1.27, 1.25 → 1.41), about 5 to 12 %. The largest slowdowns have
   no power violation at all (back-to-back kernels at 300 × 480i: 58.8 W, `pviol` 0,
   2355 MHz, ratio 1.93; at 260 × 480i: 61 W, 2370 MHz, 1.59). No thermal violation in any run
   (`tviol` 0, 75 to 91 °C).
6. **Memory bandwidth and PCIe: no.** A 2 GB/s host-to-device copy stream from a second
   process costs 0 to 7 %; decode with download to host costs no more than decode without
   (1.07 / 1.63 against 1.10 / 1.64).
7. **The real transcode during these runs** (bursts, between the competitors): 150 × 480i 1.01,
   32 × 1080i 1.01 to 1.03, 60 × 1080i 1.16, 300 × 480i 1.09 median with p95 36 ms and slow
   ticks of the item-4 kind. Its bursts were shorter and lighter than the 34–38 ms episodes of
   experiment 32; unthrottled `scale_cuda` with Plex's parameters reproduces those (22.8 →
   38.4 ms).

`nvidia-smi compute-policy --set-timeslice` (root; default "Default"; values 1 Short, 2 Medium,
3 Long; applies to contexts created afterwards, lost on driver reload), 32 × 1080i, encoder
and competitors started after each change [M]:

| Setting | 0.05 ms kernel at 60 Hz: ratio, its round trip p99 | 1 ms kernels back to back | `scale_cuda` unthrottled | tone map unthrottled |
|---|---|---|---|---|
| Default | 1.02, 2.3 ms | 1.50 | 1.20 | 1.27 |
| Short | 1.02, 1.6 ms | 1.51 | 1.20 | 1.26 |
| Medium | 1.01, 2.2 ms | 1.51 | 1.20 | 1.28 |
| Long | 1.00, 2.2 ms | 1.54 | 1.21 | 1.26 |

The driver accepts the setting and it changes nothing that matters here (the 4 ms kernel at
60 Hz was lost to Plex bursts in two of the four runs and is not given). Restored to Default
and read back. The setting divides the time differently in principle but cannot remove the
other context's share.

Not done: a run with no other context at all (Plex was never gone); the fix for item 4 (fewer
than 257 copies per tick, e.g. one copy per batch from a contiguous staging buffer, or several
upload streams) was not built, so that it removes the serialization is an inference from the
threshold; Jellyfin's `tonemap_cuda` and Plex's own tone-mapping path were not run (stock
`tonemap_opencl` with a host round trip stood in for them); 720p and the mixed line-up.

Host afterwards: no encoder, ffmpeg or sampler process left, no failed unit, no OOM line in
the kernel log since the start, Plex, Jellyfin and nzbget active, timeslice Default, power limit
70 W. Competitor media (184 MB) stay in `~/totalblonder-spike/out/contend/`.

## 41. The upload serialization of experiment 40: fixed by ordering, not by fewer copies (09:03–10:20) — KEPT

Question: experiment 40 found that above 256 sessions, next to any busy second context, the
overlapped upload ran before the tick's kernels instead of beside them, and proposed one copy per
batch from a contiguous pinned staging buffer. Built, measured, and it is not the cure.

Contamination, stated first: Plex had one, later two, transcoder contexts on the card throughout,
active in bursts for much of the hour (from about 09:40 PST continuously enough that no clean
paced capacity run was possible). `contend_report.py` leaves seconds with Plex activity out of
both sides of every comparison, as in experiment 40; cells that lost all their ticks that way
were repeated once (`e41r-*`) and are marked "lost" where the repeat was lost too. The
late-tick runs at the planning point (`e41w-*`) keep every tick, Plex's included. Also between
two runs at 09:07 no Plex context was present at all, the first such moment of the day; it was
not used for a measurement.

Probes (300 × 480i next to back-to-back 0.02 ms kernels of `gpucomp`, median tick ratio
competitor / alone, `results/contend/probe-*`) [M]:

| Upload | Copies per tick | Ratio | Note |
|---|---|---|---|
| one `cudaMemcpyAsync` per session (as built) | 302 | 1.61 | |
| one `cudaMemcpyBatchAsync` per batch (CUDA 12.8+ batched API) | 1 call, 300 copies | 1.59 | the driver still queues one copy per entry |
| staged: host gather into a pinned buffer laid out like a ring slot, one copy per batch | 1 | – | tick 32.9 ms alone: the gather is 15.9 ms of host `memcpy` (152 MB, 9.6 GB/s) per tick |
| staged, gather skipped (timing only, wrong pictures) | 1 | **1.78** | one 152 MB copy is serialized just the same |
| **per session, upload waits on an event after the tick's parameter copies** | 302 | **1.11** | |
| batched API + the same event | 1 call | 1.11 | |
| staged without gather + the same event | 1 | 1.03 | |

So the number of copies is not the cause. What the timeline of experiment 40 shows is that the
small parameter copies (`d_par`, session lists) that `Batch::begin()` queues on the kernel stream
*before* the kernels ran *after* the upload stream's copies, although they were submitted first:
the copy engine served the upload stream first, and the first kernel depends on those
parameters. With the busy second context this happens for a single large copy too; with the
encoder alone, or with ≤ 256 copies, it did not. Fix: `begin()` queues the parameter copies of all
batches, an event is recorded on the kernel stream, the kernels follow (`launch_begin()`), and the
upload stream waits on the event before its copies. The timeline next to the same competitor
(`results/nsys/contend-fence-tinycont300-tick.txt`): parameter copies at 0.2–0.5 ms, `k_pyr` at
0.51 ms, and the 302 copies run beside `k_pyr`, `k_me`, `k_mvsmooth`, `k_mbstat` and `k_xf`
(compare `contend-tinycont300-tick.txt`: copies 0.17–12.4 ms, `k_pyr` at 12.6 ms).

Why the copy engine reorders, and why at 256: not found in NVIDIA's documentation. The CUDA
driver API's synchronization page only warns that calls may block "due to contention for or
unavailability of internal resources" and that such behavior is undocumented; the documented
guarantee is order within a stream, none between streams. Two user reports on NVIDIA's forum
describe copies on one stream being held up by copies queued on another (single copy engine
ordering on Jetson; a peer copy stalling later copies on another stream), and Bakita et al.
(RTAS 2024) list copy-engine ordering among the open questions of GPU scheduling. The 256 is
undocumented; the guess of experiment 40 (a per-stream or per-channel limit of 256 entries)
stays a guess, and the fix does not depend on it.
Sources: docs.nvidia.com/cuda/cuda-driver-api/api-sync-behavior.html;
forums.developer.nvidia.com/t/ordering-of-cudamemcpyasync-issued-to-separate-streams-on-jetson-agx/70173;
forums.developer.nvidia.com/t/queueing-device-to-device-peer-memcpy-stalls-concurrent-copy-operations/295894;
www.cs.unc.edu/~jbakita/rtas24.pdf.

Other per-session API calls (timeline, 300 × 480i): per tick 304 `cudaMemcpyAsync` (302 uploads,
the rest per batch), 17 kernel launches, one `cudaMemsetAsync` per batch, one
`cudaStreamSynchronize`, three waits; no per-session memset or stream operation. 1080-line
formats take three copies per session (planes with padding rows), so 86 or more sessions of
1080 lines would also have crossed 256; with the fence the count no longer matters.

Correctness: streams byte-identical to the previous build on every upload path
(`scripts/upload_ident.sh`: 480i at coded size, 1080i with padding rows, a 696×470 crop with
padding columns and rows, a mixed two-rate run; with and without `--overlap`; 74 streams each,
also for the two discarded modes); `make check` 15/15; `make check-full` 54/54; `qdiff` 0.000 dB.

Uncontended free-run, A/B/A/B (Mmb/s, old → new; `e41-preup1/2`, `e41-fence1/2`) [M]:

| | 1080i | 720p | 480i | mixed |
|---|---|---|---|---|
| pair 1 | 19.27 → 18.81 | 22.06 → 21.88 | 16.80 → 17.01 | 20.17 → 20.21 |
| pair 2 | 18.70 → 18.86 | 21.76 → 21.67 | 16.59 → 16.69 | 19.90 → 19.96 |
| mean change | −0.8 % | −0.6 % | +1.0 % | +0.3 % |

Within the scatter of single runs (spread up to 8 % at 480i): neither a cost nor a gain.

Paced 480i capacity: no clean run (the transcoder was busy in every attempt; `e41-cap*`). Next to
the live transcode, alternately: 390 sessions on time old 76–90 % (p99 47–48 ms), new 87–90 %
(p99 35 ms); 400 old 41–75 % (p99 47–56 ms), new 86–95 % (p99 35–41 ms); 410 old 25–54 %, new
70–91 %. Below 99 % in all, so the clean capacity of §0.1 (400) is neither confirmed nor moved;
the encoder alone is unchanged (the fence only matters when another context is busy) [E].

One competitor, before → after (median tick ratio; on time in the competitor's window;
"slow" = single ticks > 1.25 × the median) [M]:

| Competitor | 300 × 480i | 400 × 480i | 60 × 1080i |
|---|---|---|---|
| alone, ms | 22.6 / 22.4 | 31.1 / 30.9 | 25.2 / 25.1 |
| back-to-back 0.02 ms kernels | 1.62 (0 %) → **1.14 (100 %)** | 1.55 → **1.11** | 1.11 → 1.11 |
| Plex's pipeline (1080p H.264 NVDEC → `scale_cuda` 720×404 → NVENC), unthrottled | 1.69 (17 %) → **1.23 (100 %)** | 1.60 → **1.23** | 1.27 → 1.27 |
| the same, real time | 1.07 (76 %, 41 slow) → **1.01 (100 %, 0)** | 1.02 (90 %, 12 slow) → 1.01 (100 %, 0) | lost → 1.00 |
| 2160p HEVC 10-bit → `tonemap_opencl` → NVENC, unthrottled | 1.77 (25 %) → **1.41 (84 %)** | 1.41 → 1.30 | 1.40 → 1.43 |
| the same, real time | 1.29 (64 %, 43 slow) → **1.12 (100 %, 0)** | 1.18 → 1.13 | 1.14 → 1.12 |

At 400 × 480i the card is at capacity alone (93 % of the period), so every ratio above 1.07
makes the ticks late; the on-time column says nothing there. The expectation of experiment 40
held: 1.6–1.8 became 1.1–1.4, the same ratios as at 60 × 1080i, which never had the problem, and
the single slow ticks are gone (41–43 per run before, none after).

### 41b. Several transcodes at once (new binary)

`contend.sh` runs `A+B+C` as simultaneous processes (one context each). New competitor `x4k`:
2160p HEVC 10-bit → `scale_cuda` 1920×1080 nv12 → H.264 NVENC 8 Mbit/s (a 2160p source
transcoded without tone mapping). `tm` = the tone-map pipeline above, `1080p` = Plex's pipeline.
RT = real time (`-re`, 24 fps), max = unthrottled. Median tick ratio, on time in brackets where
below 100 % (`results/contend/e41m-*`, `e41r-*`; `contend_table.py`) [M]:

| Competitors | 60 × 1080i (76 % of period alone) RT | max | 300 × 480i (68 %) RT | max | mixed × 17 (52 %) RT | max |
|---|---|---|---|---|---|---|
| 1 × 1080p | 1.00 | 1.27 | 1.01 | 1.23 | – | – |
| 2 × 1080p | 1.02 | 1.27 (97 %) | 1.03 | 1.25 | 1.01 | 1.27 |
| 3 × 1080p | 1.03 | 1.26 | 1.03 | 1.25 | 1.02 | 1.27 |
| 1 × 2160p | 1.03 | 1.95 (0 %) | 1.05 | 1.93 (0 %) | 1.03–1.06 | 1.96–2.07 (16–43 %) |
| 2 × 1080p + 1 × 2160p | lost | 1.47 (1 %) | 1.06 | 1.47 (53 %) | 1.06 | 1.57 (98 %) |
| 1 × 1080p + 2 × 2160p | 1.12 (96 %, few) | 1.77 (0 %) | lost | 1.77 (0 %) | 1.13 | 1.85 (72 %) |
| 1 × tm | 1.12 | 1.43 | 1.12 | 1.41 (84 %) | – | – |
| 1 × tm + 2 × 1080p | 1.16 (91 %) | 1.61 (0 %) | 1.10 | 1.59 (12 %) | lost | 1.71–1.88 (56–87 %) |
| 2 × 2160p + 1 × tm | 1.22 (few), 1.33 (47 %) | 1.80 (0 %) | 1.18 | 1.74–1.78 (0 %) | 1.28 | 1.99 (42 %) |

Competitors' own rates: every real-time competitor kept 24–26 fps in every combination, three at
once included. Unthrottled, they share the decoder and encoder: 1 × 1080p 620–690 fps, 2 × 300–365,
3 × 206–242 each (the total stays 620–730); 2160p 310–350 fps alone, 124–137 next to others;
tone mapping 58–73 fps. No session or throughput limit stopped a real-time transcode. (The
RTX 2000 Ada is a workstation card; NVIDIA's session limit for consumer cards does not apply
[K].)

Is the cost additive in competitor frames? At real time, yes within the scatter: per frame of
the competitor about 0.25 ms of our time for a 1080p transcode (three of them at 24 fps: 1.8 %
predicted, 2–3 % measured), 1.2–2 ms for a 2160p one (from 1.03–1.05), 3.9 ms for a tone-mapped
2160p frame (1.12); 1 × 1080p + 2 × 2160p predicted 1.07–1.11, measured 1.12–1.13; 2 × 2160p +
tm predicted 1.18–1.22, measured 1.18–1.28 (1.33 in a run with the two live Plex sessions also
active). No extra cost per additional context is visible. Unthrottled, it is not additive and
does not grow with the number of contexts: the competitors share the video engines, so their
frames per second split, and the ratio is set by the heaviest pipeline present (3 × 1080p 1.26
= 1 × 1080p 1.27; a single 2160p transcode 1.93–2.07, more than 2160p mixed with 1080p ones).
The worst case is about 2.0: the encoder gets half the time. The round-robin hypothesis
("each busy context takes its own 2 ms slices") would give 3 or 4 with three busy competitors;
it does not happen because the competitors' contexts are idle while they wait for the video
engines [E].

### 41c. Late ticks at the planning point

Each format paced at about 83 % of the period alone (65 × 1080i 27.7 ms; 360 × 480i 27.7 ms; mixed
× 23 12.1 ms = 73 %), the worst cases next to it, every tick kept (`e41w-*`,
`contend_late.py`; "repeat share" = 1 − period / mean tick, the pictures a skip-on-deadline
policy would repeat to keep real time; backlog = how far the paced encoder fell behind) [M]:

| Competitors | 1080i | 480i | mixed |
|---|---|---|---|
| 2 × 2160p + 1 × tm, real time | median 1.03 of the period; 72 % late; longest late run 0.73 s; repeats 2 %; backlog ≤ 0.25 s | 1.04; 68 % late; 1.0 s; 4 %; 0.44 s | 0.88; 27 % late; runs ≤ 4 ticks; 0 % |
| 1 × tm + 2 × 1080p, real time | 0.97; 15 % late; 0.47 s; 0 % | 0.95; 5 % late; 0.13 s; 0 % | 0.83; 3 % late; ≤ 3 ticks; 0 % |
| 2 × 2160p + 1 × tm, unthrottled | 1.40; 87 % late; 5.7 s (the whole window); repeats 26 %; backlog 2.4 s | 1.39; 88 %; 5.7 s; 26 %; 2.4 s | 1.36; 86 %; 5.7 s; 24 %; 2.3 s |
| 1 × 2160p, unthrottled | 1.61; 100 %; 5.2 s; 36 %; 5.2 s | 1.57; 100 %; 5.2 s; 35 %; 4.7 s | 1.47; 99 %; 5.4 s; 31 %; 3.6 s |

So at 83 % a real-time worst case costs a few percent of pictures in runs under a second; an
unthrottled 2160p burst costs a quarter to a third of the pictures for as long as it lasts
(Plex's bursts on 1080p sources lasted 1–3 s in experiment 32; 2160p bursts were not observed).

Correction to the harness: `contend_report.py` placed tick i at T0 + i × period; when the
encoder falls behind, ticks run later than that and were attributed to the wrong competitor. It
now uses the actual start (the later of the due time and the end of the previous tick). Runs in
which the encoder kept up are unchanged; the rows above use the corrected times.

Not done: a clean paced capacity at 480i (and 720p, 1080p) with the new binary; 720p and 1080p
next to competitors (the ratios are a share of time and should carry over [E]); Plex's own
tone mapping and Jellyfin's `tonemap_cuda` (stock `tonemap_opencl` stands in); how long Plex
bursts on 2160p sources last.

Host afterwards (10:00 PST): no encoder, ffmpeg, `gpucomp`, sampler or profiler process left; no
failed unit; no OOM line in the kernel log; Plex, Jellyfin and nzbget active; power limit 70 W
(default 70 W); timeslice policy Default. Old binary kept as `build/m2venc-preup`, new as
`build/m2venc-fence` on `nixark`.

# Work package C: input path on the device, dynamic sessions, deadline policy (2026-11-25, from 10:03 PST)

Start 10:03 PST. Host before the first measurement: no process on the card (`nvidia-smi pmon`
empty), no `m2venc`/`ffmpeg` of ours, 19 GiB available, 55 °C, 70 W limit, timeslice Default.
New binaries: `build/decbench` (decode only), `build/m2vpipe` (decode → convert → encode),
`build/m2venc` (now with run-time sessions, device input, repeats). ffmpeg in the flake is
9.0.2 with `--enable-nvdec --enable-cuvid`; no flake change was needed (the headers and
libraries of the `ffmpeg` package are already in the build environment; `-lcuda` comes from
`/run/opengl-driver/lib`).

## 42. Decode through libavcodec inside the encoder's CUDA context (10:05–10:20)

- `src/dec.h`: `av_hwdevice_ctx_alloc(CUDA)`, `hwctx->cuda_ctx = cuCtxGetCurrent()` (the primary
  context the CUDA runtime uses), `hwctx->stream` = a non-blocking stream of its own, then
  `av_hwdevice_ctx_init`. Works at the first attempt: frames arrive as `AV_PIX_FMT_CUDA` with
  `sw_format` nv12 / p010le, `data[0]`/`data[1]` are device pointers our kernels read directly.
- AV1 trap: `avcodec_find_decoder(AV_CODEC_ID_AV1)` returns libdav1d, which has no hwaccel; the
  first run decoded AV1 on the CPU at 1 core per stream with NVDEC at 0 %. The native decoder
  must be asked for by name (`av1`). `dec.h` does that and reports any input whose frames are
  not on the device.
- Inputs: `scripts/prep_inputs.sh` (18 files, 249 MB under `media/in/`, 10 s each).

## 43. Encoder: run-time sessions, device input, repeats (10:20–10:35)

- Slots: a batch keeps `sess[slot]` (nullptr = free); `open()` takes a free slot without any
  device work; `close()` flushes and frees the slot when the last picture is out; `reserve()`
  makes a batch on a side thread (buffers cleared on a non-blocking stream of its own), `step()`
  adopts it at its start; `reap()` frees batches that stayed empty, also on a side thread.
- `submit_dev()` returns the ring planes for the caller's kernels (queued on the encoder's stream
  before the tick); `submit_again()` codes the previous frame again from the same ring slot.
  Ring slots are now counted per real frame, not per display index (same values without repeats).
- Repeats made on the host (`Batch::host_repeat`): per slice two coded macroblocks with a zero
  vector, the rest skipped; a P repeat leaves the reference slots alone and marks the session
  `dup`, B pictures before it in display order then repeat too; a key frame position becomes a
  repeat P without GOP header and the next submitted frame is typed I off the grid.
- Checks: old binary `m2venc-fence` against the new one, 74 streams: identical. Quick gate 15/15,
  quality unchanged. `pipe_check.py`: device input identical to host input (23 streams); random
  repeats 10 / 50 / 90 % and holds: strict decode clean in ffmpeg and libmpeg2, both buffer models
  hold, coded pictures match the decoder to 63 dB or better (no drift after repeats); churn: 27
  undisturbed streams byte-identical to the run without churn, 79 churned streams decode.

## 44. NVDEC capacity, first pass (10:20–10:45) — see the table in the spike notes

`scripts/decbench.py --label a1` (142 runs of 7–10 s; `results/dec/a1.jsonl`). 35 runs were
disturbed (my own gate runs during the HEVC 1080p files, a Plex transcode from about 10:38 during
HEVC 2160p 8-bit, MPEG-2 and VP9); those rows were measured again later (experiment 49).
First findings, all [M]:

- H.264 1080p: 900–935 fps in total whatever the number of streams (1 to 16), NVDEC at 100 %.
  720p 1830 fps, 480-line 3200–3300 fps. HEVC 1080p 2050 fps, 2160p 8-bit 530 fps, 10-bit 470 fps.
  AV1 1080p 1800 fps (after the libdav1d trap of experiment 42).
- Paced at the limit the decoder does not drop anything: frames leave it later and later
  (H.264 1080p30: 24 streams hold, 29 recover from the start-up backlog and hold, 33 are 0.9 s
  behind after 10 s and still growing).
- Host CPU: 0.004 to 0.012 core per real-time stream (demux, parsing, API).
- Device memory per decode session: 65 MB H.264 1080p, 35 MB 720p, 12–14 MB SD, 73 MB HEVC 1080p,
  235 MB HEVC 2160p 8-bit, 431 MB HEVC 2160p 10-bit.

## 45. Pipeline harness: first paced runs (10:45–11:00)

`build/m2vpipe`, `scripts/pipe_lineup.py`. Mistakes found on the way, all in the harness:

- Frames were counted late because the frame clock gave a frame only 60 ms after its packets:
  with two B pictures a frame leaves the decoder up to two frame periods after its nominal
  time. The lead is now 300 ms (a real mapper uses its jitter-buffer target, SPEC 5.3: 1 s);
  the decoded frames wait on the device during that time (about lead × rate frames per input).
- Looping a transport stream by a time seek resumed after the first key frame; the frame count
  then ran half a second behind for the rest of the run (17 % of MPEG-2 frames "late"). Loops
  of `.ts` inputs seek to byte 0 now. A byte seek "succeeds" on Matroska and then delivers
  nothing: that made every decode thread spin (4.7 cores) for one run.
- A 29.97p source into a 29.97i output with one selection per field: for half of all source
  phases every output frame took its two fields from two source frames. A source at the output
  frame rate now gets one selection per frame (both fields from one frame, `progressive_frame`
  = 1); other rates keep the per-field rule (3:2 from 24p, successive frames from 50/60p).
- AV1 through libdav1d (experiment 42).

First results (a Plex transcode was on the card at low duty during these; marked):

- 1080p30 H.264 → 1080i: 27 channels hold, 28 do not: NVDEC is at 92–95 %, the encoder's tick
  is 7.0–7.3 ms of the 16.7 ms base tick, SM 40 %. **The decoder is the limit, not the encoder.**
- Decoders in a context of their own (`--dec-ctx own`): same limit, tick 7.98 against 7.27 ms at
  28 channels and 7.61 against 7.02 at 27 (+8 to +10 %): two contexts in one process time-slice
  like two processes. The decoders stay in the encoder's context.
- `unsafe_output` (no copy into a pool frame): 9 % of frames late at 28 against 18 % (both fail);
  `h264_cuvid` instead of the hwaccel: worse (84 % late, ticks late). The hwaccel with pool copy
  stays the default.
- 720p60 H.264 → 720p: 26 channels (NVDEC 94 %).

## 46. Conversion kernels: geometry, quality, cost (11:00–11:15)

`src/pipe.h`, `src/convbench.cu`, `scripts/pipe_check.py` (`geom-*`, `pipe-*`, `--quality`).

- A CPU reference for interlaced material does not exist in ffmpeg: `zscale` scales an
  interlaced frame as if it were progressive (a frame with a white top field and a black
  bottom field comes out gray), `scale=interl=1` scales the fields separately but puts both
  at the same vertical phase. The geometry is therefore checked against an analytic pattern
  (sinusoids along both axes in luma and chroma, coded with H.264 at qp 2, decoded by NVDEC):
  every output sample must have the value of the pattern at the source position the mapping
  assigns to it. Seven conversions: 47.8 to 55.5 dB [M]; the field → field cases 52.1 and 48.7 dB
  where swscale's field mode reaches 27.8 and 34.7 dB; bob at 59.94p over both field parities
  50.2 dB in the worst frame (no line jitter between fields).
- First version of the pattern check failed five of seven cases at 31.7 / 23.6 dB with the CPU
  reference failing identically: `-colorspace bt709` as an encoder option made ffmpeg convert
  the pattern's matrix. The description is set with `setparams` now.
- Against zimg Lanczos-3 on real content (`results/pipe/quality.txt`): Lanczos-3 52.2–57.5 dB
  (two implementations of the same filter; the rest is rounding of the 8-bit intermediate),
  Catmull-Rom 45–49 dB, bilinear 38–40 dB. Same-size paths are bit-exact (H.264, AV1); MPEG-2
  through NVDEC against libavcodec is 61 dB (the inverse transform is not bit-exact between
  decoders).
- Cost per output frame (`results/pipe/convbench.txt`, 16 jobs per launch, nothing else
  running): copy 30 µs (1080), 8 µs (720p); 1080p → 720p 99 µs (bicubic 77, bilinear 63);
  2160p → 1080p 300 µs (10-bit 401; bilinear 184); 2160p → 704×360 240 µs; 1080p60 → 1080i
  with the vertical low-pass 228 µs; 1080i → 480i by fields 64 µs; bob to 720p 62 µs; 3 × 3
  mosaic of 720p sources into 1080 291 µs, 4 × 4 773 µs; fill 15 µs, color matrix 30 µs,
  overlay 34 µs at 1080. For scale: one 1080i picture costs the encoder about 440 µs at
  capacity, one 480i picture 75 µs. A 2160p source into a 480i channel costs three times its
  encode. Not optimized: the decoder is the limit in every line-up measured (below).
- Deinterlacers against the truth (`scripts/deint_quality.sh`; the interlaced clip was woven
  from the 1080p50 original, so every field has a true frame): to 720p59.94, luma PSNR / SSIM:
  pipeline bob Lanczos 34.78 dB / 0.972, pipeline bob bilinear 32.67 / 0.963, cuvid bob 34.24 /
  0.969, ffmpeg yadif 33.12 / 0.967, ffmpeg bwdif 35.48 / 0.977. (cuvid adaptive: experiment 51.)

## 47. Line-up capacities, decoders in the encoder's context (11:00–11:45)

`scripts/pipe_lineup.py`, 20 s paced runs, criterion: 99 % of ticks on time and at most 1 % of
decoded frames more than the lead (300 ms) after their time. `results/pipe/cap3.jsonl` (no
other process on the card), `cap4.jsonl`.

| Line-up | Holds | Fails | NVDEC at the limit | Tick mean at the limit (period 16.7 ms) | SM | Host CPU, cores |
|---|---|---|---|---|---|---|
| a: 1080p30 H.264 → 1080i | 27 | 28 (2.7 % of frames late) | 91–94 % | 6.1 ms | 35 % | 0.37 |
| a_mix: H.264 1080p30 / H.264 1080i / HEVC 1080p30 / MPEG-2 1080i → 1080i | 29 | 30 (3.2 %) | 89–94 % | 6.6 ms | 38 % | 0.40 |
| c: mixed unit (1 × 1080i + 1 × 720p + 4 × 480i from nine kinds of input) | 8 units = 48 channels | 9 (44 %) | 87 % | 4.9 ms | 26 % | 0.57 |
| e: 3 × 3 mosaic into 1080i with overlay, nine decoders each | 4 channels (36 inputs) | 5 (8 %) | 75 % | 2.0 ms | 12 % | 0.36 |

In every line-up the decoder gives out first; the encoder's tick is at 12 to 40 % of its period.

## 48. Host path against device path; hybrids (11:15–12:00)

Fixed line-ups, a Plex transcode at 1–2 % of the decoder on the card (`results/pipe/host2.jsonl`):

| Line-up | Path | Host CPU, cores (per channel) | PCIe to the card, MB/s | Tick mean, ms | Ticks on time | W |
|---|---|---|---|---|---|---|
| 8 × 1080p30 → 1080i | host (software decode, swscale, upload) | 1.83 (0.229) | 808 | 3.23 | 100 % | 23 |
| | device | 0.10 (0.013) | 14 | 1.90 | 100 % | 26 |
| 16 × | host | 3.62 (0.226) | 1389 | 6.34 | 99.33 % | 28 |
| | device | 0.20 (0.013) | 23 | 3.92 | 100 % | 34 |
| 20 × | host | 4.56 (0.228) | 2068 | 9.11 | 93.99 % | 32 |
| 24 × | device | 0.27 (0.011) | 31 | 5.43 | 100 % | 47 |
| 2 mixed units (12 channels) | host | 1.35 (0.113) | 474 | 2.14 | 100 % | 21 |
| | device | 0.13 (0.011) | 39 | 1.66 | 100 % | 23 |
| 4 mixed units (24 channels) | host | 2.56 (0.107) | 955 | 5.09 | 100 % | 24 |
| | device | 0.28 (0.012) | 28 | 2.92 | 100 % | 28 |

The device path needs a seventeenth of the host CPU and a fiftieth of the bus traffic; it
draws 3 to 6 W more at equal line-up (NVDEC). The host path stops at about 16 channels of
1080 on this VM (the tick thread loses the CPU to 20 decode threads); decode threads now run
at nice 10.

Two harness faults found here: 40 software inputs page-locked their frame buffers inside the
decode threads at the start (1280 `cudaHostAlloc` calls), which held the driver, and with it
the tick thread, for most of a 20 s run; the buffers are allocated before the run now.

## 49. Sessions at run time under load (11:20–11:35)

`scripts/churn_bench.sh`: 48 × 1080i stable (57 % of the 16.7 ms tick) plus pools of 8 × 1080i,
6 × 720p, 16 × 480i, 4 × 1080p opened, closed and reconfigured at 2 operations per second (mean
tick 11.4–11.5 ms, 69 %), 24 s, `results/churn/churn48-*.txt` [M]:

| Mode | Late ticks of 1440 | Ticks with an operation: p99 / max, ms | Without: p99, ms | Cost of the operations |
|---|---|---|---|---|
| no operations | 0 | – | 11.1 | – |
| batches allocated inside the tick (`naive`) | 1 | 19.7 / 19.7 | 13.8 | open in a free slot 2 µs; close < 1 µs; open with a new batch of 4 slots: mean 16.8 ms, max 85.8 ms (9.4 ms without the first, larger one) |
| batches made on a side thread (`reserve`) | 3 | 19.1 / 19.1 | 13.7 | reserve 0.06 ms, adopt 2 µs on the tick thread; the side thread needs 7.9 ms (max 10.5) and the tick thread's CUDA calls wait behind its driver allocations; an open waits 5.5 ms on average (max 33) for its batch |
| batches laid out in an arena taken at start (`arena`) | 0 | 13.1 / 13.1 | 13.7 | open with a new batch: mean 0.46 ms, max 1.85 ms, inside the tick |

The side thread was the first design and is not enough: the allocation itself (`cudaMalloc`,
page-locked host memory) takes a lock inside the driver that kernel launches of other threads
wait for. The arena removes the driver from the operation. Byte identity of the undisturbed
sessions holds in all four modes (gate: 36 streams).

A new *input* at run time creates a decoder (driver work that no arena covers): opened on a side
thread while 20 channels ran, three decoders were ready after 32, 3 and 15 ms; the longest tick in
the 1.5 s after each was 5.8, 9.2 and 8.8 ms (p99 of the run 6.8 ms, period 16.7 ms), no late tick.

## 50. Deadline policy (11:35–11:50)

Three versions, all measured with `contend.sh` (65 × 1080i from host memory, 80–85 % of the
period alone, next to transcodes):

1. Cost per macroblock from a moving average, pictures shed until the estimate fits. Next to an
   unthrottled 2160p transcode: 44 % of pictures repeated, the backlog stays under 22 ms
   (without the policy: every tick late, 3.6 s behind after 10 s, still 2.8 s behind 15 s after
   the burst). But 43–55 % of the ticks ended just after the deadline: the estimate was the
   mean and had a 2 % per tick downward drift for recovery, so the tick rode the deadline.
2. With a safety factor, and a real Plex burst at the start of the run: **98 % of all pictures
   repeated for the whole run**, alone as well. Two mechanisms locked together: (a) with almost
   everything shed, the tick still took 17 ms (the upload of 65 host frames), which the model
   charged to the one session it coded, so every macroblock looked expensive; (b) every missed
   key frame asked for a key frame at the next frame, which was missed again: after a second
   every frame of every session was typed I, the class shed last.
3. Current: (a) the plan uses the throughput (macroblocks per second) of the best of the last
   three ticks; a tick that ends early raises the estimate by the ratio of time available to
   time used, so there is no state to get stuck in; one picture is always coded when there is
   time; (b) a missed key frame gets one make-up attempt, then the session waits for the grid.
   Result (`results/contend/dl-on-1080i65`, no other process on the card):

| Next to | Ticks late | Pictures repeated | Largest backlog | Competitor |
|---|---|---|---|---|
| nothing (the gaps, including right after each burst) | 0 % | 0 % | 0 ms | – |
| one 2160p transcode, unthrottled | 3.3 % | 47.9 % | 17 ms | 347 fps |
| one 1080p transcode with `scale_cuda`, unthrottled | 1.0 % | 13.3 % | 1 ms | 639 fps |
| two 2160p transcodes and a tone-mapped one, real time | 7.9 % | 10.8 % | 14 ms | 25, 24, 25 fps |
| 1080p, 2160p and scaled 1080p transcodes, real time | 2.3 % | 0.7 % | 11 ms | 25, 25, 25 fps |
| without the policy: one 2160p transcode, unthrottled | 100 % | – | 3646 ms | 348 fps |

   (The first 5.7 s of that run show 4.8 % repeats alone: a Plex transcode started on the card
   at that moment.) A late tick here ends a few milliseconds after the next one was due.

## 51. The shared decoder: who loses (11:50–12:00)

Pipeline with the deadline policy next to the transcodes of `contend.sh`, 10 s each
(`results/contend/pc-a24`, `pc-a26`, `pc-c7`; competitors alone: `alone-comps`: 2160p transcode
400 fps, 1080p transcode 460 fps, 1080p with `scale_cuda` 899 fps, H.264 decode 905 fps):

| Our line-up (NVDEC used by it) | Next to | Our ticks late | Our output frames that repeat their input | Competitors |
|---|---|---|---|---|
| 24 × 1080p30 → 1080i (80 %) | 1080p + 2160p + scaled 1080p, real time | 0 % | 0 % | 25, 25, 25 fps |
| | two 2160p + tone-mapped, real time | 0 % | 0 % | 20, 20, 19 fps: below real time |
| | 2160p transcode, unthrottled | 0 % | 0 % | 38 fps (alone 400) |
| | 1080p transcode, unthrottled | 0 % | 0 % | 65 fps (alone 460) |
| | H.264 1080p decode, unthrottled | 0 % | 0 % | 63 fps (alone 905) |
| 26 × (88 %) | one 1080p, real time | 0 % | 0 % | 25 fps |
| | 1080p + 2160p + scaled 1080p, real time | 0 % | 0 % | 25, 19, 25 fps |
| | two 2160p + tone-mapped, real time | 0.1 % | 46.8 %, and 58 % in the 5 s after it | 18, 18, 18 fps |
| | 2160p transcode, unthrottled | 0 % | 2.9 % | 20 fps |
| 7 mixed units, 42 channels (76 %) | 1080p + 2160p + scaled 1080p, real time | 0 % | 0 % | 25, 25, 25 fps |
| | two 2160p + tone-mapped, real time | 0 % | 21.7 % | 25, 25, 25 fps |
| | 2160p transcode, unthrottled | 0 % | 0 % | 51 fps |

(The first window of `pc-a26` had 9.7 % repeated input: a real Plex transcode ran flat out on
the card at that moment, 80 % of the decoder.)

The decoder is shared by the picture: a session that asks for more than its share gets what is
left. Our many small real-time sessions keep their rate as long as the total fits; a transcode
gets the remainder, so Plex's faster-than-real-time bursts shrink to a tenth, and 2160p
real-time transcodes fall below real time once we use more than about 80 %. Past the total
(26 channels plus three heavy transcodes) both sides lose, and our decoders do not recover by
themselves afterwards: they are behind by what they could not decode and catch up only as fast
as the spare capacity allows.

## 52. Decoder table without the transcode, software decode, hybrids (12:00–12:20)

- The six inputs whose rows had been disturbed (experiment 44) were measured again with no
  other process on the card (`results/dec/a2.jsonl`, 47 runs, none disturbed). MPEG-2 1080i is
  810 / 870 / 865 fps at 1 / 2 / 4 streams, not 322 → 768 as in the first pass; VP9 1430 / 1678;
  MPEG-2 480i 2801 / 3235. The apparent growth with the number of streams was the Plex transcode.
- Software decode, one thread, cores per real-time stream (`decbench --sw`, `a1.jsonl`): H.264
  1080p30 0.19, 1080i 0.23, 1080p60 0.32, 720p60 0.15, 720p50 0.13, 1366×768p30 0.08, 480i 0.046,
  854×480p30 0.033; HEVC 1080p30 0.24, 10-bit 0.30, 2160p30 1.05 (28 fps on one thread), 2160p24
  10-bit 1.37 (17 fps); MPEG-2 1080i 0.068, 480i 0.011; AV1 0.18 (libdav1d); VP9 0.16. Four
  threads decode four streams at the same cost per frame (3.97 cores).
- Hybrid line-ups (`results/pipe/hyb2.jsonl`, `hyb3.jsonl`; a Plex transcode at 1–2 % of the
  decoder present): 22 × 1080p30 on NVDEC plus software-decoded ones: 30 channels hold (2.1
  cores, NVDEC 79 %), 34 hold (99.62 % of ticks, 2.95 cores), 38 do not (97.0 and 90.5 % of
  ticks in two runs, 3.8–4.0 cores: the tick thread is late on a host whose 8 vCPUs are shared).
  Mixed units with the four SD inputs in software: 9 units 98.9 and 99.6 % of ticks in two runs,
  2.5 cores. Decode threads run at nice 10 since these runs.
- Clean-up of line-up (b) and (d) (`cap4.jsonl`, Plex at 1 %): 720p60 → 720p holds 27 (28:
  10.8 % of frames late); 2160p/1080p → 480i holds 24 (25: 26 %).

## 53. Closing series: decode variants, priority, resynchronization, the half stage, device input at the encoder's limit, kernel shares (12:15–12:40)

- Decode variants at 4 streams (`results/dec/a1-var.jsonl`, `a1-table.md`): no route changes the
  engine's rate. Decoder surfaces instead of the pool copy: SM utilization 13 % instead of 26 %
  at 471–495 fps of 2160p 10-bit, 45 % more device memory (623 against 431 MB per decoder).
  `*_cuvid`: 1.5 to 2 times the host CPU per frame. cuvid adaptive deinterlace: decode rate
  unchanged, **SM 34 %** at 819 frames/s of 1080i (63 % with a frame per field); cuvid bob 4 %.
  (The first variants run included the gate's pattern clips and stopped at an unknown codec
  prefix; rerun on the 18 real inputs.)
- Deinterlacers again, cuvid through the ffmpeg program this time (in the harness its frames
  were a frame late and mis-timed, which gave 18–25 dB): adaptive 34.66 dB / SSIM 0.9742 (its
  output one frame later), bob 34.41 / 0.9719; pipeline bob 34.78 / 0.9722; bwdif 35.48 / 0.9766.
- **Half stage** (`k_half`, ratios of 3 and more): 2160p → 720p 264 → 179 µs (10-bit 336 → 232),
  2160p → 704×360 240 → 143 µs (359 → 199); against zimg Lanczos 57.5 → 49.5 dB (2160p → 720p),
  against the analytic pattern 51.4 and 49.2 dB (geometry and chroma siting kept). On by default,
  `--exact-down` turns it off. The stage itself costs about 70 µs per 2160p frame: its chroma
  path reads bytes one at a time.
- **Priority** (`results/contend/prio-1080i`): 33 sessions of priority 1 and 32 of priority 0
  next to an unthrottled 2160p transcode: 0.06 % and 39.6 % of pictures repeated; in the burst
  25.3 % overall, 7.3 % of ticks late, at most 16 ms behind. The order was class first, then
  priority, until this run; it is priority first now.
- **Resynchronization** (`--resync 600`, `results/contend/pc-a26-resync`): 26 channels next to
  two 2160p and a tone-mapped transcode in real time: 32.1 % of output frames repeat their input
  (46.8 % without), 17 % in the 2.5 s after (58 %), worst frame 592 ms late (866), 7
  resynchronizations, 410 packets skipped; competitors 19 fps each.
- **Device input at the encoder's limit**: 67 × 1080i with one device copy per frame: 98.75 % of
  ticks, mean tick 29.95 ms; from host memory with the overlapped upload: 100 %, 28.08 ms. 70:
  92.4 % (32.0 ms) against 100 % (29.8 ms). The copy is compute inside the tick; the upload
  was not. About 7 % of the encoder's capacity, irrelevant while the decoder ends at 27.
- **Kernel time by kernel** (Nsight Systems, `scripts/nsys_pipe.sh`): line-up (a) at 27:
  conversion 6.5 % (`k_conv_copy`), the driver's `ConvertNV12BLtoNV12` 6.2 %, `k_me` 25.8 %;
  (c) at 8 units: conversion 10.9 %, driver conversion 6.6 %; (d) at 24: conversion 44 %
  (`k_conv_h` 22.4 %, `k_half` 13.8 %, `k_conv_v` 4.5 %), driver conversions 26.1 %, `k_me` 9.8 %.

Correction to experiment 49: the counts of "open with a new batch" include the batch of the 48
stable sessions made at start (2.9 GB: 73 to 86 ms from the driver, 1.85 ms from the arena).
The batches of 4 slots made during the run: from the driver in the tick 5.3 ms on average, all
operations of one tick 9.4 ms at most; from the arena 0.23 ms on average, 0.41 ms at most per
tick. Late ticks and tick percentiles are as stated.

## 54. End of work package C (12:35 PST)

Final gates with the final binaries: `make check-full` 54 of 54 (97 s, quality identical to the
baseline), `make check-pipe` 46 of 46. Sustained: 7 mixed units (42 channels) 180 s and 24 ×
1080p30 → 1080i 120 s with deadline and resynchronization, every tick on time
(`results/pipe/sustain.jsonl`). Conversion classes: five classes of the mixed line-up took
18.7 ms to make at start (tap tables allocated and uploaded on the tick thread).

Host afterwards (12:32 PST): no process of ours left (`m2venc`, `m2vpipe`, `decbench`,
`convbench`, ffmpeg, samplers, profilers); no failed unit; no OOM line in the kernel log since
the start of this work (10:00 PST), no Xid or NVRM line; Plex, Jellyfin and nzbget running;
power limit 70 W (default 70 W); timeslice policy Default; card idle at 56 °C, 120 MiB; 18 GiB
available. Under `~/totalblonder-spike`: media 20 GB (of it `media/in` 0.25 GB new), `out`
1.5 GB, `build` 1.5 GB. Nothing was installed or changed outside that directory; sudo was not
used in this package (no Nsight Compute run; Nsight Systems needs no root). During the work
Plex transcodes were on the card three times (about 10:38–11:05, 11:10–11:40 with a burst at
80 % of the decoder around 11:13 and 11:52, and one more start at 11:45 PST): the affected runs
are marked in the result files and were repeated where a figure depends on them. Our runs took
decoder time from those transcodes while they overlapped (experiment 51); whether a viewer
noticed was not checked (no service, log or setting of Plex was touched).

## 55. A decoder that is behind drops to its reference pictures (12:40–12:55)

- Frame times of constant-rate inputs now come from the frame's index (its time stamp over the
  rate, rounded), not from a count of frames returned: a picture the decoder never returns
  leaves a gap instead of shifting everything after it. Gate cases unchanged (bit-exact
  same-size paths, 3:2 pattern, weaving).
- `--degrade ms`: while a decode thread is that far behind its packets it sets `skip_frame =
  AVDISCARD_NONREF` (libavcodec then never sends non-reference pictures to NVDEC) and takes it
  back below a quarter of the threshold. 26 × 1080p30 → 1080i next to two 2160p transcodes and
  a tone-mapped one in real time (`results/contend/pc-a26-degrade`, 250 ms, resync 800 ms as the
  second line): 3.0 % of output frames repeat their input (46.8 % without, 32.1 % with the
  resynchronization alone), worst frame 299 ms after its time, 0 % in the gap after, 36
  suspensions, no resynchronization; competitors 20, 20, 19 fps. Next to an unthrottled 2160p
  transcode: 0 % (competitor 42 fps).
- Over capacity for good, both options on (`results/pipe/final.jsonl`): 29 × 1080p30 → 1080i,
  20 s: every tick on time, 9.3 % of decoded frames late (worst 2.1 s), 10 % of output frames
  on the slate (a resynchronization waits for the input's next key frame, 2 s away at most with
  these inputs, and the channel's hold is 1 s). With the final binaries and nothing else on the
  card: 27 channels of (a) and 8 mixed units hold as before; `make check-pipe` 46 of 46,
  `make check` 15 of 15.
- Host at the end (12:41 PST): no process of ours, card idle, power limit 70 W, timeslice
  Default, no failed unit, no OOM line since the start.
