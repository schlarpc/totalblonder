# CUDA MPEG-2 encoder: optimization ideas

> **History.** This list of ideas was written for the C++ spike; it is
> not kept up to date. The spike's programs were deleted in package X1 (2026-11-25,
> `docs/port/X1-notes.md`) and are in the git history at commit `e79069b`; its result files
> are frozen in `spikes/cuda-mpeg2/results/`. The living documentation: `docs/ARCHITECTURE.md`
> (the Rust encoder and how it is checked), `gate/README.md` (the gate and benchmark tools),
> `golden/README.md` (the golden vectors), `notes/post-port-worklist.md` (what is next).

Desk research on 2026-11-25 for the next round of work on `spikes/cuda-mpeg2/`. Nothing here
was run on `nixark` or on any GPU. Inputs: `notes/cuda-mpeg2-spike.md` (the spike report),
`spikes/cuda-mpeg2/src/kernels.h`, `m2venc.cu`, `entropy.h`, the result files under
`spikes/cuda-mpeg2/results/`, vendor documentation and open-source encoders.

Labels:

- **[M]** measured: in the spike (cited by result file), or here by compiling the spike
  kernels for `sm_89` on this machine with nvcc 12.9 and reading the SASS (CPU only; the
  spike uses nvcc 13.2, so register counts may differ slightly).
- **[V]** vendor claim from current vendor documentation.
- **[E]** my estimate, with the arithmetic shown.
- **[K]** general knowledge, not checked against a source in this session.

## 0. What the numbers say before any idea

Stage times at 1080i, N = 28, serial pipeline (`results/profile-stages.txt`) [M]:

| Stage | ms per tick | Share of serial tick | Share once the upload is overlapped |
|---|---|---|---|
| Upload and pad | 8.54 | 28.7 % | hidden (copy engine), except `k_pad` |
| Pyramid and motion search | 11.50 | 38.6 % | **54 %** |
| Prediction, DCT, quantization | 5.61 | 18.8 % | 26 % |
| Entropy coding | 2.49 | 8.4 % | 12 % |
| Reconstruction and pyramid | 1.43 | 4.8 % | 7 % |
| Download | 0.23 | 0.8 % | 1 % |

Three facts drive the ranking:

1. **The upload is already solved for throughput.** `--overlap` and `--on-gpu` give the same
   capacity (spike §6) [M]. Any further upload work (NVDEC, NV12, faster copies) buys host
   CPU and PCIe headroom, not channels. After overlap, motion search is over half of the
   GPU's work.
2. **B pictures are about 80 % of motion-search time.** Motion search costs 8.3 ms for a P
   tick and 14.6 ms for a B tick [M]; a 15-picture GOP has 4 P and 10 B pictures, so B
   pictures take 146 / (146 + 33) = 82 % [E].
3. **The quarter-resolution full search is only about a quarter of `k_me`.** From
   `results/range.txt` (16 × 1080i) [M]: ±11 px (25 candidates) 63.15 fps per session, ±35 px
   (289) 57.25, ±67 px (1089) 45.88. A linear fit gives 5.8 µs per candidate per frame-tick,
   so the 289-candidate level costs about 1.7 ms of a 17.5 ms tick, 10 % of the tick and about
   25 % of motion search [E]. The other 75 % is the full-pel ±1, the half-pel ring, the
   window copy, the zero-vector check and the prediction copies.

Why `k_me` is slow, from its SASS [M, compiled here]: `ptxas` reports **255 registers and a
1216-byte stack frame** (48 bytes of spills). The arrays `c[256]`, `c1[64]`, `c2[16]`,
`win[400]`, `pf[256]`, `pb[256]` live in local memory. The instruction mix is byte loads
(`LDG.E.U8` 1080 static, `LDL.U8` 616), byte packing (`PRMT` 1085), and `IABS`/`IADD3`/
`IMAD.IADD` (about 2,800). Every pixel difference costs roughly four instructions: two byte
loads, a subtract and an absolute-value-add. The 92 % L1 hit rate in the ncu table is mostly
this local-memory traffic. At 8 resident warps per SM (16 % occupancy) there are two warps
per scheduler to hide the latency of those loads.

What the hardware offers instead [M, compiled here for `sm_89`]:

