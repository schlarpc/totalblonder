//! The syncframe syntax: syncinfo, bsi, audblk, auxdata, errorcheck (A/52 §5.3, p.30–37),
//! written over any [`BitSink`], plus the frame layout rules of §5.5 (p.52).
//!
//! What this encoder sends, by element (the rest of the syntax is fixed at "absent"):
//!
//! | Element | Value |
//! |---|---|
//! | compre, langcode, timecod1e, timecod2e, addbsie | 0 |
//! | dynrnge | 0 in every block: dynrng is 0 dB (§5.4.3.3, p.43) |
//! | cplstre / cplinu | 1 / 0 in block 0, cplstre 0 after: coupling off |
//! | rematstr | 2/0 only; 1 in block 0 and when the flags change |
//! | baie | 1 in block 0 only: one set of parameters per frame |
//! | snroffste | 1 in block 0, and in a later block whose SNR offset differs from the last |
//! | deltbaie | 0: no delta bit allocation (§5.4.3.47, p.48) |
//! | skiple | 1 only where padding is needed to meet §5.5 |
//! | auxdatae | 0: the aux bits are zero filler (§5.4.4, p.50–52) |
//! | crcrsv | 0, or 1 if crc2 would otherwise equal the syncword (§5.4.5.1, p.52) |

use crate::bitalloc::{NOMINAL_FAST_GAIN, Params, SnrOffset};
use crate::bits::BitSink;
use crate::config::Config;
use crate::exponents::{self, ChannelStrategy, ExpStrategy};
use crate::quantize::Quantized;
use crate::tables::{BSID, LFE_END_MANT, N_COEFS, SYNCWORD};

/// Most full-bandwidth channels (3/2).
pub const MAX_FBW: usize = 5;
/// Most channels including LFE.
pub const MAX_CHANNELS: usize = 6;

/// Bits after the last audio block that are not aux data: auxdatae (1), crcrsv (1), crc2 (16)
/// (A/52 Table 5.4, Table 5.5, p.36–37).
pub const TRAILER_BITS: usize = 18;

/// Most skip bytes one block can carry: skipl is 9 bits (A/52 §5.4.3.59, p.50).
pub const MAX_SKIP_BYTES: u16 = 511;

/// The per-block choices that shape the audblk syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSide {
    /// blksw per full-bandwidth channel.
    pub blksw: [bool; MAX_FBW],
    /// dithflag per full-bandwidth channel.
    pub dithflag: [bool; MAX_FBW],
    /// gainrng per full-bandwidth channel (sent with new exponents).
    pub gainrng: [u8; MAX_FBW],
    /// Exponent strategy per channel; the LFE channel (index nfchans) is Reuse or New(D15).
    pub strategy: [ChannelStrategy; MAX_CHANNELS],
    /// rematstr (2/0 only).
    pub rematstr: bool,
    /// rematflg, four bands with coupling off (A/52 Table 5.15, p.46).
    pub rematflg: [bool; 4],
    /// The SNR offset sent in this block (snroffste = 1): csnroffst, and the same fsnroffst for
    /// every channel. Block 0 must send one (§5.4.3.36, p.47); a later block sends one when its
    /// offset differs from the block before, and otherwise inherits it.
    pub snr: Option<SnrOffset>,
    /// skipl, if skiple = 1.
    pub skip: Option<u16>,
}

impl Default for BlockSide {
    fn default() -> Self {
        Self {
            blksw: [false; MAX_FBW],
            dithflag: [false; MAX_FBW],
            gainrng: [0; MAX_FBW],
            strategy: [ChannelStrategy::Reuse; MAX_CHANNELS],
            rematstr: false,
            rematflg: [false; 4],
            snr: None,
            skip: None,
        }
    }
}

/// End bin (one past the last coded bin) of channel `ch`: the bandwidth for full-bandwidth
/// channels, 7 for the LFE channel (A/52 §7.1.3, p.60).
pub fn channel_end(cfg: &Config, ch: usize) -> usize {
    if ch < cfg.nfchans() {
        cfg.bandwidth_code().end_mant()
    } else {
        LFE_END_MANT
    }
}

