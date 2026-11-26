# K1-reference-adapter: the reference adapter, every kernel tested

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-device-ref` |

## Goal

Prove that every kernel runs correctly on `tb-device-ref` (the kernels compiled for the host
by `nix/kernels.nix`: `refLib`, from `kernels/tb_kernels_ref.cc` and
`kernels/shim/cuda_runtime.h`), so that E4 can run the whole encoder in the sandbox.

## What exists

`docs/ARCHITECTURE.md` §5.1. `crates/tb-device-ref/tests/plumbing.rs` tests `k_pyr` against a
Rust model and checks all 17 launch signatures by parameter size. The fiber launcher for
kernels with barriers (`k_xf`, `k_rowstat`, `k_ent_rowsum`) is written and has never run.

## What to build

Per-kernel tests in `crates/tb-device-ref/tests/`, each a small plan (as
`tb_device::selftest` does it) with outputs compared to a model written in Rust from the
kernel's comment in `spikes/cuda-mpeg2/src/kernels.h`:

| Kernels | Model |
|---|---|
| `k_pad_cols`, `k_pad_rows` | edge replication |
| `k_set_mode` | constant records |
| `k_mbstat` | variances and the frame/field DCT test, straight from the comment |
| `k_rowstat` | sums over a row of hand-made histograms (first use of the fibers and shuffles: block size 32 and 64) |
| `k_ent_rowsum`, `k_ent_picsum`, `k_ent_scatter` | prefix sums and bit placement of hand-made macroblock scratch words |
| `k_ent_resolve`, `k_ent_pack` | hand-made `MbInfo` rows: skipped runs, DC prediction, vector prediction; compare the packed bits with a bit string written from H.262 for two or three macroblocks |
| `k_quant`, `k_recon`, `k_xf`, `k_me`, `k_mvsmooth` | structural checks only (a flat block gives DC only; a zero-motion picture gives zero vectors; reconstruction of an intra block of constant value is that value). Exact values wait for K3 |

Also: a test that a kernel whose threads do not reach a barrier together is reported as
`DeviceError::Failed { code: -3 }` (needs a deliberately wrong block size for `k_rowstat`),
and a run of the tests under `valgrind` or with `-fsanitize=address` on the kernel library
(add a `refLibAsan` variant in `nix/kernels.nix` and a check that runs the test binary
against it), because a kernel that reads outside its buffers is silent on the card.

Fix what the tests find in `kernels/shim/cuda_runtime.h` and `kernels/tb_kernels_ref.cc`.
`kernels.h` itself is the oracle's: a bug there is a finding for the report, not an edit.

## Acceptance

All of the above in `nix flake check`. A table in the report: kernel, what was compared,
result.

## Not in scope

Floating-point equality with the card (K3). Speed.

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
