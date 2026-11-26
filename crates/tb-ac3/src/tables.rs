//! The normative tables of ATSC A/52:2012 that the encoder needs, as the decoder uses them.
//!
//! Every table is cited by number and page. The numeric content was transcribed from the text of
//! the PDF by `tools/extract-tables.py`; `tests/tables_pdf.rs` compares each constant here with
//! that extraction (`tests/data/a52_tables.txt`), so an edit that drifts from the standard fails a
//! test. Tables that only the 32 and 44.1 kHz sample rates use are not carried.

/// Number of transform coefficients (bins) in one audio block (A/52 §7.9.2, p.97).
pub const N_COEFS: usize = 256;

/// Samples per channel in one audio block: 256 new samples (A/52 §5.1, p.29).
pub const BLOCK_SAMPLES: usize = 256;

/// Audio blocks per syncframe (A/52 §5.1, p.29).
pub const BLOCKS_PER_FRAME: usize = 6;

/// Samples per channel in one syncframe: 6 × 256 = 1536 (A/52 §5.1, p.29).
pub const FRAME_SAMPLES: usize = BLOCKS_PER_FRAME * BLOCK_SAMPLES;

/// Number of 1/6-octave bit allocation bands (A/52 §7.2.1, p.63; Table 7.12, p.71).
pub const N_BANDS: usize = 50;

/// syncword, `0x0B77` (A/52 §5.4.1.1, p.37).
pub const SYNCWORD: u16 = 0x0B77;

/// bsid for streams built to the main body of A/52 (A/52 §5.4.2.1, p.38).
pub const BSID: u8 = 8;

/// End bin of the LFE channel: lfeendmant = 7 (A/52 §7.1.3, p.60).
pub const LFE_END_MANT: usize = 7;

/// Number of exponent groups of the LFE channel: nlfegrps = 2 (A/52 §7.1.3, p.61).
pub const LFE_N_GROUPS: usize = 2;

/// Slow decay table, `slowdec[sdcycod]` (A/52 Table 7.6, p.70).
pub const SLOW_DECAY: [i32; 4] = [0x0f, 0x11, 0x13, 0x15];

/// Fast decay table, `fastdec[fdcycod]` (A/52 Table 7.7, p.70).
pub const FAST_DECAY: [i32; 4] = [0x3f, 0x53, 0x67, 0x7b];

/// Slow gain table, `slowgain[sgaincod]` (A/52 Table 7.8, p.70).
pub const SLOW_GAIN: [i32; 4] = [0x540, 0x4d8, 0x478, 0x410];

/// dB per bit table, `dbpbtab[dbpbcod]` (A/52 Table 7.9, p.70).
pub const DB_PER_BIT: [i32; 4] = [0x000, 0x700, 0x900, 0xb00];

/// Floor table, `floortab[floorcod]` (A/52 Table 7.10, p.70–71). The last entry is printed as
/// `0xf800`, a 16-bit two's complement value: −2048 in the signed arithmetic of §7.2.1 (p.63).
pub const FLOOR: [i32; 8] = [0x2f0, 0x2b0, 0x270, 0x230, 0x1f0, 0x170, 0x0f0, -0x800];

/// Fast gain table, `fastgain[fgaincod]` (A/52 Table 7.11, p.71).
pub const FAST_GAIN: [i32; 8] = [0x080, 0x100, 0x180, 0x200, 0x280, 0x300, 0x380, 0x400];

/// First bin of each band, `bndtab[band]` (A/52 Table 7.12, p.71).
pub const BAND_START: [u8; N_BANDS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, //
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 31, 34, 37, //
    40, 43, 46, 49, 55, 61, 67, 73, 79, 85, 97, 109, 121, 133, 157, 181, //
    205, 229,
];

/// Width of each band in bins, `bndsz[band]` (A/52 Table 7.12, p.71).
pub const BAND_SIZE: [u8; N_BANDS] = [
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
    1, 1, 1, 3, 3, 3, 3, 3, 3, 3, 6, 6, 6, 6, 6, 6, 12, 12, 12, 12, 24, 24, 24, 24, 24,
];

/// Bin number to band number, `masktab[bin]` (A/52 Table 7.13, p.72). Bins 253–255 are never
/// coded (the highest end bin is 253) and the table gives them band 0.
pub const MASK_TAB: [u8; N_COEFS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, //
    20, 21, 22, 23, 24, 25, 26, 27, 28, 28, 28, 29, 29, 29, 30, 30, 30, 31, 31, 31, //
    32, 32, 32, 33, 33, 33, 34, 34, 34, 35, 35, 35, 35, 35, 35, 36, 36, 36, 36, 36, //
    36, 37, 37, 37, 37, 37, 37, 38, 38, 38, 38, 38, 38, 39, 39, 39, 39, 39, 39, 40, //
    40, 40, 40, 40, 40, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 42, 42, 42, //
    42, 42, 42, 42, 42, 42, 42, 42, 42, 43, 43, 43, 43, 43, 43, 43, 43, 43, 43, 43, //
    43, 44, 44, 44, 44, 44, 44, 44, 44, 44, 44, 44, 44, 45, 45, 45, 45, 45, 45, 45, //
    45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 45, 46, 46, 46, //
    46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, 46, //
    46, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, //
    47, 47, 47, 47, 47, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, //
    48, 48, 48, 48, 48, 48, 48, 48, 48, 49, 49, 49, 49, 49, 49, 49, 49, 49, 49, 49, //
    49, 49, 49, 49, 49, 49, 49, 49, 49, 49, 49, 49, 49, 0, 0, 0,
];

