//! Program specific information: sections (H.222.0 §2.4.4.1–2.4.4.2, pointer_field), the
//! program association table (§2.4.4.3, Table 2-30), the program map table (§2.4.4.8,
//! Table 2-33) and the descriptors a receiver needs (§2.6).

use crate::packet::Pid;

/// The CRC of MPEG-2 sections (H.222.0 Annex A): polynomial 0x04C11DB7, initial value
/// 0xFFFFFFFF, no reflection, no final XOR. Over a whole section including its CRC_32 field the
/// result is 0.
pub fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Largest section: section_length is at most 1021 for PSI (§2.4.4.5, §2.4.4.9), so 1024
/// bytes with the 3-byte header.
pub const MAX_SECTION: usize = 1024;

/// Reassembles sections on one PID (§2.4.4.1: with payload_unit_start_indicator set, the first
/// payload byte is pointer_field, the number of bytes that finish the previous section).
#[derive(Debug, Clone, Default)]
pub struct SectionAssembler {
    buf: Vec<u8>,
    active: bool,
    /// Sections dropped for a bad CRC or length.
    pub bad: u64,
}

impl SectionAssembler {
    /// Feed one packet's payload. `continuous` is false after a continuity error (the partial
    /// section is dropped). Complete sections with a good CRC go to `f`.
    pub fn push(
        &mut self,
        payload: &[u8],
        unit_start: bool,
        continuous: bool,
        mut f: impl FnMut(&[u8]),
    ) {
        if !continuous {
            self.buf.clear();
            self.active = false;
        }
        if unit_start {
            let Some((&ptr, rest)) = payload.split_first() else {
                return;
            };
            let ptr = usize::from(ptr).min(rest.len());
            if self.active {
                self.feed(&rest[..ptr], &mut f);
            }
            self.buf.clear();
            self.active = true;
            self.feed(&rest[ptr..], &mut f);
        } else if self.active {
            self.feed(payload, &mut f);
        }
    }

    fn feed(&mut self, data: &[u8], f: &mut impl FnMut(&[u8])) {
        self.buf.extend_from_slice(data);
        loop {
            if !self.active || self.buf.len() < 3 {
                return;
            }
            if self.buf[0] == 0xFF {
                // Stuffing (§2.4.4.2): the rest of the packet is padding.
                self.buf.clear();
                self.active = false;
                return;
            }
            let len = 3 + (usize::from(self.buf[1] & 0x0F) << 8 | usize::from(self.buf[2]));
            if !(3 + 4 + 5..=MAX_SECTION).contains(&len) {
                self.bad += 1;
                self.buf.clear();
                self.active = false;
                return;
            }
            if self.buf.len() < len {
                return;
            }
            // Long-form sections carry a CRC_32 (all PSI here does).
            if self.buf[1] & 0x80 != 0 && crc32_mpeg2(&self.buf[..len]) == 0 {
                f(&self.buf[..len]);
            } else {
                self.bad += 1;
            }
            self.buf.drain(..len);
        }
    }
}

/// The long-form header of a PSI section (Table 2-30, 2-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionHeader {
    /// table_id.
    pub table_id: u8,
    /// transport_stream_id (PAT) or program_number (PMT).
    pub id: u16,
    /// version_number.
    pub version: u8,
    /// current_next_indicator.
    pub current: bool,
    /// section_number.
    pub number: u8,
    /// last_section_number.
    pub last: u8,
}

/// The header and the body between it and the CRC. `s` is a complete section whose CRC was
/// checked (so its length is at least 12).
pub fn split_section(s: &[u8]) -> Option<(SectionHeader, &[u8])> {
    let body = s.get(8..s.len().checked_sub(4)?)?;
    Some((
        SectionHeader {
            table_id: s[0],
            id: u16::from_be_bytes([s[3], s[4]]),
            version: (s[5] >> 1) & 0x1F,
            current: s[5] & 1 != 0,
            number: s[6],
            last: s[7],
        },
        body,
    ))
}

/// One entry of a PAT: a program and its PMT PID (program_number 0 is the network PID and is
/// left out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatEntry {
    /// program_number.
    pub program: u16,
    /// program_map_PID.
    pub pmt_pid: Pid,
}

/// The programs of a PAT section body (Table 2-30).
pub fn parse_pat(body: &[u8]) -> Vec<PatEntry> {
    body.as_chunks::<4>()
        .0
        .iter()
        .filter_map(|e| {
            let program = u16::from_be_bytes([e[0], e[1]]);
            (program != 0).then_some(PatEntry {
                program,
                pmt_pid: Pid::from_bits(e[2], e[3]),
            })
        })
        .collect()
}

/// A descriptor: tag and body (§2.6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// descriptor_tag.
    pub tag: u8,
    /// The bytes after descriptor_length.
    pub data: Vec<u8>,
}

/// Descriptors in a loop; a truncated last one is dropped.
pub fn parse_descriptors(mut d: &[u8]) -> Vec<Descriptor> {
    let mut v = Vec::new();
    while d.len() >= 2 {
        let len = usize::from(d[1]);
        let Some(data) = d.get(2..2 + len) else {
            break;
        };
        v.push(Descriptor {
            tag: d[0],
            data: data.to_vec(),
        });
        d = &d[2 + len..];
    }
    v
}

