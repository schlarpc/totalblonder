//! A bit reader over one start-code unit.

/// Reads bits, most significant first. Reading past the end yields zero bits and sets
/// [`BitReader::overrun`].
#[derive(Debug, Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Position in bits from the start of the unit.
    #[inline]
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Total number of bits.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.data.len() * 8
    }

    /// True if a read went past the end.
    #[inline]
    pub(crate) fn overrun(&self) -> bool {
        self.pos > self.len()
    }

    /// The next `n` bits (n <= 32) without consuming them.
    #[inline]
    pub(crate) fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 32);
        if n == 0 {
            return 0;
        }
        let byte = self.pos >> 3;
        let shift = (self.pos & 7) as u32;
        let word = match self.data.get(byte..byte + 8) {
            Some(b) => {
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                u64::from_be_bytes(a)
            }
            None => {
                let mut a = [0u8; 8];
                if let Some(rest) = self.data.get(byte..) {
                    for (d, s) in a.iter_mut().zip(rest) {
                        *d = *s;
                    }
                }
                u64::from_be_bytes(a)
            }
        };
        ((word << shift) >> (64 - n)) as u32
    }

    /// Consumes `n` bits.
    #[inline]
    pub(crate) fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    /// Reads `n` bits (n <= 32).
    #[inline]
    pub(crate) fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }

    /// Reads one bit.
    #[inline]
    pub(crate) fn bit(&mut self) -> bool {
        self.read(1) != 0
    }

    /// True if every remaining bit is zero.
    pub(crate) fn rest_is_zero(&self) -> bool {
        if self.pos >= self.len() {
            return true;
        }
        let byte = self.pos >> 3;
        let shift = self.pos & 7;
        let first = self.data.get(byte).copied().unwrap_or(0) & (0xFFu8 >> shift);
        first == 0
            && self
                .data
                .get(byte + 1..)
                .is_none_or(|r| r.iter().all(|&b| b == 0))
    }
}
