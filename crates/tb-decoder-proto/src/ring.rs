//! The input ring: compressed packets travel to the worker through a shared byte ring.
//!
//! Positions are absolute byte counts since the ring was made (`u64`, never wrapping in
//! practice: 2^64 bytes); the offset in the mapping is `pos % cap`. A packet is contiguous in
//! the mapping: when it does not fit before the end, the tail is skipped and it starts at
//! offset 0. The parent ([`InputRing`]) is the only writer and reserves space; the worker reads
//! packets in order and returns space with `Consumed { upto }`, the only credit there is. The
//! parent never waits: a packet that does not fit is refused ([`InputRing::reserve`] returns
//! `None`) and the caller drops and counts it.
//!
//! Why a ring and not the socket: a key frame of a 30 Mbit/s stream can be a megabyte or more,
//! more than a socket's buffer, so inline packets would need fragmentation and a queue in the
//! parent; here a packet is one copy into the ring and a 40-byte message.

/// Errors of ring accounting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RingError {
    /// The ring's capacity is zero or not representable.
    #[error("ring capacity {0} is not usable")]
    Capacity(u64),
    /// The worker returned space it was never given, or went backwards.
    #[error("consumed position {upto} outside [{release}, {write}]")]
    Consumed {
        /// Claimed.
        upto: u64,
        /// Released so far.
        release: u64,
        /// Written so far.
        write: u64,
    },
    /// A packet reference does not lie inside the ring.
    #[error("packet at {pos} of {len} bytes does not lie in a ring of {cap}")]
    Span {
        /// Position.
        pos: u64,
        /// Length.
        len: u32,
        /// Capacity.
        cap: u64,
    },
}

/// Where a reserved packet goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Span {
    /// Absolute position of the first byte.
    pub pos: u64,
    /// Offset in the mapping.
    pub offset: usize,
    /// Bytes.
    pub len: u32,
    /// The write position before this reservation (for [`InputRing::unreserve`]).
    prev: u64,
}

/// The parent's accounting of the input ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRing {
    cap: u64,
    write: u64,
    release: u64,
}

impl InputRing {
    /// A ring of `cap` bytes.
    pub fn new(cap: u64) -> Result<Self, RingError> {
        if cap == 0 || usize::try_from(cap).is_err() {
            return Err(RingError::Capacity(cap));
        }
        Ok(Self {
            cap,
            write: 0,
            release: 0,
        })
    }

    /// Capacity in bytes.
    pub fn capacity(&self) -> u64 {
        self.cap
    }

    /// Largest packet the ring takes: half its capacity, so that a packet always fits once the
    /// worker has caught up, wherever the write position is.
    pub fn max_packet(&self) -> u64 {
        self.cap / 2
    }

    /// Bytes written and not yet consumed (including skipped tails).
    pub fn pending(&self) -> u64 {
        self.write - self.release
    }

    /// Absolute write position.
    pub fn write_pos(&self) -> u64 {
        self.write
    }

    /// Space for `len` bytes, or `None` if the ring is too full now (or the packet is larger
    /// than [`InputRing::max_packet`], or empty).
    pub fn reserve(&mut self, len: u32) -> Option<Span> {
        let n = u64::from(len);
        if n == 0 || n > self.max_packet() {
            return None;
        }
        let off = self.write % self.cap;
        let pos = if off + n > self.cap {
            self.write + (self.cap - off)
        } else {
            self.write
        };
        let end = pos + n;
        if end - self.release > self.cap {
            return None;
        }
        let prev = self.write;
        self.write = end;
        Some(Span {
            pos,
            offset: (pos % self.cap) as usize,
            len,
            prev,
        })
    }

    /// Takes back the most recent reservation (its message could not be sent). Returns false,
    /// and changes nothing, if `span` is not the most recent one.
    pub fn unreserve(&mut self, span: Span) -> bool {
        if span.pos + u64::from(span.len) != self.write {
            return false;
        }
        self.write = span.prev;
        true
    }

    /// The worker has consumed everything before `upto`.
    pub fn consumed(&mut self, upto: u64) -> Result<(), RingError> {
        if upto < self.release || upto > self.write {
            return Err(RingError::Consumed {
                upto,
                release: self.release,
                write: self.write,
            });
        }
        self.release = upto;
        Ok(())
    }

    /// Forgets everything in flight (a new worker starts with an empty ring).
    pub fn reset(&mut self) {
        self.write = 0;
        self.release = 0;
    }
}

/// The worker's side: the byte range of a packet in a ring of `cap` bytes, checked to lie
/// inside it without wrapping.
pub fn locate(cap: u64, pos: u64, len: u32) -> Result<std::ops::Range<usize>, RingError> {
    let err = RingError::Span { pos, len, cap };
    if cap == 0 {
        return Err(err);
    }
    let off = pos % cap;
    let end = off.checked_add(u64::from(len)).ok_or(err.clone())?;
    if end > cap || len == 0 {
        return Err(err);
    }
    let off = usize::try_from(off).map_err(|_| err.clone())?;
    let end = usize::try_from(end).map_err(|_| err)?;
    Ok(off..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_wraps_by_skipping_the_tail() {
        let mut r = InputRing::new(100).unwrap();
        let a = r.reserve(40).unwrap();
        assert_eq!((a.pos, a.offset), (0, 0));
        let b = r.reserve(40).unwrap();
        assert_eq!((b.pos, b.offset), (40, 40));
        // 20 left at the end, 0 free at the start: no room
        assert!(r.reserve(30).is_none());
        r.consumed(40).unwrap();
        // the tail (80..100) is skipped; the packet goes to offset 0
        let c = r.reserve(30).unwrap();
        assert_eq!((c.pos, c.offset), (100, 0));
        assert_eq!(r.pending(), 130 - 40);
        assert_eq!(locate(100, c.pos, c.len).unwrap(), 0..30);
    }

    #[test]
    fn size_limits() {
        let mut r = InputRing::new(100).unwrap();
        assert!(r.reserve(0).is_none());
        assert!(r.reserve(51).is_none());
        assert!(r.reserve(50).is_some());
        assert!(InputRing::new(0).is_err());
    }

    #[test]
    fn a_packet_of_half_the_ring_always_fits_when_drained() {
        for cap in [7u64, 100, 101, 4096] {
            let mut r = InputRing::new(cap).unwrap();
            for len in 1..=(cap / 2) as u32 {
                for _ in 0..3 {
                    let s = r.reserve(len).unwrap();
                    assert!(locate(cap, s.pos, s.len).is_ok());
                    r.consumed(r.write_pos()).unwrap();
                }
            }
        }
    }

    #[test]
    fn unreserve_only_the_last() {
        let mut r = InputRing::new(100).unwrap();
        let a = r.reserve(10).unwrap();
        let b = r.reserve(10).unwrap();
        assert!(!r.unreserve(a));
        assert!(r.unreserve(b));
        assert_eq!(r.write_pos(), 10);
    }

    #[test]
    fn consumed_is_checked() {
        let mut r = InputRing::new(100).unwrap();
        let _ = r.reserve(10).unwrap();
        assert!(r.consumed(11).is_err());
        r.consumed(5).unwrap();
        assert!(r.consumed(4).is_err());
        r.consumed(10).unwrap();
    }

    #[test]
    fn locate_refuses_what_crosses_the_end() {
        assert!(locate(100, 90, 20).is_err());
        assert!(locate(100, 190, 10).is_ok());
        assert!(locate(100, u64::MAX, u32::MAX).is_err());
        assert!(locate(0, 0, 1).is_err());
        assert!(locate(100, 0, 0).is_err());
    }
}
