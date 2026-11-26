//! The chunk stream layer (Adobe RTMP 1.0 §5.3): messages in, chunks out, and back.
//!
//! A chunk is a basic header (§5.3.1.1: format and chunk stream id, 1–3 bytes), a message
//! header (§5.3.1.2: 11, 7, 3 or 0 bytes for formats 0–3), an optional extended timestamp
//! (§5.3.1.3: 4 bytes when the 24-bit field is 0xFFFFFF) and up to *chunk size* payload bytes.
//!
//! Timestamp rules, as ffmpeg (`libavformat/rtmppkt.c`) reads and writes them:
//! - format 0 carries the absolute timestamp; formats 1 and 2 a delta added to the previous
//!   message's timestamp on the chunk stream (§5.3.1.2.2, .3);
//! - a format 3 chunk that *starts* a message repeats the last timestamp field of the chunk
//!   stream as a delta (§5.3.1.2.4, §5.3.2.1 example 1);
//! - a format 3 chunk that *continues* a message changes nothing;
//! - the fmt-3 extended-timestamp rule: when the latest format 0/1/2 header of the chunk stream
//!   used an extended timestamp, every following format 3 chunk carries the 4-byte extended
//!   field too (§5.3.1.3, "this field MUST be sent when the normal timestamp is set to
//!   0xffffff ... the most recent Type 0, 1, or 2 chunk"). It is read and, for a continuation,
//!   ignored; for a new message it is the delta. ffmpeg and librtmp both write it this way.
//!
//! Hostile input: every length is checked before it is used, nothing is allocated from a
//! declared length (message buffers grow with the bytes that actually arrive), the number of
//! chunk streams, the size of one message and the total of partially received messages are
//! bounded ([`Limits`]), and no input panics.

use std::collections::BTreeMap;

use crate::types::{ChunkStreamId, Message, MessageStreamId, MessageType, RtmpTime};

/// The chunk size both sides start with (§5.4.1).
pub const DEFAULT_CHUNK_SIZE: u32 = 128;
/// The largest chunk size the protocol allows: 31 bits (§5.4.1, the top bit must be 0).
pub const MAX_CHUNK_SIZE: u32 = 0x7FFF_FFFF;
const EXTENDED: u32 = 0x00FF_FFFF;

/// Bounds on what a peer can make the decoder hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest message accepted (the 24-bit length field allows 16 MiB - 1).
    pub max_message_len: u32,
    /// Most chunk streams a peer may open. A publisher uses about four.
    pub max_chunk_streams: usize,
    /// Most bytes held in partially received messages across all chunk streams.
    pub max_buffered: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // A 1080p intra picture at a high rate is around 1 MiB; 8 MiB leaves room for 4K.
            max_message_len: 8 << 20,
            max_chunk_streams: 64,
            max_buffered: 16 << 20,
        }
    }
}

/// A violation of the chunk layer, or a limit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    /// A format 1, 2 or 3 chunk on a chunk stream that has had no format 0 header.
    #[error("format {fmt} chunk on {csid} before any format 0 header (RTMP §5.3.1.2)")]
    NoPriorHeader {
        /// The format.
        fmt: u8,
        /// The chunk stream.
        csid: ChunkStreamId,
    },
    /// A message longer than [`Limits::max_message_len`].
    #[error("message of {len} bytes exceeds the limit of {limit}")]
    MessageTooLong {
        /// Declared length.
        len: u32,
        /// The limit.
        limit: u32,
    },
    /// More chunk streams than [`Limits::max_chunk_streams`].
    #[error("more than {0} chunk streams")]
    TooManyChunkStreams(usize),
    /// Partially received messages exceed [`Limits::max_buffered`].
    #[error("partial messages exceed {0} bytes")]
    BufferLimit(usize),
    /// Set Chunk Size with a zero value or the top bit set (§5.4.1).
    #[error("invalid chunk size {0:#x} (RTMP §5.4.1: 1..=0x7FFFFFFF)")]
    ChunkSize(u32),
    /// A protocol control message too short for its fields.
    #[error("protocol control message type {type_id} has {len} bytes")]
    ShortControl {
        /// Message type.
        type_id: u8,
        /// Its length.
        len: usize,
    },
}

