# MPEG-2 format conformance: what the standards allow, what the encoder enforces

Audit of 2026-11-25. Read-only. It covers the Rust host (crates/tb-m2v-syntax, tb-m2v-encoder,
tb-m2v-rate, tb-m2venc) and the C++ encoder and kernels of the spike as they were then (the C++
host was deleted in package X1 and its references below are to the git history at `e79069b`;
the kernels are `kernels/` now).

**Sources.** All are text extractions in work/standards/. Page numbers are the printed ones.
- ITU-T H.262 (02/2000) = ISO/IEC 13818-2:2000: `ITU-T_H.262-200002-S.txt`. The 2012 Amd 4
  prepub has the same §8 tables.
- ATSC A/53 Part 4:2009: `A_53-Part-4-2009.txt`.
- ANSI/SCTE 43 2015 (R2021): `ANSI_SCTE_43_2015_R2021.txt`.
- SCTE 54 2024 and 2009: `SCTE_54_2024.txt`, `SCTE_54_2009_lawresource.txt`.
- ATSC A/53 Part 3:2013: `A53-Part-3-2013.txt`.
- ITU-T H.222.0 (06/2012): `ITU-T_H.222.0-201206.txt`.
- CTA-708-E S-2023: `CTA-708-E-S-2023.txt`.
- SCTE 128-1 2020: `ANSI_SCTE-128-1-2020-...txt`. Used only for 1080p60.

**Conventions.** Every normative statement cites clause, table and page. **[K]** marks
something not verified in these texts: background knowledge or an estimate. I re-checked the
prior digest (notes/standards-digest.md §5–§7) against the texts. It is correct where it overlaps
with this note, but it does not cover SCTE 43 Table 3 or the H.262 level tables.

**H.262 table numbering.** The brief's numbers differ from the text. The correct numbers are:
- Table 8-5: syntactic constraints of profiles.
- Table 8-6: maximum bits in a macroblock.
- Table 8-7: repeat_first_field.
- Table 8-8: f_code, vector range and frame_rate_code per level.
- Table 8-11: sample density.
- Table 8-12: luminance sample rate.
- Table 8-13: bit rate.
- Table 8-14: VBV size.

---

## 1. Summary

**(1) How our formats fit the standards.** All four formats we exercise are legal on both ATSC
terrestrial (A/53-4 Table 6.2, p.9) and US cable (SCTE 43 Table 3, p.3):
- 1080i29.97 and 1080p29.97 at 1920 wide: MP@HL.
- 720p59.94: MP@HL.
- 704×480i29.97, at 4:3 or 16:9: MP@ML.

For these formats, the encoder writes headers that are correct field by field:
- frame_rate_code 4 or 7.
- profile_and_level_indication 0x44 (HL) or 0x48 (ML), which is the lowest level that fits.
- vertical_size 1080 for a coded height of 1088.
- An explicit color description.
- vbv_buffer_size at most 488 units.
- bit_rate rounded up in 400 bit/s units.
- frame_pred_frame_dct = 1 in progressive sequences.
- Closed GOPs, with a sequence header before every I picture.
- vbv_delay = 0xFFFF. A/53 §6.1.5 and SCTE 43 §7.1.5 allow this value.

Luminance sample rates are inside Table 8-12. 1080i29.97 uses 62.61 of the 62.67 Msample/s
allowed, so it is 0.1 % below the HL ceiling. Our typical rates (15, 12 and 4 Mbit/s) are far
below the A/53 limit (19.4 Mbit/s) and the SCTE 43 limits (27.0 or 38.8 Mbit/s).

**(2) Do we constrain every relevant parameter?** No. The headers are right for the inputs we
give today, but almost nothing is validated. The encoder accepts the following without error:
- Any even picture size up to 4096 wide.
- Any frame-rate fraction.
- Interlaced 720p and 1080i at 59.94.
- Any aspect code.
- Any bit rate and any VBV size.
- Any search range.

What it then writes can be illegal. These are the faults:
- **frame_rate_code is wrong** for 23.976 and 24 fps. Both are written as code 8 (60 Hz). This
  also happens for any non-canonical fraction, such as 25000/1000. The cause is that the code is
  chosen from the numerator alone (headers.rs:86-95).
- **profile_and_level_indication is not the lowest level** for High-1440 formats: 1440×1080,
  720p at 30 fps or below, and 480p59.94. headers.rs:34-38 knows only ML and HL.
- **f_code can reach 9.** The vertical limit is 5 (Table 8-8). This happens when the search
  range is large (layout.rs:100).
- **The 4608-bit macroblock limit is not checked** (Table 8-6).
- **bit_rate and VBV size are not checked** against the level, A/53 or SCTE 43. The fixed-quantizer
  mode writes 80 Mbit/s at HL, which exceeds the 38.8 Mbit/s limit of A/53 and SCTE 43.
- **The buffer delay is not checked** against the 1 s T-STD limit of H.222.0 §2.4.2.6.
- **progressive_frame = 1 can be written with frame_pred_frame_dct = 0.** A/53 §6.1.6 forbids
  this. It can happen only on the device input path.
- **Caption bytes are copied unchecked** into user data.

Every fault in this list is unreachable with today's four formats and the gate command lines,
but an input or a config field can reach each of them.

**(3) Which legal formats we leave unused.** These are the most valuable:
- **Film through repeat_first_field.** Code 24 coded pictures per second and let the decoder
  repeat fields or frames:
  - 1080i29.97 with 3:2 pulldown.
  - 720p59.94 with 2:3 frame repeats. Table 8-7 allows repeat_first_field at frame_rate_code 7
    with progressive_sequence = 1.
  - 480i with 3:2 pulldown.
- **Native 23.976 and 29.97 progressive sequences:** 1080p23.976, 720p23.976 and 720p29.97.
  These need little more than the frame-rate-code fix and a level fix.
- **Cable-only reduced widths** (SCTE 43 Table 3):
  - 1440×1080 (16:9).
  - 544, 528 and 352×480 (4:3 only, interlaced 29.97 or progressive 23.976).
  - 720×480.
- **AFD.** A/53 says it should be present when the picture is letterboxed or pillarboxed.
- **intra_dc_precision of 9 or 10 bits.**

1280×1080, which the brief lists, is **not** in SCTE 43 Table 3. 1080p59.94 is not legal MPEG-2:
it needs 125.2 Msample/s against the 62.67 Msample/s limit of MP@HL.

---

## 2. What is legal

### 2.1 Compression formats

Legend:
- aspect_ratio_information: 1 = square, 2 = 4:3, 3 = 16:9.
- frame_rate_code: 1 = 23.976, 2 = 24, 4 = 29.97, 5 = 30, 7 = 59.94, 8 = 60.
- Codes 3 (25) and 6 (50) are allowed in neither table.
- "P" (`progressive_sequence` = 1) and "I" (`progressive_sequence` = 0) columns list the allowed
  frame_rate_code values.
