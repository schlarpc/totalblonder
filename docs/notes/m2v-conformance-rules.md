# MPEG-2 video elementary stream: the conformance rule catalog

Every normative constraint on an MPEG-2 video elementary stream that the verifier
`tb-m2v-verify` (crates/tb-m2v-verify) knows, extracted from the text of the standards, not
from any encoder. The catalog is data in the verifier (`src/rules.rs`); everything below
the marker line is printed by `tb-m2v-verify --rules-md`, and the flake check
`verify/rules-catalog` fails if this file and the binary disagree. To change a rule, edit
`src/rules.rs` and regenerate:

    nix develop -c cargo run -q -p tb-m2v-verify -- --rules-md > /tmp/rules.md
    # then replace everything below the marker line with /tmp/rules.md

**Sources** (text extractions in work/standards/ of the main checkout; page numbers are the
printed ones):

- ITU-T H.262 (02/2000) = ISO/IEC 13818-2:2000: clauses 5.2.3, 6.1 to 6.3, 7.2 to 7.6, 8,
  Annexes B and C.
- ATSC A/53 Part 4:2009 ("A/53-4"): clause 6.
- ANSI/SCTE 43 2015 (R2021): clause 7.
- CTA-708-E S-2023: clauses 4.3 and 4.4, Annexes B, C and E.
- ATSC A/53 Part 3:2013, SCTE 54 2024, ITU-T H.222.0 (06/2012): only where they reach into
  the video elementary stream.

**Columns.**

- *ID*: stable identifier; the verifier's findings, its JSON, `--allow` and the allowlist
  file use it.
- *Sev*: S1 a decoder may misdecode or reject the stream; S2 illegal per H.262 but commonly
  tolerated; S3 legal H.262 but outside A/53 Part 4, SCTE 43 or CTA-708 (or the project's
  operating model); S4 a recommendation or a legal construct worth knowing about.
- *Checked*: **yes** (a detecting test exists for every such rule, `tests/rules.rs`);
  *implied* (the syntax cannot express a violation, or a violation necessarily shows as
  another rule: the text says which); **no** (not checkable from the elementary stream alone,
  with the reason).

**Scopes.** stream (start codes, stuffing, order of structures), sequence, GOP, picture,
slice, macroblock (with motion vectors), block, cross-picture, buffer model, profile/level
(H.262 clause 8, Main and Simple profile), broadcast profile (selected with `--profile a53`,
`scte43-64qam` or `scte43-256qam`; not applied with `h262-only`).

**What the text does not say, though it is often assumed** (checked while extracting):

- An escape code for a (run, level) pair that has a variable length code is *not* forbidden
  by H.262 7.2.2.3 (it was in MPEG-1 practice only by convention). `BLK-ESCAPE-REDUNDANT` is
  a note.
- Saturation and mismatch control (7.4.3, 7.4.4) are decoder processes; no rule forbids a
  stream whose coefficients saturate. `BLK-SATURATION` is a note; mismatch is not checkable.
- There is no rule on the interval between I-pictures, GOP headers or sequence headers in
  H.262, A/53 Part 4 or SCTE 43. SCTE 54 clause 9.2 (informative) suggests a sequence header
  every 0.5 s. `BC-SEQ-INTERVAL` is a note.
- Zero stuffing after user data is user data (6.2.2.2.2 reads bytes until a start code
  prefix), so three zero bytes after user data break 6.3.4.1. `UD-ZERO-RUN` reports it.
- The forbidden and marker values exist to keep 23 zero bits out of the stream; the tests
  show the pairs: two escapes with level 0, a quantizer matrix with three zeros, a zero time
  code without its marker bit and caption constructs without marker bits each give
  `SC-EMULATION` (or `UD-ZERO-RUN`) together with their own rule.
- CTA-708-E has no normative table of cc_count by repeat_first_field; only the formula of
  4.3.6. Its field-order statements for CEA-608 pairs are in an informative annex that quotes
  text no longer in the body. `CC-608-ORDER` is a note.
- H.262 6.3.8 does not say what the GOP time code counts when frames are shown for three
  fields: mpeg2enc and mencoder count coded frames, a time code on the clock counts display
  frame periods. `GOP-TIMECODE-CONTINUITY` accepts both.
- That the output fields alternate between top and bottom is not in 6.3.10 with the flags but
  in 7.12 (p.106), as "a requirement on the bitstream": `OUT-FIELD-PARITY`. (Missing until the
  film work; mencoder's soft telecine with B-pictures breaks it, and so did two hand-built
  test streams of this crate.)
- A/53 Part 4 Table 6.1 prints bit_rate_value 48500 and vbv_buffer_size_value 488 without
  "less than or equal"; the prose and SCTE 43 Table 2 say "≤". The verifier reads them as
  maxima.
- The first picture after a sequence header may be a P-picture (6.1.1.6), so a stream that
  starts with a P-picture is legal; only B is forbidden there.

<!-- generated: tb-m2v-verify --rules-md -->
## Counts

| Scope | S1 | S2 | S3 | S4 | Total | Checked |
|---|---|---|---|---|---|---|
| stream | 10 | 9 | 0 | 1 | 20 | 19 |
| sequence | 10 | 9 | 0 | 1 | 20 | 20 |
| GOP | 1 | 2 | 0 | 3 | 6 | 6 |
| picture | 6 | 16 | 0 | 1 | 23 | 23 |
| slice | 8 | 3 | 0 | 0 | 11 | 10 |
| macroblock | 17 | 5 | 0 | 0 | 22 | 19 |
| block | 8 | 0 | 0 | 2 | 10 | 7 |
| cross-picture | 3 | 12 | 0 | 0 | 15 | 14 |
| buffer model | 3 | 2 | 1 | 0 | 6 | 5 |
| profile/level | 0 | 14 | 0 | 1 | 15 | 15 |
| broadcast profile | 0 | 0 | 29 | 12 | 41 | 37 |
| **all** | 66 | 72 | 30 | 21 | 189 | 175 |