/// One elementary stream of a PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmtStream {
    /// stream_type (Table 2-34).
    pub stream_type: u8,
    /// elementary_PID.
    pub pid: Pid,
    /// Its descriptors.
    pub descriptors: Vec<Descriptor>,
}

/// A PMT section body (Table 2-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pmt {
    /// PCR_PID.
    pub pcr_pid: Pid,
    /// Program-level descriptors.
    pub descriptors: Vec<Descriptor>,
    /// Elementary streams in order.
    pub streams: Vec<PmtStream>,
}

/// Parse a PMT body; `None` if its lengths do not fit.
pub fn parse_pmt(body: &[u8]) -> Option<Pmt> {
    let pcr_pid = Pid::from_bits(*body.first()?, *body.get(1)?);
    let info_len = usize::from(body.get(2)? & 0x0F) << 8 | usize::from(*body.get(3)?);
    let descriptors = parse_descriptors(body.get(4..4 + info_len)?);
    let mut rest = &body[4 + info_len..];
    let mut streams = Vec::new();
    while rest.len() >= 5 {
        let es_len = usize::from(rest[3] & 0x0F) << 8 | usize::from(rest[4]);
        let d = rest.get(5..5 + es_len)?;
        streams.push(PmtStream {
            stream_type: rest[0],
            pid: Pid::from_bits(rest[1], rest[2]),
            descriptors: parse_descriptors(d),
        });
        rest = &rest[5 + es_len..];
    }
    Some(Pmt {
        pcr_pid,
        descriptors,
        streams,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Wrap a body into a long-form section with a CRC.
    pub(crate) fn section(table_id: u8, id: u16, version: u8, body: &[u8]) -> Vec<u8> {
        let len = 5 + body.len() + 4;
        let mut s = vec![table_id, 0xB0 | (len >> 8) as u8, len as u8];
        s.extend_from_slice(&id.to_be_bytes());
        s.push(0xC1 | (version << 1));
        s.extend_from_slice(&[0, 0]);
        s.extend_from_slice(body);
        let crc = crc32_mpeg2(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    #[test]
    fn crc_known_value() {
        // The PAT ffmpeg writes for program 1 on PID 0x1000, tsid 1: CRC 2AB104B2.
        let s = [
            0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xF0, 0x00,
        ];
        assert_eq!(crc32_mpeg2(&s), 0x2AB1_04B2);
        let mut full = s.to_vec();
        full.extend_from_slice(&0x2AB1_04B2u32.to_be_bytes());
        assert_eq!(crc32_mpeg2(&full), 0);
    }

    #[test]
    fn sections_across_packets_with_pointer_field() {
        let pat = section(0, 1, 3, &[0, 1, 0xF0, 0x00, 0, 2, 0xE1, 0x00]);
        let mut a = SectionAssembler::default();
        let mut got = Vec::new();
        // First packet: pointer 0, the first 5 bytes.
        let mut p1 = vec![0];
        p1.extend_from_slice(&pat[..5]);
        a.push(&p1, true, true, |s| got.push(s.to_vec()));
        assert!(got.is_empty());
        // Second packet starts a new section after finishing this one.
        let mut p2 = vec![(pat.len() - 5) as u8];
        p2.extend_from_slice(&pat[5..]);
        p2.extend_from_slice(&pat);
        p2.extend_from_slice(&[0xFF; 10]);
        a.push(&p2, true, true, |s| got.push(s.to_vec()));
        assert_eq!(got, [pat.clone(), pat.clone()]);
        let (h, body) = split_section(&got[0]).unwrap();
        assert_eq!((h.table_id, h.id, h.version, h.current), (0, 1, 3, true));
        let e = parse_pat(body);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].pmt_pid.get(), 0x1000);
        // A corrupted section is counted, not delivered.
        let mut bad = pat.clone();
        bad[9] ^= 1;
        let mut p = vec![0];
        p.extend_from_slice(&bad);
        a.push(&p, true, true, |s| got.push(s.to_vec()));
        assert_eq!(got.len(), 2);
        assert_eq!(a.bad, 1);
    }

    #[test]
    fn pmt_with_descriptors() {
        let body = [
            0xE1, 0x00, // PCR PID 0x100
            0xF0, 0x06, 0x05, 0x04, b'A', b'C', b'-', b'3', // registration
            0x1B, 0xE1, 0x00, 0xF0, 0x00, // H.264 on 0x100
            0x81, 0xE1, 0x01, 0xF0, 0x06, 0x0A, 0x04, b'e', b'n', b'g', 0x00, // AC-3, eng
        ];
        let p = parse_pmt(&body).unwrap();
        assert_eq!(p.pcr_pid.get(), 0x100);
        assert_eq!(p.descriptors[0].tag, 5);
        assert_eq!(p.streams.len(), 2);
        assert_eq!(p.streams[1].stream_type, 0x81);
        assert_eq!(p.streams[1].descriptors[0].data, b"eng\0");
        assert!(parse_pmt(&body[..8]).is_none());
    }
}
