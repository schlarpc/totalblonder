# Captions from timed text (`tb-captions`): design record and log

Working log of the package that turns subtitle text into TV captions (CEA-608 and CEA-708)
and reads a source's own captions. Started and finished 2026-11-26, unattended (the owner
was asleep; decisions below are mine and say why).

## State

Built, on branch `worktree-agent-aa35b04e1145cf1e5` from `60ca408`:

1. `b1b378b` timed text in: SubRip and WebVTT parsers, the cue model;
2. `6230c08` CEA-608: odd-parity bytes, codes, character sets, row encoding, a decoder;
3. `3f6c67e` CEA-708: commands, code sets, service blocks, packets, a decoder;
4. `afa623c` layout, the scheduler (`Captioner`), `cc_data()` assembly, pass-through, merge;
5. `fd57354` (other crate) `tb-m2venc --cc-text`: the driver feeds the encoder our bytes;
   sandbox test through the reference device; ignored test with ffmpeg and CCExtractor;
6. `b2c2e18` (other crate) the same on film pictures and at 59.94 Hz;
7. `2f43bb7` the `caption_service_descriptor()` of A/65 for the tracks;
8. this log.

Not built (see "Open"): wiring into `tb-pipe` (a channel with a text input), a gate case,
roll-up for live input in the harnesses, colors and XDS.

## How things are run

- `nix develop -c cargo nextest run -p tb-captions` (47 tests, under a second);
  `-p tb-m2venc -E 'test(text_captions)'` (2 tests, reference device, ~10 s debug).
- Real decoders, ignored test (needs ffmpeg on PATH; CCExtractor optional):
  `nix build --inputs-from . nixpkgs#ccextractor -o /tmp/ccx`, then
  `CCEXTRACTOR=/tmp/ccx/bin/ccextractor nix develop -c cargo nextest run -p tb-m2venc
  --release --run-ignored only -E 'test(real_decoders)'` (~45 s).
- By hand: `tb-m2venc --device ref --conformance h262 --input clip.yuv --size 176x144
  --interlaced --frames 330 --cc-text in.srt --output out`, then
  `ffmpeg -data_field first -f lavfi -i "movie=out.m2v[out0+subcc]" -map 0:s -c:s srt out.srt`.
- Tools seen: ffmpeg 6.1.6 (user profile), CCExtractor 0.96.5 (nixpkgs of the flake).

## Standards read

Printed page numbers. CTA-608-E: PDF page = printed + 12; CTA-708-E: printed + 9.

- **CTA-608-E §5.3, D.2 (p.78)**: odd parity on every byte; "each two-byte control code whose
  first character is in the range of 0x10 to 0x1F should be transmitted twice in successive
  frames"; doubling off by default on field 2 (D.2, B.14 p.70). Bytes 0x01-0x0F never on
  field 1 (B.11.5).
- **§4.1 Table 1 (p.10), Tables 49-53 (p.96-101)**: CC1/CC2 on field 1, CC3/CC4 on field 2;
  data channel 2 sets 0x08 in the first byte; field 2's miscellaneous codes use 0x15/0x1D
  (B.11.6, p.69). PAC rows and second-byte bases from Table 53 (row 11 is 0x10 0x40; rows
  2, 4, ..., 15 use 0x60).
- **§6.4 (p.18-25)**: standard set (Table 50: 0x2A á, 0x5C é, 0x5E í, 0x5F ó, 0x60 ú, 0x7B ç,
  0x7C ÷, 0x7D Ñ, 0x7E ñ, 0x7F ■), special (Table 49, 0x11 0x3x), extended (Tables 5-10,
  0x12/0x13 0x20-0x3F) "incorporates an automatic BS" so a standard character goes first;
  "an extended character cannot be implemented in the 32nd position" (NOTE2).
- **B.8.3 (p.65-66)**: pop-on order RCL, ENM, PAC, text, ..., (EDM), EOC; no pre-send more
  than 15 s ahead. **B.11.2 (p.68)**: "the first EOC ... occurs on the frame the captioner
  desires the caption to display". **B.11.7 (p.69)**: 1-4 frames of delay unobjectionable.
  **B.8.1**: roll-up rows RU, CR, PAC. **B.5**: base row not above the roll-up depth.
  **C.7, C.10, C.12, C.13 (p.73-75)**: receiver rules used by our decoder.
