# G1-gate-parity: the driver and the correctness gate

| | |
|---|---|
| Runs on | GPU (nixark), serial |
| Needs | E4, D1, K1 |
| Crate | `tb-m2venc` |

## Goal

`tb-m2venc` takes the place of the C++ `m2venc` in the correctness gate and passes it, with
coded streams byte-identical to the oracle's.

## Steps

1. Before anything else: run the oracle's full gate on the tree as it is
   (`./scripts/run.sh make check-full` and `make check-pipe` in
   `~/totalblonder-rs/spikes/cuda-mpeg2`), because the oracle was modified when the port
   started (shared ABI header, golden hooks) and only the quick gate was re-run then.
2. Port the driver: `spikes/cuda-mpeg2/src/m2venc.cu` (options at `:57-164`, main loop from
   `:310`) to `crates/tb-m2venc`: the same options for everything `scripts/check.py`,
   `scripts/formats.py` and `scripts/bench.py` pass, the same output files (`--output`,
   `--recon`, `--stats`, `--json`, `--ticklog`), the paced and free-running loops. The clock,
   file I/O and pacing live here; the encoder stays pure. Options that exist only for
   experiments that were decided (`--churn naive`, `--blocking-wait`, `--spin`) may be
   refused with a message.
3. Make the gate scripts take the encoder binary from an environment variable (`M2VENC`,
   default `build/m2venc`): the one edit to the oracle's scripts this package owns.
4. Byte identity first: for every case of `check.py --quick`, then the full list, the stream
   of `tb-m2venc` equals the oracle's (`cmp`). Differences are bugs in the port; use
   `m2venc --golden` on the failing case and compare record by record to find the first
   decision that differs.
5. Then the gate itself with `M2VENC` set: `make check` (15 cases), `make check-full` (54).
   `make check-pipe` needs the pipeline harness and belongs to G2.

## Acceptance

`check` and `check-full` pass with the Rust binary on nixark; the streams of all their cases
are byte-identical to the oracle's; the results are copied to
`spikes/cuda-mpeg2/results/check/rust-*.{json,txt}` and committed.

## Not in scope

Capacity (G2). The server binary.

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-m2venc      # this package's tests
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt
  nix flake check                   # before every commit: everything, as CI would run it
  ```
  New files must be `git add`ed before `nix flake check` sees them.
- The C++ is the specification. Port it operation for operation: same order of floating-point
  operations, `f64` where the C++ has `double`, `f32` where it has `float`, no `mul_add`, the
  same integer widths and truncations. Where the C++ looks wrong, port it as it is and write
  the doubt in your report; do not fix it.
- Keep the public signatures of the skeleton. If one cannot work, change it, say so in the
  report, and update `docs/ARCHITECTURE.md` if it shows that signature.
- Sans-io crates keep `#![forbid(unsafe_code)]`: no clock, thread, file, socket, FFI or global
  state; time is an argument. No `unwrap` / `expect` outside tests. `thiserror` for errors. No
  new dependency without a reason in the report.
- Remove the `#![allow(unused_variables, ...)]` skeleton attribute from a module when its
  bodies are in.
- Tests compare with the golden records exactly (`assert_eq!` on `f64` is intended). Use
  `tb_golden::case("…")` (add `tb-golden.workspace = true` under `[dev-dependencies]`).
- Do not edit: `spikes/cuda-mpeg2/` (the oracle), `golden/*.jsonl`, other crates' modules,
  `docs/SPEC.md`. If the golden vectors lack something you need, say what in the report; the
  vectors are regenerated on the GPU machine by whoever holds it.
- Commit each finished unit by explicit path, plain message, no attribution lines.
- Report at the end: what is done, what is not, every deviation from the C++ or the skeleton,
  and the output of `nix flake check`.

## Rules on nixark (GPU packages only)

- Only one GPU package works on nixark at a time. `nixark` is shared with Plex, Jellyfin and
  nzbget; look at `nvidia-smi pmon` before timing anything.
- Work in the repository on bagel and run on nixark through `scripts/nixark.sh CMD…`: it
  copies the tree to `~/totalblonder-rs`, refuses under 40 GB free disk, and runs the command
  in a memory-capped scope (10 GB). Never touch `~/totalblonder-spike` except to read its
  `media/` (the gate's clips; `~/totalblonder-rs/spikes/cuda-mpeg2/media` is a symlink to it).
- The oracle's own tools run from `~/totalblonder-rs/spikes/cuda-mpeg2` through its
  `scripts/run.sh` (`./scripts/run.sh make check`), as `notes/cuda-mpeg2-spike.md` §2 says.
- No `nixos-rebuild`, nothing under `/etc`, no reboot, no service restart, no global Nix
  configuration, no garbage collection. Leave no process running. The login shell is fish:
  run script files or pipe to `bash -s`.
- Prefer building on bagel and `nix copy --no-check-sigs --to ssh://nixark <paths>`.