## Scope: stream

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `SC-SEQUENCE-ERROR` | S1 | H.262 §6.2.1 | 21 | sequence_error_code (B4) is not part of the video syntax; a media interface inserts it where uncorrectable errors were found. | yes |
| `SC-ALIGNED` | S1 | H.262 §6.2.1 | 21 | All start codes shall be byte aligned. | implied: the verifier finds start codes by bytes, as decoders do; a prefix at another bit position is SC-EMULATION |
| `STUFF-NONZERO` | S1 | H.262 §5.2.3 | 10 | Between the end of a syntactic structure and the next start code only zero bits (to the byte boundary) and zero bytes may occur. | yes |
| `STREAM-START` | S1 | H.262 §6.2.2 | 22 | A video sequence starts with a sequence_header(); nothing but zero stuffing precedes it. | yes |
| `ORD-SEQEXT-MISSING` | S1 | H.262 §6.3.1 | 37 | Every sequence_header() shall be followed immediately by sequence_extension() (otherwise the stream is ISO/IEC 11172-2, which this verifier does not check). | yes |
| `ORD-SEQEXT-STRAY` | S1 | H.262 §6.3.1 | 37 | sequence_extension() shall only occur immediately following a sequence_header(). | yes |
| `ORD-PCE-MISSING` | S1 | H.262 §6.3.1 | 37 | Each picture_header() shall be followed immediately by a picture_coding_extension(). | yes |
| `ORD-PCE-STRAY` | S1 | H.262 §6.3.1 | 37 | picture_coding_extension() shall only occur immediately following a picture_header(). | yes |
| `ORD-UNIT` | S1 | H.262 §6.2.2 | 22 | Headers, extensions, user data and slices occur only at the points the video_sequence() syntax allows (for example no group_of_pictures_header() between a picture header and its slices, no slice without a picture header, no picture before the first sequence header). | yes |
| `HDR-TRUNCATED` | S1 | H.262 §6.2.1, §6.2.2 to §6.2.3 | 21-31 | A header or extension is shorter than its syntax: a start code (or the end of the stream) occurs inside it, which also means its fields emulate a start code. | yes |
| `SC-EMULATION` | S2 | H.262 §6.2.1 | 21 | A start code prefix (23 zero bits and a one) shall not occur anywhere except as a byte-aligned start code: no 23 or more consecutive zero bits followed by a one at any other bit position. | yes |
| `SC-RESERVED` | S2 | H.262 §6.2.1, Table 6-1 | 22 | Start code values B0, B1 and B6 are reserved. | yes |
| `SC-SYSTEM` | S2 | H.262 §6.2.1, Table 6-1 | 22 | System start codes (B9 to FF) belong to H.222.0 and do not occur in a video elementary stream. | yes |
| `SEQ-END-PLACEMENT` | S2 | H.262 §6.2.2, §6.3.1 | 22, 37 | sequence_end_code follows the picture data of a coded picture; after it only a new sequence_header() (or the end of the stream) may follow. | yes |
| `ORD-EXT-PLACE` | S2 | H.262 §6.2.2.2.1 | 24 | Each extension occurs only at its point: sequence display and sequence scalable extensions after sequence_extension(); quant matrix, copyright, picture display, scalable, camera parameters and ITU-T extensions after picture_coding_extension(); none after a group_of_pictures_header(). | yes |
| `ORD-EXT-DUP` | S2 | H.262 §6.3.1 | 37 | At each point where extensions are allowed, each type of extension shall not occur more than once. | yes |
| `ORD-EXT-RESERVED` | S2 | H.262 §6.3.1, Table 6-2 | 37 | extension_start_code_identifier values 0, 6 and 13 to 15 are reserved. | yes |
| `ORD-SEQ-NO-PICTURE` | S2 | H.262 §6.3.1 | 37 | Following a sequence_header() there shall be at least one coded picture before a repeat sequence_header() or a sequence_end_code. | yes |
| `UD-ZERO-RUN` | S2 | H.262 §6.3.4.1 | 40 | User data shall not contain a string of 23 or more consecutive zero bits. | yes |
| `SEQ-END-MISSING` | S4 | H.262 §6.2.2, §6.3.2 | 22, 38 | A video sequence is terminated by sequence_end_code. | yes |

## Scope: sequence

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `SEQ-HSIZE-ZERO` | S1 | H.262 §6.3.3 | 38 | horizontal_size_value shall not be zero. | yes |
| `SEQ-VSIZE-ZERO` | S1 | H.262 §6.3.3 | 38 | vertical_size_value shall not be zero. | yes |
| `SEQ-FRAME-RATE` | S1 | H.262 §6.3.3, Table 6-4 | 39 | frame_rate_code 0 is forbidden and 9 to 15 are reserved. | yes |
| `SEQ-MARKER` | S1 | H.262 §6.3.3 | 40 | The marker_bit of the sequence header shall be '1'. | yes |
| `QM-ZERO` | S1 | H.262 §6.3.11 | 52 | No value of a quantizer matrix (sequence header or quant matrix extension) shall be zero. | yes |
| `SEQ-REPEAT-DIFF` | S1 | H.262 §6.1.1.6 | 12 | In a repeat sequence header every data element except the quantizer matrices shall have the same value as in the first sequence header of the sequence. | yes |
| `SEQ-REPEAT-EXT-DIFF` | S1 | H.262 §6.1.1.6 | 12 | Every data element of the sequence_extension() after a repeat sequence header shall have the same value as in the first one. | yes |
| `SEQX-CHROMA` | S1 | H.262 §6.3.5, Table 6-5 | 41 | chroma_format '00' is reserved. | yes |
| `SEQX-MARKER` | S1 | H.262 §6.2.2.3 | 25 | The marker_bit of the sequence extension shall be '1'. | yes |
| `SDE-MARKER` | S1 | H.262 §6.2.2.4 | 25 | The marker_bit of the sequence display extension shall be '1'. | yes |
| `SEQ-ASPECT` | S2 | H.262 §6.3.3, Table 6-3 | 38 | aspect_ratio_information 0 is forbidden and 5 to 15 are reserved. | yes |
| `SEQ-BITRATE-ZERO` | S2 | H.262 §6.3.3 | 39 | bit_rate (bit_rate_value with bit_rate_extension) shall not be zero. | yes |
| `SEQ-CONSTRAINED` | S2 | H.262 §6.3.3 | 40 | constrained_parameters_flag shall be '0'. | yes |
| `QM-INTRA-DC` | S2 | H.262 §6.3.11 | 52 | The first value of an intra quantizer matrix shall be 8. | yes |
| `SEQ-REPEAT-SDE` | S2 | H.262 §6.1.1.6 | 12 | If a sequence_display_extension() (or sequence_scalable_extension()) follows the first sequence header, an identical one shall follow every repeat; if none follows the first, none shall occur in the sequence. | yes |
| `SEQX-PROFILE` | S2 | H.262 §8, Tables 8-2 to 8-4 | 108-109 | profile_and_level_indication shall carry a defined profile and level; reserved values shall not be used. | yes |
| `SEQX-FRAME-RATE-EXT` | S2 | H.262 §6.3.3 | 39 | frame_rate_extension_n and frame_rate_extension_d shall be zero when the frame rate is a Table 6-4 value, and (n+1) and (d+1) shall have no common divisor above one. | yes |
| `SDE-VIDEO-FORMAT` | S2 | H.262 §6.3.6, Table 6-6 | 41 | video_format 6 and 7 are reserved. | yes |
| `SDE-COLOR` | S2 | H.262 §6.3.6, Tables 6-7 to 6-9 | 42-44 | colour_primaries, transfer_characteristics and matrix_coefficients: 0 is forbidden; 3 and the values above the tables (8 or 9 to 255) are reserved. | yes |
| `SDE-DISPLAY-SIZE` | S4 | H.262 §6.3.6 | 45 | display_horizontal_size and display_vertical_size define the intended display rectangle in samples and lines of the coded frame; zero is meaningless. | yes |

