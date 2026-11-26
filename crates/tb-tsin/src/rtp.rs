//! RTP (RFC 3550) as a carrier of MPEG-2 transport streams (RFC 2250 §2, payload type 33).
//!
//! - [`RtpPacket::parse`]: the fixed header (§5.1), CSRC list, header extension (§5.3.1) and
//!   padding, validated as §A.1 recommends (version 2, lengths that fit).
//! - [`RtpReceiver`]: sequence numbers put back in order through a small bounded window, with
//!   loss, duplicate and late counts. A packet is released when everything before it has
//!   arrived, when the window is full, or when the oldest held packet has waited
//!   [`RtpConfig::max_delay`]; what never arrived is counted lost. A new SSRC, or a sequence
//!   jump beyond §A.1's MAX_DROPOUT (3000 ahead) / MAX_MISORDER (100 behind) confirmed by a
//!   second packet in sequence, starts a new session ([`RtpOutput::NewSession`]).
//! - The first packet of a session defines its start: an earlier sequence number arriving
//!   after it is late (dropped). No start-up delay is spent waiting for one; PSI repeats, so
//!   the demuxer recovers at the next PAT/PMT and key frame.

use std::collections::VecDeque;
use std::time::Duration;

/// A parsed RTP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    /// Marker bit.
    pub marker: bool,
    /// Payload type.
    pub payload_type: u8,
    /// Sequence number.
    pub sequence: u16,
    /// Timestamp (90 kHz for MP2T, RFC 2250 §2: the transmission time of the first byte).
    pub timestamp: u32,
    /// Synchronization source.
    pub ssrc: u32,
    /// The payload, without padding.
    pub payload: &'a [u8],
}

/// Why a datagram is not an RTP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RtpError {
    /// Shorter than the fixed header.
    #[error("shorter than the RTP header")]
    Short,
    /// Version is not 2.
    #[error("RTP version {0}")]
    Version(u8),
    /// CSRC list, extension or padding longer than the packet.
    #[error("RTP header fields overrun the packet")]
    Overrun,
}

impl<'a> RtpPacket<'a> {
    /// Parse a datagram (RFC 3550 §5.1, §5.3.1).
    pub fn parse(d: &'a [u8]) -> Result<Self, RtpError> {
        if d.len() < 12 {
            return Err(RtpError::Short);
        }
        let v = d[0] >> 6;
        if v != 2 {
            return Err(RtpError::Version(v));
        }
        let padding = d[0] & 0x20 != 0;
        let extension = d[0] & 0x10 != 0;
        let cc = usize::from(d[0] & 0x0F);
        let mut start = 12 + 4 * cc;
        if extension {
            let e = d.get(start..start + 4).ok_or(RtpError::Overrun)?;
            start += 4 + 4 * usize::from(u16::from_be_bytes([e[2], e[3]]));
        }
        let mut end = d.len();
        if padding {
            // The last octet counts the padding, itself included; zero is invalid.
            let n = usize::from(d[end - 1]);
            if n == 0 || n > end {
                return Err(RtpError::Overrun);
            }
            end -= n;
        }
        let payload = d.get(start..end).ok_or(RtpError::Overrun)?;
        Ok(Self {
            marker: d[1] & 0x80 != 0,
            payload_type: d[1] & 0x7F,
            sequence: u16::from_be_bytes([d[2], d[3]]),
            timestamp: u32::from_be_bytes([d[4], d[5], d[6], d[7]]),
            ssrc: u32::from_be_bytes([d[8], d[9], d[10], d[11]]),
            payload,
        })
    }
}

/// The payload type of MPEG-2 transport streams (RFC 3551 Table 5, RFC 2250).
pub const PT_MP2T: u8 = 33;

/// Receiver settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpConfig {
    /// Packets held for reordering (at most 256).
    pub window: usize,
    /// Longest a packet is held waiting for an earlier one.
    pub max_delay: Duration,
    /// Accepted payload type: 33, or a dynamic type agreed out of band.
    pub payload_type: u8,
}

impl Default for RtpConfig {
    fn default() -> Self {
        Self {
            window: 64,
            max_delay: Duration::from_millis(50),
            payload_type: PT_MP2T,
        }
    }
}

/// Counters (RFC 3550 §6.4.1 terms where they exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RtpStats {
    /// Packets accepted.
    pub received: u64,
    /// Sequence numbers never received (released past).
    pub lost: u64,
    /// Duplicates dropped.
    pub duplicates: u64,
    /// Packets that arrived after their place was released (dropped).
    pub late: u64,
    /// Packets that arrived out of order and were put back.
    pub reordered: u64,
    /// Datagrams that were not RTP, or of another payload type.
    pub rejected: u64,
    /// Sessions started (SSRC changes and sequence restarts).
    pub sessions: u64,
}

