//! Test signals and their analysis, for this crate's tests, its measurement log and callers'
//! tests: tones, deterministic noise, program-like noise, a sine fit for THD+N.

use std::f64::consts::PI;

/// A deterministic generator (SplitMix64), so every run makes the same signals.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, 1)`.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
    /// Uniform in `0..n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    /// Standard normal (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.uniform().max(1e-300);
        let v = self.uniform();
        (-2.0 * u.ln()).sqrt() * (2.0 * PI * v).cos()
    }
}

/// `n` samples of `amp · sin(2π f t + phase)` at `rate` Hz.
pub fn sine(f: f64, amp: f64, rate: u32, n: usize, phase: f64) -> Vec<f32> {
    let w = 2.0 * PI * f / f64::from(rate);
    (0..n)
        .map(|i| (amp * (w * i as f64 + phase).sin()) as f32)
        .collect()
}

/// The amplitude of a sine at `-dbfs` dB relative to full scale (peak).
pub fn db_to_amp(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Least-squares fit of `a sin + b cos + c` at frequency `f` to `y`: returns the fitted
/// amplitude and the RMS of the residual (THD+N in absolute terms).
pub fn fit_sine(y: &[f32], f: f64, rate: f64) -> (f64, f64) {
    let w = 2.0 * PI * f / rate;
    // Normal equations of the 3×3 least-squares problem.
    let mut m = [[0.0f64; 3]; 3];
    let mut v = [0.0f64; 3];
    for (i, &s) in y.iter().enumerate() {
        let b = [(w * i as f64).sin(), (w * i as f64).cos(), 1.0];
        for r in 0..3 {
            for c in 0..3 {
                m[r][c] += b[r] * b[c];
            }
            v[r] += b[r] * f64::from(s);
        }
    }
    let x = solve3(m, v);
    let mut e = 0.0;
    for (i, &s) in y.iter().enumerate() {
        let fit = x[0] * (w * i as f64).sin() + x[1] * (w * i as f64).cos() + x[2];
        e += (f64::from(s) - fit).powi(2);
    }
    (
        (x[0] * x[0] + x[1] * x[1]).sqrt(),
        (e / y.len().max(1) as f64).sqrt(),
    )
}

fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> [f64; 3] {
    for c in 0..3 {
        let p = (c..3)
            .max_by(|&a, &b| m[a][c].abs().total_cmp(&m[b][c].abs()))
            .unwrap_or(c);
        m.swap(c, p);
        v.swap(c, p);
        let d = m[c][c];
        if d == 0.0 {
            continue;
        }
        for r in 0..3 {
            if r != c {
                let k = m[r][c] / d;
                let pivot = m[c];
                for (x, p) in m[r].iter_mut().zip(pivot) {
                    *x -= k * p;
                }
                v[r] -= k * v[c];
            }
        }
    }
    [
        v[0] / m[0][0].max(f64::MIN_POSITIVE),
        v[1] / m[1][1].max(f64::MIN_POSITIVE),
        v[2] / m[2][2].max(f64::MIN_POSITIVE),
    ]
}

/// Program-like noise at 48 kHz: pink-ish noise (a 1/f approximation by Kellet's filter)
/// shaped by a syllabic envelope (about 4 Hz, with pauses of 0.2–1 s every few seconds), so
/// that the BS.1770 gate and a slow normalizer see something like speech and music, not a
/// stationary tone. Returns `n` samples per channel for `channels` channels (independent
/// noise, common envelope), RMS roughly −20 dBFS before scaling.
pub fn program(seed: u64, channels: usize, n: usize) -> Vec<Vec<f32>> {
    let mut rng = Rng(seed);
    let mut out = vec![Vec::with_capacity(n); channels];
    let mut state = vec![[0.0f64; 7]; channels];
    let mut pause_left = 0usize;
    let mut next_pause = 48_000 * 3;
    let mut phase = 0.0f64;
    for i in 0..n {
        if i == next_pause {
            pause_left = 9_600 + rng.below(38_400) as usize;
            next_pause = i + pause_left + 48_000 * (2 + rng.below(4) as usize);
        }
        phase += 2.0 * PI * (3.0 + 2.0 * (i as f64 / 480_000.0).sin().abs()) / 48_000.0;
        let env = if pause_left > 0 {
            pause_left -= 1;
            0.01
        } else {
            0.35 + 0.65 * (0.5 + 0.5 * phase.sin()).powi(2)
        };
        for (c, o) in out.iter_mut().enumerate() {
            let w = rng.normal();
            let b = &mut state[c];
            // Paul Kellet's pink-noise filter (economy coefficients extended).
            b[0] = 0.99886 * b[0] + w * 0.0555179;
            b[1] = 0.99332 * b[1] + w * 0.0750759;
            b[2] = 0.96900 * b[2] + w * 0.1538520;
            b[3] = 0.86650 * b[3] + w * 0.3104856;
            b[4] = 0.55000 * b[4] + w * 0.5329522;
            b[5] = -0.7616 * b[5] - w * 0.0168980;
            let p = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
            b[6] = w * 0.115926;
            o.push((p * env * 0.035) as f32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_fit_recovers_amplitude_and_noise() {
        let mut y = sine(1000.0, 0.25, 48_000, 48_000, 1.0);
        let (a, r) = fit_sine(&y, 1000.0, 48_000.0);
        assert!((a - 0.25).abs() < 1e-7);
        assert!(r < 1e-7);
        let mut g = Rng(1);
        for s in &mut y {
            *s += (g.normal() * 1e-3) as f32;
        }
        let (_, r) = fit_sine(&y, 1000.0, 48_000.0);
        assert!((r - 1e-3).abs() < 2e-5, "{r}");
    }

    #[test]
    fn rng_is_deterministic_and_uniform() {
        let mut a = Rng(7);
        let mut b = Rng(7);
        let mut mean = 0.0;
        for _ in 0..10_000 {
            let u = a.uniform();
            assert_eq!(u, b.uniform());
            assert!((0.0..1.0).contains(&u));
            mean += u;
        }
        assert!((mean / 10_000.0 - 0.5).abs() < 0.01);
    }
}