## Scope: GOP

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `GOP-MARKER` | S1 | H.262 §6.3.8, Table 6-11 | 46 | The marker_bit inside time_code shall be '1'. | yes |
| `GOP-TIMECODE-RANGE` | S2 | H.262 §6.3.8, Table 6-11 | 46 | time_code_hours is 0 to 23, time_code_minutes 0 to 59, time_code_seconds 0 to 59, time_code_pictures 0 to 59. | yes |
| `GOP-DROP-FRAME` | S2 | H.262 §6.3.8 | 46 | drop_frame_flag may be '1' only if the frame rate is 29.97 Hz; with it, picture numbers 0 and 1 of each minute not divisible by ten do not exist. | yes |
| `GOP-TIMECODE-PICTURES` | S4 | H.262 §6.3.8 | 46 | time_code_pictures counts pictures within a second of the frame rate rounded to an integer, so it is below that integer. | yes |
| `GOP-TIMECODE-CONTINUITY` | S4 | H.262 §6.3.8 | 46 | The time code refers to the first picture after the GOP header with temporal_reference zero; successive GOP time codes advance by the number of frames displayed between them, or, where frames are shown for three fields or repeated, by the display time between them in frame periods (not used by decoding). | yes |
| `GOP-BROKEN-LINK` | S4 | H.262 §6.3.8 | 46 | broken_link shall be '0' during encoding; '1' marks an edit after which the first B-pictures may not decode correctly. | yes |

## Scope: picture

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `ORD-NO-SLICE` | S1 | H.262 §6.2.3.7 | 30 | picture_data() contains at least one slice(). | yes |
| `PIC-TYPE` | S1 | H.262 §6.3.9, Table 6-12 | 47 | picture_coding_type 0 is forbidden, 4 (D-picture) shall not be used in H.262, 5 to 7 are reserved. | yes |
| `PCE-FCODE-RESERVED` | S1 | H.262 §6.3.10 | 48 | f_code[s][t] takes values 1 to 9 or 15; zero is forbidden and 10 to 14 are reserved. | yes |
| `PCE-PICTURE-STRUCTURE` | S1 | H.262 §6.3.10, Table 6-14 | 49 | picture_structure '00' is reserved. | yes |
| `PDX-MARKER` | S1 | H.262 §6.2.3.3 | 29 | The marker_bits of the picture display extension shall be '1'. | yes |
| `CPX-MARKER` | S1 | H.262 §6.2.3.6 | 30 | The marker_bits of the copyright extension shall be '1'. | yes |
| `SEQX-PROGRESSIVE` | S2 | H.262 §6.3.5 | 40 | When progressive_sequence is '1' the sequence contains only progressive frame-pictures (picture_structure "Frame", progressive_frame '1'). | yes |
| `PIC-FULL-PEL` | S2 | H.262 §6.3.9 | 48 | full_pel_forward_vector and full_pel_backward_vector shall be '0'. | yes |
| `PIC-FCODE-111` | S2 | H.262 §6.3.9 | 48 | forward_f_code and backward_f_code of the picture header shall be '111'. | yes |
| `PIC-EXTRA-INFO` | S2 | H.262 §6.3.9 | 48 | extra_bit_picture shall be '0'; extra_information_picture shall not be present. | yes |
| `PCE-FCODE-UNUSED` | S2 | H.262 §6.3.10 | 48 | An f_code that is not used shall be 15: all four in an I-picture without concealment motion vectors, f_code[1][t] in every I- and P-picture. | yes |
| `PCE-TFF-FIELD` | S2 | H.262 §6.3.10 | 49 | In a field picture top_field_first shall be '0'. | yes |
| `PCE-TFF-PROGRESSIVE` | S2 | H.262 §6.3.10 | 49 | In a progressive sequence, top_field_first shall be '0' when repeat_first_field is '0'. | yes |
| `PCE-FPFD-FIELD` | S2 | H.262 §6.3.10 | 50 | In a field picture frame_pred_frame_dct shall be '0'. | yes |
| `PCE-FPFD-PROGRESSIVE` | S2 | H.262 §6.3.10 | 50 | frame_pred_frame_dct shall be '1' if progressive_sequence is '1'. | yes |
| `PCE-RFF-FIELD` | S2 | H.262 §6.3.10 | 50 | In a field picture repeat_first_field shall be '0'. | yes |
| `PCE-RFF-INTERLACED` | S2 | H.262 §6.3.10 | 50 | If progressive_sequence and progressive_frame are both '0', repeat_first_field shall be '0'. | yes |
| `PCE-CHROMA-420-TYPE` | S2 | H.262 §6.3.10 | 50 | With 4:2:0, chroma_420_type shall equal progressive_frame; otherwise it shall be '0'. | yes |
| `PCE-PROGRESSIVE-FRAME-FIELD` | S2 | H.262 §6.3.10 | 50 | If progressive_frame is '1', picture_structure shall be "Frame". | yes |
| `QMX-CHROMA` | S2 | H.262 §6.3.11 | 52 | With 4:2:0, load_chroma_intra_quantiser_matrix and load_chroma_non_intra_quantiser_matrix shall be '0'. | yes |
| `PDX-NO-SDE` | S2 | H.262 §6.3.12 | 53 | A picture_display_extension() shall not occur unless a sequence_display_extension() followed the previous sequence header. | yes |
| `CPX-RESERVED` | S2 | H.262 §6.3.15 | 55 | The reserved bits of the copyright extension shall be zero; with copyright_flag '0', copyright_identifier and copyright_number shall be zero. | yes |
| `PCE-COMPOSITE-RFF` | S4 | H.262 §6.3.10 NOTE 2 | 50 | It is recommended that repeat_first_field and composite_display_flag are not both set. | yes |

