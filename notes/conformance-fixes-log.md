# Conformance fixes: running log

Working log of the package that fixes the conformance gaps and the ported bugs of
`notes/post-port-worklist.md`. Newest entries at the end of each section. Started 2026-11-25.

## How things are run

- Worktree `~/totalblonder-wt/conformance`, branch `fix/conformance`, started from
  `a76ae7c`.
- Local: `nix develop -c cargo test --workspace` (the dev shell carries the kernel builds),
  `nix flake check` before a commit that changes behavior.
- GPU host: `NIXARK_DIR=totalblonder-rs-fix scripts/nixark.sh CMD` from this worktree; tools
  built here and copied with `nix copy --no-check-sigs --to ssh://nixark`.
- nixark at the start: 19 GB available, 63 GB free on /, card idle, 70 W, compute mode Default,
  no failed unit.

## Decisions

- **Profiles** (`tb_m2v_syntax::profile::Profile`): `h262` (H.262 Main Profile limits only),
  `a53` (A/53 Part 4, 19.4 Mbit/s), `scte43-64qam` (27.0 Mbit/s), `scte43-256qam` (38.8 Mbit/s,
  the default). The driver option is `--conformance NAME` (`--profile` was taken by the stage
  profiler of the benchmark).
- **Cable-only sizes stay refused** (1440x1080; 720, 544, 528, 352 x 480). SCTE 43 Table 3
  (p.3) lists them, but SCTE 54-2024 §7.4 (p.14) says an MPEG-2 video stream on cable "shall
  comply with" A/53 Part 4, whose Table 6.2 (p.9) does not; SCTE 54-2024 does not cite SCTE 43
  at all. A stream of such a size cannot satisfy both texts, so by the letter only the
  intersection is safe: the three broadcast profiles admit exactly A/53 Part 4 Table 6.2 and
  differ in the rate ceiling. The refusal names the reason.
- **Level**: the lowest of Main, High-1440, High that fits (Tables 8-8, 8-11, 8-12). Low level
  is never signaled: no format of the broadcast tables fits it, and under `h262` the small
  test pictures are labeled Main (legal: a level is an upper bound).
- **Fixed quantizer** is a legal constrained mode, not a test switch: it declares a rate and a
  buffer inside the profile, the buffer model runs for it, the quantizer stands while the
  picture fits and the ladder is the backstop. The gate's fixed-quantizer case (quantizer 6 on
  the golden clip) is byte-identical; quantizer 1 on noise is now a whole, legal stream.
