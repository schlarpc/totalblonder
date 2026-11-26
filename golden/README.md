# Golden vectors

What the encoder decided on its host side, and what crossed its host/device boundary, for ten
small cases, as `tb-m2venc --golden` records them on the reference adapter. They are
regression snapshots: each pure crate is tested against the records it owns for exact
equality, the tick is replayed against the recorded plans (`tb_device::sim::golden`), and the
assembled encoder against the coded streams (`*.sha256`). A deliberate change of behavior
regenerates them (below) and the diff of the vectors is part of the change's review.

| File | Case |
|---|---|
| `tables.jsonl` | The table record the kernels read (`Tables`), as the encoder builds it (`tb-m2venc --golden-tables`) |
| `taps.jsonl` | The host arithmetic of the conversion path: filter taps (`tb_convert::make_taps`) for specs covering every branch, and every color matrix (`make_csc`); `tb-pipe --golden-taps`, from `tb_convert::golden` |
| `cbr.jsonl` | 176x144 progressive, 30 frames, GOP 6 / M 3, rate control at 300 kbit/s, captions, two rate changes at key frames |
| `ladder.jsonl` | 176x144 noise, 24 frames, 135 kbit/s with the smallest buffer: correction passes and ladder rungs 33 to 36 |
| `fixedq.jsonl` | 184x144 (width not a multiple of 16), 12 frames, fixed quantizer, a forced key frame |
| `interlaced.jsonl` | 176x144 interlaced (coded height 160), 24 frames, overlapped upload, captions, a rate change inside a GOP |
| `shed.jsonl` | Two staggered sessions, 30 frames each; pictures given up at random (`--force-shed`) and one session held for six frames: repeats, missed and off-grid key frames |
| `skip.jsonl` | 176x144 noise at 132 kbit/s (the floor of the buffer guarantee), smallest buffer, a key frame every second picture: the all-skipped rung 37 and I pictures replaced by P pictures |
| `deadline.jsonl` | Three staggered sessions with a real deadline (`--test-deadline 1025000`: 1.025 ms of the reference adapter's step clock, 1 ms of it the margin): the budget path of the deadline policy, pictures given up by budget and rotation |
| `churn.jsonl` | A running session beside a pool of three interlaced sessions opened and closed at run time (`--churn 5:15`, two slots per batch, empty batches reaped after three ticks): batches laid out in the arena and given back, four batches in all |
| `devin.jsonl` | 184x144 frames written on the device (`--input-mode dev`: the driver copies them from the clip held there; the encoder pads the columns) |
| `twofmt.jsonl` | Two formats in one encoder (176x144 progressive with captions, 176x144 interlaced): two batches in every phase |
| `film.jsonl` | Film mode on a 176x144 interlaced session (`--film 3,2,3,2,3,2,2,i,i,3,3,2`): frames shown for three and two fields with a cut in the cadence, video pictures and a still in between; captions of 30 and 20 constructs; a rate change; four frames held. Some ticks have no picture |
| `film60.jsonl` | Film mode on a 176x144 progressive session at 59.94 Hz (`--film 3,2,3,2,3,2,1,1,2,3`): frames shown one, two and three times, pictures given up at random, overlapped upload |
| `<case>.sha256` | SHA-256 of the coded stream of each written session of the case |

Every stream of every case is also checked on its own merits by `gate/gate_small.py` (strict
decode in ffmpeg and libmpeg2, both buffer models, reconstruction against decode, captions, the
conformance verifier), in `nix flake check` on the reference adapter and on the card
(`nix run .#gate-small -- --device cuda --golden`).

Not covered by these cases (the crates' own tests cover them): a device that fails or is lost
(the replays inject faults into the recorded traces), the driver's own copies of
device-written frames (they are the driver's plan, not the encoder's, and are not recorded),
captions through the pipeline harness.

## Provenance

The first five cases, `tables` and `taps` were recorded by the C++ encoder of the CUDA spike and
its dump of the conversion tables (both deleted in X1; in the git history at `e79069b`), which
the Rust port was written against. Before the C++ was
deleted (package X1, `docs/port/X1-notes.md`, "Golden hand-over") the Rust recorder was proved
to write the same records:

- `tables.jsonl` and `taps.jsonl`: byte-identical;
- the five encoder cases, recorded afresh by the C++ encoder on the card (equal to the committed
  files) against the Rust recorder on the reference adapter and on the card: every record
  identical, byte for byte, except the timing values of the measured records (`tick_end`,
  `shed_hist`) and three differences that come from the C++ encoder's cases running without its
  arena: the `batch` record says `arena` 1 and the arena's rounded `dev_bytes`, and the set-up
  of the batch (`fill ablk_d`, `upload d_T <- e->T`) appears as the first two operations of
  the first plan that waits on the kernel stream. Run with its arena (`--arena 64`), the
  C++ encoder records the same two operations with the same sizes, before the `batch` record
  (the C++ issues them when it lays the batch out, the port with its next plan);
- the coded streams: identical (the digests of the five cases are the C++ encoder's).

The vectors were then regenerated with the Rust recorder; the other five cases were added
then. The C++ is gone; `tb-m2venc --golden` is the only recorder.

## Regenerating

```sh
./golden/generate.sh                        # any machine with Nix; no GPU (reference adapter)
nix run .#gate-golden -- DIR                # on the card: the same cases, compared with golden/
```

`generate.sh` builds `tb-m2venc` and `tb-pipe` with Nix from the tree (or takes `TB_M2VENC`,
`TB_PIPE`), makes the clips with `tb-m2venc --make-clip` (a port of `make_clip.py`, checked
against it by `gate/make-clip`), runs every case twice and refuses a case whose two runs
differ. On the reference adapter even the measured records are deterministic (its clock counts
executed steps). The check `golden/fresh` of `nix flake check` runs it with `--check` and fails
when a committed file differs from what the tree writes. `gate-golden` runs the cases on the
card: every record but the measured ones and the command line, and every stream, must be the
reference adapter's (on nixark at the hand-over: identical for nine cases and both tables).
The case `deadline` is the exception: its deadlines are on the device's clock, which counts
executed steps only on the reference adapter; on the card the policy decides by the real time
of each tick, so its records and streams differ by nature, and it is checked there only on its
streams' own merits.

The recorder is off unless asked for (`Encoder::record_golden`): the hooks are untaken
branches, and the gates' streams are byte-identical with the recorder in the code. The
records come out in the order the C++ encoder wrote them, with its number format.

## Format

One JSON object per line. `k` is the record's kind. Conventions:

- integers are JSON integers; flags are 0 or 1;
- `double` values are printed with 17 significant digits, which reads back to the same IEEE
  value: compare with `==`;
- byte strings, and arrays of C records, are lowercase hex (`par`, `rows`, `rowq`, `geo`,
  `bytes`, `cc`, `data`); records are in the layout of `crates/tb-kernel-abi`
  (little-endian);
- `fnv` and `rng` are 64-bit values as 8 hex bytes, little-endian;
- `s` is a session (order of opening, from 0), `b` a batch (order of creation), `tick` the
  encoder's step count; picture types are 1 = I, 2 = P, 3 = B;
- arrays indexed by picture type have four entries, entry 0 unused (`X`, `left`,
  `floor_hist`, `last_rung`, `theta`, `mdl_hdr`).

The reader and the writer are `crates/tb-golden` (`tb_golden::case("cbr")`,
`tb_golden::write`); the encoder's recorder is `crates/tb-m2v-encoder/src/golden.rs`.

Records are written in the order things happen, so a record's inputs are the state left by
the records before it. "Written by" names the function that writes each record; most keep the
names of the C++ functions they were ported from (`Batch::begin` is
`tb-m2v-encoder/src/tick/batch.rs`, `Session::host_repeat` `tick/session.rs`, `Encoder::*`
`tick.rs`; rate control's records come through `tb_m2v_rate::control::Observer`).

### Frames shown for other than one frame period (film mode)

The records of a frame that is shown for one frame period in the session's field order, at a
display time equal to its index, are what they were before film mode existed: the vectors of
sessions without film frames did not change when it was added. For any other frame the
records carry more fields, after the ones listed below:

| Record | Added fields |
|---|---|
| `submit` | `start` (display units accepted before the frame: fields of an interlaced session, frame periods of a progressive one), `units` (its display duration in those units), `top_first`. Whenever `units` is not one frame period or `top_first` is not the session's field order |
| `emit` | `start`, `units`, `top_first` |
| `hdr` | `time` (display time of the first field in frame periods, rounded down: the GOP time code), `tff`, `rff` (top_field_first and repeat_first_field as written) |
| `rc_plan` | `gop_frames` (display time to the next grid key frame in frame periods), `frames` (the picture's display time in frame periods). `to_next_key` is then an estimate of the picture count |
| `au` | `units`, `top_first`, `t` (the picture's time on the decoding clock, seconds; `coded / fps` otherwise) |

In every record `next_key` is the display time of the next grid key frame in frame periods
(the display index of that frame while every frame is shown for one frame period).

### Setup

| Kind | Written by | Fields |
|---|---|---|
| `golden` | the driver (`tb-m2venc --golden`) | `version` (1), `args` (the command line) or `case` |
| `tables` | `tb-m2venc --golden-tables` | `size`, `bytes`: the `Tables` record |
| `batch` | `Encoder::open`, when it lays a new batch out | `b`, `w`, `h`, `fps_n`, `fps_d`, `interlaced`, `capacity`, `geo` (the `Geo` record), `row_bs`, `dev_bytes`, `cap_words`, `fbytes`, `arena`, `keep_recon` |
| `open` | `Encoder::open` | `s`, `b`, `slot`, `error` (always empty: a refused session writes no record; the C++ wrote one with its message), every field of `SessionConfig` under its C++ name (`profile`: the name of the conformance profile; `color`: the three codes of the color description, empty when it follows the height), `min_rate`, `first_min`, `nmb`, `mbh`, `fps`, `X` (initial complexities), `D` |

### Input and GOP structure

| Kind | Written by | Fields |
|---|---|---|
| `submit` | `Encoder::submit`, before the GOP structure takes the frame | `s`, `disp` (the index the frame gets), `pts`, `slot`, `avail_step`, `prog`, and the state before: `want_key`, `rekey`, `hold`, `hold_prev`, `next_key`, `gop_first`, `pending` (frames waiting), `gop`, `m`, `gop_phase`, `cc` |
| `emit` | `Encoder::submit` and `Encoder::flush` (what `GopState` emits) | one frame leaving for the coded-order queue: `s`, `disp`, `type`, `gop_start`, `off_grid`, `slot`, `pts`, `avail_step`, `prog`, `cc`. The `emit` records between two `submit` records are the output of the first |
| `flush` | `Encoder::flush`, `Encoder::close` (the first flush of a session) | `s`, `pending` |
| `force_key` | `Encoder::force_keyframe` | `s` |
| `set_rate` | `Encoder::set_rate` | `s`, `bps` (asked), `clamped` (pending rate), `at_gop`, `ticket` |

### Buffer model

| Kind | Written by | Fields |
|---|---|---|
| `vbv` | rate control, after each operation on the buffer model (`advance`, `set_rate`, `add`) | `s`, `op`; operands: `advance`: `to`, and `cap` (the result of `cap()` afterwards); `set_rate`: `at`, `rate`; `add`: `bits`, and `room` (the result of `room()` afterwards). State after the operation: `D`, `queue`, `t`, `chg` (the rate history as `[t0, r0, t1, r1, ...]`; the first time is -1e18) |

Replaying the `vbv` records of a session in order on a fresh model (`D` and the initial rate
from `open`) must reproduce `queue`, `t`, `chg`, `cap` and `room` exactly.

### The tick

| Kind | Written by | Fields |
|---|---|---|
| `tick` | `Encoder::begin_tick`, on entry | `tick`, `deadline` (given or not), `rng` (state of the test hook's generator before the tick), `force_shed` |
| `shed_hist` | same | `rate_hist`: the policy's throughput history. Measured: differs between runs |
| `shed_cand` | `Encoder::plan_shed`, per candidate, in collection order | `tick`, `s`, `class` (0 B, 1 P, 2 key), `mbs`, `rot`, `priority`, `have_ref` |
| `shed_budget` | `Encoder::plan_shed` (`ShedState::plan_with_budget`), real-deadline path only | `budget`, `rate`, `allowed`, `mbs`. Measured |
| `shed` | `Encoder::plan_shed`, after the decision | `tick`, `candidates`, `shed_count`, `shed` (sessions that give up their picture), `rng` after |
| `begin` | `Batch::begin`, per picture to code | `tick`, `s`, `b`, `slot`, `type`, `key`, `disp`, `coded`, `t` (picture time), `cbr`, `e_before`, `cap`, `rate`, `ticket`, `fixed_q`, state before the picture: `floor_hist`, `avg_act`, `last_qs`, `newer`, `ref_disp`, `last_dist_f`, `last_dist_b`; `aq`, `tff`; `par`: the `SessPar` record that goes to the device |
| `lists` | `Batch::begin` | `tick`, `b`, `code`, `intra`, `me`: the session lists (slots) |
| `rowstat` | `Batch::quantize`, per picture | `tick`, `s`, `rows`: `mbh` `RowStat` records, as the device returned them |
| `hdr` | after the headers of a picture are written (`Batch::quantize`; `Batch::collect` for an I picture replaced by P; `Session::host_repeat`) | `s`, `type`, `gop_hdr`, `seq_hdr`, `disp`, `tref_base`, `prog`, `cbr`, `cc`, `bytes` (the headers written). Static inputs are in `open` and `batch` |
| `choose` | `RateControl::choose_obs` | `s`, `type`, `tgt`, the model used (`theta`, `hdr`), results: `ok`, `code`, `frac`, `nzpred`, `rowq` |
| `rc_plan` | `Batch::quantize`, per picture, after the decisions | `tick`, `s`, `type`, `key`, `cbr`, `intra`, `nfield`, `hdr_bits`, `avg_act`, `rate`, `fps`, `gop`, `m`, `to_next_key`, `R`, `X`, `left`, `target`, `hard`, `limit`, `code`, `frac`, `nzpred`, `rung`, `keep`, `last_rung`, `last_qs`, `e_before`, `cap`, `cap_bytes`, `rowq` |
| `qent` | `Batch::launch_qent` | `tick`, `b`, `list` (slots that quantize in this pass), `xf` (those that repeat the transform), `par` (their `SessPar` records, in list order) |
| `pass` | `RateControl::after_pass_obs` (from `Batch::retry`), per rate-controlled picture of the pass | what the pass measured and the test applied: `tick`, `s`, `type`, `have_ref`, `tries`, `rung`, `bytes`, `tex_bits`, `over_cap`, `tgt`, `limit`, `must`, `want`, and the choice that produced it: `code`, `frac`, `nzpred`, `theta`, `hdr` |
| `pass_out` | same, at every way out | `action` (`accept` or `again`), and the state after: `type`, `key`, `rung`, `keep`, `want_pred`, `code`, `frac`, `nzpred`, `tries`, `violations`, `isub`, `theta`, `hdr`, `rowq` |
| `finish` | `Batch::finish_gpu` | `tick`, `b`, `out_words`, `outpos` (per coded picture: first word, words reserved) |
| `au` | `Batch::collect` and `Session::host_repeat` | the access unit: `tick`, `s`, `coded`, `disp`, `type`, `position_type` (the type the GOP structure gave the frame), `key`, `off_grid`, `repeat`, `key_missed`, `len`, `fnv` (FNV-1a 64 of the bytes), `pts`, `dts`, `avg_q`, `complexity`, `bits`, `rate`, `ticket`, `queue`, `room`, `cap`, `rung`, `tries`, `intra`, `field`, `slice_bytes`, `tex_bits`; the session after it: `cbr`, `theta`, `mdl_hdr`, `X`, `R`, `left`, `floor_hist`, `last_rung`, `last_qs`, `avg_act`, `violations`, `newer`, `ref_disp`, `dup`, `have_ref`, `rekey`, `tref_base`; `rowq` (coded pictures); `data`: the whole access unit (headers and slices), when it is at most 65536 bytes, which is every picture of the committed cases |
| `tick_end` | `Encoder::end_tick`, when the tick coded something | `mbs`, `seconds`, `rate_hist`. Measured (deterministic on the reference adapter) |

### Device operations

| Kind | Written by | Fields |
|---|---|---|
| `op` | the plan of each phase as the encoder hands it out (`golden::Gold::commit`): every step in queue order, merged with the records written while the plan was built, then the host's wait for it | `tick` (-1 before the first tick, else the last tick begun), `op`, `st` (`main`, `upload`, `side`), and per operation: `upload` / `download` / `copy`: `dst`, `src` (the buffer names of the C++, for example `d_par`, `h_list[L_CODE]`; frames `dy`/`du`/`dv` from `sy`/`su_`/`sv`; the set-up `ablk_d`, `d_T` from `e->T`), `n` (bytes); `upload2d`: `dst`, `src`, `dst_pitch`, `src_pitch`, `width`, `rows`; `fill`: `dst`, `byte`, `n`; `launch`: `kernel`, `grid`, `block`; `record` / `waitfor`: the parameter event (`event: frames` for the frames event); `wait`: a host wait for the stream (`which`: 0, 1, 2 for the three waits of a tick) |

The `op` records of a tick are what the tick must issue as plans: the same operations on the
same buffers with the same sizes and launch dimensions, in the same order per stream, with a
wait where the recording waited. Buffer names map to the fields of
`tb_m2v_encoder::layout::BatchLayout` (`d_par` -> `d_par`, `h_list[L_CODE]` ->
`h_list[List::Code]`, `dl` / `lcur` -> the list of the current pass).

### Conversion (`taps.jsonl`)

Written by `tb_convert::golden::taps_records` (`tb-pipe --golden-taps FILE`), host arithmetic
only; the test `the_committed_taps_are_fresh` of `tb-convert` fails when the committed file
differs from what it writes. A spec the C++ refused (it ended the process) is a record with
`error` 1.

| Kind | Written by | Fields |
|---|---|---|
| `taps` | `make_taps`, one record per spec | `seq` (order), `set` (`grid`: every filter, scale and pair of row lattices; `weave`: the same with `weave_lp`; `phase`: chroma phase and `pad4`; `crop`: crops with fractional origins, bob and frame -> field; `small`: tiny sources; `class`: the four axes of a conversion class as `Converter::get` builds them), the `TapSpec`: `n_src`, `s_step`, `s_off`, `n_dst`, `d_step`, `d_off`, `crop0`, `crop_len`, `phase`, `filter` (0 bilinear, 1 bicubic, 2 Lanczos-3), `weave_lp`, and `pad4`. In `class` records also `key` (the `ConvKey`: `sw`, `sh`, `bps`, `cx`, `cy`, `cw`, `ch`, `dw`, `dh`, `srows`, `drows`, `filter`, `weave_lp`; rows 0 all, 1 top, 2 bottom) and `axis` (`hy`, `hc`, `vy`, `vc`). Result: `error` 1 when `make_taps` refuses the spec (the taps, rounded up to a multiple of four, outnumber the source lattice; `make_taps` returns an error), else `error` 0 and the `HostTaps`: `n`, `nt`, `n_lat`, `start` (`n` `int32`, hex) and `coef` (`n * nt` `int16`, hex) |
| `csc` | `make_csc`, every combination | inputs `from709`, `to709`, `from_full`, `interlaced`; the `CscPar`: `m` (9, row-major, Q12), `off` (3, Q12), `interlaced_out` |
