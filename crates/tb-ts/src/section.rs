//! Sections: the long form of Rec. §2.4.4.10–2.4.4.11 (Table 2-30 onward, p.47–59) as used by
//! PSI and by ATSC PSIP (A/65 §4, §6), their packetization, and their reassembly.
//!
//! Writing: [`Section::long`] computes `section_length` and the CRC, and refuses a section
//! longer than its kind allows (1,024 bytes for PSI, 4,096 for private sections such as PSIP;
//! Rec. §2.4.4.5 p.48, §2.4.4.11 p.58). [`Cursor`] cuts sections into packets: each section
//! starts a packet with `pointer_field` 0, and the packet that ends a section is completed with
//! 0xFF (Rec. p.45; Annex C.4 p.163), so no packet on a section PID carries an adaptation
//! field. Starting every section at a packet boundary costs a little bandwidth and spares
//! receivers the packed case; the MGT must start a packet anyway (A/65 §6.2).
//!
//! Reading: [`Assembler`] joins the sections of one PID from its packets; [`Parsed::parse`]
//! checks a long-form section's length and CRC.

use crate::crc::crc32_mpeg2;
use crate::packet::{Adaptation, Fill, MAX_PAYLOAD, Packet, View};
use crate::pid::{Cc, Pid};

/// A `table_id`; 0xFF is forbidden because it marks stuffing (Rec. Table 2-31, p.48).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TableId(u8);

impl TableId {
    /// program_association_section.
    pub const PAT: TableId = TableId(0x00);
    /// TS_program_map_section.
    pub const PMT: TableId = TableId(0x02);
    /// A/65 master guide table.
    pub const MGT: TableId = TableId(0xC7);
    /// A/65 terrestrial virtual channel table.
    pub const TVCT: TableId = TableId(0xC8);
    /// A/65 cable virtual channel table.
    pub const CVCT: TableId = TableId(0xC9);
    /// A/65 rating region table.
    pub const RRT: TableId = TableId(0xCA);
    /// A/65 event information table.
    pub const EIT: TableId = TableId(0xCB);
    /// A/65 extended text table.
    pub const ETT: TableId = TableId(0xCC);
    /// A/65 system time table.
    pub const STT: TableId = TableId(0xCD);

    /// `v`, unless it is 0xFF.
    pub const fn new(v: u8) -> Option<Self> {
        if v == 0xFF { None } else { Some(TableId(v)) }
    }

    /// The byte.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A 5-bit `version_number`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Version(u8);

impl Version {
    /// Version `v` modulo 32.
    pub const fn new(v: u8) -> Self {
        Version(v & 0x1F)
    }
    /// The value.
    pub const fn get(self) -> u8 {
        self.0
    }
    /// The next version, modulo 32 (Rec. §2.4.4.5, p.49).
    #[must_use]
    pub const fn next(self) -> Self {
        Version((self.0 + 1) & 0x1F)
    }
}

/// Which size limit and `private_indicator` a section has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PAT, PMT, CAT: at most 1,024 bytes, `'0'` after `section_syntax_indicator`
    /// (Rec. Table 2-30 and 2-33).
    Psi,
    /// A private section (Rec. Table 2-38, p.58) such as every A/65 table: at most 4,096
    /// bytes, `private_indicator` 1 (A/65 Table 6.1 onward).
    Private,
    /// A private section limited to 1,024 bytes: the A/65 STT, TVCT, CVCT and RRT
    /// (`section_length` at most 1021; A/65 §6.1, §6.3.1, §6.4).
    Private1024,
}

impl Kind {
    /// The largest whole section, bytes.
    pub const fn max_len(self) -> usize {
        match self {
            Kind::Psi | Kind::Private1024 => 1024,
            Kind::Private => 4096,
        }
    }
}

/// The fields of a long-form section header after `section_length`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Long {
    /// `table_id`.
    pub table_id: TableId,
    /// `transport_stream_id`, `program_number` or the table's own extension.
    pub extension: u16,
    /// `version_number`.
    pub version: Version,
    /// `current_next_indicator`.
    pub current: bool,
    /// `section_number`.
    pub number: u8,
    /// `last_section_number`.
    pub last: u8,
}

/// A section that cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SectionError {
    /// Longer than its kind allows.
    #[error("section of {len} bytes exceeds {max}")]
    TooLong {
        /// Bytes it would have.
        len: usize,
        /// Bytes allowed.
        max: usize,
    },
    /// `section_number` above `last_section_number`.
    #[error("section_number {0} above last_section_number {1}")]
    Number(u8, u8),
}

/// A complete section, CRC included: valid by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Section(Vec<u8>);