| Source | SASS per 16 pixel differences |
|---|---|
| `iabs(a - b)` loop on bytes (the spike) | 32 `LDG.U8` + 16 `IABS` + 24 adds |
| `__usad(a, b, s)` on bytes | 32 `LDG.U8` + 16 `VABSDIFF.U32` (scalar; why the spike's `__usad` trial did not help) |
| `__vsadu4(a, b)` on 32-bit words | 8 `LDG` + **4 `VABSDIFF4.U8.ACC`** |
| `__dp4a(__vabsdiffu4(a, b), 0x01010101, s)` | 8 `LDG` + 4 `VABSDIFF4.U8` + 4 `IDP.4A.U8.U8` |

`VABSDIFF4` and `IDP4A` are native on compute capability 8.0–8.9 ("NVIDIA Ampere GPU and Ada
Instruction Set" table, https://docs.nvidia.com/cuda/cuda-binary-utilities/index.html) [V].
`__vsadu4` therefore does four absolute differences and the accumulate in one instruction.
Its issue rate per SM is not published [K]; measure it.

## 1. Ranked table

Gains are for 1080i at capacity with the upload overlapped (the 21.3 ms of GPU work per
28-channel tick above), unless stated. Capacity gain = old tick / new tick − 1. Ideas 1–3
overlap: together they are not the sum (see the combined estimate below the table).

| # | Idea | Mechanism | Expected gain | Effort | Risk | Source |
|---|---|---|---|---|---|---|
| 1 | Small search window around predictors | Quarter-res search ±2 (25 candidates) instead of ±8 (289), centered on the best of {0, co-located vector of the previous picture of the same type, scaled P vector}. | +10 % measured for the range cut alone (±11 px, no PSNR loss on two clips) [M]; predictors keep fast motion covered | Hours | Low; check sports/pans | `results/range.txt`; x264 `motionsearch.cl` predictor scheme |
| 2 | B-picture search seeded from the P vectors | B pictures are coded after their two anchors; the later anchor's forward vector `v` gives forward `v·d/3` and backward `−v·(3−d)/3` for a B at distance `d`. Refine ±1 full-pel + half-pel around those and the zero vector; skip the quarter-res level for B. | B search 2–3× cheaper; motion search −40–55 %; **+20–35 % capacity** [E] | 1–2 days | Medium: non-uniform motion; keep a cheap fallback (±2 quarter-res) when the seeded SAD is poor | §2.2 |
| 3 | Packed-pixel `k_me` rewrite | 32-bit words, `__vsadu4`, `__vavgu4` for half-pel, no local arrays, forward and backward in separate threads, then cooperative mapping (16 lanes per macroblock). | Per-pixel cost from ~4 instructions to ~0.5–1; motion search 2.5–4× faster; **+30–50 %** alone [E] | 3–5 days | Low (same decisions, bit-exact check against the old kernel) | SASS table above; §2.3 |
| 4 | Cheaper `k_xq` traffic | Skip `predbuf` writes in B pictures (no reconstruction follows); write a block's 64 levels only if it has a non-zero; 32-bit loads with funnel shifts for the half-pel prediction. | Transform stage −30–50 %; **+8–14 %** [E] | 1 day | Low | §2.4; ncu DRAM bytes |
| 5 | Parallel slice resolution and packing scans | Replace the one-thread-per-slice `k_ent_resolve`, `k_ent_rowsum`, `k_ent_picsum` with warp-per-slice ballot/scan; compact all sessions' output on the device and download once. | Entropy −40–60 %; **+5–7 %**, more at 480i [E] | 2 days | Low (CPU coder is the byte-exact oracle) | §2.5; Balevic 2009 |
| 6 | Fewer launches, no host round trip | One CUDA Graph per picture type, rate-control arithmetic in a kernel, one memset/one download instead of N, `cudaMemcpy2DAsync` straight into the padded planes (drop `k_pad`). | **+3–8 %**, mostly 480i (N = 160 issues ~500 API calls per tick) [E] | 2–3 days | Low | NVIDIA CUDA Graphs docs |
| 7 | Staggered GOP phase, overlapping ticks | Per-session picture type in the kernels; different sessions are I/P/B on the same tick. | Paced 720p 28 → 32 channels (the late ticks are the I ticks) [E from M]; flatter tick for 1080i | 3–4 days | Medium (per-session picture type touches every kernel) | Spike §6, §7 item 1; SPEC §8.4 |
| 8 | Feed-forward rate control from one transform | In `k_xq`, count non-zero levels at 4–6 trial quantizers (ρ-domain model) per macroblock row; pick the picture quantizer and per-row quantizers before entropy coding. | Removes most retries (0.01–0.1 per picture) and enables row-level control on a parallel coder; quality and VBV, not speed | 3–4 days | Medium (model fit per content) | He & Mitra 2002; §2.6 |
| 9 | Strict-VBV ladder | Non-linear quantizer (up to 112), coefficient truncation, forced skips, and an all-skipped P/B picture as the last resort; reserve buffer for the worst-case I picture. | Makes `strict_vbv` true; required by SPEC §8.1 | 2–3 days | Low | §2.7; H.262 Table 7-6, §7.6.6 |
| 10 | Field motion search seeded by the frame search | Split the existing SAD accumulators by row parity: every frame candidate also scores two field candidates. Add a field half-pel step. | Quality: closes part of the 0.5–0.6 dB gap to ffmpeg `+ilme` [M gap]; costs +15–25 % of the (rewritten) motion search [E] | 4–6 days (MC, MV coding, chroma) | Medium | §2.8 |
| 11 | Activity-based adaptive quantization (TM5 step 3) | `act` is already computed in `k_me`; `mquant = Q·(2·act + avg)/(act + 2·avg)`; needs `macroblock_quant` in the coder. | Subjective quality; slightly lower PSNR [K] | 2 days | Low | MPEG-2 Test Model 5, ISO/IEC JTC1/SC29/WG11 N0400 (MPEG93/457) [K: document not retrieved; formula as paraphrased in patents, e.g. US 6,463,100] |
| 12 | NVDEC input | Decode H.264/HEVC on the card, NV12 → padded planes in the existing pad kernel. | **0 channels** (overlap already hides the upload) [M]; saves the CPU decode, 0.1–0.2 core per 1080 input [K], and 2.6 GB/s of PCIe | 1–2 weeks with scaling | Medium: one NVDEC at ~903 fps 1080p H.264 [V] is about our whole line-up and is shared with Plex/Jellyfin | §2.9 |
| 13 | Rate change under a constant header | `bit_rate` = the program's ceiling, `vbv_delay` = 0xFFFF in every picture, one never-ending sequence; the statmux varies delivery below the ceiling and the encoder's buffer model follows it. | Conformant mid-stream rate change at any picture (today's stream violates H.262 §6.1.1.6) | 1 day + checker update | Low for conformance; vendor practice unconfirmed | H.262 §6.3.3, §6.3.9, C.3; A/53-4 §6.1.5; SCTE 43 §7.1.5; §2.10 |
| 14 | Lock clocks / tune power | `nvidia-smi -lgc` to a clock the card can hold at 88 °C. | 0 % mean; fewer late ticks (cold runs are 4 % faster than hot ones) [M] | Hours | Low | §3 |

The order is bang for buck. Items 1, 2, 4 and 5 are cheap and large; item 3 is the largest
single gain but costs a week; items 8–11 buy quality and compliance, not channels.

**Combined estimate [E].** Items 1–3 bring motion search from 11.5 ms to about 3–4 ms per
1080i tick (2.5–4× from packing, 1.5–2× from seeding B pictures, overlapping). Item 4 brings
the transform from 5.6 to about 3.5 ms, item 5 entropy from 2.5 to about 1.3 ms. The tick then
falls from 21.3 ms to about 10–11 ms of GPU work, so about **twice the overlapped capacity:
roughly 60–65 × 1080i or 60–70 × 720p59.94** on the card, before field prediction takes some
of it back. At that point the 2.6 GB/s per 28 channels of upload grows to about 5.6 GB/s of
the link's measured 10 GB/s, and NVDEC (one engine, item 12) could not feed it. Treat this as
the target to measure against, not a promise.

## 2. Detail for the top ideas

### 2.1 Small window around predictors (idea 1)

- Today: level 2 is a ±8 full search at quarter resolution (17 × 17 = 289 candidates of 4×4
  pixels), level 1 ±1, level 0 ±1, then eight half-pel candidates (`me_search`).
- Change: candidate centers for level 2 = {zero, co-located vector of the last picture of the
  same type (keep the previous `MbInfo` buffer), the scaled P vector for B pictures (idea 2),
  the median of the three upper neighbors from the previous picture}. Search ±2 around the
  best center (25 candidates) or ±1 around each center (9 each).
- Evidence: ±11 px gave the same PSNR as ±35 on `crowd_run` and `pedestrian_area` and 10 %
  more throughput; ±67 px cost 20 % [M]. Two clips are not sports; the temporal predictor is
  what keeps long vectors reachable over time (an object moving 40 px per frame is found once
  and then tracked).
- x264's OpenCL lookahead does the same on a GPU: it iterates a small diamond search, taking
  predictors from the previous pass's vectors of the neighbors, alternating "up and left" on
  even passes and "down and right" on odd ones, and scales vectors between pyramid levels
  (`common/opencl/motionsearch.cl`, https://github.com/mirror/x264/blob/master/common/opencl/motionsearch.cl,
  added by Steve Borho in 2013). Its authors note it does more work than the CPU search and is
  "very slightly worse in quality" because of the broken neighbor dependency
  (https://mailman.videolan.org/pipermail/x264-devel/2013-April/009969.html).
- The `f_code` can then shrink (±11 px fits `f_code` 3 for frame vectors in half-pel units),
  saving a few bits per vector.

### 2.2 B pictures from the P vectors (idea 2)

- With M = 3, the coded order is P(t), B(t−2), B(t−1). When the B pictures are searched, the
  P picture's vectors `v` (from P(t) to P(t−3)) are on the device in `d_info`.
- Seeds for a B at display distance `d` ∈ {1, 2} after the earlier anchor: forward
  `round(v·d/3)`, backward `round(−v·(3−d)/3)`, in half-pel. Read `v` at the co-located
  macroblock and at the macroblock the trajectory crosses (two candidates each).
- Search per direction: candidates {zero, the two seeds}, ±1 full-pel around the best, then
  the half-pel ring. That is about 3 + 9 + 8 = 20 evaluations of 256 pixels against today's
  289 small + 9 + 9 + 8 + 1. If the best SAD is above a threshold (for example 1.5 × the
  macroblock's intra activity), fall back to a ±2 quarter-resolution search for that
  macroblock.
- The bidirectional candidate is the average of the two winning predictions, as now.
- This is the constant-velocity assumption behind H.264 temporal direct mode [K]. MPEG-2 has
  no direct mode, so the vectors are coded explicitly; the gain is in search time only.
- Expected [E]: B search falls by 2–3×; since B pictures are 82 % of search time, motion search
  falls by 40–55 %.

### 2.3 Packed-pixel `k_me` (idea 3)

Step 1 (thread per macroblock, 1–2 days):

- Hold the current block as 64 `uint32_t` (16 rows × 4 words) in registers, `c1` as 16 words
  and `c2` as 4 words. No `uint8_t` arrays, no `win[]`, no `pf/pb[]`.
- Reference rows: the planes are `[n][h][w]` with `w` a multiple of 16, so rows are 4-byte
  aligned. For a candidate at `x`, load the words at `x & ~3` and the next one and shift with
  `__funnelshift_r(lo, hi, 8 * (x & 3))` (one `SHF`), then `__vsadu4` (one `VABSDIFF4.ACC`).
- For the level-2 window, slide along a reference row: load the 6 words that cover all 17
  horizontal candidates once, and produce each shifted word with one funnel shift. Per
  candidate row: 1 shift + 1 `VABSDIFF4` for 4 pixels, about 0.5 instruction per pixel
  against about 4 today.
- Half-pel candidates: horizontal and vertical half-pel are exact with `__vavgu4` (rounding
  `(a + b + 1) >> 1` per byte, which is H.262's two-sample rule). The four-sample diagonal rule
  `(a + b + c + d + 2) >> 2` is not exact as `vavg(vavg(a, b), vavg(c, d))` (off by at most 1);
  that is fine for ranking candidates, while `k_xq` keeps the exact prediction. `__vavgu4`
  compiles to about four instructions per word here [M], so a half-pel pixel costs about 1.5
  instructions against about 8 today.
- Optional: half-pel SAD on every second row (8 rows) [K: common in software encoders'
  sub-sampled SAD]; measure PSNR before keeping it.
- Drop the `pred[]` output: `k_xq` recomputes the prediction anyway. Compute the
  bidirectional SAD from the two chosen vectors by re-reading the references.
- Run forward and backward searches as separate threads (2 × nmb threads for B), then a
  small mode-decision kernel (or decide inside `k_xq`'s first pass). This halves the live
  state per thread.
- Target: under 64 registers (32 warps per SM). Do not force it with `-maxrregcount`; the
  spike's trial of that on the current code lost 6–57 % [M], because the arrays spill.
- Avoid 64-bit address arithmetic in inner loops: compute a 32-bit offset inside the session
  plane and add it to a base pointer once per row (the current code does `(size_t)yy * w`
  per candidate; SASS shows 96 `IADD3.X` and 40 `IMAD.X`) [M].

Step 2 (cooperative, 2–3 days): 16 lanes per macroblock, lane = picture row. Each lane holds
its current row (4 words) and loads its reference row and the rows ±1 (L1 hits). Per
candidate: 4 shifts + 4 `VABSDIFF4`, then a 4-step `__shfl_xor_sync` reduction across the 16
lanes (amortize by reducing several candidates' sums packed in one register: SAD of a 16-pixel
row is at most 4080, so two 16-bit sums fit in one word). The level-2 search maps naturally to
one lane per candidate row. Shared-memory tiles of the reference window pay off only when
neighboring macroblocks in one CUDA block share it; with ±2 windows the overlap is small, so
rely on L1 first and try shared memory second.

Notes:

- Early termination (stop summing when the partial SAD exceeds the best) diverges inside a
  warp and saves little with 16 rows; do not bother [K].
- Texture fetches give clamping for free but return one texel per fetch; with packed words
  the clamp is cheaper as a padded reference (pad the reference planes by 16–32 pixels on
  each side once after reconstruction, which also removes the `iclamp` calls in `k_xq`'s
  `mc_row`) [E].
- Integral images help mean and variance (activity), not SAD [K].
- Verification: the new kernel should choose the same vectors as the old one for the same
  candidate set (bit-exact `MbInfo` on a fixed clip), except for the diagonal half-pel
  approximation, which is tested by PSNR.

### 2.4 `k_xq` and `k_recon` traffic (idea 4)

- ncu says `k_xq` is at 83 % of "memory throughput" but its DRAM traffic is 115 MB read +
  59 MB written in 1.59 ms (8 × 1080i), 109 GB/s, about half of the 208 GB/s the card reached
  on `saxpy` [M, from `results/enc-ncu-summary.txt` and §2 of the spike]. The 83 % is
  therefore probably the L1/load-store path (byte loads in `mc_row`, scattered 16-bit stores),
  not DRAM [E]. Check with ncu's memory workload section (`l1tex__throughput`,
  `lsu` pipe utilization) before acting.
- Changes, cheapest first:
  1. `predbuf` is read only by `k_recon`, which does not run for B pictures. Do not write it
     in B pictures (384 bytes per macroblock; 2/3 of pictures).
  2. Write the 64 levels of a block only when the block has a non-zero level (an 8-lane
     `__ballot_sync` on `any` before the store). All readers already test `nz`; `k_recon`
     must treat uncoded intra blocks as before. Most non-intra blocks are zero at these rates
     [K], so most of the 768 bytes per macroblock disappear.
  3. Load prediction rows as 32-bit words and funnel-shift for the horizontal half-pel; pad
     the reference planes so the clamps go.
  4. Fuse `k_recon` into `k_xq` for I and P pictures, run speculatively with the first
     quantizer and redone only for sessions that retry (0.01–0.1 retries per picture [M]).
     This saves re-reading levels and prediction (`k_recon` moved 113 MB in 0.73 ms, 155 GB/s,
     so it is DRAM-bound [M]).
- Float versus integer DCT does not matter here: the kernel is not arithmetic-bound
  (SM throughput 30 %) [M]. Keep the float transform.

### 2.5 Entropy coding without a serial pass (idea 5)

All per-slice state in `resolve_slice` (`entropy.h`) is either a left-neighbor read or a
"last value set before me" query:

| State | Rule (H.262) | Parallel form |
|---|---|---|
| DC predictors | Reset at slice start, after a non-intra or skipped macroblock (§7.2.1) | Depends only on the macroblock to the left: if it is intra, its last Y/Cb/Cr DC; else 128 << precision |
| Skipped (P) | Zero vector, no coefficients, not first or last of the slice (§7.6.6) | Local |
| Skipped (B) | Same prediction type and vectors as the previous macroblock, which is not intra; no coefficients | Left neighbor only, because a skipped macroblock carries its left neighbor's vectors unchanged |
| `macroblock_address_increment` | Distance to the previous coded macroblock | Ballot of "coded" flags; distance = lane index − highest set bit below it |
| PMV (forward and backward separately) | Reset at slice start, after intra, after a P skipped or no-MC macroblock (§7.6.3.4) | Segmented "last writer" scan: for each macroblock, the nearest earlier one that either sets the vector or resets it |
| `quantiser_scale_code` (with AQ) | Last value set | Same scan |

- Mapping: one warp per slice (a 1080 row is 120 macroblocks, 4 per lane). Each lane scans
  its 4 macroblocks serially, then a warp-wide ballot / `__fns` or `__shfl_up_sync` scan
  carries the "last writer" across lanes. `k_ent_rowsum` (prefix sum of bits along the row)
  and `k_ent_picsum` (over 68 rows) are warp and block scans of the same shape.
- Output: compute session offsets on the device (scan of `pic_bytes`), let `k_ent_scatter`
  write into one packed buffer, and download it with one `cudaMemcpyAsync` instead of N; zero
  only the words that will be written with one kernel, not N `cudaMemsetAsync` calls.
- The pattern is the one in Balevic, "Parallel Variable-Length Encoding on GPGPUs" (Euro-Par
  2009 workshops): codeword lengths, prefix sum, then a scatter with atomics
  (https://tesla.rcub.bg.ac.rs/~taucet/coding.html). The spike already does the last two.
- Slice rules: "The first and last macroblock of a slice shall be in the same horizontal row
  of macroblocks" (H.262 §6.1.2); several slices may share a row (§6.3.16); under the
  restricted slice structure required by Main Profile (Table 8-5, §6.1.2.2) every macroblock
  must be in a slice. A/54A §5.4 recommends four to eight slices per row in intra slices for
  error resilience (a recommendation, not a rule).
- More slices per row would also parallelize the serial pass, but each slice costs a 32-bit
  start code, 5-bit quantizer, byte alignment and predictor resets: at 4 slices per row in
  1080i that is roughly 68 × 3 × 60 bits ≈ 12 kbit per picture, about 0.4 Mbit/s or 2.5 % of
  15 Mbit/s [E]. The scan is free by comparison; keep one slice per row.

### 2.6 Feed-forward rate control (idea 8)

- Problem: rows are coded in parallel, so there is no macroblock-row feedback; today a
  picture that misses by 35 % is quantized and packed again.
- Mechanism: after the DCT in `k_xq`, also quantize each coefficient at K trial quantizers
  (for example q·{0.7, 0.85, 1, 1.2, 1.45, 1.8}) and count non-zero levels per macroblock row
  (one comparison per coefficient per trial, a `__popc` of a ballot). He and Mitra's
  ρ-domain model says bits are close to linear in the fraction of non-zero coefficients:
  `R(q) ≈ θ·(1 − ρ(q))`, with θ fitted from the last coded picture of the same type ("A
  Linear Source Model and a Unified Rate Control Algorithm for DCT Video Coding", IEEE TCSVT
  12(11), Nov 2002; companion paper reporting under 5 % error estimating R(q) before coding:
  https://vision.ece.ucsb.edu/sites/vision.ece.ucsb.edu/files/publications/01ICASSPHe1.pdf).
- Use: choose the picture quantizer that meets the target (TM5 steps 1–2 for the target, as
  now), then distribute the picture's budget across rows from each row's R(q) curve, which
  replaces row feedback with a feed-forward plan that is compatible with parallel rows. The
  per-row quantizer is coded in the slice header (`quantiser_scale_code`, 5 bits), so it costs
  nothing extra. Keep the existing retry as the safety net.
- Cost: K extra compare-and-count per coefficient in a kernel that is load/store-bound; small
  [E]. A real "encode at several quantizers and pick" (full `k_xq` + bit counting per
  quantizer) would cost 30–40 % per extra trial and is not worth it.
- Statmux: `bits × quantizer` per picture is what SPEC §8.4 consumes. The same R(q) curves give
  a complexity figure for the picture just analyzed (bits at a reference quantizer), one tick
  earlier and less noisy than `bits × avg_qp` [E].

### 2.7 Strict VBV ladder (idea 9)

When the buffer model says the picture will not fit, apply in order (each step is a
parameter of `k_xq` or the coder, per session):

1. `q_scale_type = 1` (picture coding extension) to reach quantiser_scale 112 instead of 62
   (H.262 Table 7-6) [K]. This also fixes `-non_linear_quant` parity with ffmpeg.
2. Coefficient truncation: keep only the first k levels of each block in scan order (k = 16,
   8, 3, 1), and raise the dead zone. One extra compare in the quantizer.
3. Force `cbp = 0` for all non-intra macroblocks: P macroblocks with a zero vector become
   skipped, others "MC, not coded".
4. Last resort for P and B: an all-skipped picture. Each slice codes its first and last
   macroblock (zero vector, no coefficients) and skips the rest, so a 1080 picture is about
   68 slices × (4-byte start code + ~4 bytes) ≈ 0.6 kB. The decoder shows the reference
   again: a visible freeze, but conformant.
5. I pictures cannot be skipped. A DC-only I picture at 1080 is about 8,160 × 6 blocks × ~7
   bits ≈ 43 kB [E]. Keep at least that much (plus headers and the next pictures' minimum
   sizes) in the buffer before every I picture; the rate control's target clamp
   (`Tt ≤ vbv − 0.1·vbv_bits`) becomes a hard floor of this size.

The spike's stress test (720p `crowd_run` forced from 12 to 6 Mbit/s: 91 underflows) is the
regression test [M].

### 2.8 Field motion search (idea 10)

- In a frame picture, `frame_motion_type = 01` (field) gives each macroblock two 16×8
  vectors, one per field, each with `motion_vertical_field_select` choosing the reference
  field (H.262 §6.3.17.1, §7.6.3) [K].
- Free seed: a frame candidate at vertical offset `dy` (in frame lines) compares current top
  field rows with reference rows of parity `dy & 1`. Keep two accumulators in every frame SAD
  (even rows, odd rows). Then each frame candidate `(dx, dy)` also scores: top field → the
  reference field of parity `dy & 1` at offset `floor(dy/2)`; bottom field → the opposite
  parity likewise. So the full-pel ±1 step around the frame winner already evaluates 9
  candidates per (field, reference field) combination for the cost of one register [E].
- Then a field half-pel ring per field (8 × 128 pixels, vertical half-pel in field lines) for
  the best reference field of each current field. Cost: about one extra frame half-pel step.
- Decision: field if `SAD_top + SAD_bottom + λ·(extra vector bits)` is below the frame SAD.
- Coder work: field MC in `k_xq` (`mc_row` with doubled stride and field select, chroma
  likewise), vertical PMV scaling for field vectors in frame pictures (§7.6.3.1: the vertical
  predictor is halved/doubled when switching between frame and field vectors) [K], and the
  `motion_vertical_field_select` bits.
- Dual prime "shall only be used in P-pictures where there are no B-pictures between the
  predicted and reference fields or frames" (H.262 §7.6.1), so it does not apply to this GOP
  structure (M = 3). 16×8 motion compensation is a field-picture tool (§7.6.1); in frame
  pictures "field prediction" means one vector per field of the macroblock, 16 pixels × 8
  field lines each.
- Field pictures (two pictures per frame) double the header and slice count and need the
  whole pipeline per field; leave them out.
- (FFmpeg comparison: see §2.11.)

### 2.9 NVDEC as the input path (idea 12)

- RTX 2000 Ada has **one NVDEC** (5th generation) and **one NVENC** (8th generation, AV1
  capable) [V, https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new;
  PNY datasheet "1x encode, 1x decode"]. NVENC sessions are "Unrestricted" on this card (12 on
  the RTX 4060) [V, same matrix].
- NVDEC throughput, Ada at a 2160 MHz video clock, 1080p: **H.264 903 fps, HEVC 1641 fps, AV1
  1018 fps** per engine [V, NVDEC application note 13.1,
  https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvdec-application-note/index.html].
  The RTX 2000 Ada's video clock is not published [unverified]; read it with
  `nvidia-smi -q -d CLOCK`.
- Against the need: 28–34 channels of 1080 at 29.97 fps are 840–1,020 decoded frames per
  second, so one NVDEC at ~900 fps (H.264) is **fully used by today's 1080 line-up alone**, and
  Plex and Jellyfin share the same engine. With HEVC sources there is about 1.6× headroom.
  After ideas 1–5 the encoder would outrun the decoder.
- Interop: `cuvidMapVideoFrame` returns a device pointer and pitch to an NV12 surface (luma,
  then interleaved CbCr) [V, NVDEC programming guide]. Copy it into the ring with a variant of
  `k_pad` that deinterleaves CbCr; that kernel already exists in shape. Scaling (1080p30 →
  1080i, anything → 720p/480i) also has to run on the device then.
- Gain: no throughput gain over `--overlap` [M]; the gain is CPU (no software decode) and
  PCIe. Do it when the input stage is built, not for encoder speed.

### 2.10 Mid-stream rate change (idea 13)

Rules (quotes checked in the local H.262 2012 Amd. 4 pre-publication text, which matches the
2000 edition where spot-checked):

- §6.1.1.6: in repeat sequence headers all data elements except the quantizer matrices
  "shall have the same values as in the first sequence header" (and the sequence extension
  repeats unchanged).
- §6.3.3: `bit_rate` "shall be the upper bound of the rate at which the coded data is supplied
  to the input of the VBV", in both constant and variable bit rate operation; NOTE 2: "there
  is no requirement that the value of bit_rate is the actual bitrate".
- §6.3.9: if any `vbv_delay` in a sequence is 0xFFFF, all of them shall be.
- Annex C.3.2 (0xFFFF): data enters the VBV at Rmax until it is full; C.3.1 (real values):
  any R(n) ≤ Rmax is legal. D.9.11: "Constant bitrate operation is simply a special case of
  variable bitrate operation."
- H.222.0 §2.4.2.3: with `vbv_delay` = 0xFFFF (and no STD descriptor) the leak method applies;
  at High Level the transport buffer drains at `min(1.05 × bit_rate, Rmax[level])`. So the
  declared `bit_rate` must be at least the highest delivery rate the mux will use.
- ATSC A/53 Part 4 §6.1.5 and SCTE 43 §7.1.5: `vbv_delay` ≤ 45,000 "in all cases other than
  when vbv_delay has the value 0xFFFF". 0xFFFF is allowed, not required. SCTE 43's
  informative note: deployed decoders manage the buffer from PCR/PTS, not `vbv_delay`, unless
  an STD descriptor with `leak_valid` = 0 is present.
- A/54A §5.3: maximum bit rate and VBV size "are required to remain constant" within a
  sequence; changing them needs `sequence_end_code` and a new sequence header. §9.5.2: unless
  "well-constrained concatenated sequences" are produced, buffer overflow or underflow "will at
  times occur at the junction", and seamless display across sequences is only recommended
  for receivers.

Options:

| Option | Conformance | Consumer decoders | Verdict |
|---|---|---|---|
| (i) Constant header with `bit_rate` = the program's ceiling `Rmax_i` (≤ 15.99 Mbit/s at D = 0.5 s), constant `vbv_buffer_size`, `vbv_delay` = 0xFFFF in every picture; the statmux varies the mux rate `r_p ≤ Rmax_i`; the encoder's own buffer model runs at `r_p` | Legal (§6.3.3, C.3.2, A/53-4 §6.1.5, SCTE 43 §7.1.5) | One sequence that never ends; nothing for a decoder to react to | **Recommended** |
| (iii) The same header, real `vbv_delay` values computed at the varying `r_p` | Legal (C.3.1) | Same; SCTE 43 says decoders ignore `vbv_delay` without an STD descriptor | Legal, no benefit over (i); harder to check |
| (ii) `sequence_end_code` and a new sequence with the new `bit_rate` at a closed GOP | Legal | Puts a sequence junction in front of the decoder at every statmux tick; A/54A warns of buffer trouble at junctions; splicer patents describe freezes and black frames, "low-end cable set-top boxes" show macroblocking or green flashes at splices (US 5,859,660; US 8,824,855) [indirect] | Only for rare changes (new ceiling, new format) |
| Change `bit_rate` in a repeat sequence header (what the spike's `--rate2` and a reopened libav `mpeg2video` do) | **Violates §6.1.1.6** | Unknown | Do not ship |

FFmpeg's `mpeg2video` already writes option (i) when not CBR: `vbv_delay` 0xFFFF unless
`rc_min_rate == rc_max_rate`, and `bit_rate` = `(rc_max_rate + 399)/400`
(`libavcodec/mpeg12enc.c` and `mpegvideo_enc.c` at commit `bf11bea4`,
https://github.com/FFmpeg/FFmpeg/blob/bf11bea4dcb4578b53906d1f5290f5b6bcae2dc2/libavcodec/mpeg12enc.c#L359).
If `rc_max_rate` is unset it writes `bit_rate` 0x3FFFF (104.9 Mbit/s), which breaks A/53 and
SCTE 43.

Consequences for the project:

- **SPEC §8.1's plan for libav `mpeg2video` (close and reopen at a GOP boundary with a new
  rate) changes `bit_rate` in the next sequence header and breaks §6.1.1.6.** Either keep
  `rc_max_rate` at `Rmax_i` and change only the internal rate target, or reopen with
  `-seq_end 1` so that the reopen is option (ii). This affects the CPU reference, not only the
  GPU encoder.
- For the GPU encoder: write `bit_rate` = `Rmax_i`, `vbv_delay` = 0xFFFF; `set_rate` changes
  the rate-control target and the buffer model's fill rate only, effective at any picture
  (`RateChange::Seamless` becomes possible). Lower before raise (SPEC §8.5) still applies.
  With a constant `vbv_buffer_size`, SPEC §8.2's "constant D at every rate" holds only inside
  the encoder's model (buffer used = r_p × D), not in the header.
- `scripts/vbv_check.py` assumes one rate; it must take the delivery-rate schedule as input to
  check (i).
- What commercial statmux encoders write is **not confirmed**. Patents describe 0xFFFF as the
  "true VBR" marker (EP 1 372 113 B1), and an Arris statmux patent keeps a per-encoder buffer
  model (US 2014/0112385 A1), but no vendor manual was found.

### 2.11 FFmpeg reference behavior

At FFmpeg commit `bf11bea4` (`libavcodec/motion_est.c`, `motion_est_template.c`,
`mpegvideo_enc.c`, `options_table.h`; links in the paths below are under
https://github.com/FFmpeg/FFmpeg/blob/bf11bea4dcb4578b53906d1f5290f5b6bcae2dc2/libavcodec/):

- Defaults: `motion_est` = `epzs`; `dia_size` = 0 (small diamond); `me_range` = 0 (limited
  only by `f_code`); `cmp`/`subcmp`/`mbcmp` = SAD (so half-pel uses `sad_hpel_motion_search`);
  `subq` 8; `mbd` simple; `last_pred` 0; `mv0_threshold` 256; `ildctcmp` VSAD.
- EPZS (`epzs_motion_search_internal`): score (0,0) first and stop if its SAD is below
  `h²·mv0_threshold/256` (256 for a 16×16 block) with left, top and top-right all zero; else try the median and its
  ±1 neighbors, the co-located vector of the previous picture, left, top, top-right (only
  left and co-located on the first row), and if still poor the previous picture's right and
  below vectors; then the small diamond. For MPEG-2 the vector-cost predictor is the left
  vector (`pred_x = P_LEFT`), matching MPEG-2's PMV. That is the candidate set idea 1 uses,
  minus the left dependency.
- `+ildct+ilme`: frame pictures only (`av_assert0(picture_structure == PICT_FRAME)` in
  `mpeg12enc.c`), no field pictures, no dual prime. `interlaced_search()` searches each field
  of the macroblock (16 pixels × 8 field lines, doubled stride) against **both** reference
  fields (`field_select` 0 and 1) with EPZS plus sub-pel, seeded with the frame vector
  `(mx, my/2)`, adds a small penalty for the opposite field, and in B pictures searches
  forward and backward fields separately. With `mbd` simple it takes field prediction when
  its cost beats frame prediction. Field DCT is decided per macroblock by VSAD.
- One slice per macroblock row (`mb_x == 0 && mb_y != 0` starts a slice).

So ffmpeg's default spends four field searches per reference (two current fields × two
reference fields). §2.8's parity-split accumulators get the same-neighborhood candidates for
free and add one field half-pel step; the difference from ffmpeg is that ffmpeg's EPZS can wander
further from the frame vector.

Quality evidence: the spike measured 0.5–0.6 dB below ffmpeg `+ildct+ilme` at 1080i, and field
DCT alone worth 1.2 dB on `pedestrian_area` [M]. A forum post on the x262 encoder reports an
RD-based per-macroblock frame/field decision worth −29 % BD-rate on VQEG clips and field DCT
−4.4 % (https://forum.doom9.org/showthread.php?p=2033828) [unverified: forum, read through
summaries]. A "0.5–2 dB" figure for MPEG-2's interlace tools circulates without a reachable
source [unverified]. Puri, Aravind and Haskell, "Adaptive frame/field motion compensated video
coding", Signal Processing: Image Communication 1993, is the primary paper (dB values behind a
paywall).

## 3. Kernel-level guidance for the measured symptoms

- **255 registers, 16 % occupancy, instruction-bound.** Low occupancy is fine when each warp
  has independent arithmetic to issue (ILP) and few long-latency loads [K; Volkov, "Better
  performance at lower occupancy", GTC 2010]. `k_me` is the opposite case: its loads are byte
  loads from local memory with a dependent add after each. Fix the instruction count first
  (§2.3); occupancy follows from dropping the arrays.
- **One thread per macroblock versus per candidate or per row.** Thread per macroblock with
  packed words (step 1 of §2.3) already removes most instructions. Per-row lanes (step 2) buy
  latency hiding and smaller per-thread state. One thread per candidate suits the level-2
  window (all lanes share the current block, which becomes a broadcast load).
- **Shared memory versus L1.** L1 already hits 92 % for `k_me`; most of that is local memory.
  Shared memory pays only for data reused by several warps of the block (neighboring
  macroblocks' overlapping windows). On Ada, L1 and shared memory are one 128 KB array per SM;
  `cudaFuncAttributePreferredSharedMemoryCarveout` chooses the split [K].
- **Half-float or 8-bit packing.** Packing pixels 4 per word is the main win (`VABSDIFF4`).
  Half floats do not help integer SAD. Storing levels as int8 with an escape list would halve
  the level traffic but is second-order once zero blocks are not written.
- **64-bit address arithmetic.** See §2.3; use 32-bit offsets within a plane.
- **`__restrict__` and `#pragma unroll`.** Add `const __restrict__` to all read-only kernel
  pointers so loads can use the non-coherent path (`LDG.CONSTANT` in the test kernels here
  [M]); unroll the 4-word row loops fully, not the candidate loops (register pressure).
- **Launch overhead.** Each tick issues per session: one H2D copy, one memset, one D2H copy,
  plus ~15 launches and 2–4 host synchronizations per picture [M, `m2venc.cu`]. At N = 160
  that is about 500 API calls per tick. NVIDIA quotes 1.5–3× for graphs of many small kernels
  [V, https://docs.nvidia.com/dl-cuda-graph/cuda-graph-basics/quantitative-benefits.html], and
  a per-kernel overhead from 3.8 to 3.4 µs in its own example
  [V, https://developer.nvidia.com/blog/cuda-graphs]. The bigger win is removing the host
  round trip in the retry loop (idea 8 makes retries rare; conditional graph nodes, CUDA 12.4
  and later, can express "repeat for active sessions" without the host [K]).
- **Upload.** RTX 2000 Ada is PCIe 4.0 x8 [V, https://www.nvidia.com/en-us/design-visualization/rtx-2000/],
  15.75 GB/s raw per direction; the spike measured about 10 GB/s in the upload stage [M].
  Upload is already overlapped; the remaining cost on the SMs is `k_pad` (0.19 ms per launch
  at 8 sessions, three launches). `cudaMemcpy2DAsync` into the padded planes with the plane
  pitch, plus a tiny kernel for the 8 bottom rows of 1080, removes it. Write-combined host
  memory (`cudaHostAllocWriteCombined`) helps only when the CPU writes and the GPU reads the
  buffer through PCIe without a copy [K]; the decoder writes into it, so it is neutral at best.
  Raw 4:2:0 8-bit (1.5 bytes per pixel) is already the minimum without compression; NV12 is
  the same size.
- **MPS.** The encoder is one process with one context, so MPS changes nothing for it.
  Plex/Jellyfin run their own contexts; separate contexts time-slice the SMs [K]. They mostly
  use NVENC/NVDEC (separate engines) and short CUDA scaling kernels, so the expected
  interference is late ticks rather than lost capacity; measure it. Making Plex an MPS client
  is not practical.
- **Power and heat.** The card sits at 63–65 W of 70 W and 85–88 °C, clocks moving between
  2130 and 2430 MHz, and runs about 4 % slower hot than cold [M]. Lowering the power cap
  cannot raise throughput on a card that is already near its cap. Reducing instructions
  (ideas 1–3) reduces energy per macroblock, which is the real lever. For predictable ticks,
  locking the graphics clock to what it holds hot (`nvidia-smi -lgc 2100,2100`, root) trades
  the 4 % cold bonus for fewer late ticks [K]. Check airflow in the chassis: a 70 W
  low-profile card at 88 °C has little margin [K].

## 4. Fixed-function hardware for motion search: verdict

### 4.1 Optical Flow Accelerator (NVOFA)

Facts [V unless marked]:

- Present on Turing (except TU117), Ampere and Ada (NVOFA application note,
  https://docs.nvidia.com/video-technologies/optical-flow-sdk/nvofa-application-note/index.html).
  No NVIDIA document lists it per chip; the RTX 4060 (AD107) supports DLSS 3 frame generation,
  which runs on the optical flow accelerator
  (https://www.nvidia.com/en-us/geforce/graphics-cards/40-series/rtx-4060-4060ti/), so it is
  present on AD107 and very likely on RTX 2000 Ada [unverified for this board].
- Output grids 4×4, 2×2, 1×1 on Ampere and Ada; hardware cost output; ROI; external hints;
  vectors are `int16` S10.5 (1/32 pel storage), with quarter-pel accuracy claimed
  (https://developer.nvidia.com/blog/an-introduction-to-the-nvidia-optical-flow-sdk/).
- Throughput, RTX 4090 at a 2114 MHz video clock, 1080p, SDK 5.0: 4×4 grid **536 / 1000 / 1296
  fps** (slow / medium / fast); 2×2 210 / 336 / 711; 1×1 98 / 113 / 222. No 4K figures. The note
  says throughput scales with the video clock and depends on memory bandwidth; RTX 2000 Ada has
  224 GB/s against the 4090's 1008 GB/s. No figure for AD107 exists and no third-party
  benchmark was found.
- API: CUDA buffers allocated by the SDK (`NvOFCreateGPUBufferCuda`, CUarray or CUdeviceptr);
  there is no call to wrap an existing device pointer, so every frame costs a device-to-device
  copy in. Inputs grayscale 8-bit, NV12 or ABGR. No fixed session limit.
- Runtime `libnvidia-opticalflow.so` ships with the driver (listed in the 595.112.07 driver
  README,
  https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/installedcomponents.html);
  nixpkgs' `nvidia_x11` builder copies all `*.so.*` from the installer, so it should be in the
  NixOS driver package [derived, not checked in the store]. nixpkgs has
  `nvidia-optical-flow-sdk` (headers only, version 2.0); SDK 5.0 is behind a developer login,
  license "free to use, including commercial" (https://developer.nvidia.com/opticalflow/download).
- Separate from NVENC on Ampere and later (NVIDIA forum answer,
  https://forums.developer.nvidia.com/t/simultaneous-support-for-optical-flow-accelerator-and-nvenc/196464);
  Ada assumed the same [derived].

Need: flows per second = channels × frame rate × (P + 2·B)/GOP = 1.6 references per frame on
average for GOP 15, M = 3. 28 × 1080i at 29.97: **1,343 1080-frame flows/s** (or 2,690 field
flows of 1920×540 if done per field). 32 × 720p59.94: 3,069 flows of 0.44 × 1080p, about
**1,350 1080-equivalents/s**. After ideas 1–5 the need doubles.

Verdict: the best published number (4×4, fast preset, on an RTX 4090 with 4.5× the memory
bandwidth) is 1,296 fps, just under today's need and half of the post-optimization need, on a
lower-quality preset, without per-field flows, with a device copy per frame and a 4×4 → 16×16
reduction and half-pel refinement still in CUDA. The 1,000 fps "medium" figure is 74 % of
today's need. **NVOFA cannot replace the motion search for the whole line-up.** It could seed
long vectors for a subset (for example, hints for sports channels), but idea 1's temporal
predictors do that at near-zero cost. Worth a one-hour measurement with the SDK's
`AppOFCuda` sample on the card when it is free (fps at 1080p, 4×4 fast/medium) before closing
the question; do not plan on it.

### 4.2 NVENC motion-estimation-only mode

- `NvEncRunMotionEstimationOnly` (H.264 and HEVC; no AV1) returns per-macroblock
  `NV_ENC_H264_MV_DATA`: up to 4 vectors (16×16, 16×8, 8×16 or 8×8 partition) in quarter-pel
  units and a `mbCost`; HEVC returns per-CU data without cost. Output can go to video memory
  (H.264 only). Supported on Ada in SDK 13.1 [V, nvEncodeAPI.h in
  https://github.com/FFmpeg/nv-codec-headers and the NVENC application note 13.1,
  https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvenc-application-note/index.html].
- Throughput: no figure for ME-only exists [unverified]. Full H.264 encode on one Ada NVENC
  at 1080p (RTX 4090, 2415 MHz) is 910 fps at P1 and 652 at P3 [V]; ME-only is presumably
  faster but unmeasured.
- RTX 2000 Ada has one NVENC, shared with Plex/Jellyfin transcodes.
- Mapping: 16×16 vectors map directly to MPEG-2 frame vectors after rounding quarter-pel to
  half-pel (then a half-pel re-check); 16×8 partitions are frame-line partitions, not field
  vectors, so they do not give MPEG-2 field prediction.

Verdict: one shared engine whose full encode rate (~900 fps, P1) is below today's 1,343
flows/s; reject. Neither block helps throughput as much as idea 3, which runs on the SMs that
are already there.

## 5. Ideas considered and rejected

| Idea | Why not |
|---|---|
| NVOFA as the motion search | §4.1: best published rate (1,296 fps 1080p, 4×4, fast, RTX 4090) is below today's 1,343 flows/s need; AD107 numbers unknown; copy-in per frame |
| NVENC ME-only | §4.2: one engine shared with Plex; ~900 fps class; no field vectors |
| `-maxrregcount` on the current `k_me` | Measured 6–57 % slower [M]; the local arrays spill |
| Integer / 16-bit fixed-point DCT for speed | `k_xq` is at 30 % SM throughput; arithmetic is not the limit [M] |
| Matching a decoder's integer IDCT | Drift is already 0.03 MSE at 15 pictures [M]; TV decoders' IDCTs vary, so no single target exists; H.262 Annex A's IEEE 1180 accuracy is a decoder requirement [K] |
| More slices per row for parallelism | ~2.5 % of the bit rate at 4 slices per row [E]; a warp scan does the same for free |
| Full multi-quantizer encode ("encode at several q and pick") | Each trial costs a `k_xq` + bit count, 30–40 % of the tick; the ρ-domain count gets most of the benefit for a few compares |
| Field pictures | Twice the headers and pipeline passes; frame pictures with field prediction get most of the gain [K] |
| Dual prime | Only in P pictures with no B pictures between them and their reference (H.262 §7.6.1); not our GOP |
| Texture fetches for SAD | One texel per fetch; packed words through L1 do four pixels per load |
| Early termination in SAD | Warp divergence, 16-row blocks; little to save [K] |
| Integral images | Help activity/variance, not SAD [K] |
| MPS | One encoder process already; Plex cannot be an MPS client in practice |
| Several encoder processes | Measured: 2–8 processes change throughput by at most 2 % and raise host CPU [M] |
| Lowering the power cap for throughput | Already at 63–65 of 70 W; a lower cap lowers clocks [K] |
| Zero-copy (mapped pinned) input | Same PCIe bytes; kernels stall on PCIe latency instead of the copy engine [K] |
| Larger search range | ±67 px cost 20 % for no PSNR gain on these clips [M] |

## 6. Side findings

- The spike's 480i runs use 720×480. ATSC A/53 Part 4 Table 6.2 allows 704 or 640 wide at 480
  lines, not 720 (`notes/standards-digest.md` §5.1). Use 704×480 for over-the-air; it is 2 %
  fewer macroblocks.

## 7. Unverified claims

Listed here so nobody builds on them unchecked:

- NVOFA presence on RTX 2000 Ada specifically (inferred from AD107 / RTX 4060 DLSS 3 support);
  its throughput on AD107; whether `libnvidia-opticalflow.so` is in the NixOS 595 driver
  output.
- NVENC ME-only throughput; whether ME-only sessions count against any session cap.
- The RTX 2000 Ada's video clock (NVDEC/NVENC throughput scales with it) and MPEG-2 decode
  rate.
- `VABSDIFF4` issue rate per SM on Ada (native per the SASS table; rate not published).
- All [E] gains in §1; they rest on instruction counting and the spike's stage profile.
- That most non-intra blocks are all-zero at these rates (drives idea 4's write saving).
- Conditional graph nodes' availability in CUDA 13.2 (introduced in the 12.x series [K]).
- What commercial statmux MPEG-2 encoders write in `bit_rate` and `vbv_delay`; which consumer
  ATSC/QAM decoders misbehave at a mid-stream `sequence_end_code` (only indirect evidence).
- Published dB gains of MPEG-2 field prediction and dual prime (forum figures and a
  paywalled paper only); the field-search gain in §2.8 must be measured.
- The [K] items in §2.3 and §3 (sub-sampled half-pel SAD quality, early termination,
  write-combined memory, MPS/time-slicing behavior, clock locking).
