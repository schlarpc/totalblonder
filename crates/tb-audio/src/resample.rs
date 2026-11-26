//! Resampling any input rate (8–192 kHz) to 48 kHz, with a ratio trimmed continuously by up to
//! ±500 ppm (SPEC §5.4: the session's skew plus a correction on the buffer).
//!
//! Design: a Kaiser-windowed sinc evaluated at a continuously variable fractional position
//! (Smith's "bandlimited interpolation"). The kernel is tabulated at 512 phases per input
//! sample, and the coefficients for a position between two phases are interpolated linearly
//! (24-bit fraction). The input position is a 64.64 fixed-point number advanced by an exact
//! step, so the ratio has no rounding drift and a trim change only changes the step: the
//! position, and so the waveform, stays continuous (no click, no phase jump).
//!
//! Why not a fixed rational polyphase (L/M) filter: the trim makes the ratio irrational and
//! time-varying, which a fixed L/M bank cannot follow; ±500 ppm needs a resolution far below
//! 1/512 of a sample, which the interpolated table gives (its error is below −140 dB, measured
//! in the tests). Why not an asynchronous IIR or a polynomial (Farrow) interpolator: neither
//! reaches 100 dB with this little arithmetic at audio band edges.
//!
//! Filter choice, as a function of the lower of the two rates `lo = min(fs_in, 48 kHz)`:
//! - `lo ≥ 44.1 kHz`: passband to 20 kHz, stopband from `lo − 20 kHz`. Only content that
//!   lands above 20 kHz in the output is allowed through the transition band (an image or alias
//!   of a frequency `f` lands at `lo − f` after folding), so nothing audible is aliased while
//!   the transition band is twice as wide as a "stop at Nyquist" design (half the taps).
//! - `lo < 44.1 kHz` (speech rates, 32 kHz): passband to `0.45 lo`, stopband from `0.5 lo`;
//!   here the region above the input's Nyquist is audible in the output, so images must be gone
//!   by the input's Nyquist.
//! - Stopband attenuation 120 dB (Kaiser `β = 12.26`).
//!
//! The cutoff is the middle of the transition band; at 44.1 kHz and 48 kHz input it falls on the
//! input's Nyquist exactly, so the phase-0 row of the table is an exact unit impulse. 48 → 48 kHz
//! with zero trim is therefore bit-transparent as long as the trim has been zero since
//! construction (or [`Resampler::reset`]): the position stays on integer samples and every
//! output is `1.0 · x + Σ 0.0 · x'`. After a non-zero trim the position is fractional and the
//! output is a (transparent, see the measurements) fractional delay.
//!
//! Timing: output sample `n` is the input signal at input position `p(n)`
//! ([`Resampler::next_output_position`]); there is no group delay in positions. An output can
//! only be computed once `K` input samples past its position have arrived, so the processing
//! latency is `K` input samples (`K` is [`Design::half_taps`]).

use crate::fir::{Kernel, kaiser_beta, kaiser_length};

/// The output sampling rate of every track (A/53 Part 5 §5.1: AC-3 at 48 kHz).
pub const OUTPUT_RATE: u32 = 48_000;

/// Phases of the kernel table, as a power of two.
const PHASE_BITS: u32 = 9;
const PHASES: usize = 1 << PHASE_BITS;
/// Input samples taken into the history at a time; bounds the history buffer.
const CHUNK: usize = 4096;
/// Stopband attenuation of the design, dB.
const ATTEN_DB: f64 = 120.0;

/// An input sampling rate the resampler accepts: 8 kHz to 192 kHz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputRate(u32);

/// A rate outside 8–192 kHz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("input sampling rate {0} Hz is outside 8000..=192000")]
pub struct RateError(pub u32);

impl InputRate {
    /// 48 kHz.
    pub const HZ_48000: Self = Self(48_000);
    /// 44.1 kHz.
    pub const HZ_44100: Self = Self(44_100);

    /// `hz`, if within 8–192 kHz.
    pub const fn new(hz: u32) -> Result<Self, RateError> {
        if hz < 8_000 || hz > 192_000 {
            return Err(RateError(hz));
        }
        Ok(Self(hz))
    }
    /// The rate in Hz.
    pub const fn hz(self) -> u32 {
        self.0
    }
}

/// A trim of the conversion ratio in parts per billion, within ±500 ppm (SPEC §5.4).
///
/// A positive trim takes input faster: `1 + trim` input seconds are consumed for every output
/// second, so the output of a given input is shorter. Use it when the input is running ahead
/// (its samples pile up); a negative trim when it lags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Trim(i32);