/// syncinfo and bsi (A/52 Tables 5.1, 5.2, p.30–32), with crc1 written as 0.
pub fn write_header<S: BitSink>(cfg: &Config, s: &mut S) {
    let mode = cfg.coding_mode();
    // syncinfo
    s.put(16, u32::from(SYNCWORD));
    s.put(16, 0); // crc1, filled in after the frame is complete
    s.put(2, cfg.sample_rate().fscod());
    s.put(6, cfg.bit_rate().frmsizecod());
    // bsi
    s.put(5, u32::from(BSID));
    s.put(3, cfg.bitstream_mode().code());
    s.put(3, mode.acmod());
    if let Some(c) = mode.center_mix_level() {
        s.put(2, c.code());
    }
    if let Some(sm) = mode.surround_mix_level() {
        s.put(2, sm.code());
    }
    if let Some(d) = mode.dolby_surround() {
        s.put(2, d.code());
    }
    s.put(1, u32::from(cfg.lfe()));
    s.put(5, cfg.dialnorm().code());
    s.put(1, 0); // compre
    s.put(1, 0); // langcode
    match cfg.settings().audio_production {
        Some(p) => {
            s.put(1, 1);
            s.put(5, p.mix_level.code());
            s.put(2, p.room_type.code());
        }
        None => s.put(1, 0),
    }
    // acmod 0 (1+1) is not offered, so the second-channel fields never appear.
    s.put(1, u32::from(cfg.settings().copyright));
    s.put(1, u32::from(cfg.settings().original));
    s.put(1, 0); // timecod1e
    s.put(1, 0); // timecod2e
    s.put(1, 0); // addbsie
}

/// The side information of audio block `blk` (A/52 Table 5.3, p.32–36) up to and including the
/// skip field: everything but the mantissas. `exps[ch]` are the decoded exponents of channels
/// with new exponents in this block; only their values, not their count, depend on them.
pub fn write_block_side<S: BitSink>(
    cfg: &Config,
    blk: usize,
    side: &BlockSide,
    params: &Params,
    exps: &[[u8; N_COEFS]],
    s: &mut S,
) {
    let nf = cfg.nfchans();
    let lfe = cfg.lfe();
    let acmod = cfg.coding_mode().acmod();
    for ch in 0..nf {
        s.put(1, u32::from(side.blksw[ch]));
    }
    for ch in 0..nf {
        s.put(1, u32::from(side.dithflag[ch]));
    }
    s.put(1, 0); // dynrnge
    // Coupling strategy: must be sent in block 0 (§5.4.3.7, p.43); coupling stays off.
    if blk == 0 {
        s.put(1, 1); // cplstre
        s.put(1, 0); // cplinu
    } else {
        s.put(1, 0); // cplstre
    }
    if acmod == 2 {
        s.put(1, u32::from(side.rematstr));
        if side.rematstr {
            // cplinu == 0: four bands (Table 5.15, p.46).
            for f in side.rematflg {
                s.put(1, u32::from(f));
            }
        }
    }
    for ch in 0..nf {
        s.put(2, side.strategy[ch].code());
    }
    if lfe {
        let new = matches!(side.strategy[nf], ChannelStrategy::New(_));
        s.put(1, u32::from(new)); // lfeexpstr (Table 7.5, p.60)
    }
    for ch in 0..nf {
        if side.strategy[ch] != ChannelStrategy::Reuse {
            s.put(6, cfg.bandwidth_code().code()); // chbwcod (channels not in coupling)
        }
    }
    let mut groups = [0u8; 84];
    for (ch, strategy) in side.strategy.iter().enumerate().take(nf) {
        if let ChannelStrategy::New(st) = *strategy {
            let end = channel_end(cfg, ch);
            let n = exponents::n_groups(end, st);
            let abs = exponents::encode(&exps[ch], end, st, &mut groups);
            s.put(4, u32::from(abs));
            for &g in &groups[..n] {
                s.put(7, u32::from(g));
            }
            s.put(2, u32::from(side.gainrng[ch]));
        }
    }
    if lfe && side.strategy[nf] != ChannelStrategy::Reuse {
        let abs = exponents::encode(&exps[nf], LFE_END_MANT, ExpStrategy::D15, &mut groups);
        s.put(4, u32::from(abs));
        for &g in &groups[..2] {
            s.put(7, u32::from(g));
        }
    }
    // Bit allocation parameters: once per frame, in block 0, which must have them
    // (§5.4.3.30, p.47).
    if blk == 0 {
        s.put(1, 1); // baie
        s.put(2, params.sdcycod.code());
        s.put(2, params.fdcycod.code());
        s.put(2, params.sgaincod.code());
        s.put(2, params.dbpbcod.code());
        s.put(3, params.floorcod.code());
    } else {
        s.put(1, 0); // baie
    }
    // SNR offsets: in block 0 (§5.4.3.36, p.47; the encoder always gives it one) and wherever
    // they change.
    match side.snr {
        Some(snr) => {
            s.put(1, 1); // snroffste
            s.put(6, u32::from(snr.coarse));
            for _ in 0..cfg.channels() {
                s.put(4, u32::from(snr.fine));
                s.put(3, NOMINAL_FAST_GAIN.code());
            }
        }
        None => s.put(1, 0),
    }
    s.put(1, 0); // deltbaie: in block 0 this means "no delta bit allocation" (§5.4.3.47)
    match side.skip {
        Some(n) => {
            s.put(1, 1);
            s.put(9, u32::from(n));
            for _ in 0..n {
                s.put(8, 0);
            }
        }
        None => s.put(1, 0),
    }
}

