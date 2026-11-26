//! Exponents: extraction, strategy, the decoder's differential limits, grouping
//! (A/52 §7.1, p.57–62; encoder guidance §8.2.7, §8.2.8, §8.2.10, p.112).
//!
//! An exponent is the number of leading zeros of a coefficient's magnitude as a binary fraction,
//! 0..=24 (§7.1.1, p.57). The first exponent of a set is sent as a 4-bit absolute value (0..=15),
//! the rest as differences of −2..=+2, one difference per 1, 2 or 4 bins for D15, D25, D45, three
//! mapped differences per 7-bit group (§7.1.2, p.58–59). [`process`] turns the exponents the
//! signal needs into the nearest set the syntax can express, never raising an exponent (that
//! would make a mantissa reach 1.0); the result is exactly what the decoder reconstructs.

/// A new-exponent strategy: D15, D25 or D45 (A/52 Table 7.4, p.59).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpStrategy {
    /// One exponent per bin, `01`.
    D15,
    /// One exponent per pair of bins, `10`.
    D25,
    /// One exponent per four bins, `11`.
    D45,
}

impl ExpStrategy {
    /// Bins sharing one exponent: `grpsize` (A/52 §7.1.3, p.62).
    pub fn group_size(self) -> usize {
        match self {
            ExpStrategy::D15 => 1,
            ExpStrategy::D25 => 2,
            ExpStrategy::D45 => 4,
        }
    }

    /// The 2-bit chexpstr code (A/52 Table 7.4, p.59).
    pub fn code(self) -> u32 {
        match self {
            ExpStrategy::D15 => 1,
            ExpStrategy::D25 => 2,
            ExpStrategy::D45 => 3,
        }
    }

    /// From a 2-bit code; `None` for reuse (`00`).
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(ExpStrategy::D15),
            2 => Some(ExpStrategy::D25),
            3 => Some(ExpStrategy::D45),
            _ => None,
        }
    }
}

/// What a channel sends in one block: reuse the previous block's exponents, or new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelStrategy {
    /// `00`: reuse (A/52 Table 7.4, p.59). Not allowed in block 0 (§5.4.3.22, p.46).
    Reuse,
    /// New exponents with this strategy.
    New(ExpStrategy),
}

impl ChannelStrategy {
    /// chexpstr (A/52 Table 7.4, p.59).
    pub fn code(self) -> u32 {
        match self {
            ChannelStrategy::Reuse => 0,
            ChannelStrategy::New(s) => s.code(),
        }
    }
}

/// Number of 7-bit groups after the absolute exponent, nchgrps, for a full-bandwidth channel
/// whose coded bins end at `end` (A/52 §7.1.3, p.61). The LFE channel always has 2 (D15, end 7),
/// which the same formula gives.
pub fn n_groups(end: usize, s: ExpStrategy) -> usize {
    match s {
        ExpStrategy::D15 => (end - 1) / 3,
        ExpStrategy::D25 => (end - 1 + 3) / 6,
        ExpStrategy::D45 => (end - 1 + 9) / 12,
    }
}

/// Bins the decoder fills with exponents: the absolute one plus `3·grpsize` per group. May
/// exceed `end` by up to 9 for D25/D45; those bins carry no mantissas.
pub fn covered_bins(end: usize, s: ExpStrategy) -> usize {
    1 + n_groups(end, s) * 3 * s.group_size()
}

/// Bits of an exponent set: the 4-bit absolute exponent and 7 bits per group
/// (A/52 Table 5.3, p.34). gainrng is not included.
pub fn exponent_bits(end: usize, s: ExpStrategy) -> usize {
    4 + 7 * n_groups(end, s)
}

/// The exponent of one coefficient: leading zeros of |c| as a fraction, 0..=24
/// (A/52 §7.1.1, p.57; §8.2.7, p.112). |c| ≥ 0.5 gives 0; |c| ≥ 1 also gives 0 and is
/// clamped later by the quantizer.
#[inline]
pub fn exponent_of(c: f32) -> u8 {
    let bits = c.to_bits() & 0x7fff_ffff;
    // |c| = 1.f × 2^(E − 127) for a normal number; leading zeros = 126 − E for |c| < 1.
    let biased = (bits >> 23) as i32;
    if biased == 0 {
        return 24; // zero or subnormal
    }
    (126 - biased).clamp(0, 24) as u8
}

/// Exponents of `coefs[..end]`.
pub fn extract(coefs: &[f32], end: usize, out: &mut [u8; 256]) {
    for (o, &c) in out[..end].iter_mut().zip(&coefs[..end]) {
        *o = exponent_of(c);
    }
    out[end..].fill(24);
}

/// Replaces `exps[..end]` by the elementwise minimum with `other[..end]`: the exponents that
/// serve every block of a reuse run (A/52 §8.2.8, p.112).
pub fn merge_min(exps: &mut [u8; 256], other: &[u8; 256], end: usize) {
    for (a, &b) in exps[..end].iter_mut().zip(&other[..end]) {
        *a = (*a).min(b);
    }
}

