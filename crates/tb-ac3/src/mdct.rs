//! The forward transform: window and MDCT, long (512) or two short (256) blocks.
//!
//! Definition (A/52 §8.2.3.2, p.110), for the windowed sequence `x[n]`, `0 ≤ n < N`:
//!
//! ```text
//! XD[k] = −2/N · Σ x[n] · cos( 2π/(4N)·(2n+1)(2k+1) + π/4·(2k+1)(1+α) ),   0 ≤ k < N/2
//! ```
//!
//! with `α = 0` for the long transform (N = 512) and `α = −1` / `+1` for the first / second
//! short transform (N = 256 each, on the first / second half of the windowed 512 samples). The
//! two short transforms' coefficients are interleaved, first transform in the even bins
//! (A/52 §7.9.2, p.97; the decoder's de-interleave in §7.9.4.2 step 1, p.100).
//!
//! The window is the 512-point symmetric window of Table 7.33 (p.102), used back to back
//! (§8.2.3.1, p.110). The table prints 256 values to five decimals; they are the
//! Kaiser–Bessel-derived window with α = 5, which is computed here in double precision
//! (`window_matches_table_7_33` pins it to the table).
//!
//! Computation: the phase term for α = ±1 is a circular shift by ∓N/4 of the anti-periodic
//! input; the result is folded to N/2 points and transformed as a DCT-IV of N/2 points through a
//! complex FFT of N/4 points. `forward_matches_direct_sum` checks it against the O(N²)
//! definition above.

use crate::tables::N_COEFS;

/// Window length (A/52 §7.9.4, p.98: N = 512).
pub const N_LONG: usize = 512;

/// The first half of the A/52 window, `w[0..256]` (A/52 Table 7.33, p.102), as the
/// Kaiser–Bessel-derived window with α = 5: `w[n] = sqrt(Σ_{j≤n} K[j] / Σ_{j≤256} K[j])`,
/// `K[j] = I0(π·α·sqrt(1 − (j/128 − 1)²))`.
pub fn kbd_window_half() -> [f64; 256] {
    fn bessel_i0(x: f64) -> f64 {
        // Power series; converges quickly for the arguments used here (x ≤ 5π).
        let mut sum = 1.0;
        let mut term = 1.0;
        let q = x * x / 4.0;
        for k in 1..100 {
            term *= q / f64::from(k * k);
            sum += term;
            if term < sum * 1e-17 {
                break;
            }
        }
        sum
    }
    let alpha = 5.0f64;
    let mut kernel = [0.0f64; 257];
    for (j, k) in kernel.iter_mut().enumerate() {
        let t = j as f64 / 128.0 - 1.0;
        *k = bessel_i0(std::f64::consts::PI * alpha * (1.0 - t * t).max(0.0).sqrt());
    }
    let total: f64 = kernel.iter().sum();
    let mut out = [0.0f64; 256];
    let mut acc = 0.0;
    for (n, o) in out.iter_mut().enumerate() {
        acc += kernel[n];
        *o = (acc / total).sqrt();
    }
    out
}

/// An in-place radix-2 complex FFT of fixed power-of-two size, `X[k] = Σ x[n]·e^(−2πi·nk/n)`.
#[derive(Debug, Clone)]
struct Fft {
    n: usize,
    /// Twiddles of every stage, stage after stage: for the stage of length `len`, the `len/2`
    /// values `e^(−2πi·k/len)`. Stored contiguously so a stage reads them in order.
    twiddle_re: Vec<f32>,
    twiddle_im: Vec<f32>,
    /// Bit-reversal swaps (i, j) with i < j.
    swaps: Vec<(u16, u16)>,
}

impl Fft {
    fn new(n: usize) -> Self {
        debug_assert!(n.is_power_of_two() && (2..=1 << 15).contains(&n));
        let bits = n.trailing_zeros();
        let mut twiddle_re = Vec::with_capacity(n);
        let mut twiddle_im = Vec::with_capacity(n);
        let mut len = 2;
        while len <= n {
            for k in 0..len / 2 {
                let a = -2.0 * std::f64::consts::PI * k as f64 / len as f64;
                twiddle_re.push(a.cos() as f32);
                twiddle_im.push(a.sin() as f32);
            }
            len *= 2;
        }
        let swaps = (0..n)
            .filter_map(|i| {
                let j = i.reverse_bits() >> (usize::BITS - bits);
                (j > i).then_some((i as u16, j as u16))
            })
            .collect();
        Self {
            n,
            twiddle_re,
            twiddle_im,
            swaps,
        }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        let (re, im) = (&mut re[..n], &mut im[..n]);
        for &(i, j) in &self.swaps {
            re.swap(usize::from(i), usize::from(j));
            im.swap(usize::from(i), usize::from(j));
        }
        let mut len = 2;
        let mut tw = 0;
        while len <= n {
            let half = len / 2;
            let (wre, wim) = (
                &self.twiddle_re[tw..tw + half],
                &self.twiddle_im[tw..tw + half],
            );
            for (cre, cim) in re.chunks_exact_mut(len).zip(im.chunks_exact_mut(len)) {
                let (are, bre) = cre.split_at_mut(half);
                let (aim, bim) = cim.split_at_mut(half);
                for k in 0..half {
                    let (wr, wi) = (wre[k], wim[k]);
                    let tr = bre[k] * wr - bim[k] * wi;
                    let ti = bre[k] * wi + bim[k] * wr;
                    bre[k] = are[k] - tr;
                    bim[k] = aim[k] - ti;
                    are[k] += tr;
                    aim[k] += ti;
                }
            }
            tw += half;
            len *= 2;
        }
    }
}

