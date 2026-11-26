//! The encoder: from 1536 samples per channel to one syncframe.
//!
//! Per frame (A/52 §8.2, p.108–113):
//!
//! 1. per channel and block: transient detection (§8.2.2), window and MDCT (§8.2.3);
//! 2. rematrixing in 2/0 (§7.5.3, §8.2.6);
//! 3. exponents: extraction, strategy, processing to what the decoder reconstructs
//!    (§8.2.7–§8.2.10);
//! 4. bit allocation: the decoder's masking curve per exponent set, then a binary search over
//!    the combined coarse/fine SNR offset for the largest that fits the frame and the §5.5 limits
//!    (§8.2.12);
//! 5. quantization (§8.2.13) and packing (§8.2.14): side information, mantissas, skip padding
//!    where §5.5 needs it, zero aux bits, and the two CRCs (§7.10.1).
//!
//! If the side information alone would not fit (very low rates), the exponent plan falls back to
//! D45 everywhere, then to new exponents in block 0 only; [`check_rate_feasible`] refuses, at
//! configuration time, any configuration for which even that last plan does not fit, so every
//! call produces a valid frame.
//!
//! All working storage is allocated in [`Encoder::new`]; encoding a frame allocates nothing.

use crate::bitalloc::{self, BapCounts, Mask, NOMINAL_FAST_GAIN, Params, SnrOffset};
use crate::bits::{BitCounter, BitSink, BitWriter};
use crate::config::{Config, ConfigError};
use crate::crc;
use crate::exponents::{self, ChannelStrategy, ExpStrategy};
use crate::mdct::{self, Mdct};
use crate::quantize::{self, ChannelInput, Quantized};
use crate::syntax::{self, BlockSide, Layout, MAX_CHANNELS, MAX_FBW, TRAILER_BITS, channel_end};
use crate::tables::{BLOCKS_PER_FRAME, FRAME_SAMPLES, N_COEFS, REMATRIX_BAND_EDGES};
use crate::transient::TransientDetector;

/// Input that does not have the shape the configuration promised.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    /// Wrong number of channel slices.
    #[error("expected {expected} channels, got {got}")]
    Channels {
        /// Channels the configuration has, including LFE.
        expected: usize,
        /// Slices given.
        got: usize,
    },
    /// A channel slice of the wrong length.
    #[error("channel {channel} has {got} samples, expected 1536")]
    Samples {
        /// Index of the channel.
        channel: usize,
        /// Samples given.
        got: usize,
    },
}

/// How exponents are planned; later plans spend fewer bits on exponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExponentPlan {
    /// The strategy of A/52 §8.2.8: new exponents where the spectrum moved, D15/D25/D45 by
    /// run length.
    Normal,
    /// The same reuse decisions, every new set D45.
    AllD45,
    /// New exponents (D45) in block 0 only, reused in blocks 1–5. The smallest legal frame.
    Minimal,
}

/// What the encoder decided for the last frame, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    /// The SNR offset of each block.
    pub snr: [SnrOffset; 6],
    /// The exponent plan used.
    pub plan: ExponentPlan,
    /// Bits of side information and mantissas (everything except aux filler and trailer).
    pub used_bits: usize,
    /// Bits of the syncframe.
    pub frame_bits: usize,
    /// Zero bits in the aux field.
    pub aux_bits: usize,
    /// Skip bytes added for the §5.5 limits.
    pub skip_bytes: usize,
    /// Blocks × channels coded with short transforms.
    pub short_blocks: usize,
    /// Whether crcrsv had to be set.
    pub crcrsv: bool,
}

