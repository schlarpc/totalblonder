//! Writing decoded pictures and samples into a slot: plain copies, and the few conversions
//! that are trivial enough to do here rather than refuse (no scaling, no resampling):
//!
//! - planar 4:2:2 to 4:2:0: each chroma row of the output is the mean of two input rows
//!   (MPEG-2 4:2:2 profile, some H.264 High 4:2:2 streams);
//! - planar 10-bit 4:2:0 (`yuv420p10le`, software HEVC Main 10) to P010: samples shifted to
//!   the high bits, chroma interleaved;
//! - audio in any of f32/s16/s32/f64, planar or packed, to f32 planar.
//!
//! Anything else is counted as unsupported and dropped. Pure functions on slices.

/// Copies `rows` rows of `bytes` from `src(r)` into `dst` at `stride`. False if a row is
/// missing or the destination is short.
pub fn copy_plane<'a>(
    dst: &mut [u8],
    stride: usize,
    rows: usize,
    bytes: usize,
    src: impl Fn(usize) -> Option<&'a [u8]>,
) -> bool {
    for r in 0..rows {
        let Some(s) = src(r) else { return false };
        let Some(d) = dst.get_mut(r * stride..r * stride + bytes) else {
            return false;
        };
        let Some(s) = s.get(..bytes) else {
            return false;
        };
        d.copy_from_slice(s);
    }
    true
}

/// A 4:2:2 chroma plane (`in_rows` rows) to 4:2:0 (`out_rows`): the mean of rows 2r and 2r+1
/// (rounded), the last row alone when the count is odd.
pub fn chroma_422_to_420<'a>(
    dst: &mut [u8],
    stride: usize,
    out_rows: usize,
    in_rows: usize,
    bytes: usize,
    src: impl Fn(usize) -> Option<&'a [u8]>,
) -> bool {
    for r in 0..out_rows {
        let a = src(2 * r);
        let b = src((2 * r + 1).min(in_rows.saturating_sub(1)));
        let (Some(a), Some(b)) = (a, b) else {
            return false;
        };
        let Some(d) = dst.get_mut(r * stride..r * stride + bytes) else {
            return false;
        };
        for ((o, x), y) in d.iter_mut().zip(a).zip(b) {
            *o = (u16::from(*x) + u16::from(*y)).div_ceil(2) as u8;
        }
    }
    true
}

/// 10-bit samples (u16 LE, value in the low bits) to P010 (value in the high bits): `n`
/// samples per row.
pub fn luma_10_to_p010<'a>(
    dst: &mut [u8],
    stride: usize,
    rows: usize,
    n: usize,
    src: impl Fn(usize) -> Option<&'a [u8]>,
) -> bool {
    for r in 0..rows {
        let Some(s) = src(r) else { return false };
        let Some(d) = dst.get_mut(r * stride..r * stride + 2 * n) else {
            return false;
        };
        for (o, i) in d
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(s.as_chunks::<2>().0)
        {
            *o = (u16::from_le_bytes(*i) << 6).to_le_bytes();
        }
    }
    true
}

/// Two 10-bit chroma planes to one interleaved P010 chroma plane: `n` samples per row each.
pub fn chroma_10_to_p010<'a>(
    dst: &mut [u8],
    stride: usize,
    rows: usize,
    n: usize,
    u: impl Fn(usize) -> Option<&'a [u8]>,
    v: impl Fn(usize) -> Option<&'a [u8]>,
) -> bool {
    for r in 0..rows {
        let (Some(us), Some(vs)) = (u(r), v(r)) else {
            return false;
        };
        let Some(d) = dst.get_mut(r * stride..r * stride + 4 * n) else {
            return false;
        };
        for ((o, a), b) in d
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(us.as_chunks::<2>().0)
            .zip(vs.as_chunks::<2>().0)
        {
            let a = u16::from_le_bytes(*a) << 6;
            let b = u16::from_le_bytes(*b) << 6;
            o[..2].copy_from_slice(&a.to_le_bytes());
            o[2..].copy_from_slice(&b.to_le_bytes());
        }
    }
    true
}

/// Audio sample encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// f32.
    F32,
    /// i16.
    S16,
    /// i32.
    S32,
    /// f64.
    F64,
}

