//! Mantissa quantization and grouping (A/52 §7.3, p.76–80; §8.2.11, §8.2.13, p.112–113).
//!
//! A coefficient `c` with decoded exponent `e` has the normalized mantissa `m = c·2^e`, which
//! the exponent processing keeps inside (−1, 1). bap 1–5 quantize `m` symmetrically to 3, 5, 7,
//! 11 or 15 levels with values `(2q − (L − 1))/L` (Tables 7.19–7.23, p.78–79); bap 6–15 round it
//! to a two's complement fraction of 5–16 bits (§7.3.2, p.77). Codes of bap 1, 2 and 4 are
//! grouped in threes, threes and twos into one word written at the position of the group's first
//! mantissa; groups run on across the channels of a block and the last ones are padded
//! (§7.3.5, p.79–80).

use crate::tables::{MANTISSA_BITS, N_COEFS, SYMMETRIC_LEVELS};

/// Mantissa bits of an ungrouped asymmetric quantizer, `qntztab[bap]` for bap 6–15
/// (A/52 Table 7.18, p.77).
#[inline]
fn asym_bits(bap: u8) -> u32 {
    MANTISSA_BITS[usize::from(bap)]
}

/// 2^e for e in 0..=24, exactly.
#[inline]
fn pow2(e: u8) -> f32 {
    f32::from_bits((127 + u32::from(e)) << 23)
}

/// The symmetric quantizer: the code `q` in `0..L` whose value `(2q − (L − 1))/L` is nearest
/// `m` (A/52 Tables 7.19–7.23, p.78–79).
#[inline]
pub fn symmetric_code(m: f32, levels: u32) -> u32 {
    let l = levels as f32;
    let q = ((l * m + l) * 0.5).floor();
    q.clamp(0.0, l - 1.0) as u32
}

/// The asymmetric quantizer: `m` rounded to a `bits`-bit two's complement fraction, returned as
/// the raw `bits`-bit code (A/52 §7.3.2, p.77).
#[inline]
pub fn asymmetric_code(m: f32, bits: u32) -> u32 {
    let half = (1i32 << (bits - 1)) as f32;
    let v = (m * half).round().clamp(-half, half - 1.0) as i32;
    (v as u32) & ((1u32 << bits) - 1)
}

/// The value of a (single, ungrouped) mantissa code for `bap` 1..=15, before the exponent
/// shift: what the decoder reconstructs (A/52 §7.3.2, §7.3.3, p.77–79).
pub fn mantissa_value(bap: u8, code: u32) -> f32 {
    match bap {
        1..=5 => {
            let l = SYMMETRIC_LEVELS[usize::from(bap)] as f32;
            (2.0 * code as f32 - (l - 1.0)) / l
        }
        6..=15 => {
            let bits = asym_bits(bap);
            // Sign-extend the code.
            let shift = 32 - bits;
            let v = ((code << shift) as i32) >> shift;
            v as f32 / (1i64 << (bits - 1)) as f32
        }
        _ => 0.0,
    }
}

/// Codes and widths of one channel in one block, in stream order. A grouped mantissa's group
/// word sits at the group's first mantissa with its width; the other members have width 0.
#[derive(Debug, Clone)]
pub struct Quantized {
    /// Code to write at each bin.
    pub codes: [u16; N_COEFS],
    /// Bits to write at each bin.
    pub widths: [u8; N_COEFS],
}

impl Default for Quantized {
    fn default() -> Self {
        Self {
            codes: [0; N_COEFS],
            widths: [0; N_COEFS],
        }
    }
}

/// The open group of one grouped quantizer: where its word goes, how many members it has and
/// the word so far.
#[derive(Debug, Clone, Copy, Default)]
struct OpenGroup {
    slot: Option<(usize, usize)>,
    members: u32,
    word: u32,
}

/// Grouped quantizers: (bap, members per group, levels, group width) (A/52 §7.3.5, p.80).
const GROUPED: [(u8, u32, u32, u8); 3] = [(1, 3, 3, 5), (2, 3, 5, 7), (4, 2, 11, 7)];

/// One channel of a block to quantize.
#[derive(Debug, Clone, Copy)]
pub struct ChannelInput<'a> {
    /// Transform coefficients.
    pub coefs: &'a [f32; N_COEFS],
    /// Decoded exponents.
    pub exps: &'a [u8; N_COEFS],
    /// Bit allocation pointers.
    pub bap: &'a [u8; N_COEFS],
    /// One past the last coded bin.
    pub end: usize,
}

