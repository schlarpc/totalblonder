//! The deterministic test clips of the golden vectors: a port of `crates/tb-golden/vectors/make_clip.py`
//! (`tb-m2venc --make-clip`). The flake check `gate/make-clip` compares the two outputs.

/// What kind of clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipKind {
    /// Progressive motion, light noise.
    Plain,
    /// The two fields of a frame are half a frame period apart.
    Interlaced,
    /// Strong noise: a picture that cannot be coded well at any rate.
    Noisy,
    /// Uniform white noise over the whole sample range in luma and chroma: the largest
    /// macroblocks a picture can have (the macroblock size rule of H.262 Table 8-6).
    White,
    /// The plain clip with a band of white noise (lines 48 to 79, luma and chroma): a busy
    /// patch in an easy picture, which rate control codes with a fine quantizer.
    Patch,
}

fn luma(x: i64, y: i64, t: i64, scene: u32, w: i64, n: i64) -> i64 {
    // Python's // and % round toward minus infinity.
    let fd = |a: i64, b: i64| a.div_euclid(b);
    let fm = |a: i64, b: i64| a.rem_euclid(b);
    if scene == 0 {
        let mut v =
            96 + fm((x + 3 * t) * 5 + y * 3, 97) + fm(fd(x + 3 * t, 16) + fd(y, 16), 2) * 40;
        let (bx, by) = (20 + fm(2 * n - 2 * t, w - 48), 24 + fm(t, 40));
        if bx <= x && x < bx + 32 && by <= y && y < by + 32 {
            v = 40 + fm((x - bx) * 7 + (y - by) * 11, 150);
        }
        v
    } else {
        let mut v = 60 + fm((x - 2 * t) * 2 + (y + t) * 4, 140);
        if fm(fd(x - 2 * t, 24) + fd(y + t, 24), 3) == 0 {
            v = 220 - fd(v, 2);
        }
        v
    }
}

/// `make_clip.py WIDTH HEIGHT FRAMES SEED [interlaced|noisy|white|patch]`: `frames` frames of raw 4:2:0.
pub fn make_clip(w: u32, h: u32, frames: u32, seed: u64, kind: ClipKind) -> Vec<u8> {
    let (wi, hi, n) = (i64::from(w), i64::from(h), i64::from(frames));
    let div = if kind == ClipKind::Noisy { 1 } else { 16 };
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let (cw, ch) = (wi / 2, hi / 2);
    let white = |y: i64| match kind {
        ClipKind::White => true,
        ClipKind::Patch => (48..80).contains(&y),
        _ => false,
    };
    let mut out = Vec::with_capacity((frames as usize) * (w as usize) * (h as usize) * 3 / 2);
    for f in 0..n {
        let scene = if f < n / 2 { 0 } else { 1 };
        for y in 0..hi {
            let t = if kind == ClipKind::Interlaced {
                2 * f + (y & 1)
            } else {
                2 * f
            };
            for x in 0..wi {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let noise = ((state >> 57) as i64 - 64).div_euclid(div);
                let v = luma(x, y, t, scene, wi, n) + noise;
                if white(y) {
                    out.push((state >> 56) as u8);
                } else {
                    out.push(v.clamp(16, 235) as u8);
                }
            }
        }
        for c in 0..2i64 {
            for y in 0..ch {
                for x in 0..cw {
                    let v = 128 + ((((x + f) / 8 + y / 8 + c + i64::from(scene)) % 5) - 2) * 12;
                    if white(2 * y) {
                        state = state
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        out.push((state >> 56) as u8);
                    } else {
                        out.push(v as u8);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_ranges() {
        for kind in [ClipKind::Plain, ClipKind::Interlaced, ClipKind::Noisy] {
            let c = make_clip(64, 48, 4, 3, kind);
            assert_eq!(c.len(), 4 * 64 * 48 * 3 / 2);
            let (y, uv) = c.split_at(64 * 48);
            assert!(y.iter().all(|v| (16..=235).contains(v)));
            assert!(uv[..32 * 24 * 2].iter().all(|v| (104..=152).contains(v)));
        }
        // white noise uses the whole range; the patch only in its band
        let c = make_clip(64, 96, 1, 3, ClipKind::White);
        assert_eq!(c.len(), 64 * 96 * 3 / 2);
        assert!(c.iter().any(|v| *v < 8) && c.iter().any(|v| *v > 247));
        let p = make_clip(64, 96, 1, 3, ClipKind::Patch);
        let plain = make_clip(64, 96, 1, 3, ClipKind::Plain);
        assert_eq!(p[..48 * 64], plain[..48 * 64]);
        assert_ne!(p[48 * 64..80 * 64], plain[48 * 64..80 * 64]);
        assert!(p[48 * 64..80 * 64].iter().any(|v| *v < 8));
        assert!(p[80 * 64..96 * 64].iter().all(|v| (16..=235).contains(v)));
        assert_ne!(
            make_clip(64, 48, 2, 3, ClipKind::Plain),
            make_clip(64, 48, 2, 4, ClipKind::Plain)
        );
    }
}