/// Bytes of a long-form header (up to `last_section_number`).
pub const LONG_HEADER: usize = 8;

impl Section {
    /// A long-form section with `body` between the header and the CRC.
    pub fn long(kind: Kind, h: Long, body: &[u8]) -> Result<Section, SectionError> {
        if h.number > h.last {
            return Err(SectionError::Number(h.number, h.last));
        }
        let len = LONG_HEADER + body.len() + 4;
        if len > kind.max_len() {
            return Err(SectionError::TooLong {
                len,
                max: kind.max_len(),
            });
        }
        let section_length = (len - 3) as u16;
        let private = !matches!(kind, Kind::Psi);
        let mut v = Vec::with_capacity(len);
        v.push(h.table_id.get());
        // section_syntax_indicator 1, private_indicator, reserved '11', 12 bits of length.
        v.push(0x80 | (u8::from(private) << 6) | 0x30 | ((section_length >> 8) as u8 & 0x0F));
        v.push(section_length as u8);
        v.extend_from_slice(&h.extension.to_be_bytes());
        v.push(0xC0 | (h.version.get() << 1) | u8::from(h.current));
        v.push(h.number);
        v.push(h.last);
        v.extend_from_slice(body);
        let crc = crc32_mpeg2(&v);
        v.extend_from_slice(&crc.to_be_bytes());
        Ok(Section(v))
    }

    /// The bytes, CRC included.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Total length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Never true: a section has at least its header.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `table_id`.
    pub fn table_id(&self) -> u8 {
        self.0[0]
    }

    /// Transport packets this section needs when it starts a packet (`pointer_field` 0).
    pub fn packets(&self) -> usize {
        (self.0.len() + 1).div_ceil(MAX_PAYLOAD)
    }
}

impl AsRef<[u8]> for Section {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A long-form section read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed<'a> {
    /// The header fields.
    pub header: Long,
    /// `private_indicator`.
    pub private: bool,
    /// The bytes between header and CRC.
    pub body: &'a [u8],
}

/// Why a section could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Shorter than its own length field says, or than a long header.
    #[error("truncated section")]
    Truncated,
    /// `section_syntax_indicator` 0 (a short-form section).
    #[error("short-form section")]
    ShortForm,
    /// The CRC does not check.
    #[error("CRC mismatch")]
    Crc,
    /// `table_id` 0xFF.
    #[error("table_id 0xFF")]
    TableId,
}

impl<'a> Parsed<'a> {
    /// Reads a long-form section from the start of `b` (trailing bytes are ignored).
    pub fn parse(b: &'a [u8]) -> Result<Self, ParseError> {
        if b.len() < 3 {
            return Err(ParseError::Truncated);
        }
        let table_id = TableId::new(b[0]).ok_or(ParseError::TableId)?;
        if b[1] & 0x80 == 0 {
            return Err(ParseError::ShortForm);
        }
        let len = 3 + usize::from(u16::from_be_bytes([b[1] & 0x0F, b[2]]));
        if len < LONG_HEADER + 4 || b.len() < len {
            return Err(ParseError::Truncated);
        }
        let s = &b[..len];
        if crc32_mpeg2(s) != 0 {
            return Err(ParseError::Crc);
        }
        Ok(Parsed {
            header: Long {
                table_id,
                extension: u16::from_be_bytes([s[3], s[4]]),
                version: Version::new((s[5] >> 1) & 0x1F),
                current: s[5] & 1 != 0,
                number: s[6],
                last: s[7],
            },
            private: s[1] & 0x40 != 0,
            body: &s[LONG_HEADER..len - 4],
        })
    }
}

/// Sections of one PID being cut into packets, one packet at a time.
#[derive(Debug, Clone)]
pub struct Cursor<S> {
    sections: Vec<S>,
    /// Index of the section being written.
    at: usize,
    /// Bytes of that section already written.
    pos: usize,
}

impl<S: AsRef<[u8]>> Cursor<S> {
    /// A cursor at the start of `sections` (whole sections, in order).
    pub fn new(sections: Vec<S>) -> Self {
        Cursor {
            sections,
            at: 0,
            pos: 0,
        }
    }

    /// Every section written.
    pub fn done(&self) -> bool {
        self.at >= self.sections.len()
    }

