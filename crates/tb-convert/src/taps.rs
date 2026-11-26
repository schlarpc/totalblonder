//! Filter taps of one axis (`pp_kernel`, `pp_support`, `TapSpec`, `HostTaps`, `make_taps`
//! of the C++ spike).
//!
//! The arithmetic is the C++'s operation for operation: `f64` where it has `double`, `i32`
//! where it has `int`, the same order of operations and the same conversions (`floor`, `ceil`,
//! `lround`, the truncation to `int16_t`). `sin` is the platform's (`f64::sin`, glibc's `sin`
//! on Linux, as the C++ calls it); the golden records of `taps.jsonl` pin the results.

use crate::ConvertError;

/// Resampling filter. The order is the C++'s (`F_BILINEAR` = 0, `F_BICUBIC` = 1,
/// `F_LANCZOS3` = 2): conversion classes are ordered by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Filter {
    /// Bilinear (cheap reference).
    Bilinear,
    /// Catmull-Rom bicubic.
    Bicubic,
    /// Lanczos with three lobes (default).
    Lanczos3,
}

/// Which rows of a frame take part. The order is the C++'s (`ROWS_ALL` = 0, `ROWS_TOP` = 1,
/// `ROWS_BOTTOM` = 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rows {
    /// All rows.
    All,
    /// The top field.
    Top,
    /// The bottom field.
    Bottom,
}

impl Rows {
    /// Lattice step: 1 for all rows, 2 for a field.
    pub const fn step(self) -> u32 {
        match self {
            Rows::All => 1,
            Rows::Top | Rows::Bottom => 2,
        }
    }

    /// Lattice offset: 1 for the bottom field, else 0.
    pub const fn off(self) -> u32 {
        match self {
            Rows::Bottom => 1,
            Rows::All | Rows::Top => 0,
        }
    }
}

/// One axis of a conversion (the C++ `TapSpec`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TapSpec {
    /// Source samples on the axis.
    pub n_src: u32,
    /// Source lattice: samples `s_off + s_step * i`.
    pub s_step: u32,
    /// Source lattice offset.
    pub s_off: u32,
    /// Destination samples.
    pub n_dst: u32,
    /// Destination lattice step.
    pub d_step: u32,
    /// Destination lattice offset.
    pub d_off: u32,
    /// First source sample of the crop.
    pub crop0: f64,
    /// Length of the crop in source samples.
    pub crop_len: f64,
    /// Sampling phase (chroma siting).
    pub phase: f64,
    /// The filter.
    pub filter: Filter,
    /// Low-pass across destination lines before taking every second one (frame to field).
    pub weave_lp: bool,
}

/// Taps of one axis: for each output sample the first source sample and `nt` Q14 coefficients
/// that sum to 16384 (`HostTaps`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Taps {
    /// First source sample per output (an index into the source lattice).
    pub start: Vec<i32>,
    /// `n * nt` coefficients.
    pub coef: Vec<i16>,
    /// Outputs.
    pub n: u32,
    /// Taps per output.
    pub nt: u32,
    /// Samples of the source lattice (`HostTaps::n_lat`): the rows of the intermediate planes of
    /// a vertical axis.
    pub n_lat: u32,
}

/// The filter's kernel at `x` (the C++ `pp_kernel`).
pub fn pp_kernel(f: Filter, x: f64) -> f64 {
    let x = x.abs();
    match f {
        Filter::Bilinear => {
            if x < 1.0 {
                1.0 - x
            } else {
                0.0
            }
        }
        // Catmull-Rom (a = -0.5)
        Filter::Bicubic => {
            if x < 1.0 {
                1.5 * x * x * x - 2.5 * x * x + 1.0
            } else if x < 2.0 {
                -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
            } else {
                0.0
            }
        }
        Filter::Lanczos3 => {
            if x < 1e-9 {
                return 1.0;
            }
            if x >= 3.0 {
                return 0.0;
            }
            let a = std::f64::consts::PI * x;
            3.0 * a.sin() * (a / 3.0).sin() / (a * a)
        }
    }
}

/// Half-width of the filter's kernel in samples (the C++ `pp_support`).
pub fn pp_support(f: Filter) -> f64 {
    match f {
        Filter::Bilinear => 1.0,
        Filter::Bicubic => 2.0,
        Filter::Lanczos3 => 3.0,
    }
}

