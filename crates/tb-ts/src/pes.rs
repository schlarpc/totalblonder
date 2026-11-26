//! PES packets (Rec. §2.4.3.6–2.4.3.7, Table 2-21, p.31–39) and their cutting into transport
//! packets.
//!
//! Cable rules applied by construction (`docs/notes/standards-digest.md` §1.7, SCTE 54 §7.7, A/53-3
//! §5.5): scrambling '00'; no ESCR, ES_rate, DSM trick mode, additional copy info, PES CRC or
//! extension; a PTS on every PES; a DTS only when it differs from the PTS;
//! `data_alignment_indicator` 1 (one access unit per PES, starting at the header);
//! `PES_packet_length` 0 for video and exact for everything else.

use crate::packet::{Adaptation, Fill, MAX_PAYLOAD, Packet};
use crate::pid::{Cc, Pid};
use crate::time::Ts90k;

/// The `stream_id` of a PES packet (Rec. Table 2-22, p.33), limited to what we carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamId {
    /// `1110 xxxx`: an ISO/IEC 13818-2 / 11172-2 / H.264 video stream, number 0..=15.
    Video(VideoStreamNumber),
    /// `1011 1101`: private_stream_1, which carries AC-3 (A/53-3 §5.5.2; SCTE 54 §7.7.1).
    PrivateStream1,
}

/// A video stream number, 0..=15.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoStreamNumber(u8);

impl VideoStreamNumber {
    /// Stream 0 (`stream_id` 0xE0), the usual one.
    pub const ZERO: Self = Self(0);
    /// Number `n` if below 16.
    pub const fn new(n: u8) -> Option<Self> {
        if n < 16 { Some(Self(n)) } else { None }
    }
}

impl StreamId {
    /// The byte.
    pub const fn byte(self) -> u8 {
        match self {
            StreamId::Video(n) => 0xE0 | n.0,
            StreamId::PrivateStream1 => 0xBD,
        }
    }

    const fn is_video(self) -> bool {
        matches!(self, StreamId::Video(_))
    }
}

/// Bytes of the PES header we write at most: 9 fixed + PTS 5 + DTS 5.
pub const MAX_HEADER: usize = 19;

/// A PES header as bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    b: [u8; MAX_HEADER],
    len: u8,
}

impl Header {
    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.b[..usize::from(self.len)]
    }
}

/// A PES packet that cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PesError {
    /// A PES that is not video must give its length, which must fit in 16 bits.
    #[error("payload of {0} bytes is too long for PES_packet_length")]
    TooLong(usize),
    /// An empty access unit.
    #[error("empty payload")]
    Empty,
}

/// The header of a PES packet carrying `payload_len` bytes of one access unit with
/// presentation time `pts` and, when it differs, decoding time `dts`.
pub fn header(
    id: StreamId,
    pts: Ts90k,
    dts: Option<Ts90k>,
    payload_len: usize,
) -> Result<Header, PesError> {
    if payload_len == 0 {
        return Err(PesError::Empty);
    }
    let dts = dts.filter(|d| *d != pts);
    let data_len: u8 = if dts.is_some() { 10 } else { 5 };
    // PES_packet_length counts the bytes after itself: 3 + header data + payload (p.33).
    let length = if id.is_video() {
        0
    } else {
        let l = 3 + usize::from(data_len) + payload_len;
        u16::try_from(l).map_err(|_| PesError::TooLong(payload_len))?
    };
    let mut b = [0u8; MAX_HEADER];
    b[..3].copy_from_slice(&[0, 0, 1]);
    b[3] = id.byte();
    b[4..6].copy_from_slice(&length.to_be_bytes());
    // '10', scrambling 00, priority 0, data_alignment_indicator 1, copyright 0, original 0.
    b[6] = 0x84;
    // PTS_DTS_flags, then ESCR, ES_rate, DSM, copy info, CRC, extension: all 0.
    b[7] = if dts.is_some() { 0xC0 } else { 0x80 };
    b[8] = data_len;
    match dts {
        Some(d) => {
            b[9..14].copy_from_slice(&pts.to_pes_bytes(0b0011));
            b[14..19].copy_from_slice(&d.to_pes_bytes(0b0001));
        }
        None => b[9..14].copy_from_slice(&pts.to_pes_bytes(0b0010)),
    }
    Ok(Header {
        b,
        len: 9 + data_len,
    })
}

