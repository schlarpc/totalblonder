# S1-syntax: MPEG-2 syntax written on the host

| | |
|---|---|
| Runs on | CPU, any machine; can start now, in parallel with the other CPU packages |
| Needs | nothing |
| Crate | `tb-m2v-syntax` |

## Goal

Fill `crates/tb-m2v-syntax`: the table record the kernels read, the headers of an access
unit, and the slices of a repeated picture, byte-identical to the C++.

## The C++ it ports (`spikes/cuda-mpeg2/src/`)

| What | Where |
|---|---|
| Header bit writer `HBits` | `enc.h:36-47` (already ported: `bits.rs`) |
| `Batch::headers`: sequence header, sequence extension, sequence display extension, GOP header, picture header, picture coding extension, A/53 caption user data | `enc.h:1221-1287` |
| `Batch::skip_slices` | `enc.h:1140-1157` |
| Table set-up in `Encoder::Encoder` (VLC tables copied, `scan_ras` inverted from `scan_pos`, `qcode_near`) | `enc.h:512-527` |
| Scan patterns, intra matrix, quantizer scale | `common.h:14-47` |
| VLC tables and their generator | `vlc_tables.h`, `gen_tables.py` |

Section numbers in the C++ comments refer to ITU-T H.262 (02/2000); the text is in
`work/standards/ITU-T_H.262-200002-S.txt` on bagel (not in git).

## The Rust to fill

- `tables.rs`: `tables() -> Tables`. Add `SCAN: [[u8; 64]; 2]` (from `k_scan_fig`) and a
  generated `vlc.rs` with the seven VLC tables as `[VlcCode; N]` constants. Generate it: add a
  `--rust` mode to `spikes/cuda-mpeg2/src/gen_tables.py` (the one permitted edit in the spike:
  the C output must stay byte-identical, check with `git diff` after regenerating
  `vlc_tables.h`), run it on the H.262 text, commit `vlc.rs` with a "generated, do not edit"
  header. `VlcCode` has an explicit `pad: 0`.
- `headers.rs`: `write_headers`, `write_skip_slices`. The mapping from the C++ inputs:
  `SeqInfo.cap = Some((max_bitrate, vbv_bits))` when the session is rate-controlled
  (`s.rc.cbr`), else `None`; `PicInfo.frame_pred_frame_dct = (g.interlaced == 0)`;
  `PicInfo.progressive_frame = f.prog`; `PicInfo.tff = cfg.tff`; the frame rate used for the
  GOP time code is `lround(fps_n / fps_d)`.

## Acceptance

1. `tables()` as bytes equals `bytes` of the `tables` record of `tb_golden::case("tables")`.
2. For every `hdr` record of the cases `cbr`, `ladder`, `fixedq`, `interlaced`, `shed`:
   `write_headers` with the record's `type`, `gop_hdr`, `seq_hdr`, `disp`, `tref_base`,
   `prog`, `cc` and the static inputs from the case's `open` record (`w`, `h`, `fps_n`,
   `fps_d`, `interlaced`, `tff`, `aspect`, `max_bitrate`, `vbv_bits`, `intra_vlc`) and `batch`
   record (`geo`: `f_code`, `interlaced`, `intra_vlc_format`, `alt_scan`) produces `bytes`.
3. For every `au` record with `repeat` = 1 in `shed`: the `hdr` record written just before it
   for the same session gives the header bytes; the rest of `data` equals `write_skip_slices`
   for the batch's `mbw`, `mbh` and the access unit's `type`.
4. Unit tests from the standard for what the vectors do not reach: a picture wider than 33
   macroblocks (the `macroblock_escape` loop in the skip slices), 720x480 and 4:3 aspect
   selection, each frame-rate code, the 31-triplet caption limit.

## Not in scope

Slice data (the kernels write it). Parsing. H.264.

## Working rules (the same for every package)

- Repository: `~/totalblonder` on `bagel`. Read `docs/ARCHITECTURE.md` §1, §2 and
  the section this brief names, and `golden/README.md` (the format of the golden vectors).
- Build and test:
  ```sh
  nix develop                       # toolchain, TB_* variables, nextest, cbindgen
  cargo nextest run -p tb-m2v-syntax      # this package's tests
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
