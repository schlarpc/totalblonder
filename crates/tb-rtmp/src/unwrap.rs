//! RTMP timestamps made continuous.
//!
//! The base rule is `tb_timeline::Unwrapper`'s (which converts to 27 MHz): each step is taken
//! as the shorter way round the 2^32 circle, so a small step backwards stays backwards.
//! Mirrored here, in the input's own unit, so that this crate does not depend on the timeline.
//!
//! **31-bit publishers.** ffmpeg's FLV muxer writes the timestamp's top byte masked with 0x7F
//! ("timestamps are 32 bits _signed_", `libavformat/flvenc.c`, `put_timestamp`), and its RTMP
//! client takes message timestamps from those FLV tags, so an ffmpeg publisher (the producer is
//! one) wraps at 2^31 ms, 24.8 days, not 2^32: `2147482967, 2147483000, 219, 252`, measured
//! with `-output_ts_offset`. Read on the 2^32 circle that is a step of 24.8 days backwards.
//! So when both timestamps are below 2^31 and the 32-bit step is 2^30 ms (12.4 days) or more
//! either way, the step is taken on the 2^31 circle instead, provided that gives less than
//! 2^20 ms (17 minutes) and the continuous time has already reached the wrap zone (within
//! 2^20 ms of 2^31 or beyond: otherwise the jump is a real one, such as ffmpeg's sequence
//! header at 0 followed by frames at a large `-output_ts_offset`). No 32-bit publisher steps by
//! 12 days between two messages, and both directions are handled so that audio and video
//! interleaved across the wrap stay in order.

use crate::types::RtmpTime;

const HALF: u32 = 1 << 31;

/// Unwraps RTMP millisecond timestamps into a continuous `i64`, starting at the first raw
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MsUnwrapper {
    last: Option<u32>,
    ext: i64,
}

impl MsUnwrapper {
    /// A fresh unwrapper.
    pub const fn new() -> Self {
        Self { last: None, ext: 0 }
    }

    /// The continuous time of `t`.
    pub fn unwrap(&mut self, t: RtmpTime) -> i64 {
        match self.last {
            None => self.ext = i64::from(t.0),
            Some(l) => self.ext = self.ext.saturating_add(step(l, t.0, self.ext)),
        }
        self.last = Some(t.0);
        self.ext
    }
}

/// The step from `l` to `t`, the continuous time being `ext` (see the module documentation).
fn step(l: u32, t: u32, ext: i64) -> i64 {
    // The shorter way round 2^32: wrapping_sub reinterpreted as signed.
    let d32 = i64::from(t.wrapping_sub(l).cast_signed());
    let in_zone = ext >= i64::from(HALF) - (1 << 20);
    if in_zone && l < HALF && t < HALF && d32.unsigned_abs() >= 1 << 30 {
        let d = i64::from(t.wrapping_sub(l) & (HALF - 1));
        let d31 = if d >= 1 << 30 { d - (1 << 31) } else { d };
        if d31.unsigned_abs() < 1 << 20 {
            return d31;
        }
    }
    d32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_forward_and_steps_back() {
        let mut u = MsUnwrapper::new();
        assert_eq!(u.unwrap(RtmpTime(u32::MAX - 10)), i64::from(u32::MAX) - 10);
        assert_eq!(u.unwrap(RtmpTime(5)), i64::from(u32::MAX) + 6);
        assert_eq!(u.unwrap(RtmpTime(3)), i64::from(u32::MAX) + 4);
        let mut u = MsUnwrapper::new();
        assert_eq!(u.unwrap(RtmpTime(100)), 100);
        assert_eq!(u.unwrap(RtmpTime(50)), 50);
    }

    #[test]
    fn wraps_at_2_pow_31_like_ffmpeg() {
        let h = i64::from(HALF);
        let mut u = MsUnwrapper::new();
        assert_eq!(u.unwrap(RtmpTime(HALF - 33)), h - 33);
        // Video past the wrap, then audio from just before it, then video again.
        assert_eq!(u.unwrap(RtmpTime(219)), h + 219);
        assert_eq!(u.unwrap(RtmpTime(HALF - 10)), h - 10);
        assert_eq!(u.unwrap(RtmpTime(252)), h + 252);
        // A 32-bit publisher crossing 2^31 is unaffected.
        let mut u = MsUnwrapper::new();
        assert_eq!(u.unwrap(RtmpTime(HALF - 33)), h - 33);
        assert_eq!(u.unwrap(RtmpTime(HALF + 1)), h + 1);
        assert_eq!(u.unwrap(RtmpTime(HALF - 5)), h - 5);
        // A real reset to 0 from early in a session stays a step backwards.
        let mut u = MsUnwrapper::new();
        assert_eq!(u.unwrap(RtmpTime(5000)), 5000);
        assert_eq!(u.unwrap(RtmpTime(0)), 0);
        // A sequence header at 0, then frames just below 2^31: a jump forward, then the wrap.
        let mut u = MsUnwrapper::new();
        assert_eq!(u.unwrap(RtmpTime(0)), 0);
        assert_eq!(u.unwrap(RtmpTime(HALF - 715)), h - 715);
        assert_eq!(u.unwrap(RtmpTime(219)), h + 219);
    }
}
