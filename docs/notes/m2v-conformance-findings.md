# MPEG-2 conformance: what the verifier finds in our encoder's streams

> **Status.** This is the audit as written on 2026-11-25, before the fixes. Every finding of
> §3 and §4 has since been fixed or is refused when a session opens; the list `tools/known-findings.txt` no longer exists and
> `tools/gate-allow.txt` allows nothing.

Audit of 2026-11-25 with `tb-m2v-verify` (crates/tb-m2v-verify), the verifier written from the
standards' text and not from the encoder. The rule catalog is `m2v-conformance-rules.md`;
rule identifiers below are its IDs. Severity: S1 a decoder may misdecode or reject, S2 illegal
per H.262 but commonly tolerated, S3 legal H.262 but outside A/53 Part 4 / SCTE 43 / CTA-708
(or the operating model), S4 recommendation or note.

Statements about what real decoders do are **my reasoning**, marked [R]; none was tested on a
TV. File and line references are to the tree at the audit date; the kernels were then in the
C++ spike's source directory and are `crates/tb-kernel-abi/kernels/` now.

## 1. Summary

- **The 298 stored gate streams** (the C++ oracle's, byte-identical to the Rust encoder's for
  the m2venc cases; 896 MB, 30 123 pictures, 100.8 million coded macroblocks, 761 million
  coefficients) have **no S1 and no S2 finding**, under any profile. Every rule of H.262 that
  those streams exercise holds on the output, including the ones the code-reading audit
  (`format-conformance.md`) listed as "correct by construction, not asserted": vectors inside
  the picture (half-sample positions and chroma vectors included), predictor resets, skipped
  macroblock rules, DC range, escape legality, slice coverage, temporal_reference, repeat
  header identity, and the VBV of Annex C.3.2. The project's operating model also holds on
  all of them at the declared `bit_rate`.
- They have **two S3 findings**: `BC-PROGRESSIVE-FPFD` in 8 pipeline streams (617 pictures),
  which the earlier audit called latent, and `BC-FORMAT` in 8 crop-test streams (a picture
  size that no table has; a test input, not a product format). And three S4 notes.
- **Streams made here on the CPU reference adapter** in the configurations the encoder is
  meant for (27 streams, `tools/ref_cases.sh`, group "clean") are also clean.
- **Configurations that the encoder accepts but should not** (group "known", 12 streams)
  produce S1 and S2 streams. All are reachable only by options or inputs that today's gate
  does not use, and all map to an existing entry of `post-port-worklist.md`. The verifier
  shows consequences that reading the code did not: a 24 fps session is not only mislabeled
  but **underflows the VBV**, and a fixed-quantizer picture cut at the size limit **loses
  macroblocks** (`SLICE-GAP`).
- A large part of the catalog is **never exercised** by anything our encoder emits (§5);
  conformance there is unproven, but those are tools the encoder does not have.

## 2. What was run