- "Lowest level" is what profile_and_level_indication must say: A/53 Table 6.3 note (p.10) and
  SCTE 43 §7.1.3.1 (p.4). It is computed from H.262 Tables 8-8, 8-11 and 8-12 (pp.112, 114, 115),
  with P = 16⌈w/16⌉ × 16⌈h/16⌉ (or 32⌈h/32⌉ when interlaced) × frame_rate, as defined on p.115.

| vertical_size | horizontal_size | aspect | P | I | A/53-4 Table 6.2 | SCTE 43 Table 3 | Lowest level, with peak P (Msample/s) |
|---|---|---|---|---|---|---|---|
| 1080 (1088 coded) | 1920 | 1, 3 | 1, 2, 4, 5 | 4, 5 | yes | yes | HL (0x44): 62.67 at 30 Hz, 50.08 at 23.976 |
| 1080 (1088 coded) | 1440 | 3 only | 1, 2, 4, 5 | 4, 5 | **no** | yes | **H-14 (0x46)**: 46.95 at 29.97i, 47.0016 at 30p, equal to the limit |
| 720 | 1280 | 1, 3 | 1, 2, 4, 5, 7, 8 | none | yes | yes | **H-14** at 30 Hz or below (27.6); HL at 59.94 and 60 (55.2 and 55.3) |
| 480 | 720 | 2, 3 | 1, 2, 4, 5, 7, 8 | 4, 5 | **no** | yes | ML at 30 Hz or below (10.368, equal to the limit at 30i); **H-14** at 59.94 and 60p (20.7) |
| 480 | 704 | 2, 3 | 1, 2, 4, 5, 7, 8 | 4, 5 | yes | yes | ML at 30 Hz or below; **H-14** at 59.94 and 60p (20.3) |
| 480 | 640 | 1, 2 | 1, 2, 4, 5, 7, 8 | 4, 5 | yes | yes | ML at 30 Hz or below; **H-14** at 59.94 and 60p (18.4) |
| 480 | 544, 528, 352 | 2 only | 1 only | 4 only | **no** | yes | ML |

The 1088-line note differs between the two standards:
- A/53 says "the bottom 8 lines are black, per MPEG rules" (Table 6.2 note a, p.9).
- SCTE 43 says "the bottom 8 lines should be disregarded by decoders" (Table 3 note 1, p.3).

H.262 has no rule about the content of those lines: the displayable part is top-aligned
(§6.3.3, p.38). The vertical_size_value field holds 1080 and the coded height is derived from it
(§6.3.3, p.38).

Not legal in either table: 1280×1080, interlaced 720, 1080i at 59.94 or 60, 1080p at 50, 59.94 or
60, and 25 or 50 Hz in any size. 1080p59.94 also fails H.262 itself: 1920×1088×59.94 = 125.2
Msample/s, which is above the MP@HL limit of 62,668,800 (Table 8-12, p.115). The only route is
H.264 under SCTE 128-1 Table 9C (Level 4.2, frame rates 7 and 8, progressive, p.14), which is out
of scope.

### 2.2 Per-level limits that apply, Main Profile

| Limit | ML (0x48) | H-14 (0x46) | HL (0x44) | Source |
|---|---|---|---|---|
| samples/line, lines, frames/s | 720, 576, 30 | 1440, 1088, 60 | 1920, 1088, 60 | Table 8-11, p.114 |
| luminance sample rate | 10,368,000 | 47,001,600 | 62,668,800 | Table 8-12, p.115 |
| frame_rate_code | 1–5 | 1–8 | 1–8 | Table 8-8, p.112 |
| bit rate (bit_rate ≤) | 15 Mbit/s | 60 Mbit/s | 80 Mbit/s | Table 8-13, p.116 |
| VBV buffer size | 1,835,008 bits (112 units) | 7,340,032 (448 units) | 9,781,248 (597 units) | Table 8-14, p.116 |
| f_code[s][0], horizontal | 1–8 | 1–9 | 1–9 | Table 8-8, p.112 |
| f_code[s][1], vertical, frame pictures | 1–5 | 1–5 | 1–5 | Table 8-8, p.112 |
| vertical vector range, frame pictures | [−128, 127.5] | [−128, 127.5] | [−128, 127.5] | Table 8-8, p.112 |

The bit-rate and buffer limits that A/53 and SCTE 43 add on top of these levels are:

| Limit | A/53-4 | SCTE 43 | Source |
|---|---|---|---|
| bit_rate | ≤ 19.4 Mbit/s (bit_rate_value 48500); ≤ 38.8 Mbit/s (97000) in high data rate mode | ≤ 27.0 Mbit/s (67500) on 64-QAM; ≤ 38.8 Mbit/s (97000) on 256-QAM | A/53 Table 6.1 and §6.1.1, p.9; SCTE 43 Table 2 and §7.1.1, p.2 |
| vbv_buffer_size_value | ≤ 488 units (7,995,392 bits) | ≤ 488 units | same tables |

For H-14 formats, the level's 448-unit limit is the binding one, because it is lower than 488.

Main Profile has these further constraints (Table 8-5, p.110):
- chroma_format 4:2:0.
- frame_rate_extension_n and frame_rate_extension_d = 0.
- aspect_ratio_information 1, 2 or 3. Code 4 (2.21:1) is not allowed in MP.
- picture types I, P and B.
- intra_dc_precision 8, 9 or 10 bits.
- repeat_first_field constrained by Table 8-7.
- Restricted slice structure (§6.1.2.2, p.18): every macroblock is in a slice.

### 2.3 Other constraints that apply to us

**Sequence level**

- **A/53 and SCTE 43 field values.**
  - A sequence_extension follows every sequence header.
  - chroma_format = '01'.
  - horizontal_size_extension, vertical_size_extension, bit_rate_extension and
    vbv_buffer_size_extension are all 0.
  - frame_rate_extension_n and frame_rate_extension_d are 0.
  - Sources: A/53 Table 6.3 (p.10); SCTE 43 Table 4 (p.3–4).
- **Color.**
  - video_format = '000' (A/53 Table 6.4, p.10; SCTE 43 Table 5, p.4).
  - A/53 requires colour_primaries, transfer_characteristics and matrix_coefficients to be
    explicit (§6.1.4, p.10). All values from H.262 Tables 6-7 to 6-9 are allowed.
  - SCTE 43 also requires them to be explicit, except in the "NTSC formats" (480 lines,
    frame_rate_code 4, interlaced). There, absence means SMPTE 170M. SCTE 43 prefers BT.709 for all
    formats (§7.1.4, p.4).
- **Repeated sequence headers** must equal the first one, apart from the quantizer matrices. The
  sequence_extension and sequence_display_extension in them must also be identical.
- **First picture after a sequence header** must be an I or a P picture, never a B picture.
- **First frame after a GOP header** must be an I frame.
- Sources for the last three rules: H.262 §6.1.1.6 (p.12–13) and §6.1.1.7 (p.13).
- **Header field values** (H.262 §6.3.3, p.38–40):
  - horizontal_size_value ≠ 0 and vertical_size_value ≠ 0. This rules out sizes that are
    multiples of 4096.
  - bit_rate is in units of 400 bit/s, rounded up, and must not be 0.
  - bit_rate bounds the VBV input rate in both VBV modes.
  - constrained_parameters_flag = 0.
