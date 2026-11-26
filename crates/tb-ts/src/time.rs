//! The system time clock and the time stamps derived from it.
//!
//! - The STC counts 27 MHz ticks (Rec. §2.4.2.1, p.12). Inside the server it is kept
//!   unwrapped in a `u64` ([`Stc`]), which lasts 21,000 years; only the encodings wrap.
//! - A PCR is the STC modulo 2^33 · 300, sent as a 33-bit base in 90 kHz units and a 9-bit
//!   extension (Rec. eq. 2-1 to 2-3, p.13).
//! - PTS and DTS are 33-bit counts of 90 kHz (Rec. §2.4.3.7, p.38: `PTS = (system_clock_frequency
//!   · tp / 300) % 2^33`), [`Ts90k`].

/// 27 MHz: the system clock frequency (Rec. §2.4.2.1, p.12).
pub const STC_HZ: u64 = 27_000_000;
/// PTS and DTS wrap at 2^33 (Rec. §2.4.3.7, p.38).
pub const TS_MODULUS: u64 = 1 << 33;
/// PCR values wrap at 2^33 · 300 (Rec. eq. 2-1, p.13).
pub const PCR_MODULUS: u64 = TS_MODULUS * 300;

/// A system clock time: 27 MHz ticks, unwrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Stc(pub u64);

impl Stc {
    /// The PCR that encodes this time.
    pub const fn pcr(self) -> Pcr {
        Pcr(self.0 % PCR_MODULUS)
    }

    /// The 90 kHz time stamp of this time, rounded down (Rec. §2.4.3.7: "DIV 300").
    pub const fn ts90k(self) -> Ts90k {
        Ts90k((self.0 / 300) % TS_MODULUS)
    }

    /// Ticks from `earlier` to `self`, saturating at zero.
    pub const fn since(self, earlier: Stc) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// This time plus `ticks`.
    #[must_use]
    pub const fn plus(self, ticks: u64) -> Stc {
        Stc(self.0 + ticks)
    }
}

/// A program clock reference: 27 MHz modulo 2^33 · 300.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pcr(u64);

impl Pcr {
    /// A PCR from base (33 bits, 90 kHz) and extension (0..300).
    pub const fn from_parts(base: u64, ext: u16) -> Option<Self> {
        if base >= TS_MODULUS || ext >= 300 {
            None
        } else {
            Some(Pcr(base * 300 + ext as u64))
        }
    }

    /// The value in 27 MHz ticks, below [`PCR_MODULUS`].
    pub const fn ticks(self) -> u64 {
        self.0
    }

    /// `program_clock_reference_base` (Rec. eq. 2-2).
    pub const fn base(self) -> u64 {
        self.0 / 300
    }

    /// `program_clock_reference_extension` (Rec. eq. 2-3).
    pub const fn ext(self) -> u16 {
        (self.0 % 300) as u16
    }

    /// The six bytes of the adaptation field (Rec. Table 2-6, p.23): 33 bits of base, 6
    /// reserved bits set to one, 9 bits of extension.
    pub const fn to_bytes(self) -> [u8; 6] {
        let b = self.base();
        let e = self.ext();
        [
            (b >> 25) as u8,
            (b >> 17) as u8,
            (b >> 9) as u8,
            (b >> 1) as u8,
            (((b & 1) as u8) << 7) | 0x7E | ((e >> 8) as u8 & 1),
            e as u8,
        ]
    }

    /// The PCR of six adaptation-field bytes (the reserved bits are ignored).
    pub const fn from_bytes(p: [u8; 6]) -> Self {
        let base = ((p[0] as u64) << 25)
            | ((p[1] as u64) << 17)
            | ((p[2] as u64) << 9)
            | ((p[3] as u64) << 1)
            | ((p[4] as u64) >> 7);
        let ext = (((p[4] & 1) as u64) << 8) | p[5] as u64;
        // ext may read as 300..511 from a damaged stream; keep it inside the modulus.
        Pcr((base * 300 + ext) % PCR_MODULUS)
    }
}

/// A 33-bit 90 kHz time stamp (PTS or DTS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ts90k(u64);

impl Ts90k {
    /// The stamp of `v` modulo 2^33.
    pub const fn wrap(v: u64) -> Self {
        Ts90k(v % TS_MODULUS)
    }

    /// The stamp of a signed 90 kHz time, modulo 2^33.
    pub const fn wrap_i64(v: i64) -> Self {
        Ts90k(v.rem_euclid(TS_MODULUS as i64) as u64)
    }

    /// The 33-bit value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The five bytes of a PES header time stamp field with the 4-bit `prefix` ('0010' PTS
    /// alone, '0011' PTS before a DTS, '0001' DTS; Rec. Table 2-21, p.32) and its three
    /// marker bits.
    pub const fn to_pes_bytes(self, prefix: u8) -> [u8; 5] {
        let t = self.0;
        [
            ((prefix & 0x0F) << 4) | (((t >> 30) as u8 & 0x07) << 1) | 1,
            (t >> 22) as u8,
            (((t >> 15) as u8) << 1) | 1,
            (t >> 7) as u8,
            ((t as u8) << 1) | 1,
        ]
    }

    /// The stamp in five PES header bytes; `None` if a marker bit is zero.
    pub const fn from_pes_bytes(b: [u8; 5]) -> Option<Self> {
        if b[0] & 1 == 0 || b[2] & 1 == 0 || b[4] & 1 == 0 {
            return None;
        }
        let t = (((b[0] >> 1) as u64 & 0x07) << 30)
            | ((b[1] as u64) << 22)
            | (((b[2] >> 1) as u64) << 15)
            | ((b[3] as u64) << 7)
            | ((b[4] >> 1) as u64);
        Some(Ts90k(t))
    }

    /// Ticks from `earlier` to `self`, modulo 2^33 (so a wrap between them is handled).
    pub const fn since(self, earlier: Ts90k) -> u64 {
        (self.0 + TS_MODULUS - earlier.0) % TS_MODULUS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcr_round_trip() {
        for t in [
            0u64,
            1,
            299,
            300,
            27_000_000 * 3600 * 26 + 12345,
            PCR_MODULUS - 1,
        ] {
            let p = Stc(t).pcr();
            assert_eq!(Pcr::from_bytes(p.to_bytes()), p);
            assert_eq!(p.base() * 300 + u64::from(p.ext()), t % PCR_MODULUS);
            // Reserved bits are ones.
            assert_eq!(p.to_bytes()[4] & 0x7E, 0x7E);
        }
    }

    #[test]
    fn pcr_wraps() {
        assert_eq!(Stc(PCR_MODULUS + 5).pcr().ticks(), 5);
    }

    #[test]
    fn ts_round_trip_and_markers() {
        for v in [0u64, 1, 0x1_FFFF_FFFF, 0x1_2345_6789, 90_000] {
            for prefix in [0b0010, 0b0011, 0b0001] {
                let b = Ts90k::wrap(v).to_pes_bytes(prefix);
                assert_eq!(b[0] >> 4, prefix);
                assert_eq!(Ts90k::from_pes_bytes(b), Some(Ts90k::wrap(v)));
            }
        }
        let mut b = Ts90k::wrap(5).to_pes_bytes(2);
        b[2] &= 0xFE;
        assert_eq!(Ts90k::from_pes_bytes(b), None);
    }

    #[test]
    fn ts_since_wraps() {
        assert_eq!(Ts90k::wrap(3).since(Ts90k::wrap(TS_MODULUS - 2)), 5);
        assert_eq!(Ts90k::wrap_i64(-1).get(), TS_MODULUS - 1);
    }
}
