//! Protocol control messages (Adobe RTMP 1.0 §5.4), user control messages (§6.2, §7.1.7) and
//! command messages (§7.1.1, §7.2), built and parsed. Used by the server and usable by a
//! future client.

use crate::amf0::{self, Amf0Value, AmfError};
use crate::types::{ChunkStreamId, Message, MessageStreamId, MessageType, RtmpTime};

fn control(type_id: MessageType, payload: Vec<u8>) -> Message {
    Message {
        csid: ChunkStreamId::CONTROL,
        timestamp: RtmpTime(0),
        type_id,
        stream: MessageStreamId::CONTROL,
        payload,
    }
}

/// Set Chunk Size (§5.4.1).
pub fn set_chunk_size(size: u32) -> Message {
    control(MessageType::SET_CHUNK_SIZE, size.to_be_bytes().to_vec())
}

/// Acknowledgment (§5.4.3): the number of bytes received so far (modulo 2^32).
pub fn acknowledgment(sequence: u32) -> Message {
    control(MessageType::ACKNOWLEDGMENT, sequence.to_be_bytes().to_vec())
}

/// Window Acknowledgment Size (§5.4.4).
pub fn window_ack_size(size: u32) -> Message {
    control(MessageType::WINDOW_ACK_SIZE, size.to_be_bytes().to_vec())
}

/// Set Peer Bandwidth limit types (§5.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitType {
    /// 0: limit output to the window.
    Hard = 0,
    /// 1: limit to the window or the previous limit, whichever is smaller.
    Soft = 1,
    /// 2: hard if the previous was hard, otherwise ignore.
    Dynamic = 2,
}

/// Set Peer Bandwidth (§5.4.5).
pub fn set_peer_bandwidth(size: u32, limit: LimitType) -> Message {
    let mut p = size.to_be_bytes().to_vec();
    p.push(limit as u8);
    control(MessageType::SET_PEER_BANDWIDTH, p)
}

/// User control event types (§7.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserControl {
    /// 0: the stream is ready for use.
    StreamBegin(MessageStreamId),
    /// 1: playback is over.
    StreamEof(MessageStreamId),
    /// 2: no more data on the stream.
    StreamDry(MessageStreamId),
    /// 3: the client's buffer length in ms.
    SetBufferLength(MessageStreamId, u32),
    /// 4: the stream is recorded.
    StreamIsRecorded(MessageStreamId),
    /// 6: ping request with a timestamp.
    PingRequest(u32),
    /// 7: ping response echoing the timestamp.
    PingResponse(u32),
    /// Any other event (type, data).
    Other(u16),
}

impl UserControl {
    /// Parse a user control message body; `None` if it is shorter than its event needs.
    pub fn parse(p: &[u8]) -> Option<Self> {
        let t = u16::from_be_bytes([*p.first()?, *p.get(1)?]);
        let word = |at: usize| -> Option<u32> {
            let b = p.get(at..at + 4)?;
            Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let sid = || word(2).map(MessageStreamId);
        Some(match t {
            0 => Self::StreamBegin(sid()?),
            1 => Self::StreamEof(sid()?),
            2 => Self::StreamDry(sid()?),
            3 => Self::SetBufferLength(sid()?, word(6)?),
            4 => Self::StreamIsRecorded(sid()?),
            6 => Self::PingRequest(word(2)?),
            7 => Self::PingResponse(word(2)?),
            other => Self::Other(other),
        })
    }

    /// The message carrying this event (on chunk stream 2, message stream 0).
    pub fn to_message(self) -> Message {
        let (t, a, b) = match self {
            Self::StreamBegin(s) => (0u16, s.0, None),
            Self::StreamEof(s) => (1, s.0, None),
            Self::StreamDry(s) => (2, s.0, None),
            Self::SetBufferLength(s, ms) => (3, s.0, Some(ms)),
            Self::StreamIsRecorded(s) => (4, s.0, None),
            Self::PingRequest(t) => (6, t, None),
            Self::PingResponse(t) => (7, t, None),
            Self::Other(t) => (t, 0, None),
        };
        let mut p = t.to_be_bytes().to_vec();
        p.extend_from_slice(&a.to_be_bytes());
        if let Some(b) = b {
            p.extend_from_slice(&b.to_be_bytes());
        }
        control(MessageType::USER_CONTROL, p)
    }
}

/// A command message (§7.1.1): name, transaction id, command object, arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    /// The procedure name (`connect`, `publish`, `_result`, ...).
    pub name: String,
    /// Transaction id (0 when no reply is expected).
    pub transaction: f64,
    /// The command object (null if absent).
    pub object: Amf0Value,
    /// Further arguments.
    pub args: Vec<Amf0Value>,
}

