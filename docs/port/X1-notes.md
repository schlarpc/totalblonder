# X1 running notes: retiring the C++ host

Working log of the package that deletes the C++ oracle now that the Rust binaries have parity.
Newest entries at the end of each section. Started 2026-11-25 (PST).

## How things are run

- Worktree `~/totalblonder-wt/X1`, branch `port/X1`, started from `e79069b`
  (merge of `port/G2`). GPU host `nixark`: `NIXARK_DIR=totalblonder-rs-x1 scripts/nixark.sh CMD`;
  tools built on bagel and copied with `nix copy --no-check-sigs --to ssh://nixark`.
- Removed on nixark for space (allowed by the brief): `~/totalblonder-rs-g1`,
  `~/totalblonder-rs-k3`, `~/totalblonder-rs-k3-base`, `~/totalblonder-rs-g2-base`. 63 GB free
  afterwards. The media corpus stays in `~/totalblonder-spike/media`.
- Scratch on nixark: `~/x1-work/<label>/` (gate outputs, logs), `~/x1-ref.sh` (the reference
  run: `check-full`, `check-pipe`, `gate-small --device cuda --golden`, then `sha256sum` of every
  `*.m2v` written).

## Reference (before any change)

- Commit `e79069b`; gate tools `/nix/store/p5yxdzj0z23wxbiafvxj7ab2fbn52hgj-gate-tools` (the G2
  final tools, unchanged by the merge).
- Device image `tb-kernels-sm89` (`/nix/store/gac8bpcaj9s01s3yvs0a7zd25ky0b5kq-…`):
  `tb_kernels.cubin` SHA-256 `8138f6c0a74f7c4d6d37228789e35259ec7e4940411472d505088ee19cfb1720`,
  `tb_kernels.sym` `3baf4286fb5f3aae773b2a823f2e6418d0c3d20f5d1a0e5e014c897ada42bca1`.
- On the card, 2026-11-26 02:25-02:29 UTC, card idle by `nvidia-smi pmon`, 18 GB available:
  `check-full` 54 / 54 (92 s), `check-pipe` 46 / 46, `gate-small --device cuda --golden` 5 / 5.
  308 coded streams (74 + 228 + 6); their SHA-256 are `docs/port/X1-reference-streams.sha256`
  (paths relative to the run's scratch directory).
- The same run repeated at once (`before2`): all 308 digests identical. The gates are
  deterministic on the card, so "every stream hash unchanged" is a usable acceptance test.

## Verifier in the gate (commit `dec21d9`)

- `conform.py` (gate machinery): every stream a case writes, `tb-m2v-verify --json --delivery
  operating`; profile `scte43-256qam` for the broadcast gates (`check-full`, `check-pipe`,
  `gate/pipe-ref-small` whose output is 480i), `h262-only` for the 176x144 cases of `gate-small`.
  S1/S2 fail; S3 fails unless `crates/tb-m2v-verify/tools/gate-allow.txt` allows the rule for the
  stream (globs on file names, with the worklist item); S4 notes are listed only.
- Survey of the reference streams (before the hook): under `scte43-256qam` the only S1-S3 findings
  of our 304 streams are `BC-PROGRESSIVE-FPFD` (8 pipeline streams), `BC-FORMAT` (8 devin-crop
  streams, 696x470; 6 small 176x144), `BC-LEVEL-LOWEST` (5 small); under `h262-only` none. The
  four `*.ref.m2v` (ffmpeg's reference encodes) are not ours and are skipped.
- New finding of the hook: the slate of `gate/pipe-ref-small` carries `BC-PROGRESSIVE-FPFD`
  (item 7 again); allowed as `slate-*`.
- On the card with the hook (tools `mylv9kk5…`): `check-full` 54 / 54 (70 streams verified, no
  allowed finding), `check-pipe` 46 / 46 (228 verified; both allow entries matched), `gate-small`
  5 / 5; all 308 digests equal to the reference.

## Golden hand-over

The recorder (commit `5fb7ab6`): the encoder writes every record of `golden/README.md` itself
(rate control through an observer, plan steps merged with the records at the position they were
written, `%.17g`). `tb-pipe --golden-taps` writes `taps.jsonl` from `tb-convert`.

Proof, 2026-11-25, before any C++ was deleted. Sources: the committed oracle files (`e79069b`);
the oracle re-run now on the card (`nix build .#m2venc-oracle`, the old `generate.sh` cases, two
runs); the Rust recorder on the reference adapter (bagel) and on the card (nixark). Records
compared after dropping the three measured kinds (`tick_end`, `shed_hist`, `shed_budget`: wall
clock in the C++); those were compared separately with their timing fields removed: same count,
same ticks and macroblocks.

| Case | Records (no measured) committed / oracle now / Rust ref / Rust card | committed = oracle now | oracle run a = b | oracle vs Rust ref | Rust ref vs Rust card |
|---|---|---|---|---|---|
| cbr | 1438 / 1438 / 1440 / 1440 | yes | yes | 3 differences (below) | only `golden.args` |
| ladder | 1345 / 1345 / 1347 / 1347 | yes | yes | the same 3 | only `golden.args` |
| fixedq | 586 / 586 / 588 / 588 | yes | yes | the same 3 | only `golden.args` |
| interlaced | 1411 / 1411 / 1413 / 1413 | yes | yes | the same 3 | only `golden.args` |
| shed | 1538 / 1538 / 1540 / 1540 | yes | yes | the same 3 | only `golden.args` |

The three differences, all from the oracle's golden runs not using its arena (the port has only
the arena path, ARCHITECTURE §4.1), plus the command line:

1. `batch.arena` 1 (oracle 0) and `batch.dev_bytes` the arena's rounded size (oracle: sum of
   unrounded allocations; `BatchLayout::dev_bytes_unrounded` reproduces that, the layout test
   compared it).