- **low_delay** = 1 means the sequence has no B pictures and may contain big pictures (§6.3.5,
  p.41).

**Picture level**

- **vbv_delay** must be ≤ 45000 unless it is 0xFFFF (A/53 §6.1.5, p.10; SCTE 43 §7.1.5, p.4).
  If any vbv_delay in a sequence is 0xFFFF, all of them must be (H.262 §6.3.9, p.48).
- **frame_pred_frame_dct** must be '1' when progressive_frame = '1' (A/53 §6.1.6, p.11; SCTE 43
  §7.1.6, p.4). This is stricter than H.262, which requires it only when progressive_sequence = 1
  (§6.3.10, p.50).
- **f_code** values are 1–9 or 15. Unused entries must be 15 (§6.3.10, p.48).
- **Vectors in range.**
  - Reconstructed vectors and predictors must lie in [low, high] (§7.6.3.1, p.74).
  - Field vectors in frame pictures may use only half the vertical range (§7.6.3.2 and
    Table 7-8, p.74–75).
  - Vectors must not refer outside the coded picture (§7.6.3.8, p.78).
- **Skipped macroblocks.** None in I pictures. A skipped macroblock in a B frame picture takes the
  direction of the previous macroblock, with frame-based prediction and vectors equal to the
  predictors (§7.6.6, p.81). The first and last macroblocks of a slice must not be skipped
  (§6.1.2, p.18).
- **Macroblock size.** At most 4608 bits per 4:2:0 macroblock. At most two macroblocks per
  macroblock row may exceed this (Table 8-6, p.110).
- **repeat_first_field** (Table 8-7, p.111):
  - Interlaced sequences: allowed at frame rates 3–8.
  - Progressive sequences: allowed only at 59.94 and 60.
  - At MP@ML with vertical_size > 480 or 25 Hz, B pictures must have repeat_first_field = 0.
  - When progressive_frame = 0, repeat_first_field must be 0 (§6.3.10, p.50).
- **temporal_reference** increments by 1 per frame in display order and restarts at 0 after a
  GOP header (§6.3.9, p.47).
- **GOP time code** counts integer frames per second when drop_frame_flag = 0. It refers to the
  picture with temporal_reference 0 (§6.3.8, p.46).
- **Dual prime** may be used only where no B pictures lie between the predicted and reference
  fields or frames (§7.6.1, p.71).
- **User data** must not contain 23 or more consecutive zero bits (§6.3.4.1, p.40).

**User data under A/53 and SCTE 43**

- At most one user_data() per user_data_type_code after each picture header (A/53 §6.2.1, p.11).
- Captions are GA94 type 0x03 cc_data() (A/53 §6.2.3.1, p.13).
- AFD is 'DTG1' and bar data is GA94 type 0x06.
  - Each persists until the next sequence header or a replacement (A/53 §6.2.3.2 and §6.2.4,
    p.13–15).
  - AFD "should" be present whenever the useful picture area is not the full coded frame
    (§6.2.4, p.15).
- SCTE 43 fixes the order of user data: SCTE 20, then A/53 captions, then AFD, then bar data
  (§7.3, p.5).
- cc_count is computed from 9600 bit/s and the presentation interval: 20 at 30i or 30p, 10 at
  60p, 25 at 24p, and alternating 20/30 for 30i with 3:2 pulldown (CTA-708 §4.3.6 and Table 4,
  p.15–16).
- The one_bit and the 4 reserved bits of each cc triplet are '1' (CTA-708 Table 2, p.10).

**VBV**

- With vbv_delay = 0xFFFF, data enters the VBV at Rmax until the buffer is full (C.3.2, p.139).
- There must be no overflow, and no underflow when low_delay = 0 (Annex C, p.138–140).

**Still pictures**

- ATSC: a stream with still pictures needs video_stream_descriptor() with
  still_picture_flag = 1, and I pictures at least every 60 s (A/53-3 §5.1.1, p.10). However,
  A/53-3 §6.2 (p.13) excludes the still-picture model from Part 3.
- Cable allows it (SCTE 54-2024 §7.8.4, p.18).

---

## 3. Enforcement table

Status values:
- **ENFORCED**: rejected at open, or clamped to a legal value.
- **CORRECT**: a fixed or derived value that is right for every input reaching it.
- **DEFAULTED**: right for the defaults, but caller input can make it wrong.
- **UNCHECKED**: no check, and the value can be illegal.
- **WRONG**: wrong for some legal input.

File paths are relative to the repo at the audit date. Kernel paths are relative to `kernels/`
(where the kernels moved, unchanged).

**Picture size and frame rate**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| horizontal_size, vertical_size | A/53 Table 6.2 or SCTE 43 Table 3; level Table 8-11; value ≠ 0 mod 4096 (§6.3.3) | Rejects 0, odd sizes and widths above 4096. Accepts any other size. A width of exactly 4096 passes, which writes horizontal_size_value = 0. | UNCHECKED | layout.rs:71-86; opts.rs:313 |
| coded size, macroblock rounding | mb_height = 2⌈h/32⌉ interlaced, ⌈h/16⌉ progressive (§6.3.3, p.38) | Implements exactly that. vertical_size_value is the display height (1080). | CORRECT | layout.rs:77-82; headers.rs:123 |
| frame_rate_code | Table 6-4 values; A/53 and SCTE 43 allow only 1, 2, 4, 5, 7, 8 | Maps 30000→4, 60000→7, 25→3, 50→6, 30→5, everything else→**8**. Gives **24000/1001→8 and 24/1→8**, and any non-reduced fraction→8. 25 and 50 are accepted although not allowed in the US. | **WRONG** | headers.rs:86-95; tested only for the six canonical fractions, headers.rs:500-512 |
| progressive_sequence per format | Table 6.2 and Table 3: 720 progressive only; interlaced only at 29.97 and 30 | Taken from the configuration without a check. | UNCHECKED | headers.rs:135; config.rs:15 |
| aspect_ratio_information | MP allows 1–3 (Table 8-5); per-format sets in Table 6.2 and Table 3 | Configured value written unchecked (any u8, masked to 4 bits). Automatic choice: 4:3 for 720 or 704×480 and for any size with w·3 = h·4, otherwise 16:9. **544, 528 and 352×480 get 16:9**, which is illegal: Table 3 allows only 2. | UNCHECKED; WRONG for the cable-only widths | headers.rs:72-82, 124 |