/// The receive state of one chunk stream.
#[derive(Debug, Clone)]
struct InStream {
    /// Timestamp of the latest message.
    timestamp: RtmpTime,
    /// The latest timestamp field (absolute for format 0, a delta otherwise), extended
    /// resolved: what a format 3 chunk starting a message adds.
    field: u32,
    /// The latest format 0/1/2 header used an extended timestamp.
    extended: bool,
    len: u32,
    type_id: MessageType,
    stream: MessageStreamId,
    /// The message being received, if one is in progress.
    partial: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadState {
    /// Collecting header bytes in `hdr`.
    Header,
    /// `left` payload bytes of the current chunk remain, for chunk stream `csid`.
    Payload { csid: ChunkStreamId, left: u32 },
}

/// Turns chunk bytes into messages. Set Chunk Size and Abort (§5.4.1, §5.4.2) act here, the
/// moment their message completes, because they change how the next chunk is read; they are
/// also passed on.
#[derive(Debug, Clone)]
pub struct ChunkDecoder {
    limits: Limits,
    chunk_size: u32,
    streams: BTreeMap<ChunkStreamId, InStream>,
    hdr: [u8; 18],
    hdr_len: usize,
    state: ReadState,
    buffered: usize,
}

impl ChunkDecoder {
    /// A decoder at the default chunk size.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            chunk_size: DEFAULT_CHUNK_SIZE,
            streams: BTreeMap::new(),
            hdr: [0; 18],
            hdr_len: 0,
            state: ReadState::Header,
            buffered: 0,
        }
    }

    /// The peer's current chunk size.
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// Bytes held in partially received messages.
    pub fn buffered(&self) -> usize {
        self.buffered
    }

    /// Decode `input`, appending each completed message to `out`. Input may be split anywhere;
    /// the messages do not depend on how. After an error the decoder must not be used again.
    pub fn push(&mut self, mut input: &[u8], out: &mut Vec<Message>) -> Result<(), ChunkError> {
        while !input.is_empty() {
            match self.state {
                ReadState::Header => {
                    let need = self.header_len()?;
                    if self.hdr_len < need {
                        let take = (need - self.hdr_len).min(input.len());
                        self.hdr[self.hdr_len..self.hdr_len + take].copy_from_slice(&input[..take]);
                        self.hdr_len += take;
                        input = &input[take..];
                        continue;
                    }
                    self.finish_header(out)?;
                }
                ReadState::Payload { csid, left } => {
                    let take = (left as usize).min(input.len());
                    if self.buffered + take > self.limits.max_buffered {
                        return Err(ChunkError::BufferLimit(self.limits.max_buffered));
                    }
                    let s = self.streams.get_mut(&csid);
                    if let Some(InStream {
                        partial: Some(buf), ..
                    }) = s
                    {
                        buf.extend_from_slice(&input[..take]);
                    }
                    self.buffered += take;
                    input = &input[take..];
                    // `take <= left`, which is a u32.
                    let left = left - take as u32;
                    if left > 0 {
                        self.state = ReadState::Payload { csid, left };
                    } else {
                        self.state = ReadState::Header;
                        self.complete_if_full(csid, out)?;
                    }
                }
            }
        }
        // A header that is complete but not yet acted on (its last byte was the last input
        // byte) is processed now so that a zero-length message is not held back.
        if self.state == ReadState::Header && self.hdr_len > 0 && self.header_len()? == self.hdr_len
        {
            self.finish_header(out)?;
        }
        Ok(())
    }

    /// The total header length implied by the bytes collected so far (at least
    /// `hdr_len + 1` while it is not yet known).
    fn header_len(&self) -> Result<usize, ChunkError> {
        if self.hdr_len == 0 {
            return Ok(1);
        }
        let b0 = self.hdr[0];
        let fmt = b0 >> 6;
        let basic = match b0 & 0x3F {
            0 => 2,
            1 => 3,
            _ => 1,
        };
        if self.hdr_len < basic {
            return Ok(basic);
        }
        let msg = [11, 7, 3, 0][usize::from(fmt)];
        let base = basic + msg;
        if self.hdr_len < base {
            return Ok(base);
        }
        let ext = if fmt < 3 {
            be24(&self.hdr[basic..basic + 3]) == EXTENDED
        } else {
            let csid = self.basic_csid();
            match self.streams.get(&csid) {
                Some(s) => s.extended,
                None => return Err(ChunkError::NoPriorHeader { fmt, csid }),
            }
        };
        Ok(base + if ext { 4 } else { 0 })
    }

    fn basic_csid(&self) -> ChunkStreamId {
        let id = match self.hdr[0] & 0x3F {
            0 => 64 + u32::from(self.hdr[1]),
            1 => 64 + u32::from(self.hdr[1]) + 256 * u32::from(self.hdr[2]),
            n => u32::from(n),
        };
        // 2..=65599 by construction of the three forms.
        ChunkStreamId::new(id).unwrap_or(ChunkStreamId::CONTROL)
    }

    fn finish_header(&mut self, out: &mut Vec<Message>) -> Result<(), ChunkError> {
        let h = self.hdr;
        let len_total = self.hdr_len;
        self.hdr_len = 0;
        let fmt = h[0] >> 6;
        let csid = self.basic_csid();
        let basic = match h[0] & 0x3F {
            0 => 2,
            1 => 3,
            _ => 1,
        };
        let m = &h[basic..len_total];
        let ext_val = |at: usize| be32(&m[at..at + 4]);

        if !self.streams.contains_key(&csid) {
            if fmt != 0 {
                return Err(ChunkError::NoPriorHeader { fmt, csid });
            }
            if self.streams.len() >= self.limits.max_chunk_streams {
                return Err(ChunkError::TooManyChunkStreams(
                    self.limits.max_chunk_streams,
                ));
            }
        }
        let prev = self.streams.get(&csid).cloned();
        let mut s = prev.unwrap_or(InStream {
            timestamp: RtmpTime(0),
            field: 0,
            extended: false,
            len: 0,
            type_id: MessageType(0),
            stream: MessageStreamId(0),
            partial: None,
        });

        let continuing = fmt == 3 && s.partial.is_some();
        if !continuing {
            // A new header while a message is in progress: the partial is dropped (lenient,
            // as nginx-rtmp; the spec has no such sequence).
            if let Some(p) = s.partial.take() {
                self.buffered -= p.len();
            }
            match fmt {
                0..=2 => {
                    let raw = be24(&m[0..3]);
                    s.extended = raw == EXTENDED;
                    let field = if s.extended {
                        ext_val([11, 7, 3][usize::from(fmt)])
                    } else {
                        raw
                    };
                    s.field = field;
                    if fmt == 0 {
                        s.timestamp = RtmpTime(field);
                        s.stream = MessageStreamId(u32::from_le_bytes([m[7], m[8], m[9], m[10]]));
                    } else {
                        s.timestamp = s.timestamp.add(field);
                    }
                    if fmt < 2 {
                        s.len = be24(&m[3..6]);
                        s.type_id = MessageType(m[6]);
                    }
                }
                _ => {
                    if s.extended {
                        s.field = ext_val(0);
                    }
                    s.timestamp = s.timestamp.add(s.field);
                }
            }
            if s.len > self.limits.max_message_len {
                return Err(ChunkError::MessageTooLong {
                    len: s.len,
                    limit: self.limits.max_message_len,
                });
            }
            s.partial = Some(Vec::new());
        }
        let have = s.partial.as_ref().map_or(0, Vec::len);
        // `have < len` while a message is in progress; `len` fits in 24 bits.
        let remaining = s.len.saturating_sub(have as u32);
        self.streams.insert(csid, s);
        let left = remaining.min(self.chunk_size);
        if left > 0 {
            self.state = ReadState::Payload { csid, left };
            Ok(())
        } else {
            self.state = ReadState::Header;
            self.complete_if_full(csid, out)
        }
    }

    fn complete_if_full(
        &mut self,
        csid: ChunkStreamId,
        out: &mut Vec<Message>,
    ) -> Result<(), ChunkError> {
        let Some(s) = self.streams.get_mut(&csid) else {
            return Ok(());
        };
        let full = s
            .partial
            .as_ref()
            .is_some_and(|p| p.len() >= s.len as usize);
        if !full {
            return Ok(());
        }
        let payload = s.partial.take().unwrap_or_default();
        self.buffered -= payload.len();
        let msg = Message {
            csid,
            timestamp: s.timestamp,
            type_id: s.type_id,
            stream: s.stream,
            payload,
        };
        self.control(&msg)?;
        out.push(msg);
        Ok(())
    }

    /// Set Chunk Size and Abort take effect here.
    fn control(&mut self, msg: &Message) -> Result<(), ChunkError> {
        if msg.stream != MessageStreamId::CONTROL {
            return Ok(());
        }
        let short = || ChunkError::ShortControl {
            type_id: msg.type_id.0,
            len: msg.payload.len(),
        };
        match msg.type_id {
            MessageType::SET_CHUNK_SIZE => {
                let v = msg.payload.get(..4).map(be32).ok_or_else(short)?;
                if v == 0 || v > MAX_CHUNK_SIZE {
                    return Err(ChunkError::ChunkSize(v));
                }
                self.chunk_size = v;
            }
            MessageType::ABORT => {
                let v = msg.payload.get(..4).map(be32).ok_or_else(short)?;
                if let Ok(id) = ChunkStreamId::new(v)
                    && let Some(s) = self.streams.get_mut(&id)
                    && let Some(p) = s.partial.take()
                {
                    self.buffered -= p.len();
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// The send state of one chunk stream.
#[derive(Debug, Clone, Copy)]
struct OutStream {
    timestamp: RtmpTime,
    field: u32,
    len: u32,
    type_id: MessageType,
    stream: MessageStreamId,
}

/// Turns messages into chunks, choosing the most compact header format (§5.3.1.2), as
/// ffmpeg's writer does: format 0 for the first message of a chunk stream, a new message
/// stream, or a timestamp that goes backwards; otherwise 1 (new length or type), 2 (new
/// delta) or 3 (everything repeats).
#[derive(Debug, Clone)]
pub struct ChunkEncoder {
    chunk_size: u32,
    streams: BTreeMap<ChunkStreamId, OutStream>,
}

impl Default for ChunkEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkEncoder {
    /// An encoder at the default chunk size.
    pub fn new() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            streams: BTreeMap::new(),
        }
    }

    /// Our chunk size.
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// Change our chunk size. Write the Set Chunk Size message (at the old size) first; the
    /// value must be valid (1..=[`MAX_CHUNK_SIZE`]).
    pub fn set_chunk_size(&mut self, size: u32) -> Result<(), ChunkError> {
        if size == 0 || size > MAX_CHUNK_SIZE {
            return Err(ChunkError::ChunkSize(size));
        }
        self.chunk_size = size;
        Ok(())
    }

    /// Append `msg` as chunks to `out`. The payload must be shorter than 2^24 bytes (the
    /// length field); a longer one is refused with [`ChunkError::MessageTooLong`].
    pub fn write(&mut self, msg: &Message, out: &mut Vec<u8>) -> Result<(), ChunkError> {
        let len = u32::try_from(msg.payload.len())
            .ok()
            .filter(|&l| l <= EXTENDED)
            .ok_or(ChunkError::MessageTooLong {
                len: u32::try_from(msg.payload.len()).unwrap_or(u32::MAX),
                limit: EXTENDED,
            })?;
        let prev = self.streams.get(&msg.csid).copied();
        let delta = prev
            .filter(|p| p.stream == msg.stream)
            .and_then(|p| msg.timestamp.since(p.timestamp).map(|d| (p, d)));
        let (fmt, field) = match delta {
            None => (0u8, msg.timestamp.0),
            Some((p, d)) if p.len == len && p.type_id == msg.type_id => {
                if d == p.field {
                    (3, d)
                } else {
                    (2, d)
                }
            }
            Some((_, d)) => (1, d),
        };
        let extended = field >= EXTENDED;
        basic_header(out, fmt, msg.csid);
        let ts24 = if extended { EXTENDED } else { field };
        match fmt {
            0 => {
                out.extend_from_slice(&be24_bytes(ts24));
                out.extend_from_slice(&be24_bytes(len));
                out.push(msg.type_id.0);
                out.extend_from_slice(&msg.stream.0.to_le_bytes());
            }
            1 => {
                out.extend_from_slice(&be24_bytes(ts24));
                out.extend_from_slice(&be24_bytes(len));
                out.push(msg.type_id.0);
            }
            2 => out.extend_from_slice(&be24_bytes(ts24)),
            _ => {}
        }
        // For format 3, `field` equals the previous field, so `extended` is the previous
        // header's flag, which is what the reader uses to expect the 4 bytes.
        if extended {
            out.extend_from_slice(&field.to_be_bytes());
        }
        let cs = self.chunk_size as usize;
        let mut chunks = msg.payload.chunks(cs);
        if let Some(first) = chunks.next() {
            out.extend_from_slice(first);
        }
        for c in chunks {
            basic_header(out, 3, msg.csid);
            if extended {
                out.extend_from_slice(&field.to_be_bytes());
            }
            out.extend_from_slice(c);
        }
        self.streams.insert(
            msg.csid,
            OutStream {
                timestamp: msg.timestamp,
                field,
                len,
                type_id: msg.type_id,
                stream: msg.stream,
            },
        );
        Ok(())
    }
}

fn basic_header(out: &mut Vec<u8>, fmt: u8, csid: ChunkStreamId) {
    let id = csid.get();
    let f = fmt << 6;
    if id < 64 {
        // 2..=63.
        out.push(f | id as u8);
    } else if id < 64 + 256 {
        out.push(f);
        out.push((id - 64) as u8);
    } else {
        let v = id - 64;
        out.push(f | 1);
        out.push((v & 0xFF) as u8);
        out.push((v >> 8) as u8);
    }
}

pub(crate) fn be24(b: &[u8]) -> u32 {
    (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2])
}

pub(crate) fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be24_bytes(v: u32) -> [u8; 3] {
    let b = v.to_be_bytes();
    [b[1], b[2], b[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(csid: u32, ts: u32, t: u8, stream: u32, payload: Vec<u8>) -> Message {
        Message {
            csid: ChunkStreamId::new(csid).unwrap(),
            timestamp: RtmpTime(ts),
            type_id: MessageType(t),
            stream: MessageStreamId(stream),
            payload,
        }
    }

    fn decode_all(bytes: &[u8]) -> Result<Vec<Message>, ChunkError> {
        let mut d = ChunkDecoder::new(Limits::default());
        let mut out = Vec::new();
        d.push(bytes, &mut out)?;
        Ok(out)
    }

    /// §5.3.2.1 example 1: four audio messages, formats 0, 2, 3, 3, each a 32-byte message
    /// with deltas of 33 ms.
    #[test]
    fn spec_example_1() {
        let mut b = vec![
            0x03, 0, 0x03, 0xE8, 0, 0, 32, 8, 0x01, 0x34, 0x12, 0x00, // fmt 0, ts 1000
        ];
        b.extend_from_slice(&[0xAA; 32]);
        b.extend_from_slice(&[0x83, 0, 0, 33]); // fmt 2, delta 33
        b.extend_from_slice(&[0xBB; 32]);
        b.push(0xC3); // fmt 3
        b.extend_from_slice(&[0xCC; 32]);
        b.push(0xC3);
        b.extend_from_slice(&[0xDD; 32]);
        let m = decode_all(&b).unwrap();
        let ts: Vec<u32> = m.iter().map(|m| m.timestamp.0).collect();
        assert_eq!(ts, [1000, 1033, 1066, 1099]);
        assert!(m.iter().all(|m| m.stream == MessageStreamId(0x0012_3401)));
        assert_eq!(m[3].payload, vec![0xDD; 32]);
    }

    /// §5.3.2.2 example 2: one 307-byte video message in three chunks (128, 128, 51).
    #[test]
    fn spec_example_2() {
        let mut b = vec![0x04, 0, 0x03, 0xE8, 0, 0x01, 0x33, 9, 0x01, 0, 0, 0];
        b.extend_from_slice(&[1; 128]);
        b.push(0xC4);
        b.extend_from_slice(&[2; 128]);
        b.push(0xC4);
        b.extend_from_slice(&[3; 51]);
        let m = decode_all(&b).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].payload.len(), 307);
        assert_eq!(m[0].timestamp, RtmpTime(1000));
    }

    #[test]
    fn extended_timestamps_repeat_in_format_3() {
        let mut enc = ChunkEncoder::new();
        let mut b = Vec::new();
        let base = 0x0100_0000u32;
        let msgs: Vec<Message> = (0..4)
            .map(|i| msg(6, base + i * 33, 9, 1, vec![i as u8; 300]))
            .collect();
        for m in &msgs {
            enc.write(m, &mut b).unwrap();
        }
        // First chunk: fmt 0 with 0xFFFFFF and the extended field; continuation chunks carry
        // the 4 bytes again.
        assert_eq!(&b[1..4], &[0xFF; 3]);
        assert_eq!(&b[12..16], &base.to_be_bytes());
        assert_eq!(b[16 + 128], 0xC6);
        assert_eq!(&b[17 + 128..21 + 128], &base.to_be_bytes());
        assert_eq!(decode_all(&b).unwrap(), msgs);
    }

    #[test]
    fn round_trip_all_formats_and_ids() {
        let mut enc = ChunkEncoder::new();
        let mut b = Vec::new();
        let mut msgs = Vec::new();
        for (i, csid) in [3u32, 63, 64, 319, 320, 65_599].into_iter().enumerate() {
            for k in 0..5u32 {
                let len = if k == 2 {
                    0
                } else {
                    10 + 100 * (k as usize % 3)
                };
                msgs.push(msg(
                    csid,
                    k * 40 + i as u32,
                    8 + (k % 2) as u8,
                    1,
                    vec![7; len],
                ));
            }
        }
        // Timestamp going backwards and a new message stream force format 0.
        msgs.push(msg(3, 5, 8, 1, vec![1, 2]));
        msgs.push(msg(3, 6, 8, 2, vec![1, 2]));
        for m in &msgs {
            enc.write(m, &mut b).unwrap();
        }
        assert_eq!(decode_all(&b).unwrap(), msgs);
        // And split at every byte.
        let mut d = ChunkDecoder::new(Limits::default());
        let mut out = Vec::new();
        for x in &b {
            d.push(std::slice::from_ref(x), &mut out).unwrap();
        }
        assert_eq!(out, msgs);
    }

    #[test]
    fn set_chunk_size_applies_to_the_next_chunk() {
        let mut enc = ChunkEncoder::new();
        let mut b = Vec::new();
        enc.write(&msg(2, 0, 1, 0, 4096u32.to_be_bytes().to_vec()), &mut b)
            .unwrap();
        enc.set_chunk_size(4096).unwrap();
        let big = msg(4, 0, 9, 1, vec![9; 5000]);
        enc.write(&big, &mut b).unwrap();
        let m = decode_all(&b).unwrap();
        assert_eq!(m[1], big);
    }

    #[test]
    fn limits_and_violations() {
        // fmt 1 on a fresh chunk stream.
        assert!(matches!(
            decode_all(&[0x43, 0, 0, 0, 0, 0, 1, 8]),
            Err(ChunkError::NoPriorHeader { fmt: 1, .. })
        ));
        // A declared 16 MiB message is refused before any of it is buffered.
        assert!(matches!(
            decode_all(&[0x03, 0, 0, 0, 0xFF, 0xFF, 0xFE, 9, 0, 0, 0, 0]),
            Err(ChunkError::MessageTooLong { .. })
        ));
        // Chunk size 0 and with the top bit set.
        for v in [0u32, 0x8000_0000] {
            let mut b = vec![0x02, 0, 0, 0, 0, 0, 4, 1, 0, 0, 0, 0];
            b.extend_from_slice(&v.to_be_bytes());
            assert_eq!(decode_all(&b), Err(ChunkError::ChunkSize(v)));
        }
        // Too many chunk streams.
        let mut d = ChunkDecoder::new(Limits {
            max_chunk_streams: 2,
            ..Limits::default()
        });
        let mut out = Vec::new();
        let mut b = Vec::new();
        for c in 3..6u8 {
            b.extend_from_slice(&[c, 0, 0, 0, 0, 0, 1, 8, 0, 0, 0, 0, 0]);
        }
        assert_eq!(
            d.push(&b, &mut out),
            Err(ChunkError::TooManyChunkStreams(2))
        );
        // Buffer limit across chunk streams.
        let mut d = ChunkDecoder::new(Limits {
            max_buffered: 300,
            ..Limits::default()
        });
        let mut b = Vec::new();
        for c in 3..6u8 {
            b.extend_from_slice(&[c, 0, 0, 0, 0, 1, 0, 8, 0, 0, 0, 0]);
            b.extend_from_slice(&[0; 128]);
        }
        assert_eq!(d.push(&b, &mut out), Err(ChunkError::BufferLimit(300)));
    }

    #[test]
    fn abort_drops_the_partial_message() {
        let mut b = vec![0x04, 0, 0, 0, 0, 1, 0, 9, 1, 0, 0, 0];
        b.extend_from_slice(&[1; 128]);
        // Abort csid 4.
        b.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 4, 2, 0, 0, 0, 0, 0, 0, 0, 4]);
        // A new message on csid 4 with format 3: starts afresh, length 256 again.
        b.push(0xC4);
        b.extend_from_slice(&[2; 128]);
        b.push(0xC4);
        b.extend_from_slice(&[3; 128]);
        let mut d = ChunkDecoder::new(Limits::default());
        let mut out = Vec::new();
        d.push(&b, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].payload[..128], [2; 128]);
        assert_eq!(d.buffered(), 0);
    }
}