/// Per-block working state.
#[derive(Debug, Clone)]
struct BlockWork {
    side: BlockSide,
    coefs: [[f32; N_COEFS]; MAX_CHANNELS],
    raw: [[u8; N_COEFS]; MAX_CHANNELS],
    exps: [[u8; N_COEFS]; MAX_CHANNELS],
    bap: [[u8; N_COEFS]; MAX_CHANNELS],
    mask: [Mask; MAX_CHANNELS],
    /// What `bap[ch]` (and `counts[ch]`) currently hold: the allocation for this offset and
    /// floor over the current exponents, or nothing valid.
    bap_key: [Option<BapKey>; MAX_CHANNELS],
    counts: [BapCounts; MAX_CHANNELS],
    quant: [Quantized; MAX_CHANNELS],
}

/// The inputs of a bap besides the exponents: SNR offset index and floor code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BapKey {
    offset: u16,
    floorcod: u32,
}

/// Side information bits of each block without and with an SNR offset sent in it.
#[derive(Debug, Clone, Copy)]
struct SideBits {
    without: [usize; 6],
    with: [usize; 6],
}

/// The outcome of the bit allocation search: an SNR offset index per block and the layout.
#[derive(Debug, Clone, Copy)]
struct Allocation {
    offsets: [u16; 6],
    layout: Layout,
}

impl Default for BlockWork {
    fn default() -> Self {
        Self {
            side: BlockSide::default(),
            coefs: [[0.0; N_COEFS]; MAX_CHANNELS],
            raw: [[24; N_COEFS]; MAX_CHANNELS],
            exps: [[24; N_COEFS]; MAX_CHANNELS],
            bap: [[0; N_COEFS]; MAX_CHANNELS],
            mask: std::array::from_fn(|_| Mask::default()),
            bap_key: [None; MAX_CHANNELS],
            counts: [BapCounts::default(); MAX_CHANNELS],
            quant: std::array::from_fn(|_| Quantized::default()),
        }
    }
}

/// An AC-3 encoder for one configuration.
#[derive(Debug, Clone)]
pub struct Encoder {
    cfg: Config,
    mdct: Mdct,
    scratch: mdct::Scratch,
    /// The last 256 input samples of each channel.
    history: [[f32; 256]; MAX_CHANNELS],
    detectors: [TransientDetector; MAX_FBW],
    /// blksw of block 5 of the previous frame, for the dither rule of §8.2.9.
    last_blksw: [bool; MAX_FBW],
    blocks: Vec<BlockWork>,
    out: Vec<u8>,
    stats: FrameStats,
}

/// The run of blocks sharing block `b`'s exponents: the block that sent them.
fn run_start(blocks: &[BlockWork], ch: usize, b: usize) -> usize {
    let mut s = b;
    while s > 0 && blocks[s].side.strategy[ch] == ChannelStrategy::Reuse {
        s -= 1;
    }
    s
}

impl Encoder {
    /// An encoder for a validated configuration.
    pub fn new(cfg: Config) -> Self {
        let frame_bytes = cfg.frame_bytes();
        Self {
            cfg,
            mdct: Mdct::new(),
            scratch: mdct::Scratch::default(),
            history: [[0.0; 256]; MAX_CHANNELS],
            detectors: std::array::from_fn(|_| TransientDetector::new()),
            last_blksw: [false; MAX_FBW],
            blocks: (0..BLOCKS_PER_FRAME)
                .map(|_| BlockWork::default())
                .collect(),
            out: vec![0; frame_bytes],
            stats: FrameStats {
                snr: [SnrOffset::default(); 6],
                plan: ExponentPlan::Normal,
                used_bits: 0,
                frame_bits: frame_bytes * 8,
                aux_bits: 0,
                skip_bytes: 0,
                short_blocks: 0,
                crcrsv: false,
            },
        }
    }

    /// The configuration.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Decisions made for the last frame.
    pub fn last_stats(&self) -> FrameStats {
        self.stats
    }

    /// Samples of delay from input to decoded output: 256, the half block of overlap
    /// (the first output block of a decoder reconstructs the 256 samples before the first
    /// input block).
    pub const DELAY_SAMPLES: usize = 256;

