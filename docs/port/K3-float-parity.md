# K3-float-parity: floating-point parity of the two kernel builds

| | |
|---|---|
| Runs on | GPU (nixark), serial |
| Needs | K1 (the kernels run on the reference adapter) |
| Crate | `tb-device-ref`, `tb-device-cuda` |

## Goal

Decide, with measurements, how the host build of the kernels and the device image produce the
same bytes for the four kernels that use `float` (`k_xf`, `k_quant`, `k_recon`, and the
temporal candidate of `k_me`), and implement the decision.

## What is known

`docs/ARCHITECTURE.md` §7.3. nvcc contracts multiply-add into fused operations; the host
build uses `-ffp-contract=off`. The oracle built with `-fmad=false` produces different
streams on three of four test configurations (measured 2026-11-25; the script was: build
`m2venc` twice in `~/totalblonder-rs/spikes/cuda-mpeg2`, with and without `-fmad=false`, and
`cmp` the streams of 1080i, 480i, 720p at 3 Mbit/s, 1080p fixed quantizer).

## Steps

1. Tests that run each of the four kernels on both adapters with the same inputs (real
   picture data: use `golden/make_clip.py` clips through the stages before the kernel on the
   reference adapter) and compare outputs byte for byte. Record where they differ.
2. Read the PTX (`nvcc -ptx`, or `cuobjdump`) of the four kernels and list every `fma.rn.f32`
   with the source expression it came from (`fdct8`, `idct8`, `n_act`, the quantizer
   expressions, the temporal vector scaling).
3. Option (a): write those contractions as `fmaf(...)` in `kernels.h`, build the image with
   `-fmad=false` and the host with `-ffp-contract=off`, and show (i) the image's streams are
   byte-identical to the oracle's of today on the gate's quick set, (ii) both adapters agree
   in the tests of step 1, (iii) `make bench` is within noise (old / new / old, as
   `notes/cuda-mpeg2-spike.md` §2 requires). This package may edit `kernels.h` for this
   purpose only.
4. If (a) cannot reproduce today's streams exactly, stop and report options (b) and (c) with
   their measured cost; the owner decides.
5. Also check the non-contraction differences: `floorf`, float-to-int conversion of
   out-of-range values, division. List any kernel expression whose result could differ
   between x86-64 SSE and the card for the same IEEE inputs.

## Acceptance

A section in `docs/ARCHITECTURE.md` §7.3 replacing the open question with the result and
the numbers; the tests of step 1 in `nix run .#gpu-plumbing` (card) and their reference half
in `nix flake check`.

## Not in scope

Any other change to the kernels.

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-device-ref      # this package's tests
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
