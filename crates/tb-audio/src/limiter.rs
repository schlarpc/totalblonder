//! A true-peak limiter with look-ahead: keeps the output below −2 dBTP (A/85 §6 p.18, SPEC
//! §8.3).
//!
//! Per input sample the true peak of the interval it starts is estimated with the 4×
//! oversampler of the meter ([`crate::loudness::Oversampler`], BS.1770-4 Annex 2), the largest
//! over all channels; the gain that interval needs is `r = min(1, threshold / peak)`. The gain
//! curve is then made from `r` in three steps, all channels linked (the stereo image does not
//! move):
//!
//! 1. *hold*: the minimum of `r` over the last `L + 1` samples (`L` the look-ahead);
//! 2. *release*: drops are followed at once, rises with a first-order release (default 80 ms);
//! 3. *attack*: a moving average over `L` samples, so the gain glides down over the look-ahead
//!    instead of stepping.
//!
//! The audio is delayed by `L − 1` samples plus the oversampler's lag, so the sample whose
//! interval needed `r` is multiplied by the average of `L` values that are all at most `r`
//! (each of them is a minimum over a window that contains that `r`), as is the next sample.
//! The output can therefore not exceed the threshold at the oversampler's points, by
//! construction rather than by tuning. The threshold sits `guard` (default 0.2 dB) below the
//! ceiling to cover what a 4× estimate can miss between its points (at most 0.12 dB for a
//! 20 kHz sine, BS.1770-4 Annex 2 §2) and the gain's own variation within an interval.
//!
//! When the input never comes near the threshold the gain is exactly 1.0 and the output is the
//! input, delayed.
//!
//! Defaults \[choice\]: look-ahead 3 ms (144 samples): long enough that the gain reduction of a
//! transient is a 3 ms ramp, not a click; short enough to add little latency. Release 80 ms:
//! short enough that the limiter does not duck the program after a peak, long enough not to
//! modulate low frequencies (a 40 Hz period is 25 ms).

use crate::loudness::{Dbtp, Oversampler};
use std::collections::VecDeque;

/// Parameters of a [`Limiter`] (defaults in [`Default`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterConfig {
    /// The true-peak ceiling (default −2 dBTP, A/85 §6).
    pub ceiling: Dbtp,
    /// Margin below the ceiling for the estimate, dB (default 0.2).
    pub guard_db: f64,
    /// Look-ahead, samples at 48 kHz (default 144: 3 ms).
    pub lookahead: usize,
    /// Release time constant, seconds (default 0.08).
    pub release_s: f64,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            ceiling: Dbtp(-2.0),
            guard_db: 0.2,
            lookahead: 144,
            release_s: 0.08,
        }
    }
}

/// A [`LimiterConfig`] value out of range.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("limiter {field} = {value} is outside {lo}..={hi}")]
pub struct LimiterConfigError {
    /// The field.
    pub field: &'static str,
    /// Its value.
    pub value: f64,
    /// Lowest allowed.
    pub lo: f64,
    /// Highest allowed.
    pub hi: f64,
}

