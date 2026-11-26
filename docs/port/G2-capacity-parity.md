# G2-capacity-parity: capacity, and the pipeline harness

| | |
|---|---|
| Runs on | GPU (nixark), serial; last |
| Needs | G1, K2, C1, T1 |
| Crate | `tb-m2venc` |

## Goal

Show that the Rust host costs no measurable capacity, and port the pipeline harness so that
`check-pipe` runs on the Rust side.

## Steps

1. **Benchmarks**: `scripts/bench.py` with `M2VENC` set to `tb-m2venc`: `make bench` (free
   run, stage times) and `make bench-cap` for 1080i, 720p, 480i, 1080p and mixed, with the
   realistic arguments of `notes/cuda-mpeg2-spike.md` §2 (`--stagger --overlap`). Run old /
   new / old back to back, two runs each, and report per format: macroblocks per second,
   `cap99`, host CPU, tick time, and the host stages against the budget of
   `docs/ARCHITECTURE.md` §8.1.
2. Where the Rust host is slower beyond noise (3 %), profile (`perf` on the tick thread,
   Nsight Systems for the timeline as `scripts/nsys_profile.sh` does) and fix in the core or
   the adapter. Expected places: plan building and validation, argument marshaling, the wait
   strategy, the fourth dependency on the upload stream (`docs/ARCHITECTURE.md` §12 item 4).
3. **Pipeline harness**: port `spikes/cuda-mpeg2/src/m2vpipe.cu` and `dec.h` as an adapter
   crate (libav decode with the `cuda` hwaccel in the adapter's context, one thread per
   input) plus the harness binary, using `tb-timeline` (frame clock), `tb-convert`
   (planning) and the conversion kernels (K2). This needs the protocol operation for device
   memory outside the arena (decoder surfaces): design it in `tb-device` first and get the
   design reviewed before building on it.
4. `make check-pipe` (46 cases) with the Rust harness.

## Acceptance

Capacity (`cap99`) per format within noise of the C++ in back-to-back runs; `check-pipe`
passes; the numbers are in `notes/cuda-mpeg2-spike.md` as a new dated section and the
budget table of the architecture document is updated with measured values.

After this package the C++ host can retire: `docs/ARCHITECTURE.md` §7.4 says what moves
where.

## Not in scope

Mux, audio, the server.

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