/// Turns the exponents the signal needs (`exps[..end]`, bins at and above `end` ignored) into
/// the exponents the decoder will reconstruct for strategy `s`, in place, over
/// [`covered_bins`] bins. Each output exponent is ≤ every input exponent it stands for, the
/// absolute exponent is ≤ 15 (A/52 §7.1.3, p.60), and neighboring shared exponents differ by
/// at most 2 (§7.1.2, p.58); among such sets it is the largest.
pub fn process(exps: &mut [u8; 256], end: usize, s: ExpStrategy) {
    let gs = s.group_size();
    let ngrp = n_groups(end, s) * 3;
    // One value per shared exponent: the DC exponent, then one per `gs` bins from bin 1.
    // Groups beyond `end` (D25/D45 overhang) keep 24: they stand for no coded bin.
    let mut rep = [24u8; 256];
    rep[0] = exps[0].min(15);
    for (r, group) in rep[1..=ngrp].iter_mut().zip(exps[1..end].chunks(gs)) {
        *r = group.iter().copied().fold(24, u8::min);
    }
    // Differences limited to ±2 by lowering exponents, upward then downward in frequency.
    let mut prev = rep[0];
    for r in &mut rep[1..=ngrp] {
        *r = (*r).min(prev + 2);
        prev = *r;
    }
    let mut next = rep[ngrp];
    for r in rep[..ngrp].iter_mut().rev() {
        *r = (*r).min(next + 2);
        next = *r;
    }
    exps[0] = rep[0];
    for (group, &r) in exps[1..1 + ngrp * gs]
        .chunks_exact_mut(gs)
        .zip(&rep[1..=ngrp])
    {
        group.fill(r);
    }
}

/// Codes a processed exponent set: returns the absolute exponent and writes the 7-bit group
/// values, `25·M1 + 5·M2 + M3` with `M = diff + 2` (A/52 §7.1.2, p.59), into `groups`.
pub fn encode(exps: &[u8; 256], end: usize, s: ExpStrategy, groups: &mut [u8]) -> u8 {
    let gs = s.group_size();
    let n = n_groups(end, s);
    let mut prev = i32::from(exps[0]);
    for (g, out) in groups[..n].iter_mut().enumerate() {
        let mut v = 0i32;
        for j in 0..3 {
            let e = i32::from(exps[1 + (3 * g + j) * gs]);
            let d = e - prev + 2;
            debug_assert!((0..=4).contains(&d), "unprocessed exponents");
            v = v * 5 + d.clamp(0, 4);
            prev = e;
        }
        *out = v as u8;
    }
    exps[0]
}

/// A decoder's exponent expansion (A/52 §7.1.3 pseudo-code, p.61–62): `absexp` and the groups
/// into `out[0..covered]`. Returns `None` on a group value above 124 or an exponent outside
/// 0..=24 (A/52 §7.10.2 condition 17, p.107).
pub fn decode(absexp: u8, groups: &[u8], s: ExpStrategy, out: &mut [u8; 256]) -> Option<usize> {
    let gs = s.group_size();
    let mut prev = i32::from(absexp);
    out[0] = absexp;
    let mut bin = 1;
    for &g in groups {
        if g > 124 {
            return None;
        }
        let m = [
            i32::from(g / 25),
            i32::from((g % 25) / 5),
            i32::from((g % 25) % 5),
        ];
        for d in m {
            let e = prev + d - 2;
            if !(0..=24).contains(&e) {
                return None;
            }
            for _ in 0..gs {
                *out.get_mut(bin)? = e as u8;
                bin += 1;
            }
            prev = e;
        }
    }
    Some(bin)
}

/// Sum of absolute differences between two blocks' exponents, the measure of how much a
/// channel's spectrum moved (after FFmpeg's `ac3enc`, which uses the same measure).
pub fn difference(a: &[u8; 256], b: &[u8; 256], end: usize) -> u32 {
    a[..end]
        .iter()
        .zip(&b[..end])
        .map(|(x, y)| u32::from(x.abs_diff(*y)))
        .sum()
}

/// Above this many exponent units of change, summed over the coded bins, a block gets new
/// exponents: FFmpeg's `EXP_DIFF_THRESHOLD`, 500, whatever the bandwidth. A narrow bandwidth
/// (a low rate per channel) therefore reuses more readily, which is where exponent bits are
/// dearest. (Scaling the threshold to the bandwidth was tried first: at 3/2 192 kbit/s it spent
/// a third of the frame on exponents and lost 10 dB on transients; notes/ac3-encoder-log.md.)
pub fn reuse_threshold(_end: usize) -> u32 {
    500
}

