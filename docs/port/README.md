# Work packages of the port

> **Completed history.** The port is finished: the Rust encoder passed the gate and held the
> C++ encoder's capacity (G1, G2), and package X1 retired the C++ (`X1-notes.md`: the
> reference streams, the golden hand-over, the moves and deletions). These briefs and notes are
> kept as the record of how each part was ported and checked; they cite files of the C++ spike
> (`spikes/cuda-mpeg2/src/`, deleted in X1, in the git history at `e79069b`) and commands that no
> longer exist (the comparison apps, the spike's Makefile). The living documentation is
> `docs/ARCHITECTURE.md`; the work that remains is `notes/post-port-worklist.md`.

One file per package. Each is written for someone with no other context: goal, the C++ it
ports with file and line, the Rust to fill, the golden vectors and acceptance tests, what not
to touch, how to build and test. `docs/ARCHITECTURE.md` §10 has the order.

Line numbers refer to `spikes/cuda-mpeg2/src/` at the commit that added these briefs
(`enc.h` has 1798 lines there). The golden hooks (`GOLD …` lines, `gold_*` functions) are not
part of the behavior to port.

## Can start now, in parallel, on any machine (no GPU)

| Package | Fills | Golden records it is tested against |
|---|---|---|
| [S1](S1-syntax.md) | `tb-m2v-syntax`: tables, headers, skip slices | `tables`, `hdr`, `au.data` of repeats |
| [R1](R1-buffer-model.md) | `tb-m2v-rate::vbv` | `vbv`, `open` |
| [E1](E1-gop.md) | `tb-m2v-encoder::gop` | `submit`, `emit`, `flush`, `force_key` |
| [E2](E2-layout.md) | `tb-m2v-encoder::layout` | `batch`, `open` |
| [E3](E3-shed.md) | `tb-m2v-encoder::shed` | `tick`, `shed_cand`, `shed` |
| [D2](D2-device-sim.md) | `tb-device::sim`: traces from golden records, protocol checks | `op` and the records that carry bytes |
| [K1](K1-reference-adapter.md) | `tb-device-ref`: every kernel tested on the host | Rust models of the kernels |
| [C1](C1-convert.md) | `tb-convert` | new vectors (`taps`), made in the sandbox |
| [T1](T1-timeline.md) | `tb-timeline` | none: new code from the SPEC |

These touch different crates or different modules of one crate (`E1`, `E2`, `E3` are three
files of `tb-m2v-encoder`; `R1` and `R2` two halves of `tb-m2v-rate`), so they do not
conflict. Each adds its own `[dev-dependencies]` line; resolve `Cargo.toml` / `Cargo.lock`
conflicts by taking both.

## After others (still no GPU)

| Package | Needs | Fills |
|---|---|---|
| [R2](R2-rate-control.md) | R1 | `tb-m2v-rate::{model, control}` |
| [E4](E4-tick.md) | S1 R1 R2 E1 E2 E3 D2 | `tb-m2v-encoder::tick`: the encoder |

## Need the GPU: one at a time on nixark, in this order

| Package | Needs | What |
|---|---|---|
| [D1](D1-cuda-adapter.md) | — | CUDA adapter complete; cost of the protocol measured |
| [K2](K2-conversion-kernels.md) | — | conversion kernels in image and ABI (GPU only for the final oracle check) |
| [K3](K3-float-parity.md) | K1 | float kernels identical on both builds |
| [G1](G1-gate-parity.md) | E4 D1 K1 | `tb-m2venc` passes `check` and `check-full`, streams byte-identical |
| [G2](G2-capacity-parity.md) | G1 K2 C1 T1 | capacity within noise; pipeline harness; `check-pipe` |

## Retirement of the C++

| Package | Needs | What |
|---|---|---|
| [X1](X1-notes.md) | G2 | the verifier in the gate, the Rust golden recorder and the hand-over, kernels to `kernels/`, the gate to `gate/`, `tb-engine`, the C++ deleted; the same streams on the card before and after |
