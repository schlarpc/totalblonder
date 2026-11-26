//! The transport packet layer (H.222.0 §2.4.3.2, Table 2-2; adaptation field §2.4.3.4,
//! Table 2-6) and byte-stream framing with sync acquisition.

/// Size of a transport packet.
pub const PACKET_SIZE: usize = 188;
/// The sync byte (§2.4.3.3).
pub const SYNC: u8 = 0x47;

/// A packet identifier: 13 bits (§2.4.3.3, Table 2-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(u16);

/// A value above 0x1FFF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("PID {0:#x} is above 0x1FFF")]
pub struct InvalidPid(pub u16);

impl Pid {
    /// The program association table (Table 2-3).
    pub const PAT: Self = Self(0);
    /// Null packets.
    pub const NULL: Self = Self(0x1FFF);
    /// Validating constructor.
    pub const fn new(v: u16) -> Result<Self, InvalidPid> {
        if v > 0x1FFF {
            return Err(InvalidPid(v));
        }
        Ok(Self(v))
    }
    /// The 13-bit value.
    pub const fn get(self) -> u16 {
        self.0
    }
    /// From the two header bytes that hold it (masking makes it valid).
    pub(crate) const fn from_bits(hi: u8, lo: u8) -> Self {
        Self((((hi & 0x1F) as u16) << 8) | lo as u16)
    }
}

/// The adaptation field's flags that matter to a receiver (Table 2-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Adaptation {
    /// discontinuity_indicator: the continuity counter (and the time base) may jump.
    pub discontinuity: bool,
    /// random_access_indicator: the next PES of this PID starts at a random access point.
    pub random_access: bool,
}

/// A parsed transport packet header and its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TsPacket<'a> {
    /// transport_error_indicator.
    pub error: bool,
    /// payload_unit_start_indicator.
    pub unit_start: bool,
    /// The PID.
    pub pid: Pid,
    /// transport_scrambling_control is non-zero.
    pub scrambled: bool,
    /// continuity_counter.
    pub cc: u8,
    /// The packet has a payload (adaptation_field_control 01 or 11).
    pub has_payload: bool,
    /// The adaptation field, if present.
    pub adaptation: Option<Adaptation>,
    /// The payload (empty if none).
    pub payload: &'a [u8],
}

/// Why a packet cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    /// The first byte is not 0x47.
    #[error("no sync byte")]
    Sync,
    /// adaptation_field_control 00 (reserved) or an adaptation field longer than the packet.
    #[error("malformed adaptation field")]
    Adaptation,
}

impl<'a> TsPacket<'a> {
    /// Parse one 188-byte packet.
    pub fn parse(p: &'a [u8; PACKET_SIZE]) -> Result<Self, PacketError> {
        if p[0] != SYNC {
            return Err(PacketError::Sync);
        }
        let afc = (p[3] >> 4) & 3;
        let (adaptation, start) = match afc {
            0 => return Err(PacketError::Adaptation),
            1 => (None, 4),
            _ => {
                let len = usize::from(p[4]);
                // §2.4.3.5: 0..=182 with a payload, exactly 183 without.
                let max = if afc == 3 { 182 } else { 183 };
                if len > max || (afc == 2 && len != 183) {
                    return Err(PacketError::Adaptation);
                }
                let a = if len > 0 {
                    Adaptation {
                        discontinuity: p[5] & 0x80 != 0,
                        random_access: p[5] & 0x40 != 0,
                    }
                } else {
                    Adaptation::default()
                };
                (Some(a), 5 + len)
            }
        };
        let has_payload = afc & 1 != 0;
        Ok(Self {
            error: p[1] & 0x80 != 0,
            unit_start: p[1] & 0x40 != 0,
            pid: Pid::from_bits(p[1], p[2]),
            scrambled: p[3] & 0xC0 != 0,
            cc: p[3] & 0x0F,
            has_payload,
            adaptation,
            payload: if has_payload { &p[start..] } else { &[] },
        })
    }
}

