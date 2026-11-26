//! PES packets (H.222.0 §2.4.3.6, Table 2-21; semantics §2.4.3.7).

/// A parsed PES header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PesHeader {
    /// stream_id.
    pub stream_id: u8,
    /// PES_packet_length (0: unbounded, video only).
    pub packet_length: u16,
    /// PTS, 33 bits.
    pub pts: Option<u64>,
    /// DTS, 33 bits.
    pub dts: Option<u64>,
    /// Offset of the payload from the start of the PES packet.
    pub payload_offset: usize,
}

/// Why a PES packet cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PesError {
    /// No packet_start_code_prefix 0x000001.
    #[error("no PES start code")]
    StartCode,
    /// The header is longer than the data.
    #[error("PES header truncated")]
    Truncated,
}

/// stream_ids with no optional PES header (Table 2-21: program_stream_map, padding_stream,
/// private_stream_2, ECM, EMM, program_stream_directory, DSMCC_stream, H.222.1 type E).
fn has_optional_header(id: u8) -> bool {
    !matches!(id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xFF | 0xF2 | 0xF8)
}

/// A 33-bit timestamp from its 5-byte form (§2.4.3.7: 4 prefix bits, 3 bits, marker, 15 bits,
/// marker, 15 bits, marker). Marker bits are not checked (some muxers get them wrong).
fn timestamp(b: &[u8]) -> u64 {
    (u64::from(b[0] >> 1) & 0x07) << 30
        | u64::from(b[1]) << 22
        | (u64::from(b[2]) >> 1) << 15
        | u64::from(b[3]) << 7
        | u64::from(b[4]) >> 1
}

impl PesHeader {
    /// Parse the header at the start of `p`.
    pub fn parse(p: &[u8]) -> Result<Self, PesError> {
        if p.len() < 6 {
            return Err(PesError::Truncated);
        }
        if p[..3] != [0, 0, 1] {
            return Err(PesError::StartCode);
        }
        let stream_id = p[3];
        let packet_length = u16::from_be_bytes([p[4], p[5]]);
        if !has_optional_header(stream_id) {
            return Ok(Self {
                stream_id,
                packet_length,
                pts: None,
                dts: None,
                payload_offset: 6,
            });
        }
        let h = p.get(6..9).ok_or(PesError::Truncated)?;
        let flags = h[1] >> 6;
        let hlen = usize::from(h[2]);
        let opt = p.get(9..9 + hlen).ok_or(PesError::Truncated)?;
        let (pts, dts) = match flags {
            2 if opt.len() >= 5 => (Some(timestamp(&opt[..5])), None),
            3 if opt.len() >= 10 => (Some(timestamp(&opt[..5])), Some(timestamp(&opt[5..10]))),
            // 01 is forbidden; a short header carries no usable time.
            _ => (None, None),
        };
        Ok(Self {
            stream_id,
            packet_length,
            pts,
            dts,
            payload_offset: 9 + hlen,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The 5-byte form of `t` with prefix `prefix` (0b0010 PTS only, 0b0011 / 0b0001 PTS+DTS).
    pub(crate) fn ts_bytes(prefix: u8, t: u64) -> [u8; 5] {
        [
            prefix << 4 | ((t >> 29) as u8 & 0x0E) | 1,
            (t >> 22) as u8,
            ((t >> 14) as u8 & 0xFE) | 1,
            (t >> 7) as u8,
            ((t << 1) as u8) | 1,
        ]
    }

    /// A PES packet with PTS (and DTS).
    pub(crate) fn pes(stream_id: u8, pts: u64, dts: Option<u64>, payload: &[u8]) -> Vec<u8> {
        let mut opt = Vec::new();
        let flags = if let Some(d) = dts {
            opt.extend_from_slice(&ts_bytes(3, pts));
            opt.extend_from_slice(&ts_bytes(1, d));
            0xC0
        } else {
            opt.extend_from_slice(&ts_bytes(2, pts));
            0x80
        };
        let len = 3 + opt.len() + payload.len();
        let len = if (0xE0..=0xEF).contains(&stream_id) {
            0
        } else {
            len
        };
        let mut v = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8, 0x80, flags];
        v.push(opt.len() as u8);
        v.extend_from_slice(&opt);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn timestamps_33_bits() {
        for t in [0u64, 1, 90_000, (1 << 32) + 12345, (1 << 33) - 1] {
            let p = pes(0xE0, t, Some(t.saturating_sub(3003)), b"x");
            let h = PesHeader::parse(&p).unwrap();
            assert_eq!(h.pts, Some(t));
            assert_eq!(h.dts, Some(t.saturating_sub(3003)));
            assert_eq!(&p[h.payload_offset..], b"x");
        }
        let p = pes(0xC0, 7, None, b"ab");
        let h = PesHeader::parse(&p).unwrap();
        assert_eq!(
            (h.pts, h.dts, usize::from(h.packet_length)),
            (Some(7), None, p.len() - 6)
        );
        assert_eq!(
            PesHeader::parse(&[0, 0, 2, 0xE0, 0, 0]),
            Err(PesError::StartCode)
        );
        assert_eq!(
            PesHeader::parse(&[0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 20]),
            Err(PesError::Truncated)
        );
        let h = PesHeader::parse(&[0, 0, 1, 0xBE, 0, 2, 0xFF, 0xFF]).unwrap();
        assert_eq!(h.payload_offset, 6);
    }
}
