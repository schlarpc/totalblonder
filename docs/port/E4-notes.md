# E4 running notes (decisions and open points)

Working log of the E4 package (`tb-m2v-encoder::tick`). Newest decisions override the brief
where they differ; each is repeated in the module docs of `tick.rs`.

## Decisions

1. **Upload fence / second event.** The oracle's op sequence is reproduced exactly: with
   `overlap` a tick starts with a plan of its own that waits on `UPLOAD` (phase `Sync`, the C++
   `cudaStreamSynchronize(st_up)`), then `Begin` with `Record(PARAMS)` on `MAIN` after the
   parameter uploads and `WaitFor(PARAMS)` at the head of the `UPLOAD` steps. No `FRAMES`
   event is emitted: golden traces replay with `Compare::NormalForm` unmodified, wait for
   wait. ARCHITECTURE risk 4 foresaw the host wait as a fourth plan.
2. **Always three plans per tick** (`Begin`, `Quant`, `Finish`), empty when no batch has work:
   the oracle waits three times in every tick and the golden traces contain those waits.
3. **Set-up of a new batch** (arena path, enc.h:964 and 993): `Fill` of the batch's device
   block and `Upload` of the tables, on `MAIN`, at the head of the next plan that waits on
   `MAIN` (`settle(MAIN)` or the next `Begin`). The golden cases ran without the arena, so
   their leading `finish()` plans are empty; the golden test puts the two set-up steps into
   the first plan that waits on `MAIN` and checks the uploaded table bytes against
   `golden/tables.jsonl`.
4. **Padding of device-written frames and `k_tick`** are queued at the head of `Begin` (the
   C++ queues them before the host wait on the upload stream; same order on `MAIN`). A plan
   that waits on `UPLOAD` carries no `MAIN` steps, so every step belongs to a plan that waits
   for its stream (the frame upload is the one exception, as in the C++).
5. **Host frames with `overlap`**: two frame regions per slot, used alternately, because the
   upload of tick n may still read its region while the caller writes the frame of tick
   n + 1 (the C++ asks the caller to keep the frame valid for two steps).
6. **Device errors**: see module docs (`Failed`/`BadPlan`: repeats + re-key for the pictures
   in the plan; `Lost`: every session repeats until `device_restored`; `Timeout`:
   `Step::Wait`). Ring slots whose upload was in a failed plan are remembered and their
   frames become repeats.
7. **Phase tag** for the adapter's wait classes: `Encoder::phase()` / `Phase::class()`.
8. **Snapshot / restore** of a session for a restart in a fresh process after `Lost`
   (D1 finding: the CUDA context cannot be rebuilt in-process).

## Not done / open

- Brief acceptance 5 (end to end on `tb-device-ref`): the encoder may not depend on an
  adapter (ARCHITECTURE §2) and the package is confined to `tb-m2v-encoder`; belongs to G1.
- `WaitFor(FRAMES)` instead of the host wait: not built; G2 can measure whether it matters.

## State (end of the package)

- `src/tick.rs` (API, tick driver, errors), `src/tick/batch.rs` (the C++ `Batch` phases),
  `src/tick/session.rs` (session state, host repeats).
- Tests: `tests/tick_golden.rs` (five golden cases through `sim::Replay`, faults),
  `tests/tick_model.rs` (model device `tests/common/model.rs`), `tests/tick_alloc.rs`.
- Golden replay needed no change to any merged crate.
- C++ behavior ported as is and worth a second look: the P picture that replaces an I
  picture on rung 37 keeps the sequence header (only the GOP header is dropped); the
  same-code correction accepted after `choose` rewrote the row quantizers (R2's note); a
  refused `open` in the C++ leaves the new batch allocated (the port checks the rate first).
