# E4-tick: the encoder and its tick

| | |
|---|---|
| Runs on | CPU for the logic (replay, then reference adapter); the last step on the GPU belongs to G1 |
| Needs | S1, R1, R2, E1, E2, E3, D2 |
| Crate | `tb-m2v-encoder` |

## Goal

Fill `crates/tb-m2v-encoder/src/tick.rs`: sessions, batches, and the state machine that codes
one picture per session per tick by writing device plans and reading completions. With a
device that returns the oracle's results it must issue the oracle's operations and produce the
oracle's access units.

## The C++ it ports (`spikes/cuda-mpeg2/src/enc.h`)

| What | Where |
|---|---|
| Interface and overview | `enc.h:1-15`; `notes/cuda-mpeg2-spike.md` §6, §C.6, §C.7 |
| `Session` state, `submit`, `submit_dev`, `submit_again`, `set_rate`, `force_keyframe` | `enc.h:278-354`, `820-854` |
| `Encoder::open`, `close`, `finish_close`, `release`, `reap` | `enc.h:535-576`, `622-651` |
| `Encoder::step`: the order of phases for all batches | `enc.h:731-801` |
| `Batch::upload`, `pad_dev` | `enc.h:1078-1133` |
| `Batch::begin`, `launch_begin` | `enc.h:1329-1432` |
| `Batch::quantize`, `launch_qent`, `retry` | `enc.h:1435-1669` |
| `Batch::finish_gpu`, `collect`, `host_repeat` | `enc.h:1671-1797`, `1183-1218` |
| Launch macros and block sizes | `enc.h:1059-1076` |

Not ported (decided, `docs/ARCHITECTURE.md` §4.1): `reserve` / `adopt` and the side thread,
batches allocated from the driver, `Encoder::wait` (the adapter's), the profile marks.

## Design constraints

- The shape is in `tick.rs`'s module comment: phases `Begin`, `Quant` (repeated), `Finish`,
  one plan each, all batches in one plan, `Step::Run` / `Step::Done`.
- Use the pieces: `gop::GopState`, `layout::BatchLayout` placed with
  `tb_device::ArenaAlloc`, `shed::ShedState`, `tb_m2v_rate::RateControl`,
  `tb_m2v_syntax::headers`. Parameters go into the host arena as bytes (`bytemuck`).
- Overlapped upload: `Record(PARAMS)` after the parameter uploads on `MAIN`,
  `WaitFor(PARAMS)` before the frame uploads on `UPLOAD`. The frames of tick n are on the
  device for tick n + 1 because tick n + 1 starts with a plan that waits on `UPLOAD`, as the
  C++ does (`enc.h:747`). (This brief first asked for a second event, `FRAMES`, in place of
  that wait; the oracle's `op` records cannot confirm it, so it was not built.
  `docs/ARCHITECTURE.md` §4.2.)
- No allocation in `begin_tick` / `resume` after warm-up: reuse vectors; access-unit bytes
  are the one allocation per picture (as in the C++).
- Device errors (`docs/ARCHITECTURE.md` §4.5): a failed completion makes the batches of that
  plan emit repeats for the tick and re-key; `Lost` holds every session until the caller
  calls a new `Encoder::device_restored()` (add it), after which batches are cleared and
  tables uploaded again by the next plan.

## Acceptance

1. **Operations.** For each golden case, the plans of each tick in normal form (D2) equal
   the tick's `op` records, wait for wait, the host wait on the upload stream of `--overlap`
   (case `interlaced`) included. The oracle's waits outside ticks (`Encoder::finish`) are
   `Encoder::settle`. The golden cases ran without the arena: the clearing and the table
   upload of the new batch, which the port issues with the first plan that waits on `MAIN`,
   are added to the trace by the test.
2. **Bytes up.** `SessPar` blocks, lists, row quantizers and output positions written to the
   host arena equal `begin.par`, `lists`, `qent.par`, `rc_plan.rowq` / `pass_out.rowq`,
   `finish.outpos`.
3. **Access units.** Driven by the `Replay` built from each case (D2), `take_output` yields
   the `au` records: every field, and `data` byte for byte. Concatenated per session plus
   `SEQUENCE_END` after a session's last picture, the SHA-256 equals `golden/<case>.sha256`
   (compute it in the test with a small hand-written SHA-256 or compare lengths and FNV per
   picture, which the records carry).
4. **Faults** (replay with `Fault`): a failed `Quant` plan yields repeats for that tick and a
   key frame next; `Lost` holds all sessions and the streams stay decodable (headers and
   skip slices only); a delayed plan changes nothing but times; a deadline that has passed
   sheds as E3 says.
5. **Reference adapter**: the case `fixedq` end to end on `tb-device-ref` produces a stream
   that an independent check accepts: every start code in order, slice count, and for the
   integer-only path (intra DC, the packer) equality with the golden bytes where K3's
   findings allow. State in the report what differs and why.
6. A test with a counting global allocator: after 30 ticks of warm-up, `begin_tick` and
   `resume` allocate only the access-unit vectors.

## Not in scope

The command-line driver and the gate (G1). Conversions (C1, K2). The CUDA adapter (D1).

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
