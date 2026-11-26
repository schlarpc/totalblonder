//! The buffer model (the C++ `Vbv`).
//!
//! - coded picture n enters the mux queue at `e_n = n * T` (T = frame period) and is removed by
//!   the decoder at `e_n + D`, `D = vbv_bits / max_bitrate`;
//! - `rho(t)` is the session's rate, piecewise constant, changed at a picture time;
//! - the mux drains the queue at a rate `a(t) >= rhobar(t) = max{rho(s) : t - D <= s <= t}`
//!   while data is queued;
//! - guarantee: every picture has completely left the queue by `e_n + D` (no decoder
//!   underflow), and the decoder never holds more than `max_bitrate * D = vbv_bits`.

/// Time of the first history entry, "since forever" (the C++ `-1e18`).
const T_FIRST: f64 = -1e18;
/// End of the last history entry, "until forever" (the C++ `1e18`).
const T_LAST: f64 = 1e18;

/// A rate that applies from a time on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateChange {
    /// Seconds on the session's picture clock.
    pub t: f64,
    /// Bit/s.
    pub rate: f64,
}

/// State of the buffer model of one session.
#[derive(Clone, Debug, PartialEq)]
pub struct Vbv {
    /// The buffer delay D in seconds.
    pub d: f64,
    /// Bits in the queue at time `t`, just after the last picture was added.
    pub queue: f64,
    /// Time of the last picture.
    pub t: f64,
    /// Rate history, ascending in time. Never empty after [`Vbv::new`].
    pub changes: Vec<RateChange>,
}

impl Vbv {
    /// A model with delay `d` and initial rate `rate`, empty queue, at time 0
    /// (`Vbv::init`; the first history entry is at -1e18).
    pub fn new(d: f64, rate: f64) -> Self {
        Self {
            d,
            queue: 0.0,
            t: 0.0,
            changes: vec![RateChange { t: T_FIRST, rate }],
        }
    }

    /// The current rate.
    pub fn rate(&self) -> f64 {
        self.changes.last().map_or(0.0, |c| c.rate)
    }

    /// Change the rate at time `at` (no entry when the rate is unchanged; an entry at or after
    /// `at` is overwritten).
    pub fn set_rate(&mut self, at: f64, rate: f64) {
        match self.changes.last_mut() {
            Some(last) if rate != last.rate => {
                if last.t >= at {
                    last.rate = rate;
                } else {
                    self.changes.push(RateChange { t: at, rate });
                }
            }
            Some(_) => {}
            None => self.changes.push(RateChange { t: at, rate }),
        }
    }

    /// `rhobar` at time `x`, from the history known now.
    pub fn rbar(&self, x: f64) -> f64 {
        let mut m = 0.0;
        for (i, c) in self.changes.iter().enumerate() {
            let end = self.changes.get(i + 1).map_or(T_LAST, |n| n.t);
            // std::max(m, r): r only when m < r.
            if c.t <= x && end + self.d > x && m < c.rate {
                m = c.rate;
            }
        }
        m
    }

    /// Integral of `rhobar` over `[a, b]`: bits the mux is assumed to deliver.
    ///
    /// The C++ (`gint`) collects the breakpoints `a`, `b`, every change time and every change
    /// time plus D strictly inside `(a, b)`, sorts them and sums `rbar(midpoint) * width` from
    /// left to right. The change times ascend strictly and adding D keeps that order (rounding
    /// is monotone), so both lists of interior points are already sorted and a merge yields
    /// the sorted sequence without allocating; the sum then runs over the same values in the
    /// same order.
    pub fn delivered(&self, a: f64, b: f64) -> f64 {
        if a > b {
            // The C++ sorts {b, a}: no point lies strictly inside, one interval from b to a.
            return self.rbar(0.5 * (b + a)) * (a - b);
        }
        let d = self.d;
        let inside = move |x: &f64| *x > a && *x < b;
        let mut starts = self.changes.iter().map(|c| c.t).filter(inside).peekable();
        let mut ends = self
            .changes
            .iter()
            .map(move |c| c.t + d)
            .filter(inside)
            .peekable();
        let interior = std::iter::from_fn(|| match (starts.peek(), ends.peek()) {
            (Some(s), Some(e)) if e < s => ends.next(),
            (Some(_), _) => starts.next(),
            (None, _) => ends.next(),
        });
        let mut s = 0.0;
        let mut lo = a;
        for hi in interior.chain(std::iter::once(b)) {
            s += self.rbar(0.5 * (lo + hi)) * (hi - lo);
            lo = hi;
        }
        s
    }

    /// Advance to the time of the next picture and drop history older than D. Returns the queue
    /// before the picture is added.
    pub fn advance(&mut self, t: f64) -> f64 {
        let delivered = self.delivered(self.t, t);
        self.queue = max0(self.queue - delivered);
        self.t = t;
        let d = self.d;
        // Keep the newest entry that started at or before t - D and everything after it.
        let old = self
            .changes
            .iter()
            .skip(1)
            .take_while(|c| c.t + d < t)
            .count();
        self.changes.drain(..old);
        self.queue
    }

    /// Bits deliverable in the next D seconds.
    pub fn cap(&self) -> f64 {
        self.delivered(self.t, self.t + self.d)
    }

    /// `cap() - queue`: how large the next picture may be.
    pub fn room(&self) -> f64 {
        self.cap() - self.queue
    }

    /// Add a coded picture of `bits`.
    pub fn add(&mut self, bits: f64) {
        self.queue += bits;
    }
}

