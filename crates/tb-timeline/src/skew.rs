//! Skew estimate of the mapper (SPEC §5.3 "Skew estimate").
//!
//! The SPEC: track the minimum of `arrival_time − in_time` over a sliding window (default 30 s);
//! the slope of that minimum over time is the producer's clock error; `skew` follows it through a
//! slow filter, clamped to ±500 ppm.
//!
//! Implementation [choice, the SPEC leaves the estimator open]: the window is cut into
//! [`BUCKETS`] buckets of arrival time; each bucket keeps its minimum of `arrival − in` (network
//! delay only ever adds to it, so the minimum is the transit floor). When a bucket closes, the
//! slope of the floor against input time is the least-squares line through the closed buckets'
//! minima of the last window. It needs at least [`MIN_POINTS`] points spanning half the window.
//! The filtered skew moves [`GAIN`] of the way to the clamped estimate at every closed bucket
//! (time constant about sixteen buckets, 32 s by default) and is clamped again.
//!
//! Floating point is used here and only here. It is safe: the inputs are at most a window of
//! integer differences (exact in `f64` below 2^53 ticks, 10 years), the output is quantized to
//! whole parts per billion before the mapper uses it, the map itself is exact rational
//! arithmetic, and nothing is accumulated across buckets except the filtered skew, which is a
//! clamped contraction toward the latest estimate (its error does not grow with time).

use std::collections::VecDeque;

use crate::sat;

/// Buckets per window.
pub(crate) const BUCKETS: i64 = 15;
/// Minimum number of closed buckets for an estimate.
pub(crate) const MIN_POINTS: usize = 4;
/// Share of the distance to the estimate the filtered skew moves per closed bucket.
pub(crate) const GAIN: f64 = 0.0625;
/// Upper bound of the clamp whatever the configuration says (1+skew must stay positive).
const PPM_LIMIT: f64 = 100_000.0;

#[derive(Clone, Copy, Debug)]
struct Point {
    /// Bucket index (arrival time / bucket length).
    idx: i64,
    /// Input time of the minimum.
    x: i64,
    /// The minimum of `arrival − in`.
    d: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct SkewEstimator {
    bucket_len: i64,
    max_ppm: f64,
    cur: Option<Point>,
    done: VecDeque<Point>,
    skew_ppm: f64,
    estimate_ppm: Option<f64>,
}

impl SkewEstimator {
    pub(crate) fn new(window: i64, max_ppm: f64) -> Self {
        let max_ppm = if max_ppm.is_finite() && max_ppm > 0.0 {
            max_ppm.min(PPM_LIMIT)
        } else {
            0.0
        };
        Self {
            bucket_len: (window / BUCKETS).max(1),
            max_ppm,
            cur: None,
            done: VecDeque::with_capacity(BUCKETS as usize + 1),
            skew_ppm: 0.0,
            estimate_ppm: None,
        }
    }

    /// The filtered skew in parts per billion (what the map uses).
    pub(crate) fn ppb(&self) -> i64 {
        // |skew_ppm| <= PPM_LIMIT, so the product fits easily; `as` saturates in any case.
        (self.skew_ppm * 1000.0).round() as i64
    }

    pub(crate) fn skew_ppm(&self) -> f64 {
        self.skew_ppm
    }

    pub(crate) fn estimate_ppm(&self) -> Option<f64> {
        self.estimate_ppm
    }

    /// The minimum of the newest closed bucket: the input time of the frame that traveled
    /// fastest in it, and its `arrival − in`. The mapper measures the buffer depth on it (the
    /// depth without network delay).
    pub(crate) fn floor(&self) -> Option<(i64, i64)> {
        self.done.back().map(|p| (p.x, p.d))
    }

