//! Test signals and measurements shared by the integration tests.
#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test helpers fail the test"
)]

use tb_ac3::parse;
use tb_ac3::{Config, Encoder, FRAME_SAMPLES};

/// 48 kHz.
pub const FS: f64 = 48_000.0;

/// A deterministic generator (xorshift32), so every run measures the same signal.
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed.max(1))
    }

    /// Uniform in [−1, 1).
    pub fn uniform(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0
    }
}

/// The test signals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Signal {
    Silence,
    /// Frequency in Hz, peak level in dBFS.
    Sine(f64, f64),
    /// Logarithmic sweep 20 Hz to 20 kHz over the signal, peak level dBFS.
    Sweep(f64),
    /// White noise, RMS dBFS.
    White(f64),
    /// Pink noise (Voss–McCartney, 16 rows), RMS dBFS.
    Pink(f64),
    /// Decaying 3 kHz bursts every 100 ms over a quiet tone: a transient test.
    Clicks,
}

impl Signal {
    pub fn name(&self) -> String {
        match self {
            Signal::Silence => "silence".into(),
            Signal::Sine(f, l) => format!("sine {f:.0} Hz {l:.0} dBFS"),
            Signal::Sweep(l) => format!("sweep 20 Hz–20 kHz {l:.0} dBFS"),
            Signal::White(l) => format!("white noise {l:.0} dBFS rms"),
            Signal::Pink(l) => format!("pink noise {l:.0} dBFS rms"),
            Signal::Clicks => "clicks (bursts every 100 ms)".into(),
        }
    }

    /// `len` samples of channel `ch` (channels differ: noise seeds, tone phase).
    pub fn generate(&self, len: usize, ch: usize) -> Vec<f32> {
        let db = |l: f64| 10f64.powf(l / 20.0);
        let mut rng = Rng::new(0x9e37_79b9 ^ (ch as u32 + 1).wrapping_mul(0x85eb_ca6b));
        let two_pi = 2.0 * std::f64::consts::PI;
        match *self {
            Signal::Silence => vec![0.0; len],
            Signal::Sine(f, l) => (0..len)
                .map(|n| (db(l) * (two_pi * f * n as f64 / FS + ch as f64 * 0.7).sin()) as f32)
                .collect(),
            Signal::Sweep(l) => {
                let (f0, f1) = (20.0f64, 20_000.0f64);
                let t1 = len as f64 / FS;
                let k = (f1 / f0).ln();
                (0..len)
                    .map(|n| {
                        let t = n as f64 / FS;
                        let phase = two_pi * f0 * t1 / k * ((t / t1 * k).exp() - 1.0);
                        (db(l) * (phase + ch as f64 * 0.3).sin()) as f32
                    })
                    .collect()
            }
            Signal::White(l) => {
                // Uniform noise has rms 1/√3 of its peak.
                let a = db(l) * 3f64.sqrt();
                (0..len).map(|_| (a * rng.uniform()) as f32).collect()
            }
            Signal::Pink(l) => {
                let mut rows = [0.0f64; 16];
                let mut sum = 0.0;
                let raw: Vec<f64> = (0..len)
                    .map(|n| {
                        let i = (n + 1).trailing_zeros().min(15) as usize;
                        sum -= rows[i];
                        rows[i] = rng.uniform();
                        sum += rows[i];
                        sum + rng.uniform()
                    })
                    .collect();
                let rms = (raw.iter().map(|v| v * v).sum::<f64>() / len as f64).sqrt();
                raw.iter().map(|v| (v / rms * db(l)) as f32).collect()
            }
            Signal::Clicks => (0..len)
                .map(|n| {
                    let period = 4800;
                    let k = n % period;
                    let burst = if k < 960 {
                        0.8 * (-(k as f64) / 120.0).exp() * (two_pi * 3000.0 * k as f64 / FS).sin()
                    } else {
                        0.0
                    };
                    let bed = 0.01 * (two_pi * 440.0 * n as f64 / FS).sin();
                    (burst + bed) as f32
                })
                .collect(),
        }
    }
}

/// Encodes `input` (one Vec per channel, A/52 order) frame by frame, padding the end with
/// zeros; checks every frame with the strict parser; returns the concatenated stream.
pub fn encode_all(cfg: Config, input: &[Vec<f32>]) -> Vec<u8> {
    let mut enc = Encoder::new(cfg);
    let len = input[0].len();
    let frames = len.div_ceil(FRAME_SAMPLES) + 1; // one more to flush the overlap
    let mut out = Vec::with_capacity(frames * cfg.frame_bytes());
    let mut chunk = vec![vec![0.0f32; FRAME_SAMPLES]; input.len()];
    for f in 0..frames {
        for (c, ch) in chunk.iter_mut().enumerate() {
            for (i, s) in ch.iter_mut().enumerate() {
                *s = input[c].get(f * FRAME_SAMPLES + i).copied().unwrap_or(0.0);
            }
        }
        let refs: Vec<&[f32]> = chunk.iter().map(|c| c.as_slice()).collect();
        let frame = enc.encode_frame(&refs).expect("shape");
        assert_eq!(frame.len(), cfg.frame_bytes());
        if let Err(e) = parse::parse_frame(frame) {
            panic!("frame {f} rejected by the parser: {e}");
        }
        out.extend_from_slice(frame);
    }
    out
}

/// SNR in dB of `decoded` against `reference` over `reference`'s length, `decoded` delayed by
/// `delay` samples. Infinite when the error is zero.
pub fn snr_db(reference: &[f32], decoded: &[f32], delay: usize) -> f64 {
    let mut sig = 0.0f64;
    let mut err = 0.0f64;
    for (n, &r) in reference.iter().enumerate() {
        let d = decoded.get(n + delay).copied().unwrap_or(0.0);
        sig += f64::from(r) * f64::from(r);
        err += (f64::from(r) - f64::from(d)).powi(2);
    }
    if err == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / err).log10()
    }
}

/// SNR in dB after low-pass filtering both signals at `cutoff` Hz (a 511-tap windowed-sinc
/// FIR), which measures the coded band only.
pub fn snr_db_lowpass(reference: &[f32], decoded: &[f32], delay: usize, cutoff: f64) -> f64 {
    let taps = 511usize;
    let h: Vec<f64> = (0..taps)
        .map(|i| {
            let m = i as f64 - (taps / 2) as f64;
            let wc = 2.0 * std::f64::consts::PI * cutoff / FS;
            let sinc = if m == 0.0 {
                wc / std::f64::consts::PI
            } else {
                (wc * m).sin() / (std::f64::consts::PI * m)
            };
            let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (taps - 1) as f64).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (taps - 1) as f64).cos();
            sinc * w
        })
        .collect();
    let filt = |x: &[f32], off: usize, len: usize| -> Vec<f64> {
        (0..len)
            .map(|n| {
                let mut acc = 0.0;
                for (k, hk) in h.iter().enumerate() {
                    if let Some(v) = (n + off + k).checked_sub(taps / 2).and_then(|i| x.get(i)) {
                        acc += hk * f64::from(*v);
                    }
                }
                acc
            })
            .collect()
    };
    let len = reference.len();
    let r = filt(reference, 0, len);
    let d = filt(decoded, delay, len);
    let sig: f64 = r.iter().map(|v| v * v).sum();
    let err: f64 = r.iter().zip(&d).map(|(a, b)| (a - b).powi(2)).sum();
    if err == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / err).log10()
    }
}
