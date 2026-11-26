# D2-device-sim: the device simulator

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-device` |

## Goal

Make `tb_device::sim` able to stand in for a device in tests of the tick (E4), fed from the
golden vectors, and make it check the protocol's rules.

## What exists

`crates/tb-device/src/sim.rs`: `Recorder`, `Replay`, `Fault::{Fail, Delay}`, with tests.
`docs/ARCHITECTURE.md` §4 describes the protocol.

## What to build

1. **Host-arena rule.** `Replay` (and a new checking wrapper usable around any device)
   tracks the host regions named by uploads and downloads of plans in flight and reports a
   violation when the caller changes such a region before the completion (compare a snapshot
   at `submit` with the bytes at `wait`), or submits a plan that overlaps one in flight.
   Violations are `DeviceError::BadPlan` with a clear `what`.
2. **Scenario helpers** for E4's tests: a builder that takes a trace and a list of faults by
   phase ("the second `Quant` plan of tick 7 fails with `Lost`", "every plan from tick 3 to 5
   takes 9 ms longer"), on top of `Fault`.
3. **Normal form of a plan** for comparison with the oracle: a function from `Plan` to a list
   of `(stream, operation name, kernel name, grid, block, byte count)` tuples, and the same
   list from the `op` records of a golden tick (`golden/README.md`, "Device operations"), in a
   new dev-only module or in `tb-golden` behind a helper. Waits in the `op` records separate
   the plans of a tick. Buffer identity is by name on the oracle's side; provide the
   comparison by size and order, and leave the name mapping (C++ expression → layout field)
   as a table that E4 fills.
4. **Traces from golden records**: given a case and a batch layout (a table of `HostBuf` per
   C++ name), build the `Trace` a correct run would record: per plan the uploads (`par`,
   lists, `rowq`, `outpos` from the `begin` / `lists` / `qent` / `finish` records) and the
   downloads (`rowstat.rows`, `PicStat` from `pass.bytes` / `pass.tex_bits`, the slices from
   `au.data` minus the header bytes of the matching `hdr` record, placed by `finish.outpos`).
   E2 defines the layout type; until it lands, take the table as an argument.

## Acceptance

Unit tests for each item with hand-made traces, plus: the trace built from case `fixedq`
has one plan per `wait` record, and replaying it with a core that submits the normal-form
plans in order (a test double) completes without a mismatch.

## Not in scope

The tick itself (E4). Real timing models of the card.

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-device      # this package's tests
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
