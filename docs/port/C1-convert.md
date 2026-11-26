# C1-convert: conversion planning

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-convert` |

## Goal

Fill `crates/tb-convert`: the filter taps and color matrices of the conversion path,
bit-exact against the C++, and the planning types that turn "this source into that rectangle
of that session's input" into kernel launches.

## The C++ it ports (`spikes/cuda-mpeg2/src/pipe.h`)

| What | Where |
|---|---|
| The design | the comment at the top, `pipe.h:1-24`; `notes/cuda-mpeg2-spike.md` §C.3 |
| `pp_kernel`, `pp_support` | `pipe.h:361-374` |
| `TapSpec`, `HostTaps`, `make_taps` | `pipe.h:380-438` |
| `ConvKey`, `ConvClassHost`, `PreSet`, `Converter` (class cache, job lists, the half stage for large downscales, launches) | `pipe.h:440-608` |
| `make_csc` | `pipe.h:610-644` |

## Steps

1. **Vectors first.** `make_taps` and `make_csc` are host code with no CUDA call. Write
   `spikes/cuda-mpeg2/src/tapsdump.cu` (a new file; it may include `pipe.h`) that prints, as
   golden records (`golden.h`), the taps of a list of specs covering every branch: each filter;
   up- and downscale by 2, 1.5, 2.25 and 4.5; each row lattice (all, top, bottom) on source
   and destination; chroma phases; `weave_lp`; crops with fractional origin; `pad4` on and
   off; and every `make_csc` combination. Build it with a derivation in `nix/kernels.nix`
   (like `oracle`), run it in a derivation (it needs no GPU), write `golden/taps.jsonl`, add
   the case to `tb-golden` and document the two record kinds in `golden/README.md` (these are
   the edits outside your crate that this package owns).
2. Port `make_taps`, `make_csc` and test them against the vectors exactly (`f64` math: `sin`,
   `floor`, `lround` as the C++ calls them; where a libm function could differ between the
   C++ build and Rust, say so in the report and show that the vectors pin it).
3. Design and write the planning half as pure code: class keys, job lists per class and tick,
   and the steps to emit (`tb_device::Op::Launch` of the conversion kernels). The kernel
   records for `k_conv_h`, `k_conv_v`, `k_conv_copy`, `k_half`, `k_fill`, `k_overlay`, `k_csc`
   arrive with K2; until then define the planner's output as your own enum of launches with
   the kernels' arguments by name, so K2 only has to map it.

## Acceptance

Steps 1 and 2 in `nix flake check`. Step 3 with unit tests for the cases of
`scripts/pipe_check.py` `geom-*` (which launches, in which order, with which counts).

## Not in scope

The kernels (K2). Decoding. The frame clock (T1).

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-convert      # this package's tests
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