/// A PES packet being cut into transport packets, one at a time, so a mux can interleave
/// other PIDs between them.
#[derive(Debug, Clone)]
pub struct Cursor<D> {
    header: Header,
    data: D,
    /// Bytes of header + data already written.
    pos: usize,
}

impl<D: AsRef<[u8]>> Cursor<D> {
    /// Cursor at the start of the PES `header` followed by `data`.
    pub fn new(header: Header, data: D) -> Self {
        Cursor {
            header,
            data,
            pos: 0,
        }
    }

    /// Bytes left to write.
    pub fn remaining(&self) -> usize {
        usize::from(self.header.len) + self.data.as_ref().len() - self.pos
    }

    /// Nothing written yet.
    pub fn at_start(&self) -> bool {
        self.pos == 0
    }

    /// Everything written.
    pub fn done(&self) -> bool {
        self.remaining() == 0
    }

    /// Packets the rest needs if none carries an adaptation field (a lower bound when PCRs
    /// will be added).
    pub fn packets_left(&self) -> usize {
        self.remaining().div_ceil(MAX_PAYLOAD)
    }

    /// The next packet on `pid`, with the adaptation field `af` (PCR, flags) and the counter
    /// `cc` advanced. The first packet has `payload_unit_start_indicator` set; the last is
    /// completed with adaptation-field stuffing. `None` when the PES is finished.
    pub fn next_packet(&mut self, pid: Pid, cc: &mut Cc, af: &Adaptation) -> Option<Packet> {
        if self.done() {
            return None;
        }
        let mut tmp = [0u8; MAX_PAYLOAD];
        let hl = usize::from(self.header.len);
        let data = self.data.as_ref();
        let mut n = 0;
        let mut p = self.pos;
        while n < MAX_PAYLOAD && p < hl + data.len() {
            if p < hl {
                let k = (hl - p).min(MAX_PAYLOAD - n);
                tmp[n..n + k].copy_from_slice(&self.header.b[p..p + k]);
                n += k;
                p += k;
            } else {
                let k = (hl + data.len() - p).min(MAX_PAYLOAD - n);
                tmp[n..n + k].copy_from_slice(&data[p - hl..p - hl + k]);
                n += k;
                p += k;
            }
        }
        let pusi = self.pos == 0;
        let (pkt, took) = Packet::payload(pid, pusi, cc.take(), af, &tmp[..n], Fill::Adaptation)?;
        self.pos += took;
        Some(pkt)
    }

    /// The access unit's bytes.
    pub fn data(&self) -> &D {
        &self.data
    }
}

/// A PES header read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    /// `stream_id`.
    pub stream_id: u8,
    /// `PES_packet_length`.
    pub length: u16,
    /// `data_alignment_indicator`.
    pub aligned: bool,
    /// PTS, if present.
    pub pts: Option<Ts90k>,
    /// DTS, if present.
    pub dts: Option<Ts90k>,
    /// Bytes of the header (the payload starts here).
    pub header_len: usize,
}

/// Why a PES header could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PesParseError {
    /// Shorter than its header.
    #[error("truncated PES header")]
    Truncated,
    /// No `packet_start_code_prefix`.
    #[error("no start code prefix")]
    StartCode,
    /// The '10' bits, a reserved PTS_DTS_flags value or a marker bit is wrong.
    #[error("malformed PES header")]
    Malformed,
}

