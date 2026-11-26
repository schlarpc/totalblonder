//! Bits as a function of the quantizer (the rho-domain model of He and Mitra):
//! `bits ~ theta * non-zero levels + header bits`.
use tb_kernel_abi::{NZ_BINS, RowStat};

/// Lower edges of the histogram bins of [`RowStat::nz`]: a coefficient in bin i becomes zero
/// when the base quantiser_scale exceeds a value in `[edge[i], edge[i + 1])`
/// (the C++ `k_nz_edge`, as `float` values widened to `double`).
pub const NZ_EDGE: [f64; NZ_BINS + 1] = [
    1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0, 64.0, 96.0, 128.0, 192.0, 1e9,
];

/// The model of one picture type of one session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateModel {
    /// Bits per non-zero level.
    pub theta: f64,
    /// Bits that do not depend on the quantizer; negative: not measured yet.
    pub hdr: f64,
}

impl Default for RateModel {
    fn default() -> Self {
        Self {
            theta: 6.5,
            hdr: -1.0,
        }
    }
}

/// quantiser_scale for each quantiser_scale_code with `q_scale_type = 1` (Table 7-6; the C++
/// `k_qscale`).
pub const QSCALE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64,
    72, 80, 88, 96, 104, 112,
];

/// Non-zero levels of a row (or of a sum of rows) at quantiser_scale `q`, interpolated
/// logarithmically inside a bin (the C++ `nz_at`).
///
/// The C++ divides two `float` edges and takes `std::log` of the `float` quotient, which is the
/// `float` overload (`logf`): the bin width is computed in `f32` here as well.
pub fn nz_at(row: &RowStat, q: f64) -> f64 {
    let mut cum = [0.0f64; NZ_BINS + 1];
    for i in (0..NZ_BINS).rev() {
        cum[i] = cum[i + 1] + f64::from(row.nz[i]);
    }
    if q <= 1.0 {
        return cum[0];
    }
    for i in 0..NZ_BINS {
        if q < NZ_EDGE[i + 1] {
            if i == NZ_BINS - 1 {
                return cum[i];
            }
            let width = f64::from((edge_f32(i + 1) / edge_f32(i)).ln());
            let f = (q / NZ_EDGE[i]).ln() / width;
            return cum[i] + f * (cum[i + 1] - cum[i]);
        }
    }
    0.0
}

/// `k_nz_edge[i]` as the C++ holds it (`float`; every edge is exact in `f32`).
#[allow(
    clippy::cast_possible_truncation,
    reason = "the edges are exactly representable in f32"
)]
fn edge_f32(i: usize) -> f32 {
    NZ_EDGE[i] as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_f32_exact() {
        for e in NZ_EDGE {
            assert_eq!(f64::from(e as f32), e);
        }
    }

    #[test]
    fn nz_at_endpoints() {
        let mut r = RowStat::default();
        for (i, n) in r.nz.iter_mut().enumerate() {
            *n = 10 * (i as u32 + 1);
        }
        let total: f64 = r.nz.iter().map(|&n| f64::from(n)).sum();
        assert_eq!(nz_at(&r, 0.5), total);
        assert_eq!(nz_at(&r, 1.0), total);
        // At a lower edge the interpolation gives the cumulative count of that bin.
        assert_eq!(nz_at(&r, 2.0), total - 10.0 - 20.0);
        // The last bin is not interpolated; past the last edge nothing is left.
        assert_eq!(nz_at(&r, 500.0), 160.0);
        assert_eq!(nz_at(&r, 2e9), 0.0);
        // Monotone in q.
        let mut last = f64::INFINITY;
        for &q in &QSCALE[1..] {
            let v = nz_at(&r, f64::from(q));
            assert!(v <= last);
            last = v;
        }
    }
}