/// A command message that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// The AMF0 body is malformed.
    #[error("command body: {0}")]
    Amf(#[from] AmfError),
    /// It does not start with a name and a transaction id.
    #[error("command does not start with a name and a transaction id")]
    Shape,
    /// An AMF3 command (type 17) whose body does not start with the AMF0 format byte.
    #[error("AMF3-encoded command messages are not supported")]
    Amf3,
}

impl Command {
    /// Parse a command message body. For type 17 (`amf3`), the body is a format byte, which
    /// must be 0 (AMF0 follows; this is what every encoder that uses type 17 sends), then AMF0
    /// values; an AMF3 value inside is refused.
    pub fn parse(payload: &[u8], amf3: bool, limits: amf0::Limits) -> Result<Self, CommandError> {
        let body = if amf3 {
            match payload.split_first() {
                Some((0, rest)) => rest,
                _ => return Err(CommandError::Amf3),
            }
        } else {
            payload
        };
        let mut v = amf0::decode_all(body, limits)?.into_iter();
        let name = match v.next() {
            Some(Amf0Value::String(s)) => s,
            _ => return Err(CommandError::Shape),
        };
        let transaction = match v.next() {
            Some(Amf0Value::Number(n)) => n,
            // Some encoders omit everything after the name of a notification.
            None => 0.0,
            _ => return Err(CommandError::Shape),
        };
        let object = v.next().unwrap_or(Amf0Value::Null);
        Ok(Self {
            name,
            transaction,
            object,
            args: v.collect(),
        })
    }

    /// The message carrying this command (AMF0, type 20).
    pub fn to_message(&self, csid: ChunkStreamId, stream: MessageStreamId) -> Message {
        let mut p = Vec::new();
        amf0::encode(&Amf0Value::String(self.name.clone()), &mut p);
        amf0::encode(&Amf0Value::Number(self.transaction), &mut p);
        amf0::encode(&self.object, &mut p);
        for a in &self.args {
            amf0::encode(a, &mut p);
        }
        Message {
            csid,
            timestamp: RtmpTime(0),
            type_id: MessageType::COMMAND_AMF0,
            stream,
            payload: p,
        }
    }
}

/// An `onStatus` information object (§7.2.2): level, code, description.
pub fn status(level: &str, code: &str, description: &str) -> Amf0Value {
    Amf0Value::object([
        ("level", Amf0Value::string(level)),
        ("code", Amf0Value::string(code)),
        ("description", Amf0Value::string(description)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_control_round_trip() {
        for e in [
            UserControl::StreamBegin(MessageStreamId(1)),
            UserControl::SetBufferLength(MessageStreamId(1), 3000),
            UserControl::PingRequest(77),
            UserControl::PingResponse(77),
        ] {
            assert_eq!(UserControl::parse(&e.to_message().payload), Some(e));
        }
        assert_eq!(UserControl::parse(&[0, 6, 1]), None);
    }

    #[test]
    fn command_round_trip_and_amf3_wrapper() {
        let c = Command {
            name: "connect".into(),
            transaction: 1.0,
            object: Amf0Value::object([("app", Amf0Value::string("live"))]),
            args: vec![],
        };
        let m = c.to_message(ChunkStreamId::COMMAND, MessageStreamId(0));
        assert_eq!(
            Command::parse(&m.payload, false, amf0::Limits::default()).unwrap(),
            c
        );
        let mut wrapped = vec![0];
        wrapped.extend_from_slice(&m.payload);
        assert_eq!(
            Command::parse(&wrapped, true, amf0::Limits::default()).unwrap(),
            c
        );
        assert_eq!(
            Command::parse(&m.payload, true, amf0::Limits::default()),
            Err(CommandError::Amf3)
        );
        assert_eq!(
            Command::parse(&[0x05], false, amf0::Limits::default()),
            Err(CommandError::Shape)
        );
    }
}