/// A trim beyond ±500 ppm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("trim {0} ppb is beyond ±500 ppm")]
pub struct TrimError(pub i64);

impl Trim {
    /// The largest trim, in ppb.
    pub const MAX_PPB: i32 = 500_000;
    /// No trim.
    pub const ZERO: Self = Self(0);

    /// `ppb` parts per billion, if within ±500 ppm.
    pub fn new(ppb: i64) -> Result<Self, TrimError> {
        match i32::try_from(ppb) {
            Ok(v) if (-Self::MAX_PPB..=Self::MAX_PPB).contains(&v) => Ok(Self(v)),
            _ => Err(TrimError(ppb)),
        }
    }
    /// `ppb` clamped to ±500 ppm.
    pub fn saturating(ppb: i64) -> Self {
        let c = ppb.clamp(-i64::from(Self::MAX_PPB), i64::from(Self::MAX_PPB));
        // Within ±500_000 after the clamp.
        Self(i32::try_from(c).unwrap_or(0))
    }
    /// The trim in parts per billion.
    pub const fn ppb(self) -> i32 {
        self.0
    }
}

/// Why [`Resampler::process`] refused its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResampleError {
    /// The number of input or output planes is not the resampler's channel count.
    #[error("expected {expected} channels, got {got}")]
    Channels {
        /// The resampler's channel count.
        expected: usize,
        /// Planes given.
        got: usize,
    },
    /// The input planes differ in length.
    #[error("input planes differ in length")]
    Ragged,
}

/// The filter a [`Resampler`] uses (see the module documentation for the choices).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Design {
    /// The input rate.
    pub input_rate: InputRate,
    /// Upper edge of the passband, Hz.
    pub passband_hz: f64,
    /// Lower edge of the stopband, Hz.
    pub stopband_hz: f64,
    /// Design stopband attenuation, dB.
    pub atten_db: f64,
    /// Kaiser `β`.
    pub beta: f64,
    /// Taps on each side of the position (`K`); the filter has `2K` taps per output sample.
    pub half_taps: usize,
    /// Kernel phases tabulated per input sample.
    pub phases: usize,
}

impl Design {
    /// The design for `rate`.
    pub fn for_rate(rate: InputRate) -> Self {
        let fs = f64::from(rate.hz());
        let lo = fs.min(f64::from(OUTPUT_RATE));
        let (pb, sb) = if lo >= 44_100.0 {
            (20_000.0, lo - 20_000.0)
        } else {
            (0.45 * lo, 0.5 * lo)
        };
        let n = kaiser_length(ATTEN_DB, (sb - pb) / fs);
        // Half-length rounded up to a multiple of 4, so 2K is a multiple of 8 (the dot
        // product's lane count).
        let half = ((n / 2.0).ceil() as usize).div_ceil(4) * 4;
        Self {
            input_rate: rate,
            passband_hz: pb,
            stopband_hz: sb,
            atten_db: ATTEN_DB,
            beta: kaiser_beta(ATTEN_DB),
            half_taps: half,
            phases: PHASES,
        }
    }

    /// The cutoff (middle of the transition band), Hz.
    pub fn cutoff_hz(&self) -> f64 {
        (self.passband_hz + self.stopband_hz) / 2.0
    }

    fn kernel(&self) -> Kernel {
        let scale = 2.0 * self.cutoff_hz() / f64::from(self.input_rate.hz());
        Kernel::new(scale, self.half_taps as f64, self.beta)
    }

    /// Magnitude response of the continuous kernel (before tabulation and `f32` rounding) at
    /// `f_hz`, in dB. The kernel is sampled at `phases` points per input sample; its spectrum
    /// is what an input tone at `f_hz`, or an image at `f_hz`, is multiplied by.
    pub fn response_db(&self, f_hz: f64) -> f64 {
        let k = self.kernel();
        let p = self.phases as f64;
        let n = (2 * self.half_taps * self.phases) as i64;
        let w = 2.0 * std::f64::consts::PI * f_hz / (p * f64::from(self.input_rate.hz()));
        let (mut re, mut im) = (0.0, 0.0);
        for i in -n / 2..=n / 2 {
            let h = k.at(i as f64 / p);
            re += h * (w * i as f64).cos();
            im -= h * (w * i as f64).sin();
        }
        10.0 * ((re * re + im * im) / (p * p)).max(1e-300).log10()
    }
}

