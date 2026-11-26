//! Leaky buckets in exact integer arithmetic: the buffers of the T-STD (Rec. §2.4.2.3–2.4.2.6,
//! p.14–19) that the scheduler must never overflow, and the token bucket that paces a
//! program's video to its delivery rate.
//!
//! Contents are kept in "bit-ticks" (bits × 27 000 000): a buffer draining at `r` bit/s loses
//! exactly `r` bit-ticks per 27 MHz tick, so draining and filling are integer operations with
//! no rounding at all.

use tb_ts::STC_HZ;

/// A buffer of `size` bytes drained at `rate` bit/s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leaky {
    size: u128,
    rate: u128,
    level: u128,
    at: u64,
}

const fn bt(bytes: u64) -> u128 {
    bytes as u128 * 8 * STC_HZ as u128
}

impl Leaky {
    /// An empty buffer of `size_bytes` draining at `rate_bps`, observed at STC `at`.
    pub const fn new(size_bytes: u64, rate_bps: u64, at: u64) -> Self {
        Leaky {
            size: bt(size_bytes),
            rate: rate_bps as u128,
            level: 0,
            at,
        }
    }

    /// Drains up to STC `now` (never backwards).
    pub fn advance(&mut self, now: u64) {
        if now > self.at {
            let d = u128::from(now - self.at) * self.rate;
            self.level = self.level.saturating_sub(d);
            self.at = now;
        }
    }

    /// Whether `bytes` more fit at `now`.
    pub fn admits(&mut self, now: u64, bytes: u64) -> bool {
        self.advance(now);
        self.level + bt(bytes) <= self.size
    }

    /// Adds `bytes` at `now` (the caller checked [`Leaky::admits`]).
    pub fn add(&mut self, now: u64, bytes: u64) {
        self.advance(now);
        self.level += bt(bytes);
    }

    /// Bytes held at the last observation.
    pub fn bytes(&self) -> u64 {
        (self.level / bt(1)) as u64
    }

    /// Changes the drain rate from `now` on.
    pub fn set_rate(&mut self, now: u64, rate_bps: u64) {
        self.advance(now);
        self.rate = u128::from(rate_bps);
    }
}

/// A token bucket: credit accrues at `rate` bit/s up to `depth` packets; a packet costs one
/// packet of credit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    rate: u128,
    cap: u128,
    credit: u128,
    at: u64,
}

/// Bit-ticks of one 188-byte packet.
const PACKET: u128 = bt(188);

impl Tokens {
    /// A full bucket of `depth` packets at `rate_bps`, observed at `at`.
    pub const fn new(rate_bps: u64, depth: u64, at: u64) -> Self {
        let cap = PACKET * depth as u128;
        Tokens {
            rate: rate_bps as u128,
            cap,
            credit: cap,
            at,
        }
    }

    fn advance(&mut self, now: u64) {
        if now > self.at {
            self.credit = (self.credit + u128::from(now - self.at) * self.rate).min(self.cap);
            self.at = now;
        }
    }

    /// Whether a packet may go at `now`.
    pub fn ready(&mut self, now: u64) -> bool {
        self.advance(now);
        self.credit >= PACKET
    }

    /// Spends one packet at `now` (the caller checked [`Tokens::ready`]).
    pub fn spend(&mut self, now: u64) {
        self.advance(now);
        self.credit = self.credit.saturating_sub(PACKET);
    }

    /// Changes the rate from `now` on.
    pub fn set_rate(&mut self, now: u64, rate_bps: u64) {
        self.advance(now);
        self.rate = u128::from(rate_bps);
    }

    /// Credit at `now`, in bit-ticks (for choosing who goes first).
    pub fn credit(&mut self, now: u64) -> u128 {
        self.advance(now);
        self.credit
    }

    /// The rate, bit/s.
    pub fn rate(&self) -> u64 {
        self.rate as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_buffer_spacing() {
        // TB of 512 bytes at 2 Mbit/s (audio): two packets back to back fit, a third does not
        // until 52 bytes (416 bits, 208 µs = 5 616 ticks) have drained.
        let mut b = Leaky::new(512, 2_000_000, 0);
        assert!(b.admits(0, 188));
        b.add(0, 188);
        b.add(0, 188);
        assert!(!b.admits(0, 188));
        assert!(!b.admits(5_615, 188));
        assert!(b.admits(5_616, 188));
        assert_eq!(b.bytes(), 324);
    }

    #[test]
    fn tokens_pace_to_rate() {
        // 1 Mbit/s: one packet per 1504 µs = 40 608 ticks; depth 2.
        let mut t = Tokens::new(1_000_000, 2, 0);
        let mut sent = 0;
        let mut now = 0;
        while now < 27_000_000 {
            if t.ready(now) {
                t.spend(now);
                sent += 1;
            }
            now += 1000;
        }
        // 664.9 packets per second plus the initial two.
        assert!((665..=667).contains(&sent), "{sent}");
    }
}