/// The basic strategy of A/52 §8.2.8 (p.112): a block gets new exponents when its exponents
/// moved by more than the threshold since the previous block (or `force_new` says so); a set
/// used by one block is D45, by two or three D25, by four to six D15. Block 0 always has new
/// exponents (§5.4.3.22, p.46). A block with short transforms is D45 (§8.2.2, p.109): callers
/// force a new set in it and in the block after it, which makes its run one block long.
pub fn choose(
    raw: &[[u8; 256]; 6],
    end: usize,
    force_new: [bool; 6],
    out: &mut [ChannelStrategy; 6],
) {
    let mut new = [false; 6];
    new[0] = true;
    let threshold = reuse_threshold(end);
    for b in 1..6 {
        new[b] = force_new[b] || difference(&raw[b], &raw[b - 1], end) > threshold;
    }
    let mut b = 0;
    while b < 6 {
        let mut e = b + 1;
        while e < 6 && !new[e] {
            e += 1;
        }
        let s = match e - b {
            1 => ExpStrategy::D45,
            2 | 3 => ExpStrategy::D25,
            _ => ExpStrategy::D15,
        };
        out[b] = ChannelStrategy::New(s);
        for o in &mut out[b + 1..e] {
            *o = ChannelStrategy::Reuse;
        }
        b = e;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_counts_match_section_7_1_3() {
        // chbwcod 60: endmant 253 (A/52 §7.1.3, p.60).
        assert_eq!(n_groups(253, ExpStrategy::D15), 84);
        assert_eq!(n_groups(253, ExpStrategy::D25), 42);
        assert_eq!(n_groups(253, ExpStrategy::D45), 21);
        // LFE: nlfegrps = 2 at end 7 (A/52 §7.1.3, p.61).
        assert_eq!(n_groups(7, ExpStrategy::D15), 2);
        for code in 0..=60usize {
            let end = (code + 12) * 3 + 37;
            for s in [ExpStrategy::D15, ExpStrategy::D25, ExpStrategy::D45] {
                let c = covered_bins(end, s);
                assert!(
                    c >= end && c <= 256 && c < end + 12,
                    "end {end} {s:?} covers {c}"
                );
            }
        }
    }

    #[test]
    fn exponent_of_matches_leading_zeros() {
        assert_eq!(exponent_of(0.0), 24);
        assert_eq!(exponent_of(0.75), 0);
        assert_eq!(exponent_of(-0.5), 0);
        assert_eq!(exponent_of(0.4999), 1);
        assert_eq!(exponent_of(0.25), 1);
        assert_eq!(exponent_of(0.2499), 2);
        assert_eq!(exponent_of(1.5), 0);
        assert_eq!(exponent_of(2f32.powi(-24)), 23);
        assert_eq!(exponent_of(2f32.powi(-25)), 24);
        assert_eq!(exponent_of(1e-30), 24);
    }

    fn pseudo_random_exps(seed: u32) -> [u8; 256] {
        let mut s = seed;
        std::array::from_fn(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((s >> 16) % 25) as u8
        })
    }

    #[test]
    fn processed_exponents_round_trip_and_never_rise() {
        for seed in 0..50 {
            for end in [7usize, 73, 76, 214, 250, 253] {
                for s in [ExpStrategy::D15, ExpStrategy::D25, ExpStrategy::D45] {
                    if end == 7 && s != ExpStrategy::D15 {
                        continue;
                    }
                    let raw = pseudo_random_exps(seed);
                    let mut e = raw;
                    process(&mut e, end, s);
                    for bin in 0..end {
                        assert!(e[bin] <= raw[bin], "exponent rose at {bin}");
                    }
                    assert!(e[0] <= 15);
                    let mut groups = [0u8; 84];
                    let abs = encode(&e, end, s, &mut groups);
                    let mut back = [0u8; 256];
                    let n = n_groups(end, s);
                    let covered = decode(abs, &groups[..n], s, &mut back).unwrap();
                    assert_eq!(covered, covered_bins(end, s));
                    assert_eq!(
                        &back[..covered],
                        &e[..covered],
                        "seed {seed} end {end} {s:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn decoder_rejects_bad_groups() {
        let mut out = [0u8; 256];
        assert!(decode(10, &[125], ExpStrategy::D15, &mut out).is_none());
        // 0 → M = (0,0,0) → three −2 steps from 2: below zero.
        assert!(decode(2, &[0], ExpStrategy::D15, &mut out).is_none());
    }

    #[test]
    fn strategy_follows_run_length() {
        let quiet = [[10u8; 256]; 6];
        let mut out = [ChannelStrategy::Reuse; 6];
        choose(&quiet, 253, [false; 6], &mut out);
        assert_eq!(out[0], ChannelStrategy::New(ExpStrategy::D15));
        assert!(out[1..].iter().all(|s| *s == ChannelStrategy::Reuse));

        let mut force = [false; 6];
        force[3] = true;
        force[4] = true;
        choose(&quiet, 253, force, &mut out);
        assert_eq!(out[0], ChannelStrategy::New(ExpStrategy::D25));
        assert_eq!(out[3], ChannelStrategy::New(ExpStrategy::D45));
        assert_eq!(out[4], ChannelStrategy::New(ExpStrategy::D25));
        assert_eq!(out[5], ChannelStrategy::Reuse);
    }
}
