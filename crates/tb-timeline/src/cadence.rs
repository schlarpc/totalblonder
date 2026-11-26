//! Output cadence (SPEC §5.2), the field-rate tick divider of the C++ harness, and unwrapping of
//! input timestamps that wrap (33-bit MPEG PTS, 32-bit RTMP milliseconds).

use crate::{HZ, Ticks27, sat};

/// Why a [`Cadence`] could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CadenceError {
    /// The frame period is zero or has a zero denominator.
    #[error("frame period {num}/{den} s is not positive")]
    Period {
        /// Numerator of the period in seconds.
        num: u64,
        /// Denominator of the period in seconds.
        den: u64,
    },
}

/// A program's fixed output cadence (SPEC §5.2): frame `k` has presentation time
/// `P0 + k · period`, with the period a rational number of seconds (for example 1001/60000 s for
/// 720p59.94). Computed exactly from `k` with a 128-bit intermediate, never by accumulation, so
/// the frame times of day 30 are as exact as those of the first second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cadence {
    p0: Ticks27,
    num: u64,
    den: u64,
}

impl Cadence {
    /// A cadence starting at `p0` with a frame period of `num / den` seconds.
    pub fn new(p0: Ticks27, num: u64, den: u64) -> Result<Self, CadenceError> {
        if num == 0 || den == 0 {
            return Err(CadenceError::Period { num, den });
        }
        Ok(Self { p0, num, den })
    }
    /// Presentation time of output frame `k`: `P0 + floor(k · 27 MHz · num / den)`.
    ///
    /// Every standard rate (1001/60000, 1001/30000, 1/25, 1/50, 1001/24000) is a whole number of
    /// 27 MHz ticks, so the floor is exact for them.
    pub fn pts(&self, k: u64) -> Ticks27 {
        let off = i128::from(k) * i128::from(HZ) * i128::from(self.num) / i128::from(self.den);
        Ticks27(sat(i128::from(self.p0.0) + off))
    }
    /// The frame period rounded down to a whole tick.
    pub fn period(&self) -> Ticks27 {
        Ticks27(sat(
            i128::from(HZ) * i128::from(self.num) / i128::from(self.den)
        ))
    }
    /// The start time `P0`.
    pub fn p0(&self) -> Ticks27 {
        self.p0
    }
}

/// A 27 MHz time as written into a PES header: rounded to the nearest 90 kHz tick (half up) and
/// reduced modulo 2^33 (SPEC §5.2 "rounded to 90 kHz when written").
pub fn to_pts90(t: Ticks27) -> u64 {
    let v = (i128::from(t.0) + 150).div_euclid(300);
    // rem_euclid of a positive modulus is in [0, 2^33), which fits u64.
    u64::try_from(v.rem_euclid(1i128 << 33)).unwrap_or(0)
}

/// A format's clock as a divisor and phase of the field-rate tick (59.94 Hz), as the C++ harness
/// does it (`(t + c.phase) % c.every != c.every - 1` skips the tick): 720p uses every
/// tick (`every = 1`), 29.97 Hz channels use every second one and alternate between the two
/// phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divider {
    every: u32,
    phase: u32,
}

impl Divider {
    /// Fire on every `every`-th field tick with phase `phase` (`every` 0 is taken as 1; the phase
    /// is reduced modulo `every`).
    pub fn new(every: u32, phase: u32) -> Self {
        let every = every.max(1);
        Self {
            every,
            phase: phase % every,
        }
    }
    /// Whether the format's frame clock runs at field tick `tick`.
    pub fn fires(&self, tick: u64) -> bool {
        let e = u64::from(self.every);
        (tick % e + u64::from(self.phase)) % e == e - 1
    }
    /// The output frame index of field tick `tick` if it fires: the number of earlier ticks that
    /// fired.
    pub fn frame_index(&self, tick: u64) -> Option<u64> {
        if !self.fires(tick) {
            return None;
        }
        let e = u64::from(self.every);
        // ticks 0..=tick that fire: those with (t + phase) % e == e - 1
        let first = (e - 1 + e - u64::from(self.phase)) % e;
        Some((tick - first) / e)
    }
}

/// Makes an input time base that wraps continuous, converting it to 27 MHz.
///
/// MPEG PTS wrap at 2^33 ticks of 90 kHz (26.5 hours); RTMP timestamps at 2^32 ms (49.7 days).
/// A step is taken as the shorter way round the circle, so a step of less than half the range
/// backwards stays backwards (and the mapper ends the session for it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unwrapper {
    bits: u32,
    scale: i64,
    last: Option<u64>,
    ext: i128,
}