    /// Encodes one syncframe from `input`: one slice of exactly 1536 samples per channel, in
    /// A/52 order (Table 5.8, p.39: L, C, R, SL, SR as present) followed by LFE if on. Samples
    /// are full scale at ±1.0; values beyond are clipped and NaN is taken as 0.
    ///
    /// Returns the frame, exactly [`Config::frame_bytes`] long; it borrows the encoder's
    /// buffer until the next call.
    pub fn encode_frame(&mut self, input: &[&[f32]]) -> Result<&[u8], InputError> {
        let nch = self.cfg.channels();
        if input.len() != nch {
            return Err(InputError::Channels {
                expected: nch,
                got: input.len(),
            });
        }
        for (channel, x) in input.iter().enumerate() {
            if x.len() != FRAME_SAMPLES {
                return Err(InputError::Samples {
                    channel,
                    got: x.len(),
                });
            }
        }
        self.encode_checked(input);
        Ok(&self.out)
    }

    /// [`Encoder::encode_frame`] after the shape of `input` has been checked.
    fn encode_checked(&mut self, input: &[&[f32]]) {
        let nch = self.cfg.channels();
        self.analyze(input);
        if self.cfg.coding_mode().acmod() == 2 {
            self.rematrix();
        }
        for b in 0..BLOCKS_PER_FRAME {
            for ch in 0..nch {
                let end = channel_end(&self.cfg, ch);
                let blk = &mut self.blocks[b];
                exponents::extract(&blk.coefs[ch], end, &mut blk.raw[ch]);
            }
        }
        self.set_dither();

        let alloc_params = Params::NOMINAL;
        let header_bits = {
            let mut c = BitCounter::new();
            syntax::write_header(&self.cfg, &mut c);
            c.position()
        };
        let mut chosen = None;
        let mut alloc_params = alloc_params;
        for plan in [
            ExponentPlan::Normal,
            ExponentPlan::AllD45,
            ExponentPlan::Minimal,
        ] {
            self.plan_exponents(plan);
            self.compute_masks(&alloc_params);
            let side = self.side_bits(&alloc_params);
            if let Some(a) = self.search(header_bits, &side, &alloc_params) {
                chosen = Some((plan, a));
                // Every block at the highest offset with bits to spare: the masking floor is
                // what limits the allocation. Lower floors (floortab 5, 6, 7: 0x170, 0x0f0,
                // −0x800, Table 7.10) allow larger baps for quiet coefficients. A lower floor
                // is taken only if every block still gets the highest offset with it, so that
                // no coefficient loses resolution to pay for it. The mask does not depend on
                // the floor (§7.2.2.7), and the side information has the same size.
                let all_max = |a: &Allocation| a.offsets.iter().all(|&o| o == SnrOffset::MAX_INDEX);
                let mut current = a;
                for code in 5..=7 {
                    if !(all_max(&current) && current.layout.aux_bits > 0) {
                        break;
                    }
                    let Some(floorcod) = bitalloc::FloorCode::new(code) else {
                        break;
                    };
                    let p = Params {
                        floorcod,
                        ..alloc_params
                    };
                    match self.search(header_bits, &side, &p) {
                        Some(b) if all_max(&b) => {
                            alloc_params = p;
                            current = b;
                            chosen = Some((plan, b));
                        }
                        _ => break,
                    }
                }
                break;
            }
        }
        // `check_rate_feasible` guarantees the minimal plan fits with no mantissas.
        let (plan, alloc) = match chosen {
            Some(c) => c,
            None => {
                debug_assert!(false, "configuration admitted a frame that cannot be built");
                let empty = Layout {
                    skip: [None; 6],
                    used_bits: 0,
                    aux_bits: 0,
                };
                let a = Allocation {
                    offsets: [0; 6],
                    layout: empty,
                };
                (ExponentPlan::Minimal, a)
            }
        };
        let layout = alloc.layout;
        self.finalize_allocation(&alloc.offsets, &alloc_params);
        for (b, skip) in layout.skip.iter().enumerate() {
            self.blocks[b].side.skip = *skip;
        }
        let crcrsv = self.write_frame(&alloc_params);
        let snr = alloc.offsets.map(SnrOffset::from_index);
        let short_blocks = self
            .blocks
            .iter()
            .map(|blk| {
                blk.side.blksw[..self.cfg.nfchans()]
                    .iter()
                    .filter(|&&s| s)
                    .count()
            })
            .sum();
        self.stats = FrameStats {
            snr,
            plan,
            used_bits: layout.used_bits,
            frame_bits: self.out.len() * 8,
            aux_bits: layout.aux_bits,
            skip_bytes: layout.skip.iter().flatten().map(|&n| usize::from(n)).sum(),
            short_blocks,
            crcrsv,
        };
    }