/// A streaming resampler from one [`InputRate`] to 48 kHz for a fixed number of channels, with
/// per-channel history. After construction it does not allocate, except to grow the caller's
/// output vectors.
#[derive(Clone, Debug)]
pub struct Resampler {
    design: Design,
    taps: usize,
    /// `(PHASES + 1) · taps` coefficients: row `r` is the kernel at fractional position
    /// `r / PHASES`, normalized to unit sum.
    table: Vec<f32>,
    /// `PHASES · taps`: row `r + 1` minus row `r`.
    delta: Vec<f32>,
    coef: Vec<f32>,
    /// Per channel: history and pending input, `len` valid samples.
    bufs: Vec<Vec<f32>>,
    len: usize,
    /// Absolute input index of `bufs[_][0]` (negative while the zero prefill is in the buffer).
    base: i64,
    /// Integer part of the position, as an index into the buffers.
    pos: usize,
    /// Fractional part of the position, 2^-64 units.
    frac: u64,
    step_int: usize,
    step_frac: u64,
    trim: Trim,
}

impl Resampler {
    /// A resampler from `rate` to 48 kHz for `channels` channels (at least one).
    pub fn new(rate: InputRate, channels: usize) -> Self {
        let design = Design::for_rate(rate);
        let k = design.half_taps;
        let taps = 2 * k;
        let kernel = design.kernel();
        let mut table = Vec::with_capacity((PHASES + 1) * taps);
        let mut row = vec![0.0f64; taps];
        for r in 0..=PHASES {
            let phi = r as f64 / PHASES as f64;
            for (j, c) in row.iter_mut().enumerate() {
                // Coefficient j multiplies x[i − K + 1 + j]; the position is i + phi.
                *c = kernel.at(phi + (k as f64 - 1.0) - j as f64);
            }
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|c| (c / sum) as f32));
        }
        let delta = (0..PHASES * taps)
            .map(|i| table[i + taps] - table[i])
            .collect();
        let channels = channels.max(1);
        let mut s = Self {
            design,
            taps,
            table,
            delta,
            coef: vec![0.0; taps],
            bufs: vec![vec![0.0; taps + CHUNK + 8]; channels],
            len: 0,
            base: 0,
            pos: 0,
            frac: 0,
            step_int: 0,
            step_frac: 0,
            trim: Trim::ZERO,
        };
        s.reset();
        s
    }

    /// Forget all input: the next input sample is position 0 again, preceded by silence. The
    /// trim is set to zero.
    pub fn reset(&mut self) {
        let k = self.design.half_taps;
        for b in &mut self.bufs {
            b.fill(0.0);
        }
        self.len = k - 1;
        self.pos = k - 1;
        self.base = -(k as i64 - 1);
        self.frac = 0;
        self.set_trim(Trim::ZERO);
    }

    /// The filter in use.
    pub fn design(&self) -> &Design {
        &self.design
    }

    /// The channel count.
    pub fn channels(&self) -> usize {
        self.bufs.len()
    }

    /// The current trim.
    pub fn trim(&self) -> Trim {
        self.trim
    }

    /// Set the trim; it applies from the next output sample.
    pub fn set_trim(&mut self, trim: Trim) {
        self.trim = trim;
        let fs = u128::from(self.design.input_rate.hz());
        // ppb in -5e5..=5e5, so 1e9 + ppb is positive.
        let scale = u128::try_from(1_000_000_000i64 + i64::from(trim.ppb())).unwrap_or(0);
        let q = ((fs * scale) << 64) / (u128::from(OUTPUT_RATE) * 1_000_000_000);
        self.step_int = usize::try_from(q >> 64).unwrap_or(usize::MAX);
        self.step_frac = q as u64;
    }

    /// The exact input position of the next output sample: `(index, fraction · 2^64)`, where
    /// index 0 is the first input sample given since construction or reset.
    pub fn next_output_position(&self) -> (i64, u64) {
        (self.base + self.pos as i64, self.frac)
    }

    /// Input samples received since construction or reset.
    pub fn input_consumed(&self) -> i64 {
        self.base + self.len as i64
    }

    /// Resample `input` (one plane per channel, equal lengths) and append the output samples
    /// to `output` (one vector per channel). Returns the number of samples appended per
    /// channel. Non-finite input samples are taken as zero.
    pub fn process(
        &mut self,
        input: &[&[f32]],
        output: &mut [Vec<f32>],
    ) -> Result<usize, ResampleError> {
        let ch = self.bufs.len();
        for got in [input.len(), output.len()] {
            if got != ch {
                return Err(ResampleError::Channels { expected: ch, got });
            }
        }
        let n = input[0].len();
        if input.iter().any(|p| p.len() != n) {
            return Err(ResampleError::Ragged);
        }
        let mut produced = 0;
        let mut off = 0;
        while off < n {
            let room = self.bufs[0].len() - self.len;
            let take = room.min(n - off);
            for (b, p) in self.bufs.iter_mut().zip(input) {
                for (d, &s) in b[self.len..self.len + take]
                    .iter_mut()
                    .zip(&p[off..off + take])
                {
                    *d = if s.is_finite() { s } else { 0.0 };
                }
            }
            self.len += take;
            off += take;
            produced += self.run(output);
            self.compact();
        }
        Ok(produced)
    }

    fn run(&mut self, output: &mut [Vec<f32>]) -> usize {
        let k = self.design.half_taps;
        let taps = self.taps;
        let mut produced = 0;
        while self.pos + k < self.len {
            let phase = (self.frac >> (64 - PHASE_BITS)) as usize;
            // The next 24 bits of the fraction, as the interpolation weight between rows.
            let t = ((self.frac << PHASE_BITS) >> 40) as f32 * (1.0 / 16_777_216.0);
            let row = &self.table[phase * taps..][..taps];
            let d = &self.delta[phase * taps..][..taps];
            for ((c, &r), &d) in self.coef.iter_mut().zip(row).zip(d) {
                *c = r + t * d;
            }
            let start = self.pos + 1 - k;
            for (b, o) in self.bufs.iter().zip(output.iter_mut()) {
                o.push(dot(&b[start..start + taps], &self.coef));
            }
            produced += 1;
            let (f, carry) = self.frac.overflowing_add(self.step_frac);
            self.frac = f;
            self.pos += self.step_int + usize::from(carry);
        }
        produced
    }

    /// Drop history no longer needed: keep `K − 1` samples before the position.
    fn compact(&mut self) {
        let k = self.design.half_taps;
        let shift = (self.pos + 1).saturating_sub(k).min(self.len);
        if shift == 0 {
            return;
        }
        for b in &mut self.bufs {
            b.copy_within(shift..self.len, 0);
        }
        self.len -= shift;
        self.pos -= shift;
        self.base += shift as i64;
    }
}

