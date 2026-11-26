//! Datagram framing (SPEC §4.1), pure.
//!
//! A datagram carries `packets_per_datagram` consecutive slots of one carrier (7 by default:
//! 1,316 bytes). With [`Framing::Rtp`] a 12-byte RTP header (RFC 3550 §5.1) comes first:
//! version 2, no padding, extension or CSRC, marker 0, payload type 33 (MP2T, RFC 3551
//! Table 5), a sequence number that increments by one per datagram, and a 90 kHz timestamp
//! taken from the STC of the datagram's first slot (RFC 2250 §2: "the timestamp ... is the
//! target transmission time of the first byte of the packet payload").

use tb_ts::PACKET_LEN;

/// How datagrams are framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Framing {
    /// The TS packets are the UDP payload.
    Udp,
    /// An RTP header in front, with this SSRC.
    Rtp {
        /// Synchronization source identifier.
        ssrc: u32,
    },
}

/// Packets per datagram, 1..=7 (7 × 188 = 1,316 bytes fits a 1,500-byte MTU with RTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PerDatagram(u8);

impl PerDatagram {
    /// The usual seven.
    pub const SEVEN: PerDatagram = PerDatagram(7);
    /// `n` if in 1..=7.
    pub const fn new(n: u8) -> Option<Self> {
        if n >= 1 && n <= 7 {
            Some(Self(n))
        } else {
            None
        }
    }
    /// The count.
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// Bytes of an RTP header without CSRC or extension.
pub const RTP_HEADER: usize = 12;
/// Largest datagram: RTP header and seven packets.
pub const MAX_DATAGRAM: usize = RTP_HEADER + 7 * PACKET_LEN;

/// Writes the datagram for `packets` into `out` and returns its length.
/// `seq` is the datagram's sequence number, `ts90k` its first slot's STC in 90 kHz units.
pub fn datagram(
    framing: Framing,
    seq: u16,
    ts90k: u32,
    packets: &[[u8; PACKET_LEN]],
    out: &mut [u8; MAX_DATAGRAM],
) -> usize {
    let mut n = 0;
    if let Framing::Rtp { ssrc } = framing {
        out[0] = 0x80; // V=2, P=0, X=0, CC=0
        out[1] = 33; // M=0, PT=33
        out[2..4].copy_from_slice(&seq.to_be_bytes());
        out[4..8].copy_from_slice(&ts90k.to_be_bytes());
        out[8..12].copy_from_slice(&ssrc.to_be_bytes());
        n = RTP_HEADER;
    }
    for p in packets.iter().take(7) {
        out[n..n + PACKET_LEN].copy_from_slice(p);
        n += PACKET_LEN;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_header() {
        let mut out = [0u8; MAX_DATAGRAM];
        let p = tb_ts::Packet::null().0;
        let n = datagram(
            Framing::Rtp { ssrc: 0x1234_5678 },
            0xBEEF,
            90_000,
            &[p; 7],
            &mut out,
        );
        assert_eq!(n, 12 + 1316);
        assert_eq!(
            &out[..12],
            &[
                0x80, 33, 0xBE, 0xEF, 0, 1, 0x5F, 0x90, 0x12, 0x34, 0x56, 0x78
            ]
        );
        assert_eq!(out[12], 0x47);
        let n = datagram(Framing::Udp, 0, 0, &[p; 7], &mut out);
        assert_eq!(n, 1316);
        assert_eq!(out[0], 0x47);
    }

    #[test]
    fn per_datagram_range() {
        assert!(PerDatagram::new(0).is_none());
        assert!(PerDatagram::new(8).is_none());
        assert_eq!(PerDatagram::SEVEN.get(), 7);
    }
}
