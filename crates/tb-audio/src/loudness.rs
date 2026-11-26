//! Loudness and true peak after ITU-R BS.1770-4, at 48 kHz.
//!
//! ATSC A/85:2013 (with Corrigendum 1) makes BS.1770 the measurement (§2.1 p.10, Annex A
//! pp.38–43): K-weighted mean square per channel, channel weights, gating in 400 ms blocks
//! every 100 ms with an absolute gate at −70 LKFS and a relative gate 10 dB below the
//! absolute-gated loudness (Annex A p.41), true peak by 4× oversampling (Annex A §A.3 p.43).
//! A/85 describes the algorithm but does not print its constants; those below are from
//! BS.1770-4 (10/2015):
//!
//! - K-weighting, Annex 1 Table 1 (pre-filter, the "head" shelf) and Table 2 (RLB high-pass),
//!   coefficients at 48 kHz;
//! - channel weights, Annex 1 Table 3: L, R, C 1.0; Ls, Rs 1.41 (+1.5 dB); LFE excluded;
//! - loudness `L = −0.691 + 10 log10 Σ G_i z_i`, Annex 1 eq. (2);
//! - gating, Annex 1 eqs. (3)–(7): blocks of 400 ms overlapping by 75 %, `Γa = −70 LKFS`,
//!   `Γr = L(above Γa) − 10 LU`;
//! - momentary (400 ms) and short-term (3 s) loudness as in EBU Tech 3341 §2.2 / ITU-R
//!   BS.1771, updated every 100 ms;
//! - true peak, Annex 2: 4× oversampling, absolute maximum. BS.1770-4 gives a 48-tap
//!   interpolating filter as an example; ours is a 4-phase, 24-tap-per-phase Kaiser-windowed
//!   sinc (passband to 20 kHz within 0.005 dB), checked against the EBU Tech 3341 true-peak
//!   tolerances (+0.2 / −0.4 dB) in the tests.
//!
//! Validation: `tests/ebu3341.rs` (EBU Tech 3341 v4 test signals synthesized here) and
//! `tests/ffmpeg.rs` (the same files measured by ffmpeg's `ebur128` filter).

use crate::fir::{Kernel, kaiser_beta, kaiser_length};
use crate::layout::{OutputChannel, OutputLayout};
use std::fmt;

/// Samples per 100 ms step at 48 kHz.
pub const STEP: usize = 4_800;
/// The loudness of zero power, used where a log of zero would be taken.
const FLOOR: f64 = -200.0;

/// A loudness in LUFS (= LKFS, A/85 §3.4).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Loudness(pub f64);

impl Loudness {
    /// The value in LUFS.
    pub fn lufs(self) -> f64 {
        self.0
    }
    /// BS.1770-4 eq. (2): the loudness of a weighted mean square.
    pub fn from_power(p: f64) -> Self {
        if p > 0.0 {
            Self(-0.691 + 10.0 * p.log10())
        } else {
            Self(FLOOR)
        }
    }
    /// The weighted mean square of this loudness.
    pub fn to_power(self) -> f64 {
        10f64.powf((self.0 + 0.691) / 10.0)
    }
}

impl fmt::Display for Loudness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2} LUFS", self.0)
    }
}

/// A true-peak level in dB relative to full scale (dBTP, A/85 §3.4).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Dbtp(pub f64);

impl Dbtp {
    /// The level of a linear peak magnitude.
    pub fn from_linear(a: f64) -> Self {
        Self(if a > 0.0 { 20.0 * a.log10() } else { FLOOR })
    }
    /// The linear magnitude.
    pub fn linear(self) -> f64 {
        10f64.powf(self.0 / 20.0)
    }
}

impl fmt::Display for Dbtp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2} dBTP", self.0)
    }
}

/// Why a meter refused its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MeterError {
    /// Wrong number of planes.
    #[error("expected {expected} planes, got {got}")]
    Planes {
        /// Planes expected.
        expected: usize,
        /// Planes given.
        got: usize,
    },
    /// The planes differ in length.
    #[error("planes differ in length")]
    Ragged,
}

