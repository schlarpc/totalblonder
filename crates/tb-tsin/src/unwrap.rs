//! 33-bit PTS/DTS made continuous: the rule of `tb_timeline::Unwrapper` (each step the shorter
//! way round the circle), mirrored in 90 kHz ticks so that this crate depends only on
//! `tb-media`.

/// Unwraps 33-bit 90 kHz timestamps into a continuous `i64`, starting at the first raw value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unwrap33 {
    last: Option<u64>,
    ext: i64,
}

const M: u64 = 1 << 33;

impl Unwrap33 {
    /// A fresh unwrapper.
    pub const fn new() -> Self {
        Self { last: None, ext: 0 }
    }

    /// The continuous time of `raw` (its low 33 bits).
    pub fn unwrap(&mut self, raw: u64) -> i64 {
        let raw = raw & (M - 1);
        match self.last {
            // raw < 2^33.
            None => self.ext = raw as i64,
            Some(l) => {
                let d = raw.wrapping_sub(l) & (M - 1);
                let d = if d >= M / 2 {
                    d as i64 - M as i64
                } else {
                    d as i64
                };
                self.ext = self.ext.saturating_add(d);
            }
        }
        self.last = Some(raw);
        self.ext
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps() {
        let mut u = Unwrap33::new();
        assert_eq!(u.unwrap(M - 3000), (M - 3000) as i64);
        assert_eq!(u.unwrap(3000), (M + 3000) as i64);
        // PTS ahead of DTS, then DTS: small steps both ways.
        assert_eq!(u.unwrap(M - 10), (M - 10) as i64);
        assert_eq!(u.unwrap(6000), (M + 6000) as i64);
    }
}