- **CTA-708-E §4.3 (p.9-17)**: cc_data(); 608 constructs first, one per field period
  (§4.3.1 rule 2); padding `cc_valid` 0 with `cc_type` 10/11, data 00 00 (§4.3.5); cc_count
  by presentation time (§4.3.6, Table 4: 20 at 30i, 20/30 with 3:2, 10 at 60p); a packet may
  span cc_data() structures (§4.3.3); "cc_type 01 is associated with a Top Field" (§4.4.2).
- **§5 (p.21-22)**: packets of 2 to 128 bytes, sequence number in b7-b6 counting 0-3,
  `packet_size_code` in pairs (0 = 64). **§6 (p.23-26)**: services 1-63 (0 forbidden), block
  headers (7 = extended), no block across packets, "every syntactic element shall be entirely
  contained in a single" packet, null header last "if space permits".
- **§7.1 (p.27-38)**: C0 (ETX 0x03, BS, FF, CR, HCR, EXT1 0x10), C1 (0x80-0x9F), G0 (0x7F ♪),
  G1 Latin-1, G2 (Table 17), G3 (caption icon only). The G2 glyphs at 0x35 and 0x7A-0x7F are
  in a symbol font in the PDF's text layer; read as • and the box-drawing characters │ ┐ └ ─
  ┘ ┌ from the table's drawing.
- **§8.10.5 (p.63-83)**: every command's bit layout; the examples of the standard are unit
  tests. The SetPenAttributes example's prose (p.78: "pen size = 0 = SMALL", "font style =
  4") disagrees with its own bytes `0x90 0x4A 0xCA` under the coding table (size 2, font 2);
  the table is followed. **§8.11 (p.84-85)**: proper order; every caption self-contained.
- **§9.4-9.10 (p.87-91)**: 32 characters per row for captions meant for 4:3 and 16:9; at least
  4 windows and 8 rows; legacy decoders ignore windows of more than 4 rows; word wrap off;
  left justification is the safe one; transparent borderless windows are in the minimum.
- **ATSC A/65 §6.9.2 (p.68-70)**: `caption_service_descriptor()`.

## Decisions

- **Time unit: milliseconds** (`Millis`), not 27 MHz: SubRip and WebVTT are milliseconds, the
  output grid is the field period (16.68 ms); conversion `ms x 60 / 1001` is exact integer
  arithmetic. Text packets of `tb_media` (`CodecId::Text`) convert through
  `Rational::rescale`. Time 0 is the first picture the captioner writes.
- **One layout for both services**: 32 cells, at most 4 rows, rows 1-15, bottom cues ending on
  row 15, top cues starting on row 1, centered unless the cue says otherwise. Widths are
  CEA-608's (a mid-row code takes a cell, a substitution like `…` -> `...` takes three, an
  extended character in the last cell counts double). CEA-708 shows the same rows in a
  32-column window, so both look the same and CEA-708 rows break where CEA-608's must.
- **Overlapping cues** become screens (each interval of the time line with its active cues,
  earlier cues on top); more than four rows: re-wrap as a paragraph, then split in time by
  characters, reported.
