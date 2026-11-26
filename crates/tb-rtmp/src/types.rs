//! Identifiers and timestamps of the chunk layer (Adobe RTMP 1.0 §5.3), as newtypes.

use std::fmt;

/// A chunk stream id (§5.3.1.1). Valid ids are 2 to 65599: 0 and 1 are the escape values of
/// the basic header, and 2 is reserved for protocol control messages (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkStreamId(u32);

/// A chunk stream id outside 2..=65599.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("chunk stream id {0} is outside 2..=65599")]
pub struct InvalidChunkStreamId(pub u32);

impl ChunkStreamId {
    /// The smallest id.
    pub const MIN: u32 = 2;
    /// The largest id: a 3-byte basic header carries `65535 + 64`.
    pub const MAX: u32 = 65_599;
    /// Protocol control messages and user control messages (§5.4, §6.2).
    pub const CONTROL: Self = Self(2);
    /// Where this server sends commands (any id from 3 up is valid; 3 is customary).
    pub const COMMAND: Self = Self(3);

    /// Validating constructor.
    pub const fn new(id: u32) -> Result<Self, InvalidChunkStreamId> {
        if id < Self::MIN || id > Self::MAX {
            return Err(InvalidChunkStreamId(id));
        }
        Ok(Self(id))
    }
    /// The id.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ChunkStreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "csid {}", self.0)
    }
}

/// A message stream id (§5.3.1.2.1): any 32-bit value; 0 is the NetConnection (control) stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct MessageStreamId(pub u32);

impl MessageStreamId {
    /// The NetConnection stream, where connect and createStream travel.
    pub const CONTROL: Self = Self(0);
}

/// An RTMP timestamp: milliseconds modulo 2^32 (§5.3.1.2.1, "timestamps ... roll over").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RtmpTime(pub u32);

impl RtmpTime {
    /// `self + delta`, modulo 2^32 (§5.3.1.2.2: deltas are added with wrap-around).
    pub const fn add(self, delta: u32) -> Self {
        Self(self.0.wrapping_add(delta))
    }
    /// The forward distance from `earlier` to `self` modulo 2^32, if it is less than 2^31
    /// (the half-range rule of serial number arithmetic: otherwise `self` is earlier).
    pub const fn since(self, earlier: Self) -> Option<u32> {
        let d = self.0.wrapping_sub(earlier.0);
        if d < 1 << 31 { Some(d) } else { None }
    }
}

/// A message type id (§5.4, §6.2, §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageType(pub u8);

impl MessageType {
    /// Set Chunk Size (§5.4.1).
    pub const SET_CHUNK_SIZE: Self = Self(1);
    /// Abort Message (§5.4.2).
    pub const ABORT: Self = Self(2);
    /// Acknowledgment (§5.4.3).
    pub const ACKNOWLEDGMENT: Self = Self(3);
    /// User Control Message (§6.2, §7.1.7).
    pub const USER_CONTROL: Self = Self(4);
    /// Window Acknowledgment Size (§5.4.4).
    pub const WINDOW_ACK_SIZE: Self = Self(5);
    /// Set Peer Bandwidth (§5.4.5).
    pub const SET_PEER_BANDWIDTH: Self = Self(6);
    /// Audio message (§7.1.4): one FLV audio tag body.
    pub const AUDIO: Self = Self(8);
    /// Video message (§7.1.5): one FLV video tag body.
    pub const VIDEO: Self = Self(9);
    /// Data message, AMF3 (§7.1.2).
    pub const DATA_AMF3: Self = Self(15);
    /// Shared object message, AMF3 (§7.1.3).
    pub const SHARED_OBJECT_AMF3: Self = Self(16);
    /// Command message, AMF3 (§7.1.1).
    pub const COMMAND_AMF3: Self = Self(17);
    /// Data message, AMF0 (§7.1.2).
    pub const DATA_AMF0: Self = Self(18);
    /// Shared object message, AMF0 (§7.1.3).
    pub const SHARED_OBJECT_AMF0: Self = Self(19);
    /// Command message, AMF0 (§7.1.1).
    pub const COMMAND_AMF0: Self = Self(20);
    /// Aggregate message (§7.1.6): a run of FLV tags.
    pub const AGGREGATE: Self = Self(22);
}

/// One complete RTMP message (§5.3.1, §6.1): its header fields and payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The chunk stream it traveled on.
    pub csid: ChunkStreamId,
    /// Its timestamp.
    pub timestamp: RtmpTime,
    /// Its type.
    pub type_id: MessageType,
    /// Its message stream.
    pub stream: MessageStreamId,
    /// The payload.
    pub payload: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_validate() {
        assert!(ChunkStreamId::new(0).is_err());
        assert!(ChunkStreamId::new(1).is_err());
        assert_eq!(ChunkStreamId::new(2).unwrap(), ChunkStreamId::CONTROL);
        assert!(ChunkStreamId::new(65_599).is_ok());
        assert!(ChunkStreamId::new(65_600).is_err());
    }

    #[test]
    fn time_wraps() {
        assert_eq!(RtmpTime(u32::MAX).add(2), RtmpTime(1));
        assert_eq!(RtmpTime(1).since(RtmpTime(u32::MAX)), Some(2));
        assert_eq!(RtmpTime(u32::MAX).since(RtmpTime(1)), None);
    }
}
