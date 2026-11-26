# E3-shed: the deadline policy

| | |
|---|---|
| Runs on | CPU, any machine; can start now |
| Needs | nothing |
| Crate | `tb-m2v-encoder` |

## Goal

Fill `crates/tb-m2v-encoder/src/shed.rs`: which sessions give up their picture when a tick
would not finish in time.

## The C++ it ports

`Encoder::plan`, `spikes/cuda-mpeg2/src/enc.h:673-727`, with the comment above it
(`enc.h:658-672`) and `notes/cuda-mpeg2-spike.md` §C.7 (the rule, and the two earlier
versions that failed and why). The rate history update is at `enc.h:791-795`. Time enters as
`left_s` = `deadline - now - shed_margin` computed by the caller; the function reads no clock.

The test hook (`force_shed`) uses the generator `rng = rng * 6364136223846793005 +
1442695040888963407` and gives up a candidate when `(rng >> 40) / 2^24 < p`, except a key
frame of a session without a reference. The seed is `0x9E3779B97F4A7C15 + seed *
0x2545F4914F6CDD1D` (`m2venc.cu`, after the encoder is made).

The rotation value of a candidate is `(k + steps * 7) % 1000003` with k its index in
collection order; candidates are collected batch by batch, slot by slot.

## Acceptance

1. Case `shed` (hook path): for each tick, candidates from the `shed_cand` records (in
   order), `rng` before from the `tick` record; the result equals the `shed` record (`shed`
   list, `shed_count`, `rng` after).
2. Deadline path, from the C++ text, as table-driven tests: nothing shed when the work fits;
   order by priority, then class (B, P, key), then rotation; a key frame of a session
   without reference is never shed; with time left at least one picture is coded; a tick that
   starts after its deadline sheds everything it may; no shedding in the first 16 ticks or
   with an empty history.
3. `observe` shifts the history as `enc.h:791-795` does, and only for ticks that coded
   something.

## Not in scope

Making the repeat pictures (S1 writes them, E4 emits them).

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