/// Reads the header at the start of a PES packet (stream ids with the optional header only).
pub fn parse(b: &[u8]) -> Result<Parsed, PesParseError> {
    if b.len() < 9 {
        return Err(PesParseError::Truncated);
    }
    if b[..3] != [0, 0, 1] {
        return Err(PesParseError::StartCode);
    }
    if b[6] >> 6 != 0b10 {
        return Err(PesParseError::Malformed);
    }
    let hl = 9 + usize::from(b[8]);
    if b.len() < hl {
        return Err(PesParseError::Truncated);
    }
    let ts = |at: usize, prefix: u8| -> Result<Ts90k, PesParseError> {
        let mut x = [0u8; 5];
        x.copy_from_slice(b.get(at..at + 5).ok_or(PesParseError::Truncated)?);
        if x[0] >> 4 != prefix {
            return Err(PesParseError::Malformed);
        }
        Ts90k::from_pes_bytes(x).ok_or(PesParseError::Malformed)
    };
    let (pts, dts) = match b[7] >> 6 {
        0b00 => (None, None),
        0b10 => (Some(ts(9, 0b0010)?), None),
        0b11 => (Some(ts(9, 0b0011)?), Some(ts(14, 0b0001)?)),
        _ => return Err(PesParseError::Malformed),
    };
    Ok(Parsed {
        stream_id: b[3],
        length: u16::from_be_bytes([b[4], b[5]]),
        aligned: b[6] & 0x04 != 0,
        pts,
        dts,
        header_len: hl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid() -> Pid {
        Pid::new(0x31).unwrap()
    }

    fn reassemble(packets: &[Packet]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in packets {
            out.extend_from_slice(p.view().unwrap().payload());
        }
        out
    }

    #[test]
    fn video_header_has_zero_length_and_both_stamps() {
        let h = header(
            StreamId::Video(VideoStreamNumber::ZERO),
            Ts90k::wrap(9009),
            Some(Ts90k::wrap(6006)),
            100_000,
        )
        .unwrap();
        let p = parse(h.as_bytes()).unwrap();
        assert_eq!(p.stream_id, 0xE0);
        assert_eq!(p.length, 0);
        assert!(p.aligned);
        assert_eq!(
            (p.pts, p.dts),
            (Some(Ts90k::wrap(9009)), Some(Ts90k::wrap(6006)))
        );
        assert_eq!(p.header_len, 19);
    }

    #[test]
    fn dts_equal_to_pts_is_left_out() {
        let t = Ts90k::wrap(1234);
        let h = header(StreamId::PrivateStream1, t, Some(t), 768).unwrap();
        let p = parse(h.as_bytes()).unwrap();
        assert_eq!((p.pts, p.dts, p.header_len), (Some(t), None, 14));
        // 3 + 5 + 768.
        assert_eq!(p.length, 776);
    }

    #[test]
    fn audio_too_long_is_refused() {
        assert_eq!(
            header(StreamId::PrivateStream1, Ts90k::wrap(0), None, 70_000),
            Err(PesError::TooLong(70_000))
        );
        assert_eq!(
            header(StreamId::PrivateStream1, Ts90k::wrap(0), None, 0),
            Err(PesError::Empty)
        );
    }

    #[test]
    fn cursor_round_trips_every_size_near_packet_edges() {
        for len in [
            1usize, 160, 161, 165, 166, 170, 183, 184, 350, 351, 352, 5000,
        ] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
            let h = header(StreamId::PrivateStream1, Ts90k::wrap(1), None, len).unwrap();
            let mut c = Cursor::new(h, data.clone());
            let mut cc = Cc::new(15);
            let mut pk = Vec::new();
            let mut first = true;
            while let Some(p) = c.next_packet(pid(), &mut cc, &Adaptation::default()) {
                assert_eq!(p.view().unwrap().pusi(), first);
                first = false;
                pk.push(p);
            }
            let bytes = reassemble(&pk);
            let parsed = parse(&bytes).unwrap();
            assert_eq!(&bytes[parsed.header_len..], &data[..]);
            assert_eq!(usize::from(parsed.length), bytes.len() - 6);
            // Counters 15, 0, 1, ...
            for (i, p) in pk.iter().enumerate() {
                assert_eq!(usize::from(p.view().unwrap().cc().get()), (i + 15) % 16);
            }
        }
    }

    #[test]
    fn cursor_with_pcr_on_first_packet() {
        let data = vec![0u8; 1000];
        let h = header(
            StreamId::Video(VideoStreamNumber::ZERO),
            Ts90k::wrap(0),
            None,
            1000,
        )
        .unwrap();
        let mut c = Cursor::new(h, data.clone());
        let mut cc = Cc::default();
        let af = Adaptation {
            random_access: true,
            ..Adaptation::pcr(crate::Stc(1).pcr())
        };
        let p0 = c.next_packet(pid(), &mut cc, &af).unwrap();
        assert!(p0.view().unwrap().random_access());
        let mut pk = vec![p0];
        while let Some(p) = c.next_packet(pid(), &mut cc, &Adaptation::default()) {
            pk.push(p);
        }
        let bytes = reassemble(&pk);
        let parsed = parse(&bytes).unwrap();
        assert_eq!(&bytes[parsed.header_len..], &data[..]);
    }
}
