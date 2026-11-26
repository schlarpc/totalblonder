//! Transport stream packets (Rec. §2.4.3.2, Table 2-2, p.21; adaptation field Table 2-6, p.23).
//!
//! Writing: [`Packet::payload`] takes as much payload as fits and fills the rest the way the
//! payload type requires ([`Fill`]); [`Packet::adaptation_only`] makes a packet with no
//! payload (a PCR alone); [`Packet::null`] a null packet. The header fields a mux must never
//! set (transport_error_indicator, transport_priority, scrambling) are always zero.
//!
//! Reading: [`View::parse`] checks the parts of the syntax that would make the other fields
//! meaningless (sync byte, reserved adaptation_field_control, adaptation_field_length bounds)
//! and gives access to the rest.

use crate::pid::{Cc, Pid};
use crate::time::Pcr;

/// Bytes in a transport stream packet.
pub const PACKET_LEN: usize = 188;
/// Payload bytes of a packet without adaptation field.
pub const MAX_PAYLOAD: usize = 184;
const SYNC: u8 = 0x47;

/// One 188-byte packet.
#[derive(Clone, PartialEq, Eq)]
pub struct Packet(pub [u8; PACKET_LEN]);

impl std::fmt::Debug for Packet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Packet({:02x?}..)", &self.0[..8])
    }
}

/// Flags and PCR of an adaptation field. All false and no PCR means "no adaptation field
/// needed" (one may still be written for stuffing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Adaptation {
    /// `discontinuity_indicator` (Rec. §2.4.3.5, p.24). Our output never sets it
    /// (`notes/standards-digest.md` §1.8); it exists for completeness and tests.
    pub discontinuity: bool,
    /// `random_access_indicator`: the next PES starting on this PID begins with an access
    /// point (p.25).
    pub random_access: bool,
    /// `elementary_stream_priority_indicator` (p.26).
    pub es_priority: bool,
    /// A program clock reference.
    pub pcr: Option<Pcr>,
}

impl Adaptation {
    /// Adaptation field with a PCR only.
    pub const fn pcr(pcr: Pcr) -> Self {
        Adaptation {
            discontinuity: false,
            random_access: false,
            es_priority: false,
            pcr: Some(pcr),
        }
    }

    fn needed(&self) -> bool {
        self.discontinuity || self.random_access || self.es_priority || self.pcr.is_some()
    }

    /// Bytes the field needs, including `adaptation_field_length` and the flags byte; 0 when
    /// nothing is set.
    pub fn len(&self) -> usize {
        if !self.needed() {
            0
        } else if self.pcr.is_some() {
            8
        } else {
            2
        }
    }

    /// True when the field carries nothing.
    pub fn is_empty(&self) -> bool {
        !self.needed()
    }

    fn flags(&self) -> u8 {
        (u8::from(self.discontinuity) << 7)
            | (u8::from(self.random_access) << 6)
            | (u8::from(self.es_priority) << 5)
            | (u8::from(self.pcr.is_some()) << 4)
    }
}

/// How the room left after a short payload is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    /// Stuffing bytes in the adaptation field: the only stuffing allowed in packets carrying
    /// PES data (Rec. §2.4.3.5, p.24 and p.32).
    Adaptation,
    /// 0xFF bytes after the payload: how a section PID ends a packet after a section
    /// (Rec. §2.4.4, p.45; Annex C.4, p.163). PSI and PSIP packets never carry an adaptation
    /// field (`notes/standards-digest.md` §1.1).
    PayloadFf,
}

fn header(b: &mut [u8; PACKET_LEN], pid: Pid, pusi: bool, afc: u8, cc: Cc) {
    let p = pid.get();
    b[0] = SYNC;
    b[1] = (u8::from(pusi) << 6) | ((p >> 8) as u8 & 0x1F);
    b[2] = p as u8;
    b[3] = (afc << 4) | cc.get();
}