## Scope: slice

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `SLICE-VPOS` | S1 | H.262 §6.3.16 | 55 | slice_vertical_position addresses a macroblock row of the picture (1 to 175, and no more than the number of rows). | yes |
| `SLICE-ORDER` | S1 | H.262 §6.1.2 | 18 | Slices shall not overlap and shall occur in raster order. | yes |
| `SLICE-GAP` | S1 | H.262 §6.1.2.2, Table 8-5 | 18, 110 | Restricted slice structure (all profiles checked here): every macroblock of the picture shall be enclosed in a slice. | yes |
| `SLICE-ROW-OVERRUN` | S1 | H.262 §6.1.2 | 18 | The first and last macroblock of a slice shall be in the same horizontal row of macroblocks. | yes |
| `SLICE-QSCALE-ZERO` | S1 | H.262 §6.3.16 | 55 | quantiser_scale_code of a slice shall not be zero. | yes |
| `SLICE-EMPTY` | S1 | H.262 §6.1.2, §6.2.4 | 18, 32 | Every slice shall contain at least one macroblock. | yes |
| `SLICE-TRUNCATED` | S1 | H.262 §6.2.4 | 32 | A slice consists of whole macroblocks followed by zero bits up to the next start code: the data shall not end inside a macroblock. | yes |
| `SLICE-FIRST-LAST-SKIPPED` | S1 | H.262 §6.1.2 | 18 | The first and last macroblocks of a slice shall not be skipped macroblocks. | implied: the syntax cannot express a skipped first or last macroblock; under the restricted slice structure the attempt shows as SLICE-GAP |
| `SLICE-EXTRA-INFO` | S2 | H.262 §6.3.16 | 56 | extra_bit_slice shall be '0'; extra_information_slice shall not be present. | yes |
| `SLICE-INTRA-FLAG` | S2 | H.262 §6.3.16 | 56 | intra_slice shall be '0' if any macroblock of the slice is non-intra. | yes |
| `SLICE-PICTURE-ID` | S2 | H.262 §6.3.16 | 56 | slice_picture_id shall be zero unless slice_picture_id_enable is '1'; both shall have the same value in all slices of a picture. | yes |

