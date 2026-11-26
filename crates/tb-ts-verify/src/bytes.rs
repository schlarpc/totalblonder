//! A bounds-checked big-endian reader over a byte slice.

/// Reads big-endian fields; every read returns `None` past the end.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rd<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn left(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_be_bytes([s[0], s[1]]))
    }

    pub(crate) fn u24(&mut self) -> Option<u32> {
        let s = self.take(3)?;
        Some(u32::from_be_bytes([0, s[0], s[1], s[2]]))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let s = &self.data[self.pos..];
        self.pos = self.data.len();
        s
    }
}

/// Splits a descriptor loop into (tag, body) pairs; `Err` with the offset where a descriptor
/// overruns the loop.
pub(crate) fn descriptors(data: &[u8]) -> Result<Vec<(u8, &[u8])>, usize> {
    let mut v = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if i + 2 > data.len() {
            return Err(i);
        }
        let tag = data[i];
        let len = usize::from(data[i + 1]);
        let Some(body) = data.get(i + 2..i + 2 + len) else {
            return Err(i);
        };
        v.push((tag, body));
        i += 2 + len;
    }
    Ok(v)
}

/// A 3-byte ISO 639 language code as text (non-printable bytes as '?').
pub(crate) fn lang(code: u32) -> String {
    code.to_be_bytes()[1..]
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

/// FNV-1a over a slice, for content comparisons.
pub(crate) fn fnv(data: &[u8], mut h: u64) -> u64 {
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// The FNV-1a offset basis.
pub(crate) const FNV0: u64 = 0xCBF2_9CE4_8422_2325;