pub(crate) fn check_planes(planes: &[&[f32]], expected: usize) -> Result<usize, MeterError> {
    if planes.len() != expected {
        return Err(MeterError::Planes {
            expected,
            got: planes.len(),
        });
    }
    let n = planes.first().map_or(0, |p| p.len());
    if planes.iter().any(|p| p.len() != n) {
        return Err(MeterError::Ragged);
    }
    Ok(n)
}

/// A second-order section, transposed direct form II, in `f64`.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    const fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// BS.1770-4 Annex 1 Table 1: the pre-filter ("stage 1") at 48 kHz.
const PRE: Biquad = Biquad::new(
    [
        1.535_124_859_586_97,
        -2.691_696_189_406_38,
        1.198_392_810_852_85,
    ],
    [-1.690_659_293_182_41, 0.732_480_774_215_85],
);
/// BS.1770-4 Annex 1 Table 2: the RLB high-pass ("stage 2") at 48 kHz.
const RLB: Biquad = Biquad::new(
    [1.0, -2.0, 1.0],
    [-1.990_047_454_833_98, 0.990_072_250_366_21],
);

/// The K-weighting filter of one channel at 48 kHz.
#[derive(Clone, Copy, Debug)]
pub struct KWeighting {
    pre: Biquad,
    rlb: Biquad,
}

impl Default for KWeighting {
    fn default() -> Self {
        Self { pre: PRE, rlb: RLB }
    }
}

impl KWeighting {
    /// Filter one sample.
    #[inline]
    pub fn run(&mut self, x: f64) -> f64 {
        self.rlb.run(self.pre.run(x))
    }
}

/// BS.1770-4 Annex 1 Table 3: the weight of an output channel (`None`: not measured).
pub fn channel_weight(c: OutputChannel) -> Option<f64> {
    match c {
        OutputChannel::L | OutputChannel::C | OutputChannel::R => Some(1.0),
        OutputChannel::Ls | OutputChannel::Rs => Some(1.41),
        OutputChannel::Lfe => None,
    }
}

/// K-weighted, channel-weighted energy in 100 ms steps; the common front of the loudness
/// meter and the normalizer.
#[derive(Clone, Debug)]
pub struct BlockMeter {
    filters: Vec<KWeighting>,
    weights: Vec<f64>,
    /// Weighted sum of squares in the current step, and samples in it.
    acc: f64,
    fill: usize,
    /// The last 30 complete steps (3 s), as weighted mean squares; `head` is the next slot.
    steps: [f64; 30],
    head: usize,
    complete: u64,
}

impl BlockMeter {
    /// A meter for the channels of `layout` (LFE ignored).
    pub fn new(layout: OutputLayout) -> Self {
        let weights = layout
            .channels()
            .iter()
            .map(|&c| channel_weight(c).unwrap_or(0.0))
            .collect();
        Self {
            filters: vec![KWeighting::default(); layout.count()],
            weights,
            acc: 0.0,
            fill: 0,
            steps: [0.0; 30],
            head: 0,
            complete: 0,
        }
    }

    /// The channel count.
    pub fn channels(&self) -> usize {
        self.weights.len()
    }

    /// Samples still needed to complete the current step.
    pub fn until_step(&self) -> usize {
        STEP - self.fill
    }

    /// Measure `n` samples of `planes` starting at `off`, which must not cross a step boundary
    /// (`n ≤ until_step()`). Returns whether a step completed.
    pub fn feed(&mut self, planes: &[&[f32]], off: usize, n: usize) -> bool {
        let n = n.min(self.until_step());
        for ((p, f), &w) in planes.iter().zip(&mut self.filters).zip(&self.weights) {
            if w == 0.0 {
                continue;
            }
            let mut s = 0.0;
            for &x in &p[off..off + n] {
                let y = f.run(f64::from(x));
                s += y * y;
            }
            self.acc += w * s;
        }
        self.fill += n;
        if self.fill < STEP {
            return false;
        }
        self.steps[self.head] = self.acc / STEP as f64;
        self.head = (self.head + 1) % self.steps.len();
        self.complete += 1;
        self.acc = 0.0;
        self.fill = 0;
        true
    }

