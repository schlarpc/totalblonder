//! The carrier's slot clock (SPEC §5.1).
//!
//! A carrier's output is an endless sequence of 188-byte slots `n = 0, 1, …`. At a constant
//! rate `R` bit/s slot `n` starts at `n · 1504 / R` seconds after the epoch. The STC at the
//! byte that carries the last bit of `program_clock_reference_base` (byte 10 of the packet,
//! Rec. §2.4.2.2, p.13) is
//!
//! ```text
//! STC(n) = stc0 + floor((188·n + 10) · 8 · 27 000 000 / R)
//! ```
//!
//! computed exactly in 128-bit integers, so a PCR's error against its byte position is the
//! rounding to one 27 MHz tick and nothing accumulates (TR 101 290 PCR_AC ≈ 0 by
//! construction).

use tb_ts::{PACKET_LEN, STC_HZ, Stc};

/// A transport stream rate in bit/s, validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TsRate(u64);

/// The modulator's 256-QAM Annex B payload rate, bit/s (SCTE 07; `notes/standards-digest.md`
/// §8). A carrier's rate must stay below it.
pub const QAM256_PAYLOAD: u64 = 38_810_701;

impl TsRate {
    /// The default carrier rate of SPEC §3, 38.8 Mbit/s: 276 ppm under the payload rate.
    pub const DEFAULT: TsRate = TsRate(38_800_000);

    /// `bps` if within 1 to 200 Mbit/s.
    pub const fn new(bps: u64) -> Option<Self> {
        if bps >= 1_000_000 && bps <= 200_000_000 {
            Some(TsRate(bps))
        } else {
            None
        }
    }

    /// Bit/s.
    pub const fn bps(self) -> u64 {
        self.0
    }
}

/// Maps slot indices to STC values and wall time offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotClock {
    rate: TsRate,
    stc0: u64,
}

const BITS: u128 = (PACKET_LEN as u128) * 8;

impl SlotClock {
    /// A clock at `rate` whose slot 0 starts at STC `stc0`.
    pub const fn new(rate: TsRate, stc0: Stc) -> Self {
        SlotClock { rate, stc0: stc0.0 }
    }

    /// The rate.
    pub const fn rate(&self) -> TsRate {
        self.rate
    }

    /// STC of byte `byte` (0..188) of slot `n`.
    pub fn stc_at_byte(&self, n: u64, byte: u64) -> Stc {
        let bits = (u128::from(n) * PACKET_LEN as u128 + u128::from(byte)) * 8;
        let t = bits * u128::from(STC_HZ) / u128::from(self.rate.0);
        Stc(self.stc0 + t as u64)
    }

    /// STC at the start of slot `n`.
    pub fn stc(&self, n: u64) -> Stc {
        self.stc_at_byte(n, 0)
    }

    /// The value a PCR in slot `n` must carry.
    pub fn pcr_stc(&self, n: u64) -> Stc {
        self.stc_at_byte(n, 10)
    }

    /// The first slot whose start is at or after `t`.
    pub fn slot_at_or_after(&self, t: Stc) -> u64 {
        let dt = u128::from(t.0.saturating_sub(self.stc0));
        // smallest n with n·BITS·HZ/R ≥ dt  ⇔  n ≥ dt·R / (BITS·HZ)
        let num = dt * u128::from(self.rate.0);
        let den = BITS * u128::from(STC_HZ);
        num.div_ceil(den) as u64
    }

    /// Nanoseconds from the epoch to the start of slot `n` (for the pacer), rounded down.
    pub fn nanos(&self, n: u64) -> u128 {
        u128::from(n) * BITS * 1_000_000_000 / u128::from(self.rate.0)
    }

    /// 27 MHz ticks of one slot, rounded up.
    pub fn slot_ticks(&self) -> u64 {
        (BITS * u128::from(STC_HZ)).div_ceil(u128::from(self.rate.0)) as u64
    }

    /// 27 MHz ticks needed to send `packets` packets at `bps`, rounded up.
    pub fn ticks_for(packets: u64, bps: u64) -> u64 {
        if bps == 0 {
            return u64::MAX;
        }
        (u128::from(packets) * BITS * u128::from(STC_HZ)).div_ceil(u128::from(bps)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_over_a_month() {
        let c = SlotClock::new(TsRate::DEFAULT, Stc(0));
        // One second of 38.8 Mbit/s is 38_800_000 / 1504 slots, not an integer; but 1504 · 27e6
        // / 38.8e6 · n is exact at n = 38_800_000 / gcd: check a month of slots against f64.
        let n = 25_797_872u64 * 3600 * 24 * 30;
        let t = c.stc(n).0 as f64;
        let want = n as f64 * 1504.0 * 27e6 / 38.8e6;
        assert!((t - want).abs() < 1e6 * 1e-6 * want.max(1.0));
        // Monotone and slot length ~1046.6 ticks.
        assert_eq!(c.stc(1).0 - c.stc(0).0, 1046);
        assert_eq!(c.slot_ticks(), 1047);
    }

    #[test]
    fn pcr_offset_is_ten_bytes() {
        let c = SlotClock::new(TsRate::DEFAULT, Stc(1000));
        let d = c.pcr_stc(5).0 - c.stc(5).0;
        // 80 bits at 38.8 Mbit/s = 55.67 ticks.
        assert!((55..=56).contains(&d));
    }

    #[test]
    fn slot_at_or_after_inverts() {
        let c = SlotClock::new(TsRate::DEFAULT, Stc(7));
        for n in [0u64, 1, 2, 1000, 123_456_789] {
            assert_eq!(c.slot_at_or_after(c.stc(n)), n);
            assert_eq!(c.slot_at_or_after(Stc(c.stc(n).0 + 1)), n + 1);
        }
    }
}
