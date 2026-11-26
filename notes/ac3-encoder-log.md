# AC-3 encoder (`tb-ac3`): log

Built 2026-11-26, unattended. This file covers what was built, the decisions taken and why, how
the encoder was checked, and the measurements. A/52 means ATSC A/52:2012
(`docs/external/standards/2026-11-24-atsc-itu-etsi/A52-201212-17.pdf`); clause, table and page numbers
are that edition's. The code cites them at every normative constant and rule.

## 1. What exists

`crates/tb-ac3`, pure Rust, `#![forbid(unsafe_code)]`, sans-io, one dependency (`thiserror`).

| Module | Contents | Tested by |
|---|---|---|
| `tables` | Every A/52 table the encoder and decoder need at 48 kHz (Tables 5.18, 7.6–7.16, 7.18, 7.25, 7.34) | `tests/tables_pdf.rs`: each table against the PDF's text |
| `bits` | `BitSink` trait; `BitCounter` and `BitWriter` run the same syntax code, so sizing and writing cannot disagree; `BitReader` | unit tests |
| `crc` | CRC-16 (x¹⁶+x¹⁵+x²+1), crc1 by the inverse of x^L, crc2, crcrsv; a separate bitwise LFSR as the decoder runs it | crc1/crc2 against the LFSR; a forced crc2 = syncword case |
| `mdct` | KBD (α = 5) window, long and short transforms (α = 0, ±1) via DCT-IV on a radix-2 FFT | against the O(N²) definition of §8.2.3.2; MDCT → §7.9.4 IMDCT reconstructs any long/short pattern to < 1e-5 |
| `transient` | §8.2.2 detector (4th-order Butterworth 8 kHz high-pass, three-level peak tree) | unit tests |
| `exponents` | Extraction, strategy choice (§8.2.8), processing to decodable sets (min over reuse runs and groups, ±2 limits, DC ≤ 15), grouping, decoder expansion | round trip over all bandwidths and strategies; never raises an exponent |
| `bitalloc` | §7.2.2 transcribed step by step (integer, as the decoder); split into the SNR-independent mask and the bap step | hand-worked cases (silence; one loud bin worked through by hand in the test's comment), monotonicity |
| `quantize` | Symmetric and asymmetric quantizers, grouping across channels with padding (§7.3.5) | nearest-level checks, group layout, bit count = bap counts |
| `syntax` | syncinfo, bsi, audblk, auxdata, errorcheck over any `BitSink`; the §5.5 layout rules and skip padding | `layout` cases; every frame through the parser |
| `encoder` | `Encoder`, plans, SNR offset search, packing | `tests/roundtrip.rs` |
| `parse` | Strict parser: header, both CRCs (LFSR), all six blocks down to the mantissas, §5.5 limits, §7.10.2 conditions | runs on every frame every test makes |
| `decode` | Reference decoder: rematrix undo (§7.5.4) and the §7.9.4 IMDCT procedure written literally | gives SNR without external tools |

Examples: `encode_raw` (f32le stdin → .ac3; flags `noswitch`, `noremat`, `bw=N`, `stats`),
`inspect` (per-frame and per-block contents of an .ac3 file, via the parser), `throughput`,
`stage_cost`.

API:

```rust
let cfg = ConfigBuilder { dialnorm: Dialnorm::new(24)?, ..ConfigBuilder::new(AudioCodingMode::STEREO, false, BitRate::Kbps192) }.build()?;
let mut enc = Encoder::new(cfg);                       // allocates everything once
let frame: &[u8] = enc.encode_frame(&[&left, &right])?; // exactly cfg.frame_bytes()
let silence: Vec<u8> = tb_ac3::silent_frame(&cfg);      // SPEC §6.6
```

## 2. Decisions

| Decision | Why |
|---|---|
| `Encoder::new(Config)` is infallible; all validation is in `ConfigBuilder::build` | The task asked for `new → Result`. Validating once in the config, including whether the smallest legal frame fits the rate, makes an encoder that exists always able to produce a valid frame; a `Result` that is always `Ok` would be noise |
| `encode_frame` returns `Result<&[u8], InputError>`, not `Frame` | Wrong channel count or length is the only failure; panicking is forbidden (ARCHITECTURE §1 rule 5). Returning a borrow of the encoder's buffer avoids an allocation per frame; callers copy into their mux buffer |
| Rates above 448 kbit/s refused | A/53 Part 5 §5.3. `BitRate` still names all 19 A/52 rates |
| acmod 0 (1+1) not offered | A/53 Part 5 §5.4 prohibits it. Mix levels and dsurmod live inside the `AudioCodingMode` variant they belong to; bsmod 7 is checked against acmod (Table 5.7) |
| Coupling off; delta bit allocation off; dynrng/compr absent (0 dB) | As asked. A/85 §8.3/§9.1 recommends choosing a DRC profile other than none: not done (§5) |
| Automatic bandwidth by rate per full-bandwidth channel: ≥ 96 kbit/s 20 kHz, ≥ 80 19 kHz, ≥ 64 18 kHz, ≥ 48 13 kHz, ≥ 32 10 kHz, else 7 kHz | Without coupling, bandwidth is the main defense against starvation. Matches FFmpeg (coupling off) at 2/0 192 (≈20 kHz) and 3/2 384 (18 kHz); wider than FFmpeg below 64 kbit/s per channel, where FFmpeg drops to 6.8 kHz. `Bandwidth::Fixed` overrides |
| Exponent strategy: new set when the sum of exponent differences over the coded bins exceeds 500 (FFmpeg's threshold), D45/D25/D15 by run length 1 / 2–3 / 4–6 (§8.2.8); a short block and the block after it always get a new set | First tried scaling 500 by bandwidth; at 3/2 192 that spent a third of the frame on exponents (clicks SNR 12.3 → 15.1 dB after the change) |
| Block switching on by default, §8.2.2 detector | Pre-echo before transients drops by 20–35 dB (table below) at a cost of ~1 dB of overall SNR on the click signal |
| Dither on, except in a short block and the block after it (§8.2.9) and in blocks whose exponents are all 24 | The last rule makes digital silence decode to exact zeros |
| Rematrixing: flag a band when min(E(L+R), E(L−R)) < min(E(L), E(R)) | §7.5.3's pseudo-code (minimum). §8.2.6's prose says "maximum", which contradicts it; the pseudo-code is followed. Not applied in a block where only one of L/R is short |
| SNR offsets per block | A common offset is found by bisection; then a water-filling pass raises blocks that can still rise (snroffste is sent in a block whose offset differs from the previous). Matters when a §5.5 limit, not the frame size, stops the common offset: one test frame went from 934 unused bits to 14 |
| Lower masking floor when every block is at the top offset with bits left | floorcod 5, 6, 7 tried in turn, kept only while every block keeps offset 63/15, so no coefficient loses resolution |
| §5.5 limits enforced | Blocks 0–1 must end before the 5/8 point; block 5's mantissas must start after it. The layout check rejects allocations that break the first; skip bytes (blocks 5, 4, 3, 2) fix the second. Both checked by the parser on every frame |
| Window computed (KBD α = 5) rather than the 5-decimal table | Exact TDAC; pinned to Table 7.33 within its printed rounding |
| No 3 Hz DC high-pass, no 120 Hz LFE low-pass (§8.2.1.3 "may"/"should") | Inputs come from our own decoder/resampler; LFE above bin 6 (656 Hz) is simply not coded |

## 3. Checks and results

### 3.1 Tables against the PDF

`tools/extract-tables.py` runs on `pdftotext -layout` output and writes
`tests/data/a52_tables.txt` (slowdec, fastdec, slowgain, dbpbtab, floortab, fastgain, bndtab,
bndsz, masktab, latab, hth[48 kHz], baptab, window, frame sizes and 5/8 sizes for all 38
frmsizecod). The constants were typed separately; `tests/tables_pdf.rs` compares every entry.
All match. floortab[7] is printed `0xf800` and carried as −2048. Extra consistency tests:
masktab agrees with bndtab/bndsz; frame words = 2 × kbit/s; the computed KBD window rounds to
every Table 7.33 entry (max difference 5.0e-6) and satisfies w[n]² + w[255−n]² = 1.

### 3.2 Frames accepted by independent decoders

`tests/external.rs::external_decoders_accept_every_frame` (ignored in the sandbox):
all seven acmods × LFE on/off × 96/192/384/448 kbit/s × pink noise, clicks, full-scale sweep
(−6 dBFS) and silence, 1 s each.

- **224 streams, 7392 frames**: FFmpeg 6.1.6 with
  `-err_detect crccheck+bitstream+buffer+explode` decoded every frame with nothing on stderr and
  the full sample count; liba52 0.8.0 (`a52dec`) decoded every frame with no "error" line and the
  right frame count.
- ffprobe: codec ac3, 48000 Hz, the right channel count and layout name (or FFmpeg's explicit
  spelling for unnamed layouts), and `bit_rate` = the nominal rate, on all 56 layout/rate pairs.
- **dialnorm**: ffprobe does not show it, so it was read through FFmpeg's decoder: with
  `-target_level -31` it applies 2^((−31 + dialnorm)/6). Measured gains −30.103, −11.038,
  −7.024, −4.014, 0.000 dB for dialnorm 1, 20, 24, 27, 31: FFmpeg reads exactly the value
  written. The crate's parser reads it too (`bsi_carries_the_configuration`).
- **Silence** (`silent_frame`, and the encoder on zeros): FFmpeg and a52dec output peak 0.0
  exactly, for 1/0, 2/0 and 3/2+LFE at 96–448 kbit/s; the reference decoder gives exact zeros.

### 3.3 Quality against FFmpeg's encoder

`quality_against_ffmpeg_encoder`: 2 s signals, decoded by FFmpeg; SNR in dB averaged over the
full-bandwidth channels (the LFE carries a 50 Hz tone and is not scored). "<15 kHz" low-passes
both signals first (511-tap Blackman sinc), which measures the coded band. FFmpeg's encoder is
run with its defaults (coupling on for multichannel) and with `-channel_coupling 0`. Delays:
256 samples for both encoders. "Pre-echo" is the error power (dBFS) in the 256 samples ending
192 samples before each click burst, the span a long transform smears noise into.

| config | signal | ours | ours <15 kHz | ours via a52dec | ffmpeg ac3 | ffmpeg <15 kHz | ffmpeg, coupling off | pre-echo ours | pre-echo ffmpeg |
|---|---|---|---|---|---|---|---|---|---|
| 1/0 96 | sine 100 Hz −20 dBFS | 90.1 | 91.1 | – | 82.2 | 82.4 | – | – | – |
| 1/0 96 | sine 1 kHz −6 dBFS | 72.8 | 74.2 | – | 77.7 | 81.9 | – | – | – |
| 1/0 96 | sine 1 kHz −40 dBFS | 71.8 | 73.3 | – | 73.5 | 77.3 | – | – | – |
| 1/0 96 | sine 10 kHz −12 dBFS | 57.6 | 63.9 | – | 59.9 | 62.8 | – | – | – |
| 1/0 96 | sweep −12 dBFS | 40.8 | 88.1 | – | 46.6 | 63.3 | – | – | – |
| 1/0 96 | white noise −20 dBFS | 2.5 | 8.2 | – | 3.5 | 8.1 | – | – | – |
| 1/0 96 | pink noise −20 dBFS | 12.7 | 16.5 | – | 13.0 | 17.6 | – | – | – |
| 1/0 96 | clicks | 31.0 | 31.7 | – | 34.5 | 36.2 | – | −88.4 | −58.2 |
| 1/0 96 | clicks, switching off | 31.8 | 32.8 | – | – | – | – | −57.1 | – |
| 2/0 96 | sine 100 Hz −20 dBFS | 60.3 | 61.9 | 60.2 | 64.0 | 64.9 | 61.8 | – | – |
| 2/0 96 | sine 1 kHz −6 dBFS | 54.5 | 55.0 | 54.5 | 58.2 | 58.9 | 62.5 | – | – |
| 2/0 96 | sine 1 kHz −40 dBFS | 60.6 | 62.2 | 56.0 | 58.7 | 59.5 | 62.3 | – | – |
| 2/0 96 | sine 10 kHz −12 dBFS | 45.9 | 46.2 | 45.9 | 0.0 | 0.0 | 0.0 | – | – |
| 2/0 96 | sweep −12 dBFS | 12.0 | 16.6 | 12.0 | 8.1 | 9.2 | 8.1 | – | – |
| 2/0 96 | white noise −20 dBFS | 1.2 | 2.1 | 1.1 | 1.0 | 1.7 | 1.4 | – | – |
| 2/0 96 | pink noise −20 dBFS | 8.0 | 9.0 | 8.0 | 9.0 | 10.3 | 9.8 | – | – |
| 2/0 96 | clicks | 23.1 | 23.2 | 23.1 | 28.7 | 29.2 | 28.7 | −82.2 | −64.7 |
| 2/0 96 | clicks, switching off | 26.0 | 26.3 | – | – | – | – | −48.8 | – |
| 2/0 192 | sine 100 Hz −20 dBFS | 73.4 | 81.8 | 69.4 | 73.2 | 82.6 | 72.3 | – | – |
| 2/0 192 | sine 1 kHz −6 dBFS | 67.3 | 74.4 | 67.3 | 69.0 | 78.9 | 71.1 | – | – |
| 2/0 192 | sine 1 kHz −40 dBFS | 66.0 | 71.2 | 57.6 | 65.8 | 75.6 | 66.2 | – | – |
| 2/0 192 | sine 10 kHz −12 dBFS | 60.9 | 64.4 | 60.8 | 58.8 | 60.7 | 62.0 | – | – |
| 2/0 192 | sweep −12 dBFS | 41.2 | 85.5 | 41.2 | 28.1 | 32.1 | 46.1 | – | – |
| 2/0 192 | white noise −20 dBFS | 2.7 | 9.0 | 2.6 | 3.6 | 7.5 | 3.8 | – | – |
| 2/0 192 | pink noise −20 dBFS | 11.9 | 16.3 | 11.8 | 12.3 | 16.8 | 12.1 | – | – |
| 2/0 192 | clicks | 39.6 | 47.0 | 39.6 | 42.4 | 52.9 | 42.1 | −98.1 | −76.1 |
| 2/0 192 | clicks, switching off | 41.1 | 51.6 | – | – | – | – | −73.3 | – |
| 2/0 384 | sine 100 Hz −20 dBFS | 74.7 | 82.1 | 69.6 | 77.7 | 93.2 | 77.7 | – | – |
| 2/0 384 | sine 1 kHz −6 dBFS | 71.4 | 83.1 | 71.3 | 74.6 | 92.2 | 74.6 | – | – |
| 2/0 384 | sine 1 kHz −40 dBFS | 66.0 | 71.3 | 57.6 | 69.3 | 84.9 | 69.3 | – | – |
| 2/0 384 | sine 10 kHz −12 dBFS | 67.7 | 81.6 | 67.4 | 69.9 | 86.1 | 69.9 | – | – |
| 2/0 384 | sweep −12 dBFS | 41.2 | 86.4 | 41.2 | 46.2 | 87.1 | 46.2 | – | – |
| 2/0 384 | white noise −20 dBFS | 7.6 | 23.4 | 7.6 | 8.0 | 22.9 | 8.0 | – | – |
| 2/0 384 | pink noise −20 dBFS | 16.7 | 32.2 | 16.6 | 18.2 | 32.3 | 18.2 | – | – |
| 2/0 384 | clicks | 41.6 | 56.9 | 41.6 | 43.0 | 84.3 | 43.0 | −110.6 | −90.7 |
| 2/0 384 | clicks, switching off | 42.6 | 88.4 | – | – | – | – | −89.4 | – |
| 2/0 448 | sine 100 Hz −20 dBFS | 74.7 | 82.1 | 69.6 | 77.7 | 93.2 | 77.7 | – | – |
| 2/0 448 | sine 1 kHz −6 dBFS | 71.4 | 83.1 | 71.3 | 74.6 | 92.2 | 74.6 | – | – |
| 2/0 448 | sine 1 kHz −40 dBFS | 66.0 | 71.3 | 57.6 | 69.6 | 85.0 | 69.6 | – | – |
| 2/0 448 | sine 10 kHz −12 dBFS | 67.7 | 81.6 | 67.4 | 70.0 | 89.1 | 70.0 | – | – |
| 2/0 448 | sweep −12 dBFS | 41.2 | 86.4 | 41.2 | 46.2 | 90.6 | 46.2 | – | – |
| 2/0 448 | white noise −20 dBFS | 7.8 | 28.5 | 7.8 | 8.1 | 27.8 | 8.1 | – | – |
| 2/0 448 | pink noise −20 dBFS | 18.2 | 37.1 | 18.2 | 19.0 | 37.4 | 19.0 | – | – |
| 2/0 448 | clicks | 41.6 | 56.9 | 41.6 | 43.0 | 88.8 | 43.0 | −110.6 | −90.8 |
| 2/0 448 | clicks, switching off | 42.6 | 88.4 | – | – | – | – | −89.4 | – |
| 3/2+LFE 192 | sine 100 Hz −20 dBFS | 51.4 | 52.4 | – | 57.0 | 58.2 | 54.8 | – | – |
| 3/2+LFE 192 | sine 1 kHz −6 dBFS | 49.6 | 50.1 | – | 54.0 | 54.9 | 53.9 | – | – |
| 3/2+LFE 192 | sine 1 kHz −40 dBFS | 54.2 | 55.3 | – | 55.2 | 56.3 | 56.0 | – | – |
| 3/2+LFE 192 | sine 10 kHz −12 dBFS | 0.4 | 0.4 | – | 0.0 | 0.0 | 0.0 | – | – |
| 3/2+LFE 192 | sweep −12 dBFS | 10.0 | 12.1 | – | 7.7 | 8.7 | 8.1 | – | – |
| 3/2+LFE 192 | white noise −20 dBFS | 1.0 | 1.7 | – | 0.6 | 1.0 | 1.2 | – | – |
| 3/2+LFE 192 | pink noise −20 dBFS | 7.4 | 8.2 | – | 8.0 | 8.9 | 8.8 | – | – |
| 3/2+LFE 192 | clicks | 14.7 | 14.7 | – | 25.9 | 26.2 | 23.0 | −73.4 | −51.3 |
| 3/2+LFE 192 | clicks, switching off | 15.3 | 15.3 | – | – | – | – | −38.6 | – |
| 3/2+LFE 384 | sine 100 Hz −20 dBFS | 58.7 | 64.6 | – | 57.0 | 64.0 | 64.5 | – | – |
| 3/2+LFE 384 | sine 1 kHz −6 dBFS | 58.8 | 63.8 | – | 63.4 | 72.2 | 63.1 | – | – |
| 3/2+LFE 384 | sine 1 kHz −40 dBFS | 61.0 | 68.1 | – | 62.3 | 73.0 | 62.7 | – | – |
| 3/2+LFE 384 | sine 10 kHz −12 dBFS | 53.7 | 57.6 | – | 55.2 | 58.4 | 55.9 | – | – |
| 3/2+LFE 384 | sweep −12 dBFS | 18.5 | 63.5 | – | 17.7 | 58.6 | 18.5 | – | – |
| 3/2+LFE 384 | white noise −20 dBFS | 2.3 | 5.6 | – | 2.7 | 5.6 | 2.8 | – | – |
| 3/2+LFE 384 | pink noise −20 dBFS | 10.1 | 12.1 | – | 11.0 | 13.6 | 10.8 | – | – |
| 3/2+LFE 384 | clicks | 26.7 | 27.0 | – | 29.8 | 30.2 | 28.8 | −84.5 | −52.4 |
| 3/2+LFE 384 | clicks, switching off | 27.3 | 27.6 | – | – | – | – | −50.0 | – |
| 3/2+LFE 448 | sine 100 Hz −20 dBFS | 61.9 | 70.1 | – | 67.4 | 80.1 | 65.7 | – | – |
| 3/2+LFE 448 | sine 1 kHz −6 dBFS | 60.5 | 67.7 | – | 66.4 | 79.3 | 66.1 | – | – |
| 3/2+LFE 448 | sine 1 kHz −40 dBFS | 61.7 | 69.7 | – | 63.6 | 76.2 | 62.8 | – | – |
| 3/2+LFE 448 | sine 10 kHz −12 dBFS | 56.4 | 62.8 | – | 57.2 | 61.9 | 58.8 | – | – |
| 3/2+LFE 448 | sweep −12 dBFS | 21.1 | 71.9 | – | 28.1 | 61.9 | 45.6 | – | – |
| 3/2+LFE 448 | white noise −20 dBFS | 2.8 | 7.7 | – | 3.4 | 8.2 | 3.2 | – | – |
| 3/2+LFE 448 | pink noise −20 dBFS | 11.7 | 15.2 | – | 12.0 | 16.1 | 11.5 | – | – |
| 3/2+LFE 448 | clicks | 30.4 | 31.0 | – | 35.0 | 36.1 | 32.4 | −87.8 | −58.3 |
| 3/2+LFE 448 | clicks, switching off | 31.2 | 32.1 | – | – | – | – | −52.9 | – |

Reading it (SNR is not loudness of artifacts; it is a check that the coder is sane and a
comparison under identical conditions):

- **Same ballpark** on noise, sweeps and tones: mostly within 0–4 dB of FFmpeg either way.
  a52dec agrees with FFmpeg's decode of our streams to 0.1 dB except where its 16-bit output
  caps the figure (~57.6 dB for the −40 dBFS tone).
- **Better**: pre-echo, by 20–35 dB at every configuration (FFmpeg's encoder has no block
  switching); full-band sweeps at 2/0 192 and below; the 10 kHz tone at 2/0 96, which FFmpeg's
  6.8 kHz bandwidth removes.
- **Worse, honestly**:
  - Clicks SNR: 1.4–5.6 dB below FFmpeg at 2/0 and 3/2 384/448, and 11 dB below at 3/2+LFE
    192 kbit/s. At that rate our 10 kHz bandwidth (vs FFmpeg's 6.8 kHz) and the exponent sets
    around transients cost bits; with `bw=0` and no switching our figure is 21.2 dB against
    FFmpeg's 23.0 (coupling off). Block switching itself costs ~1–3 dB SNR on this signal
    while removing the pre-echo.
  - In-band SNR at 384/448 kbit/s on tones: FFmpeg reaches 85–93 dB below 15 kHz where we reach
    71–83 dB. Our frames at these rates often sit at the top SNR offset with the masking floor
    the limit; FFmpeg spends more of the frame on the tone. Inaudible at these levels, but it is
    a difference. Not investigated further.
  - 3/2+LFE 192 tones: 1–5 dB below FFmpeg (coupling frees FFmpeg's bits).
- Rematrixing (pure test, L = R pink noise, 2/0 96): 11.0 dB with, 7.6 dB without.

### 3.4 Pure tests (run in the sandbox)

55 tests: tables, CRCs (including crcrsv), MDCT against the direct sum, MDCT → standard IMDCT
reconstruction with block switching, exponents, bit allocation by hand, quantizers and
grouping, layout rules, configuration validation (every rate ≤ 448 × every layout × LFE builds),
every rate × layout × LFE encoding white noise with every frame parsed strictly, silence to
exact zeros, BSI fields, input shape errors, round-trip SNR through the reference decoder,
transients producing short blocks, the §5.5 limits with all energy in one block (skip padding
exercised), frames filled to within 2 % on noise.

### 3.5 Cost

Release build, one thread, on bagel (`cargo run --release -p tb-ac3 --example throughput`;
input: tone + noise + a burst every 100 ms, so transients, exponent updates and the search all
run):

| layout | kbit/s | ms per s of audio | share of one core |
|---|---|---|---|
| 1/0 | 96 | 1.36 | 0.14 % |
| 2/0 | 96 | 1.96 | 0.20 % |
| 2/0 | 192 | 2.54 | 0.25 % |
| 2/0 | 448 | 2.52 | 0.25 % |
| 3/2+LFE | 384 | 6.32 | 0.63 % |
| 3/2+LFE | 448 | 6.20 | 0.62 % |

100 stereo tracks at 192 kbit/s ≈ 25 % of one core. Per call (`stage_cost`): long MDCT 1.1 µs,
short pair 1.0 µs, transient detector 0.9 µs per block, mask 0.5 µs, bap 0.3 µs. Nothing is
allocated per frame after `Encoder::new` (by inspection: all working storage is fixed arrays in
the encoder; the frame buffer is reused).

## 4. How to reproduce

```text
nix develop -c cargo nextest run -p tb-ac3                       # pure tests
A52DEC=$(nix build --inputs-from . nixpkgs#a52dec --print-out-paths --no-link)/bin/a52dec \
  nix develop -c cargo nextest run -p tb-ac3 --release --test external --run-ignored only --no-capture
nix develop -c cargo run --release -p tb-ac3 --example throughput -- 30
pdftotext -layout docs/external/standards/2026-11-24-atsc-itu-etsi/A52-201212-17.pdf - \
  | python3 -I crates/tb-ac3/tools/extract-tables.py | diff - crates/tb-ac3/tests/data/a52_tables.txt
```

## 5. Not done, or uncertain

- **Coupling**: not implemented. It is the main reason FFmpeg does better on 5.1 at 192 kbit/s.
- **DRC words** (dynrng, compr): not generated; A/85 recommends a DRC profile other than none.
- **Per-channel fine SNR offsets and fast-gain/other bit allocation parameters**: one fine
  offset for all channels, nominal sdcycod/fdcycod/sgaincod/dbpbcod/fgaincod.
- **High-rate tone SNR gap** to FFmpeg (§3.3) not explained.
- The transient detector's filter is our choice (the standard names no coefficients), and
  thresholds are the §8.2.2 ones; not tuned on real program material. No listening tests were
  done; all quality figures are SNR.
- The parser runs the crate's own bit allocation, so it does not independently check the bap
  tables; FFmpeg's and liba52's clean decodes (a mismatch would desynchronize mantissas and
  fail `explode`/consistency checks or produce garbage SNR) are the independent check.
- 32 and 44.1 kHz, E-AC-3, 1+1: out of scope.
- The no-allocation claim is by inspection, not by a counting allocator (that would need
  `unsafe` in a test).