    /// The next packet on `pid` with counter `cc` advanced; `None` when done.
    pub fn next_packet(&mut self, pid: Pid, cc: &mut Cc) -> Option<Packet> {
        let s = self.sections.get(self.at)?.as_ref();
        let start = self.pos == 0;
        let mut tmp = [0xFFu8; MAX_PAYLOAD];
        let mut n = 0;
        if start {
            // pointer_field: the section starts right after it.
            tmp[0] = 0;
            n = 1;
        }
        let k = (s.len() - self.pos).min(MAX_PAYLOAD - n);
        tmp[n..n + k].copy_from_slice(&s[self.pos..self.pos + k]);
        n += k;
        self.pos += k;
        if self.pos >= s.len() {
            self.at += 1;
            self.pos = 0;
        }
        let (p, _) = Packet::payload(
            pid,
            start,
            cc.take(),
            &Adaptation::default(),
            &tmp[..n],
            Fill::PayloadFf,
        )?;
        Some(p)
    }

    /// Packets still to come.
    pub fn packets_left(&self) -> usize {
        let mut n = 0;
        for (i, s) in self.sections.iter().enumerate().skip(self.at) {
            let s = s.as_ref();
            n += if i == self.at && self.pos > 0 {
                (s.len() - self.pos).div_ceil(MAX_PAYLOAD)
            } else {
                (s.len() + 1).div_ceil(MAX_PAYLOAD)
            };
        }
        n
    }
}

/// Joins the sections of one PID from its packets (Rec. §2.4.4.1–2.4.4.2, p.45–46).
///
/// Tolerant, for checkers and probes: a continuity error or a packet with
/// `transport_error_indicator` drops the section in progress; stuffing after a section is
/// skipped; sections are returned as raw bytes (check them with [`Parsed::parse`]). Memory is
/// bounded by the 4,096-byte section limit.
#[derive(Debug, Default)]
pub struct Assembler {
    buf: Vec<u8>,
    /// Bytes the section in `buf` needs, once its length field has been seen.
    need: Option<usize>,
    last_cc: Option<Cc>,
    /// Sections dropped (continuity, length, error indicator).
    pub dropped: u64,
}

impl Assembler {
    /// A new assembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one packet of the PID; complete sections are appended to `out`.
    pub fn push(&mut self, v: &View<'_>, out: &mut Vec<Vec<u8>>) {
        if v.tei() {
            self.reset();
            self.last_cc = None;
            return;
        }
        if !v.has_payload() {
            return;
        }
        let cc = v.cc();
        if let Some(l) = self.last_cc {
            if cc == l {
                return; // duplicate packet
            }
            if cc != l.next() {
                self.reset();
            }
        }
        self.last_cc = Some(cc);
        let mut p = v.payload();
        if v.pusi() {
            let Some((&ptr, rest)) = p.split_first() else {
                return;
            };
            let ptr = usize::from(ptr);
            if ptr > rest.len() {
                self.reset();
                return;
            }
            // The bytes before the pointed-to start end the section in progress.
            if !self.buf.is_empty() {
                self.take(&rest[..ptr], out);
                if !self.buf.is_empty() {
                    self.reset();
                }
            }
            p = &rest[ptr..];
            self.scan_new(p, out);
        } else if !self.buf.is_empty() {
            self.take(p, out);
        }
    }

    fn reset(&mut self) {
        if !self.buf.is_empty() {
            self.dropped += 1;
        }
        self.buf.clear();
        self.need = None;
    }

    /// Sections starting at the front of `p`, back to back, until stuffing.
    fn scan_new(&mut self, mut p: &[u8], out: &mut Vec<Vec<u8>>) {
        while let Some(&b) = p.first() {
            if b == 0xFF {
                return;
            }
            let used = self.take(p, out);
            if !self.buf.is_empty() {
                return; // continues in the next packet
            }
            p = &p[used..];
        }
    }

