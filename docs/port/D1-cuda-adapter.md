# D1-cuda-adapter: the CUDA adapter, complete

| | |
|---|---|
| Runs on | GPU (nixark), serial with the other GPU packages; first of them |
| Needs | nothing (the proof of plumbing works today) |
| Crate | `tb-device-cuda` |

## Goal

Bring `crates/tb-device-cuda` from the plumbing proof to what the tick needs at capacity, and
measure what the protocol costs.

## What exists

`docs/ARCHITECTURE.md` §5.2. `src/api.rs` (own bindings, about thirty driver functions),
`src/lib.rs` (`CudaDevice`), `tests/plumbing.rs` (`nix run .#gpu-plumbing`). The C++ to
match: `Encoder::Encoder` (`spikes/cuda-mpeg2/src/enc.h:501-528`: device flags, streams,
event), `Encoder::wait` (`enc.h:481-495`), and the measured behavior in
`notes/cuda-mpeg2-spike.md` §0.2 (hybrid wait), §0.7 and optlog experiment 41 (the upload
fence).

## What to build

1. **Hybrid wait**: sleep for a share of the phase's usual duration, then poll, then block,
   as `Encoder::wait` does (keep an average per plan kind; the caller passes a small integer
   tag with the plan or the adapter keys on `plan.wait` and plan length).
2. **Several plans in flight** on different streams, completed in any order.
3. **`Lost`**: tear the context's objects down and make them again on request
   (`CudaDevice::restore()`), without leaking the pinned arena.
4. **Timing**: optional event pairs around a plan for device-side duration, for the stage
   profile of the benchmarks.
5. **Tests on the card**: `Upload2d`; a plan that waits on an event recorded by another
   stream in the same plan and in an earlier plan; a deliberately failing launch (grid 0 is
   rejected by validation: use a kernel image symbol mismatch or an out-of-resources launch)
   to see which statuses arrive synchronously and which at the wait, and document it in the
   crate; 10,000 plans without growth of host memory.
6. **Cost**: nanoseconds per step in `submit` for uploads, launches and events, against the
   same sequence of driver calls made directly (a bench binary under `crates/tb-device-cuda/
   benches` run through its breakbulk app), with the card idle. Record the numbers in
   `docs/ARCHITECTURE.md` §8.1.

## Acceptance

`nix run .#gpu-plumbing` passes with the new tests on nixark; `nix flake check` passes on
bagel (GPU tests ignored there); the cost table is in the architecture document.

## Not in scope

NVDEC and foreign device pointers (a later package designs that operation).

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-device-cuda      # this package's tests
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
