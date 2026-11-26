# R1-buffer-model: the buffer model

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-m2v-rate` |

## Goal

Fill `crates/tb-m2v-rate/src/vbv.rs`: the operating buffer model that the encoder enforces
and the mux will rely on, bit-exact against the C++.

## The C++ it ports

`struct Vbv`, `spikes/cuda-mpeg2/src/enc.h:114-177` (the comment above it states the model;
`notes/cuda-mpeg2-spike.md` §5.1 explains the rate-change contract; the self-test at
`m2venc.cu:186-193` is a worked example). Ignore `gid` and `gold(...)`: they are the golden
hooks.

| C++ | Rust |
|---|---|
| `init(d, rate)` | `Vbv::new` |
| `rate`, `set_rate`, `rbar`, `advance`, `cap`, `room`, `add` | same names |
| `gint(a, b)` | `delivered(a, b)` |
| `E`, `t`, `D`, `chg` | `queue`, `t`, `d`, `changes` |

`gint` builds a vector of breakpoints, sorts it and sums `rbar(midpoint) * width` left to
right. Reproduce exactly that order of additions. It allocates in the C++; here it runs once or
twice per picture on the hot path, so collect the breakpoints in a fixed array (at most
2 + 2 * changes entries; `advance` prunes history older than D, so the count is small) or in
a scratch vector owned by `Vbv`.

## Acceptance

1. For every session of the cases `cbr`, `ladder`, `interlaced`, `shed`: start from
   `Vbv::new(open.D, open.bitrate)` and apply the session's `vbv` records in order.
   - `advance`: call `advance(to)`; then `queue`, `t` and `changes` equal the record's
     `queue`, `t`, `chg`, and `cap()` equals the record's `cap`.
   - `set_rate`: call `set_rate(at, rate)`; state equals the record.
   - `add`: call `add(bits)`; `queue` equals the record's, `room()` equals `room`.
   Every comparison is exact.
2. The C++ self-test as a unit test: `new(0.5, 8e6)`, `advance(1.0)`, `set_rate(1.0, 4e6)`,
   then `cap()` at t = 1.0, 1.25, 1.6 is 4e6, 3e6, 2e6 within 1.
3. Property tests (plain loops over a fixed pseudo-random sequence, no new dependency):
   `rbar` is the maximum of the rates of the last D seconds; `delivered` is additive over
   adjacent intervals to 1e-6 relative; the queue never goes negative.

## Not in scope

`control.rs`, `model.rs` (R2). The normative Annex C model (the gate's `vbv_check.py` checks
it on streams).

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-m2v-rate      # this package's tests
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