- **No sequence header on a picture that is not an I picture.** H.262 §6.1.1.6 (p.13) allows
  one before a P picture, but its purpose is random access ("in order that random access into
  the video sequence is possible"), and the P pictures in question are all-skipped: the host's
  repeats (hold, shed) and the P picture that replaces an I picture on the last ladder rung.
  A decoder that starts on one has no reference: per the same clause "the reconstructed
  picture may not be fully defined", and it stays undefined until the next I picture because
  nothing in a skipped picture refreshes it. So the I-to-P replacement loses the sequence
  header it had kept from the C++, and repeats carry none. Consequence: during a hold the
  interval between sequence headers is the length of the hold plus one frame (the first frame
  after a hold is a make-up key frame, `rekey`), which exceeds the 0.5 s that SCTE 54-2024
  §9.2 (p.25, informative: "may repeat sequence_header information frequently, for example
  once every 0.5 seconds") suggests; the verifier notes it (`BC-SEQ-INTERVAL`, S4). A hold
  that must stay acquirable needs a real I picture of the held frame at the key interval,
  which costs device time; not built.
- **sequence_end_code**: both drivers (`tb-m2venc`, `tb-pipe`) already write it after the
  last access unit of every stream (the audit's "only the test driver" predates `tb-pipe`'s
  writer); nothing to change.
- **Caption bytes are refused, not repaired**, when they are not a legal `cc_data()`
  (`tb_m2v_syntax::captions::validate`, called by `Encoder::submit`). With the marker bits
  checked, no escaping is needed against start code emulation: the longest run of zero bits in
  the user data is 19.
- **Verifier correction** (`CC-608-FIELD2`): it reported valid field 2 data without *valid*
  field 1 data. CTA-708-E §4.3.2.2.1 (p.14) requires "a cc_data() structure for each field,
  even if a decoder is being instructed not to generate the run-in clock and start bit on one
  field (by setting cc_valid to 0 for that field type)", and §4.3.2.2 (p.13) says what the
  decoder does with field 2 data beside a cc_valid 0 field 1 pair. The rule now reports a
  field without any construct, in both directions, with the clause's exception above 30/1.001
  Hz. Tests: `tests/rules.rs`, three cases.
- **drop_frame_flag**: 1 at 29.97 Hz only. H.262 §6.3.8 (p.46): "It may be set to '1' only if
  the frame rate is 29.97 Hz"; at 59.94 Hz the count is 60 per second with the flag 0.

## Items

### Items 1 and 2 (commit `9e60b48`)

- `crates/tb-m2v-syntax/src/profile.rs`: `check(SeqConfig, Profile) -> SeqParams`; `Level`;
  `frame_rate_code`; `time_code_base`; `Color`. `headers.rs`: `SeqInfo` holds `SeqParams`;
  `TimeCode::of` (drop-frame at 29.97 Hz).
- `Encoder::open` calls the check first; `LayoutError::SearchRange` refuses a range above 30
  (19 with field prediction).
- Golden vectors regenerated: every stream digest changed (drop_frame_flag in every GOP header
  at 29.97 Hz). On the card the 720p streams (59.94 Hz, flag 0) must stay byte-identical.
- `nix flake check` green.

### Item 3: macroblock size rule (H.262 Table 8-6)

- Kernels: `k_ent_pack` marks a macroblock above 4608 bits (without the 38 bits of a slice
  header) in bit 15 of `mbtex`; `k_ent_rowsum` counts them (high byte of the texture sum's
  word in the warp reduction) and sets bit 31 of `slice_tex` for a row with more than two;
  `k_ent_picsum` collects the rows in `PicStat::big_rows` (PicStat grows from 8 to 24 bytes).
  No new buffer, no new launch, no new download.
- Rate control (`after_pass`): a pass with a big row is never accepted; the row's quantizer
  code gets a floor at the next code that doubles quantiser_scale (`coarser`), which lasts for
  the picture; a row at code 31 enters the ladder (rung 32 keeps 12 coefficients per block:
  at most 72 x 24 = 1728 bits of coefficients per macroblock). Counter `Stats::mbfix`
  (`mb_limit_passes` in the driver's JSON).
- Proof: `crates/tb-m2venc/tests/conformance.rs` (white noise at 15 Mbit/s on 176x144, and a
  noise band in an easy picture at 8 Mbit/s: `mbfix > 0`, the verifier finds nothing), the
  rate-control unit tests, `ref_cases.sh` cases `mb-white`, `mb-patch`, `mb-patch-i`.
- Streams of the golden cases: unchanged (the rule is never reached there); records change
  (`pass.big_rows`, the size of `h_pic` / `d_pic`).

### Item 4: fixed quantizer, no cut (commit `2f14f57`)

- `RateControl`: the buffer model runs for both modes; `plan_picture` gives a fixed-quantizer
  picture the buffer's room as its limit; `after_pass` makes every row coarser by doubling
  steps when the picture does not fit, then the ladder. `first_min` is a real bound now
  (`first_picture_bits`): the first-picture violation is refused at open instead of counted.
- `collect`: no `.min(cap)` on the slices; a picture larger than the output buffer (not
  reachable) is given up, never cut.
- A rate change on a fixed-quantizer session is applied.
- `nix flake check` green (run on the tree of items 3 and 4).

### Item 5: progressive_frame implies frame_pred_frame_dct

- `SessPar::frame_only` and `SessPar::zigzag` (the two padding bytes). Kernels: `k_mbstat`
  (no field DCT decision, activity from the four frame blocks), `k_me` (no field search),
  `k_xf` / `k_quant` / `k_recon` (scan of the picture), `encode_mb` (no frame_motion_type and
  dct_type bits). Host: the picture header says frame_pred_frame_dct 1 and alternate_scan 0
  (unless the session asked for the alternate scan), the skip slices of a repeat follow.
- `write_headers` writes progressive_frame 1 in an interlaced sequence only with
  frame_pred_frame_dct 1.
- Test switch `tb-m2venc --test-progressive P` (device input); end-to-end test with all,
  none and mixed pictures.

### Item 9: the ported bugs

- Same-code correction: the pass's row codes, `frac` and `nzpred` are restored when the
  correction is not made (test fails without the fix).
- Rate change on a fixed-quantizer session: with item 4.
- Sequence header on the P picture that replaces an I picture: dropped (decision above); the
  golden stream `skip` changes.
- Level-2 motion search: `ME_MAX_RANGE2` = 30 in the ABI; the host refuses more, the kernel
  clamps, and a constant assertion ties the loads to the guard bytes.
- Time code: with item 1. Slice numbering above 2800 lines: such sizes are refused (item 2).
- Texture bit count around End of Block: see the measurement below.

### Item 10: depth term of the timeline's skew (tb-timeline)

- `MapperConfig::depth_tau` (10 minutes) and `depth_max_ppm` (50): the map's skew is the
  estimate plus `(J − depth) / depth_tau`, the depth measured on the fastest frame of the
  newest skew bucket (`SkewEstimator::floor`), integer arithmetic, clamped with the estimate
  to `skew_max_ppm`.
- 30-day simulations (`tests/long_run.rs`, release, `--include-ignored`): full rate (30.000
  fps 37 ppm slow into 29.97, 77.7 million ticks) depth of the last day J − 4.98 ms (arrival
  jitter) to J + 0.13 ms, was a walk of 36 to 41 ms; scaled run J − 4.3 ms to J + 2.7 ms with
  the term, J + 59 to J + 67 ms without (`depth_tau` zero), both asserted.

### Session interrupted at 21:25, resumed 22:23

- State at the resume: items 1 to 4 committed; items 5 and 9 staged and tested by cargo (412
  tests) but the flake check interrupted; items 7 and 8 and the verifier correction written
  and tested together with them (418 tests) and set aside in the scratch directory
  (`full/tree.tar`: the files of the complete tree) so that 5 and 9 could be committed
  first; item 10 in the working tree. GPU: gates of commit `9e60b48` done (all pass; 235 of
  308 reference streams differ: every 29.97 Hz stream, by the drop-frame time code; the 73
  at 59.94 Hz and the four `.ref` streams identical); runs for `af620d6` and `2f14f57` cut.

## On the card (nixark), 2026-11-26 UTC

Tools built on bagel and copied; the tree of each commit put into `~/totalblonder-rs-fix`
(only that directory; `git archive` into a new directory, then renamed), everything inside
`scripts/capped.sh` (10 GB). Scratch: `~/cf-work/<label>/`, scripts `~/cf-*.sh`.

### Gates per commit, and which streams changed

Every run: `gate-check` 54 / 54, `gate-pipe` 46 / 46, `gate-small --device cuda --golden`
10 / 10, `gate-golden` 20 files the same as the reference adapter's (`deadline` not compared, by
design). 325 streams per run (308 of them in `docs/port/X1-reference-streams.sha256`).

| Commit | Items | Streams that differ from the run before |
|---|---|---|
| `9e60b48` | 1, 2 (and 3, 6, 8 of the gaps) | 235 of 308 against the X1 reference: every 29.97 Hz stream (drop_frame_flag and the dropped picture numbers in each GOP header). The 73 others are byte-identical: all 69 streams at 59.94 Hz (720p, churn and mixed-format 720p sessions) and the four `.ref` streams |
| `af620d6` | 3 (macroblock size rule; kernel change) | none |
| `2f14f57` | 4 (fixed quantizer, no cut, first-picture bound) | none |
| `8af85af` | 5, 9 | 11: the eight pipeline streams with progressive sources in interlaced sessions (`pipe-1080p24-1080i-pulldown`, `pipe-1080p30-1080i-psf`, `pipe-1080p-480i-crop`, `pipe-1080p-480i-letterbox`, `pipe-720p50-1080i`, `pipe-854x480-1080i`, `pipe-av1-1080i`, `pipe-mosaic-2x2-overlay`: frame-only coding); `720p-still` and `1080i-still` (the same-code correction); `skip` of the small gate (no sequence header on I replaced by P) |
| `1557e03` | 7, 8, verifier correction | 2: `1080i-pedestrian_area-cc` and `interlaced` of the small gate (order of the two CEA-608 constructs). Color: none |

(The runs of the commits before item 5 used an allow file with the old `BC-PROGRESSIVE-FPFD`
line, since their trees still had that finding; from `8af85af` on nothing is allowed.)
`gpu-plumbing` at the end: 12 / 12, the float parity test among them.

### Verifier on the final run

298 streams of `gate-check` and `gate-pipe` under `scte43-256qam` (the `devin-crop` streams
under `h262-only`), plus the small gate's under `h262-only`: no S1, S2 or S3 finding, nothing
allowed. Notes (S4): `BC-COLOR-PREFERRED` on the 112 SD streams (SMPTE 170M, which is what
the converter produces; SCTE 43 prefers BT.709) and `BC-SEQ-INTERVAL` on the 6 hold and shed
streams (decision above). `CC-608-ORDER` is gone.

### Quality

Against `gate/baselines/baseline.json`, 53 cases with a figure: items 1 to 4 reproduce the
baseline exactly (largest difference 0.00005 dB and 0.00005 SSIM, the rounding of the stored
values). Items 5 and 9 change two cases, both from the same-code correction fix; the baseline
is updated for them:

| Case | Mbit/s | PSNR-Y dB | PSNR-U | PSNR-V | SSIM |
|---|---|---|---|---|---|
| `720p-still` before | 9.8835 | 33.8213 | 38.4004 | 38.7304 | 0.9504 |
| `720p-still` after | 9.9119 | 33.9087 | 38.5717 | 38.8915 | 0.9523 |
| `1080i-still` before | 12.4001 | 32.4148 | 36.9302 | 37.3161 | 0.9261 |
| `1080i-still` after | 12.4097 | 32.4259 | 36.9504 | 37.3348 | 0.9265 |

Frame-only coding (item 5) is not measured by a gate case (the pipeline gate's figures are of
the conversion, before the encoder). Measured with `~/cf-prog.sh`: progressive 1080p clips in
a 1080i session, device input, 150 frames, reconstruction against the source, pictures not
flagged (field tools, as before) and flagged (frame-only):

| Clip, rate | PSNR-Y field tools | frame-only | SSIM field tools | frame-only |
|---|---|---|---|---|
| crowd_run 15 Mbit/s | 28.288 | 28.669 | 0.8123 | 0.8222 |
| crowd_run 8 | 25.549 | 25.894 | 0.7281 | 0.7417 |
| pedestrian_area 15 | 39.761 | 40.125 | 0.9537 | 0.9565 |
| pedestrian_area 8 | 35.731 | 36.519 | 0.9148 | 0.9237 |
| cuts 15 | 31.000 | 31.341 | 0.8928 | 0.8983 |
| cuts 8 | 28.084 | 28.414 | 0.8270 | 0.8378 |

Same rate in each pair (15.22 / 8.12 Mbit/s): 0.33 to 0.79 dB better, no case worse.

### Capacity

`gate-bench --mode free --rounds 3 --seconds 5`, builds interleaved, `nvidia-smi pmon`
alongside (no other process on the card, 60 runs, none disturbed). Final encoder against the
encoder of `a76ae7c`, throughput ratio of the means (range over rounds):

| Format | new / old |
|---|---|
| 1080i | 0.9973 (0.9961 to 0.9988) |
| 1080p | 0.9967 (0.9934 to 1.0016) |
| 720p | 0.9880 (0.9647 to 1.0182) |
| 480i | 0.9969 (0.9911 to 1.0066) |
| mixed | 0.9980 (0.9869 to 1.0075) |

Within the spread of the runs (0.1 to 1.5 % within a build); if the 0.3 % is real it is the
marks and counts of the macroblock size rule in the entropy kernels. Frame-only pictures are
cheaper than pictures with field tools (same build, device input, flagged against not
flagged): 1080i 1.111, 480i 1.079, mixed 1.074 (no field search, no field DCT decision).

### Texture bits around End of Block (the last ported inconsistency)

`encode_mb` counts End of Block as texture for non-intra blocks and not for intra blocks.
Three builds through `gate-check` (54 cases): as it is (A), End of Block never counted (B),
always counted (C).

| Build | Cases failing the quality baseline | Mean PSNR-Y | Mean SSIM | Mean abs. rate error | Extra passes per picture | Ladder pictures |
|---|---|---|---|---|---|---|
| A (kept) | 0 | 26.9450 | 0.74491 | 5.407 % | 0.1184 | 1588 |
| B | 3 (`720p-cuts-quarter` −0.25 dB, `480i-cuts-quarter` −0.73 dB, `1080i-cuts-ratestep` −0.12 dB) | 26.9547 | 0.74529 | 5.382 % | 0.1271 | 1601 |
| C | 5 | 26.9258 | 0.74449 | 5.439 % | 0.1214 | 1614 |

Neither uniform rule is better: B gains 0.01 dB on the mean and loses up to 0.73 dB on the
starved cut cases with more passes; C is worse throughout. The code stays; its comment now
says what it does.

### Last commit on the card (`fcd9425`), and the end state

The comment in `kernels/entropy.h` changes the device image's bytes (not its code), so the
gates ran once more on the last commit: `gate-check` 54 / 54, `gate-pipe` 46 / 46,
`gate-small` on the card 10 / 10, `gate-golden` 20 the same, `gpu-plumbing` 12 / 12; all 325
streams byte-identical to the run of `1557e03`. (The capacity runs and the End of Block
builds were made on `1557e03`.)

nixark at the end (07:19 UTC): no failed unit (system and user), no out-of-memory line in the
kernel log, power limit 70 W, compute mode Default, no process on the card, 19 GB available,
57 GB free on /. Kernel log: NVRM Xid 31 at 06:53 and 07:19 UTC, process `device_fault_is…`:
the CUDA adapter's test that provokes a fault on purpose (the earlier ones are X1's and G2's).
Left there: `~/totalblonder-rs-fix` (the tree), `~/cf-work` (logs, digests and results; the
streams removed), `~/cf-*.sh`. Nothing of earlier packages was deleted.
No incident. One departure from the brief's wording: the tree was put on nixark with a script
of mine (`git archive` of the commit under test, unpacked into `~/totalblonder-rs-fix`, a
name fixed in the script) rather than `scripts/nixark.sh`, because each commit's own tree was
needed there; no `rsync --delete` was run anywhere.