impl LimiterConfig {
    /// Check every field against its range.
    pub fn validate(&self) -> Result<(), LimiterConfigError> {
        let checks: [(&'static str, f64, f64, f64); 4] = [
            ("ceiling", self.ceiling.0, -20.0, 0.0),
            ("guard_db", self.guard_db, 0.0, 3.0),
            ("lookahead", self.lookahead as f64, 16.0, 4800.0),
            ("release_s", self.release_s, 0.001, 10.0),
        ];
        for (field, value, lo, hi) in checks {
            if !(value >= lo && value <= hi) {
                return Err(LimiterConfigError {
                    field,
                    value,
                    lo,
                    hi,
                });
            }
        }
        Ok(())
    }
}

/// Why [`Limiter::process`] refused its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LimiterError {
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

/// Re-sum the moving average from its ring this often, so rounding cannot accumulate.
const RESUM: u32 = 1 << 16;
/// Samples taken at a time by the fast path.
const FAST_CHUNK: usize = 64;

/// A linked true-peak limiter for a fixed number of channels at 48 kHz.
#[derive(Clone, Debug)]
pub struct Limiter {
    cfg: LimiterConfig,
    threshold: f32,
    release: f64,
    os: Vec<Oversampler>,
    /// Audio delay line per channel (`delay` samples).
    lines: Vec<VecDeque<f32>>,
    delay: usize,
    /// Monotonic queue for the running minimum: (sample index, r).
    minq: VecDeque<(u64, f32)>,
    idx: u64,
    /// The released gain, in `f64`: in `f32` a first-order rise toward 1.0 stalls a few
    /// hundred ppm short of it, where its step falls below half an ulp.
    env: f64,
    /// Moving average of `env` over `lookahead` samples.
    ring: VecDeque<f32>,
    sum: f64,
    since_resum: u32,
    /// Smallest gain applied so far (for reports).
    min_gain: f32,
    /// Peaks below `threshold / bound` (sample magnitude) cannot need limiting.
    bound: f32,
}

impl Limiter {
    /// A limiter for `channels` channels.
    pub fn new(channels: usize, cfg: LimiterConfig) -> Result<Self, LimiterConfigError> {
        cfg.validate()?;
        let channels = channels.max(1);
        let l = cfg.lookahead;
        let delay = l - 1 + Oversampler::LAG;
        let threshold = (cfg.ceiling.0 - cfg.guard_db).clamp(-200.0, 0.0);
        let mut s = Self {
            cfg,
            threshold: Dbtp(threshold).linear() as f32,
            release: 1.0 - (-1.0 / (cfg.release_s * 48_000.0)).exp(),
            os: vec![Oversampler::default(); channels],
            lines: vec![VecDeque::with_capacity(delay + 1); channels],
            delay,
            minq: VecDeque::with_capacity(l + 2),
            idx: 0,
            env: 1.0,
            ring: VecDeque::with_capacity(l + 1),
            sum: 0.0,
            since_resum: 0,
            min_gain: 1.0,
            bound: Oversampler::default().gain_bound() * 1.001,
        };
        s.reset();
        Ok(s)
    }

    /// Forget all history (the delay lines are silent again).
    pub fn reset(&mut self) {
        for o in &mut self.os {
            *o = Oversampler::default();
        }
        for l in &mut self.lines {
            l.clear();
            l.resize(self.delay, 0.0);
        }
        self.minq.clear();
        self.idx = 0;
        self.env = 1.0;
        self.ring.clear();
        self.ring.resize(self.cfg.lookahead, 1.0);
        self.sum = self.cfg.lookahead as f64;
        self.since_resum = 0;
    }

    /// The delay from input to output, samples.
    pub fn latency(&self) -> usize {
        self.delay
    }

    /// The smallest gain applied since construction (1.0: never limited).
    pub fn min_gain(&self) -> f32 {
        self.min_gain
    }

    /// No gain reduction is in progress or pending: the gain is exactly 1 and stays 1 while
    /// the input needs none.
    fn idle(&self) -> bool {
        self.env == 1.0
            && self.sum == self.cfg.lookahead as f64
            && self.minq.iter().all(|&(_, v)| v == 1.0)
    }

    /// No interval ending in `i..e` can exceed the threshold: every sample in the windows is
    /// below `threshold / bound`.
    fn quiet(&self, planes: &[Vec<f32>], i: usize, e: usize) -> bool {
        let limit = self.threshold / self.bound;
        self.os
            .iter()
            .zip(planes)
            .all(|(o, p)| o.window_max() < limit && p[i..e].iter().all(|v| v.abs() < limit))
    }

    /// The fast path: the gain is 1 throughout; only the histories and the delay advance.
    fn pass(&mut self, planes: &mut [Vec<f32>], i: usize, e: usize) {
        for ((o, line), p) in self
            .os
            .iter_mut()
            .zip(&mut self.lines)
            .zip(planes.iter_mut())
        {
            for v in &mut p[i..e] {
                o.feed(*v);
                line.push_back(*v);
                *v = line.pop_front().unwrap_or(0.0);
            }
        }
        self.idx += (e - i) as u64;
        self.minq.clear();
        self.minq.push_back((self.idx - 1, 1.0));
    }