    /// Steps 1: transient detection, gain range, MDCT, per channel and block.
    fn analyze(&mut self, input: &[&[f32]]) {
        let nf = self.cfg.nfchans();
        let switching = self.cfg.settings().block_switching;
        let mut buf = [0.0f32; 512];
        let mut new = [0.0f32; 256];
        for (ch, x) in input.iter().enumerate() {
            let end = channel_end(&self.cfg, ch);
            for b in 0..BLOCKS_PER_FRAME {
                for (o, &v) in new.iter_mut().zip(&x[b * 256..b * 256 + 256]) {
                    *o = if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) };
                }
                buf[..256].copy_from_slice(&self.history[ch]);
                buf[256..].copy_from_slice(&new);
                self.history[ch] = new;
                let short = ch < nf && switching && self.detectors[ch].detect(&new);
                // gainrng from the 512 samples before windowing (A/52 §7.9.5, p.103).
                let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                let gainrng = match peak {
                    p if p >= 0.5 => 0,
                    p if p >= 0.25 => 1,
                    p if p >= 0.125 => 2,
                    _ => 3,
                };
                let blk = &mut self.blocks[b];
                if ch < nf {
                    blk.side.blksw[ch] = short;
                    blk.side.gainrng[ch] = gainrng;
                }
                self.mdct
                    .forward(&buf, short, &mut blk.coefs[ch], &mut self.scratch);
                blk.coefs[ch][end..].fill(0.0);
            }
        }
    }

    /// Step 2: rematrixing flags and the sum/difference transform (A/52 §7.5.3, p.85). A band
    /// is rematrixed when L+R or L−R has less energy than L and R; blocks where only one of the
    /// two channels uses short transforms are not rematrixed.
    fn rematrix(&mut self) {
        let enabled = self.cfg.settings().rematrixing;
        let end = channel_end(&self.cfg, 0);
        let mut prev = [false; 4];
        for b in 0..BLOCKS_PER_FRAME {
            let blk = &mut self.blocks[b];
            let mut flags = [false; 4];
            if enabled && blk.side.blksw[0] == blk.side.blksw[1] {
                for (band, flag) in flags.iter_mut().enumerate() {
                    let lo = REMATRIX_BAND_EDGES[band];
                    let hi = REMATRIX_BAND_EDGES[band + 1].min(end);
                    let (mut sl, mut sr, mut ss, mut sd) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for k in lo..hi {
                        let (l, r) = (blk.coefs[0][k], blk.coefs[1][k]);
                        sl += l * l;
                        sr += r * r;
                        ss += (l + r) * (l + r);
                        sd += (l - r) * (l - r);
                    }
                    *flag = ss.min(sd) < sl.min(sr);
                }
            }
            for (band, &flag) in flags.iter().enumerate() {
                if flag {
                    let lo = REMATRIX_BAND_EDGES[band];
                    let hi = REMATRIX_BAND_EDGES[band + 1].min(end);
                    for k in lo..hi {
                        let (l, r) = (blk.coefs[0][k], blk.coefs[1][k]);
                        blk.coefs[0][k] = 0.5 * (l + r);
                        blk.coefs[1][k] = 0.5 * (l - r);
                    }
                }
            }
            blk.side.rematflg = flags;
            blk.side.rematstr = b == 0 || flags != prev;
            prev = flags;
        }
    }

    /// Dither (A/52 §8.2.9, p.112): on, except in a block with short transforms and the block
    /// after, and in a block whose coefficients are all below the exponent range (digital
    /// silence decodes to zeros instead of dither).
    fn set_dither(&mut self) {
        let nf = self.cfg.nfchans();
        let end = channel_end(&self.cfg, 0);
        for ch in 0..nf {
            let mut prev = self.last_blksw[ch];
            for blk in self.blocks.iter_mut() {
                let silent = blk.raw[ch][..end].iter().all(|&e| e == 24);
                blk.side.dithflag[ch] = !(blk.side.blksw[ch] || prev || silent);
                prev = blk.side.blksw[ch];
            }
            self.last_blksw[ch] = prev;
        }
    }

    /// Step 3: strategies and decoded exponents for every channel and block.
    fn plan_exponents(&mut self, plan: ExponentPlan) {
        let nf = self.cfg.nfchans();
        for ch in 0..self.cfg.channels() {
            let end = channel_end(&self.cfg, ch);
            let mut strategy = [ChannelStrategy::Reuse; 6];
            match plan {
                ExponentPlan::Minimal => {
                    strategy[0] = ChannelStrategy::New(ExpStrategy::D45);
                }
                ExponentPlan::Normal | ExponentPlan::AllD45 => {
                    let raw: [[u8; N_COEFS]; 6] = std::array::from_fn(|b| self.blocks[b].raw[ch]);
                    let mut force = [false; 6];
                    if ch < nf {
                        for (b, f) in force.iter_mut().enumerate().skip(1) {
                            *f = self.blocks[b].side.blksw[ch] || self.blocks[b - 1].side.blksw[ch];
                        }
                    }
                    exponents::choose(&raw, end, force, &mut strategy);
                    if plan == ExponentPlan::AllD45 {
                        for s in &mut strategy {
                            if let ChannelStrategy::New(_) = s {
                                *s = ChannelStrategy::New(ExpStrategy::D45);
                            }
                        }
                    }
                }
            }
            if ch == nf {
                // The LFE channel has only D15 (A/52 Table 7.5, p.60).
                for s in &mut strategy {
                    if let ChannelStrategy::New(_) = s {
                        *s = ChannelStrategy::New(ExpStrategy::D15);
                    }
                }
            }
            // One exponent set per run: the minimum over the run, processed.
            let mut b = 0;
            while b < 6 {
                let mut e = b + 1;
                while e < 6 && strategy[e] == ChannelStrategy::Reuse {
                    e += 1;
                }
                let mut set = self.blocks[b].raw[ch];
                for later in b + 1..e {
                    let other = self.blocks[later].raw[ch];
                    exponents::merge_min(&mut set, &other, end);
                }
                if let ChannelStrategy::New(st) = strategy[b] {
                    exponents::process(&mut set, end, st);
                }
                for (k, blk) in self.blocks[b..e].iter_mut().enumerate() {
                    blk.exps[ch] = set;
                    blk.side.strategy[ch] = strategy[b + k];
                }
                b = e;
            }
        }
    }

    /// Step 4a: the masking curve of every exponent set.
    fn compute_masks(&mut self, p: &Params) {
        for ch in 0..self.cfg.channels() {
            let end = channel_end(&self.cfg, ch);
            for blk in self.blocks.iter_mut() {
                // New masks: no bap computed before is valid.
                blk.bap_key[ch] = None;
                if blk.side.strategy[ch] != ChannelStrategy::Reuse {
                    let exps = blk.exps[ch];
                    bitalloc::compute_mask(&exps, end, NOMINAL_FAST_GAIN, p, &mut blk.mask[ch]);
                }
            }
        }
    }

    /// Side information bits per block (with skiple = 0), without and with an SNR offset sent
    /// in the block.
    fn side_bits(&self, p: &Params) -> SideBits {
        let count = |b: usize, snr: Option<SnrOffset>| {
            let mut c = BitCounter::new();
            let mut side = self.blocks[b].side;
            side.skip = None;
            side.snr = snr;
            syntax::write_block_side(&self.cfg, b, &side, p, &self.blocks[b].exps, &mut c);
            c.position()
        };
        SideBits {
            without: std::array::from_fn(|b| count(b, None)),
            with: std::array::from_fn(|b| count(b, Some(SnrOffset::default()))),
        }
    }

    /// Mantissa bits of block `b` at SNR offset index `s`; fills the block's `bap`.
    fn block_bits(&mut self, b: usize, s: u16, p: &Params) -> usize {
        let snr = SnrOffset::from_index(s);
        // §7.2.2.1.1: csnroffst and every fsnroffst zero → every bap is 0.
        let zero = s == 0;
        let key = BapKey {
            offset: s,
            floorcod: p.floorcod.code(),
        };
        let mut total = BapCounts::default();
        for ch in 0..self.cfg.channels() {
            let end = channel_end(&self.cfg, ch);
            let rs = run_start(&self.blocks, ch, b);
            let (before, from_b) = self.blocks.split_at_mut(b);
            let blk = &mut from_b[0];
            if blk.bap_key[ch] != Some(key) {
                if rs != b && before[rs].bap_key[ch] == Some(key) {
                    // Same exponents, same offset: the run start's allocation is this one.
                    blk.bap[ch] = before[rs].bap[ch];
                    blk.counts[ch] = before[rs].counts[ch];
                } else {
                    // The mask belongs to the block that sent the exponents.
                    let mask = if rs == b {
                        &blk.mask[ch]
                    } else {
                        &before[rs].mask[ch]
                    };
                    blk.counts[ch] = if zero {
                        bitalloc::zero_bap(end, &mut blk.bap[ch])
                    } else {
                        bitalloc::compute_bap(mask, snr.value(), p.floor(), &mut blk.bap[ch])
                    };
                }
                blk.bap_key[ch] = Some(key);
            }
            total.add(&blk.counts[ch]);
        }
        total.block_bits() as usize
    }

    /// Step 4b: SNR offsets per block. First the largest offset common to all blocks that
    /// fits (bisection over the combined coarse/fine index, invariant "low fits, high does
    /// not"); then, while some block can still rise alone, the blocks that can are raised
    /// together as far as they fit, and if they cannot rise together, the lowest alone. This
    /// spends the bits a common offset leaves, which matters most when a §5.5 limit, not the
    /// frame size, stopped the common offset. `None` if even offset 0 (no mantissas) does not
    /// fit.
    fn search(&mut self, header: usize, side: &SideBits, p: &Params) -> Option<Allocation> {
        let frame_bits = self.out.len() * 8;
        let p58 = self.cfg.bit_rate().five_eighths_bytes() * 8;
        let place = |offsets: &[u16; 6], mant: &[usize; 6]| {
            let sides: [usize; 6] = std::array::from_fn(|b| {
                if b == 0 || offsets[b] != offsets[b - 1] {
                    side.with[b]
                } else {
                    side.without[b]
                }
            });
            syntax::layout(header, &sides, mant, frame_bits, p58)
        };
        let mut mant = [0usize; 6];
        let uniform = |enc: &mut Self, s: u16, mant: &mut [usize; 6]| {
            for (b, m) in mant.iter_mut().enumerate() {
                *m = enc.block_bits(b, s, p);
            }
            place(&[s; 6], mant)
        };
        let mut layout = uniform(self, 0, &mut mant)?;
        let mut lo = 0u16;
        let mut hi = SnrOffset::MAX_INDEX;
        if let Some(l) = uniform(self, hi, &mut mant) {
            return Some(Allocation {
                offsets: [hi; 6],
                layout: l,
            });
        }
        let mut lo_mant = mant;
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            match uniform(self, mid, &mut mant) {
                Some(l) => {
                    lo = mid;
                    layout = l;
                    lo_mant = mant;
                }
                None => hi = mid,
            }
        }
        let mut offsets = [lo; 6];
        let mut mant = lo_mant;

        // Water-filling over the blocks.
        for _ in 0..12 {
            let mut can = [false; 6];
            for b in 0..6 {
                if offsets[b] < SnrOffset::MAX_INDEX {
                    let mut trial = offsets;
                    trial[b] += 1;
                    let mut m = mant;
                    m[b] = self.block_bits(b, trial[b], p);
                    can[b] = place(&trial, &m).is_some();
                }
            }
            let Some(lowest) = (0..6).filter(|&b| can[b]).min_by_key(|&b| offsets[b]) else {
                break;
            };
            // Raise every block that can rise to a common level t, as high as it fits.
            let raise = |enc: &mut Self, t: u16, offsets: &[u16; 6], mant: &[usize; 6]| {
                let mut o = *offsets;
                let mut m = *mant;
                for b in 0..6 {
                    if can[b] && o[b] < t {
                        o[b] = t;
                        m[b] = enc.block_bits(b, t, p);
                    }
                }
                place(&o, &m).map(|l| (o, m, l))
            };
            let mut t_lo = offsets[lowest];
            let mut t_hi = SnrOffset::MAX_INDEX + 1;
            let mut best = None;
            while t_hi - t_lo > 1 {
                let mid = t_lo + (t_hi - t_lo) / 2;
                match raise(self, mid, &offsets, &mant) {
                    Some(r) => {
                        t_lo = mid;
                        best = Some(r);
                    }
                    None => t_hi = mid,
                }
            }
            match best {
                Some((o, m, l)) => {
                    offsets = o;
                    mant = m;
                    layout = l;
                }
                None => {
                    // The blocks cannot rise together: raise the lowest alone, by one step at
                    // least (it was shown to fit), then as far as it fits.
                    let (mut b_lo, mut b_hi) = (offsets[lowest], SnrOffset::MAX_INDEX + 1);
                    let mut single = None;
                    while b_hi - b_lo > 1 {
                        let mid = b_lo + (b_hi - b_lo) / 2;
                        let mut o = offsets;
                        o[lowest] = mid;
                        let mut m = mant;
                        m[lowest] = self.block_bits(lowest, mid, p);
                        match place(&o, &m) {
                            Some(l) => {
                                b_lo = mid;
                                single = Some((o, m, l));
                            }
                            None => b_hi = mid,
                        }
                    }
                    match single {
                        Some((o, m, l)) => {
                            offsets = o;
                            mant = m;
                            layout = l;
                        }
                        None => break,
                    }
                }
            }
        }
        Some(Allocation { offsets, layout })
    }

    /// Step 5a: the final bap of every block at its offset, and quantization.
    fn finalize_allocation(&mut self, offsets: &[u16; 6], p: &Params) {
        let nch = self.cfg.channels();
        for (b, &s) in offsets.iter().enumerate() {
            let _ = self.block_bits(b, s, p);
            let sent = b == 0 || s != offsets[b - 1];
            let blk = &mut self.blocks[b];
            blk.side.snr = sent.then(|| SnrOffset::from_index(s));
            let inputs: [ChannelInput<'_>; MAX_CHANNELS] = std::array::from_fn(|ch| ChannelInput {
                coefs: &blk.coefs[ch],
                exps: &blk.exps[ch],
                bap: &blk.bap[ch],
                end: if ch < nch {
                    channel_end(&self.cfg, ch)
                } else {
                    0
                },
            });
            quantize::quantize_block(&inputs[..nch], &mut blk.quant[..nch]);
        }
    }

    /// Step 5b: packs the frame into `self.out` and sets both CRCs. Returns crcrsv.
    fn write_frame(&mut self, params: &Params) -> bool {
        let frame_bits = self.out.len() * 8;
        let nch = self.cfg.channels();
        let mut w = BitWriter::new(&mut self.out);
        syntax::write_header(&self.cfg, &mut w);
        for (b, blk) in self.blocks.iter().enumerate() {
            syntax::write_block_side(&self.cfg, b, &blk.side, params, &blk.exps, &mut w);
            syntax::write_mantissas(&self.cfg, &blk.quant[..nch], &mut w);
        }
        debug_assert!(w.position() + TRAILER_BITS <= frame_bits, "frame overflow");
        w.zero_fill_to(frame_bits - TRAILER_BITS); // auxbits
        w.put(1, 0); // auxdatae
        w.put(1, 0); // crcrsv
        w.put(16, 0); // crc2
        debug_assert!(!w.overflowed());
        let out = w.finish();
        crc::seal_frame(out, self.cfg.bit_rate().five_eighths_bytes())
    }
}