impl Unwrapper {
    /// `bits` wide timestamps (clamped to 1..=62), `scale` 27 MHz ticks per unit.
    pub fn new(bits: u32, scale: i64) -> Self {
        Self {
            bits: bits.clamp(1, 62),
            scale,
            last: None,
            ext: 0,
        }
    }
    /// 33-bit 90 kHz MPEG timestamps.
    pub fn mpeg_90khz() -> Self {
        Self::new(33, 300)
    }
    /// 32-bit millisecond RTMP timestamps.
    pub fn rtmp_ms() -> Self {
        Self::new(32, HZ / 1000)
    }
    /// The continuous 27 MHz time of the next raw timestamp.
    pub fn unwrap(&mut self, raw: u64) -> Ticks27 {
        let m = 1u64 << self.bits;
        let raw = raw & (m - 1);
        match self.last {
            None => self.ext = i128::from(raw),
            Some(l) => {
                let d = raw.wrapping_sub(l) & (m - 1);
                let d = if d >= m / 2 {
                    i128::from(d) - i128::from(m)
                } else {
                    i128::from(d)
                };
                self.ext += d;
            }
        }
        self.last = Some(raw);
        Ticks27(sat(self.ext * i128::from(self.scale)))
    }
    /// Start again (a new connection).
    pub fn reset(&mut self) {
        self.last = None;
        self.ext = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SPEC §5.2: frame k at P0 + k·period, exact; 720p59.94 is 450450 ticks a frame.
    #[test]
    fn spec_5_2_cadence_is_exact_over_30_days() {
        let c = Cadence::new(Ticks27(12345), 1001, 60000).unwrap();
        assert_eq!(c.period(), Ticks27(450_450));
        // 30 days of 59.94 Hz frames: the last one is still exact.
        let k = 30 * 86_400 * 60_000 / 1001;
        assert_eq!(c.pts(k), Ticks27(12345 + k as i64 * 450_450));
        for k in [0u64, 1, 2, 1000, 1_000_000, k] {
            assert_eq!(c.pts(k + 1).0 - c.pts(k).0, 450_450);
        }
        let c = Cadence::new(Ticks27(0), 1, 25).unwrap();
        assert_eq!(c.pts(25), Ticks27::from_secs(1));
        assert!(Cadence::new(Ticks27(0), 0, 1).is_err());
        assert!(Cadence::new(Ticks27(0), 1, 0).is_err());
    }

    /// SPEC §5.2: rounded to 90 kHz when written; the PES field is 33 bits.
    #[test]
    fn spec_5_2_pts90_rounds_and_wraps_at_33_bits() {
        assert_eq!(to_pts90(Ticks27(0)), 0);
        assert_eq!(to_pts90(Ticks27(149)), 0);
        assert_eq!(to_pts90(Ticks27(150)), 1);
        assert_eq!(to_pts90(Ticks27(300 * ((1 << 33) + 5))), 5);
        assert_eq!(to_pts90(Ticks27(-300)), (1 << 33) - 1);
        // 30 days at 59.94: the written PTS steps by 1501.5 -> 1501 or 1502, modulo 2^33.
        let c = Cadence::new(Ticks27(0), 1001, 60000).unwrap();
        let k0 = 30 * 86_400 * 60_000 / 1001 - 10_000;
        let mut prev = to_pts90(c.pts(k0));
        for k in k0 + 1..k0 + 10_000 {
            let p = to_pts90(c.pts(k));
            let step = (p + (1 << 33) - prev) % (1 << 33);
            assert!(step == 1501 || step == 1502, "{step}");
            prev = p;
        }
    }

    /// The C++ harness: a format's clock is a divisor and a phase of the field-rate tick.
    #[test]
    fn the_divider_matches_the_pipeline_harness_formula() {
        for every in 1..4u32 {
            for phase in 0..every {
                let d = Divider::new(every, phase);
                let mut n = 0;
                for t in 0..100u64 {
                    let cpp = (t + u64::from(phase)) % u64::from(every) == u64::from(every) - 1;
                    assert_eq!(d.fires(t), cpp);
                    if cpp {
                        assert_eq!(d.frame_index(t), Some(n));
                        n += 1;
                    } else {
                        assert_eq!(d.frame_index(t), None);
                    }
                }
            }
        }
        // the two phases of 29.97 alternate
        let (a, b) = (Divider::new(2, 0), Divider::new(2, 1));
        for t in 0..10 {
            assert_ne!(a.fires(t), b.fires(t));
        }
        assert!(Divider::new(0, 7).fires(3));
        assert_eq!(Divider::new(1, 0).frame_index(u64::MAX), Some(u64::MAX));
    }

    /// 33-bit PTS wrap: continuous across many wraps (30 days is 27 wraps).
    #[test]
    fn pts_wrap_33_bits_is_continuous_for_30_days() {
        let mut u = Unwrapper::mpeg_90khz();
        let step = 3003u64; // 29.97 fps in 90 kHz
        let start = (1u64 << 33) - 10 * step;
        let n = 30 * 86_400 * 30_000 / 1001 / 64; // every 64th frame: still < half the range
        let mut prev = u.unwrap(start);
        for i in 1..n {
            let raw = (start + i * step * 64) % (1 << 33);
            let t = u.unwrap(raw);
            assert_eq!(t.0 - prev.0, (step * 64 * 300) as i64);
            prev = t;
        }
        // backwards stays backwards
        let mut u = Unwrapper::mpeg_90khz();
        let a = u.unwrap(5);
        let b = u.unwrap((1 << 33) - 5);
        assert_eq!(b.0 - a.0, -10 * 300);
        u.reset();
        assert_eq!(u.unwrap(7), Ticks27(7 * 300));
    }

    #[test]
    fn rtmp_ms_wrap_at_32_bits() {
        let mut u = Unwrapper::rtmp_ms();
        let a = u.unwrap(u64::from(u32::MAX) - 10);
        let b = u.unwrap(23);
        assert_eq!(b.0 - a.0, 34 * 27_000);
    }
}