/// Byte stream → 188-byte packets. Sync is acquired when three sync bytes are found 188 bytes
/// apart and lost when a packet does not start with one (§2.4.3.3 leaves sync acquisition to
/// the receiver; three is the customary count). Holds at most 3 × 188 bytes between calls.
#[derive(Debug, Clone, Default)]
pub struct Framer {
    buf: Vec<u8>,
    synced: bool,
    /// Bytes skipped while looking for sync.
    pub skipped: u64,
    /// Times sync was lost.
    pub sync_losses: u64,
}

impl Framer {
    /// A framer without sync.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; `f` gets each packet in order.
    pub fn push(&mut self, input: &[u8], mut f: impl FnMut(&[u8; PACKET_SIZE])) {
        self.buf.extend_from_slice(input);
        let mut pos = 0;
        loop {
            if !self.synced {
                let b = &self.buf[pos..];
                let found = (0..b.len().saturating_sub(2 * PACKET_SIZE)).find(|&i| {
                    b[i] == SYNC && b[i + PACKET_SIZE] == SYNC && b[i + 2 * PACKET_SIZE] == SYNC
                });
                match found {
                    Some(i) => {
                        self.skipped += i as u64;
                        pos += i;
                        self.synced = true;
                    }
                    None => {
                        // Keep the last two packets' worth (a sync byte may be among them).
                        let keep = b.len().min(2 * PACKET_SIZE);
                        self.skipped += (b.len() - keep) as u64;
                        pos += b.len() - keep;
                        break;
                    }
                }
            }
            while self.buf.len() - pos >= PACKET_SIZE {
                if self.buf[pos] != SYNC {
                    self.synced = false;
                    self.sync_losses += 1;
                    break;
                }
                if let Ok(p) = <&[u8; PACKET_SIZE]>::try_from(&self.buf[pos..pos + PACKET_SIZE]) {
                    f(p);
                }
                pos += PACKET_SIZE;
            }
            if self.synced {
                break;
            }
        }
        self.buf.drain(..pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(pid: u16, cc: u8, fill: u8) -> [u8; PACKET_SIZE] {
        let mut p = [fill; PACKET_SIZE];
        p[0] = SYNC;
        p[1] = 0x40 | (pid >> 8) as u8;
        p[2] = pid as u8;
        p[3] = 0x10 | cc;
        p
    }

    #[test]
    fn parse_header_and_adaptation() {
        let mut p = pkt(0x100, 5, 0xAA);
        let t = TsPacket::parse(&p).unwrap();
        assert_eq!(
            (t.pid.get(), t.cc, t.unit_start, t.payload.len()),
            (0x100, 5, true, 184)
        );
        p[3] = 0x30 | 5;
        p[4] = 7;
        p[5] = 0x40;
        let t = TsPacket::parse(&p).unwrap();
        assert!(t.adaptation.unwrap().random_access);
        assert_eq!(t.payload.len(), 184 - 8);
        p[4] = 183;
        assert_eq!(TsPacket::parse(&p), Err(PacketError::Adaptation));
        p[3] = 0x20;
        let t = TsPacket::parse(&p).unwrap();
        assert!(!t.has_payload && t.payload.is_empty());
        p[3] = 0x00;
        assert_eq!(TsPacket::parse(&p), Err(PacketError::Adaptation));
        p[0] = 0;
        assert_eq!(TsPacket::parse(&p), Err(PacketError::Sync));
        assert!(Pid::new(0x2000).is_err());
    }

    #[test]
    fn framer_acquires_and_resyncs() {
        let mut stream = vec![1, 2, 3, SYNC, 9];
        for cc in 0..5 {
            stream.extend_from_slice(&pkt(0x100, cc, 0x00));
        }
        // A broken packet, then good ones.
        stream.extend_from_slice(&[0x11; 100]);
        for cc in 5..10 {
            stream.extend_from_slice(&pkt(0x100, cc, 0x00));
        }
        for split in [1usize, 7, 188, 1000, stream.len()] {
            let mut f = Framer::new();
            let mut ccs = Vec::new();
            for c in stream.chunks(split) {
                f.push(c, |p| ccs.push(p[3] & 0xF));
            }
            assert_eq!(ccs, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], "split {split}");
            assert_eq!(f.sync_losses, 1);
            assert_eq!(f.skipped, 5 + 100);
        }
    }
}