    /// The mean power of the last `k` complete steps (k ≤ 30), if there are that many.
    pub fn mean_of_last(&self, k: usize) -> Option<f64> {
        if k == 0 || k > self.steps.len() || self.complete < k as u64 {
            return None;
        }
        let n = self.steps.len();
        let s: f64 = (1..=k).map(|i| self.steps[(self.head + n - i) % n]).sum();
        Some(s / k as f64)
    }

    /// Complete 100 ms steps so far.
    pub fn steps(&self) -> u64 {
        self.complete
    }
}

/// Bins of the integrated-loudness histogram: 0.01 LU from −70 to +10 LUFS.
const BIN_LO: f64 = -70.0;
const BIN_WIDTH: f64 = 0.01;
const BINS: usize = 8_000;

/// The gating histogram (BS.1770-4 Annex 1 eqs. (3)–(7)) over every block since the start or
/// the last reset, in bounded memory: per bin of 0.01 LU, the count and the sum of the blocks'
/// powers. Sums are exact; only the relative gate's threshold is resolved to 0.01 LU.
#[derive(Clone, Debug)]
pub struct GatingHistogram {
    count: Vec<u64>,
    power: Vec<f64>,
}

impl Default for GatingHistogram {
    fn default() -> Self {
        Self {
            count: vec![0; BINS],
            power: vec![0.0; BINS],
        }
    }
}

impl GatingHistogram {
    /// Add a 400 ms block of mean power `p`. Blocks at or below the absolute gate are not kept.
    pub fn add(&mut self, p: f64) {
        let l = Loudness::from_power(p).0;
        if l <= BIN_LO {
            return;
        }
        let b = (((l - BIN_LO) / BIN_WIDTH) as usize).min(BINS - 1);
        self.count[b] += 1;
        self.power[b] += p;
    }

    /// The gated loudness of the blocks added, if any is above the absolute gate.
    pub fn loudness(&self) -> Option<Loudness> {
        let n: u64 = self.count.iter().sum();
        if n == 0 {
            return None;
        }
        let abs = Loudness::from_power(self.power.iter().sum::<f64>() / n as f64);
        let rel = abs.0 - 10.0;
        let first = ((rel - BIN_LO) / BIN_WIDTH).ceil().max(0.0) as usize;
        let (mut c, mut p) = (0u64, 0.0);
        for b in first.min(BINS)..BINS {
            c += self.count[b];
            p += self.power[b];
        }
        (c > 0).then(|| Loudness::from_power(p / c as f64))
    }
}

/// A BS.1770-4 loudness and true-peak meter at 48 kHz (momentary, short-term, integrated,
/// true peak).
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    blocks: BlockMeter,
    hist: GatingHistogram,
    peak: Option<TruePeak>,
}

impl LoudnessMeter {
    /// A meter for `layout`; with `true_peak`, the true peak is measured too (it costs about as
    /// much again as the loudness).
    pub fn new(layout: OutputLayout, true_peak: bool) -> Self {
        Self {
            blocks: BlockMeter::new(layout),
            hist: GatingHistogram::default(),
            peak: true_peak.then(|| TruePeak::new(layout.count())),
        }
    }

    /// Measure the next samples (one plane per channel of the layout, A/52 order).
    pub fn push(&mut self, planes: &[&[f32]]) -> Result<(), MeterError> {
        let n = check_planes(planes, self.blocks.channels())?;
        let mut off = 0;
        while off < n {
            let k = self.blocks.until_step().min(n - off);
            if self.blocks.feed(planes, off, k)
                && let Some(p) = self.blocks.mean_of_last(4)
            {
                self.hist.add(p);
            }
            off += k;
        }
        if let Some(tp) = &mut self.peak {
            tp.push(planes);
        }
        Ok(())
    }

    /// Momentary loudness: the last 400 ms, as of the last 100 ms step.
    pub fn momentary(&self) -> Option<Loudness> {
        self.blocks.mean_of_last(4).map(Loudness::from_power)
    }

    /// Short-term loudness: the last 3 s, as of the last 100 ms step.
    pub fn short_term(&self) -> Option<Loudness> {
        self.blocks.mean_of_last(30).map(Loudness::from_power)
    }