**Sequence header and sequence extension**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| profile_and_level_indication | Lowest level that fits (A/53 Table 6.3 note; SCTE 43 §7.1.3.1) | HL if w > 720, h > 576 or fps > 30.5; otherwise ML. **Never H-14.** Right for all four current formats. Wrong for 1440×1080, for 1280×720 at 30 Hz or below, and for 480p59.94 (all should be 0x46). Does not reject anything above HL. | WRONG for H-14 formats | headers.rs:34-38, 134 |
| luminance sample rate | Table 8-12 | Not computed. | UNCHECKED | — |
| bit_rate value and units | ×400, rounded up, ≠ 0; extension bits 0 under A/53 | div_ceil(400). Value split into 18 + 12 bits; the extension is non-zero only above 104.8 Mbit/s. | CORRECT (encoding) | headers.rs:117, 126, 139 |
| bit_rate limit | ML 15, H-14 60, HL 80 Mbit/s (Table 8-13); A/53 19.4 or 38.8; SCTE 43 27.0 or 38.8 | No upper bound. RateControl::new only checks rate ≤ max_bitrate. **The fixed-quantizer mode writes 80 Mbit/s at HL**, above the A/53 and SCTE 43 limit of 38.8. | UNCHECKED; WRONG in fixed-q | control.rs:326-339; headers.rs:118 |
| vbv_buffer_size limit | ML 112, H-14 448, HL 597 units (Table 8-14); A/53 and SCTE 43 ≤ 488 | The CLI default is min(max_bitrate/2, 488 units at HL or 112 at ML). Both the --vbv value and the API value are unbounded. Above 1023 units, a **non-zero vbv_buffer_size_extension** is written, which A/53 Table 6.3 forbids. | DEFAULTED (CLI); UNCHECKED (API) | opts.rs:482-485; headers.rs:117, 128, 141 |
| vbv_buffer_size rounding | B = 16384 × vbv_buffer_size (§6.3.3, p.40) | The CLI rounds vbv_bits down to a multiple of 16384. The API does not round, so the header declares floor(vbv/16384) while the buffer model uses the unrounded vbv_bits (up to 16383 bits more). | DEFAULTED | opts.rs:485; headers.rs:117; control.rs:344 |
| buffer delay D = vbv/max_bitrate | T-STD delay ≤ 1 s (H.222.0 §2.4.2.6, p.19) | Default is 0.5 s. Not checked. Example: --bitrate 1.5e6 at 480i with the ML default buffer gives 1.22 s. | DEFAULTED; UNCHECKED | opts.rs:483; control.rs:344 |
| constrained_parameters_flag; quantizer matrix load flags | 0 (§6.3.3) | Written as 0, so the default matrices are used. | CORRECT | headers.rs:129-131 |
| chroma_format; size extensions; frame_rate_extension | '01'; 0; 0 (A/53 Table 6.3) | Written as constants. | CORRECT | headers.rs:136-144 |
| low_delay | 0 when B pictures are present (§6.3.5) | Always 0. Legal even with no B pictures (--bframes 0). | CORRECT | headers.rs:142 |

**Sequence display extension**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| video_format; colour_description | '000'; explicit (A/53 §6.1.4; SCTE 43 §7.1.4) | video_format 000. Color is 1 (BT.709) for h > 576, else 6 (SMPTE 170M), in all three fields. Legal. **The choice is not tied to the matrix the converter actually used** (tb-convert csc.rs:48 takes `to_709` from elsewhere). | CORRECT field values; UNCHECKED coupling | headers.rs:145-152 |
| display_horizontal_size and display_vertical_size | Intended display rectangle (§6.3.6, p.45) | Set to w×h, which gives 1920×1080 for 1080-line formats. Consistent with A/53 §6.2.4 (the area of interest is the full frame unless AFD says otherwise). | CORRECT | headers.rs:153-155 |
| sequence_display_extension in every repeated header | §6.1.1.6 | Always written with the sequence header. | CORRECT | headers.rs:145 |

**GOP and repeated headers**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| sequence header repetition; first picture after it | §6.1.1.6: identical; first picture I or P | The sequence header and the GOP header are written only on key (I) frames. The values are constant per session. set_rate never changes bit_rate. | CORRECT | batch.rs:531-532; tick.rs:786-794 |
| GOP header: closed_gop, broken_link | §6.3.8 | closed_gop = 1, broken_link = 0. The GOP structure is truly closed: I first in display order, and the frame before a key frame is always a P picture. | CORRECT | headers.rs:170-171; gop.rs:90-121 |
| time_code | §6.3.8: drop_frame_flag 0 counts round(fps) frames per second; refers to the picture with temporal_reference 0 | lround(fps), drop_frame 0, taken from the I picture's display index, hours modulo 24. Legal. | CORRECT | headers.rs:161-169 |
| interval between I pictures | No normative limit for moving video. SCTE 54 §9.2 (p.25, informative): a sequence header about every 0.5 s. Still pictures: ≤ 60 s. | Set by gop (default 15 frames). Not checked. | DEFAULTED | config.rs:71 |

**Picture header and picture coding extension**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| temporal_reference | §6.3.9 | (disp − tref_base) mod 1024. A repeated I position keeps the old base. | CORRECT | headers.rs:174; batch.rs:586-588, 977 |
| vbv_delay | ≤ 45000 or 0xFFFF (A/53 §6.1.5) | Always 0xFFFF, so the VBV follows C.3.2. | CORRECT | headers.rs:176 |
| full_pel and forward/backward f_code in the picture header | '0' and '111' (§6.3.9, p.48) | Written that way. | CORRECT | headers.rs:177-184 |
| f_code[s][t] | 1–9 or 15; level limits (Table 8-8): vertical ≤ 5, horizontal ≤ 8 at ML | **One value for all four entries**, computed from the search range and allowed up to 9. Range 8 (default) gives f_code 4. **f_code reaches 6 or more with --range ≥ 20 (interlaced) or ≥ 31 (progressive)**, which is illegal at every level for frame pictures. Unused entries are 15. | DEFAULTED; UNCHECKED | layout.rs:95-102; headers.rs:188-201; opts.rs:333 |
| motion vector range and picture boundary | §7.6.3.1, §7.6.3.2, §7.6.3.8 | Search candidates are clamped to the coded picture and to ±(4·R2+3) full samples. The field seed allows ×3/2+2, which the f_code computation accounts for. No assertion checks the final vector against f_code. | CORRECT by construction (not asserted) | kernels.h:329, 367, 372-373, 413, 424; layout.rs:90-99 |
| intra_dc_precision | 8, 9 or 10 bits (Table 8-5) | 8 bits. The DC level is clamped to 0..255. | CORRECT | headers.rs:202; kernels.h:894 |
| picture_structure | any (frame or field) | Frame pictures only. | CORRECT | headers.rs:203 |
| top_field_first | 0 when progressive_sequence = 1 and repeat_first_field = 0 (§6.3.10) | seq.interlaced && tff. | CORRECT | headers.rs:204 |
| frame_pred_frame_dct | 1 if progressive_sequence = 1 (§6.3.10); 1 if progressive_frame = 1 (A/53 §6.1.6; SCTE 43 §7.1.6) | Taken from the batch geometry: 0 for any interlaced session with field prediction or field DCT. **progressive_frame comes from FrameSource::Device{progressive}. With progressive = true in an interlaced session, the code writes progressive_frame = 1 with frame_pred_frame_dct = 0, which violates A/53 and SCTE 43**, and the kernels still choose field modes. | WRONG (latent: device input path only) | session.rs:160, 163; headers.rs:205, 211-213; tick.rs:245-248, 710-716 |
| concealment_motion_vectors | any | 0. | CORRECT | headers.rs:206 |
| q_scale_type, intra_vlc_format, alternate_scan | any | 1; configurable; configurable (default 1 for interlaced). | CORRECT | headers.rs:207-209; layout.rs:103 |
| repeat_first_field | Table 8-7 | Always 0. | CORRECT (unused) | headers.rs:210 |
| chroma_420_type | = progressive_frame for 4:2:0 (§6.3.10) | Equal by construction. | CORRECT | headers.rs:211-213 |