    /// One frame of the session (input time, arrival time, arrival never decreasing). Returns
    /// true when the filtered skew was updated.
    pub(crate) fn sample(&mut self, x: i64, arrival: i64) -> bool {
        let idx = arrival.div_euclid(self.bucket_len);
        let d = sat(i128::from(arrival) - i128::from(x));
        match &mut self.cur {
            Some(c) if c.idx == idx => {
                if d < c.d {
                    c.x = x;
                    c.d = d;
                }
                false
            }
            Some(c) if idx < c.idx => false, // arrival went backwards: the caller prevents it
            cur => {
                let closed = cur.replace(Point { idx, x, d });
                match closed {
                    Some(p) => {
                        self.done.push_back(p);
                        // keep the closed buckets of the last window
                        while let Some(f) = self.done.front() {
                            if idx.saturating_sub(f.idx) > BUCKETS
                                || self.done.len() > BUCKETS as usize
                            {
                                self.done.pop_front();
                            } else {
                                break;
                            }
                        }
                        self.update()
                    }
                    None => false,
                }
            }
        }
    }

    fn update(&mut self) -> bool {
        if self.done.len() < MIN_POINTS {
            return false;
        }
        let Some(first) = self.done.front().copied() else {
            return false;
        };
        let span = self
            .done
            .iter()
            .map(|p| i128::from(p.x) - i128::from(first.x))
            .max()
            .unwrap_or(0);
        if span * 2 < i128::from(self.bucket_len) * i128::from(BUCKETS) {
            return false;
        }
        let n = self.done.len() as f64;
        let pts = self.done.iter().map(|p| {
            (
                (i128::from(p.x) - i128::from(first.x)) as f64,
                (i128::from(p.d) - i128::from(first.d)) as f64,
            )
        });
        let (mut sx, mut sy) = (0.0, 0.0);
        for (x, y) in pts.clone() {
            sx += x;
            sy += y;
        }
        let (mx, my) = (sx / n, sy / n);
        let (mut sxx, mut sxy) = (0.0, 0.0);
        for (x, y) in pts {
            sxx += (x - mx) * (x - mx);
            sxy += (x - mx) * (y - my);
        }
        if sxx <= 0.0 {
            return false;
        }
        let est = sxy / sxx * 1e6;
        if !est.is_finite() {
            return false;
        }
        let est = est.clamp(-self.max_ppm, self.max_ppm);
        self.estimate_ppm = Some(est);
        self.skew_ppm =
            (self.skew_ppm + (est - self.skew_ppm) * GAIN).clamp(-self.max_ppm, self.max_ppm);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounded memory and bounded output on adversarial samples: extreme values, arrival jumps,
    /// bursts, input time going anywhere.
    #[test]
    fn adversarial_samples_bounded_memory_and_skew() {
        let mut seed = 7u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            seed
        };
        for case in 0..200 {
            let window = [0, 1, 30 * 27_000_000, i64::MAX][case % 4];
            let max = [500.0, f64::NAN, -1.0, f64::INFINITY, 1e12][case % 5];
            let mut s = SkewEstimator::new(window, max);
            let mut arrival = (next() >> 1) as i64 - i64::MAX / 2;
            for _ in 0..2000 {
                let r = next();
                arrival = match r % 4 {
                    0 => arrival,
                    1 => arrival.saturating_add((r >> 40) as i64),
                    2 => arrival.saturating_add((r >> 8) as i64),
                    _ => arrival.saturating_add(1),
                };
                let x = match (r >> 2) % 5 {
                    0 => i64::MIN,
                    1 => i64::MAX,
                    2 => arrival,
                    3 => (next() >> 1) as i64,
                    _ => arrival.saturating_sub((r >> 30) as i64),
                };
                s.sample(x, arrival);
                assert!(s.done.len() <= BUCKETS as usize);
                let lim = if max.is_finite() && max > 0.0 {
                    max.min(PPM_LIMIT)
                } else {
                    0.0
                };
                assert!(s.skew_ppm().abs() <= lim);
                assert!(s.ppb().unsigned_abs() <= (lim * 1000.0) as u64 + 1);
            }
        }
    }
}