## Scope: macroblock

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `MB-ADDR-VLC` | S1 | H.262 §6.3.17, Table B.1 | 56, 121 | macroblock_address_increment is a code of Table B.1. | yes |
| `MB-STUFFING` | S1 | H.262 §6.3.17 NOTE, Table B.1 NOTE | 56, 121 | macroblock_stuffing of ISO/IEC 11172-2 ('0000 0001 111') shall not be used. | yes |
| `MB-TYPE-VLC` | S1 | H.262 §6.3.17.1, Tables B.2 to B.4 | 57, 122-123 | macroblock_type is a code of the table for the picture_coding_type. | yes |
| `MB-SKIP-I` | S1 | H.262 §6.3.17, §7.6.6 | 57, 81 | There shall be no skipped macroblocks in I-pictures. | yes |
| `MB-SKIP-B-AFTER-INTRA` | S1 | H.262 §6.3.17 | 57 | In a B-picture there shall be no skipped macroblock immediately following an intra macroblock. | yes |
| `MB-MOTION-TYPE` | S1 | H.262 §6.3.17.1, Tables 6-17 and 6-18 | 57-58 | frame_motion_type and field_motion_type '00' are reserved. | yes |
| `MB-DUAL-PRIME` | S1 | H.262 §7.6.1, Tables 7-9 and 7-10 | 71, 75-76 | Dual-prime prediction shall only be used in P-pictures (forward prediction only). | yes |
| `MB-QSCALE-ZERO` | S1 | H.262 §6.3.16, Table 7-6 | 55, 68 | quantiser_scale_code of a macroblock shall not be zero. | yes |
| `MB-CONCEAL-MARKER` | S1 | H.262 §6.2.5 | 33 | The marker_bit after concealment motion vectors shall be '1'. | yes |
| `MB-CBP-VLC` | S1 | H.262 §6.3.17.4, Table B.9 | 59, 127 | coded_block_pattern_420 is a code of Table B.9. | yes |
| `MB-CBP-ZERO` | S1 | H.262 Table B.9 NOTE | 127 | coded_block_pattern zero shall not be used with 4:2:0. | yes |
| `MB-CONCEAL-PRESENCE` | S1 | H.262 §7.6.3.9 | 78 | A concealment motion vector shall be present for all intra macroblocks if and only if concealment_motion_vectors is one. | implied: presence is tied to concealment_motion_vectors by the syntax of 6.2.5 |
| `MB-16X8-FRAME` | S1 | H.262 §7.6.1 | 71 | 16x8 motion compensation shall only be used with field pictures. | implied: frame_motion_type has no code for 16x8 |
| `MV-CODE-VLC` | S1 | H.262 §6.3.17.3, Table B.10 | 58, 128 | motion_code is a code of Table B.10. | yes |
| `MV-FCODE-15` | S1 | H.262 §6.3.10, Table 7-8 | 48, 75 | A motion vector shall not be coded with an f_code of 15 (the value for "not used"). | yes |
| `MV-OUTSIDE` | S1 | H.262 §7.6.3.8 | 78 | Reconstructed motion vectors (including those of skipped macroblocks, dual-prime derived vectors, half-sample positions and the scaled chrominance vectors) shall not refer to samples outside the coded picture. | yes |
| `MV-PREDICTOR-MODEL` | S1 | H.262 §7.6.3.3, §7.6.3.4, §7.6.6 | 74-76, 81-82 | Motion vector predictors are reset at the start of each slice, after an intra macroblock without concealment vectors, after a no-MC or skipped macroblock in a P-picture, and updated as Tables 7-9 and 7-10 say. | implied: the verifier applies the resets and updates of 7.6.3.3, 7.6.3.4 and 7.6.6 itself; an encoder that does otherwise produces vectors that fail MV-OUTSIDE, MV-DELTA-RANGE or MV-FIELD-HALF-RANGE, or decode to different pictures (not visible here) |
| `MB-BITS` | S2 | H.262 §8.2, Table 8-6 | 110 | A macroblock shall take no more than 4608 bits (4:2:0), except for at most two macroblocks in each row. | yes |
| `MB-PFIELD-AFTER-IFIELD` | S2 | H.262 §7.6.3.5 | 76 | A P field picture that is the second field of a frame whose first field is an I field shall have no skipped or no-MC non-intra macroblocks, no dual prime, and no field prediction from the same parity. | yes |
| `MV-DELTA-RANGE` | S2 | H.262 §7.6.3.1 | 74 | The reconstructed differential delta shall lie in [low:high] = [-16f : 16f-1]. | yes |
| `MV-FIELD-HALF-RANGE` | S2 | H.262 §7.6.3.1, §7.6.3.2, Table 7-8 | 74-75 | In frame pictures the vertical component of a field motion vector covers only half the range of its f_code (so that the updated predictor stays in [low:high]). | yes |
| `MV-LEVEL-VERTICAL` | S2 | H.262 §8.3, Table 8-8 note b | 112 | The vertical component of every final motion vector, and of the intermediate dual-prime values, lies in the vertical vector range of the level ([-128:127.5] in frame pictures and [-64:63.5] in field pictures at Main, High-1440 and High level). | yes |

## Scope: block

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `BLK-DC-RANGE` | S1 | H.262 §7.2.1 | 63 | The intra DC coefficient QFS[0] shall lie in 0 to 2^(8+intra_dc_precision)-1. | yes |
| `BLK-COEF-VLC` | S1 | H.262 §7.2.2, Tables B.14 and B.15 | 63, 130-137 | Every DCT coefficient code is a code of the table selected by intra_vlc_format and macroblock_intra (Table 7-3). | yes |
| `BLK-OVERRUN` | S1 | H.262 §7.2.2.4 | 64 | A block has at most 64 coefficients: runs shall not carry the coefficient index past 63. | yes |
| `BLK-ESCAPE-ZERO` | S1 | H.262 Table B.16 | 137 | The escape signed_level '0000 0000 0000' is forbidden. | yes |
| `BLK-ESCAPE-2048` | S1 | H.262 Table B.16 | 137 | The escape signed_level '1000 0000 0000' (-2048) is reserved. | yes |
| `BLK-EOB-ONLY` | S1 | H.262 Tables B.14 and B.15 NOTE 2 | 133 | End of Block shall not be the only code of a block. | implied: a non-intra block cannot start with End of Block: the same bits are the first-coefficient code of Table B.14 note 3 |
| `BLK-DC-PREDICTOR-MODEL` | S1 | H.262 §7.2.1 | 62 | The intra DC predictors are reset at the start of a slice, after a non-intra macroblock and after a skipped macroblock. | implied: the verifier applies the resets of 7.2.1 itself; an encoder that resets differently produces DC values that fail BLK-DC-RANGE or decode to different pictures (not visible here) |
| `BLK-MISMATCH` | S1 | H.262 §7.4.4, Annex A | 67, 119 | The decoder toggles F[7][7] when the coefficient sum is even; the encoder's reconstruction has to do the same. | **no**: mismatch control (7.4.4) and the IDCT (Annex A) are decoder processes, not constraints on the bitstream; whether the encoder's own reconstruction applies them shows only as drift against a reference decoder, which the gate's decode comparison covers |
| `BLK-ESCAPE-REDUNDANT` | S4 | H.262 §7.2.2.3 | 64 | Escape coding is meant for (run, level) combinations without a code in the selected table. (The text does not forbid an escape for a combination that has one; reported as a note.) | yes |
| `BLK-SATURATION` | S4 | H.262 §7.4.3 | 67 | Reconstructed coefficients are saturated to [-2048:2047]. A coded level that reaches the saturation shows that the encoder's quantizer did not model the decoder. (Legal; reported as a note.) | yes |