**Slice and macroblock layer**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| restricted slice structure | §6.1.2, §6.1.2.2 | One slice per macroblock row that covers every macroblock. The first and last macroblock of each row are never skipped. This holds in both the coded and the repeated pictures. | CORRECT | kernels.h:1140-1142; headers.rs:249-274 |
| skipped-macroblock rules | §7.6.6: none in I pictures; P frame skip has a zero vector; B frame skip takes the previous macroblock's direction and the predictors | P pictures skip only with a zero frame vector. B pictures skip only when the macroblock has the same mode as its left neighbor and its vectors equal the predictors (field-vector predictors are doubled). Intra macroblocks are never skipped. | CORRECT | kernels.h:1140-1165 |
| intra DC prediction reset | §7.2.1 | Reset to 128 after a non-intra macroblock and at the slice start. | CORRECT | kernels.h:1170-1177 |
| coefficient levels and escape | −2047..2047 (−2048 forbidden) | Levels clamped to 2047. | CORRECT | kernels.h:1019, 1030 |
| saturation and mismatch control | §7.4.3, §7.4.4 | Saturation to ±2048. The mismatch control toggles F[7][7]. | CORRECT | kernels.h:1073, 1078 |
| quantizer matrices | defaults (§6.3.11) | INTRA_Q is the default matrix. The non-intra weight is hard-coded as 16. | CORRECT | tables.rs:19; kernels.h:1023, 1072 |
| bits per macroblock | ≤ 4608, at most two per row above it (Table 8-6, p.110) | **Not checked anywhere.** d_mbbits holds every macroblock's size, but nothing compares it with the limit. A noisy intra macroblock at a low quantizer can exceed the limit [K: about 63 coefficients × 6 blocks × 12 bits ≈ 4.5 kbit]. The most likely triggers are the fixed-quantizer mode, or a busy patch in an otherwise easy CBR picture. | UNCHECKED | layout.rs:302 (d_mbbits); no check |
| output size | — | The access unit is truncated to w·h·3/2 bytes. Under CBR an over-size picture forces another pass. Under a fixed quantizer it is **cut silently**, giving a corrupt picture. | UNCHECKED (fixed-q only) | enc.h:1583, 1734; batch.rs:1048, 1153 |

**Buffer model**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| VBV: no underflow, no overflow | Annex C, C.3.2 and C.5–C.6 (p.139–140) | Operating model: each picture leaves the mux queue by e_n + D. The decoder holds at most vbv_bits. The buffer ladder (rungs 32–37) ends in an all-skipped picture. An I picture that cannot fit even at rung 36 (DC only) is **counted as a violation, not prevented**, but the minimum rate requires 100 bits per macroblock to fit. | ENFORCED (best effort) | vbv.rs:1-10; control.rs:326-339; enc.h:116-124 |
| fixed-quantizer mode | — | No rate control, so the stream is not VBV-checked. The header claims the level's maximum bit_rate and buffer. A test mode. | UNCHECKED | headers.rs:118-119; control.rs:430-438 |

**User data, captions, AFD and sequence end**

| Parameter | Limit (citation) | What the code does | Status | Where |
|---|---|---|---|---|
| user data: GA94 captions | A/53 §6.2; CTA-708 Table 2; §6.3.4.1 (fewer than 23 zero bits) | Header bytes, cc_count capped at 31, and marker_bits are correct. **Caller bytes are copied verbatim.** A length that is not a multiple of 3 writes a partial triplet. Triplet marker bits are not forced to '11111', so all-zero triplets would emulate a start code. cc_count per frame rate is the caller's job; the test driver uses 20 or 10, and would wrongly use 20 at 24p. | UNCHECKED (input) | headers.rs:215-233; run.rs:271-296 |
| AFD and bar data | A/53 §6.2.3.2 and §6.2.4: should be present when the image is not the full frame | Not generated. Without AFD, the receiver uses the full frame at the sequence header's aspect ratio (A/53 §6.2.4, p.15). This is legal. | CORRECT (absent) | — |
| sequence_end_code | ends a sequence (§6.2.2 syntax); needed before any change in header parameters (§6.1.1.6) | Provided as a constant for the caller. Only the test driver appends it; the encoder marks the last access unit. | DEFAULTED | headers.rs:277-278; tick.rs:614; run.rs:502 |
| padding of the 1088-line frame | No rule in H.262. A/53 Table 6.2 note a says "black". SCTE 43 says decoders disregard the lines. | Edge replication (copies row h−1). | CORRECT (H.262); differs from the A/53 note | kernels.h:16-30 |

---

## 4. Gaps, ranked by consequence

Each gap has a minimal fix. Tier A gaps produce a bitstream that H.262 does not allow, or that
decoders will misread. Tier B gaps are legal H.262 but not allowed by A/53 or SCTE 43. Tier C
gaps are legal but suboptimal, or are robustness gaps.

### Tier A: illegal stream, or a decoder misreads it

1. **frame_rate_code is chosen from the numerator alone** (headers.rs:86-95).
   - Effect: 23.976 and 24 fps are labeled 60 Hz, and so is any non-reduced fraction such as
     25000/1000 or 2997/100. A decoder then displays at 2.5× speed. In a transport stream, PTS
     and DTS still pace the decoder, but the display cadence and the VBV examination intervals
     are wrong.
   - Fix: reduce the fraction and match it exactly against Table 6-4. Return a code or an
     error, and refuse codes 3 and 6 for the US profile. Also add tests for 24000/1001, 24/1,
     60/1 and 50000/1000.
2. **No format whitelist at open** (tick.rs:450-453; layout.rs:71-86).
   - The encoder accepts:
     - Any even size, including 1088 written as vertical_size and widths of 4096 or more.
     - Interlaced 720p.
     - Interlaced at frame_rate_code 7 or 8.
     - 25 and 50 Hz.
     - Aspect codes 0, 4 or more.
     - Sizes above MP@HL.
   - Fix: one `validate(&SessionConfig, Plant::{Terrestrial, Cable64, Cable256})` at open. Check
     against the union of A/53 Table 6.2 and SCTE 43 Table 3 (§2.1 above), and refuse anything
     else with EncoderError::Config. It also gives the aspect code a per-format default, which
     fixes the 16:9 default for 544, 528 and 352×480.
