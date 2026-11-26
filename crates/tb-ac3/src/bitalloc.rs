//! The parametric bit allocation, exactly as the decoder runs it (A/52 §7.2.2, p.63–70).
//!
//! The encoder must arrive at the same bit allocation pointers (bap) as every decoder, so this
//! module is a transcription of the standard's integer pseudo-code, step by step, with the
//! step's number in the comments. It is split where the encoder benefits: [`compute_mask`] runs
//! steps 7.2.2.2–7.2.2.5 (independent of the SNR offset) once per exponent set, and
//! [`compute_bap`] runs step 7.2.2.7 for each trial SNR offset of the search. Delta bit
//! allocation (step 7.2.2.6) is not used by this encoder (deltbaie = 0, §8.2.12, p.113) and
//! coupling is off, so the coupling-channel branches are not carried.

use crate::tables::{
    BAND_SIZE, BAND_START, BAP_TAB, DB_PER_BIT, FAST_DECAY, FAST_GAIN, FLOOR,
    HEARING_THRESHOLD_48K, LOG_ADD, MANTISSA_BITS, MASK_TAB, N_BANDS, N_COEFS, SLOW_DECAY,
    SLOW_GAIN,
};

/// A 2-bit or 3-bit code of a bit allocation parameter, checked against its table's length.
macro_rules! code_type {
    ($name:ident, $max:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(u8);

        impl $name {
            /// The code, if in range.
            pub fn new(code: u8) -> Option<Self> {
                (code <= $max).then_some(Self(code))
            }

            /// The code.
            pub fn code(self) -> u32 {
                u32::from(self.0)
            }

            fn index(self) -> usize {
                usize::from(self.0)
            }
        }
    };
}

code_type!(SlowDecayCode, 3, "sdcycod (A/52 §5.4.3.31, Table 7.6).");
code_type!(FastDecayCode, 3, "fdcycod (A/52 §5.4.3.32, Table 7.7).");
code_type!(SlowGainCode, 3, "sgaincod (A/52 §5.4.3.33, Table 7.8).");
code_type!(DbPerBitCode, 3, "dbpbcod (A/52 §5.4.3.34, Table 7.9).");
code_type!(FloorCode, 7, "floorcod (A/52 §5.4.3.35, Table 7.10).");
code_type!(
    FastGainCode,
    7,
    "fgaincod, lfefgaincod (A/52 §5.4.3.41, Table 7.11)."
);

/// The frame-wide parameters sent when baie = 1 (A/52 Table 5.3, p.34).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Params {
    /// sdcycod.
    pub sdcycod: SlowDecayCode,
    /// fdcycod.
    pub fdcycod: FastDecayCode,
    /// sgaincod.
    pub sgaincod: SlowGainCode,
    /// dbpbcod.
    pub dbpbcod: DbPerBitCode,
    /// floorcod.
    pub floorcod: FloorCode,
}

impl Params {
    /// The nominal values of a basic encoder (A/52 §8.2.12, p.113): sdcycod 2, fdcycod 1,
    /// sgaincod 1, dbpbcod 2, floorcod 4.
    pub const NOMINAL: Params = Params {
        sdcycod: SlowDecayCode(2),
        fdcycod: FastDecayCode(1),
        sgaincod: SlowGainCode(1),
        dbpbcod: DbPerBitCode(2),
        floorcod: FloorCode(4),
    };

    /// floor = floortab[floorcod] (A/52 §7.2.2.1, p.64).
    pub fn floor(&self) -> i32 {
        FLOOR[self.floorcod.index()]
    }
}

/// The nominal fast gain code, fgaincod = 4 (A/52 §8.2.12, p.113).
pub const NOMINAL_FAST_GAIN: FastGainCode = FastGainCode(4);

/// A coarse/fine SNR offset pair (A/52 §5.4.3.37, §5.4.3.40, p.47–48).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct SnrOffset {
    /// csnroffst, 0..=63.
    pub coarse: u8,
    /// fsnroffst, 0..=15.
    pub fine: u8,
}

