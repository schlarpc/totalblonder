//! Decoding tables built from the code lists of [`crate::tables`].

use crate::tables::{self, Code, DctCode, MbTypeCode};

/// A lookup table over the next `bits` bits: (value index + 1, code length), zero for no code.
#[derive(Debug, Clone)]
pub(crate) struct Lut {
    bits: u32,
    entries: Vec<(u16, u8)>,
}

impl Lut {
    fn build(codes: impl Iterator<Item = Code>) -> Self {
        let codes: Vec<Code> = codes.collect();
        let bits = codes.iter().map(|c| u32::from(c.len)).max().unwrap_or(1);
        let mut entries = vec![(0u16, 0u8); 1usize << bits];
        for (i, c) in codes.iter().enumerate() {
            let free = bits - u32::from(c.len);
            let base = (c.bits << free) as usize;
            for e in entries.iter_mut().skip(base).take(1usize << free) {
                *e = (i as u16 + 1, c.len);
            }
        }
        Self { bits, entries }
    }

    /// Number of bits to peek.
    #[inline]
    pub(crate) fn bits(&self) -> u32 {
        self.bits
    }

    /// The index of the code that `peek` (the next [`Lut::bits`] bits) starts with, and its
    /// length.
    #[inline]
    pub(crate) fn get(&self, peek: u32) -> Option<(usize, u32)> {
        let (i, len) = self.entries.get(peek as usize).copied().unwrap_or((0, 0));
        if i == 0 {
            None
        } else {
            Some((usize::from(i) - 1, u32::from(len)))
        }
    }
}

/// What a DCT coefficient code means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DctSym {
    Invalid,
    Eob,
    Escape,
    /// (run, level); the sign bit follows the code.
    Coef(u8, u16),
}

/// A DCT coefficient table indexed by the next 16 bits.
#[derive(Debug, Clone)]
pub(crate) struct DctLut {
    entries: Vec<(DctSym, u8)>,
    /// has_code[run][level] for level 1..=40: the (run, level) pairs that have a code.
    has_code: Vec<bool>,
}

impl DctLut {
    fn build(rows: &[DctCode], eob: Code, escape: Code) -> Self {
        let mut entries = vec![(DctSym::Invalid, 0u8); 1 << 16];
        let mut has_code = vec![false; 64 * 64];
        let mut put = |c: Code, sym: DctSym| {
            let free = 16 - u32::from(c.len);
            let base = (c.bits << free) as usize;
            for e in entries.iter_mut().skip(base).take(1usize << free) {
                *e = (sym, c.len);
            }
        };
        for r in rows {
            put(r.code, DctSym::Coef(r.run, r.level));
            if let Some(h) = has_code.get_mut(usize::from(r.run) * 64 + usize::from(r.level)) {
                *h = true;
            }
        }
        put(eob, DctSym::Eob);
        put(escape, DctSym::Escape);
        Self { entries, has_code }
    }

    #[inline]
    pub(crate) fn get(&self, peek16: u32) -> (DctSym, u32) {
        let (s, l) = self
            .entries
            .get(peek16 as usize)
            .copied()
            .unwrap_or((DctSym::Invalid, 0));
        (s, u32::from(l))
    }

    /// True if (run, |level|) has a variable length code in this table.
    #[inline]
    pub(crate) fn has_code(&self, run: u32, level: u32) -> bool {
        level < 64
            && self
                .has_code
                .get((run * 64 + level) as usize)
                .copied()
                .unwrap_or(false)
    }
}

/// All decoding tables.
#[derive(Debug, Clone)]
pub(crate) struct Tables {
    pub(crate) mb_addr_inc: Lut,
    pub(crate) mb_type: [Lut; 3],
    pub(crate) cbp: Lut,
    pub(crate) motion_code: Lut,
    pub(crate) dc_size_luma: Lut,
    pub(crate) dc_size_chroma: Lut,
    pub(crate) dct: [DctLut; 2],
    /// inverse scans: raster index (v * 8 + u) of scan position n, for both scans.
    pub(crate) scan_pos: [[u8; 64]; 2],
}

impl Tables {
    pub(crate) fn new() -> Self {
        let mut scan_pos = [[0u8; 64]; 2];
        for (which, scan) in [tables::SCAN_ZIGZAG, tables::SCAN_ALTERNATE]
            .iter()
            .enumerate()
        {
            for (v, row) in scan.iter().enumerate() {
                for (u, &n) in row.iter().enumerate() {
                    if let Some(p) = scan_pos
                        .get_mut(which)
                        .and_then(|s| s.get_mut(usize::from(n)))
                    {
                        *p = (v * 8 + u) as u8;
                    }
                }
            }
        }
        let ty = |t: &[MbTypeCode]| Lut::build(t.iter().map(|m| m.code));
        Self {
            mb_addr_inc: Lut::build(tables::MB_ADDR_INC.iter().map(|c| c.0)),
            mb_type: [
                ty(tables::MB_TYPE_I),
                ty(tables::MB_TYPE_P),
                ty(tables::MB_TYPE_B),
            ],
            cbp: Lut::build(tables::CBP.iter().map(|c| c.0)),
            motion_code: Lut::build(tables::MOTION_CODE.iter().map(|c| c.0)),
            dc_size_luma: Lut::build(tables::DC_SIZE_LUMA.iter().map(|c| c.0)),
            dc_size_chroma: Lut::build(tables::DC_SIZE_CHROMA.iter().map(|c| c.0)),
            dct: [
                DctLut::build(tables::DCT_ZERO, tables::DCT_ZERO_EOB, tables::DCT_ESCAPE),
                DctLut::build(tables::DCT_ONE, tables::DCT_ONE_EOB, tables::DCT_ESCAPE),
            ],
            scan_pos,
        }
    }
}