/// Log-addition table, `latab[val]` (A/52 Table 7.14, p.73).
pub const LOG_ADD: [u8; 256] = [
    64, 63, 62, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 52, 51, 50, //
    49, 48, 47, 47, 46, 45, 44, 44, 43, 42, 41, 41, 40, 39, 38, 38, //
    37, 36, 36, 35, 35, 34, 33, 33, 32, 32, 31, 30, 30, 29, 29, 28, //
    28, 27, 27, 26, 26, 25, 25, 24, 24, 23, 23, 22, 22, 21, 21, 21, //
    20, 20, 19, 19, 19, 18, 18, 18, 17, 17, 17, 16, 16, 16, 15, 15, //
    15, 14, 14, 14, 13, 13, 13, 13, 12, 12, 12, 12, 11, 11, 11, 11, //
    10, 10, 10, 10, 10, 9, 9, 9, 9, 9, 8, 8, 8, 8, 8, 8, //
    7, 7, 7, 7, 7, 7, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, //
    5, 5, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, //
    4, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, //
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, //
    2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, //
    1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Hearing threshold at fs = 48 kHz, `hth[0][band]` (A/52 Table 7.15, p.74).
pub const HEARING_THRESHOLD_48K: [i32; N_BANDS] = [
    0x04d0, 0x04d0, 0x0440, 0x0400, 0x03e0, 0x03c0, 0x03b0, 0x03b0, 0x03a0, 0x03a0, //
    0x03a0, 0x03a0, 0x03a0, 0x0390, 0x0390, 0x0390, 0x0380, 0x0380, 0x0370, 0x0370, //
    0x0360, 0x0360, 0x0350, 0x0350, 0x0340, 0x0340, 0x0330, 0x0320, 0x0310, 0x0300, //
    0x02f0, 0x02f0, 0x02f0, 0x02f0, 0x0300, 0x0310, 0x0340, 0x0390, 0x03e0, 0x0420, //
    0x0460, 0x0490, 0x04a0, 0x0460, 0x0440, 0x0440, 0x0520, 0x0800, 0x0840, 0x0840,
];

/// Bit allocation pointer table, `baptab[address]` (A/52 Table 7.16, p.75).
pub const BAP_TAB: [u8; 64] = [
    0, 1, 1, 1, 1, 1, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, //
    6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 9, 9, 9, 9, 10, //
    10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, //
    14, 14, 14, 14, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15, 15, 15,
];

/// Quantizer levels for the symmetric quantizers, bap 1–5 (A/52 Table 7.18, p.77).
pub const SYMMETRIC_LEVELS: [u32; 6] = [0, 3, 5, 7, 11, 15];

/// Mantissa bits of the ungrouped quantizers, `qntztab[bap]` (A/52 Table 7.18, p.77). For the
/// grouped quantizers (bap 1, 2 and 4) the entry is 0: their bits are counted per group.
pub const MANTISSA_BITS: [u32; 16] = [0, 0, 0, 3, 0, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

/// Rematrixing bands at fs = 48 kHz with coupling off: first bin of each band, and the end bin of
/// the last (exclusive). Band 3 runs to coefficient 252 inclusive (A/52 Table 7.25, p.84).
pub const REMATRIX_BAND_EDGES: [usize; 5] = [13, 25, 37, 61, 253];

/// Bit rates of the frame size code table in kbit/s, indexed by `frmsizecod >> 1`
/// (A/52 Table 5.18, p.51–52).
pub const BIT_RATES_KBPS: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// Words (16 bits) per syncframe at fs = 48 kHz, indexed by `frmsizecod >> 1`
/// (A/52 Table 5.18, p.51–52). At 48 kHz both codes of a pair give the same size.
pub const WORDS_PER_FRAME_48K: [u32; 19] = [
    64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1152, 1280,
];

/// Words in the first 5/8 of the syncframe, `(framesize >> 1) + (framesize >> 3)`
/// (A/52 §7.10.1, p.104; Table 7.34, p.105).
pub const fn words_five_eighths(frame_words: u32) -> u32 {
    (frame_words >> 1) + (frame_words >> 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::needless_range_loop,
        reason = "bins and bands are the tables' indices"
    )]
    fn masktab_agrees_with_band_tables() {
        // Table 7.13 duplicates Table 7.12 (A/52 §7.2.2.3, p.65): every bin of band k maps to k.
        for band in 0..N_BANDS {
            let start = usize::from(BAND_START[band]);
            let size = usize::from(BAND_SIZE[band]);
            for bin in start..start + size {
                assert_eq!(usize::from(MASK_TAB[bin]), band, "bin {bin}");
            }
            if band + 1 < N_BANDS {
                assert_eq!(start + size, usize::from(BAND_START[band + 1]));
            }
        }
        // The bands cover bins 0..253, the most a channel can code (chbwcod 60, A/52 §7.1.3).
        assert_eq!(
            usize::from(BAND_START[49]) + usize::from(BAND_SIZE[49]),
            253
        );
    }

    #[test]
    fn frame_sizes_are_two_words_per_kbps() {
        // At 48 kHz a syncframe is 1536 samples = 32 ms, so words = kbps × 32 / 16
        // (A/52 Table 5.18, p.51; standards digest §5.3).
        for (kbps, words) in BIT_RATES_KBPS.iter().zip(WORDS_PER_FRAME_48K) {
            assert_eq!(kbps * 2, words);
        }
    }

    #[test]
    fn baptab_is_monotonic() {
        assert!(BAP_TAB.windows(2).all(|w| w[0] <= w[1]));
    }
}
