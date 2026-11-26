//! Bit writer for headers (the C++ `HBits`): most significant bit first, appended
//! to a byte vector.

/// Writes bits to the end of a byte vector.
#[derive(Debug)]
pub struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl<'a> BitWriter<'a> {
    /// A writer that appends to `out`.
    pub fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out, acc: 0, n: 0 }
    }

    /// Append the low `len` bits of `val` (`len` in 1..=32).
    pub fn put(&mut self, val: u32, len: u32) {
        debug_assert!((1..=32).contains(&len));
        let mask = if len >= 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << len) - 1
        };
        self.acc = (self.acc << len) | (u64::from(val) & mask);
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    /// Pad with zero bits to the next byte boundary.
    pub fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }

    /// Byte-align and write a start code: `00 00 01 code` (6.2.1).
    pub fn start(&mut self, code: u8) {
        self.align();
        self.put(1, 24);
        self.put(u32::from(code), 8);
    }
}

impl Drop for BitWriter<'_> {
    /// Bits that do not fill a byte are flushed with zero padding, so a writer cannot lose bits.
    fn drop(&mut self) {
        self.align();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_codes_and_fields() {
        let mut v = Vec::new();
        {
            let mut b = BitWriter::new(&mut v);
            b.start(0xB3);
            b.put(720, 12);
            b.put(480, 12);
            b.put(1, 1);
            b.start(0x00);
            b.put(0xffff_ffff, 32);
        }
        assert_eq!(
            v,
            [
                0, 0, 1, 0xB3, 0x2d, 0x01, 0xe0, 0x80, 0, 0, 1, 0, 0xff, 0xff, 0xff, 0xff
            ]
        );
    }
}
