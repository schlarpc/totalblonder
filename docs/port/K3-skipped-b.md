# K3 side check: skipped B macroblocks after a field-predicted macroblock

Verdict: **not a bug.** `k_ent_resolve` (`spikes/cuda-mpeg2/src/kernels.h`) is right as it is;
no patch. Checked 2026-11-25 on nixark with the oracle of branch `port/K3`.

## The suspicion (from K1)

`k_ent_resolve` skips a B macroblock when it is frame-predicted, has no coefficients, is not at
a row edge, has the left neighbor's mode and its vectors equal the vector predictors (walk to
the left; a field-predicted neighbor contributes its top-field vector with the vertical
component doubled). It does not look at the left neighbor's `field_pred`. If a decoder
predicted a skipped macroblock the way the previous macroblock was predicted (by fields),
encoder and decoder would differ after every field-predicted macroblock.

## What H.262 says

- 7.6.6.4 (B frame picture), for a skipped macroblock: "the prediction shall be made as if
  frame_motion_type is 'Frame-based'"; the direction (forward / backward / bidirectional) is
  the previous macroblock's; the motion vector predictors are unaffected; the vectors are taken
  directly from the predictors. No field selects take part.
- 7.6.3.1: after a field vector in a frame picture the predictor holds
  `PMV[r][s][1] = vector'[r][s][1] * 2`; Table 7-9: a field-based macroblock updates no other
  predictor. So after a field-predicted macroblock, a skipped one is predicted frame-based with
  `PMV[0][s]` = (top-field horizontal, 2 x top-field vertical), per used direction.
- 6.3.17.1 / Table 6-17: `frame_motion_type` is present only in coded macroblocks; a skipped
  macroblock carries none, hence the "as if Frame-based" of 7.6.6.4.

This is exactly what the encoder assumes (`skip` requires `!mi.field_pred`; `py = l.field_pred ?
l.mv[0][d][1] * 2 : l.mv[0][d][1]` is 7.6.3.1).

## What the decoders do (source of the gate's builds)

- ffmpeg (`libavcodec/mpeg12dec.c`, the gate's ffmpeg): in the slice loop, when a skip run is
  read, `mv_type = MV_TYPE_16X16` for frame pictures and, for B pictures,
  `mv[d][0] = last_mv[d][0]` (its predictors; field vectors are stored doubled). The
  `mb_type | MB_TYPE_SKIP` copy of the previous macroblock in `mpeg_decode_mb` is
  bookkeeping (debug output, error concealment), not the prediction.
- libmpeg2 0.5.1 (`libmpeg2/slice.c`): skipped B macroblocks of frame pictures go through
  `motion_reuse_420`, a 16-line frame prediction from `pmv[0]`.

## Measurement

- How often: the 43 interlaced streams (480i, 1080i) that `check-full` writes were decoded
  with `ffmpeg -debug:v mb_type` (a skipped macroblock inherits the previous macroblock's
  type, so a skip in a run that follows a field-predicted macroblock shows the interlaced
  mark): 4,898,872 skipped of 17,619,800 B macroblocks; **2,233 runs** (4,526 macroblocks)
  start right after a field-predicted macroblock. All but five streams have them (the five:
  quarter-rate crowd_run, noise and park_joy 1080i, quarter-rate noise 480i, the 480i
  football rate walk); e.g. testsrc2 480i: 97 runs, 839 macroblocks; pedestrian_area 1080i:
  151 runs, 383 macroblocks.
- Whether the two readings differ there: an instrumented debug build of the oracle (counter in
  `k_ent_resolve`, not committed) checked, at each such skip, whether a field-based reuse of
  the neighbor's vectors and field selects would predict the same samples as the frame-based
  `PMV[0]` prediction (both field vectors equal, same-parity selects, even vertical). It would
  not in 491 of 495 cases (testsrc2 97 / 97, cuts 20 / 22, pedestrian_area 151 / 153,
  crowd_run 5 / 5, park_joy 1080i 218 / 218; counts include the correction passes): the test
  below therefore tells the two readings apart.
- Visible effect: none. `check.py --keep` on 480i-testsrc2, 480i-cuts and
  1080i-pedestrian_area, then the largest sample difference per macroblock (Y, Cb, Cr) between
  the encoder's reconstruction and each decoder, at the 1,267 affected macroblocks of those
  streams: 0 or 1 everywhere against ffmpeg and against libmpeg2 (e.g. testsrc2: 743 x 0,
  96 x 1 against ffmpeg), the same as the other macroblocks of those pictures (0 to 2, the
  IDCT mismatch that drifts in from the references). A field-based reading would have
  predicted these macroblocks from other samples; the gate's drift figures (65 to 70 dB) and
  these per-macroblock figures show neither decoder does.

## Fix

None needed. The streams stay as they are; K1's entropy tests (`crates/tb-device-ref/tests/
entropy.rs`) model the same rule and need no change.
