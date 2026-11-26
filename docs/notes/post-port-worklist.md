# Work queued for after the C++ is retired

The C++ encoder was the oracle only until the Rust reached parity (`design-inputs.md`). It was
retired in package X1: changes that alter output bytes no longer wait.
A change that alters them regenerates the golden vectors (`crates/tb-golden/vectors/README.md`) and is reviewed
with the gate (`tools/gate/README.md`). Each item names where the evidence is.

## Conformance gaps (from `format-conformance.md`, worst first)

Fixed items are ticked.

1. [x] Frame-rate code chosen from the numerator alone: 23.976 and 24 fps are labeled 60 Hz
   (`crates/tb-m2v-syntax/src/headers.rs`). Reduce the fraction, match H.262 Table 6-4
   exactly, refuse 25 and 50 Hz.
2. [x] No check of allowed formats when a session opens. Add one check against the A/53 Part 4
   and SCTE 43 format tables, with a terrestrial / 64-QAM / 256-QAM profile.
3. [x] The vector range code (f_code) can exceed the level limit at large search ranges. Refuse
   such ranges.
4. [x] The 4608-bits-per-macroblock limit (H.262 Table 8-6) is never checked.
5. [x] Fixed-quantizer mode writes an out-of-limit bit rate and can cut an oversized picture.
6. [x] Bit rate, buffer size and buffer delay are not checked against level, A/53, SCTE 43 or
   the 1-second transport limit.
7. [x] A frame flagged progressive in an interlaced session keeps field tools allowed, which
   A/53 §6.1.6 and SCTE 43 §7.1.6 forbid.
8. [x] The level code never says High-1440.
9. [x] Caption bytes are not checked for start-code emulation; the color description follows
   picture height, not the converter's matrix; only the test driver writes sequence end
   (both drivers write it).

### From the strict verifier (`m2v-conformance-findings.md`)

- Item 7 is not latent: the pipeline gate cases already emit it (617 pictures in 8 streams).
- Item 1 also breaks the buffer model, not only the display rate.
- Item 5: cutting an oversized picture removes whole macroblocks, leaving a gap in a slice.
- [x] Low priority: the test driver's CEA-608 pair order (fixed); a sequence header on
  repeated pictures during a long hold (decided against: the log has the reasoning); the
  pipeline's caption carrier in `tb-timeline` wrote field 1 before field 2 whatever the field
  order (fixed with film mode: `CaptionCfg::top_first`, `CaptionCarrier::take_shown`).
- [x] Put the verifier in the gate (done in X1, findings §8). The list of known findings
  (`known-findings.txt`) is gone, and `gate-allow.txt` allows nothing: every item is fixed.
- Item 7 also shows on the slate (`gate/pipe-ref-small`, found by the gate's verifier hook).
- Syntax the encoder never emits is unproven: before adding field pictures or coded
  `vbv_delay`, extend the verifier's stream set first (findings §5). Done for
  `repeat_first_field` before film mode used it (findings §6).

## Bugs found during the port, ported as they were

All fixed or decided:

- [x] Rate control: a same-code correction is accepted after the quantizer choice was already
  rewritten (port package R2 report). The pass's values are restored.
- [x] A pending rate change on a fixed-quantizer session is never applied (E4). Applied.
- [x] The P picture that replaces an I picture on the last ladder rung keeps its sequence
  header. Dropped: sequence headers stay on I pictures.
- [x] Texture bit count leaves out End of Block for intra blocks only (K1). Measured: neither
  uniform rule is better; kept, and the comment says so.
- [x] Out-of-buffer read in the level-2 motion search at ranges beyond about 60 (K1). The
  range is at most 30 (refused by the host, clamped by the kernel).
- [x] GOP time code does not use drop-frame at 29.97; slice numbering truncates above 2800
  lines (S1). Drop-frame at 29.97 Hz; such sizes are refused.

## Features the owner has asked for

- [x] Film through `repeat_first_field` (flagged pulldown): 1080i and 480i with 3:2, 720p59.94
  with frame repeats. Built: `docs/ARCHITECTURE.md` §7.6. Per channel `film=auto|off`.
  Left: cadence detection for sources that arrive hard-telecined (sketch in the log); the
  product's channel configuration and mux do not exist yet and must carry the switch and the
  timing rules of the log; 720p59.94 film mode stays off until it has been seen on the TVs.
- Subtitles to captions as its own component: burn-in first, true 608/708 after.

## Formats worth adding (legal, from `format-conformance.md`)

- Native 1080p23.976, 720p23.976, 720p29.97 (needs items 1 and 8).
- Cable-only widths: 1440×1080; 544, 528, 352 × 480. Open: SCTE 54-2024 §7.4 points MPEG-2
  video at A/53, which does not list them.
- AFD for letterboxed and pillarboxed pictures.

## Structure

- [x] Move the kernel sources out of the spike (`crates/tb-kernel-abi/kernels/`); delete the C++ host, its golden hooks
  and the comparison layer; regenerate golden vectors from Rust as regression snapshots (X1).
- [x] The device-owning loop (`Engine`) in a crate of its own (`tb-engine`, X1), the first step
  of the next item.
- Run the device-owning loop in a child process (a device fault poisons the process;
  `docs/ARCHITECTURE.md` §4.5).
- [x] Decide whether the timeline's drift correction gets a weak depth term (T1 report):
  it does (`MapperConfig::depth_tau`, ten minutes).

## Added 2026-11-26

- `tb-decode` test `a_stall_shifts_the_times` depends on wall-clock time and fails under
  build load (wants at least 149 ms, measured 148.2 ms). Make it take time as an input.
- `tb-pipe --offline --stall` waits forever for a frame a stalled decoder never delivers.
- Film mode: the harness does not flush the assembler at the end of a
  run; `film-480i-shed` skips its drift check because the reconstruction dump came out short
  under heavy shedding, cause not investigated; no real source switch between film and video
  in one channel is tested; cadence detection for hard-telecined sources is not built.