    /// The full path for sample `i`.
    fn sample(&mut self, planes: &mut [Vec<f32>], i: usize) {
        let hold = self.cfg.lookahead as u64 + 1;
        let l = self.cfg.lookahead as f64;
        // 1. The true peak of the interval starting LAG samples ago, over all channels.
        let mut peak = 0.0f32;
        for (o, p) in self.os.iter_mut().zip(planes.iter()) {
            peak = peak.max(o.push(p[i]));
        }
        let r = if peak > self.threshold {
            self.threshold / peak
        } else {
            1.0
        };
        // 2. Hold: running minimum over the last `hold` values.
        while self.minq.back().is_some_and(|&(_, v)| v >= r) {
            self.minq.pop_back();
        }
        self.minq.push_back((self.idx, r));
        while self
            .minq
            .front()
            .is_some_and(|&(j, _)| j + hold <= self.idx)
        {
            self.minq.pop_front();
        }
        let h = self.minq.front().map_or(1.0, |&(_, v)| v);
        self.idx += 1;
        // 3. Release; the last millionth snaps to the target, so the gain returns to exactly 1.
        let h = f64::from(h);
        self.env = if h < self.env {
            h
        } else {
            let e = self.env + (h - self.env) * self.release;
            if h - e < 1e-6 { h } else { e }
        };
        // 4. Attack: moving average over the look-ahead.
        let old = self.ring.pop_front().unwrap_or(1.0);
        let env = self.env as f32;
        self.ring.push_back(env);
        self.sum += f64::from(env) - f64::from(old);
        self.since_resum += 1;
        if self.since_resum >= RESUM {
            self.sum = self.ring.iter().map(|&v| f64::from(v)).sum();
            self.since_resum = 0;
        }
        let g = (self.sum / l).min(1.0) as f32;
        self.min_gain = self.min_gain.min(g);
        // 5. Delay and apply.
        for (line, p) in self.lines.iter_mut().zip(planes.iter_mut()) {
            line.push_back(p[i]);
            let d = line.pop_front().unwrap_or(0.0);
            p[i] = if g == 1.0 { d } else { d * g };
        }
    }