| Set | Streams | How | Result |
|---|---|---|---|
| Stored gate streams, `work/streams/nixark/out/{check,pipecheck}` | 298 of 302 (the 4 `*.ref.m2v` are another encoder's reference encodes: three empty, one cut off; excluded) | `--profile h262-only`, `a53`, `scte43-64qam`, `scte43-256qam`, each with `--delivery operating` | 0 S1, 0 S2 in all four runs |
| Reference adapter, clean group | 27 (176x144 and 352x240; CBR at several rates, starved, IPP, intra only, GOP shapes, AQ off, intra_vlc 0, both scans, ranges, forced keys, rate plan, rate walk, captions, interlaced with and without field tools, bottom field first, two sessions) | `--delivery operating --fail-on S4` | no finding at all under H.262 |
| Reference adapter, known group | 12 (+ 2 probes) | `--delivery operating` | see §4 |
| Independent encoders | 26 ffmpeg mpeg2video configurations, 7 mjpegtools mpeg2enc | `tools/independent.py` | all decode; deviations in §6 |

Speed: the 298 streams take 18 s on one core (about 50 MB/s), so the verifier can run on
every gate stream.

Syntax our streams use (from the verifier's feature counts over the 298): I, P and B frame
pictures only; closed GOPs with a sequence header on every I-picture (1448 repeat headers);
`intra_dc_precision` 8; `q_scale_type` 1; `intra_vlc_format` 1; both scans; field prediction
(10.1 million macroblocks) and field DCT (8.9 million) in frame pictures; `macroblock_quant`
(40 million); skipped macroblocks in P (7.4 million) and B (20.4 million); intra macroblocks
in P and B; 5.7 million escapes; `macroblock_escape`; captions in 2 streams. Maxima over all
streams: macroblock 3460 bits (limit 4608), |level| 766, vector 54.5 samples horizontal and
35.5 vertical.

## 3. Findings in the stored gate streams

### 3.1 `BC-PROGRESSIVE-FPFD` (S3): progressive_frame = 1 with frame_pred_frame_dct = 0

- **Rule.** A/53 Part 4 §6.1.6 (p.11) and SCTE 43 §7.1.6 (p.4): frame_pred_frame_dct shall be
  '1' if progressive_frame is '1'. H.262 itself requires this only in progressive sequences.
- **Count.** 617 pictures in 8 streams, every picture of each:
  `pipe-1080p24-1080i-pulldown.0`, `pipe-1080p30-1080i-psf.0`, `pipe-1080p-480i-crop.0`,
  `pipe-1080p-480i-letterbox.0`, `pipe-720p50-1080i.0`, `pipe-854x480-1080i.0`,
  `pipe-av1-1080i.0`, `pipe-mosaic-2x2-overlay.0`. First: `pipe-1080p24-1080i-pulldown.0.m2v`
  byte 42, picture 0.
- **New fact.** `format-conformance.md` gap 8 calls this latent ("device input path only; the
  CLI refuses device input"). The stored pipeline cases of the oracle do emit it: every
  interlaced output session fed from a progressive source.
- **Do decoders care [R].** Decoding is unaffected: the syntax carries frame_motion_type and
  dct_type per macroblock and decoders follow frame_pred_frame_dct, not progressive_frame.
  progressive_frame steers the display side (chroma upsampling of 4:2:0, the deinterlacer's
  weave or bob decision). The pictures really are progressive, so the flag is truthful and a
  TV should show them correctly. The cost is that the encoder may still pick field tools on
  progressive content, which a broadcast analyzer reports and which wastes bits. Risk on a
  TV: low.
- **Root cause.** crates/tb-m2v-encoder/src/tick/session.rs:160 sets
  `frame_pred_frame_dct: geo.interlaced == 0` (a property of the batch geometry) and :163
  `progressive_frame: f.progressive` (a property of the frame);
  crates/tb-m2v-syntax/src/headers.rs:211-213 writes progressive_frame from it.
- **Minimal fix.** Worklist item 7. Either write progressive_frame = 0 in interlaced sessions
  until there is a per-picture frame-only mode (one line in session.rs), or make
  frame_pred_frame_dct per picture and have the kernels use frame prediction and frame DCT
  for such pictures (the real fix, and the one the film work needs).

### 3.2 `BC-FORMAT` (S3): a picture size outside the format tables

- 8 streams `devin-crop-{dev,host}.{0..3}`: 696x470, aspect 3, frame_rate_code 4, interlaced.
  Not a row of A/53 Part 4 Table 6.2 (p.9) or SCTE 43 Table 3 (p.3).
- A test of cropped input, not a format we would send. It shows that nothing refuses the size
  (crates/tb-m2v-encoder/src/layout.rs:70-73 checks only non-zero and even). Worklist item 2.
- [R] A TV would very likely decode it (it is legal MP@ML) and scale it somehow; cable-ready
  sets are only obliged to handle the table formats.

### 3.3 Notes (S4)

| Rule | Count | What | Cause, fix |
|---|---|---|---|
| `CC-608-ORDER` | 150 pictures, `1080i-pedestrian_area-cc` (also `i-cc` on the reference adapter) | CEA-608 pairs in the order field 1, field 2 (cc_type 00, 01) on pictures with top_field_first = 1. CTA-708-E Annex C (p.99, informative) and §4.4.2 (p.19) tie cc_type 01 to the top field, so it would come first. | The test driver's synthetic captions, crates/tb-m2venc/src/run.rs:271-290 (always 0xFC then 0xFD). The encoder copies caller bytes. [R] Decoders sort the pairs by cc_type into the two line-21 fields; the order within one cc_data() matters only for the 608 timing of a frame, and field-1-first is what most encoders in the field emit. Fix belongs to whoever builds cc_data() for the product. |
| `BC-COLOR-PREFERRED` | 696 headers in 120 streams (all 480-line streams) | Color description SMPTE 170M (6, 6, 6); SCTE 43 §7.1.4 (p.4) prefers BT.709 for all formats. Legal. | headers.rs:149 (`if h > 576 { 1 } else { 6 }`). No fix needed if the converter really uses the 170M matrix for SD (worklist item 9). |
| `BC-SEQ-INTERVAL` | 13 intervals in 6 streams (`hold-*`, `shed50-*`, `shed90-*`), up to 1.5 s | More than 0.6 s of video between sequence headers, while pictures are repeated under hold or shedding. No rule; SCTE 54 §9.2 (informative, p.25) suggests 0.5 s. | The repeated pictures are P-pictures and carry no headers. [R] Channel change onto such a stream waits for the next real I-picture. Consider a sequence header on a repeated picture at the key interval, if a hold can last long. |

The four profile runs differ only in `BC-COLOR-PREFERRED` (SCTE 43 only). No stream exceeds
`bit_rate_value` 48500 (A/53), so all rate ceilings in the stored set fit even the terrestrial
limit, and none has a buffer above 488 units.

## 4. Findings in configurations the encoder accepts (reference adapter, group "known")

Each is produced by `tools/ref_cases.sh` and allowed in the flake check `verify/ref-streams`
by `tools/known-findings.txt`. "Item n" is the entry of `post-port-worklist.md`, section
"Conformance gaps". Ranked by severity.

| # | Rule (sev) | Count, stream, first position | What happens | Do decoders care [R] | Root cause | Minimal fix |
|---|---|---|---|---|---|---|
| 1 | `VBV-UNDERFLOW` (S1), `PL-FRAME-RATE` (S2) | 15 pictures each in `fps-23976`, `fps-24`; first at byte 35031, picture 15. `PL-FRAME-RATE` once per stream, byte 0 | 23.976 and 24 fps sessions are written with frame_rate_code 8 (60 Hz). Main level has no code above 5, and the VBV, examined 60 times a second, has the second GOP's I-picture 36 672 bits short at its decoding time. | Yes. A decoder paces display from the frame rate code when no transport timing overrides it; in a transport stream DTS and PTS (from the real rate) disagree with the stream's own rate, and a set that trusts the header shows 2.5x speed or stutters. | crates/tb-m2v-syntax/src/headers.rs:86-95: the code is chosen from `fps_n` alone, "anything else -> 8". | Item 1: reduce the fraction, match Table 6-4 exactly, refuse the rest. |
| 2 | `SLICE-GAP` (S1), `MB-BITS` (S2), `VBV-UNDERFLOW` (S1) | `q-fixed1` (352x240, quantizer 1): 19 of 20 pictures lose their tail (first: picture 1, byte 119532, macroblocks 212 to 329 missing); 2188 macroblocks above 4608 bits beyond the two allowed per row (first: byte 121350, macroblock 3, 4841 bits); 18 underflows | A fixed-quantizer picture larger than w x h x 3/2 bytes is cut at whole macroblocks; the last slice ends early and the following rows are absent. The header claims 15 Mbit/s and the Main level buffer, which the pictures do not fit. | Yes for the cut: a decoder conceals the missing rows with whatever it has (the gate's software decoders report it as damage). The 4608-bit limit is rarely enforced by decoders, but a hardware decoder's per-macroblock input buffer is sized by it. | crates/tb-m2v-encoder/src/tick/batch.rs:1052 and :1170 (`.min(cap)`); no check of `d_mbbits` (layout.rs:302) anywhere. | Items 4 and 5: make the cut an error; count macroblocks above the limit per row and re-code. Fixed quantizer is a test mode: keep it out of product builds. |
| 3 | `SEQ-HSIZE-ZERO` (S1), `PL-SIZE` (S2) | `wide-4096`, byte 0 | A width of 4096 passes the size check and is written as horizontal_size_value 0 with horizontal_size_extension 1; H.262 §6.3.3 (p.38) forbids the zero. | Yes: twelve zero bits after the start code lengthen a zero run next to it, and decoders that ignore the size extension see width 0. | layout.rs:83-86 allows rows of exactly 256 macroblocks; headers.rs:123 writes `w & 0xfff`. | Item 2: a format whitelist at open. |
| 4 | `PL-FCODE` (S2), `MV-LEVEL-VERTICAL` (S2) | `range-40`: 46 pictures with f_code 6, and 2 vectors of +128.0 samples vertical (byte 6161, picture 1, macroblock 0); `range-24-i`: 36 pictures with f_code 6 | Search range 40 (progressive) or 24 (interlaced) gives f_code[s][1] = 6; Table 8-8 (p.112) allows 5 in frame pictures at every level. With it the search really produces a vector outside [-128 : 127.5]. | Possibly. Software decoders accept f_code up to 9. A hardware decoder sized for the level may fetch its reference window for at most 128 lines of vertical reach. | crates/tb-m2v-encoder/src/layout.rs:95-102: one f_code for all four entries, grown until it covers the search reach, capped at 9. | Item 3: refuse ranges that need a vertical f_code above 5 (or split horizontal and vertical f_codes and clamp the vertical search). |
| 5 | `PL-SAMPLE-RATE` (S2), `PL-RFF-MAIN-LEVEL` (S2) | `ml-576-30`, byte 0 | 720x576 at 29.97 Hz is labeled MP@ML: 12.4 Msample/s against 10.368, and Main level above 480 lines is 25 Hz only (§8.2.1, p.111). | Rarely: decoders seldom check the level against the size. A Main-level-only decoder would be overrun. | headers.rs:33-37: `high_level()` knows only "above 720x576 or above 30.5 fps". | Item 8: compute the level from Tables 8-8, 8-11, 8-12 (Low, Main, High-1440, High). |
| 6 | `PL-BIT-RATE` (S2), `PL-VBV-SIZE` (S2) | `ml-20mbit` (20 Mbit/s at Main level), `ml-vbv-4m` (4 014 080-bit buffer at Main level; also `BC-BUFFER-DELAY`, 1.004 s) | The command line values go into the header unchecked. | The buffer size matters: a decoder allocates what the level needs, and the multiplexer sizes the T-STD from the level. | crates/tb-m2venc/src/opts.rs:482-485 (default only); headers.rs:116-121. | Item 6: check against the level and the plant profile at open. |

Probes outside the two groups: `--fps 50/1` at 176x144 is labeled High level (legal, but
`BC-LEVEL-LOWEST`: High-1440 would do); every small test picture is labeled Main where Low
would do (`BC-LEVEL-LOWEST`, S3, only under a broadcast profile, and no broadcast format is
that small). Fixed quantizer 4 and 31 give legal H.262 streams at these sizes.

## 5. Rules our streams never exercise

Conformance to these is unproven by the audit, because the syntax never occurs in anything
the encoder emits (stored set and reference adapter). The verifier's own tests and the
independent encoders exercise them (§6, and `tests/rules.rs`).

| Not in our streams | Rules that therefore never ran on our output |
|---|---|
| Field pictures | `PCE-TFF-FIELD`, `PCE-FPFD-FIELD`, `PCE-RFF-FIELD`, `PCE-PROGRESSIVE-FRAME-FIELD`, `PCE-FIELD-PARITY`, `PCE-FIELD-TYPE`, `PIC-TEMPORAL-REF-FIELDS`, `SEQ-HDR-BETWEEN-FIELDS`, `SEQ-END-COMPLETE`, `MB-PFIELD-AFTER-IFIELD`, the field-picture limits of `PL-FCODE` and `MV-LEVEL-VERTICAL`, 16x8 prediction in `MV-OUTSIDE` |
| repeat_first_field (until film mode; our streams exercise the family since then, and the verifier's rules were confirmed on other encoders first, §6) | `PL-RFF-MAIN-LEVEL` (B-picture part: no format we accept can reach it), `PCE-COMPOSITE-RFF`, cc_count 25 in `CC-COUNT` |
| Dual prime | `MB-DUAL-PRIME`, `MB-DUAL-PRIME-B-BETWEEN`, the derived vectors in `MV-OUTSIDE` and `MV-LEVEL-VERTICAL` |
| Concealment motion vectors | `MB-CONCEAL-MARKER`, the concealment branch of `PCE-FCODE-UNUSED` |
| Quantizer matrices in the stream | `QM-ZERO`, `QM-INTRA-DC`, `QMX-CHROMA`; the matrices allowed to differ in `SEQ-REPEAT-DIFF` |
| vbv_delay other than 0xFFFF | `VBV-DELAY-RATE`, `VBV-OVERFLOW` (C.3.1; C.3.2 cannot overflow), `PIC-VBV-DELAY-MIX`, `BC-VBV-DELAY` |
| B-pictures right after an I-picture (the frame before a key frame is always a P-picture), open GOPs, broken_link | `GOP-CLOSED-PRED`, `GOP-OPEN-START`, `GOP-BROKEN-LINK` |
| Drop-frame time code | `GOP-DROP-FRAME` (the time code counts 30 per second at 29.97 Hz, which is legal) |
| low_delay = 1 | `SEQX-LOW-DELAY-B`, Annex C.10 and C.12 |
| Other extensions (picture display, copyright, quant matrix, scalable, camera, ITU-T) | `PDX-NO-SDE`, `PDX-MARKER`, `CPX-MARKER`, `CPX-RESERVED`, `ORD-EXT-DUP`, `PL-SCALABLE` |
| Slice extension bits, several slices per row, extra information | `SLICE-INTRA-FLAG`, `SLICE-PICTURE-ID`, `SLICE-EXTRA-INFO`, `PIC-EXTRA-INFO`; `SLICE-ORDER` within a row |
| intra_dc_precision 9 to 11, linear quantizer (q_scale_type 0) | the other ranges of `BLK-DC-RANGE`; `PL-INTRA-DC`; Table 7-6 column 0 |
| 4:2:2 and 4:4:4 | `PL-CHROMA`; coded_block_pattern_1 and _2; the chroma matrices |
| Macroblocks above 4608 bits under rate control | the "two per row" allowance of `MB-BITS` (largest macroblock in the 298 streams: 3460 bits; this answers open question 5 of `format-conformance.md` for the gate's material: the limit was never approached) |
| Coefficients near the saturation | `BLK-SATURATION` (largest level 766) |
| More than 1024 frames between GOP headers | the wrap of `PIC-TEMPORAL-REF` |
| More than one sequence in a stream, parameters changing | `SEQ-END-PLACEMENT`, sequence restart of the VBV; the test driver ends every stream with one sequence_end_code |
| User data after sequence or GOP headers; AFD, bar data, SCTE 20; DTVCC packets | `UD-PLACEMENT`, `UD-ORDER`, `UD-DUP-TYPE`, `UD-SCTE20-FORMAT`, `AFD-*`, `BAR-*`, `CC-STATE` (the synthetic captions carry CEA-608 pairs and padding only) |
| Frame rate codes other than 4 and 7 in a legal way; Low and High-1440 level | `PL-FRAME-RATE` on a correct stream; the 23.976 and 24 Hz rows of the format tables |
| vertical_size above 2800 | slice_vertical_position_extension |

## 6. Independent encoders (calibration)

All streams of ffmpeg's mpeg2video (6.1 and 9.0) and mjpegtools' mpeg2enc (2.2.1) decode in
the verifier without a syntax finding, which cross-checks the tables generated from Annex B
(10.7 million coefficients, 219 000 escapes, both DCT tables, both scans, both quantizer
scales, custom matrices, intra_dc_precision 8 to 11, 4:2:2, repeat_first_field, coded
vbv_delay, open and closed GOPs, several slices per row, vectors of several hundred
samples).

Where they trip a rule, the clause was read again; in every case the encoder is wrong, not
the rule:

| Encoder, configuration | Rule | What it writes | Verdict |
|---|---|---|---|
| ffmpeg, raw elementary stream output (all) | `SEQ-END-MISSING` (S4) | no sequence_end_code at the end | The syntax of 6.2.2 ends a sequence with it; every decoder treats end of data the same way. Note level is right. Our test driver writes it. |
| ffmpeg without `-maxrate` | `PL-BIT-RATE` (S2) | bit_rate_value 0x3FFFF (104.8 Mbit/s) as "variable rate", above every level | Widely tolerated: this is what most ffmpeg-made MPEG-2 files carry. Confirms S2. |
| ffmpeg constant quantizer without `-bufsize` | `VBV-PICTURE-SIZE` (S1) | vbv_buffer_size from its default bit rate (49 152 bits), pictures of 140 kbit | Software players ignore the VBV; a broadcast chain would not. The rule is right; the severity describes hardware. |
| ffmpeg 1280x720 at 59.94 Hz | `PL-SAMPLE-RATE` (S2) | High-1440 level; the luminance rate (55.2 Msample/s) needs High | ffmpeg picks the level from the size alone. Tolerated. Our encoder gets this one right. |
| ffmpeg 704x480 at 59.94 Hz | `PL-FRAME-RATE`, `PL-SAMPLE-RATE` (S2) | Main level | Same cause. |
| ffmpeg `-dc 11` | `PL-INTRA-DC` (S2) | 11-bit DC precision in Main profile | High profile only (Table 8-5). |
| ffmpeg 4:2:2 | `PL-PROFILE` (S4) | 4:2:2 profile | Not an error; the verifier's limits are Main profile's. |
| mpeg2enc `-p` (3:2 pulldown) with interlaced frames | `PCE-RFF-INTERLACED` (S2) | repeat_first_field = 1 with progressive_frame = 0 | §6.3.10 (p.50) forbids it; players tolerate it. With `-I 0` mpeg2enc writes it correctly. |
| mpeg2enc `-p -I 1` on a source it then forces progressive | `PCE-FPFD-PROGRESSIVE`, `SEQX-PROGRESSIVE` (S2) | progressive_sequence = 1 with frame_pred_frame_dct = 0 and progressive_frame = 0 | An odd corner of its option handling (not in the committed cases). |

**repeat_first_field (added with film mode).** The family was
extended before the encoder used the flag. Independent streams: x262 `--mpeg2 --pulldown 32`
(the MPEG-2 branch of x264, built from source by `tools/x262.nix`, run by hand with
`independent.py --x262`: correct soft telecine with adaptive B-pictures and closed GOPs, the
same cadence our encoder writes, no finding but the time code note), mpeg2enc `-p` in six
configurations (frames marked progressive: a correct 3:2 soft telecine, with and without
B-pictures, constant rate, a noisy source at a binding rate; frames marked interlaced and its
"ATSC 1080i" format: violations), mencoder's soft telecine (correct without B-pictures; with
them the flags follow the coded order and the output fields do not alternate), and streams
of ffmpeg whose flags `tools/esflags.py` rewrites the way DGPulldown does (480i and 1080i 3:2,
mixed patterns, 720p59.94 with frames shown two and three times, with and without caption
user data). ffmpeg's own mpeg2video does not write the flag (9.0.2: frames with repeat_pict
come out with repeat_first_field 0). What they showed:

| Finding | Evidence | Change |
|---|---|---|
| The verifier had no rule for H.262 7.12, "the fields at the output of the decoding process shall always be alternately top and bottom" | mencoder with B-pictures: 26 parity breaks in 40 pictures, no finding; two hand-built streams of `tests/rules.rs` broke it unnoticed | new rule `OUT-FIELD-PARITY` (S2); the count agrees with `esflags.py list` on every stream |
| `GOP-TIMECODE-CONTINUITY` was skipped after any repeat_first_field and for every drop-frame time code (all our 29.97 Hz streams) | reading the code | checked always; coded frames or display time both accepted (6.3.8 does not say) |
| The allowance `PCE-RFF-INTERLACED` for mpeg2enc `-p -I 0` was stale: that stream is correct | `esflags.py list`, ffprobe | allowance removed |

Mutations of those streams (one violation each, written by `esflags.py mutate`), each reported
by its rule and by nothing else beyond what may come with it: repeat_first_field cleared and
top_field_first inverted (`OUT-FIELD-PARITY`), progressive_frame cleared under
repeat_first_field (`PCE-RFF-INTERLACED`), frame_rate_code 1 (`PL-RFF`, interlaced and
progressive), top_field_first without repeat_first_field in a progressive sequence
(`PCE-TFF-PROGRESSIVE`), temporal_reference (`PIC-TEMPORAL-REF`), every picture two fields
long in a stream coded for 3:2 at a binding rate (`VBV-UNDERFLOW`: the buffer holds only with
the three-field removal intervals of C.11), cc_count 10 in a three-field picture (`CC-COUNT`).
Not obtainable: an independent stream with repeat_first_field *and* coded vbv_delay (mpeg2enc
writes 0xFFFF in its constant-rate mode too), so the intervals of C.9 and C.11 are confirmed
through C.3.2 (underflow) and by reading, not through the input rates of C.3.1 (x262 writes
0xFFFF as well; its 59.94 Hz progressive pulldown modes give no MPEG-2 output).

Not obtained: field pictures from an independent encoder (ffmpeg has none; mpeg2enc `-I 2`
wrote an empty file here) (x262 was obtained later, for the repeat_first_field work above). Field pictures, dual prime, 16x8 and
concealment vectors are covered only by the hand-built streams of `tests/rules.rs`.

**How much a damaged stream shows.** A single flipped bit in the slice data of a real stream
(300 random positions each in three of our streams) is reported in 45 to 50 % of the cases,
as `BLK-OVERRUN`, `SLICE-ROW-OVERRUN`, `MB-ADDR-VLC`, `MV-OUTSIDE`, `BLK-SATURATION` and the
like. The rest turn one valid stream into another (a sign bit, a DC differential, a residual
bit of a vector) and cannot be seen without the source pictures.

The buffer check is as sensitive: `480i-football` (4 Mbit/s, 112 units) passes as coded, and
with `bit_rate_value` lowered by 10 % in every header it underflows at 8 pictures (45 at
-25 %); the operating model at 0.9 of the declared rate fails at 10 pictures. So the encoder
uses its declared rate closely, and the verifier's model is not slack.

## 7. What the verifier cannot check from the elementary stream

- **Mismatch control and the IDCT** (7.4.4, Annex A): decoder processes. Whether the
  encoder's reconstruction matches shows only as drift against a decoder, which the gate's
  reconstruction-against-decode comparison covers.
- **Sample content**: that the bottom 8 lines of a 1088-line frame are black (A/53) or
  whether progressive_frame, the color description and AFD describe the pictures truthfully.
- **Whether `bit_rate` really bounds the delivery rate, and the operating model under a rate
  that changes**: the verifier checks the model at one constant rate (`--delivery
  operating[:RATE]`); the session's rate tickets are not in the stream.
- **Transport**: PES alignment, PTS and DTS, the T-STD and its one-second limit (only the
  quotient buffer / bit_rate is reported, `BC-BUFFER-DELAY`), video_stream_descriptor, still
  pictures.
- **Caption content**: the caption channel packet layer (sequence numbers, packet sizes,
  service blocks) and CEA-608 byte parity and protocol.
- **Legality of the stream cut at every repeat header** (6.1.1.6): only its parts (first
  picture not B, closed GOP predictions) are checked.
- **The last picture of a low-delay sequence being a big picture** (C.7).
- Rules marked *implied* in the catalog: the syntax cannot express a violation, or it shows
  as another rule (for example a wrong predictor reset in the encoder shows as `MV-OUTSIDE`,
  `BLK-DC-RANGE` or as decode drift, never by itself).

## 8. Running it

    # one stream or a directory tree of *.m2v (also *.mpv, *.mpg2)
    nix run .#verify-streams -- --profile scte43-256qam --delivery operating --quiet DIR
    # machine-readable
    nix run .#verify-streams -- --json --profile a53 DIR > report.json
    # in the dev shell
    nix develop -c cargo run --release -p tb-m2v-verify -- --details FILE.m2v

Exit status 0: no finding at or above `--fail-on` (default S2) outside `--allow` and
`--allow-file`; 1: findings; 2: usage or I/O error. `--details` prints every kept finding
with rule, clause, page, byte.bit offset, picture number in coded and display order, slice
and macroblock address.

**Flake checks** (in `nix flake check`): `verify/ref-streams` (our encoder on the reference
adapter: the clean group with no finding at all, the known group with exactly the rules of
`crates/tb-m2v-verify/tools/known-findings.txt`), `verify/independent` (ffmpeg and
mpeg2enc), `verify/rules-catalog` (this catalog file against the binary).

**In the gate** (applied in package X1). Every coded stream that a
case of the gate writes goes through the verifier: `conform.py` of the gate's machinery runs
`tb-m2v-verify --json --delivery operating` (binary from `M2V_VERIFY`, set by `nix/gate.nix`) on
the streams each case wrote, with the profile of the case: `scte43-256qam` for the broadcast
formats of `check-full` and `check-pipe` and for the pipeline's small sandbox gate (480i output),
`h262-only` for the 176x144 test pictures of the small encoder gate. Any S1 or S2 finding fails
the case; an S3 finding fails it unless `crates/tb-m2v-verify/tools/gate-allow.txt` allows that
rule for that stream (rule, globs on the stream's file name, worklist entry); S4 notes are
reported and never fail. The allowed findings of today: `BC-PROGRESSIVE-FPFD` on the pipeline
streams with progressive sources and on the slate (item 7; the slate was found by the hook, in
`gate/pipe-ref-small`), `BC-FORMAT` on the cropped-input test (item 2). The file's format (globs
after the rule) is the harness's; `--allow-file` of the verifier reads only rule identifiers and
is not used with it.

## 9. Proposed additions to the worklist (not made here)

- Item 7 is not latent: the pipeline cases emit progressive_frame = 1 with
  frame_pred_frame_dct = 0 (§3.1).
- Item 1 has a second consequence: the stream violates the VBV, not only the display rate.
- Item 5: the cut of an oversized picture removes macroblocks (`SLICE-GAP`), in whole
  macroblocks.
- New, low priority: the CEA-608 pair order of the synthetic captions (§3.3); a sequence
  header on repeated pictures during a long hold (§3.3).
- Once the encoder may change: drop lines from `known-findings.txt` and `gate-allow.txt` as
  items are fixed (the flake check fails when a listed rule of `known-findings.txt` stops
  occurring; the pipeline gate prints `gate-allow.txt` entries that matched nothing). The
  verifier runs in the gate since X1 (§8).