2. Two `op` records more, before the first wait on `main` at tick -1: `fill ablk_d` (the batch's
   block) and `upload d_T <- e->T` (the tables). The oracle run with `--arena 64` (cbr) writes the
   same two records with the same sizes (803584, 11596), only earlier: before its `batch` and
   `open` records, because the C++ issues them when it lays the batch out and the port with its
   next plan. With that run, the Rust records are identical from the sixth record on and equal as
   a multiset overall.
3. `golden.args`: the command line, which names the device (`--device ref`, `--device cuda`).

`tables.jsonl` (oracle `--golden-tables` now = committed = Rust) and `taps.jsonl` (Rust
`tb_convert::golden` = committed `tapsdump` output, byte for byte, test
`the_committed_taps_are_fresh`): identical. Stream digests of all five cases: oracle = Rust ref =
Rust card = committed.

So no parity exception of the G2 list shows in the golden cases; the only differences are the
arena path's. Then the vectors were regenerated with the Rust recorder (`golden/generate.sh`,
reference adapter, every case run twice and byte-compared) and five cases added:

| Case | What it adds | Records | Checked by |
|---|---|---|---|
| `skip` | rung 37 (11 all-skipped pictures coded) and I pictures replaced by P (2) | 1509 | replay `skip_equals_the_records` (tick_golden), control, vbv, gop, headers |
| `deadline` | the real-deadline path (`--test-deadline`, a new test switch: deadlines on the device clock; the reference adapter's clock counts steps, so it is deterministic): 16 budgets, sheds by budget | 2277 | `golden_deadline` (shed.rs: budget, shed list), control, vbv, gop, headers |
| `churn` | sessions opened/closed at run time, batches made in the arena and reaped (4 batches, 9 pool opens) | 3377 | control, vbv, gop, headers, layout (4 batch records) |
| `devin` | frames written on the device, padded columns (`k_pad_cols` from `pad_dev`) | 656 | control, vbv, gop, headers |
| `twofmt` | two formats, two batches in every phase | 2631 | control, vbv, gop, headers, layout |

Review of the new cases' streams with the gate's machinery (`gate_small.py`, cases on their own
merits: strict ffmpeg and libmpeg2 decode, normative buffer model for every stream and the
operating model with the rate schedule for the first, the verifier `h262-only`): 10 / 10 on the
reference adapter (sandbox check `gate/ref-small`) and on the card (`gate-small --device cuda
--golden`). Found while reviewing: ffmpeg probes a raw stream of one or two pictures with a "low
score" warning, which the gate counted as a decoder message; the gate's decode now names the
demuxer (`-f mpegvideo`).

On the card (`nix run .#gate-golden`, nixark): every record but the measured ones and the command
line, and every stream digest, equal to the reference adapter's for nine cases and both tables.
`deadline` differs, by nature: on the card the device clock is real time. It is exempt from the
comparison on the card and checked there on its streams' own merits.

New flake check `golden/fresh`: `golden/generate.sh --check` in the sandbox (every committed file
must be what the tree writes). The old check `gate/golden-tables` (subsumed by it) is gone.

## Kernel sources moved to `kernels/`

- `git mv` of `kernels.h`, `entropy.h`, `common.h`, `vlc_tables.h`, `pipe_kernels.h`,
  `tb_kernel_abi.h` and the generator `gen_tables.py` from `spikes/cuda-mpeg2/src/` to `kernels/`,
  contents unchanged (one comment line of `kernels/tb_kernels.cu` reworded in place, same line
  count). `nix/kernels.nix` compiles `kernels/` alone (`-Ikernels`); `nix run .#abi-header` writes
  `kernels/tb_kernel_abi.h`; the freshness check reads it there.
- Device image: `tb_kernels.cubin` before `8138f6c0…` (1552288 bytes), after `4fa9d820…`
  (1552160 bytes); `tb_kernels.sym` identical. Section by section (ELF parsed): 91 of 93 sections
  identical in contents and header, every kernel's code among them; the two that differ are
  `.debug_line` and `.rel.debug_line`. In `.debug_line` (one line-number unit) the include
  directory `/build/source/spikes/cuda-mpeg2/src` became `/build/source/kernels` (14 bytes
  shorter); the file table (names and directory indices) and the line program are identical. The
  24 relocations of `.rel.debug_line` have the same symbols and types, every offset 14 lower. So
  the image differs only in the recorded source path.
- The oracle and `tapsdump` still build at this commit (with `-Ikernels`); they go in the next.

## Engine in its own crate (`a470888`)

`crates/tb-engine`: `engine.rs` (Engine), `device.rs` (TickDevice, Guarded) and `rng.rs` (the C++
drivers' random sequences, used by both drivers) moved there with `git mv`; `tb-pipe` no longer
depends on the `tb-m2venc` crate. No behavior change (tests 38 / 38).

## Switches and refusals (`7cf258d`)

- Dropped from `tb-m2venc`: `--one-plan`, `--no-recycle`, `--frames-event` (comparison only; the
  encoder option `EncoderOptions::frames_event` stays, tested in `tick_model`), the refusal of
  `--spin` (now an unknown option, like any other) and the `NotPorted` error.
- Kept as a test switch: `--input-copy` is `--test-input-copy` (three tests compare the
  `FrameSource::Host` path with `HostAt` through it). New test switch `--test-deadline NS` (the
  golden case `deadline`).
- `tb-pipe`: input options other than `sw`, `threads`, `at`, `unsafe` are refused by name (the
  `@cuvid` / `@deint` refusal folded into it).
- The nine-stage profile of the C++ `m2venc` was never ported (the Rust `--profile` reports per
  plan); nothing to remove.

## Gate machinery moved to `gate/` (`5d88a24`)

- Moved with `git mv`: from the spike's `scripts/`: `check.py`, `formats.py`, `vbv_check.py`,
  `conform.py`, `pipe_check.py`, `bench.py`, `gpumon.py`, `pipe_lineup.py`, `contend.sh`,
  `contend_report.py`, `contend_late.py`, `contend_shed.py`, `contend_table.py`, `contend_nsys.sh`,
  `fmt.sh`, `prep_media.sh`, `prep_inputs.sh`, `derive_media.py`, `qdiff.py`, `ncu_profile.sh`,
  `ncu_summary.py`, `nsys_profile.sh`, `nsys_pipe.sh`, `nsys_tick.py`; from `scripts/`:
  `gate_small.py`, `pipe_small.py`, `bench_capacity.py`; `src/gpucomp.cu`; the baselines
  `results/check/{baseline,ref}.json` to `gate/baselines/`; `capped_selftest.sh` to `scripts/`.
- Defaults: `M2VENC` / `M2VPIPE` are `tb-m2venc` / `tb-pipe` (no `build/m2venc`), scratch and
  results under `work/gate/` (`gate/env.sh`, `gate/formats.py`), the corpus `M2V_MEDIA` (tree
  `media/` or `~/totalblonder-spike/media`). `check.py` reads the baseline from
  `gate/baselines/` and keeps its cache of reference encodes in the results directory.
- `gpucomp` (the contention competitor) is a Nix derivation (`nix/gate.nix`, the kernels' nvcc
  flags, cudart), in `gate-tools`; `nsys` and `ncu` are flake packages of the pinned toolkit
  (the spike's flake provided them).
- Apps: `gate-check` (was `gate`), `gate-pipe`, `gate-small`, `gate-golden`, `gate-bench`,
  `gate-lineup`, `gate-contend`, `gate-prep`, `gate-tb-pipe`. Gone: `gate-compare`,
  `gate-pipe-compare`, `gate-bench-compare`, `gate-oracle`.
- `gate/README.md` lists every tool.

## Deleted (`12ced6a`)

- The C++ host: `enc.h`, `dec.h`, `pipe.h`, `m2venc.cu`, `m2vpipe.cu`, `golden.h`,
  `tapsdump.cu`; `decbench.cu`, `convbench.cu` (dropped: the decoder's rate is what the pipeline
  line-ups measure with `tb-pipe` next to `nvidia-smi dmon`, and the conversion kernels are the
  same image the gate and line-ups run; neither micro-benchmark ran against the Rust code, and
  both were C++ host programs); `step0/saxpy.cu`; the spike's `Makefile`, `flake.nix`,
  `flake.lock`, `README.md`; the experiment scripts not moved (campaigns, scale and paced runs,
  quality and crosscheck helpers, `run.sh` and the spike's copy of `capped.sh`, `decbench.py`,
  `capclean.py`, `bench_diff.py`, `report_tables.py`, `deint_quality.sh`, `upload_ident.sh`, …:
  they drove `build/m2venc`; their results stay).
- The comparison layer: `scripts/gate_compare.sh`, `churn_trace.py`, `golden_au_diff.py`; the
  derivations `oracle`, `tapsdump`, `taps` and checks `kernels/oracle`, `kernels/taps`,
  `kernels/taps-fresh`; package `m2venc-oracle`.
- Kept: `spikes/cuda-mpeg2/results/` as frozen history with a README (the notes cite it).

## Documentation

- `notes/cuda-mpeg2-spike.md`, `notes/cuda-mpeg2-optlog.md`,
  `notes/cuda-mpeg2-optimization-ideas.md`: a header says they are history and where the living
  documentation is.
- `docs/ARCHITECTURE.md`: §7 is "How the encoder is checked" (golden vectors from the Rust
  recorder, the levels, the verifier in the gate); the parity exceptions are design notes there;
  §7.4 "What is C++": the kernels; §10 "History" (the packages, where the C++ is in git, what C++
  names in comments mean; the crates' comments point there); §6 the new layout and apps; risks
  updated.
- `docs/port/`: kept as completed history (a header in its README), not deleted: the notes hold
  the evidence that ARCHITECTURE and the worklist cite.
- `README.md` "Where things are", `golden/README.md` (rewritten), `gate/README.md` (new),
  `kernels/README.md`, `notes/post-port-worklist.md` (Structure items ticked), `notes/design-inputs.md`,
  `notes/m2v-conformance-findings.md` §8, `notes/format-conformance.md` and `docs/SPEC.md` (paths).
- Crate comments: the C++ provenance in plain words, no paths to removed files (`27c25ab`; done by
  a sub-agent under my brief, reviewed).
- Kernel comments that named the C++ host (`pipe.h`, `m2venc --selftest`, "the spike") were
  reworded with the same byte length per file, so that the line table (which records each
  source file's length) and therefore the device image stay byte-identical to the image after
  the move (`4fa9d820…`; checked by building it again).

## After the retirement: the same streams, with the verifier

Final tools `/nix/store/jrm695bp…-gate-tools` (commit `8a1aff6`; tb-m2venc `7a4p1xhm…`, tb-pipe
`22fspb50…`), on the card 2026-11-26 03:45-03:52 UTC, card idle by `nvidia-smi pmon`:

- `gate-check` (full) 54 / 54, 70 streams verified (no allowed finding); `gate-pipe` 46 / 46, 228
  streams verified (both `gate-allow.txt` entries matched); `gate-small --device cuda --golden`
  10 / 10; `gate-golden`: every record and digest equal to the committed ones for nine cases and
  both tables (`deadline` exempt on the card, by design).
- All 308 reference streams (`X1-reference-streams.sha256`) found again with the same SHA-256:
  0 differ. (Also at the intermediate run after the deletions, tools `cd67xjbx…`: 0 differ.)
- Device image: the final tree's cubin is `4fa9d820…`, the image after the move, whose only
  difference from the starting one (`8138f6c0…`) is the recorded source directory (above).
- `gpu-plumbing` (the CUDA adapter's tests on the card, `float_parity` among them): 12 / 12 and
  1 / 1.
- Capacity sanity, free-running, the G2 final binary (`6d1319pd…`, "old") against the final one,
  interleaved, 5 s runs, every run clean (pmon: only our processes): by round, final / old 1080i
  0.996, 1080p 1.000, 720p 1.017, 480i 0.983 with 2 rounds and 0.999 (median 1.002, 0.987 to
  1.006) with 4 rounds, mixed 0.996. Pipeline line-up (a) at 27 × 1080p30 → 1080i, paced 12 s:
  old and final both 100 % of ticks on time, tick 6.11 / 6.07 ms, decoder 85 / 91 %.
- `gpucomp` from Nix ran on the card (2 s, 121 launches, round trip p50 0.124 ms).
- `nix flake check` on bagel: 250 checks, all green, at `8a1aff6` (after it only these notes
  changed).

## Incidents

- Two `rsync`s to nixark went to the wrong directories, from a shell whose working directory was
  not the worktree (my mistake: a command chained after a backgrounded `cd … && (…) &`):
  1. the main checkout's `scripts/nixark.sh` (read only; nothing in `~/totalblonder`
     was written: `git status` there is clean, HEAD `e79069b`) synced the main checkout's tree
     into `~/totalblonder-rs-x1`; the next GPU session ran against that tree and failed at once
     ("run from the repository root"); re-synced from the worktree and re-run, all green.
  2. Correcting it, I ran the worktree's `scripts/nixark.sh` without `NIXARK_DIR`, which syncs
     to the default `~/totalblonder-rs`: that directory (a tree of an earlier package) now holds
     the X1 tree, and `rsync --delete` removed whatever it had that the X1 tree does not (for
     example a `spikes/cuda-mpeg2/build/` or `out/` of earlier runs, if there were any; the old
     sync excluded those, the new one does not). Not recoverable from here. The corpus is
     untouched (`~/totalblonder-spike/media`, 20 GB, 45 entries; a symlink to it would have been
     removed as a link, not followed).
- No out-of-memory line in the kernel log since 02:00 UTC; `systemctl --failed` empty (system
  and user); no process of ours left; power limit 70 W, compute mode Default; 63 GB free at the
  end. Kernel log: NVRM Xid 31 at 03:42 and 03:50 UTC (process `device_fault_is…`): the CUDA
  adapter's test that provokes a fault on purpose, its process only (the 02:18 one is G2's).
- Scratch on nixark: `~/x1-work` (logs and digest lists, 7 MB; the streams removed),
  `~/x1-*.sh`, `~/totalblonder-rs-x1` (the tree).

## For the owner to decide

1. `kernels/vlc_tables.h` and `common.h` still hold the C++ spike's copy of the VLC table values;
   the kernels read the `Tables` record built by `tb-m2v-syntax`. Removing the copies changes
   nothing the kernels compute but shifts the image's line table; left for the next kernel
   change (ARCHITECTURE §3).
2. `decbench` / `convbench` were dropped. If the static decoder-rate table of admission control
   (spike notes C.2) has to be measured again (new card, new driver), a small decode benchmark
   on `tb-decode` is the missing tool; the pipeline line-ups measure the decoder only as part of
   the whole.
3. The golden vectors grew from about 2 MB to 5.3 MB (`churn` 0.87 MB, `twofmt` 0.62 MB,
   `deadline` 0.69 MB); fewer frames would shrink them.
4. The `deadline` golden case is deterministic only on the reference adapter (its deadlines are
   on the device's clock); `--test-deadline` is a test switch of the driver for it.
5. `gate-allow.txt` allows by stream-name globs (`pipe-*`, `slate-*`, `devin-crop-*`); a new
   pipeline case with a progressive source inherits the allowance.
6. `tb-golden` is now a normal dependency of `tb-m2v-encoder` and `tb-convert` (the record
   writer), which brings `serde_json` into the encoder's dependency tree; the writer could be a
   crate of its own.
7. `rng` (the drivers' `std::mt19937` and distribution) went to `tb-engine` with the loop so that
   `tb-pipe` does not depend on `tb-m2venc`; it is a driver utility, not part of the loop.
8. `gate/bench.py` keeps the spike's older benchmark run besides the definitions
   `bench_capacity.py` uses; it may be cut down to the definitions.
9. `docs/port/` kept as completed history rather than replaced by one summary (the notes hold
   evidence that ARCHITECTURE and the worklist cite).
10. `EncoderOptions::frames_event` has no driver switch any more; it stays tested in the
    encoder's tests.
11. The C++ is named in three places on purpose: ARCHITECTURE §10 "History" (where the C++ is in
    git), `golden/README.md` "Provenance", and `notes/format-conformance.md` (an audit dated
    before X1, whose `enc.h` line references point into the git history).

## What remains

- The corpus still lives in `~/totalblonder-spike/media` on nixark (20 GB: raw clips, `in/`
  with the pipeline's compressed inputs, `download/`). A clean relocation: `mv` it to a
  directory of its own (for example `~/totalblonder-media`), point `gate/env.sh` and
  `gate/formats.py` (and `nix/gate.nix`'s preamble) at it, then `~/totalblonder-spike` (the old
  spike tree, its `build/`) can go. `nix run .#gate-prep -- media|inputs` rebuilds it from the
  Xiph downloads anywhere (about 25 GB).
- Not run after the move (paths updated, untested): the Nsight helpers (`gate/nsys_*.sh`,
  `gate/ncu_profile.sh`; `nix build .#nsys` / `.#ncu` evaluate), the contention harness as a
  whole (`gate-contend`; `gpucomp` itself ran), `gate-prep`.
- `notes/post-port-worklist.md`: the child process for the device-owning loop (the crate is the
  first step), the conformance gaps, the bugs ported as they were, the features.