/// Checks that the smallest legal frame of a configuration fits: exponents (D45) in block 0
/// only, no mantissas, no padding beyond what §5.5 needs. Called by
/// [`ConfigBuilder::build`](crate::ConfigBuilder::build).
pub fn check_rate_feasible(cfg: &Config) -> Result<(), ConfigError> {
    let nf = cfg.nfchans();
    let mut sides = [BlockSide::default(); 6];
    for (b, side) in sides.iter_mut().enumerate() {
        if b == 0 {
            for ch in 0..cfg.channels() {
                side.strategy[ch] = ChannelStrategy::New(if ch < nf {
                    ExpStrategy::D45
                } else {
                    ExpStrategy::D15
                });
            }
            side.rematstr = true;
            side.snr = Some(SnrOffset::default());
        }
    }
    // Exponent values do not change the size of the syntax, only its content.
    let exps = [[0u8; N_COEFS]; MAX_CHANNELS];
    let mut c = BitCounter::new();
    syntax::write_header(cfg, &mut c);
    let header = c.position();
    let side_bits: [usize; 6] = std::array::from_fn(|b| {
        let mut c = BitCounter::new();
        syntax::write_block_side(cfg, b, &sides[b], &Params::NOMINAL, &exps, &mut c);
        c.position()
    });
    let frame_bits = cfg.frame_bytes() * 8;
    let p58 = cfg.bit_rate().five_eighths_bytes() * 8;
    match syntax::layout(header, &side_bits, &[0; 6], frame_bits, p58) {
        Some(_) => Ok(()),
        None => {
            let needed = (header + side_bits.iter().sum::<usize>() + TRAILER_BITS)
                .max(header + side_bits[0] + side_bits[1]);
            Err(ConfigError::RateTooLow {
                kbps: cfg.bit_rate().kbps(),
                channels: cfg.channels(),
                bandwidth: cfg.bandwidth_code().code(),
                needed,
                available: frame_bits,
            })
        }
    }
}

/// A syncframe of digital silence for `cfg`: what [`Encoder::encode_frame`] makes from zeros
/// on a fresh encoder. Valid in place of any frame of the same configuration (docs/SPEC.md
/// §6.6), and decodes to exact zeros (dither is off for silent blocks).
pub fn silent_frame(cfg: &Config) -> Vec<u8> {
    let mut enc = Encoder::new(*cfg);
    let zeros = [0.0f32; FRAME_SAMPLES];
    let input: Vec<&[f32]> = (0..cfg.channels()).map(|_| &zeros[..]).collect();
    enc.encode_checked(&input);
    enc.out
}
