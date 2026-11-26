//! The DTVCC packet layer (§5) and service layer (§6): caption channel packets made of service
//! blocks, and their reassembly from `cc_data()` constructs.
//!
//! **Packets** (§5.1, Table 9, p.21-22): a header byte (sequence number in b7-b6,
//! `packet_size_code` in b5-b0) and data; the size is even, 2 to 128 bytes, `packet_size_code`
//! being the size in byte pairs (0 for 64 pairs). The sequence number counts 0 to 3 in
//! transmission order, over the whole caption channel; it is written when the packet is sent
//! ([`Packet::pairs`]), because the scheduler may send packets in another order than it made
//! them.
//!
//! **Service blocks** (§6.2, Table 10, p.24-25): a header (service number in b7-b5, block
//! size 1 to 31 in b4-b0; service 7 means an extended header whose second byte holds the
//! service number 7 to 63) and the data. "Service Blocks shall not cross Caption Channel
//! Packet boundaries" (§6.2.5), "every syntactic element shall be entirely contained in a
//! single DTVCC Caption Channel Packet" (§6.3, p.26); [`Packer`] also keeps every element
//! inside one service block, which §6.3 calls a simplification decoders must not rely on but
//! encoders may adopt. "A Null Service Block Header shall be inserted as the last Service
//! Block in the Caption Channel Packet if space permits" (§6.2.5): a packet whose blocks leave
//! a byte to make its size even ends with the null header 0x00 in that byte; we do not add a
//! byte pair only to hold it.

use super::{ServiceNumber, element_len};

/// Most bytes a packet holds (§5: n <= 128).
pub const MAX_PACKET: usize = 128;

/// A caption channel packet with sequence number 0 in its header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    bytes: Vec<u8>,
}

impl Packet {
    /// The packet's bytes (header first, sequence number 0); the length is even.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Byte pairs it takes in `cc_data()`.
    pub fn len_pairs(&self) -> usize {
        self.bytes.len() / 2
    }

    /// The byte pairs, the header carrying sequence number `seq` (0 to 3).
    pub fn pairs(&self, seq: u8) -> impl Iterator<Item = [u8; 2]> + '_ {
        self.bytes
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .map(move |(i, p)| {
                if i == 0 {
                    [p[0] | ((seq & 3) << 6), p[1]]
                } else {
                    *p
                }
            })
    }

    /// A packet of these bytes (header first). `None` if the length is odd, zero or more
    /// than 128, or the header's size code disagrees with it.
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        let n = bytes.len();
        if n == 0 || !n.is_multiple_of(2) || n > MAX_PACKET {
            return None;
        }
        let code = usize::from(bytes[0] & 0x3F);
        let want = if code == 0 { MAX_PACKET } else { 2 * code };
        (want == n).then_some(Self { bytes })
    }
}

/// Packs the syntactic elements of one service into packets, in order.
#[derive(Clone, Debug)]
pub struct Packer {
    service: ServiceNumber,
    elements: Vec<Vec<u8>>,
    next: usize,
}

impl Packer {
    /// The elements (each a whole command or character, at most 7 bytes) of `service`.
    pub fn new(service: ServiceNumber, elements: Vec<Vec<u8>>) -> Self {
        Self {
            service,
            elements,
            next: 0,
        }
    }

    /// All elements are packed.
    pub fn is_done(&self) -> bool {
        self.next >= self.elements.len()
    }

    fn header_len(&self) -> usize {
        if self.service.get() <= 6 { 1 } else { 2 }
    }

    /// Byte pairs of the smallest packet that holds the next element (`0` when done).
    pub fn min_pairs(&self) -> usize {
        match self.elements.get(self.next) {
            Some(e) => (1 + self.header_len() + e.len()).div_ceil(2),
            None => 0,
        }
    }

    /// Byte pairs all the remaining elements take in packets of at most `max_pairs` each.
    pub fn total_pairs(&self, max_pairs: usize) -> usize {
        let mut p = self.clone();
        let mut n = 0;
        while let Some(k) = p.next_packet(max_pairs) {
            n += k.len_pairs();
        }
        n
    }