/// Writes an adaptation field of exactly `total` bytes (length byte included) at `b[4..]`.
/// `total` is at least `af.len()`, at least 1, and at most 184.
fn write_af(b: &mut [u8; PACKET_LEN], af: &Adaptation, total: usize) {
    debug_assert!(total >= af.len().max(1) && total <= MAX_PAYLOAD);
    // adaptation_field_length excludes itself; at most 183 (Rec. p.24).
    b[4] = (total - 1) as u8;
    if total == 1 {
        return;
    }
    b[5] = af.flags();
    let mut i = 6;
    if let Some(p) = af.pcr {
        b[6..12].copy_from_slice(&p.to_bytes());
        i = 12;
    }
    b[i..4 + total].fill(0xFF);
}

impl Packet {
    /// A packet of `pid` carrying the front of `payload`: as many bytes as fit after the
    /// adaptation field `af` (if any). Returns the packet and the number of payload bytes
    /// taken (at least 1 when `payload` is not empty).
    ///
    /// When `payload` is shorter than the room, the packet is completed as `fill` says. With
    /// [`Fill::PayloadFf`] an `af` that is not empty is refused (`None`): section PIDs carry
    /// no adaptation field.
    pub fn payload(
        pid: Pid,
        pusi: bool,
        cc: Cc,
        af: &Adaptation,
        payload: &[u8],
        fill: Fill,
    ) -> Option<(Packet, usize)> {
        if payload.is_empty() || (fill == Fill::PayloadFf && !af.is_empty()) {
            return None;
        }
        let mut b = [0xFFu8; PACKET_LEN];
        let af_min = af.len();
        let room = MAX_PAYLOAD - af_min;
        let n = payload.len().min(room);
        let af_total = match fill {
            Fill::Adaptation => MAX_PAYLOAD - n,
            Fill::PayloadFf => 0,
        };
        let afc = if af_total == 0 { 0b01 } else { 0b11 };
        header(&mut b, pid, pusi, afc, cc);
        if af_total > 0 {
            write_af(&mut b, af, af_total);
        }
        let start = 4 + af_total;
        b[start..start + n].copy_from_slice(&payload[..n]);
        Some((Packet(b), n))
    }

    /// A packet with an adaptation field and no payload (adaptation_field_control '10',
    /// adaptation_field_length 183). Its continuity counter is the one of the last packet with
    /// payload on the PID: it does not advance (Rec. §2.4.3.3, p.22), so pass the value
    /// last written (`next.prev()` of the counter a mux keeps).
    pub fn adaptation_only(pid: Pid, cc_last: Cc, af: &Adaptation) -> Packet {
        let mut b = [0xFFu8; PACKET_LEN];
        header(&mut b, pid, false, 0b10, cc_last);
        write_af(&mut b, af, MAX_PAYLOAD.max(af.len()));
        Packet(b)
    }

    /// A null packet: PID 0x1FFF, payload only, 184 bytes of 0xFF
    /// (`notes/standards-digest.md` §1.5).
    pub const fn null() -> Packet {
        let mut b = [0xFFu8; PACKET_LEN];
        b[0] = SYNC;
        b[1] = 0x1F;
        b[2] = 0xFF;
        b[3] = 0x10;
        Packet(b)
    }

    /// The packet read back.
    pub fn view(&self) -> Result<View<'_>, ParseError> {
        View::parse(&self.0)
    }
}

impl Cc {
    /// The value before this one, modulo 16.
    #[must_use]
    pub const fn prev(self) -> Self {
        Cc::new(self.get().wrapping_add(15))
    }
}

/// Why a packet could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The first byte is not 0x47.
    #[error("sync byte {0:#04x}")]
    Sync(u8),
    /// adaptation_field_control '00' (reserved, Rec. Table 2-5, p.22).
    #[error("reserved adaptation_field_control")]
    ReservedAfc,
    /// adaptation_field_length out of range for the control value (p.24).
    #[error("adaptation_field_length {0} out of range")]
    AfLength(u8),
}

