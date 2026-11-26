//! The tables of H.262 the verifier decodes with, transcribed from the standard's text by
//! `tools/gen_tables.py` (never from an encoder).

/// A variable length code: `len` bits, most significant first, in the low bits of `bits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    /// The code bits, right-aligned.
    pub bits: u32,
    /// The number of bits.
    pub len: u8,
}

/// One row of Table B.14 or B.15: a (run, level) code without its sign bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DctCode {
    /// The code without the sign bit.
    pub code: Code,
    /// Number of zero coefficients before this one.
    pub run: u8,
    /// Absolute value of the coefficient.
    pub level: u16,
}

/// One row of Tables B.2 to B.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MbTypeCode {
    /// The code.
    pub code: Code,
    /// macroblock_quant.
    pub quant: bool,
    /// macroblock_motion_forward.
    pub motion_forward: bool,
    /// macroblock_motion_backward.
    pub motion_backward: bool,
    /// macroblock_pattern.
    pub pattern: bool,
    /// macroblock_intra.
    pub intra: bool,
}

#[rustfmt::skip]
#[path = "tables_gen.rs"]
mod tables_gen;

pub use tables_gen::*;