/// What the receiver hands on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtpOutput {
    /// A new sender or a restarted sequence: what follows belongs to a new session.
    NewSession {
        /// The new SSRC.
        ssrc: u32,
    },
    /// A payload, in sequence order. `lost_before` sequence numbers were skipped before it.
    Payload {
        /// The payload bytes.
        data: Vec<u8>,
        /// Packets lost just before this one.
        lost_before: u64,
    },
}

const MAX_DROPOUT: u16 = 3000;
const MAX_MISORDER: u16 = 100;

#[derive(Debug, Clone)]
struct Slot {
    arrival: Duration,
    data: Vec<u8>,
}

/// Puts one RTP flow back in order.
#[derive(Debug, Clone)]
pub struct RtpReceiver {
    config: RtpConfig,
    ssrc: Option<u32>,
    /// The next sequence number to release.
    next: u16,
    /// Slot `i` holds sequence `next + i`.
    window: VecDeque<Option<Slot>>,
    /// The sequence number a confirming packet would have after a jump (§A.1 bad_seq).
    bad_seq: Option<u16>,
    lost_pending: u64,
    stats: RtpStats,
}

impl RtpReceiver {
    /// A receiver with no source yet.
    pub fn new(config: RtpConfig) -> Self {
        Self {
            config: RtpConfig {
                window: config.window.clamp(1, 256),
                ..config
            },
            ssrc: None,
            next: 0,
            window: VecDeque::new(),
            bad_seq: None,
            lost_pending: 0,
            stats: RtpStats::default(),
        }
    }

    /// Counters.
    pub fn stats(&self) -> RtpStats {
        self.stats
    }

    /// Count a datagram that was not accepted upstream.
    pub fn reject(&mut self) {
        self.stats.rejected += 1;
    }

    /// One datagram arrived at `now`.
    pub fn on_datagram(&mut self, now: Duration, d: &[u8], out: &mut Vec<RtpOutput>) {
        let p = match RtpPacket::parse(d) {
            Ok(p) if p.payload_type == self.config.payload_type => p,
            _ => {
                self.stats.rejected += 1;
                return;
            }
        };
        if self.ssrc != Some(p.ssrc) {
            self.restart(p.ssrc, p.sequence, out);
        }
        let d = p.sequence.wrapping_sub(self.next);
        let behind = self.next.wrapping_sub(p.sequence);
        if d >= MAX_DROPOUT && behind > MAX_MISORDER {
            // A jump: accept it as a restart only if the next packet follows it (§A.1).
            if self.bad_seq == Some(p.sequence) {
                self.restart(p.ssrc, p.sequence, out);
            } else {
                self.bad_seq = Some(p.sequence.wrapping_add(1));
                self.stats.rejected += 1;
                return;
            }
        } else if d >= MAX_DROPOUT {
            // Behind: already released.
            self.stats.late += 1;
            return;
        }
        self.bad_seq = None;
        let mut d = usize::from(p.sequence.wrapping_sub(self.next));
        // Make room: release (and count lost) from the head until it fits.
        while d >= self.config.window {
            self.pop_head(out);
            d -= 1;
        }
        while self.window.len() <= d {
            self.window.push_back(None);
        }
        match &mut self.window[d] {
            Some(_) => {
                self.stats.duplicates += 1;
                return;
            }
            slot => {
                *slot = Some(Slot {
                    arrival: now,
                    data: p.payload.to_vec(),
                });
            }
        }
        self.stats.received += 1;
        if d + 1 < self.window.len() {
            self.stats.reordered += 1;
        }
        self.release_ready(out);
        self.on_tick(now, out);
    }

    /// Time passes: release packets held longer than `max_delay` (counting the gaps before
    /// them lost).
    pub fn on_tick(&mut self, now: Duration, out: &mut Vec<RtpOutput>) {
        while let Some(oldest) = self.window.iter().flatten().map(|s| s.arrival).min() {
            if now.saturating_sub(oldest) < self.config.max_delay {
                break;
            }
            // Skip the gap up to the first held packet.
            while matches!(self.window.front(), Some(None)) {
                self.pop_head(out);
            }
            self.release_ready(out);
        }
    }

    fn restart(&mut self, ssrc: u32, seq: u16, out: &mut Vec<RtpOutput>) {
        self.ssrc = Some(ssrc);
        self.next = seq;
        self.window.clear();
        self.bad_seq = None;
        self.lost_pending = 0;
        self.stats.sessions += 1;
        out.push(RtpOutput::NewSession { ssrc });
    }

    fn pop_head(&mut self, out: &mut Vec<RtpOutput>) {
        match self.window.pop_front() {
            Some(Some(s)) => out.push(RtpOutput::Payload {
                data: s.data,
                lost_before: std::mem::take(&mut self.lost_pending),
            }),
            Some(None) | None => {
                self.stats.lost += 1;
                self.lost_pending += 1;
            }
        }
        self.next = self.next.wrapping_add(1);
    }

