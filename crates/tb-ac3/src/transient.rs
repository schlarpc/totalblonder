//! Transient detection: when a channel switches to two short transforms (A/52 §8.2.2,
//! p.109–110). Encoder-only; the standard describes it as guidance.
//!
//! Per channel and audio block, the 256 new samples are high-pass filtered, and the peak of the
//! magnitude is taken over a tree of segments: level 1 is the 256 samples, level 2 two segments
//! of 128, level 3 four of 64. A transient is flagged when, on any level, a segment's peak
//! times that level's threshold `T[j]` exceeds the previous segment's peak (the previous segment
//! of the first one being the last segment of the previous block's tree). Blocks whose overall
//! peak is under the silence threshold 100/32768 are never flagged.
//!
//! The standard's filter is "a cascaded biquad direct form I IIR filter with a cutoff of 8 kHz"
//! without coefficients. Choice made here: a fourth-order Butterworth high-pass (two biquads,
//! Q = 0.5412 and 1.3066) by the bilinear transform at 48 kHz.

/// Silence threshold on the level-1 peak (A/52 §8.2.2 step 4, p.110).
const SILENCE: f32 = 100.0 / 32768.0;

/// Thresholds `T[1]`, `T[2]`, `T[3]` (A/52 §8.2.2 step 4, p.110).
const THRESHOLD: [f32; 3] = [0.1, 0.075, 0.05];

/// Cutoff of the high-pass filter (A/52 §8.2.2 step 1, p.109).
const CUTOFF_HZ: f64 = 8000.0;

#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    /// Butterworth-section high-pass by the bilinear transform (RBJ cookbook form).
    fn high_pass(fs: f64, f0: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * f0 / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + cos) / 2.0 / a0;
        let b1 = -(1.0 + cos) / a0;
        let a1 = -2.0 * cos / a0;
        let a2 = (1.0 - alpha) / a0;
        Self {
            b: [b0 as f32, b1 as f32, b0 as f32],
            a: [a1 as f32, a2 as f32],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    /// Direct form I.
    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        // Flush denormals so a long silence does not slow the filter down.
        let y = if y.abs() < 1e-30 { 0.0 } else { y };
        self.y = [y, self.y[0]];
        y
    }
}

/// The detector state of one channel.
#[derive(Debug, Clone)]
pub struct TransientDetector {
    filter: [Biquad; 2],
    /// Peak of the last segment of each level of the previous tree: `P[j][0]`.
    last_peak: [f32; 3],
}

impl Default for TransientDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl TransientDetector {
    /// A detector with a quiet history, for 48 kHz input.
    pub fn new() -> Self {
        let fs = 48_000.0;
        Self {
            filter: [
                Biquad::high_pass(fs, CUTOFF_HZ, 0.541_196_100_146_197),
                Biquad::high_pass(fs, CUTOFF_HZ, 1.306_562_964_876_376_5),
            ],
            last_peak: [0.0; 3],
        }
    }

    /// Feeds the 256 new samples of a block; returns whether the block should use short
    /// transforms (`blksw` = 1).
    pub fn detect(&mut self, new: &[f32; 256]) -> bool {
        // Level 3 peaks of four 64-sample segments; levels 2 and 1 follow from them.
        let mut p3 = [0.0f32; 4];
        for (seg, peak) in p3.iter_mut().enumerate() {
            let mut m = 0.0f32;
            for &x in &new[seg * 64..seg * 64 + 64] {
                let first = self.filter[0].step(x);
                let y = self.filter[1].step(first);
                m = m.max(y.abs());
            }
            *peak = m;
        }
        let p2 = [p3[0].max(p3[1]), p3[2].max(p3[3])];
        let p1 = p2[0].max(p2[1]);

        let mut transient = false;
        if p1 >= SILENCE {
            let check = |cur: &[f32], prev_last: f32, t: f32| -> bool {
                let mut prev = prev_last;
                for &p in cur {
                    if p * t > prev {
                        return true;
                    }
                    prev = p;
                }
                false
            };
            transient = check(&[p1], self.last_peak[0], THRESHOLD[0])
                || check(&p2, self.last_peak[1], THRESHOLD[1])
                || check(&p3, self.last_peak[2], THRESHOLD[2]);
        }
        self.last_peak = [p1, p2[1], p3[3]];
        transient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(f: impl Fn(usize) -> f32) -> [f32; 256] {
        std::array::from_fn(f)
    }

    #[test]
    fn steady_tone_is_not_a_transient() {
        let mut d = TransientDetector::new();
        let mut flagged = 0;
        for b in 0..40 {
            let x = block(|i| {
                let t = (b * 256 + i) as f32 / 48_000.0;
                0.5 * (2.0 * std::f32::consts::PI * 10_000.0 * t).sin()
            });
            if d.detect(&x) && b > 1 {
                flagged += 1;
            }
        }
        assert_eq!(flagged, 0);
    }

    #[test]
    fn click_after_silence_is_a_transient() {
        let mut d = TransientDetector::new();
        for _ in 0..4 {
            assert!(!d.detect(&[0.0; 256]));
        }
        let x = block(|i| if i == 200 { 0.8 } else { 0.0 });
        assert!(d.detect(&x));
    }

    #[test]
    fn quiet_click_under_silence_threshold_is_ignored() {
        let mut d = TransientDetector::new();
        let x = block(|i| if i == 100 { 50.0 / 32768.0 } else { 0.0 });
        assert!(!d.detect(&x));
    }

    #[test]
    fn low_frequency_step_is_filtered_out() {
        // A slow onset has no energy above 8 kHz worth flagging.
        let mut d = TransientDetector::new();
        assert!(!d.detect(&[0.0; 256]));
        let x = block(|i| 0.5 * (2.0 * std::f32::consts::PI * 100.0 * i as f32 / 48_000.0).sin());
        assert!(!d.detect(&x));
    }
}