    /// Integrated (gated) loudness since the start.
    pub fn integrated(&self) -> Option<Loudness> {
        self.hist.loudness()
    }

    /// The largest true peak since the start, if measured.
    pub fn true_peak(&self) -> Option<Dbtp> {
        self.peak.as_ref().map(TruePeak::max)
    }
}

/// Taps on each side of the true-peak interpolator.
const TP_HALF: usize = 12;
/// Oversampling factor (BS.1770-4 Annex 2).
pub const OVERSAMPLE: usize = 4;

/// A 4× interpolator of one channel giving, per input sample, the largest magnitude of the
/// sample and the three interpolated points after it. It lags its input by [`Oversampler::LAG`] samples.
#[derive(Clone, Debug)]
pub struct Oversampler {
    /// The three interpolating rows (positions 1/4, 2/4, 3/4), `2·TP_HALF` taps each.
    rows: [[f32; 2 * TP_HALF]; OVERSAMPLE - 1],
    /// History, written twice (at `i` and `i + 2·TP_HALF`) so a window is contiguous.
    hist: [f32; 4 * TP_HALF],
    at: usize,
}

impl Default for Oversampler {
    fn default() -> Self {
        // Passband 20 kHz, stopband 28 kHz (images of 20 kHz content fold to 28 kHz), the
        // attenuation 24 taps allow by Kaiser's formula (about 65 dB).
        let transition = 8_000.0 / 48_000.0;
        let atten = 7.95 + (2 * TP_HALF) as f64 * 2.285 * 2.0 * std::f64::consts::PI * transition;
        debug_assert!(kaiser_length(atten, transition) <= (2 * TP_HALF) as f64 + 1e-9);
        let k = Kernel::new(1.0, TP_HALF as f64, kaiser_beta(atten));
        let mut rows = [[0.0f32; 2 * TP_HALF]; OVERSAMPLE - 1];
        for (r, row) in rows.iter_mut().enumerate() {
            let phi = (r + 1) as f64 / OVERSAMPLE as f64;
            let c: Vec<f64> = (0..2 * TP_HALF)
                .map(|j| k.at(phi + (TP_HALF as f64 - 1.0) - j as f64))
                .collect();
            let sum: f64 = c.iter().sum();
            for (d, v) in row.iter_mut().zip(&c) {
                *d = (v / sum) as f32;
            }
        }
        Self {
            rows,
            hist: [0.0; 4 * TP_HALF],
            at: 0,
        }
    }
}

impl Oversampler {
    /// The delay between a sample entering and its peak being returned, in samples.
    pub const LAG: usize = TP_HALF;

    /// Take sample `x`; return the peak magnitude over the interval starting at the sample
    /// [`Self::LAG`] samples ago: that sample and the three interpolated points after it.
    #[inline]
    pub fn push(&mut self, x: f32) -> f32 {
        let w = 2 * TP_HALF;
        self.hist[self.at] = x;
        self.hist[self.at + w] = x;
        self.at = (self.at + 1) % w;
        // The window: the oldest of the last 2·TP_HALF samples first.
        let win = &self.hist[self.at..self.at + w];
        let mut m = win[TP_HALF - 1].abs();
        for row in &self.rows {
            m = m.max(crate::resample::dot(win, row).abs());
        }
        m
    }

    /// Take sample `x` into the history without interpolating (for a caller that knows, from
    /// [`Self::gain_bound`], that the peaks are below what concerns it).
    #[inline]
    pub fn feed(&mut self, x: f32) {
        let w = 2 * TP_HALF;
        self.hist[self.at] = x;
        self.hist[self.at + w] = x;
        self.at = (self.at + 1) % w;
    }

    /// The largest sum of absolute taps over the interpolating rows: no interpolated point
    /// exceeds this times the largest sample magnitude in its window.
    pub fn gain_bound(&self) -> f32 {
        self.rows
            .iter()
            .map(|r| r.iter().map(|v| v.abs()).sum::<f32>())
            .fold(1.0, f32::max)
    }