    /// Limit `planes` in place (one per channel, equal lengths). The output is the input
    /// delayed by [`Limiter::latency`] samples, times the gain.
    pub fn process(&mut self, planes: &mut [Vec<f32>]) -> Result<(), LimiterError> {
        let ch = self.os.len();
        if planes.len() != ch {
            return Err(LimiterError::Planes {
                expected: ch,
                got: planes.len(),
            });
        }
        let n = planes.first().map_or(0, Vec::len);
        if planes.iter().any(|p| p.len() != n) {
            return Err(LimiterError::Ragged);
        }
        let mut i = 0;
        while i < n {
            let e = (i + FAST_CHUNK).min(n);
            if self.idle() && self.quiet(planes, i, e) {
                self.pass(planes, i, e);
            } else {
                for k in i..e {
                    self.sample(planes, k);
                }
            }
            i = e;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loudness::TruePeak;
    use crate::signal::{Rng, db_to_amp, program, sine};

    fn limit(x: &[Vec<f32>], cfg: LimiterConfig) -> (Vec<Vec<f32>>, Limiter) {
        let mut l = Limiter::new(x.len(), cfg).unwrap();
        let mut out = vec![Vec::new(); x.len()];
        let mut s = 0;
        let mut r = Rng(9);
        while s < x[0].len() {
            let e = (s + 1 + r.below(3000) as usize).min(x[0].len());
            let mut b: Vec<Vec<f32>> = x.iter().map(|p| p[s..e].to_vec()).collect();
            l.process(&mut b).unwrap();
            for (o, p) in out.iter_mut().zip(b) {
                o.extend(p);
            }
            s = e;
        }
        (out, l)
    }

    fn true_peak(x: &[Vec<f32>]) -> f64 {
        let mut t = TruePeak::new(x.len());
        let r: Vec<&[f32]> = x.iter().map(|p| p.as_slice()).collect();
        t.push(&r);
        t.max().0
    }

    #[test]
    fn config_is_validated() {
        let c = LimiterConfig {
            lookahead: 2,
            ..Default::default()
        };
        assert!(Limiter::new(2, c).is_err());
        let c = LimiterConfig {
            ceiling: Dbtp(f64::NAN),
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    /// Below the threshold the limiter is a pure delay.
    #[test]
    fn quiet_input_is_delayed_exactly() {
        let x = sine(1000.0, db_to_amp(-6.0), 48_000, 10_000, 0.3);
        let (y, l) = limit(std::slice::from_ref(&x), LimiterConfig::default());
        let d = l.latency();
        assert_eq!(y[0][d..], x[..x.len() - d]);
        assert_eq!(l.min_gain(), 1.0);
    }

    /// Loud material, program and hot tones up to +12 dBFS, comes out at or below −2 dBTP
    /// measured by the meter's true peak.
    #[test]
    fn loud_input_is_held_below_the_ceiling() {
        let mut p = program(5, 2, 48_000 * 6);
        for c in &mut p {
            for v in c.iter_mut() {
                *v *= 30.0; // about +10 dB over the generator's level: heavy limiting
            }
        }
        let tones = vec![
            sine(997.0, db_to_amp(12.0), 48_000, 48_000 * 2, 0.0),
            sine(19_000.0, db_to_amp(3.0), 48_000, 48_000 * 2, 1.0),
        ];
        for x in [p, tones] {
            let tp_in = true_peak(&x);
            let (y, l) = limit(&x, LimiterConfig::default());
            let tp = true_peak(&y);
            assert!(tp_in > 0.0, "{tp_in}");
            assert!(tp <= -2.0, "true peak {tp:.3} dBTP (input {tp_in:.1})");
            assert!(l.min_gain() < 0.5);
        }
    }

    /// An isolated transient is limited with a gain ramp across the look-ahead (no step), and
    /// the gain recovers with the release time.
    #[test]
    fn transient_gain_is_a_ramp_and_recovers() {
        let n = 96_000;
        let mut x = vec![0.0f32; n];
        for (i, v) in x.iter_mut().enumerate() {
            *v = 0.1 * (i as f32 * 0.05).sin();
        }
        for v in &mut x[20_000..20_010] {
            *v = 1.0;
        }
        let (y, l) = limit(&[x.clone()], LimiterConfig::default());
        let d = l.latency();
        let g: Vec<f32> = (0..n - d)
            .map(|i| {
                if x[i].abs() > 0.01 {
                    y[0][i + d] / x[i]
                } else {
                    f32::NAN
                }
            })
            .collect();
        let finite: Vec<(usize, f32)> = g
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, v)| v.is_finite())
            .collect();
        let steepest = finite
            .windows(2)
            .map(|w| (w[1].1 - w[0].1).abs() / (w[1].0 - w[0].0) as f32)
            .fold(0.0f32, f32::max);
        // Down by about 0.8 over 144 samples: no single step beyond a few times that slope.
        assert!(steepest < 0.03, "{steepest}");
        // Release 80 ms: 208 ms after the transient 2.6 time constants, 520 ms after 6.5.
        assert!(
            (g[30_000] - (1.0 - 0.31 * (-2.6f32).exp())).abs() < 0.01,
            "{}",
            g[30_000]
        );
        assert!(g[45_000] > 0.999, "{}", g[45_000]);
        // ... and then exactly 1 (1e-6 is reached after ln(0.31e6) = 12.6 time constants, 1 s):
        // the output is the input again, bit for bit.
        assert!((70_000..n - d).all(|i| y[0][i + d] == x[i]));
    }
}