    /// Appends from `p` to the section in progress (starting one if empty); returns bytes used.
    fn take(&mut self, p: &[u8], out: &mut Vec<Vec<u8>>) -> usize {
        let mut used = 0;
        while used < p.len() {
            let want = match self.need {
                Some(n) => n - self.buf.len(),
                None => 3 - self.buf.len().min(3),
            };
            let k = want.min(p.len() - used);
            self.buf.extend_from_slice(&p[used..used + k]);
            used += k;
            if self.need.is_none() && self.buf.len() >= 3 {
                let n = 3 + usize::from(u16::from_be_bytes([self.buf[1] & 0x0F, self.buf[2]]));
                if n > 4096 {
                    self.reset();
                    return p.len();
                }
                self.need = Some(n);
            }
            if self.need == Some(self.buf.len()) {
                out.push(std::mem::take(&mut self.buf));
                self.need = None;
                return used;
            }
        }
        used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long(n: u8, last: u8) -> Long {
        Long {
            table_id: TableId::CVCT,
            extension: 0x0101,
            version: Version::new(3),
            current: true,
            number: n,
            last,
        }
    }

    #[test]
    fn section_header_fields_and_crc() {
        let s = Section::long(Kind::Private, long(0, 0), &[1, 2, 3]).unwrap();
        assert_eq!(s.len(), 8 + 3 + 4);
        let p = Parsed::parse(s.bytes()).unwrap();
        assert_eq!(p.header, long(0, 0));
        assert!(p.private);
        assert_eq!(p.body, &[1, 2, 3]);
        // section_syntax_indicator 1, private 1, reserved 11.
        assert_eq!(s.bytes()[1] & 0xF0, 0xF0);
        assert_eq!(s.bytes()[5] & 0xC0, 0xC0);
        let psi = Section::long(Kind::Psi, long(0, 0), &[]).unwrap();
        assert_eq!(psi.bytes()[1] & 0xF0, 0xB0);
    }

    #[test]
    fn limits() {
        assert!(Section::long(Kind::Psi, long(0, 0), &[0; 1012]).is_ok());
        assert_eq!(
            Section::long(Kind::Psi, long(0, 0), &[0; 1013]),
            Err(SectionError::TooLong {
                len: 1025,
                max: 1024
            })
        );
        assert!(Section::long(Kind::Private, long(0, 0), &[0; 4084]).is_ok());
        assert!(Section::long(Kind::Private, long(0, 0), &[0; 4085]).is_err());
        assert_eq!(
            Section::long(Kind::Psi, long(2, 1), &[]),
            Err(SectionError::Number(2, 1))
        );
    }

    #[test]
    fn crc_damage_detected() {
        let s = Section::long(Kind::Psi, long(0, 0), &[9; 20]).unwrap();
        let mut b = s.bytes().to_vec();
        b[12] ^= 1;
        assert_eq!(Parsed::parse(&b), Err(ParseError::Crc));
    }

    #[test]
    fn packetize_and_reassemble() {
        let pid = Pid::new(0x1FFB).unwrap();
        for sizes in [
            vec![10usize],
            vec![183 - 12],
            vec![183 - 11],
            vec![184 - 12],
            vec![500, 20, 4000],
        ] {
            let secs: Vec<Section> = sizes
                .iter()
                .enumerate()
                .map(|(i, &n)| {
                    let l = Long {
                        number: i as u8,
                        last: (sizes.len() - 1) as u8,
                        ..long(0, 0)
                    };
                    Section::long(Kind::Private, l, &vec![i as u8; n]).unwrap()
                })
                .collect();
            let want: usize = secs.iter().map(Section::packets).sum();
            let mut c = Cursor::new(secs.clone());
            assert_eq!(c.packets_left(), want);
            let mut cc = Cc::new(0);
            let mut asm = Assembler::new();
            let mut out = Vec::new();
            let mut count = 0;
            while let Some(p) = c.next_packet(pid, &mut cc) {
                let v = p.view().unwrap();
                assert_eq!(v.afc(), 1, "no adaptation field on section PIDs");
                asm.push(&v, &mut out);
                count += 1;
            }
            assert_eq!(count, want);
            assert_eq!(out.len(), secs.len());
            for (a, b) in out.iter().zip(&secs) {
                assert_eq!(a.as_slice(), b.bytes());
            }
        }
    }

    #[test]
    fn assembler_handles_packed_sections_and_loss() {
        // Two sections packed in one packet, as other muxers do.
        let pid = Pid::PAT;
        let a = Section::long(Kind::Psi, long(0, 0), &[1; 10]).unwrap();
        let b = Section::long(Kind::Psi, long(0, 0), &[2; 10]).unwrap();
        let mut payload = vec![0u8];
        payload.extend_from_slice(a.bytes());
        payload.extend_from_slice(b.bytes());
        let (p, _) = Packet::payload(
            pid,
            true,
            Cc::new(0),
            &Adaptation::default(),
            &payload,
            Fill::PayloadFf,
        )
        .unwrap();
        let mut asm = Assembler::new();
        let mut out = Vec::new();
        asm.push(&p.view().unwrap(), &mut out);
        assert_eq!(out.len(), 2);

        // A long section with its middle packet lost is dropped, the next one is kept.
        let big = Section::long(Kind::Private, long(0, 0), &[5; 600]).unwrap();
        let mut c = Cursor::new(vec![big, a.clone()]);
        let mut cc = Cc::new(0);
        let mut asm = Assembler::new();
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(p) = c.next_packet(pid, &mut cc) {
            if i != 1 {
                asm.push(&p.view().unwrap(), &mut out);
            }
            i += 1;
        }
        assert_eq!(out, vec![a.bytes().to_vec()]);
        assert_eq!(asm.dropped, 1);
    }
}