/// A parsed packet borrowed from its bytes.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    b: &'a [u8; PACKET_LEN],
    /// Index of the first payload byte (188 when there is none).
    payload_at: usize,
}

impl<'a> View<'a> {
    /// Reads the header and the adaptation field's length.
    pub fn parse(b: &'a [u8; PACKET_LEN]) -> Result<Self, ParseError> {
        if b[0] != SYNC {
            return Err(ParseError::Sync(b[0]));
        }
        let afc = (b[3] >> 4) & 3;
        let payload_at = match afc {
            0b00 => return Err(ParseError::ReservedAfc),
            0b01 => 4,
            0b10 => {
                if b[4] != 183 {
                    return Err(ParseError::AfLength(b[4]));
                }
                PACKET_LEN
            }
            _ => {
                if b[4] > 182 {
                    return Err(ParseError::AfLength(b[4]));
                }
                5 + usize::from(b[4])
            }
        };
        Ok(View { b, payload_at })
    }

    /// `transport_error_indicator`.
    pub fn tei(&self) -> bool {
        self.b[1] & 0x80 != 0
    }
    /// `payload_unit_start_indicator`.
    pub fn pusi(&self) -> bool {
        self.b[1] & 0x40 != 0
    }
    /// `transport_priority`.
    pub fn priority(&self) -> bool {
        self.b[1] & 0x20 != 0
    }
    /// The PID.
    pub fn pid(&self) -> Pid {
        Pid::from_bits(u16::from_be_bytes([self.b[1], self.b[2]]))
    }
    /// `transport_scrambling_control`.
    pub fn scrambling(&self) -> u8 {
        self.b[3] >> 6
    }
    /// `adaptation_field_control` (1, 2 or 3).
    pub fn afc(&self) -> u8 {
        (self.b[3] >> 4) & 3
    }
    /// `continuity_counter`.
    pub fn cc(&self) -> Cc {
        Cc::new(self.b[3])
    }
    /// The packet carries payload (AFC 01 or 11).
    pub fn has_payload(&self) -> bool {
        self.afc() & 1 == 1
    }
    /// The payload bytes (empty for AFC 10).
    pub fn payload(&self) -> &'a [u8] {
        &self.b[self.payload_at..]
    }
    /// The adaptation field bytes after `adaptation_field_length` (flags onward), if any.
    pub fn adaptation(&self) -> Option<&'a [u8]> {
        if self.afc() & 2 == 0 {
            return None;
        }
        Some(&self.b[5..5 + usize::from(self.b[4])])
    }
    /// The adaptation field's flags byte, if the field has one.
    pub fn af_flags(&self) -> Option<u8> {
        self.adaptation().and_then(|a| a.first().copied())
    }
    /// The PCR, if the adaptation field carries one.
    pub fn pcr(&self) -> Option<Pcr> {
        let a = self.adaptation()?;
        if a.len() >= 7 && a[0] & 0x10 != 0 {
            let mut p = [0u8; 6];
            p.copy_from_slice(&a[1..7]);
            Some(Pcr::from_bytes(p))
        } else {
            None
        }
    }
    /// `discontinuity_indicator`.
    pub fn discontinuity(&self) -> bool {
        self.af_flags().is_some_and(|f| f & 0x80 != 0)
    }
    /// `random_access_indicator`.
    pub fn random_access(&self) -> bool {
        self.af_flags().is_some_and(|f| f & 0x40 != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Stc;

    fn pid(v: u16) -> Pid {
        Pid::new(v).unwrap()
    }

    #[test]
    fn full_payload_packet() {
        let data = [7u8; 300];
        let (p, n) = Packet::payload(
            pid(0x31),
            true,
            Cc::new(3),
            &Adaptation::default(),
            &data,
            Fill::Adaptation,
        )
        .unwrap();
        assert_eq!(n, 184);
        let v = p.view().unwrap();
        assert_eq!(
            (v.pid(), v.pusi(), v.cc().get(), v.afc()),
            (pid(0x31), true, 3, 1)
        );
        assert_eq!(v.payload(), &data[..184]);
    }

    #[test]
    fn short_payload_is_stuffed_in_the_adaptation_field_for_every_length() {
        for len in 1..=184usize {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let (p, n) = Packet::payload(
                pid(0x31),
                false,
                Cc::new(0),
                &Adaptation::default(),
                &data,
                Fill::Adaptation,
            )
            .unwrap();
            assert_eq!(n, len);
            let v = p.view().unwrap();
            assert_eq!(v.payload(), &data[..]);
            if len < 184 {
                assert_eq!(v.afc(), 3);
                // A one-byte field is the length byte alone; otherwise flags 0 and 0xFF.
                let a = v.adaptation().unwrap();
                if let Some((f, rest)) = a.split_first() {
                    assert_eq!(*f, 0);
                    assert!(rest.iter().all(|&x| x == 0xFF));
                }
            }
        }
    }

    #[test]
    fn pcr_and_flags_with_payload() {
        let pcr = Stc(123_456_789_012).pcr();
        let af = Adaptation {
            random_access: true,
            ..Adaptation::pcr(pcr)
        };
        let data = [1u8; 400];
        let (p, n) =
            Packet::payload(pid(0x41), true, Cc::new(9), &af, &data, Fill::Adaptation).unwrap();
        assert_eq!(n, 176);
        let v = p.view().unwrap();
        assert_eq!(v.pcr(), Some(pcr));
        assert!(v.random_access());
        assert!(!v.discontinuity());
        // The last bit of the PCR base is in byte 10 (SPEC §5.1's "+ 10").
        assert_eq!(p.0[10] >> 7, (pcr.base() & 1) as u8);
    }

    #[test]
    fn adaptation_only_packet() {
        let pcr = Stc(42).pcr();
        let p = Packet::adaptation_only(pid(0x41), Cc::new(5), &Adaptation::pcr(pcr));
        let v = p.view().unwrap();
        assert_eq!((v.afc(), v.cc().get(), v.has_payload()), (2, 5, false));
        assert_eq!(p.0[4], 183);
        assert_eq!(v.pcr(), Some(pcr));
        assert!(v.payload().is_empty());
    }

    #[test]
    fn section_fill_refuses_adaptation() {
        let af = Adaptation::pcr(Stc(0).pcr());
        assert!(Packet::payload(pid(0), true, Cc::new(0), &af, &[0], Fill::PayloadFf).is_none());
        let (p, n) = Packet::payload(
            pid(0),
            true,
            Cc::new(0),
            &Adaptation::default(),
            &[0, 1, 2],
            Fill::PayloadFf,
        )
        .unwrap();
        assert_eq!(n, 3);
        assert_eq!(p.view().unwrap().afc(), 1);
        assert!(p.0[7..].iter().all(|&x| x == 0xFF));
    }

    #[test]
    fn null_packet() {
        let p = Packet::null();
        let v = p.view().unwrap();
        assert_eq!(v.pid(), Pid::NULL);
        assert!(!v.pusi());
        assert_eq!(v.afc(), 1);
    }

    #[test]
    fn parse_rejects() {
        let mut b = Packet::null().0;
        b[0] = 0x46;
        assert_eq!(View::parse(&b).unwrap_err(), ParseError::Sync(0x46));
        let mut b = Packet::null().0;
        b[3] = 0x00;
        assert_eq!(View::parse(&b).unwrap_err(), ParseError::ReservedAfc);
        let mut b = Packet::null().0;
        b[3] = 0x30;
        b[4] = 183;
        assert_eq!(View::parse(&b).unwrap_err(), ParseError::AfLength(183));
    }

    #[test]
    fn cc_prev() {
        assert_eq!(Cc::new(0).prev().get(), 15);
        assert_eq!(Cc::new(7).prev().get(), 6);
    }
}
