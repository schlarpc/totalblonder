# E2-layout: batch geometry and memory layout

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-m2v-encoder` |

## Goal

Fill `crates/tb-m2v-encoder/src/layout.rs`: the `Geo` of a format and the placement of every
buffer of a batch in the two arenas.

## The C++ it ports

`Batch::init`, `spikes/cuda-mpeg2/src/enc.h:919-1045`: geometry (`mbw`, `mbh` with the
interlaced rounding, `f_code` from the search reach, `alt_scan` default, `g.interlaced`,
`g.field_pred`, `row_bs`), sizes (`fbytes`, `ysz`, `csz`, `nmbtot`, `nblk`, `cap_words`), and
the list of allocations in order with their sizes. Port the arena path: device buffers are
`bytes + 2 * GUARD` rounded up to 256 with the usable region `GUARD` bytes in; host buffers
`max(bytes, 16)` rounded up to 64. `B.ring_u` and `B.ring_v` are inside the ring allocation
(`ring_y + ysz`, `+ csz`).

Additions to the C++ layout (new in the Rust design): `h_frames`, one input frame per slot in
the host arena for the host upload path (the C++ takes the caller's pointer). Put it last so
that the offsets before it match.

## Acceptance

1. For the `batch` record of each case, `geometry(cfg, capacity)` built from the `open`
   record's configuration equals `geo` (as `tb_kernel_abi::Geo`) and `row_bs`; `cap_words`
   and `fbytes` equal the record.
2. The golden cases ran without the arena (`arena` = 0), where `dev_bytes` is the sum of
   `bytes + 2 * GUARD` over the device allocations: expose that sum
   (`BatchLayout::dev_bytes_unrounded()` or a test helper) and compare it. It proves the
   allocation list and sizes.
3. Layout invariants as tests: no two buffers overlap; every device buffer has `GUARD` bytes
   free on both sides; offsets are multiples of 256 (device) and 64 (host); `at(d, h)` shifts
   every buffer; `bufs` planes lie inside the ring / reference allocations.
4. Formats: 1920x1080 interlaced (mbh 68), 1280x720, 704x480 interlaced, 184x144, and the
   refusal above 4096 samples.

## Not in scope

Using the buffers (E4). The arena allocator itself (`tb_device::ArenaAlloc`, done).

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
