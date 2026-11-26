# R2-rate-control: rate control

| | |
|---|---|
| Runs on | CPU, any machine |
| Needs | R1 for the tests to pass (same crate, different files: `model.rs` and `choose` can be written while R1 is in progress) |
| Crate | `tb-m2v-rate` |

## Goal

Fill `crates/tb-m2v-rate/src/model.rs` and `control.rs`: picture targets, row quantizers,
correction passes, the ladder and the model updates, with decisions equal to the C++ for every
picture of the golden cases.

## The C++ it ports (`spikes/cuda-mpeg2/src/enc.h`)

| What | Where | Rust |
|---|---|---|
| The design, in prose | `enc.h:179-192`; `notes/cuda-mpeg2-spike.md` §5.2, §5.3 | |
| `RateModel`, `RateControl` | `enc.h:190-213` | `model::RateModel`, `control::RateControl` |
| `nz_at` | `enc.h:215-228` | `model::nz_at` (uses `std::log`: `f64::ln`) |
| `Session::min_rate` | `enc.h:813-818` | `control::min_rate` |
| Initial state and the rate checks in `Encoder::open` | `enc.h:553-568` | `RateControl::new` |
| Rate-control part of `Batch::begin`: buffer advance, rate change, `cap`, `avg_act`, `lam`, `qfloor` | `enc.h:1362-1397` | `begin_picture` |
| `Batch::quantize`, per session: activity average, fixed quantizer, GOP budget, complexity, target, limits, ladder start | `enc.h:1470-1562` | `plan_picture` |
| `Batch::choose` | `enc.h:1290-1327` | `choose` |
| `Batch::retry`, per session | `enc.h:1571-1669` | `after_pass` |
| `Batch::collect`, rate-control part | `enc.h:1726-1795` | `finish_picture` |
| `Batch::host_repeat`, rate-control part | `enc.h:1183-1218` | `repeat_picture` |

Lines that touch `h_par`, `h_list`, `work[...]`, CUDA calls or headers are the tick's (E4):
leave them out, but return what they need (`Pass::Again { keep, all_skipped, i_becomes_p }`,
`ParInputs`). `QFLOOR_MARGIN` is 3 (`enc.h:1061`). `GOLD …` lines are hooks.

## Acceptance

Replay each session of `cbr`, `ladder`, `fixedq`, `interlaced`, `shed` through a
`RateControl`, driven by the records in file order:

1. `open` → `RateControl::new`; `X` and `D` equal the record; `min_rate()` equals `min_rate`.
2. `begin` → `begin_picture(t, new_rate, type, fixed_q)`: `new_rate` is `Some(rate)` when the
   record's `ticket` is not 0. Compare `e_before`, `cap`, and `avg_act`, `lam`, `qfloor`
   decoded from `par` (`tb_kernel_abi::SessPar`).
3. `rowstat` + `rc_plan` → `plan_picture`: compare `target`, `hard`, `limit`, `code`, `frac`,
   `nzpred`, `rung`, `keep`, `R`, `X`, `left`, `avg_act`, `rowq`.
4. Each `choose` record → the same call reproduces `ok`, `code`, `frac`, `nzpred`, `rowq`
   (the records between an `rc_plan` or `pass` and its outcome belong to it).
5. `pass` → `after_pass` with `bytes`, `tex_bits`; compare with `pass_out`: `action`, `rung`,
   `keep`, `code`, `frac`, `tries`, `violations`, `isub`, `rowq`, and the model (`theta`,
   `hdr`: the C++ restores the saved model after choosing).
6. `au` → `finish_picture` (or `repeat_picture` when `repeat` = 1; `position_type` is the
   type to pass): compare `avg_q`, `queue`, `room`, `cap`, `rung`, `tries`, and the state
   after: `theta`, `mdl_hdr`, `X`, `R`, `left`, `floor_hist`, `last_rung`, `last_qs`,
   `violations`.

Not in the vectors, test from the C++ text: rung 37 (all-skipped), an I picture replaced by P,
the first picture of a stream on rung 36 (`violations`), `over_cap`.

## Not in scope

`vbv.rs` (R1). `SessPar` assembly, lists, launches (E4).

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
