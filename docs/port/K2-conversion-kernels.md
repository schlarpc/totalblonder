# K2-conversion-kernels: conversion kernels in the image and the ABI

| | |
|---|---|
| Runs on | CPU for the work; GPU (nixark) for the oracle check at the end, serial |
| Needs | nothing; C1 uses the result |
| Crate | `tb-kernel-abi`, `tb-device`, `tb-device-ref` |

## Goal

Bring the conversion kernels of `spikes/cuda-mpeg2/src/pipe.h` into the device image, the
host library, the ABI crate and the kernel records, without changing the oracle's behavior.

## Steps

1. Split `pipe.h`: move the kernels and their device helpers (`pp_ld4` … `k_csc`,
   `pipe.h:66-357`) to a new `spikes/cuda-mpeg2/src/pipe_kernels.h` that `pipe.h` includes at
   the same place. No other change. (This package may edit these two files and `kernels/`.)
2. Move the records `Taps`, `ConvClass`, `ConvJob`, `RectJob`, `CscPar`, `PreJob`
   (`pipe.h:40-64`, `229`) to `crates/tb-kernel-abi/src/lib.rs` in the style of `Bufs`
   (pointer-bearing records as raw pointers, plain ones as `Pod`), add them to
   `cbindgen.toml`'s `include` list, to the layout test and to `kernels/abi_layout.cc`,
   regenerate the header (`nix run .#abi-header`), and delete the C++ definitions.
3. Add the seven kernels to `kernels/tb_kernel_list.h`, include `pipe_kernels.h` from
   `kernels/tb_kernels.cu` and `kernels/tb_kernels_ref.cc`, and add their records to
   `crates/tb-device/src/kernel.rs` (new argument kinds for the by-value records, with
   handle forms like `BufsRef`) and to the signature test of `tb-device-ref`.
4. A test per kernel on the reference adapter against a Rust model (copy, fill, a bilinear
   2:1 downscale with known taps, the color matrix on a flat picture).

## Acceptance

- `nix flake check` on bagel.
- On nixark, with the oracle rebuilt from the tree: `./scripts/run.sh make check` and
  `./scripts/run.sh make check-pipe` in `~/totalblonder-rs/spikes/cuda-mpeg2` pass (15 and
  46 cases), and the coded streams of three `pipe-*` cases are byte-identical to the binary
  built from the commit before this package.

## Not in scope

Planning (C1). Decoding.

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-kernel-abi      # this package's tests
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