    /// The largest sample magnitude in the history (the window of the next [`Self::push`]
    /// except its oldest sample, plus that sample).
    pub fn window_max(&self) -> f32 {
        self.hist[..2 * TP_HALF]
            .iter()
            .fold(0.0f32, |m, &v| m.max(v.abs()))
    }
}

/// True-peak measurement over all channels (BS.1770-4 Annex 2).
#[derive(Clone, Debug)]
pub struct TruePeak {
    ch: Vec<Oversampler>,
    max: f32,
}

impl TruePeak {
    /// A meter for `channels` channels.
    pub fn new(channels: usize) -> Self {
        Self {
            ch: vec![Oversampler::default(); channels],
            max: 0.0,
        }
    }
    /// Measure the next samples (one plane per channel; extra planes are ignored).
    pub fn push(&mut self, planes: &[&[f32]]) {
        for (o, p) in self.ch.iter_mut().zip(planes) {
            for &x in p.iter() {
                self.max = self.max.max(o.push(x));
            }
        }
    }
    /// The largest true peak so far.
    pub fn max(&self) -> Dbtp {
        // Flush: the last samples are LAG behind; peaks there are included once more samples
        // arrive. The sample peak of the held window bounds them from below.
        let held = self
            .ch
            .iter()
            .map(Oversampler::window_max)
            .fold(0.0, f32::max);
        Dbtp::from_linear(f64::from(self.max.max(held)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{db_to_amp, sine};

    /// The K-weighting at 997 Hz is +0.69 dB, which the −0.691 of eq. (2) cancels: a
    /// full-scale 997 Hz sine in one channel reads −3.01 LUFS (BS.1770-4 Annex 1, note to
    /// eq. (2)).
    #[test]
    fn full_scale_sine_in_one_channel_is_minus_3_01() {
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
        let x = sine(997.0, 1.0, 48_000, 48_000 * 5, 0.0);
        let z = vec![0.0; x.len()];
        m.push(&[&x, &z]).unwrap();
        let l = m.integrated().unwrap().lufs();
        assert!((l + 3.01).abs() < 0.01, "{l}");
    }

    #[test]
    fn silence_has_no_integrated_loudness() {
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, true);
        let z = vec![0.0; 48_000 * 2];
        m.push(&[&z, &z]).unwrap();
        assert_eq!(m.integrated(), None);
        assert!(m.momentary().unwrap().lufs() <= FLOOR);
        assert!(m.true_peak().unwrap().0 <= FLOOR);
    }

    /// The interpolator is flat to 20 kHz: a 20 kHz sine's interpolated points sit on the sine.
    #[test]
    fn oversampler_interpolates_to_20_khz() {
        for f in [997.0, 10_000.0, 18_000.0, 20_000.0] {
            let x = sine(f, 0.5, 48_000, 2_000, 0.123);
            let mut o = Oversampler::default();
            let mut worst = 0.0f64;
            for (i, &s) in x.iter().enumerate() {
                let p = o.push(s);
                if i > 100 {
                    // The interval starting LAG samples ago has peak at most 0.5 and at least
                    // the sample magnitude.
                    let m = f64::from(x[i - Oversampler::LAG].abs());
                    assert!(f64::from(p) >= m - 1e-6);
                    worst = worst.max(f64::from(p) - 0.5);
                }
            }
            assert!(worst < 0.5 * (db_to_amp(0.01) - 1.0), "{f} Hz: {worst}");
        }
    }

    /// Momentary and short-term follow a level step within their windows.
    #[test]
    fn momentary_and_short_term_windows() {
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
        let a = sine(1000.0, db_to_amp(-20.0), 48_000, 48_000 * 4, 0.0);
        m.push(&[&a, &a]).unwrap();
        assert!((m.momentary().unwrap().lufs() + 20.0).abs() < 0.05);
        assert!((m.short_term().unwrap().lufs() + 20.0).abs() < 0.05);
        let b = sine(1000.0, db_to_amp(-40.0), 48_000, 4_800 * 5, 0.0);
        m.push(&[&b, &b]).unwrap();
        assert!((m.momentary().unwrap().lufs() + 40.0).abs() < 0.05);
        assert!(m.short_term().unwrap().lufs() > -22.0);
    }
}