- **Styles**: italics and underline; bold dropped. A style change at a space costs nothing in
  CEA-608 (the mid-row code takes the space's cell); punctuation joined to a word takes the
  word's style (`<i>world</i>!`), else a change inside a word would show as a space.
- **CEA-608 pop-on**: load = RCL ENM rows, placed in free slots between the previous screen's
  EOC and this one's; EOC (and its copy) in the slot of the start; EDM at the end unless the
  next screen starts within a frame. A load never splits a doubled code; two equal codes in a
  row (two notes) get a null pair between them.
- **Slots**: slot k of a field is the k-th field period of that field's parity; a time maps to
  slot `field_period / 2`, within a field period of the cue time.
- **CEA-708 pop-on**: two alternating window sets (one window per block of consecutive rows,
  32 columns, anchor bottom center on the 75-row grid at `5 x row - 1`, left justified,
  transparent fill, pen white on black). Load: DLW (own set), DF hidden, SWA, SPC, SPA, SPL +
  text per row, ETX. Show: one packet `DLW other set; DSW this set` starting in the first DTVCC
  slot of the start field period. Clear: `DLW this set`. Not §8.11.3's CLW + TGW: Toggle
  depends on what the receiver already knows, wrong for a receiver that tuned in mid-load.
- **DTVCC packing**: elements whole in service blocks whole in packets; packets in
  consecutive slots, no padding inside (the carrier's rule); a packet may run into the next
  picture; sequence numbers assigned when a packet is sent, since the scheduler sends in
  time order, not creation order. Null block header only in a byte left over by even size.
- **Roll-up**: CEA-608 `RUn CR PAC text` per row, the codes placed before the cue time so the
  text starts on it; CEA-708 window 0 visible, DF repeated unchanged each row (receivers
  ignore an unchanged DF; a receiver that tunes in gets the window).
- **Late**: a load that does not fit before its start is reported (`Late`), then shown when
  complete if before its end (`LatePolicy::Delay`, default) or skipped (`Skip`); a screen
  that was to replace one whose EDM was canceled gives the EDM back when skipped.
- **Redundancy**: CC1 doubled, CC3 single (D.2, B.14), configurable.
- **Field order**: that of `tb_timeline::captions` (`cc_type` 01 with the top field, so a
  top-field-first picture starts with the field 2 construct), so our bytes and a source's
  carried bytes have one form. See the ffmpeg finding below.
- **Unused positions**: a field with a track sends valid null pairs (a steady CEA-608 stream);
  a field without one `cc_valid` 0; DTVCC padding `FA 00 00`.
- **Merge rule** (source captions vs ours): per CEA-608 field, the source's constructs while
  it sent a non-null pair there within 30 s; DTVCC as a whole while it sent any DTVCC within
  30 s (packets mix services and are not split). Ours resume with padding in place of the
  rest of a packet begun meanwhile. 30 s is longer than the 16 s of CTA-608-E D.4.
- **Characters CEA-708 lacks**: the em dash (not in G2) and ■ become `-` and █; anything else
  unknown `_` (§7.1.6). CEA-608 substitutions: `’` `'`, `…` `...`, dashes `-`, æ `ae`, others
  `?`, counted in the layout report.

## Validation

### Pure tests (tb-captions, 47)

- Parsers: well-formed files, every tolerated irregularity, every reported error with its
  line, 3000 random strings of the syntax's characters without a panic.
- CEA-608: parity of all 128 values; every code of Tables 49-53 on all four channels parses
  back; B.2.2's PAC examples; every character of the repertoire (96 + 15 + 64) round trips
  through codes and our decoder, with styles, on CC1-CC4; three CRs make two; a null between
  two notes makes two.
- CEA-708: every example of §8.10.5 byte for byte (DefineWindow `9A 38 4A D1 8B 0F 11`,
  SetWindowAttributes `97 64 53 88 22`, SetPenColor `91 3F C0 15`, ...); Figure 10's packet;
  the 216 characters of G0/G1/G2 round trip; packing into packets of 5 to 64 pairs for
  services 1-63; reassembly with shortened packets, orphans, sequence breaks.
- Scheduler (`tests/roundtrip.rs`), picture by picture through our decoders, every picture
  checked with `tb_m2v_syntax::captions::validate_shown` (the encoder's own check):
  - 29.97i, film 3:2 (30/20 constructs), 59.94p, 29.97p: text appears and goes within 2 field
    periods (CEA-608) and 3 (CEA-708) of the cue times; italics kept;
  - every field-1 control code twice in consecutive frames, never three times;
  - budget: never more than 9 DTVCC pairs per field period, one 608 pair per field period;
  - every CEA-608 character and every CEA-708 character through the whole chain;
  - too much text: reported, delayed or skipped as configured, screens before cleared;
  - roll-up two rows; two languages (CC3 and service 2, CC3 codes single); live cues added
    late;
  - `tb_timeline::captions::CaptionCarrier` carries our output: same CEA-608 pairs in the same
    pictures, same DTVCC constructs in order (it holds a packet that spans pictures until it is
    whole, so DTVCC may come a picture later, as designed there);
  - 20 000 random inputs to the decoders, the parser and the merger: no panic.

### Through the encoder (tb-m2venc, reference device)

- `text_captions_through_the_encoder`: 64x48 interlaced, no B pictures, three cues; the
  verifier under the cable profile finds nothing but the test picture size; the GA94 user
  data of all 120 pictures read back (60 bytes each) give CC1 within 34 ms and service 1
  within 51 ms of every cue time.
- `text_captions_on_film_and_at_59_94_hz`: `--film 3,2` (90 and 60 bytes alternating) and
  59.94p (30 bytes): no caption finding, read back within 51 ms.

### Real decoders (ignored test `real_decoders`, and by hand)

176x144 interlaced top field first, 2 B pictures, 330 frames, four cues with italics, special
and extended characters, a top cue, a wrapped line (`in.srt` of the test):

- **ffmpeg 6.1.6 ccaption_dec** (`-data_field first`): all four cues, text equal, times
  within 8 ms (1001 vs 1000: the frame grid), wrapped line as two rows, italics from the
  space before "world". Film 3:2 and 59.94p streams: the same four cues at the same times.
  The extended em dash (0x12 0x2A) shows as `-` (its fallback; ffmpeg's table).
- **ffmpeg finding: automatic field choice.** Without `-data_field first`, ffmpeg's decoder
  outputs nothing on our top-field-first streams: it locks onto the `cc_type` of the very
  first construct it sees, valid or not, and ours is field 2's (no data). The same encode
  with `--bff` decodes in automatic mode. Our order is CTA-708-E §4.4.2 ("cc_type 01 is
  associated with a Top Field") and the verifier's `CC-608-ORDER`; ffmpeg's choice is a
  decoder heuristic. It is further evidence for the open question already in
  `notes/film-mode-log.md` ("for top-field-first 1080i [field order] is widely field 1 first:
  if 608 captions flicker or drop on a set, this is the first thing to flip"); a set's
  decoder that guesses the field the same way would show nothing. To check on a real TV.
- **CCExtractor 0.96.5**, CEA-608 CC1: all four cues, text equal (em dash included), times
  about 100 ms late on this stream with B pictures (constant; it was 0 on the last cue's
  end). It reads only the first GOP of an elementary stream, ours and ffmpeg's own alike
  ("Skip forward to the next Sequence or GOP start"): the test remuxes to a transport stream
  with `ffmpeg -c copy` first.
- **CCExtractor CEA-708** (`-svc 1[ISO-8859-1]`): all four cues, times as for CEA-608, G0/G1
  text equal; it writes the G0 music note 0x7F as `&l`, drops G2's curly quotes (writes the
  raw codes), and wrote Latin-1 bytes rather than UTF-8 in the test's run (UTF-8 by hand).
  Text otherwise identical. No other CEA-708 decoder was at hand (ffmpeg has none).
- **tb-m2v-verify**, `--profile scte43-256qam`, on the 29.97i, film and 59.94p streams: no
  caption finding (CC-COUNT, CC-STATE, CC-608-POSITION, CC-608-FIELD2, CC-608-ORDER,
  CC-EVERY-PICTURE ...); only BC-FORMAT / BC-LEVEL-LOWEST / BC-COLOR-PREFERRED of a 176x144
  test picture. The verifier does not check the packet layer (`CC-PACKET` is not
  implemented); our read-back and both decoders do.

## Integration

- **Encoder**: `Captioner::picture(fields, top_first, &mut out)` gives exactly the bytes
  `Encoder::submit_shown(.., cc, shown)` validates. `fields` is the picture's field periods:
  `shown_counts(frame_rate_code, shown.units, units_per_frame).0 / 10`; `top_first` is
  `Some(shown.top_first)` on an interlaced session, `None` otherwise. `tb-m2venc --cc-text`
  is the example.
- **Pipeline (`tb-pipe`)**: not wired. Where it goes: next to the `CaptionCarrier` of a
  channel. A channel with a text input keeps a `Captioner`, adds cues as the text packets
  arrive (`Cue::from_text_packet`, `Cue::shifted` onto the channel clock), and for each output
  picture calls `picture` with the same `(pic.units, top_first)` it gives `take_shown`; with a
  source that has captions, `ccdata::Merger::merge(carrier_bytes, our_bytes, fields)`.
- **Tables**: `descriptor::caption_service_descriptor(services_of(tracks, languages))` for the
  PMT video loop and EIT events, when the mux exists.

## Open

1. Field order of CEA-608 constructs in top-field-first pictures (above): check on the TVs.
2. `tb-pipe` wiring and a gate case (`--cc-text` on the card: the bytes are made on the host,
   so the reference device's evidence carries over).
3. Live roll-up from a text source: the scheduler supports adding cues as they come (tested),
   nothing feeds it yet.
4. Colors (VTT classes), XDS (the digest recommends none), CC2/CC4 beside CC1/CC3 on one
   field (refused: one track per field).
5. A second CEA-708 decoder to compare with CCExtractor (a TV, or an HDHomeRun's player).