3. **f_code is uncapped** (layout.rs:95-102).
   - Effect: with --range ≥ 20 on interlaced or ≥ 31 on progressive, f_code becomes 6 or more.
     The vertical f_code of frame pictures must be at most 5 at every level (Table 8-8, p.112).
   - Fix: refuse a range2 that needs f_code > 5. Do not just cap f_code, because the search
     would then make vectors that f_code cannot code. To code wider horizontal vectors, write
     separate horizontal and vertical f_codes. This also needs the vertical search kept within
     ±127.5.
4. **Maximum bits per macroblock is not checked** (Table 8-6).
   - Effect: an illegal stream. The chance is low in CBR at our rates and real under a fixed
     quantizer. [K] Most decoders tolerate it; a conformance checker will not.
   - Fix: k_ent_pack already writes d_mbbits. Add a per-row count of macroblocks above 4608 bits
     to the row statistics. When a row has more than 2, treat the picture as `must` in
     Batch::retry: raise that row's quantizer, or lower keep.
5. **Fixed-quantizer streams are not VBV streams.**
   - Effects: the header claims 80 Mbit/s at HL, above A/53 and SCTE 43 (headers.rs:118). The
     macroblock limit is unchecked (gap 4). An over-size picture is truncated silently (enc.h:1734).
   - Fix: refuse fixed-q outside test builds, or at least write bit_rate ≤ 38.8 Mbit/s and ≤ 488
     units. Make truncation a hard error.

### Tier B: legal H.262, but not allowed by A/53 or SCTE 43, or violates a transport-layer rule

6. **bit_rate and vbv_buffer_size are not checked** against the level (Tables 8-13 and 8-14), A/53
   Table 6.1 (19.4 or 38.8 Mbit/s, 488 units) or SCTE 43 Table 2 (27.0 or 38.8 Mbit/s, 488
   units).
   - Effect: a buffer above 1023 units writes a non-zero vbv_buffer_size_extension, which A/53
     Table 6.3 forbids. An ML session above 15 Mbit/s violates MP@ML.
   - Fix: in validate(), require max_bitrate ≤ min(level limit, plant limit) and vbv_bits ≤
     min(level VBV, 488 units). Round vbv_bits down to 16384 there, not only in the CLI
     (opts.rs:485).
7. **The buffer delay is not checked against the T-STD 1 s limit** (H.222.0 §2.4.2.6, p.19).
   - Effect: vbv_bits / max_bitrate can exceed 1 s, for example 480i at 1.5 Mbit/s with the ML
     default buffer.
   - Fix: require vbv_bits ≤ max_bitrate × (1 s − mux margin). The defaults already give 0.5 s.
8. **progressive_frame = 1 is written with frame_pred_frame_dct = 0** in interlaced sessions fed
   with `FrameSource::Device { progressive: true }`. A/53 §6.1.6 and SCTE 43 §7.1.6 forbid this.
   - Effect: latent today, because the CLI refuses device input.
   - Fix now: write progressive_frame = 0 until there is a per-picture frame-only mode. The real
     fix is a per-session or per-picture "frame only" flag in SessPar. The kernels must then
     choose frame prediction and frame DCT, and the header must write frame_pred_frame_dct = 1.
     Field DCT and field prediction are batch-level (Geo) today.
9. **profile_and_level_indication is not the lowest level** for H-14 formats (headers.rs:34-38).
   - Effect: none for today's formats. For 1440×1080, 720p at 30 Hz or below, and 480p59.94,
     the stream says HL. The H-14 limits (448-unit buffer, 60 Mbit/s) also apply but are not
     enforced.
   - Fix: compute the level from the luminance rate, the size and frame_rate_code (Tables 8-8,
     8-11 and 8-12). Return 0x48, 0x46 or 0x44, and use the per-level limits in validate().
10. **The color description is chosen by height, not by what the converter produced**
    (headers.rs:149 versus tb-convert csc.rs:48).
    - Effect: legal either way, but SD converted with a BT.709 matrix would be mislabeled.
    - Fix: carry `color: (primaries, transfer, matrix)` in SessionConfig and give the same value
      to the converter.

### Tier C: legal but suboptimal, or a robustness gap

11. **Caption bytes are not sanitized** (headers.rs:215-233).
    - Fix: truncate to a multiple of 3 and OR 0xF8 into byte 0 of every triplet. Both changes
      follow CTA-708 Table 2 and guarantee no start-code emulation (§6.3.4.1).
12. **sequence_end_code is left to the caller.**
    - Effect: the product server must append it when a session closes, and whenever header
      parameters change (format, ceiling, aspect). Only the test driver does so today
      (run.rs:502).
    - Fix: document this in the AccessUnit API, or emit the code inside the last access unit.
13. **concealment_motion_vectors = 0.** On a QAM plant with occasional errors, concealment
    vectors in I pictures help decoders hide lost slices [K]. Legal and optional.
14. **Bottom 8 lines.** A/53's informative note says black; we replicate the last row. Legal
    under H.262, and SCTE 43 tells decoders to ignore the lines. No change is needed.
15. **The cc_count of the test driver is 20 at 24p**; CTA-708 Table 4 gives 25. This affects
    the test driver only.

---

## 5. Formats left on the table, ranked by value for this plant

The plant carries film and scripted TV, live and producer-pushed content at 30 and 60 fps, and SD
channels. Legality labels:
- **legal both**: allowed by A/53 Table 6.2 and SCTE 43 Table 3.
- **cable only**: allowed by SCTE 43 Table 3 only.
- **not legal**: allowed by neither.