## Scope: cross-picture

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `GOP-FIRST-I` | S1 | H.262 §6.1.1.7, §6.3.1 | 13, 37 | The first coded frame following a group_of_pictures_header() shall be a coded I-frame. | yes |
| `PCE-FIELD-PARITY` | S1 | H.262 §6.3.10 | 49 | A frame coded as two field pictures consists of two fields of opposite parity: every field picture has its partner, and the second has the opposite parity. | yes |
| `PCE-FIELD-TYPE` | S1 | H.262 §6.3.10 | 49 | Both field pictures of a frame shall have the same picture_coding_type, except that an I field may be followed by a P field. | yes |
| `SEQ-END-COMPLETE` | S2 | H.262 §6.3.1 | 37 | sequence_end_code shall be placed so that, after decoding and frame re-ordering, no frames are missing: the sequence shall not end on the first field of a frame. | yes |
| `SEQ-FIRST-PIC-B` | S2 | H.262 §6.1.1.6, §6.1.1.11 | 13, 16 | The first picture following a sequence header or repeat sequence header shall be an I-picture or a P-picture, not a B-picture. | yes |
| `SEQ-HDR-BETWEEN-FIELDS` | S2 | H.262 §6.1.1.6 | 13 | A repeat sequence header shall not precede the second field picture of a frame. | yes |
| `SEQ-RANDOM-ACCESS` | S2 | H.262 §6.1.1.6 | 13 | The bitstream that results from removing everything before any repeat sequence header shall be a legal bitstream. | **no**: needs a second pass from every repeat header; the parts that matter (first picture not B, closed GOP predictions) are checked by SEQ-FIRST-PIC-B and GOP-CLOSED-PRED |
| `SEQX-LOW-DELAY-B` | S2 | H.262 §6.3.5 | 41 | When low_delay is '1' the sequence shall not contain B-pictures. | yes |
| `GOP-CLOSED-PRED` | S2 | H.262 §6.3.8 | 46 | With closed_gop '1', the B-pictures immediately following the first coded I-frame use only backward prediction or intra coding. | yes |
| `GOP-OPEN-START` | S2 | H.262 §6.3.8, §7.6.3.8 | 46, 78 | B-pictures that follow the first I-frame of a sequence and use forward prediction refer to a frame that is not in the sequence, unless broken_link says so. | yes |
| `PIC-TEMPORAL-REF` | S2 | H.262 §6.3.9 | 47 | temporal_reference increments by one modulo 1024 per coded frame in display order; the frame displayed first among those coded after a GOP header has temporal_reference zero. | yes |
| `PIC-TEMPORAL-REF-FIELDS` | S2 | H.262 §6.3.9 | 47 | The two field pictures of a coded frame shall have the same temporal_reference. | yes |
| `PIC-VBV-DELAY-MIX` | S2 | H.262 §6.3.9 | 48 | If any vbv_delay in a sequence is 0xFFFF, all of them shall be. | yes |
| `OUT-FIELD-PARITY` | S2 | H.262 §7.12 | 106 | In an interlaced sequence the fields at the output of the decoding process shall always be alternately top and bottom (top_field_first and repeat_first_field of consecutive frames in display order must agree; the very first field of a sequence may be either). | yes |
| `MB-DUAL-PRIME-B-BETWEEN` | S2 | H.262 §7.6.1 | 71 | Dual-prime prediction shall only be used where there are no B-pictures between the predicted and the reference fields or frames. | yes |

## Scope: buffer model

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `VBV-UNDERFLOW` | S1 | H.262 Annex C.6 | 140 | With low_delay '0', all data of a picture shall be in the VBV buffer at its decoding time. | yes |
| `VBV-OVERFLOW` | S1 | H.262 Annex C.5 | 139 | When the VBV buffer is examined, before removing picture data, its fullness shall be between zero and B = 16384 x vbv_buffer_size bits. | yes |
| `VBV-PICTURE-SIZE` | S1 | H.262 Annex C.5, C.6 | 139-140 | The data of one picture never exceed the VBV buffer size (it could not be all present, C.6, within B, C.5). | yes |
| `VBV-DELAY-RATE` | S2 | H.262 Annex C.3.1 | 138-139 | With vbv_delay not 0xFFFF, the rate R(n) = d*n / (tau(n) - tau(n+1) + t(n+1) - t(n)) is positive and does not exceed the rate of the bit_rate field. | yes |
| `VBV-LOW-DELAY-BIG` | S2 | H.262 Annex C.7 | 140 | With low_delay '1', a big picture is removed at a later re-examination; the last coded picture of a sequence shall not be a big picture. | **no**: big pictures are modeled (their temporal_reference gap lengthens the examination interval), but the rule that the last picture is not one cannot be told from underflow |
| `VBV-OPERATING-MODEL` | S3 | this project's delivery contract (not a standard) | - | Operating model of this project (--delivery operating): picture n enters the multiplexer queue at its coding time e(n); the queue drains at the declared rate; every picture has left it by e(n) + D, with D = vbv_buffer_size / bit_rate. | yes |

## Scope: profile/level

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `PL-CHROMA` | S2 | H.262 §8.2, Table 8-5 | 110 | Main and Simple profile: chroma_format shall be 4:2:0. | yes |
| `PL-FRAME-RATE-EXT` | S2 | H.262 §8.2, Table 8-5 | 110 | frame_rate_extension_n and frame_rate_extension_d shall be zero. | yes |
| `PL-ASPECT` | S2 | H.262 §8.2, Table 8-5 | 110 | aspect_ratio_information shall be 1, 2 or 3. | yes |
| `PL-PICTURE-TYPE` | S2 | H.262 §8.2, Table 8-5 | 110 | Simple profile: only I- and P-pictures. | yes |
| `PL-RFF` | S2 | H.262 §8.2.1, Table 8-7 | 111 | repeat_first_field is allowed only where Table 8-7 says: not at 23.976 or 24 Hz; in progressive sequences only at 59.94 and 60 Hz. | yes |
| `PL-RFF-MAIN-LEVEL` | S2 | H.262 §8.2.1 | 111 | At Main level, if vertical_size > 480 or the frame rate is 25 Hz, B-pictures shall have repeat_first_field '0'; if vertical_size > 480 the frame rate shall be 25 Hz. | yes |
| `PL-SCALABLE` | S2 | H.262 §8.2, Table 8-5 | 110 | Main and Simple profile: no sequence_scalable_extension(), picture_spatial_scalable_extension() or picture_temporal_scalable_extension(). | yes |
| `PL-INTRA-DC` | S2 | H.262 §8.2, Table 8-5 | 110 | intra_dc_precision shall be 8, 9 or 10 bits (not 11). | yes |
| `PL-FCODE` | S2 | H.262 §8.3, Table 8-8 | 112 | f_code limits of the level: horizontal 1 to 7 (Low), 8 (Main), 9 (High-1440, High); vertical 1 to 4 (Low) or 5 in frame pictures and 1 to 3 (Low) or 4 in field pictures. | yes |
| `PL-FRAME-RATE` | S2 | H.262 §8.3, Tables 8-8 and 8-11 | 112, 114 | frame_rate_code is 1 to 5 at Low and Main level and 1 to 8 at High-1440 and High level. | yes |
| `PL-SIZE` | S2 | H.262 §8.5, Table 8-11 | 114 | horizontal_size and vertical_size do not exceed 352x288 (Low), 720x576 (Main), 1440x1088 (High-1440), 1920x1088 (High). | yes |
| `PL-SAMPLE-RATE` | S2 | H.262 §8.5, Table 8-12 | 115 | The luminance sample rate P does not exceed 3 041 280 (Low), 10 368 000 (Main), 47 001 600 (High-1440), 62 668 800 (High) samples/s. | yes |
| `PL-BIT-RATE` | S2 | H.262 §8.5, Table 8-13 | 116 | bit_rate does not exceed 4 (Low), 15 (Main), 60 (High-1440), 80 (High) Mbit/s. | yes |
| `PL-VBV-SIZE` | S2 | H.262 §8.5, Table 8-14 | 116 | vbv_buffer_size does not exceed 475 136 (Low), 1 835 008 (Main), 7 340 032 (High-1440), 9 781 248 (High) bits. | yes |
| `PL-PROFILE` | S4 | H.262 §8, Table 8-2 | 108 | The verifier's profile and level limits are those of Simple and Main profile; a stream of another profile is checked for syntax only. | yes |