/// `std::max(a, b)` for doubles: `a` unless `a < b`.
fn cmax(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

/// A count of the spec as the C++ `int`. Counts are limited to 2^24 so that no `int` arithmetic
/// of `make_taps` can overflow.
fn int(v: u32) -> Result<i32, ConvertError> {
    if v > 1 << 24 {
        return Err(ConvertError::BadSpec);
    }
    i32::try_from(v).map_err(|_| ConvertError::BadSpec)
}

/// The taps of one axis (the C++ `make_taps`). `pad4`: round the tap count up to a multiple
/// of four.
///
/// Errors: [`ConvertError::SourceTooSmall`] where the C++ ends the process (the padded tap count
/// exceeds the source lattice); [`ConvertError::BadSpec`] for specs on which the C++ is
/// undefined (a zero step, an empty source lattice, a count beyond 2^24, an output with no
/// taps).
pub fn make_taps(spec: &TapSpec, pad4: bool) -> Result<Taps, ConvertError> {
    let s = spec;
    let (n_src, s_step, s_off) = (int(s.n_src)?, int(s.s_step)?, int(s.s_off)?);
    let (n_dst, d_step, d_off) = (int(s.n_dst)?, int(s.d_step)?, int(s.d_off)?);
    if s_step == 0 || d_step == 0 {
        return Err(ConvertError::BadSpec);
    }
    let n_lat = (n_src - s_off + s_step - 1) / s_step;
    let n = (n_dst - d_off + d_step - 1) / d_step;
    if n_lat <= 0 || n < 0 {
        return Err(ConvertError::BadSpec);
    }
    let scale = s.crop_len / f64::from(n_dst); // source samples per destination sample (frame units)
    let same_lattice = s_step == d_step; // frame -> frame or field -> field
    let mut stretch = cmax(
        1.0,
        if same_lattice {
            scale
        } else {
            scale / f64::from(s_step)
        },
    ); // in lattice units
    if s_step == 1 && d_step == 2 {
        stretch = cmax(1.0, scale); // frame -> field: antialias for the frame pitch
    }
    let sup = pp_support(s.filter) * stretch;
    let lp_off = if s.weave_lp {
        scale / f64::from(s_step)
    } else {
        0.0
    };
    let mut all: Vec<Vec<(i32, f64)>> = Vec::with_capacity(n as usize);
    let mut nt: i32 = 1;
    let mut f = vec![0.0f64; n_lat as usize];
    for k in 0..n {
        let d = d_off + d_step * k;
        let pos = s.crop0 + (f64::from(d) + s.phase) * scale - s.phase; // source coordinate (frame units)
        let lat = (pos - f64::from(s_off)) / f64::from(s_step); // lattice coordinate
        let lo = (lat - sup - lp_off).floor() as i32;
        let hi = (lat + sup + lp_off).ceil() as i32;
        let mut w = Vec::with_capacity((hi - lo + 1).max(0) as usize);
        let mut sum = 0.0f64;
        for i in lo..=hi {
            let fi = f64::from(i);
            let v = if s.weave_lp {
                0.25 * pp_kernel(s.filter, (fi - (lat - lp_off)) / stretch)
                    + 0.5 * pp_kernel(s.filter, (fi - lat) / stretch)
                    + 0.25 * pp_kernel(s.filter, (fi - (lat + lp_off)) / stretch)
            } else {
                pp_kernel(s.filter, (fi - lat) / stretch)
            };
            w.push(v);
            sum += v;
        }
        // fold taps outside the lattice onto its edge samples, drop zero taps at both ends
        f.iter_mut().for_each(|x| *x = 0.0);
        let (mut first, mut last) = (n_lat, -1);
        for (i, &wi) in (lo..=hi).zip(&w) {
            if wi.abs() < 1e-6 * sum.abs() {
                continue; // (a Lanczos kernel at whole offsets is 1e-17, not 0)
            }
            let c = i.max(0).min(n_lat - 1);
            f[c as usize] += wi / sum;
            first = first.min(c);
            last = last.max(c);
        }
        if first > last {
            return Err(ConvertError::BadSpec); // the C++ reads an empty list here
        }
        all.push((first..=last).map(|i| (i, f[i as usize])).collect());
        nt = nt.max(last - first + 1);
    }
    if pad4 {
        nt = (nt + 3) & !3;
    }
    if nt > n_lat {
        return Err(ConvertError::SourceTooSmall {
            n_lat: n_lat as u32,
            nt: nt as u32,
        });
    }
    let ntu = nt as usize;
    let mut start = vec![0i32; n as usize];
    let mut coef = vec![0i16; n as usize * ntu];
    for (k, taps) in all.iter().enumerate() {
        let Some(&(front, _)) = taps.first() else {
            return Err(ConvertError::BadSpec);
        };
        let st = front.min(n_lat - nt);
        start[k] = st;
        let row = &mut coef[k * ntu..(k + 1) * ntu];
        let mut sum: i32 = 0;
        let mut big: usize = 0;
        for &(i, v) in taps {
            let q = (v * 16384.0).round() as i64 as i32; // (int)std::lround
            let at = (i - st) as usize;
            row[at] = q as i16;
            sum += q;
            if q.abs() > i32::from(row[big]).abs() {
                big = at;
            }
        }
        // int16_t += (int16_t)(16384 - sum): promoted to int, stored back as int16_t
        row[big] = (i32::from(row[big]) + i32::from((16384 - sum) as i16)) as i16;
    }
    Ok(Taps {
        start,
        coef,
        n: n as u32,
        nt: nt as u32,
        n_lat: n_lat as u32,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn filter(v: i64) -> Filter {
        match v {
            0 => Filter::Bilinear,
            1 => Filter::Bicubic,
            _ => Filter::Lanczos3,
        }
    }

    fn u(r: &tb_golden::Record, f: &str) -> u32 {
        u32::try_from(r.int(f).unwrap()).unwrap()
    }

    pub(crate) fn spec_of(r: &tb_golden::Record) -> (TapSpec, bool) {
        (
            TapSpec {
                n_src: u(r, "n_src"),
                s_step: u(r, "s_step"),
                s_off: u(r, "s_off"),
                n_dst: u(r, "n_dst"),
                d_step: u(r, "d_step"),
                d_off: u(r, "d_off"),
                crop0: r.num("crop0").unwrap(),
                crop_len: r.num("crop_len").unwrap(),
                phase: r.num("phase").unwrap(),
                filter: filter(r.int("filter").unwrap()),
                weave_lp: r.flag("weave_lp").unwrap(),
            },
            r.flag("pad4").unwrap(),
        )
    }

    pub(crate) fn taps_of(r: &tb_golden::Record) -> Taps {
        let start = r.bytes("start").unwrap();
        let coef = r.bytes("coef").unwrap();
        Taps {
            start: start
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            coef: coef
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect(),
            n: u(r, "n"),
            nt: u(r, "nt"),
            n_lat: u(r, "n_lat"),
        }
    }

    #[test]
    fn taps_match_the_golden_records() {
        let recs = tb_golden::case("taps").unwrap();
        let (mut ok, mut refused) = (0, 0);
        for r in recs.iter().filter(|r| r.kind() == "taps") {
            let (spec, pad4) = spec_of(r);
            let got = make_taps(&spec, pad4);
            let seq = r.int("seq").unwrap();
            if r.flag("error").unwrap() {
                assert!(
                    matches!(got, Err(ConvertError::SourceTooSmall { .. })),
                    "seq {seq}: {spec:?} pad4 {pad4}: expected a refusal, got {got:?}"
                );
                refused += 1;
            } else {
                assert_eq!(
                    got.as_ref(),
                    Ok(&taps_of(r)),
                    "seq {seq}: {spec:?} pad4 {pad4}"
                );
                ok += 1;
            }
        }
        assert!(ok > 800 && refused == 3, "{ok} taps, {refused} refusals");
    }

    #[test]
    fn coefficients_sum_to_one() {
        for r in tb_golden::case("taps").unwrap().iter() {
            if r.kind() != "taps" || r.flag("error").unwrap() {
                continue;
            }
            let t = taps_of(r);
            for row in t.coef.chunks_exact(t.nt as usize) {
                assert_eq!(row.iter().map(|&c| i32::from(c)).sum::<i32>(), 16384);
            }
        }
    }

    #[test]
    fn degenerate_specs_are_errors() {
        let base = TapSpec {
            n_src: 16,
            s_step: 1,
            s_off: 0,
            n_dst: 8,
            d_step: 1,
            d_off: 0,
            crop0: 0.0,
            crop_len: 16.0,
            phase: 0.5,
            filter: Filter::Lanczos3,
            weave_lp: false,
        };
        assert!(make_taps(&base, true).is_ok());
        let zero_step = TapSpec { s_step: 0, ..base };
        assert_eq!(make_taps(&zero_step, false), Err(ConvertError::BadSpec));
        let empty = TapSpec {
            s_off: 1,
            n_src: 1,
            s_step: 2,
            ..base
        };
        assert_eq!(make_taps(&empty, false), Err(ConvertError::BadSpec));
        let huge = TapSpec {
            n_src: u32::MAX,
            ..base
        };
        assert_eq!(make_taps(&huge, false), Err(ConvertError::BadSpec));
    }
}
