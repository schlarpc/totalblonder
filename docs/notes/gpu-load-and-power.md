# The encoder's load on the card, and its power (2026-11-27)

Measurements on nixark (RTX 2000 Ada, driver 595.71.05) with the server running three programs
beside every run (1 × 720p59.94, 2 × 480i: 2 % of the card), and the kernel changes they led
to. Tools: `tools/gate/README.md`. Earlier work on the same kernels was done in the CUDA spike
before the port; its conclusions that still hold are quoted where they matter.

## 1. What the questions were

Fewer pictures given up when Plex transcodes on the same card, and fewer watts.

- **Pictures are given up only near capacity.** Next to the running server (three programs),
  12 s each of a real-time and an unthrottled 1080p transcode, a real-time and an unthrottled
  2160p transcode, and unthrottled 2160p tone mapping: no picture given up. Only a synthetic
  process that keeps the compute engine busy with back-to-back 4 ms kernels cost 1.6 %.
  Contexts take turns on the compute engine, so what the encoder loses is a share of time;
  that matters once its own tick needs most of the frame period. The lever is the work per
  macroblock (§2, §3).
- **At a small line-up the watts are a floor, not work.** The server's three programs draw
  16 to 17 W at 2 % utilization; the card idles at 6.5 W without a context. The work per
  macroblock hardly shows in that. The lever is the card's clock (§4).

## 2. Where the kernels wait

`ncu_profile.sh` (4 × 1080i, 70 launches) and `ncu_lines.py`, which joins Nsight Compute's
per-instruction warp samples with the image's line table. Before the changes:

| Kernel | Share of kernel time | The line with the most samples | Its share of the kernel's samples | Sectors asked for / needed |
|---|---|---|---|---|
| `k_xf` | 32 % | the store of a coefficient | 10 % | 2.9 |
| `k_me` | 27 % | spread over the reference loads | – | – |
| `k_quant` | 14 % | the load of a coefficient | 69 % | 6.5 |
| `k_mvsmooth` | 6 % | the reference loads of the half-sample fetch | 59 % | – |
| `k_ent_pack` | 5.5 % | the load of a level | 48 % | 4.2 |
| `k_recon` | 4 % | the load of a level | 41 % | 3.2 |

`coef` and `lev` held one plane per coded position. A thread per block walks its block's mask
and reads position after position; the threads of a warp are at different positions, so each
read asked for a sector of its own.

## 3. What changed, and what it gave

Free-running macroblocks per second, `gate-bench --mode free`, builds interleaved round by
round, each against the build before it. All of them leave the coded streams byte-identical.

| Change | 1080i | 1080p | 720p | 480i | mixed |
|---|---|---|---|---|---|
| `coef`, `lev` dense per block, blocks in groups of 32, values eight by eight (`tb_kernel_abi::lev_at`); `k_xf` enumerates luma blocks two by two (32 contiguous pixels per row and warp) | +8.3 % | +9.6 % | +11.2 % | +14.9 % | +8.2 % |
| `k_xf` in blocks of two warps instead of eight (`XB` 32 → 8: the barrier between its passes) | +2.5 % | +3.3 % | +3.3 % | +2.2 % | +0.9 % |
| All of it against the start, five rounds | +11.8 % | +13.0 % | +14.1 % | +19.3 % | +9.8 % |

Of the first row, measured in steps: the layout with the values one by one +3.0 to +9.2 %,
the luma pairs +1.5 to +3.3 % on the single formats (nothing on the mixed line-up), the
values four by four another +2.0 to +3.3 %, eight by eight +0.4 to +0.9 %.

Paced, 56 × 1080i with the deadline policy, competitors for 10 s each, two runs per build
(before the `XB` change):

| | Tick alone | Unthrottled 2160p transcode: pictures given up | Unthrottled 2160p tone mapping | Unthrottled 1080p, real-time 2160p |
|---|---|---|---|---|
| Before | 22.4–23.0 ms (0.67–0.69 of the period) | 38.0 %, 39.3 % | 4.2 %, 6.4 % | 0 |
| After | 19.7–20.1 ms (0.60) | 25.6 %, 28.2 % | 0.0 %, 0.9 % | 0 |

Power at a fixed paced line-up fell by about 1 W (16 × 1080i 28.4 → 27.4 W, 40 × 1080i
51.2 → 50.2 W, the same two builds) while the busy share fell by a tenth: the watts follow the clock more than the
work (§4).

Tried and left out:

| Experiment | Result |
|---|---|
| A block's 64 values in one run (no interleaving) | +2 to +9 %: less than the interleaved layout, whose readers coalesce |
| Values one by one, four by four, sixteen by sixteen inside a group | eight is the knee: one by one 2.5 to 4 % behind it, four 0.4 to 0.9 %, sixteen 0.1 to 1.1 % |
| `k_quant` reads its coefficients four at a time (the reads under way together) | +0.2 to +0.8 %, inside the scatter |
| `k_xf`'s coefficient loops unrolled | 0 |
| `k_xf` in blocks of one warp, of four | −3 to −5 % against two (the limit of blocks per SM); four: 1 % behind two |

Still true from the spike's log: more warps per SM do not make `k_me` faster (five
experiments), per-thread early exits do not pay, and a power cap (`nvidia-smi -pl`) changes
nothing at a fixed line-up.

What is left, by the same profile: `k_xf` 34 % and `k_me` 30 % of kernel time. In `k_xf` the
reference and source loads are 17 % of its samples, the barrier 7 % before the `XB` change,
the coefficient stores 5 % (still five times the sectors: eight threads of a block store by
rank), the histogram's 64-bit shifts 9 % of its instructions. `k_me` waits for reference
loads in every stage; nothing in it stands out.

## 4. The clock

`nvidia-smi --lock-gpu-clocks=210,MAX` (root; `--reset-gpu-clocks` undoes it) caps the clock
of the compute engine. The card otherwise runs every burst of work at 2130 to 2430 MHz.

| Load | Default | Cap 1500 MHz | Cap 1000 MHz |
|---|---|---|---|
| The server's three programs | 16.9 W | 13.6 W | 13.0 W (600 MHz: 12.9 W, 300 MHz: 12.7 W) |
| 16 × 1080i paced | 27.5 W, tick 5.9 ms | 22.0 W, tick 7.5 ms | 21.6 W, tick 10.6 ms |
| 40 × 1080i paced | 48.6 W, tick 14.0 ms | 34.8 W, tick 20.8 ms | 35.8 W, tick 28.6 ms (one picture given up) |
| Unthrottled 1080p transcode alone (h264 → `scale_cuda` → `h264_nvenc`) | 898 fps | 610 fps | – |
| Unthrottled 2160p transcode alone | 404 fps | 295 fps | – |

A cap of 1500 MHz saves a fifth to a quarter of the card's draw at the same work; below it
nothing more. It costs about a third of peak throughput, for the encoder and for a transcode
alike, so a fixed cap trades capacity (and pictures given up while Plex bursts) for watts. A
cap that follows the load would keep both: lowered while `video_tick_max_microseconds` is a
small share of `video_tick_period_microseconds`, lifted when it is not. Not built.

## 5. Side effects of measuring next to the server

Nsight Compute holds the card while it profiles: each `ncu_profile.sh` run cost the running
server about 20 pictures (repeats; its programs were color bars). The paced runs at 56 ×
1080i with unthrottled competitors cost it about a thousand. Free runs and the gates cost it
none. It stepped no program down, and its counters were level again afterwards.