impl SnrOffset {
    /// Highest combined index, `63·16 + 15`.
    pub const MAX_INDEX: u16 = 1023;

    /// From a combined index `coarse·16 + fine`, which orders offsets by size; clamped.
    pub fn from_index(i: u16) -> Self {
        let i = i.min(Self::MAX_INDEX);
        Self {
            coarse: (i >> 4) as u8,
            fine: (i & 15) as u8,
        }
    }

    /// `coarse·16 + fine`.
    pub fn index(self) -> u16 {
        u16::from(self.coarse) * 16 + u16::from(self.fine)
    }

    /// snroffset = (((csnroffst − 15) << 4) + fsnroffst) << 2 (A/52 §7.2.2.1, p.64).
    pub fn value(self) -> i32 {
        (((i32::from(self.coarse) - 15) << 4) + i32::from(self.fine)) << 2
    }
}

/// The masking curve of one exponent set before the SNR offset is applied, with the PSD it was
/// computed from: the output of steps 7.2.2.2 to 7.2.2.5.
#[derive(Debug, Clone)]
pub struct Mask {
    psd: [i16; N_COEFS],
    mask: [i16; N_BANDS],
    start: usize,
    end: usize,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            psd: [0; N_COEFS],
            mask: [0; N_BANDS],
            start: 0,
            end: 0,
        }
    }
}

/// logadd (A/52 §7.2.2.3, p.66).
#[inline]
fn logadd(a: i32, b: i32) -> i32 {
    let c = a - b;
    let address = ((c.abs() >> 1).min(255)) as usize;
    if c >= 0 {
        a + i32::from(LOG_ADD[address])
    } else {
        b + i32::from(LOG_ADD[address])
    }
}

