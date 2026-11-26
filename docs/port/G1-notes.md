# G1 running notes (decisions and open points)

Working log of package G1 (`tb-m2venc`, the gate). Newest entries override the brief.

## State

- Driver built: `crates/tb-m2venc` is a library (`opts`, `device`, `engine`, `run`, `report`,
  `rng`, `clip`) plus `main.rs`.
- First end-to-end result (2026-11-25, bagel, reference adapter): all five golden cases, six
  streams, byte-identical to `golden/*.sha256`. The float caveat (K3) does not show on these
  clips; nothing is `#[ignore]`d for K3.
- The GPU host (nixark) is not to be used until told; GPU commands are prepared, not run.

## Decisions

1. **Clips are uploaded from where they are** (`FrameSource::HostAt`, added to the core with
   `Encoder::reserve_host` and `Encoder::next_key`; additive). The C++ pins the clip and
   uploads from it; copying each frame into the session's frame buffer would cost 50 MB of
   memcpy per tick at 16 x 1080i, an artifact of the harness. `--input-copy` keeps the
   frame-buffer path (`FrameSource::Host`) for comparison and tests.
2. **Arena sizes** come from a dry run of the encoder's own allocator on the line-up
   (`run::arena_sizes`), plus the clips. Pool groups: `--arena MB` or twice their sessions.
3. **Run-time sessions** (`--churn`, `--pool`): always the arena path; `naive` and `reserve`
   are accepted and mean the same. Comparable with the oracle's `--churn S:R:naive` runs (and
   with `:reserve` only when the oracle's side thread happened to be ready in the same tick).
4. **Refused**: `--input-mode dev` (needs device memory outside the arena: G2), `--spin`.
   `--profile` gives times per plan (device-side from the adapter's timing events), not the
   nine stages of the C++.
5. **Random sequences** (`--rate-walk`, `--churn`): `std::mt19937` and libstdc++'s
   `uniform_real_distribution<double>` reproduced in `rng.rs`, tested against values printed
   by the C++ built with the flake's g++ 16.2 (old and new `generate_canonical` agree).
6. **Exit status**: 0 complete; 2 refused; 3 device lost; 4 plans failed; 5 device hung.
7. **`--golden` on the Rust side** writes `au` records only (the fields a driver can see),
   for `scripts/golden_au_diff.py` against the oracle's file.

## Open

- Pooled access-unit buffers: needs a core API (`recycle`); not done.

## GPU results (2026-11-25, nixark, after merging main with K3)

- No test was ever ignored for K3: the five golden cases matched on the reference adapter
  before and after the merge.
- `gate-small --device cuda --golden`: 5 of 5, digests equal the oracle's.
- `gate-compare --quick`: oracle 15/15, Rust 15/15, 23 streams, 0 different.
- `gate-compare` (full): oracle 54/54, Rust 54/54, 74 streams, 0 different.
- `GATE_PY=pipe_check.py gate-compare --only 'shed|hold|churn'`: 10/10 each; 156 streams, 16
  different, all streams of pool sessions in the `reserve` churn runs (the oracle opens later).
- `gate-bench`: three series, ratio Rust / oracle 0.965-0.994 (1080i), 0.967-0.994 (mixed);
  Plex transcoding during some runs.
- How it was run: `nix build .#gate-tools`, `nix copy --no-check-sigs --to ssh://nixark`,
  then `NIXARK_DIR=totalblonder-rs-g1 scripts/nixark.sh env GATE_WORK=... <store>/bin/gate-compare`.
  Nothing was evaluated or built on nixark. Scratch under `~/totalblonder-rs-g1/work`
  (stream files removed afterwards).