/// The mantissas of one block: full-bandwidth channels in order, then LFE
/// (A/52 Table 5.3, p.36).
pub fn write_mantissas<S: BitSink>(cfg: &Config, quant: &[Quantized], s: &mut S) {
    for (ch, q) in quant.iter().enumerate().take(cfg.channels()) {
        let end = channel_end(cfg, ch);
        for (&w, &c) in q.widths[..end].iter().zip(&q.codes[..end]) {
            if w != 0 {
                s.put(u32::from(w), u32::from(c));
            }
        }
    }
}

/// Where the bits of a frame go, and the skip padding that the §5.5 limits require.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// skipl per block (None: skiple = 0).
    pub skip: [Option<u16>; 6],
    /// Bits used by everything except the aux filler and the trailer.
    pub used_bits: usize,
    /// Bits of aux filler.
    pub aux_bits: usize,
}

/// Places a frame with `header` bits, then per block `side[b]` side bits (with skiple = 0)
/// and `mant[b]` mantissa bits, into a syncframe of `frame_bits` whose 5/8 point is at
/// `five_eighths_bits`. Returns `None` if it does not fit. The rules (A/52 §5.5, p.52):
///
/// - syncinfo, bsi and blocks 0 and 1 end at or before the 5/8 point;
/// - block 5's mantissas, the aux data and errorcheck lie within the last 3/8, i.e. block 5's
///   mantissas start at or after the 5/8 point. When they would start earlier, skip bytes are
///   added to blocks 5, 4, 3, 2 (in that order) until they do not.
pub fn layout(
    header: usize,
    side: &[usize; 6],
    mant: &[usize; 6],
    frame_bits: usize,
    five_eighths_bits: usize,
) -> Option<Layout> {
    let block_end_1 = header + side[0] + mant[0] + side[1] + mant[1];
    if block_end_1 > five_eighths_bits {
        return None;
    }
    let start5: usize = header + side.iter().sum::<usize>() + mant[..5].iter().sum::<usize>();
    let mut skip = [None; 6];
    let mut gap = five_eighths_bits.saturating_sub(start5);
    let mut pad = 0usize;
    for b in [5usize, 4, 3, 2] {
        if gap == 0 {
            break;
        }
        // skipl (9 bits) and the bytes; skiple itself is already in side[b].
        let bytes = gap
            .saturating_sub(9)
            .div_ceil(8)
            .min(usize::from(MAX_SKIP_BYTES));
        let bits = 9 + 8 * bytes;
        skip[b] = Some(bytes as u16);
        pad += bits;
        gap = gap.saturating_sub(bits);
    }
    if gap > 0 {
        return None;
    }
    let used = start5 + mant[5] + pad;
    if used + TRAILER_BITS > frame_bits {
        return None;
    }
    Some(Layout {
        skip,
        used_bits: used,
        aux_bits: frame_bits - TRAILER_BITS - used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rules() {
        // 192 kbit/s: 6144 bits, 5/8 point at 3840.
        let (fb, p58) = (6144, 3840);
        let side = [400, 50, 50, 50, 50, 50];
        // Even spread: no padding needed.
        let l = layout(100, &side, &[800; 6], fb, p58).unwrap();
        assert_eq!(l.skip, [None; 6]);
        assert_eq!(l.used_bits, 100 + 650 + 4800);
        assert_eq!(l.aux_bits, fb - 18 - l.used_bits);
        // Blocks 0 and 1 too big for the first 5/8.
        assert!(layout(100, &side, &[2000, 1500, 0, 0, 0, 0], fb, p58).is_none());
        // Everything in block 5: its mantissas are pushed past the 5/8 point with skip bytes.
        let l = layout(100, &side, &[0, 0, 0, 0, 0, 2000], fb, p58).unwrap();
        let pad: usize = l
            .skip
            .iter()
            .flatten()
            .map(|&n| 9 + 8 * usize::from(n))
            .sum();
        assert!(100 + 650 + pad >= p58);
        assert!(l.used_bits + 18 <= fb);
        // Block 5 bigger than the last 3/8 cannot be placed.
        assert!(layout(100, &side, &[0, 0, 0, 0, 0, 2400], fb, p58).is_none());
    }
}
