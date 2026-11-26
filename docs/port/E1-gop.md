# E1-gop: GOP structure

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-m2v-encoder` |

## Goal

Fill `crates/tb-m2v-encoder/src/gop.rs`: typing frames in display order and queueing them in
coded order, including forced key frames, the grid after a missed key frame and the flush.

## The C++ it ports

`Session::enqueue` and `Session::flush`, `spikes/cuda-mpeg2/src/enc.h:856-903`; `Frame`,
`enc.h:230-241`; `Session::emit`, `enc.h:346-352`. The policy for missed key frames is in
`notes/cuda-mpeg2-spike.md` §C.7 ("Key frame position"). The fields `hold`, `hold_prev`,
`rekey`, `want_key` live in `Session`; here `released_from_hold` is `hold_prev && !hold`, and
`rekey` is also set from outside (by the tick, when a key frame is replaced by a repeat:
`enc.h:1212`).

## Acceptance

For every session of every case: replay `submit` records in order. Before each, set
`want_key` (a `force_key` record precedes), `rekey` and the hold flags from the record's
state fields, then call `enqueue`; the frames appended to `coded` equal the `emit` records
that follow (`disp`, `type`, `gop_start`, `off_grid`, `slot`, `pts`, `prog`, `cc`), and
`next_key`, `gop_first`, `pending.len()` equal the next `submit` record's state fields.
`flush` reproduces the `emit` records after the `flush` record. The case `shed` has missed and
off-grid key frames; `fixedq` a forced key frame; `open.gop_phase` is non-zero for the second
session of `shed` (staggered).

Unit tests for: `m = 1` (no B pictures), `gop = 1`, a forced key frame on the frame before a
grid key frame, two missed key frames in a row (one make-up attempt only).

## Not in scope

Ring slots and `avail_step` are inputs here (the tick assigns them).

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-m2v-encoder      # this package's tests
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