    fn release_ready(&mut self, out: &mut Vec<RtpOutput>) {
        while matches!(self.window.front(), Some(Some(_))) {
            self.pop_head(out);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn rtp(seq: u16, ssrc: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x80, PT_MP2T];
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&ssrc.to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn payloads(out: &[RtpOutput]) -> Vec<(u8, u64)> {
        out.iter()
            .filter_map(|o| match o {
                RtpOutput::Payload { data, lost_before } => Some((data[0], *lost_before)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn header_fields() {
        // V=2, P, X, CC=1, M, PT 33; one CSRC; a 1-word extension; 3 bytes of padding.
        let mut d = vec![0xB1, 0x80 | 33, 0, 7, 0, 0, 0, 9, 0, 0, 0, 5];
        d.extend_from_slice(&[1, 2, 3, 4]); // CSRC
        d.extend_from_slice(&[0xBE, 0xDE, 0, 1, 9, 9, 9, 9]); // extension
        d.extend_from_slice(&[0x47, 0x00]);
        d.extend_from_slice(&[0, 0, 3]);
        let p = RtpPacket::parse(&d).unwrap();
        assert!(p.marker);
        assert_eq!(
            (p.payload_type, p.sequence, p.timestamp, p.ssrc),
            (33, 7, 9, 5)
        );
        assert_eq!(p.payload, &[0x47, 0x00]);
        assert_eq!(RtpPacket::parse(&d[..11]), Err(RtpError::Short));
        let mut bad = d.clone();
        *bad.last_mut().unwrap() = 200;
        assert_eq!(RtpPacket::parse(&bad), Err(RtpError::Overrun));
        bad[0] = 0x40;
        assert_eq!(RtpPacket::parse(&bad), Err(RtpError::Version(1)));
    }

    #[test]
    fn reorders_counts_and_releases_on_time() {
        let mut r = RtpReceiver::new(RtpConfig::default());
        let mut out = Vec::new();
        let t = Duration::ZERO;
        for s in [10u16, 12, 12, 11, 14] {
            r.on_datagram(t, &rtp(s, 1, &[s as u8]), &mut out);
        }
        assert!(matches!(out[0], RtpOutput::NewSession { ssrc: 1 }));
        assert_eq!(payloads(&out), [(10, 0), (11, 0), (12, 0)]);
        assert_eq!(r.stats().duplicates, 1);
        assert_eq!(r.stats().reordered, 1);
        // 13 never comes: 14 is released after max_delay, with one lost before it.
        r.on_tick(Duration::from_millis(49), &mut out);
        assert_eq!(payloads(&out).len(), 3);
        r.on_tick(Duration::from_millis(50), &mut out);
        assert_eq!(payloads(&out)[3], (14, 1));
        // 13 arriving now is late.
        r.on_datagram(Duration::from_millis(60), &rtp(13, 1, &[13]), &mut out);
        assert_eq!(r.stats().late, 1);
        assert_eq!(r.stats().lost, 1);
    }

    #[test]
    fn window_overflow_and_wrap() {
        let mut r = RtpReceiver::new(RtpConfig {
            window: 4,
            ..RtpConfig::default()
        });
        let mut out = Vec::new();
        let t = Duration::ZERO;
        r.on_datagram(t, &rtp(65_534, 1, &[0]), &mut out);
        // 65535 missing; 0, 1, 2 held; 3 forces the head out.
        for s in [0u16, 1, 2, 3] {
            r.on_datagram(t, &rtp(s, 1, &[s as u8 + 1]), &mut out);
        }
        assert_eq!(payloads(&out), [(0, 0), (1, 1), (2, 0), (3, 0), (4, 0)]);
    }

    #[test]
    fn ssrc_change_and_confirmed_jump_restart() {
        let mut r = RtpReceiver::new(RtpConfig::default());
        let mut out = Vec::new();
        let t = Duration::ZERO;
        r.on_datagram(t, &rtp(100, 1, &[1]), &mut out);
        r.on_datagram(t, &rtp(5, 2, &[2]), &mut out);
        let sessions = |o: &[RtpOutput]| {
            o.iter()
                .filter(|x| matches!(x, RtpOutput::NewSession { .. }))
                .count()
        };
        assert_eq!(sessions(&out), 2);
        // A single wild packet is dropped; two in sequence restart.
        r.on_datagram(t, &rtp(30_000, 2, &[3]), &mut out);
        assert_eq!(sessions(&out), 2);
        r.on_datagram(t, &rtp(6, 2, &[4]), &mut out);
        r.on_datagram(t, &rtp(40_000, 2, &[5]), &mut out);
        r.on_datagram(t, &rtp(40_001, 2, &[6]), &mut out);
        assert_eq!(sessions(&out), 3);
        assert_eq!(payloads(&out), [(1, 0), (2, 0), (4, 0), (6, 0)]);
    }
}