## Scope: broadcast profile

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `BC-PROFILE` | S3 | A/53-4 §6; SCTE 43 §7 | 8; 2 | The video shall conform to Main Profile syntax within the limits of MP@HL. | yes |
| `BC-FORMAT` | S3 | A/53-4 §6.1.2 Table 6.2; SCTE 43 §7.1.2 Table 3 | 9; 3 | vertical_size_value, horizontal_size_value, aspect_ratio_information, frame_rate_code and progressive_sequence form a row of the compression format table. | yes |
| `BC-BIT-RATE` | S3 | A/53-4 §6.1.1 Table 6.1; SCTE 43 §7.1.1 Table 2 | 9; 2 | bit_rate_value is at most 48500 (A/53, 19.4 Mbit/s), 67500 (SCTE 43 64-QAM, 27.0 Mbit/s) or 97000 (SCTE 43 256-QAM, 38.8 Mbit/s). | yes |
| `BC-VBV-SIZE` | S3 | A/53-4 §6.1.1 Table 6.1; SCTE 43 §7.1.1 Table 2 | 9; 2 | vbv_buffer_size_value is at most 488. | yes |
| `BC-SEQEXT-FIELDS` | S3 | A/53-4 §6.1.3 Table 6.3; SCTE 43 §7.1.3 Table 4 | 10; 3-4 | chroma_format is '01'; horizontal_size_extension, vertical_size_extension, bit_rate_extension, vbv_buffer_size_extension, frame_rate_extension_n and frame_rate_extension_d are zero. | yes |
| `BC-LEVEL-LOWEST` | S3 | A/53-4 Table 6.3 note; SCTE 43 §7.1.3.1 | 10; 4 | profile_and_level_indication shall indicate the lowest profile and level consistent with the parameters of the stream. | yes |
| `BC-COLOR-EXPLICIT` | S3 | A/53-4 §6.1.4; SCTE 43 §7.1.4 | 10; 4 | colour_primaries, transfer_characteristics and matrix_coefficients shall be explicit in a sequence_display_extension() (SCTE 43 exempts 480-line interlaced 29.97 Hz). | yes |
| `BC-VIDEO-FORMAT` | S3 | A/53-4 Table 6.4; SCTE 43 Table 5 | 10; 4 | video_format shall be '000'. | yes |
| `BC-VBV-DELAY` | S3 | A/53-4 §6.1.5; SCTE 43 §7.1.5 | 10; 4 | Unless it is 0xFFFF, vbv_delay shall be at most 45000. | yes |
| `BC-PROGRESSIVE-FPFD` | S3 | A/53-4 §6.1.6; SCTE 43 §7.1.6 | 11; 4 | frame_pred_frame_dct shall be '1' if progressive_frame is '1'. | yes |
| `BC-STILL-PICTURE` | S3 | A/53-3 §5.1.1; SCTE 54 2024 §7.8.4 | 10; 18 | A stream with still pictures carries video_stream_descriptor() with still_picture_flag, and I-frames at most 60 s apart. | **no**: a transport-layer rule (video_stream_descriptor) plus an I-frame interval of 60 s; the interval is not reached by test streams |
| `BC-PES-ALIGNMENT` | S3 | A/53-3 §5.5.1, §5.1.1; SCTE 54 2024 §7.1.1 | 13, 10; 12 | Each PES packet starts with one video access unit and carries PTS (and DTS); the stream obeys the T-STD of its level. | **no**: transport layer: PES packets, PTS and DTS, T-STD |
| `UD-DUP-TYPE` | S3 | A/53-4 §6.2.1 | 11 | No more than one user_data() with the same user_data_type_code shall follow any picture header. | yes |
| `UD-TYPE-RESERVED` | S3 | A/53-4 §6.2.3, Table 6.9 | 12 | In ATSC_user_data() only user_data_type_code 0x03 (captions) and 0x06 (bar data) are defined; the others are reserved. | yes |
| `UD-ORDER` | S3 | SCTE 43 §7.3 | 5 | Picture user data structures are ordered: SCTE 20 captions, A/53 captions, AFD, bar data. | yes |
| `UD-SCTE20-FORMAT` | S3 | SCTE 43 §7.2.2 | 5 | SCTE 20 caption data shall only be used with the standard-definition interlaced formats. | yes |
| `UD-PLACEMENT` | S3 | A/53-4 §6.2; CTA-708-E Annex E.2 | 11; 103 | Caption, AFD and bar data are picture user data: they follow the picture coding extension, not a sequence or GOP header. | yes |
| `CC-TRAILER` | S3 | A/53-4 §6.2.3.1, Table 6.10 | 13 | MPEG_cc_data() ends with marker_bits '1111 1111' after cc_data(). | yes |
| `CC-LENGTH` | S3 | A/53-4 Table 6.10; CTA-708-E §4.3 Table 2 | 13; 10 | The user data holds exactly cc_data() (2 + 3 x cc_count bytes) and the marker byte. | yes |
| `CC-FIXED-BITS` | S3 | CTA-708-E §4.3 Table 2 | 10 | In cc_data(): the first reserved bit is '1', zero_bit is '0', the reserved byte is 0xFF. | yes |
| `CC-MARKER` | S3 | CTA-708-E §4.3 Table 2; A/53-4 §3.2 | 10; 6 | In every caption construct one_bit is '1' and the four reserved bits are '1111' (this also keeps the user data free of start code emulation). | yes |
| `CC-COUNT` | S3 | CTA-708-E §4.3.6 | 15-16 | cc_count is 9600 x t / 16 rounded, t being the presentation time of the picture: 20 at 29.97/30 Hz frames, 10 at 59.94/60 Hz, 25 at 23.976/24 Hz, 30 for a frame of three fields. | yes |
| `CC-STATE` | S3 | CTA-708-E §4.3.1, Figure 2 | 11-12 | After a DTVCC construct (cc_type 10 or 11) in a cc_data(), no valid CEA-608 construct (cc_valid 1, cc_type 00 or 01) shall follow. | yes |
| `CC-608-POSITION` | S3 | CTA-708-E §4.3.1 rule 2, §4.3.2.2 | 12, 14 | CEA-608 byte pairs occur only in the first one, two or three constructs, depending on the frame rate; at most two per field per cc_data(). | yes |
| `CC-608-FIELD2` | S3 | CTA-708-E §4.3.2.2.1, §4.3.2.2 | 14, 13 | When CEA-608 data is present, cc_data() includes a construct for each field, even with cc_valid '0' for one of them (frames above 30/1.001 Hz may carry fewer). Valid field 2 data beside a field 1 construct with cc_valid '0' is legal: the decoder then generates the field 1 run-in itself. | yes |
| `AFD-SYNTAX` | S3 | A/53-4 §6.2.4, Table 6.13 | 16 | afd_data(): a zero bit, active_format_flag, reserved '00 0001'; then, if the flag is set, reserved '1111' and active_format; nothing else. | yes |
| `AFD-RESERVED` | S3 | A/53-4 §6.2.4, Table 6.14 | 17 | active_format values 0001, 0101 to 0111 and 1100 are reserved. | yes |
| `BAR-SYNTAX` | S3 | A/53-4 §6.2.3.2, Table 6.11 | 14 | bar_data(): four flags, reserved '1111', then per flag one_bits '11' and a 14-bit value; nothing else. | yes |
| `BAR-FLAGS` | S3 | A/53-4 §6.2.3.2 | 14 | bottom_bar_flag equals top_bar_flag, right_bar_flag equals left_bar_flag, and top/bottom and left/right bars are not signaled together. | yes |
| `BC-COLOR-PREFERRED` | S4 | SCTE 43 §7.1.4 | 4 | SCTE 43 prefers ITU-R BT.709 (value 1 in all three color fields) for all formats. | yes |
| `BC-SEQ-INTERVAL` | S4 | SCTE 54 2024 §9.2 (informative) | 25 | Sequence headers are repeated frequently, for example every 0.5 s, for acquisition (no normative limit; reported above 0.6 s). | yes |
| `BC-BUFFER-DELAY` | S4 | H.222.0 §2.4.2.6 | 19 | Data may stay at most one second in the T-STD buffers: vbv_buffer_size / bit_rate above one second cannot be multiplexed at the declared rate. | yes |
| `BC-1088-PADDING` | S4 | A/53-4 Table 6.2 note a; SCTE 43 Table 3 note 1 | 9; 3 | Of the 1088 coded lines of a 1080-line format, the bottom 8 are black (A/53) or disregarded by decoders (SCTE 43). | **no**: needs the decoded samples; the verifier does not reconstruct pictures |
| `UD-IDENTIFIER` | S4 | A/53-4 §6.2.3, Table 6.7 | 12 | user_data_identifier should be a registered format_identifier; only 'GA94' and 'DTG1' are defined here (others are for receivers to skip). | yes |
| `CC-608-ORDER` | S4 | CTA-708-E §4.3.1 rule 2, §4.4.2, Annex C | 12, 19, 99 | The order of CEA-608 constructs follows the field display order; cc_type 01 goes with the top field and cc_type 00 with the bottom field, so with top_field_first '1' the first construct has cc_type 01 (Annex C examples; the body text is only partly normative). | yes |
| `CC-EVERY-PICTURE` | S4 | CTA-708-E §4.3.6 | 15 | Where a fixed caption bandwidth is required, cc_data() is included at every opportunity: once a stream carries captions, every picture carries cc_data(). | yes |
| `CC-PROCESS-FLAG` | S4 | CTA-708-E §4.3 | 10 | process_cc_data_flag '0' tells decoders to discard the whole cc_data(). | yes |
| `CC-PACKET` | S4 | CTA-708-E §4.3.3, §5.1 | 14, 21 | A caption channel packet starts with cc_type 11, continues with cc_type 10, and its packet_size_code matches its length. | **no**: the caption channel packet layer (sequence numbers, packet_size_code, service blocks) is above the video syntax; it needs the constructs in display order across pictures |
| `AFD-NOT-RECOMMENDED` | S4 | A/53-4 §6.2.4 | 17 | active_format 0010 and 0011 are not recommended; 0100 should be accompanied by bar data. | yes |
| `AFD-AFTER-SEQ` | S4 | A/53-4 §6.2.4 | 15 | After any sequence header the AFD, when present, shall appear before the next picture data (it does not persist across a sequence header). | yes |
| `BAR-AFTER-SEQ` | S4 | A/53-4 §6.2.3.2 | 13 | After any sequence header bar data, when in use, shall appear before the next picture data. | yes |