/// A dot product in eight lanes (vectorized by the compiler; the sum order is fixed, so the
/// result is deterministic).
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    let tail: f32 = ra.iter().zip(rb).map(|(x, y)| x * y).sum();
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            acc[l] += x[l] * y[l];
        }
    }
    ((acc[0] + acc[4]) + (acc[2] + acc[6])) + ((acc[1] + acc[5]) + (acc[3] + acc[7])) + tail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::{fit_sine, sine};

    fn run(rate: u32, trim_ppb: i64, f: f64, amp: f64, secs: f64) -> (Vec<f32>, Resampler) {
        let rate = InputRate::new(rate).unwrap();
        let mut r = Resampler::new(rate, 1);
        r.set_trim(Trim::new(trim_ppb).unwrap());
        let x = sine(
            f,
            amp,
            rate.hz(),
            (secs * f64::from(rate.hz())) as usize,
            0.3,
        );
        let mut out = vec![Vec::new()];
        // Irregular block sizes, to exercise the streaming path.
        let mut off = 0;
        let mut i = 0;
        while off < x.len() {
            let n = [1, 7, 480, 1024, 4095, 4096, 9000][i % 7].min(x.len() - off);
            r.process(&[&x[off..off + n]], &mut out).unwrap();
            off += n;
            i += 1;
        }
        (out.pop().unwrap(), r)
    }

    /// THD+N of a resampled sine, dB below the tone (fit at the expected output frequency,
    /// skipping the start-up transient).
    fn thdn_db(rate: u32, trim_ppb: i64, f: f64) -> f64 {
        let (y, _) = run(rate, trim_ppb, f, 0.5, 1.0);
        let step = f64::from(rate) / 48_000.0 * (1.0 + trim_ppb as f64 * 1e-9);
        let f_out = f / f64::from(rate) * step * 48_000.0;
        let (amp, resid) = fit_sine(&y[4800..y.len() - 4800], f_out, 48_000.0);
        assert!((amp - 0.5).abs() < 0.5 * 1e-4, "gain {amp} at {f} Hz");
        20.0 * (resid / (amp / 2f64.sqrt())).log10()
    }

    #[test]
    fn rate_and_trim_are_validated() {
        assert!(InputRate::new(7_999).is_err());
        assert!(InputRate::new(192_001).is_err());
        assert!(InputRate::new(8_000).is_ok());
        assert!(Trim::new(500_001).is_err());
        assert!(Trim::new(-500_000).is_ok());
        assert_eq!(Trim::saturating(9_000_000).ppb(), 500_000);
        assert_eq!(Trim::saturating(i64::MIN).ppb(), -500_000);
    }

    /// 48 → 48 kHz with zero trim is the identity, sample for sample, with no delay in positions.
    #[test]
    fn same_rate_zero_trim_is_bit_transparent() {
        let rate = InputRate::HZ_48000;
        let mut r = Resampler::new(rate, 2);
        let a = sine(997.0, 0.9, 48_000, 20_000, 0.1);
        let b: Vec<f32> = (0..20_000)
            .map(|i| ((i * 7919) % 2001) as f32 / 1000.0 - 1.0)
            .collect();
        let mut out = vec![Vec::new(), Vec::new()];
        for (s, e) in [(0, 1), (1, 1000), (1000, 7000), (7000, 20_000)] {
            r.process(&[&a[s..e], &b[s..e]], &mut out).unwrap();
        }
        let k = r.design().half_taps;
        assert_eq!(out[0].len(), 20_000 - k);
        assert_eq!(out[0][..], a[..20_000 - k]);
        assert_eq!(out[1][..], b[..20_000 - k]);
    }

    /// The output count follows the ratio exactly: N input samples give N·48000/fs·(1 − trim)
    /// outputs, to within the filter's look-ahead.
    #[test]
    fn output_count_follows_the_trimmed_ratio() {
        for (rate, ppb) in [
            (44_100, 0),
            (44_100, 500_000),
            (8_000, -500_000),
            (192_000, 123),
        ] {
            let (y, r) = run(rate, ppb, 440.0, 0.1, 2.0);
            let n_in = 2 * rate as usize;
            let expect = n_in as f64 * 48_000.0 / f64::from(rate) / (1.0 + ppb as f64 * 1e-9);
            let lag = r.design().half_taps as f64 * 48_000.0 / f64::from(rate);
            let got = y.len() as f64;
            assert!(
                got <= expect + 1.0 && got >= expect - lag - 2.0,
                "{rate} {ppb}: {got} vs {expect}"
            );
            assert_eq!(r.input_consumed(), n_in as i64);
        }
    }

    /// Sines through 44.1 → 48 and 48 → 48 (±500 ppm): THD+N well below −100 dB.
    #[test]
    fn sine_thdn_is_below_minus_100_db() {
        for (rate, ppb, f) in [
            (44_100, 0, 997.0),
            (44_100, 0, 15_000.0),
            (44_100, 317_000, 997.0),
            (48_000, 500_000, 997.0),
            (48_000, -500_000, 19_000.0),
            (48_000, 1_234, 6_000.0),
            (96_000, 0, 997.0),
            (32_000, 0, 997.0),
        ] {
            let d = thdn_db(rate, ppb, f);
            assert!(
                d < -100.0,
                "{rate} Hz trim {ppb} ppb, {f} Hz: THD+N {d:.1} dB"
            );
        }
    }

    /// A trim change in the middle of a tone leaves no click: the residual around the change is
    /// no larger than elsewhere.
    #[test]
    fn trim_change_is_click_free() {
        let rate = InputRate::HZ_44100;
        let mut r = Resampler::new(rate, 1);
        let x = sine(1000.0, 0.5, 44_100, 44_100, 0.0);
        let mut y = vec![Vec::new()];
        r.process(&[&x[..22_050]], &mut y).unwrap();
        let (pos_i, pos_f) = r.next_output_position();
        let split = y[0].len();
        r.set_trim(Trim::new(500_000).unwrap());
        r.process(&[&x[22_050..]], &mut y).unwrap();
        // Reconstruct the expected tone from the exact positions of the outputs.
        let mut p = pos_i as f64 + pos_f as f64 / 2f64.powi(64);
        let step = 44_100.0 / 48_000.0 * 1.0005;
        let w = 2.0 * std::f64::consts::PI * 1000.0 / 44_100.0;
        let mut worst: f64 = 0.0;
        for &v in &y[0][split..split + 2000] {
            worst = worst.max((f64::from(v) - 0.5 * (w * p).sin()).abs());
            p += step;
        }
        assert!(worst < 1e-5, "{worst}");
    }

    /// The designed kernel: passband ripple and stopband rejection (continuous response).
    #[test]
    fn design_passband_and_stopband() {
        let d = Design::for_rate(InputRate::HZ_44100);
        assert_eq!(d.cutoff_hz(), 22_050.0);
        let ripple = (0..=20)
            .map(|i| d.response_db(f64::from(i) * 1000.0).abs())
            .fold(0.0, f64::max);
        assert!(ripple < 1e-4, "ripple {ripple} dB");
        let stop = [24_100.0, 25_000.0, 30_000.0, 44_100.0, 60_000.0]
            .iter()
            .map(|&f| d.response_db(f))
            .fold(f64::MIN, f64::max);
        assert!(stop < -115.0, "stopband {stop} dB");
    }
}
