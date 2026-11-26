//! A slow reference decoder for tests: parsed coefficients to PCM by the standard's own
//! equations, so that the encoder can be measured end to end where no external decoder exists
//! (the Nix sandbox).
//!
//! Rematrixing is undone as in A/52 §7.5.4 (p.85), and each block is inverse transformed with
//! the step-by-step procedure of §7.9.4.1 / §7.9.4.2 (p.98–102) — pre-twiddle, a complex IFFT
//! written as its defining double sum, post-twiddle, windowing with de-interleaving — then
//! overlapped and added with the factor 2 of step 6. Double precision, no dither, no dynamic
//! range control, no downmix.

use crate::mdct::kbd_window_half;
use crate::parse::Frame;
use crate::tables::{N_COEFS, REMATRIX_BAND_EDGES};

/// Overlap-add state of every channel.
#[derive(Debug, Clone)]
pub struct ReferenceDecoder {
    window: [f64; 256],
    delay: Vec<[f64; 256]>,
}

type Complex = (f64, f64);

fn cmul(a: Complex, b: Complex) -> Complex {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// Steps 2–4 of §7.9.4: pre-twiddle `X` (length 2m, m = N/4 or N/8 points), IFFT of m points
/// by its definition, post-twiddle. `cos_div` is 8N for the long and 4N for the short transform.
fn twiddle_ifft(x: &[f64], m: usize, cos_div: f64) -> Vec<Complex> {
    let pi = std::f64::consts::PI;
    let xc = |k: usize| -> Complex {
        let a = 2.0 * pi * (8.0 * k as f64 + 1.0) / cos_div;
        (-a.cos(), -a.sin())
    };
    let len = 2 * m;
    let z: Vec<Complex> = (0..m)
        .map(|k| cmul((x[len - 2 * k - 1], x[2 * k]), xc(k)))
        .collect();
    (0..m)
        .map(|n| {
            let mut acc = (0.0, 0.0);
            for (k, zk) in z.iter().enumerate() {
                let a = 2.0 * pi * (k * n) as f64 / m as f64;
                let t = cmul(*zk, (a.cos(), a.sin()));
                acc = (acc.0 + t.0, acc.1 + t.1);
            }
            cmul(acc, xc(n))
        })
        .collect()
}

impl ReferenceDecoder {
    /// A decoder for `channels` channels with silent history.
    pub fn new(channels: usize) -> Self {
        Self {
            window: kbd_window_half(),
            delay: vec![[0.0; 256]; channels],
        }
    }

    /// The window `w[n]`, n in 0..512 (Table 7.33 used back to back).
    fn w(&self, n: usize) -> f64 {
        if n < 256 {
            self.window[n]
        } else {
            self.window[511 - n]
        }
    }

    /// The windowed 512 samples of one block (§7.9.4.1 / §7.9.4.2 steps 1–5).
    fn imdct(&self, coefs: &[f64; N_COEFS], short: bool) -> [f64; 512] {
        const N: usize = 512;
        let mut x = [0.0f64; N];
        if !short {
            let y = twiddle_ifft(coefs, N / 4, 8.0 * N as f64);
            for n in 0..N / 8 {
                x[2 * n] = -y[N / 8 + n].1 * self.w(2 * n);
                x[2 * n + 1] = y[N / 8 - n - 1].0 * self.w(2 * n + 1);
                x[N / 4 + 2 * n] = -y[n].0 * self.w(N / 4 + 2 * n);
                x[N / 4 + 2 * n + 1] = y[N / 4 - n - 1].1 * self.w(N / 4 + 2 * n + 1);
                x[N / 2 + 2 * n] = -y[N / 8 + n].0 * self.w(N / 2 - 2 * n - 1);
                x[N / 2 + 2 * n + 1] = y[N / 8 - n - 1].1 * self.w(N / 2 - 2 * n - 2);
                x[3 * N / 4 + 2 * n] = y[n].1 * self.w(N / 4 - 2 * n - 1);
                x[3 * N / 4 + 2 * n + 1] = -y[N / 4 - n - 1].0 * self.w(N / 4 - 2 * n - 2);
            }
        } else {
            let x1: Vec<f64> = (0..N / 4).map(|k| coefs[2 * k]).collect();
            let x2: Vec<f64> = (0..N / 4).map(|k| coefs[2 * k + 1]).collect();
            let y1 = twiddle_ifft(&x1, N / 8, 4.0 * N as f64);
            let y2 = twiddle_ifft(&x2, N / 8, 4.0 * N as f64);
            for n in 0..N / 8 {
                x[2 * n] = -y1[n].1 * self.w(2 * n);
                x[2 * n + 1] = y1[N / 8 - n - 1].0 * self.w(2 * n + 1);
                x[N / 4 + 2 * n] = -y1[n].0 * self.w(N / 4 + 2 * n);
                x[N / 4 + 2 * n + 1] = y1[N / 8 - n - 1].1 * self.w(N / 4 + 2 * n + 1);
                x[N / 2 + 2 * n] = -y2[n].0 * self.w(N / 2 - 2 * n - 1);
                x[N / 2 + 2 * n + 1] = y2[N / 8 - n - 1].1 * self.w(N / 2 - 2 * n - 2);
                x[3 * N / 4 + 2 * n] = y2[n].1 * self.w(N / 4 - 2 * n - 1);
                x[3 * N / 4 + 2 * n + 1] = -y2[N / 8 - n - 1].0 * self.w(N / 4 - 2 * n - 2);
            }
        }
        x
    }

    /// One block of coefficients per channel (full-bandwidth first, then LFE) to 256 samples
    /// per channel, appended to `out[ch]`.
    pub fn decode_block(&mut self, coefs: &[[f64; N_COEFS]], blksw: &[bool], out: &mut [Vec<f32>]) {
        for (ch, c) in coefs.iter().enumerate() {
            let short = blksw.get(ch).copied().unwrap_or(false);
            let x = self.imdct(c, short);
            for n in 0..256 {
                // Step 6: pcm[n] = 2·(x[n] + delay[n]); delay[n] = x[N/2 + n].
                out[ch].push((2.0 * (x[n] + self.delay[ch][n])) as f32);
                self.delay[ch][n] = x[256 + n];
            }
        }
    }

    /// Decodes a parsed frame: 1536 samples per channel appended to `out`.
    pub fn decode_frame(&mut self, frame: &Frame, out: &mut [Vec<f32>]) {
        let stereo = frame.header.bsi.acmod == 2;
        for blk in &frame.blocks {
            let mut coefs: Vec<[f64; N_COEFS]> = blk
                .coefs
                .iter()
                .map(|c| std::array::from_fn(|k| f64::from(c[k])))
                .collect();
            if stereo {
                // §7.5.4: left = L' + R', right = L' − R', up to the lower bandwidth.
                let end = blk.end[0].min(blk.end[1]);
                for (band, &flag) in blk.rematflg.iter().enumerate() {
                    if !flag {
                        continue;
                    }
                    let lo = REMATRIX_BAND_EDGES[band];
                    let hi = REMATRIX_BAND_EDGES[band + 1].min(end);
                    let (left, right) = coefs.split_at_mut(1);
                    for (l, r) in left[0][lo..hi].iter_mut().zip(&mut right[0][lo..hi]) {
                        (*l, *r) = (*l + *r, *l - *r);
                    }
                }
            }
            self.decode_block(&coefs, &blk.blksw, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdct::{Mdct, Scratch};

    /// The encoder's MDCT followed by the standard's IMDCT reconstructs the input exactly (up to
    /// float precision) for any pattern of long and short blocks: time-domain aliasing cancels
    /// across every combination (A/52 §7.9, p.97–102).
    #[test]
    fn mdct_and_spec_imdct_reconstruct_with_block_switching() {
        let mdct = Mdct::new();
        let mut scratch = Scratch::default();
        let mut s = 12345u32;
        let input: Vec<f32> = (0..256 * 24)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        let pattern = [
            false, true, true, false, false, true, false, true, false, false, true,
        ];
        let mut dec = ReferenceDecoder::new(1);
        let mut out = vec![Vec::new()];
        let mut prev = [0.0f32; 256];
        for (b, chunk) in input.chunks(256).enumerate() {
            let mut buf = [0.0f32; 512];
            buf[..256].copy_from_slice(&prev);
            buf[256..].copy_from_slice(chunk);
            prev.copy_from_slice(chunk);
            let short = pattern[b % pattern.len()];
            let mut c = [0.0f32; 256];
            mdct.forward(&buf, short, &mut c, &mut scratch);
            let c64: [f64; 256] = std::array::from_fn(|k| f64::from(c[k]));
            dec.decode_block(&[c64], &[short], &mut out);
        }
        // Output block b is input block b − 1.
        let err = input[..input.len() - 256]
            .iter()
            .zip(&out[0][256..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 1e-5, "max reconstruction error {err}");
    }
}
