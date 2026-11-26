# T1-timeline: the input timeline

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-timeline` |

## Goal

Fill `crates/tb-timeline`: the timestamp mapper, the frame clock and the fallback selector of
`docs/SPEC.md` §5.3, §5.4 and §9.5, as pure state machines.

## Sources

- `docs/SPEC.md` §5.2 to §5.4 and §9.5: the specification. Defaults are there.
- `spikes/cuda-mpeg2/src/m2vpipe.cu` (search for the frame clock: selection of the newest
  frame due at a tick, repeat, drop, the slate after a hold, the 59.94 Hz tick with a divisor
  and phase per format) and `notes/cuda-mpeg2-spike.md` §C.7 "Scheduler shape" and §C.9
  "Timestamps" (equal-rate sources whose phase sits on a tick boundary alternate between
  repeat and drop without hysteresis: design the hysteresis here and state it).
- `notes/producers.md`: what the first producer does to timestamps (resets to zero on each
  connection, jumps across idle periods).

There is no oracle with vectors for this crate: the mapper and the fallback chain are new
code from the specification. Write the rules as tests first.

## Acceptance

Tests, each named after the rule of the SPEC it checks:

- Mapper: session start places the first key frame J after arrival; skew follows a producer
  100 ppm fast and is clamped at ±500 ppm; each discontinuity rule ends the session with its
  `SessionEnd`; a gap shorter than the live timeout does not.
- Frame clock: 30.000 fps into 29.97 drops one frame in 1001; a producer 100 ppm slow repeats
  one in 10,000; equal rates with a phase on the tick boundary and ±1 ms arrival jitter give
  no drop or repeat (the hysteresis); 24 fps into 59.94 gives the 3:2 cadence.
- Fallback: demotion after `hold` of repeats, promotion after `promote_after` of health, a
  flapping input does not flap the output, cuts only on GOP starts.
- No function takes a clock: every time is an argument.

Document each open choice you had to make (the SPEC marks most of this [proposed]) in the
report, for the owner.

## Not in scope

Audio (the sample clock joins later). Decoding. Scaling decisions (C1).

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-timeline      # this package's tests
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