/// calc_lowcomp (A/52 §7.2.2.4, p.67–68). The standard's text has a stray `;` after the first
/// `if`; the intended reading (and every decoder's) is a conditional assignment.
#[inline]
fn calc_lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
    if bin < 7 {
        if b0 + 256 == b1 {
            384
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else if bin < 20 {
        if b0 + 256 == b1 {
            320
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    } else {
        (a - 128).max(0)
    }
}

/// Steps 7.2.2.2 to 7.2.2.5 for a full-bandwidth or LFE channel (start = 0): PSD, band
/// integration, excitation with low-frequency compensation, masking curve against the hearing
/// threshold. `exps[start..end]` are the decoded exponents.
pub fn compute_mask(exps: &[u8], end: usize, fgaincod: FastGainCode, p: &Params, out: &mut Mask) {
    let start = 0usize;
    out.start = start;
    out.end = end;
    let sdecay = SLOW_DECAY[p.sdcycod.index()];
    let fdecay = FAST_DECAY[p.fdcycod.index()];
    let sgain = SLOW_GAIN[p.sgaincod.index()];
    let dbknee = DB_PER_BIT[p.dbpbcod.index()];
    let fgain = FAST_GAIN[fgaincod.index()];

    // 7.2.2.2 Exponent mapping into PSD (p.65).
    for (psd, &e) in out.psd[start..end].iter_mut().zip(&exps[start..end]) {
        *psd = (3072 - (i32::from(e) << 7)) as i16;
    }

    // 7.2.2.3 PSD integration (p.65–66).
    let mut bndpsd = [0i32; N_BANDS];
    {
        let mut j = start;
        let mut k = usize::from(MASK_TAB[start]);
        loop {
            let lastbin = (usize::from(BAND_START[k]) + usize::from(BAND_SIZE[k])).min(end);
            bndpsd[k] = i32::from(out.psd[j]);
            j += 1;
            while j < lastbin {
                bndpsd[k] = logadd(bndpsd[k], i32::from(out.psd[j]));
                j += 1;
            }
            k += 1;
            if end <= lastbin {
                break;
            }
        }
    }

    // 7.2.2.4 Compute excitation function (p.66–67).
    let bndstrt = usize::from(MASK_TAB[start]);
    let bndend = usize::from(MASK_TAB[end - 1]) + 1;
    let mut excite = [0i32; N_BANDS];
    let mut lowcomp = 0i32;
    let mut fastleak = 0i32;
    let mut slowleak = 0i32;
    // bndstrt is 0 for full-bandwidth and LFE channels; the coupling branch is not carried.
    debug_assert_eq!(bndstrt, 0);
    // The LFE channel is recognized as the decoder does: its bndend is 7.
    let not_lfe_last = |bin: usize| bndend != 7 || bin != 6;
    lowcomp = calc_lowcomp(lowcomp, bndpsd[0], bndpsd[1], 0);
    excite[0] = bndpsd[0] - fgain - lowcomp;
    lowcomp = calc_lowcomp(lowcomp, bndpsd[1], bndpsd[2], 1);
    excite[1] = bndpsd[1] - fgain - lowcomp;
    let mut begin = 7;
    for bin in 2..7 {
        if not_lfe_last(bin) {
            lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
        }
        fastleak = bndpsd[bin] - fgain;
        slowleak = bndpsd[bin] - sgain;
        excite[bin] = fastleak - lowcomp;
        if not_lfe_last(bin) && bndpsd[bin] <= bndpsd[bin + 1] {
            begin = bin + 1;
            break;
        }
    }
    for bin in begin..bndend.min(22) {
        if not_lfe_last(bin) {
            lowcomp = calc_lowcomp(lowcomp, bndpsd[bin], bndpsd[bin + 1], bin);
        }
        fastleak -= fdecay;
        fastleak = fastleak.max(bndpsd[bin] - fgain);
        slowleak -= sdecay;
        slowleak = slowleak.max(bndpsd[bin] - sgain);
        excite[bin] = (fastleak - lowcomp).max(slowleak);
    }
    for bin in 22..bndend {
        fastleak -= fdecay;
        fastleak = fastleak.max(bndpsd[bin] - fgain);
        slowleak -= sdecay;
        slowleak = slowleak.max(bndpsd[bin] - sgain);
        excite[bin] = fastleak.max(slowleak);
    }

    // 7.2.2.5 Compute masking curve (p.68), fscod 0.
    for bin in bndstrt..bndend {
        if bndpsd[bin] < dbknee {
            excite[bin] += (dbknee - bndpsd[bin]) >> 2;
        }
        out.mask[bin] = excite[bin].max(HEARING_THRESHOLD_48K[bin]) as i16;
    }
}

/// Mantissa counts of one exponent set that decide its size in bits: grouped quantizers are
/// counted, the others summed (A/52 Table 7.17, p.76; grouping §7.3.5, p.79–80).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BapCounts {
    /// Mantissas with bap 1 (3 per 5-bit group).
    pub n1: u32,
    /// Mantissas with bap 2 (3 per 7-bit group).
    pub n2: u32,
    /// Mantissas with bap 4 (2 per 7-bit group).
    pub n4: u32,
    /// Bits of all other mantissas.
    pub other_bits: u32,
}

impl BapCounts {
    /// Adds another set's counts (sets in one block share partial groups, §7.3.5).
    pub fn add(&mut self, o: &BapCounts) {
        self.n1 += o.n1;
        self.n2 += o.n2;
        self.n4 += o.n4;
        self.other_bits += o.other_bits;
    }

    /// Mantissa bits of one block whose counts these are: the last groups padded
    /// (A/52 §7.3.5, p.80).
    pub fn block_bits(&self) -> u32 {
        self.n1.div_ceil(3) * 5
            + self.n2.div_ceil(3) * 7
            + self.n4.div_ceil(2) * 7
            + self.other_bits
    }
}

/// Step 7.2.2.7 (p.69–70): bap for `mask` at `snroffset` and `floor`, written into
/// `bap[start..end]`; returns the counts. The caller handles the special case of
/// §7.2.2.1.1 (all SNR offsets zero: every bap is 0).
pub fn compute_bap(mask: &Mask, snroffset: i32, floor: i32, bap: &mut [u8; N_COEFS]) -> BapCounts {
    // A histogram of bap values, turned into counts at the end (no branch per bin).
    let mut hist = [0u32; 16];
    let (start, end) = (mask.start, mask.end);
    let mut i = start;
    let mut j = usize::from(MASK_TAB[start]);
    loop {
        let lastbin = (usize::from(BAND_START[j]) + usize::from(BAND_SIZE[j])).min(end);
        let mut m = i32::from(mask.mask[j]) - snroffset - floor;
        if m < 0 {
            m = 0;
        }
        m &= 0x1fe0;
        m += floor;
        for (b, &psd) in bap[i..lastbin].iter_mut().zip(&mask.psd[i..lastbin]) {
            let address = ((i32::from(psd) - m) >> 5).clamp(0, 63) as usize;
            *b = BAP_TAB[address];
            hist[usize::from(*b)] += 1;
        }
        i = lastbin;
        j += 1;
        if end <= lastbin {
            break;
        }
    }
    BapCounts {
        n1: hist[1],
        n2: hist[2],
        n4: hist[4],
        other_bits: hist
            .iter()
            .zip(MANTISSA_BITS)
            .map(|(n, bits)| n * bits)
            .sum(),
    }
}

/// Counts of an all-zero bap (the special case of §7.2.2.1.1 or an empty set).
pub fn zero_bap(end: usize, bap: &mut [u8; N_COEFS]) -> BapCounts {
    bap[..end].fill(0);
    BapCounts::default()
}

/// The whole of §7.2.2 for one full-bandwidth or LFE exponent set, as a decoder runs it:
/// `exps[0..end]` to `bap[0..end]`. `all_offsets_zero` is the condition of §7.2.2.1.1.
#[allow(clippy::too_many_arguments)]
pub fn decoder_bap(
    exps: &[u8],
    end: usize,
    p: &Params,
    fgaincod: FastGainCode,
    snr: SnrOffset,
    all_offsets_zero: bool,
    bap: &mut [u8; N_COEFS],
) -> BapCounts {
    if all_offsets_zero {
        return zero_bap(end, bap);
    }
    let mut mask = Mask::default();
    compute_mask(exps, end, fgaincod, p, &mut mask);
    compute_bap(&mask, snr.value(), p.floor(), bap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snr_offset_formula() {
        // (((csnroffst − 15) << 4) + fsnroffst) << 2 (A/52 §7.2.2.1, p.64).
        assert_eq!(
            SnrOffset {
                coarse: 15,
                fine: 0
            }
            .value(),
            0
        );
        assert_eq!(SnrOffset { coarse: 0, fine: 0 }.value(), -960);
        assert_eq!(
            SnrOffset {
                coarse: 63,
                fine: 15
            }
            .value(),
            ((48 << 4) + 15) << 2
        );
        for i in 0..=SnrOffset::MAX_INDEX {
            assert_eq!(SnrOffset::from_index(i).index(), i);
        }
    }

    #[test]
    fn logadd_cases() {
        // Equal operands add latab[0] = 64 (3 dB in units of 1/128 of 6 dB... 64 = half a step).
        assert_eq!(logadd(1000, 1000), 1064);
        assert_eq!(logadd(1000, 0), 1000);
        assert_eq!(logadd(0, 1000), 1000);
        assert_eq!(logadd(1000, 998), 1000 + 63);
    }

    #[test]
    fn lowcomp_cases() {
        assert_eq!(calc_lowcomp(0, 100, 356, 3), 384);
        assert_eq!(calc_lowcomp(100, 400, 356, 3), 36);
        assert_eq!(calc_lowcomp(30, 400, 356, 3), 0);
        assert_eq!(calc_lowcomp(30, 100, 200, 3), 30);
        assert_eq!(calc_lowcomp(0, 100, 356, 10), 320);
        assert_eq!(calc_lowcomp(200, 0, 0, 20), 72);
    }

    /// A hand-worked case: a flat spectrum of exponent 24 (silence) everywhere.
    /// psd = 0 in every bin; every band's mask sits at or above the hearing threshold, so
    /// at snroffset 0 every address is negative and every bap is 0.
    #[test]
    fn silence_gets_no_bits_at_nominal_offset() {
        let exps = [24u8; 256];
        let mut bap = [9u8; 256];
        let c = decoder_bap(
            &exps,
            253,
            &Params::NOMINAL,
            NOMINAL_FAST_GAIN,
            SnrOffset {
                coarse: 15,
                fine: 0,
            },
            false,
            &mut bap,
        );
        assert_eq!(c, BapCounts::default());
        assert!(bap[..253].iter().all(|&b| b == 0));
    }

    /// A hand-worked case: one loud bin (exponent 0, psd 3072) at bin 40 in a silent spectrum.
    ///
    /// Band 32 holds bins 40–42 (Table 7.12): bndpsd[32] = logadd(logadd(3072, 0), 0) = 3072.
    /// All other bands are 0. Bands 0–31 have bndpsd 0, so the excitation there is negative
    /// and the mask is the hearing threshold. At band 32 (≥ 22) the leaks restart from the
    /// band: fastleak = 3072 − fgain(0x280) = 2432, slowleak = 3072 − sgain(0x4d8) = 1832, so
    /// excite = 2432; bndpsd ≥ dbknee (0x900 = 2304), no knee term; mask = 2432.
    /// With csnroffst 15, fsnroffst 0 (snroffset 0) and floor 0x1f0 = 496:
    /// m = ((2432 − 0 − 496) & 0x1fe0) + 496 = 1920 + 496 = 2416;
    /// address = (3072 − 2416) >> 5 = 20 → baptab[20] = 7 for bin 40;
    /// bins 41, 42 (psd 0): address < 0 → bap 0.
    #[test]
    fn single_loud_bin_by_hand() {
        let mut exps = [24u8; 256];
        exps[40] = 0;
        let mut bap = [0u8; 256];
        let c = decoder_bap(
            &exps,
            253,
            &Params::NOMINAL,
            NOMINAL_FAST_GAIN,
            SnrOffset {
                coarse: 15,
                fine: 0,
            },
            false,
            &mut bap,
        );
        assert_eq!(bap[40], 7);
        assert_eq!(bap[41], 0);
        assert_eq!(bap[42], 0);
        assert_eq!(c.other_bits, 6);
        assert_eq!(c.n1 + c.n2 + c.n4, 0);
        // The bins below see only the hearing threshold: psd 0 against mask ≥ 752: bap 0.
        assert!(bap[..40].iter().all(|&b| b == 0));
    }

    #[test]
    fn bits_rise_with_snr_offset() {
        let exps: [u8; 256] = std::array::from_fn(|i| (2 + (i * 7) % 13) as u8);
        let mut mask = Mask::default();
        compute_mask(&exps, 253, NOMINAL_FAST_GAIN, &Params::NOMINAL, &mut mask);
        let mut bap = [0u8; 256];
        let mut last = 0;
        for i in (0..=SnrOffset::MAX_INDEX).step_by(16) {
            let s = SnrOffset::from_index(i);
            let total: u32 = {
                let c = compute_bap(&mask, s.value(), Params::NOMINAL.floor(), &mut bap);
                // Bits per mantissa × 6, monotonic in bap: 5/3, 7/3, 7/2 and the rest.
                c.n1 * 10 + c.n2 * 14 + c.n4 * 21 + c.other_bits * 6
            };
            assert!(total >= last);
            last = total;
        }
    }

    #[test]
    fn block_bits_pad_groups() {
        let c = BapCounts {
            n1: 4,
            n2: 1,
            n4: 3,
            other_bits: 10,
        };
        assert_eq!(c.block_bits(), 2 * 5 + 7 + 2 * 7 + 10);
    }
}