/// A DCT-IV of `m` points through a complex FFT of `m/2` points:
/// `X[k] = Σ v[n]·cos(π/m·(n+½)(k+½))`.
#[derive(Debug, Clone)]
struct Dct4 {
    m: usize,
    fft: Fft,
    /// `e^(−iπ(n+¼)/m)`, pre-twiddle.
    pre: Vec<(f32, f32)>,
    /// `e^(−iπk/m)`, post-twiddle.
    post: Vec<(f32, f32)>,
}

impl Dct4 {
    fn new(m: usize) -> Self {
        let pi = std::f64::consts::PI;
        let pre = (0..m / 2)
            .map(|n| {
                let a = -pi * (n as f64 + 0.25) / m as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let post = (0..m / 2)
            .map(|k| {
                let a = -pi * k as f64 / m as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self {
            m,
            fft: Fft::new(m / 2),
            pre,
            post,
        }
    }

    /// `v` has `m` points; `out` receives `m` points scaled by `scale`.
    fn run(&self, v: &[f32], out: &mut [f32], scale: f32, re: &mut [f32], im: &mut [f32]) {
        let m = self.m;
        let h = m / 2;
        for n in 0..h {
            let (a, b) = (v[2 * n], v[m - 1 - 2 * n]);
            let (c, s) = self.pre[n];
            re[n] = a * c - b * s;
            im[n] = a * s + b * c;
        }
        self.fft.run(&mut re[..h], &mut im[..h]);
        for k in 0..h {
            let (c, s) = self.post[k];
            let yr = re[k] * c - im[k] * s;
            let yi = re[k] * s + im[k] * c;
            out[2 * k] = yr * scale;
            out[m - 1 - 2 * k] = -yi * scale;
        }
    }
}

/// Phase of a transform: `α` of A/52 §8.2.3.2 (p.110).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// α = −1: the first of two short transforms.
    FirstShort,
    /// α = 0: the long transform.
    Long,
    /// α = +1: the second of two short transforms.
    SecondShort,
}

/// The forward transform with its tables, built once.
#[derive(Debug, Clone)]
pub struct Mdct {
    /// The full 512-point window, `w[n]` and `w[511 − n]`.
    window: [f32; N_LONG],
    long: Dct4,
    short: Dct4,
}

/// Working storage for [`Mdct::forward`], so that a transform allocates nothing.
#[derive(Debug, Clone)]
pub struct Scratch {
    windowed: [f32; N_LONG],
    work: Work,
}

/// The transform's own buffers, apart from the windowed input.
#[derive(Debug, Clone)]
struct Work {
    folded: [f32; 256],
    re: [f32; 128],
    im: [f32; 128],
}

impl Default for Scratch {
    fn default() -> Self {
        Self {
            windowed: [0.0; N_LONG],
            work: Work {
                folded: [0.0; 256],
                re: [0.0; 128],
                im: [0.0; 128],
            },
        }
    }
}

impl Default for Mdct {
    fn default() -> Self {
        Self::new()
    }
}

impl Mdct {
    /// Builds the window and the twiddle tables.
    pub fn new() -> Self {
        let half = kbd_window_half();
        let mut window = [0.0f32; N_LONG];
        for n in 0..256 {
            window[n] = half[n] as f32;
            window[N_LONG - 1 - n] = half[n] as f32;
        }
        Self {
            window,
            long: Dct4::new(256),
            short: Dct4::new(128),
        }
    }

    /// The window, `w[0..512]`.
    pub fn window(&self) -> &[f32; N_LONG] {
        &self.window
    }

    /// One transform of `x` (already windowed, `x.len()` = N ∈ {256, 512}) into `out`
    /// (N/2 coefficients), as defined in A/52 §8.2.3.2. The phase's circular shift and the fold
    /// (a, b, c, d) → (−c_r − d, a − b_r) are done together, one loop per phase.
    fn transform(&self, x: &[f32], phase: Phase, out: &mut [f32], s: &mut Work) {
        let n = x.len();
        let q = n / 4;
        let (lo, hi) = s.folded[..n / 2].split_at_mut(q);
        match phase {
            // v[i] = −x[3q−1−i] − x[3q+i]; v[q+i] = x[i] − x[2q−1−i].
            Phase::Long => {
                for i in 0..q {
                    lo[i] = -x[3 * q - 1 - i] - x[3 * q + i];
                    hi[i] = x[i] - x[2 * q - 1 - i];
                }
            }
            // α = −1, x'[j] = x[j + q] (j < 3q), −x[j − 3q] (j ≥ 3q):
            // v[i] = x[i] − x[n−1−i]; v[q+i] = x[q+i] − x[3q−1−i].
            Phase::FirstShort => {
                for i in 0..q {
                    lo[i] = x[i] - x[n - 1 - i];
                    hi[i] = x[q + i] - x[3 * q - 1 - i];
                }
            }
            // α = +1, x'[j] = −x[j + 3q] (j < q), x[j − q] (j ≥ q):
            // v[i] = −x[2q−1−i] − x[2q+i]; v[q+i] = −x[3q+i] − x[q−1−i].
            Phase::SecondShort => {
                for i in 0..q {
                    lo[i] = -x[2 * q - 1 - i] - x[2 * q + i];
                    hi[i] = -x[3 * q + i] - x[q - 1 - i];
                }
            }
        }
        let dct = if n == N_LONG { &self.long } else { &self.short };
        let scale = -2.0 / n as f32;
        dct.run(&s.folded[..n / 2], out, scale, &mut s.re, &mut s.im);
    }

    /// Windows the 512 input samples of one block (256 previous, 256 new) and transforms them:
    /// one long transform, or two short ones interleaved when `short` is set.
    pub fn forward(
        &self,
        input: &[f32; N_LONG],
        short: bool,
        out: &mut [f32; N_COEFS],
        s: &mut Scratch,
    ) {
        for ((o, x), w) in s
            .windowed
            .iter_mut()
            .zip(input.iter())
            .zip(self.window.iter())
        {
            *o = x * w;
        }
        let (windowed, work) = (&s.windowed, &mut s.work);
        if short {
            let mut first = [0.0f32; 128];
            let mut second = [0.0f32; 128];
            self.transform(&windowed[..256], Phase::FirstShort, &mut first, work);
            self.transform(&windowed[256..], Phase::SecondShort, &mut second, work);
            for (pair, (a, b)) in out
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(first.iter().zip(&second))
            {
                pair[0] = *a;
                pair[1] = *b;
            }
        } else {
            self.transform(windowed, Phase::Long, out, work);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The O(N²) definition of A/52 §8.2.3.2 in double precision.
    pub(crate) fn direct(x: &[f64], phase: Phase) -> Vec<f64> {
        let n = x.len();
        let alpha = match phase {
            Phase::FirstShort => -1.0,
            Phase::Long => 0.0,
            Phase::SecondShort => 1.0,
        };
        let pi = std::f64::consts::PI;
        (0..n / 2)
            .map(|k| {
                let k = k as f64;
                let s: f64 = x
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| {
                        let i = i as f64;
                        v * (2.0 * pi / (4.0 * n as f64) * (2.0 * i + 1.0) * (2.0 * k + 1.0)
                            + pi / 4.0 * (2.0 * k + 1.0) * (1.0 + alpha))
                            .cos()
                    })
                    .sum();
                -2.0 / n as f64 * s
            })
            .collect()
    }

    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn window_matches_table_7_33() {
        // The table prints five decimals; the KBD α = 5 window must round to it.
        let table: [f64; 8] = [
            0.00014, 0.00024, 0.00037, 0.00051, 0.00067, 0.00086, 0.00107, 0.0013,
        ];
        let w = kbd_window_half();
        for (i, t) in table.iter().enumerate() {
            assert!((w[i] - t).abs() <= 0.5e-5, "w[{i}] = {} vs {t}", w[i]);
        }
        // Princen–Bradley: w[n]² + w[255 − n]²... for the symmetric 512 window,
        // w[n]² + w[n + 256]² = 1, i.e. w[n]² + w[255 − n]² = 1.
        for n in 0..256 {
            let s = w[n] * w[n] + w[255 - n] * w[255 - n];
            assert!((s - 1.0).abs() < 1e-12, "n {n}: {s}");
        }
    }

    #[test]
    fn forward_matches_direct_sum() {
        let m = Mdct::new();
        let mut s = Scratch::default();
        for seed in 1..4 {
            let x = noise(512, seed);
            let xa: [f32; 512] = x.clone().try_into().unwrap();
            for short in [false, true] {
                let mut out = [0.0f32; 256];
                m.forward(&xa, short, &mut out, &mut s);
                let windowed: Vec<f64> = x
                    .iter()
                    .zip(m.window().iter())
                    .map(|(a, b)| f64::from(*a) * f64::from(*b))
                    .collect();
                let reference: Vec<f64> = if short {
                    let a = direct(&windowed[..256], Phase::FirstShort);
                    let b = direct(&windowed[256..], Phase::SecondShort);
                    (0..256)
                        .map(|k| if k % 2 == 0 { a[k / 2] } else { b[k / 2] })
                        .collect()
                } else {
                    direct(&windowed, Phase::Long)
                };
                let err = out
                    .iter()
                    .zip(&reference)
                    .map(|(a, b)| (f64::from(*a) - b).abs())
                    .fold(0.0, f64::max);
                assert!(err < 2e-6, "seed {seed} short {short}: max error {err}");
            }
        }
    }
}