    /// The next packet, at most `max_pairs` byte pairs (2 to 64) long and as full as the
    /// elements allow; `None` when done or when the next element does not fit.
    pub fn next_packet(&mut self, max_pairs: usize) -> Option<Packet> {
        let cap = 2 * max_pairs.min(MAX_PACKET / 2);
        let hdr = self.header_len();
        let mut bytes = vec![0u8];
        while self.next < self.elements.len() {
            // one service block: whole elements, at most 31 bytes
            let room = cap.saturating_sub(bytes.len() + hdr).min(31);
            let mut size = 0;
            let mut k = self.next;
            while k < self.elements.len() && size + self.elements[k].len() <= room {
                size += self.elements[k].len();
                k += 1;
            }
            if size == 0 {
                break;
            }
            let sn = self.service.get();
            if sn <= 6 {
                bytes.push((sn << 5) | size as u8);
            } else {
                bytes.push(0xE0 | size as u8);
                bytes.push(sn & 0x3F);
            }
            for e in &self.elements[self.next..k] {
                bytes.extend_from_slice(e);
            }
            self.next = k;
        }
        if bytes.len() == 1 {
            return None;
        }
        if bytes.len() % 2 == 1 {
            // the null service block header, also the byte that makes the size even
            bytes.push(0);
        }
        let pairs = bytes.len() / 2;
        bytes[0] = (pairs % 64) as u8;
        Some(Packet { bytes })
    }
}

/// The elements of a byte string of commands (each command's bytes as
/// [`super::element_len`] delimits them). A truncated last element is dropped.
pub fn split_elements(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let Some(n) = element_len(data[i], data.get(i + 1).copied()) else {
            break;
        };
        if i + n > data.len() {
            break;
        }
        out.push(data[i..i + n].to_vec());
        i += n;
    }
    out
}

/// A service block of a received packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block<'a> {
    /// Service number (0 never appears: the null header ends the list).
    pub service: u8,
    /// The block data.
    pub data: &'a [u8],
}

/// The service blocks of a packet (header byte first). Stops at the null header or at the
/// end; a block that runs over the end is cut there.
pub fn blocks(packet: &[u8]) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < packet.len() {
        let h = packet[i];
        i += 1;
        if h == 0 {
            break;
        }
        let mut sn = h >> 5;
        let size = usize::from(h & 0x1F);
        if sn == 7 && size != 0 {
            let Some(&e) = packet.get(i) else { break };
            sn = e & 0x3F;
            i += 1;
        }
        let end = (i + size).min(packet.len());
        out.push(Block {
            service: sn,
            data: &packet[i..end],
        });
        i = end;
    }
    out
}

/// Reassembles caption channel packets from DTVCC constructs (`cc_type` 3 and 2) in display
/// order. A packet ends when its `packet_size_code` bytes are in or when the next packet
/// starts (a shortened packet, §4.6); padding constructs (`cc_valid` 0) do not end a packet
/// (§4.3.7, Table 8 note 4). Continuation constructs outside a packet are counted and dropped.
#[derive(Clone, Debug, Default)]
pub struct Reassembler {
    open: Option<(Vec<u8>, usize)>,
    last_seq: Option<u8>,
    /// Constructs of `cc_type` 2 that belonged to no packet.
    pub orphans: u64,
    /// Packets whose sequence number did not follow the one before (§5.1).
    pub seq_breaks: u64,
    /// Packets that ended before their declared size.
    pub shortened: u64,
}

impl Reassembler {
    /// A reassembler with nothing open.
    pub fn new() -> Self {
        Self::default()
    }