impl Sample {
    /// Bytes per sample.
    pub fn size(self) -> usize {
        match self {
            Self::S16 => 2,
            Self::F32 | Self::S32 => 4,
            Self::F64 => 8,
        }
    }

    fn get(self, b: &[u8]) -> f32 {
        match (self, b) {
            (Self::F32, [a, b, c, d, ..]) => f32::from_le_bytes([*a, *b, *c, *d]),
            (Self::S16, [a, b, ..]) => f32::from(i16::from_le_bytes([*a, *b])) / 32768.0,
            (Self::S32, [a, b, c, d, ..]) => {
                (f64::from(i32::from_le_bytes([*a, *b, *c, *d])) / 2_147_483_648.0) as f32
            }
            (Self::F64, [a, b, c, d, e, f, g, h, ..]) => {
                f64::from_le_bytes([*a, *b, *c, *d, *e, *f, *g, *h]) as f32
            }
            _ => 0.0,
        }
    }
}

/// Audio to f32 planar in `dst` (`ch * n * 4` bytes). `plane(c)` is channel `c`'s plane
/// (planar) or, with `packed`, `plane(0)` holds all channels interleaved.
pub fn audio_to_f32_planar<'a>(
    dst: &mut [u8],
    ch: usize,
    n: usize,
    kind: Sample,
    packed: bool,
    plane: impl Fn(usize) -> Option<&'a [u8]>,
) -> bool {
    let sz = kind.size();
    if dst.len() < ch * n * 4 {
        return false;
    }
    for c in 0..ch {
        let src = if packed { plane(0) } else { plane(c) };
        let Some(src) = src else { return false };
        for i in 0..n {
            let at = if packed { (i * ch + c) * sz } else { i * sz };
            let Some(s) = src.get(at..at + sz) else {
                return false;
            };
            let v = kind.get(s);
            let o = (c * n + i) * 4;
            dst[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_copy_and_bounds() {
        let src = [[1u8, 2, 3, 9], [4, 5, 6, 9]];
        let mut dst = [0u8; 8];
        assert!(copy_plane(&mut dst, 4, 2, 3, |r| src
            .get(r)
            .map(|s| &s[..])));
        assert_eq!(dst, [1, 2, 3, 0, 4, 5, 6, 0]);
        assert!(!copy_plane(&mut dst, 4, 3, 3, |r| src
            .get(r)
            .map(|s| &s[..])));
        assert!(!copy_plane(&mut dst[..6], 4, 2, 3, |r| src
            .get(r)
            .map(|s| &s[..])));
    }

    #[test]
    fn chroma_averaging() {
        let src = [[10u8, 20], [11, 21], [30, 40]];
        let mut dst = [0u8; 4];
        assert!(chroma_422_to_420(&mut dst, 2, 2, 3, 2, |r| src
            .get(r)
            .map(|s| &s[..])));
        assert_eq!(dst, [11, 21, 30, 40]);
    }

    #[test]
    fn ten_bit_packing() {
        let y = [0x3ffu16.to_le_bytes(), 1u16.to_le_bytes()].concat();
        let mut dst = [0u8; 4];
        assert!(luma_10_to_p010(&mut dst, 4, 1, 2, |_| Some(&y[..])));
        assert_eq!(dst, [0xc0, 0xff, 0x40, 0x00]);
        let u = 2u16.to_le_bytes();
        let v = 3u16.to_le_bytes();
        let mut dst = [0u8; 4];
        assert!(chroma_10_to_p010(
            &mut dst,
            4,
            1,
            1,
            |_| Some(&u[..]),
            |_| Some(&v[..])
        ));
        assert_eq!(dst, [0x80, 0x00, 0xc0, 0x00]);
    }

    #[test]
    fn audio_conversions() {
        let packed: Vec<u8> = [16384i16, -32768, 0, 32767]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut dst = [0u8; 16];
        assert!(audio_to_f32_planar(
            &mut dst,
            2,
            2,
            Sample::S16,
            true,
            |_| Some(&packed[..])
        ));
        let f: Vec<f32> = dst
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert_eq!(f, vec![0.5, 0.0, -1.0, 32767.0 / 32768.0]);
        assert!(!audio_to_f32_planar(
            &mut dst[..8],
            2,
            2,
            Sample::S16,
            true,
            |_| Some(&packed[..])
        ));
    }
}
