//! Bit-level output and input, most significant bit first (A/52 §5.2, p.29).
//!
//! The syncframe syntax is written once, generically over [`BitSink`]: [`BitCounter`] runs it to
//! size a frame during the bit allocation search, [`BitWriter`] runs it to emit the frame. The
//! two cannot disagree about how many bits a field takes, because they execute the same code.

/// Something that accepts bit fields.
pub trait BitSink {
    /// Appends the low `n` bits of `value`, most significant first. `n` is at most 32.
    fn put(&mut self, n: u32, value: u32);
    /// Bits accepted so far.
    fn position(&self) -> usize;
}

/// Counts bits without storing them.
#[derive(Debug, Default, Clone, Copy)]
pub struct BitCounter {
    bits: usize,
}

impl BitCounter {
    /// A counter at zero.
    pub fn new() -> Self {
        Self::default()
    }
}

impl BitSink for BitCounter {
    #[inline]
    fn put(&mut self, n: u32, _value: u32) {
        self.bits += n as usize;
    }

    #[inline]
    fn position(&self) -> usize {
        self.bits
    }
}

/// Writes bits into a caller-owned byte buffer of fixed size.
///
/// Writing past the end of the buffer does not panic: the bits are dropped and
/// [`BitWriter::overflowed`] reports it. The encoder sizes every frame before writing it, so an
/// overflow is an internal error that its own checks turn into a test failure. Bits are staged in
/// a small accumulator; [`BitWriter::finish`] writes the last partial byte.
#[derive(Debug)]
pub struct BitWriter<'a> {
    buf: &'a mut [u8],
    /// Whole bytes written to `buf`.
    byte: usize,
    /// Staged bits, right-aligned; fewer than 8 between calls.
    acc: u64,
    nacc: u32,
    overflow: bool,
}

impl<'a> BitWriter<'a> {
    /// A writer at the start of `buf`.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            byte: 0,
            acc: 0,
            nacc: 0,
            overflow: false,
        }
    }

    /// Whether any bit fell past the end of the buffer.
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    /// Advances to `bit` by writing zeros. Does nothing if already there or beyond.
    pub fn zero_fill_to(&mut self, bit: usize) {
        while self.position() < bit {
            let n = (bit - self.position()).min(32);
            self.put(n as u32, 0);
        }
    }

    /// Writes the staged partial byte (padded with zeros) and returns the buffer.
    pub fn finish(mut self) -> &'a mut [u8] {
        if self.nacc > 0 {
            let pad = 8 - self.nacc;
            self.put(pad, 0);
        }
        self.buf
    }

    #[inline]
    fn emit(&mut self, b: u8) {
        match self.buf.get_mut(self.byte) {
            Some(slot) => {
                *slot = b;
                self.byte += 1;
            }
            None => self.overflow = true,
        }
    }
}

impl BitSink for BitWriter<'_> {
    #[inline]
    fn put(&mut self, n: u32, value: u32) {
        debug_assert!(n <= 32);
        debug_assert!(
            n == 32 || value >> n == 0,
            "value {value:#x} wider than {n} bits"
        );
        if n == 0 {
            return;
        }
        let mask = if n == 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << n) - 1
        };
        self.acc = (self.acc << n) | (u64::from(value) & mask);
        self.nacc += n;
        while self.nacc >= 8 {
            self.nacc -= 8;
            let b = (self.acc >> self.nacc) as u8;
            self.emit(b);
        }
        self.acc &= (1u64 << self.nacc) - 1;
    }

    #[inline]
    fn position(&self) -> usize {
        self.byte * 8 + self.nacc as usize
    }
}

/// Reads bits, most significant first. Reading past the end yields an error, not a panic.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    buf: &'a [u8],
    bit: usize,
}

/// A read past the end of the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("read of {n} bits at bit {at} runs past the end ({len} bits)")]
pub struct OutOfData {
    /// Bits requested.
    pub n: usize,
    /// Bit position of the read.
    pub at: usize,
    /// Bits available in total.
    pub len: usize,
}

impl<'a> BitReader<'a> {
    /// A reader at the start of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, bit: 0 }
    }

    /// Reads `n` (at most 32) bits as an unsigned number.
    pub fn get(&mut self, n: u32) -> Result<u32, OutOfData> {
        let len = self.buf.len() * 8;
        if self.bit + n as usize > len {
            return Err(OutOfData {
                n: n as usize,
                at: self.bit,
                len,
            });
        }
        let mut v: u64 = 0;
        for _ in 0..n {
            let b = (self.buf[self.bit >> 3] >> (7 - (self.bit & 7))) & 1;
            v = (v << 1) | u64::from(b);
            self.bit += 1;
        }
        Ok(v as u32)
    }

    /// Reads one bit as a flag.
    pub fn flag(&mut self) -> Result<bool, OutOfData> {
        Ok(self.get(1)? == 1)
    }

    /// Skips `n` bits.
    pub fn skip(&mut self, n: usize) -> Result<(), OutOfData> {
        let len = self.buf.len() * 8;
        if self.bit + n > len {
            return Err(OutOfData {
                n,
                at: self.bit,
                len,
            });
        }
        self.bit += n;
        Ok(())
    }

    /// Current bit position.
    pub fn position(&self) -> usize {
        self.bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_and_reader_round_trip() {
        let fields: &[(u32, u32)] = &[
            (16, 0x0B77),
            (1, 1),
            (5, 0b10110),
            (3, 0),
            (7, 124),
            (32, 0xDEAD_BEEF),
            (2, 3),
            (0, 0),
            (13, 0x1abc),
        ];
        let mut buf = [0xffu8; 16];
        let mut w = BitWriter::new(&mut buf);
        let mut c = BitCounter::new();
        for &(n, v) in fields {
            w.put(n, v);
            c.put(n, v);
        }
        assert_eq!(w.position(), c.position());
        assert!(!w.overflowed());
        w.zero_fill_to(128);
        assert!(!w.overflowed());
        w.finish();
        assert_eq!(&buf[..2], &[0x0B, 0x77]);
        let mut r = BitReader::new(&buf);
        for &(n, v) in fields {
            assert_eq!(r.get(n).unwrap(), v);
        }
        let used = c.position();
        let rest = 128 - used;
        r.skip(0).unwrap();
        assert_eq!(r.get(rest.min(32) as u32).unwrap(), 0);
    }

    #[test]
    fn writer_reports_overflow_instead_of_panicking() {
        let mut buf = [0u8; 1];
        let mut w = BitWriter::new(&mut buf);
        w.put(6, 0x3f);
        w.put(10, 0x3ff);
        assert!(w.overflowed());
        w.finish();
        assert_eq!(buf[0], 0xff);
    }

    #[test]
    fn reader_reports_end_of_data() {
        let buf = [0xA5u8];
        let mut r = BitReader::new(&buf);
        assert_eq!(r.get(4).unwrap(), 0xA);
        assert!(r.get(5).is_err());
        assert_eq!(r.get(4).unwrap(), 0x5);
    }
}