1. **Film through repeat_first_field.** Legal both: repeat_first_field is not restricted by
   A/53 or SCTE 43 beyond H.262 Table 8-7. This is the owner's request, and it is the biggest
   gain for film and scripted TV.
   - Variants:
     - **1080i29.97 3:2**: progressive_sequence 0, frame_rate_code 4, 24 coded frames per second,
       each with progressive_frame = 1. repeat_first_field alternates 0/1 and top_field_first
       follows the field cadence.
     - **720p59.94 2:3 frame repeat**: progressive_sequence 1, frame_rate_code 7,
       repeat_first_field = 1 with top_field_first 0 or 1 to show each frame 2 or 3 times
       (§6.3.10, p.49–50; Table 8-7 allows this at code 7). The decoder outputs 60 frames per
       second from 24 coded frames.
     - **480i 3:2**: MP@ML allows repeat_first_field in B pictures at 480 lines (Table 8-7 rules,
       p.111).
   - What it buys:
     - 20 % fewer coded pictures at 1080i and 60 % fewer at 720p.
     - Every picture is coded as a progressive frame (frame DCT and prediction).
     - [K] A typical saving is 15–30 % of bit rate at equal quality on film.
   - Cost: medium to large. Something must detect or receive the cadence, so the converter needs
     inverse telecine, or the producer supplies 24p. Everything listed below that assumes one
     coded picture per frame period must change.
   - **Status (2026-11-26): built**, see `notes/film-mode-log.md`. The list below was checked
     against the code of that day; every item is resolved there ("The list of
     format-conformance.md, item by item").
   - **Code that assumes one picture per frame period:**
     - `PicInfo` has no repeat_first_field field, and headers.rs:210 writes 0. top_field_first
       is per session (session.rs:159, cfg.tff). It must become per picture.
     - frame_pred_frame_dct and field modes come from the batch Geo (layout.rs:114;
       session.rs:160), but A/53 §6.1.6 needs frame_pred_frame_dct = 1 on every
       progressive_frame = 1 picture. A per-session or per-picture flag is needed in SessPar and
       the kernels (k_me and the mode decision).
     - DTS: `Session::dts()` = pts0 + (n_coded − 1) × T (session.rs:127-135). With repeated
       fields, DTS must advance by 2 or 3 field periods per picture.
     - PTS in the driver: n × 90000 × fps_d / fps_n (run.rs:860). The producer path will need
       the same change [not located in the product server, which is outside this audit].
     - Buffer model time: `Session::t()` = n_coded / fps (session.rs:138-140), and e_n = n × T
       (vbv.rs:3; enc.h:116). The VBV examination intervals become 2T or 3T for progressive
       sequences (C.9, p.140) and the field-based equivalent for interlaced ones (C.10)
       [C.10 not re-read].
     - Rate control: per-picture time t = 1/fps and the GOP budget rate × len × t
       (control.rs:440, 455). min_rate assumes one picture per frame period (control.rs:281-293).
     - GOP grid in display frames (gop.rs:90-121; config.rs:19). The 0.5 s key interval must be
       counted in time, not in coded pictures.
     - GOP time_code counts coded pictures as frames (headers.rs:161-169). Under 3:2 it must count
       output frames at 30.
     - Tick cadence: one tick per frame period (tick.rs:18; run.rs:559, 700). With 3:2 there are
       4 coded pictures per 5 periods, or 2 per 5 for 720p.
     - Shedding and repeats: `FrameSource::Again` and `host_repeat` make whole repeated pictures
       (session.rs:175-200). With repeat_first_field, a repeat should become a flag instead.
     - The batch key `same_format` includes fps (config.rs:98-107). A film session at 24 coded
       pictures per second and a video session at 29.97 would land in different batches.
     - cc_count must follow CTA-708 Table 4: alternating 20/30 at 30i with pulldown, and 25 per
       picture at native 24p.
     - temporal_reference counts coded frames. This is already correct with repeat_first_field
       (§6.3.9).
2. **Native progressive sequences at the film rate and at 30.** Legal both.
   - Formats: 1080p23.976 and 1080p24 (frame_rate_code 1 and 2); 720p23.976 and 720p29.97
     (codes 1 and 4).
   - Cost: small. Fix gap 1 (frame_rate_code) and gap 9 (720p at 30 Hz or below is H-14). The
     tick then runs at the lower rate.
   - What it buys: the same picture savings as item 1, without the decoder doing pulldown.
   - [K] Risk: some cable-ready TVs handle 1080p24 MPEG-2 badly. 1080i with
     repeat_first_field is what US broadcast film carriage uses.
   - Worth doing first, as a stepping stone and for producers that push 24p.
3. **1440×1080i29.97 and 1440×1080p at codes 1, 2, 4 and 5.** Cable only; aspect 3 only.
   - What it buys: 25 % fewer luma samples. [K] This means roughly a 10–20 % lower rate at equal
     subjective quality on soft content, and a loss on sharp content.
   - Cost: small to medium. Needs the H-14 level (0x46), the 448-unit buffer and 60 Mbit/s
     limits, and a horizontal scale in tb-convert, which has a polyphase scaler.
   - Risk: SCTE 54-2024 §7.4 (p.14) says MPEG-2 video "shall comply with" A/53 Part 4, which
     does not list 1440. See the open questions.
4. **AFD, and bar data where needed.** Legal both. A/53 says AFD should be present when the
   image is letterboxed or pillarboxed.
   - What it buys: correct framing on 16:9 SD channels and on 4:3 material carried in HD
     (pillarbox, AFD 1001).
   - Cost: small. Add one more user_data() after the captions in headers.rs (order per SCTE 43
     §7.3), and a per-session or per-frame AFD value.
5. **Reduced SD widths: 544, 528 and 352×480.** Cable only; interlaced at 29.97 or progressive
   at 23.976; **4:3 only**. Also **720×480**, which is cable only.
   - What it buys: [K] 544 wide saves roughly 15–25 % on low-rate SD channels. 352 wide is "half
     D1" for very low rates.
   - Cost: small. Needs the whitelist, the aspect fix and a converter scale.
   - These widths cannot carry 16:9 SD, because Table 3 allows aspect 2 only.
6. **intra_dc_precision 9 or 10.** Legal both (MP: Table 8-5, p.110).
   - What it buys: [K] less contouring in dark, flat gradients in HD at moderate rates.
   - Cost: small. Change the DC quantization at kernels.h:894, the reconstruction (8·l) at
     kernels.h:1071, the DC VLC size range, the DC predictor reset (128 → 256 or 512) and the
     header field.
7. **Custom quantizer matrices.** Legal both.
   - What it buys: [K] modest perceptual gains, or tuning for low-rate SD.
   - Cost: medium. The non-intra weight is hard-coded as 16 (kernels.h:1023, 1072), and the
     matrices must be written in the sequence header (or quant_matrix_extension) and kept the
     same in every repeated header.
8. **Longer or open GOPs.** Legal both: there is no normative maximum I-picture interval for
   moving video in A/53, SCTE 43 or SCTE 54. SCTE 54 §9.2 (p.25) is informative and suggests a
   sequence header about every 0.5 s.
   - What it buys: [K] 5–10 % at 1 s instead of 0.5 s, at the cost of longer channel change.
   - Open GOPs (leading B pictures that reference the previous GOP) save a little more but need
     broken_link handling at splices. Worth trying 1 s GOPs on film channels only.
9. **480p59.94 and 480p60 (704, 640 and 720 wide).** Legal both (720 wide: cable only).
   - Needs H-14 signaling. Niche, for SD sports. 480p at 29.97 or 23.976 is ML and adds little
     over 480i.
10. **Still pictures** (H.222.0 §2.1.70: one intra picture per sequence).
    - Cable allows them (SCTE 54 §7.8.4). They need video_stream_descriptor with
      still_picture_flag = 1 and I pictures at least every 60 s (A/53-3 §5.1.1). A/53-3 §6.2
      excludes the model for terrestrial.
    - Not worth it: our skipped-picture repeat (headers.rs:241-275) already costs only about
      mbh × 10 bytes per picture (min_rate, control.rs:288-293) and stays an ordinary stream.
11. **Concealment motion vectors.** See gap 13. Small cost; worth it if field error reports show
    slice loss.
12. **Field pictures.** Legal both.
    - Large cost: a second picture structure through the ME, mode, entropy and header code.
    - [K] Small gain over frame pictures with field prediction and field DCT. Not worth it.
13. **Dual prime.** Legal only with no B pictures between the prediction and its reference
    (§7.6.1, p.71). Not useful with M = 3. No.
14. **low_delay = 1.** Only without B pictures, and with "big pictures". No use for broadcast.
15. **Pan-scan** (picture_display_extension, §6.3.12). Legal, but A/53 relies on AFD instead
    [K: receivers largely ignore pan-scan]. No.
16. **4:2:2.** **Not legal.** Main Profile is 4:2:0 only (Table 8-5), and A/53 Table 6.3 and
    SCTE 43 Table 4 require chroma_format '01'.
17. **1080p59.94 and 1080p60.** **Not legal** in MPEG-2: 125.2 Msample/s is above the 62.67
    limit (Table 8-12), and the format is in neither table. Only H.264 under SCTE 128-1 Table 9C
    (Level 4.2) allows it. Out of scope.
18. **1280×1080.** **Not legal.** It is not in SCTE 43 Table 3 (p.3), contrary to the brief.
19. **3D frame packing.** No standard we hold defines MPEG-2 frame-compatible 3D signaling for
    cable. SCTE 43 only cites CEA-708.1 3D captions. **Standard not held**; skip.

---

## 6. What the transport-stream layer needs from the encoder

These are only the requirements the held texts state.

- **PES.** For the requirements below, the encoder must deliver one access unit per picture,
  starting at its first header (the sequence, GOP or picture start code), with PTS and DTS.
  `AccessUnit` already does this (au.rs; batch.rs:1187).
  - Each PES packet starts with a video access unit, aligned. The first payload byte is the
    first byte of the access unit.
  - Every PES header has a PTS, plus a DTS where it is needed.
  - For terrestrial, at most one coded frame per PES.
  - PES_packet_length = 0 and data_alignment_indicator = 1.
  - Source for the four rules above: A/53-3 §5.5.1, p.12–13.
  - Cable: PES_scrambling_control 00, and ESCR, ES_rate, CRC, private data, pack header,
    sequence counter and P-STD buffer all 0 (SCTE 54-2024 §7.7, p.15).
- **PMT.**
  - stream_type 0x02.
  - data_stream_alignment_descriptor with alignment_type 0x02 (video access unit) in the ES
    loop (A/53-3 §5.4.1, p.12).
  - At most one video ES per program (SCTE 54-2024 §7.4, p.14).
- **PTS frequency.** Coded PTS no more than 0.7 s apart (H.222.0 §2.7.4). Trivial with a PTS on
  every picture.
- **T-STD.** The mux must model the T-STD with these values:
  - EB size = vbv_buffer_size from the sequence header.
  - Multiplex buffer, ML: MBS = BSmux + BSoh + VBVmax[level] − vbv_buffer_size.
  - Multiplex buffer, H-14 and HL: MBS = BSmux + BSoh, with BSmux = 4 ms × Rmax[level] and BSoh
    = 1/750 s × Rmax[level].
  - Transfer from MB to EB uses the **leak method**, because our vbv_delay is 0xFFFF. The leak
    rate is Rmax[level] at ML and min(1.05 × bit_rate, Rmax[level]) at H-14 and HL.
  - Sources: H.222.0 §2.4.2.3, p.14–17; A/53-3 §5.1.1, p.10; SCTE 54-2024 §7.1.1, p.12.
  - Data may spend at most **1 s** in the T-STD buffers (H.222.0 §2.4.2.6, p.19). This is why
    gap 7 matters.
  - The level the encoder signals sets these buffers, which is why gap 9 matters for H-14
    formats.
- **Random access.** H.262 requires only that each repeated sequence header gives a decodable
  stream (§6.1.1.6). No normative maximum I or sequence-header interval exists for moving video.
  SCTE 54 §9.2 (informative) suggests about 0.5 s. We put a sequence header and a closed GOP on
  every I picture.
- **Still pictures**, if ever used: video_stream_descriptor() with still_picture_flag = 1, I
  pictures at least every 60 s, and a 60 s T-STD delay limit (A/53-3 §5.1.1; H.222.0
  §2.4.2.6). Not needed.
- **Sequence end and format changes.** Any change to a header parameter needs a new sequence,
  so the server must place sequence_end_code before the new sequence header. Header parameters
  include size, frame rate, the bit_rate ceiling, the VBV size and aspect (H.262 §6.1.1.6). The
  encoder flags the last access unit (`AccessUnit::last`).
- **vbv_delay = 0xFFFF.** SCTE 43 §7.1.5 (p.4, informative) notes that decoders then manage the
  buffer from PCR and PTS. The mux must not send an STD descriptor with leak_valid = 0.
- **Multiplex rate.** 256-QAM carries 38.81070 Mbit/s and 64-QAM 26.97035 Mbit/s (SCTE 54-2024
  §11, p.25). The video ceiling per program is ≤ 38.8 or 27.0 Mbit/s (SCTE 43 Table 2). In
  practice the sum of ceilings, plus audio, PSI and PSIP, must fit the multiplex. SCTE 54 sets no
  per-program video limit beyond that.

---

## 7. Open questions

1. **Does SCTE 54-2024 §7.4 override SCTE 43?** §7.4 (p.14) requires MPEG-2 video to comply with
   A/53 Part 4, not with SCTE 43. The 2009 edition (§5.4) said only "one video ES". If the plant's
   reference is SCTE 54-2024, then 1440×1080, 720×480 and 544/528/352×480 are outside it although
   SCTE 43 allows them. [K] In practice US cable has carried 1440×1080 and 528×480 MPEG-2 for
   years. We need a decision, and preferably a test on the actual TVs.
2. **Which plant profile per channel?** On 64-QAM, the bit_rate ceiling is ≤ 27.0 Mbit/s; on
   256-QAM it is ≤ 38.8. The terrestrial profile (19.4) matters only if a stream is ever
   re-broadcast. validate() needs to know which profile applies.
3. **A/53-3 on still pictures.** §5.1.1 (p.10) gives rules for still pictures, but §6.2 (p.13)
   excludes the still-picture model. Moot unless we use still pictures.
4. **How do TVs handle 1080p24 and 720p with repeat_first_field in MPEG-2?** [K] We have no test
   data. We need a test run on the TVs before choosing between items 1 and 2 of §5.
5. **Can the macroblock-bit limit be reached in CBR at our rates?** I estimated it; a gate case
   with a noisy patch in a flat frame at a high rate would settle it. Run that before building
   gap 4.
6. **H.262 C.10 to C.12.** The field-based VBV examination intervals for interlaced sequences
   with repeat_first_field were not re-read for this note. Read them before the film work.

**Standards not held:** SMPTE 2016-1 (AFD and bar data line numbering), CEA-CEB16 (receiver
handling of AFD), ETSI TS 101 154 (the AFD syntax origin; A/53-4 reproduces it), SCTE 20 and SCTE
21 (only for legacy VBI carriage), and any MPEG-2 3D signaling standard.
