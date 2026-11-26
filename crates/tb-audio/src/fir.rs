//! Kaiser-windowed sinc design, shared by the resampler and the true-peak oversampler.
//!
//! The Kaiser formulas are the classical ones (Kaiser 1974; Oppenheim & Schafer, *Discrete-Time
//! Signal Processing*, 3rd ed., §7.6.1, eqs. 7.62–7.63): for a stopband attenuation `A` dB and
//! a transition width `Δω` rad/sample, `β = 0.1102 (A − 8.7)` for `A > 50`, and the length is
//! `N = (A − 7.95) / (2.285 Δω)` taps.

use std::f64::consts::PI;

/// Kaiser's `β` for a stopband attenuation of `atten_db`.
pub(crate) fn kaiser_beta(atten_db: f64) -> f64 {
    if atten_db > 50.0 {
        0.1102 * (atten_db - 8.7)
    } else if atten_db >= 21.0 {
        0.5842 * (atten_db - 21.0).powf(0.4) + 0.07886 * (atten_db - 21.0)
    } else {
        0.0
    }
}

/// Kaiser's estimate of the filter length (taps) for `atten_db` and a transition width of
/// `transition` cycles per sample.
pub(crate) fn kaiser_length(atten_db: f64, transition: f64) -> f64 {
    (atten_db - 7.95) / (2.285 * 2.0 * PI * transition)
}

/// The zeroth-order modified Bessel function of the first kind, by its power series (converges
/// for every argument used here, `x < 40`, to the last bit within ~60 terms).
pub(crate) fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// A Kaiser-windowed sinc low-pass kernel on a continuous argument.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Kernel {
    /// Cutoff as a fraction of the sampling rate of the argument, times two (1.0: Nyquist).
    pub scale: f64,
    /// Half-width: the kernel is zero for `|u| >= half`.
    pub half: f64,
    pub beta: f64,
    pub i0_beta: f64,
}

impl Kernel {
    pub(crate) fn new(scale: f64, half: f64, beta: f64) -> Self {
        Self {
            scale,
            half,
            beta,
            i0_beta: bessel_i0(beta),
        }
    }

    /// `scale · sinc(scale · u) · w(u / half)`. Exactly zero outside `(−half, half)`, and
    /// exactly zero where `scale · u` is a non-zero integer (the zero crossings of the sinc), so
    /// a Nyquist-cutoff kernel at integer `u` is an exact unit impulse.
    pub(crate) fn at(&self, u: f64) -> f64 {
        if u.abs() >= self.half {
            return 0.0;
        }
        let x = self.scale * u;
        let sinc = if x == 0.0 {
            1.0
        } else if x.fract() == 0.0 {
            0.0
        } else {
            (PI * x).sin() / (PI * x)
        };
        let r = u / self.half;
        let w = bessel_i0(self.beta * (1.0 - r * r).max(0.0).sqrt()) / self.i0_beta;
        self.scale * sinc * w
    }
}

/// Magnitude in dB of the frequency response of a set of taps at `f` cycles per sample.
#[cfg(test)]
pub(crate) fn response_db(taps: &[f64], f: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (k, &h) in taps.iter().enumerate() {
        let w = -2.0 * PI * f * k as f64;
        re += h * w.cos();
        im += h * w.sin();
    }
    10.0 * (re * re + im * im).max(1e-300).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bessel_i0_matches_tabulated_values() {
        // Abramowitz & Stegun Table 9.8: I0(1) = 1.266065878, I0(5) = 27.23987182.
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-12);
        assert!((bessel_i0(5.0) - 27.239_871_823_604_45).abs() < 1e-9);
    }

    #[test]
    fn nyquist_kernel_is_an_impulse_at_integers() {
        let k = Kernel::new(1.0, 20.0, kaiser_beta(120.0));
        assert_eq!(k.at(0.0), 1.0);
        for n in 1..25 {
            assert_eq!(k.at(f64::from(n)), 0.0);
            assert_eq!(k.at(-f64::from(n)), 0.0);
        }
    }

    #[test]
    fn kaiser_design_meets_its_attenuation() {
        // A half-band low-pass, 0.2 to 0.3 cycles/sample, 100 dB.
        let a = 100.0;
        let n = kaiser_length(a, 0.1).ceil() as usize + 1;
        let k = Kernel::new(0.5, n as f64 / 2.0 + 0.5, kaiser_beta(a));
        let c = (n as f64 - 1.0) / 2.0;
        let taps: Vec<f64> = (0..n).map(|i| k.at(i as f64 - c)).collect();
        let worst_stop = (300..=500)
            .map(|i| response_db(&taps, f64::from(i) / 1000.0))
            .fold(f64::MIN, f64::max);
        let worst_pass = (0..=200)
            .map(|i| response_db(&taps, f64::from(i) / 1000.0).abs())
            .fold(0.0, f64::max);
        // Kaiser's length formula is an estimate, good to a couple of dB.
        assert!(worst_stop < -95.0, "{worst_stop}");
        assert!(worst_pass < 1e-3, "{worst_pass}");
    }
}