    /// One construct (`cc_valid`, `cc_type`, the two bytes); complete packets are appended
    /// to `out`.
    pub fn feed(&mut self, valid: bool, ty: u8, data: [u8; 2], out: &mut Vec<Vec<u8>>) {
        if !valid || ty < 2 {
            return;
        }
        if ty == 3 {
            if let Some((p, _)) = self.open.take() {
                self.shortened += 1;
                out.push(p);
            }
            let code = usize::from(data[0] & 0x3F);
            let want = if code == 0 { MAX_PACKET } else { 2 * code };
            let seq = data[0] >> 6;
            if let Some(last) = self.last_seq
                && (last + 1) & 3 != seq
            {
                self.seq_breaks += 1;
            }
            self.last_seq = Some(seq);
            self.open = Some((data.to_vec(), want));
        } else if let Some((p, _)) = self.open.as_mut() {
            p.extend_from_slice(&data);
        } else {
            self.orphans += 1;
            return;
        }
        if let Some((p, want)) = &self.open
            && p.len() >= *want
            && let Some((p, _)) = self.open.take()
        {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Figure 10 (p.26): a 20-byte packet, sequence number 2, with blocks of services 1 (3
    /// bytes), 6 (4 bytes) and extended service 21 (8 bytes).
    #[test]
    fn figure_10() {
        let mut v = vec![0x8A];
        v.push(0x23);
        v.extend([1, 2, 3]);
        v.push(0xC4);
        v.extend([4, 5, 6, 7]);
        v.extend([0xE8, 0x15]);
        v.extend([8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(v.len(), 20);
        assert_eq!(v[0] >> 6, 2);
        assert_eq!(v[0] & 0x3F, 10);
        let b = blocks(&v);
        assert_eq!(
            b,
            [
                Block {
                    service: 1,
                    data: &[1, 2, 3]
                },
                Block {
                    service: 6,
                    data: &[4, 5, 6, 7]
                },
                Block {
                    service: 21,
                    data: &[8, 9, 10, 11, 12, 13, 14, 15]
                },
            ]
        );
        assert!(Packet::from_bytes(v).is_some());
    }

    #[test]
    fn packing() {
        let els: Vec<Vec<u8>> = (0..60u8)
            .map(|i| {
                if i % 5 == 0 {
                    vec![0x92, 1, i]
                } else {
                    vec![b'a' + i % 26]
                }
            })
            .collect();
        for (sn, maxp) in [(1u8, 64usize), (1, 5), (2, 10), (21, 64), (63, 7)] {
            let s = ServiceNumber::new(sn).unwrap();
            let mut p = Packer::new(s, els.clone());
            let total = p.total_pairs(maxp);
            let mut got = Vec::new();
            let mut pairs = 0;
            while let Some(k) = p.next_packet(maxp) {
                assert!(k.len_pairs() <= maxp);
                assert!(Packet::from_bytes(k.bytes().to_vec()).is_some());
                pairs += k.len_pairs();
                for b in blocks(k.bytes()) {
                    assert_eq!(b.service, sn);
                    assert!(!b.data.is_empty() && b.data.len() <= 31);
                    // every block holds whole elements
                    let e = split_elements(b.data);
                    assert_eq!(e.concat(), b.data);
                    got.extend(e);
                }
            }
            assert!(p.is_done());
            assert_eq!(got, els, "service {sn}, {maxp} pairs");
            assert_eq!(pairs, total);
        }
        // a packet too small for the next element
        let mut p = Packer::new(ServiceNumber::PRIMARY, vec![vec![0x98, 0, 0, 0, 0, 0, 0]]);
        assert_eq!(p.min_pairs(), 5);
        assert_eq!(p.next_packet(4), None);
        assert_eq!(p.next_packet(5).map(|k| k.len_pairs()), Some(5));
    }

    #[test]
    fn reassembly() {
        let s = ServiceNumber::PRIMARY;
        let mut p = Packer::new(s, (0..100u8).map(|i| vec![0x20 + i % 90]).collect());
        let mut packets = Vec::new();
        while let Some(k) = p.next_packet(64) {
            packets.push(k);
        }
        let mut r = Reassembler::new();
        let mut out = Vec::new();
        for (n, k) in packets.iter().enumerate() {
            for (i, pair) in k.pairs(n as u8 & 3).enumerate() {
                r.feed(true, if i == 0 { 3 } else { 2 }, pair, &mut out);
                // padding inside does not end it
                r.feed(false, 2, [0, 0], &mut out);
            }
        }
        assert_eq!(out.len(), packets.len());
        for (n, (a, b)) in out.iter().zip(&packets).enumerate() {
            assert_eq!(a[0] >> 6, n as u8 & 3);
            assert_eq!(a[1..], b.bytes()[1..]);
        }
        assert_eq!((r.orphans, r.seq_breaks, r.shortened), (0, 0, 0));
        // a shortened packet, an orphan, a sequence break
        let seq = packets.len() as u8 & 3;
        r.feed(true, 3, [(seq << 6) | 0x05, 0x21], &mut out);
        r.feed(true, 2, [0x41, 0x42], &mut out);
        r.feed(true, 3, [(((seq + 2) & 3) << 6) | 0x01, 0x00], &mut out);
        assert_eq!(out.len(), packets.len() + 2);
        assert_eq!(r.shortened, 1);
        assert_eq!(r.seq_breaks, 1);
        r.feed(true, 2, [1, 2], &mut out);
        assert_eq!(r.orphans, 1);
    }
}