/// `std::max(0.0, x)`: `x` only when `0 < x`.
fn max0(x: f64) -> f64 {
    if 0.0 < x { x } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++ `gint` as written: a vector of breakpoints, sorted, summed left to right.
    fn gint_sorted(v: &Vbv, a: f64, b: f64) -> f64 {
        let mut bp = vec![a, b];
        for c in &v.changes {
            if c.t > a && c.t < b {
                bp.push(c.t);
            }
            if c.t + v.d > a && c.t + v.d < b {
                bp.push(c.t + v.d);
            }
        }
        bp.sort_by(f64::total_cmp);
        let mut s = 0.0;
        for w in bp.windows(2) {
            s += v.rbar(0.5 * (w[0] + w[1])) * (w[1] - w[0]);
        }
        s
    }

    /// A fixed xorshift sequence.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Random walks over the model: rate changes at picture times, pictures of random size.
    fn walks(mut f: impl FnMut(&mut Vbv, &mut Rng, f64)) {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for walk in 0..64 {
            let d = 0.1 + rng.unit();
            let period = [1.0 / 25.0, 1001.0 / 30000.0, 1001.0 / 60000.0, 0.2][walk % 4];
            let mut v = Vbv::new(d, 1e5 + 1e7 * rng.unit());
            for n in 0..300 {
                let t = f64::from(n) * period;
                f(&mut v, &mut rng, t);
            }
        }
    }

    #[test]
    fn selftest_rate_decrease() {
        let mut v = Vbv::new(0.5, 8e6);
        v.advance(1.0);
        v.set_rate(1.0, 4e6);
        let c0 = v.cap();
        v.advance(1.25);
        let c1 = v.cap();
        v.advance(1.6);
        let c2 = v.cap();
        assert!((c0 - 4e6).abs() < 1.0, "{c0}");
        assert!((c1 - 3e6).abs() < 1.0, "{c1}");
        assert!((c2 - 2e6).abs() < 1.0, "{c2}");
    }

    #[test]
    fn set_rate_overwrites_or_appends() {
        let mut v = Vbv::new(0.5, 1e6);
        v.set_rate(1.0, 1e6);
        assert_eq!(v.changes.len(), 1);
        v.set_rate(1.0, 2e6);
        assert_eq!(v.changes.len(), 2);
        v.set_rate(1.0, 3e6);
        assert_eq!(v.changes[1], RateChange { t: 1.0, rate: 3e6 });
        v.set_rate(0.5, 4e6);
        assert_eq!(v.changes.len(), 2);
        assert_eq!(v.rate(), 4e6);
    }

    #[test]
    fn delivered_equals_sorted_breakpoints() {
        walks(|v, rng, t| {
            if rng.next().is_multiple_of(5) {
                v.set_rate(t, 1e5 + 1e7 * rng.unit());
            }
            let a = t - 2.0 * rng.unit();
            let b = t + 2.0 * rng.unit();
            assert_eq!(v.delivered(a, b).to_bits(), gint_sorted(v, a, b).to_bits());
            assert_eq!(v.cap().to_bits(), gint_sorted(v, v.t, v.t + v.d).to_bits());
            v.advance(t);
        });
    }

    #[test]
    fn rbar_is_max_of_last_d() {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        for _ in 0..64 {
            let d = 0.1 + rng.unit();
            let period = 0.02 + 0.2 * rng.unit();
            let mut v = Vbv::new(d, 1e5 + 1e7 * rng.unit());
            // The whole history, never pruned.
            let mut log = v.changes.clone();
            for n in 0..300 {
                let t = f64::from(n) * period;
                if rng.next().is_multiple_of(4) {
                    let r = 1e5 + 1e7 * rng.unit();
                    v.set_rate(t, r);
                    if log.last().is_some_and(|l| l.rate == r) {
                    } else if let Some(last) = log.last_mut().filter(|l| l.t >= t) {
                        last.rate = r;
                    } else {
                        log.push(RateChange { t, rate: r });
                    }
                }
                v.advance(t);
                assert!(v.changes.iter().all(|c| log.contains(c)));
                let x = t + d * rng.unit();
                // The rates in force somewhere in (x - D, x].
                let mut want: f64 = 0.0;
                for (i, c) in log.iter().enumerate() {
                    let end = log.get(i + 1).map_or(T_LAST, |n| n.t);
                    if c.t <= x && end + d > x {
                        want = want.max(c.rate);
                    }
                }
                assert_eq!(v.rbar(x), want);
            }
        }
    }

    #[test]
    fn delivered_is_additive() {
        walks(|v, rng, t| {
            if rng.next().is_multiple_of(4) {
                v.set_rate(t, 1e5 + 1e7 * rng.unit());
            }
            v.advance(t);
            let a = t - rng.unit();
            let m = a + 2.0 * rng.unit();
            let b = m + 2.0 * rng.unit();
            let whole = v.delivered(a, b);
            let parts = v.delivered(a, m) + v.delivered(m, b);
            assert!(
                (whole - parts).abs() <= 1e-6 * whole.abs(),
                "{whole} {parts}"
            );
        });
    }

    #[test]
    fn queue_never_negative() {
        walks(|v, rng, t| {
            if rng.next().is_multiple_of(6) {
                v.set_rate(t, 1e5 + 1e7 * rng.unit());
            }
            let before = v.advance(t);
            assert!(before >= 0.0 && v.queue >= 0.0);
            let room = v.room();
            v.add((room * 1.2 * rng.unit()).max(0.0));
            assert!(v.queue >= 0.0);
        });
    }
}