/// Quantizes the channels of one block, in their stream order, into `out` (same length), with
/// groups shared across channels and padded at the end of the block. Returns the mantissa bits
/// written, which equals `BapCounts::block_bits` of the same baps.
pub fn quantize_block(chans: &[ChannelInput<'_>], out: &mut [Quantized]) -> u32 {
    let mut open = [OpenGroup::default(); 3];
    let mut bits = 0u32;
    for (ci, ch) in chans.iter().enumerate() {
        out[ci].widths.fill(0);
        for bin in 0..ch.end {
            let bap = ch.bap[bin];
            if bap == 0 {
                continue;
            }
            let m = ch.coefs[bin] * pow2(ch.exps[bin]);
            match bap {
                1 | 2 | 4 => {
                    let gi = usize::from(bap >> 1); // 1 → 0, 2 → 1, 4 → 2
                    let (_, size, levels, width) = GROUPED[gi];
                    let g = &mut open[gi];
                    if g.members == 0 {
                        out[ci].widths[bin] = width;
                        bits += u32::from(width);
                        g.slot = Some((ci, bin));
                        g.word = 0;
                    }
                    g.word = g.word * levels + symmetric_code(m, levels);
                    g.members += 1;
                    if g.members == size {
                        if let Some((c, b)) = g.slot {
                            out[c].codes[b] = g.word as u16;
                        }
                        g.members = 0;
                    }
                }
                3 | 5 => {
                    let levels = SYMMETRIC_LEVELS[usize::from(bap)];
                    let width = MANTISSA_BITS[usize::from(bap)];
                    out[ci].codes[bin] = symmetric_code(m, levels) as u16;
                    out[ci].widths[bin] = width as u8;
                    bits += width;
                }
                _ => {
                    let width = asym_bits(bap);
                    out[ci].codes[bin] = asymmetric_code(m, width) as u16;
                    out[ci].widths[bin] = width as u8;
                    bits += width;
                }
            }
        }
    }
    // Pad the open groups with the code of value 0, the middle level (§7.3.5, p.80).
    for (g, &(_, size, levels, _)) in open.iter_mut().zip(GROUPED.iter()) {
        if g.members > 0 {
            while g.members < size {
                g.word = g.word * levels + (levels - 1) / 2;
                g.members += 1;
            }
            if let Some((c, b)) = g.slot {
                out[c].codes[b] = g.word as u16;
            }
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitalloc::BapCounts;

    fn value_for_levels(l: u32, q: u32) -> f32 {
        let l = l as f32;
        (2.0 * q as f32 - (l - 1.0)) / l
    }

    #[test]
    fn symmetric_levels_round_to_nearest() {
        for &l in &SYMMETRIC_LEVELS[1..] {
            for i in -1000..=1000 {
                let m = i as f32 / 1000.0 * 0.9999;
                let q = symmetric_code(m, l);
                let v = value_for_levels(l, q);
                for other in 0..l {
                    let w = value_for_levels(l, other);
                    assert!((m - v).abs() <= (m - w).abs() + 1e-6, "l {l} m {m}");
                }
            }
        }
        // mantissa_value agrees with Tables 7.19–7.23.
        assert_eq!(mantissa_value(1, 0), -2.0 / 3.0);
        assert_eq!(mantissa_value(4, 10), 10.0 / 11.0);
        assert_eq!(mantissa_value(5, 7), 0.0);
    }

    #[test]
    fn asymmetric_round_trip() {
        for bap in 6u8..=15 {
            let bits = asym_bits(bap);
            let step = 1.0 / (1u32 << (bits - 1)) as f32;
            for m in [-1.0f32, -0.5, -step, 0.0, step, 0.3333, 0.75] {
                let code = asymmetric_code(m, bits);
                assert!(code < 1 << bits);
                let v = mantissa_value(bap, code);
                assert!((v - m).abs() <= step / 2.0 + 1e-6, "bap {bap} m {m} v {v}");
            }
            // The range: (1 − 2^−(bits−1)) to −1 (A/52 §7.3.2, p.77).
            assert_eq!(mantissa_value(bap, asymmetric_code(2.0, bits)), 1.0 - step);
            assert_eq!(mantissa_value(bap, asymmetric_code(-2.0, bits)), -1.0);
        }
    }

    #[test]
    fn groups_span_channels_and_count_like_bap_counts() {
        let coefs = [0.3f32; N_COEFS];
        let exps = [0u8; N_COEFS];
        let mut bap_a = [0u8; N_COEFS];
        let mut bap_b = [0u8; N_COEFS];
        // Channel A: two bap-1 mantissas, one bap-4; channel B: two bap-1, one bap-2, one bap 9.
        bap_a[3] = 1;
        bap_a[5] = 4;
        bap_a[9] = 1;
        bap_b[0] = 1;
        bap_b[1] = 2;
        bap_b[2] = 1;
        bap_b[4] = 9;
        let chans = [
            ChannelInput {
                coefs: &coefs,
                exps: &exps,
                bap: &bap_a,
                end: 20,
            },
            ChannelInput {
                coefs: &coefs,
                exps: &exps,
                bap: &bap_b,
                end: 20,
            },
        ];
        let mut out = [Quantized::default(), Quantized::default()];
        let bits = quantize_block(&chans, &mut out);
        let counts = BapCounts {
            n1: 4,
            n2: 1,
            n4: 1,
            other_bits: 8,
        };
        assert_eq!(bits, counts.block_bits());
        // The first bap-1 group starts at A[3] and holds A[3], A[9], B[0].
        assert_eq!(out[0].widths[3], 5);
        assert_eq!(out[0].widths[9], 0);
        assert_eq!(out[1].widths[0], 0);
        // The second starts at B[2] and is padded with two zero codes (1).
        assert_eq!(out[1].widths[2], 5);
        let q = symmetric_code(0.3, 3);
        assert_eq!(u32::from(out[0].codes[3]), q * 9 + q * 3 + q);
        assert_eq!(u32::from(out[1].codes[2]), q * 9 + 3 + 1);
        // The 11-level pair at A[5] is padded with the zero code 5.
        assert_eq!(u32::from(out[0].codes[5]), symmetric_code(0.3, 11) * 11 + 5);
    }
}
