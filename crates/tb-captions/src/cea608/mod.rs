//! CEA-608 (CTA-608-E S-2019) as carried in `cc_data()`: byte pairs with odd parity, one pair
//! per field per frame, the caption commands, and the encoding of caption rows into the
//! [`Unit`]s the scheduler places in time.
//!
//! Page numbers are the printed ones of CTA-608-E (PDF page = printed + 12).
//!
//! **Bytes.** Seven data bits and an odd parity bit in b7 (§5.3; D.2 "All data shall be forced
//! to odd parity"): [`OddByte`] can only hold such a byte. The null pair is 0x80 0x80
//! ([`Pair::NULL`]).
//!
//! **Channels.** CC1 and CC2 are data channels 1 and 2 of field 1, CC3 and CC4 of field 2
//! (§4.1 Table 1, p.10). The data channel is in the first byte of every code: 0x10 to 0x17 for
//! channel 1, 0x18 to 0x1F for channel 2 (Tables 49 to 53). On field 2 the miscellaneous
//! control codes use 0x15 / 0x1D in place of 0x14 / 0x1C (B.11.6, p.69).
//!
//! **Redundancy.** "Each two-byte control code whose first character is in the range of 0x10
//! to 0x1F should be transmitted twice in successive frames" (D.2, p.78, Normative/
//! Regulatory); receivers act on the first and ignore the copy (C.12, p.74-75). This covers
//! the commands, PACs, mid-row codes, tab offsets and the special and extended characters. On
//! field 2 "the unnecessary doubling of control byte pairs should be disabled as a default"
//! (D.2; B.14, p.70): [`Redundancy`]. Two equal codes that are both meant (two music notes)
//! are kept apart by a null pair, or a receiver would take the second for the copy of the
//! first ([`Units`]).

pub mod chars;
pub mod decode;

pub use chars::Char608;

use crate::cue::SpanStyle;
use chars::Coding;

/// A byte with odd parity: seven data bits, b7 set when they have an even number of ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OddByte(u8);

/// A byte whose parity is not odd.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("byte {0:#04x} does not have odd parity (CTA-608-E §5.3)")]
pub struct ParityError(pub u8);

impl OddByte {
    /// The seven low bits of `v` with the parity bit set as needed.
    pub const fn new(v: u8) -> Self {
        let v = v & 0x7F;
        if v.count_ones().is_multiple_of(2) {
            Self(v | 0x80)
        } else {
            Self(v)
        }
    }

    /// A received byte, if its parity is odd.
    pub const fn check(raw: u8) -> Result<Self, ParityError> {
        if raw.count_ones() % 2 == 1 {
            Ok(Self(raw))
        } else {
            Err(ParityError(raw))
        }
    }

    /// The byte as sent.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The seven data bits.
    pub const fn value(self) -> u8 {
        self.0 & 0x7F
    }
}

/// Two bytes sent in one field of one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pair(pub [OddByte; 2]);

impl Pair {
    /// The null pair: filler a receiver ignores (CTA-708-E §4.3.2.2, p.13).
    pub const NULL: Self = Self([OddByte::new(0), OddByte::new(0)]);

    /// The pair of these seven-bit values.
    pub const fn new(a: u8, b: u8) -> Self {
        Self([OddByte::new(a), OddByte::new(b)])
    }

    /// A received pair, if both bytes have odd parity.
    pub fn check(raw: [u8; 2]) -> Result<Self, ParityError> {
        Ok(Self([OddByte::check(raw[0])?, OddByte::check(raw[1])?]))
    }

    /// The bytes as sent.
    pub const fn bytes(self) -> [u8; 2] {
        [self.0[0].get(), self.0[1].get()]
    }

    /// The seven-bit values.
    pub const fn values(self) -> [u8; 2] {
        [self.0[0].value(), self.0[1].value()]
    }

    /// The null pair (both values zero).
    pub const fn is_null(self) -> bool {
        self.0[0].value() == 0 && self.0[1].value() == 0
    }

    /// A two-byte control code (first value 0x10 to 0x1F).
    pub const fn is_control(self) -> bool {
        let a = self.0[0].value();
        a >= 0x10 && a <= 0x1F
    }
}

/// Field 1 or field 2 of a frame (`cc_type` 00 and 01).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// Field 1: CC1, CC2 (`cc_type` 00).
    One,
    /// Field 2: CC3, CC4, XDS (`cc_type` 01).
    Two,
}

impl Field {
    /// 0 or 1: `cc_type`, and the index used for per-field arrays.
    pub const fn index(self) -> usize {
        match self {
            Self::One => 0,
            Self::Two => 1,
        }
    }
}

/// A caption channel (Table 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Field 1, data channel 1: the primary synchronous captions.
    Cc1,
    /// Field 1, data channel 2.
    Cc2,
    /// Field 2, data channel 1: usually a second language.
    Cc3,
    /// Field 2, data channel 2.
    Cc4,
}

impl Channel {
    /// Its field.
    pub const fn field(self) -> Field {
        match self {
            Self::Cc1 | Self::Cc2 => Field::One,
            Self::Cc3 | Self::Cc4 => Field::Two,
        }
    }

    /// Data channel 2 (0x08 in the first byte of every code).
    pub const fn second_data_channel(self) -> bool {
        matches!(self, Self::Cc2 | Self::Cc4)
    }

    /// Whether control codes are sent twice by default: on field 1 (D.2), not on field 2
    /// (D.2, B.14).
    pub const fn default_redundancy(self) -> Redundancy {
        match self.field() {
            Field::One => Redundancy::Doubled,
            Field::Two => Redundancy::Single,
        }
    }
}

/// How control codes are sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Redundancy {
    /// Twice, in consecutive frames (D.2).
    Doubled,
    /// Once (field 2 default, B.14).
    Single,
}

/// A caption row, 1 (top) to 15 (bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Row(u8);

impl Row {
    /// Row `n`, 1 to 15.
    pub const fn new(n: u8) -> Option<Self> {
        if n >= 1 && n <= 15 {
            Some(Self(n))
        } else {
            None
        }
    }
    /// The bottom row.
    pub const BOTTOM: Self = Self(15);
    /// The top row.
    pub const TOP: Self = Self(1);
    /// Its number.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A column, 0 (left) to 31: the 32 columns of a row (C.13, p.75).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Column(u8);

impl Column {
    /// Columns in a row.
    pub const COUNT: usize = 32;
    /// Column `n`, 0 to 31.
    pub const fn new(n: u8) -> Option<Self> {
        if n < 32 { Some(Self(n)) } else { None }
    }
    /// Its index.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// The colors of PACs and mid-row codes (Tables 51 and 53), in code order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// White.
    White,
    /// Green.
    Green,
    /// Blue.
    Blue,
    /// Cyan.
    Cyan,
    /// Red.
    Red,
    /// Yellow.
    Yellow,
    /// Magenta.
    Magenta,
}

impl Color {
    const ALL: [Self; 7] = [
        Self::White,
        Self::Green,
        Self::Blue,
        Self::Cyan,
        Self::Red,
        Self::Yellow,
        Self::Magenta,
    ];
}

/// What a PAC sets besides the row (Table 53).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PacAttr {
    /// A color, at column 0.
    Color(Color),
    /// White italics, at column 0.
    Italics,
    /// Column `4 x n` (n = 0 to 7), white, not italic.
    Indent(u8),
}

/// What a mid-row code sets (Table 51). A mid-row code takes one column and shows as a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MidRowAttr {
    /// A color (italics off).
    Color(Color),
    /// Italics (the color stays).
    Italics,
}

/// The miscellaneous control codes (Table 52), second byte 0x20 to 0x2F.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Misc {
    /// Resume caption loading: pop-on style, text goes to non-displayed memory.
    Rcl = 0x20,
    /// Backspace.
    Bs = 0x21,
    /// Delete to end of row.
    Der = 0x24,
    /// Roll-up, 2 rows.
    Ru2 = 0x25,
    /// Roll-up, 3 rows.
    Ru3 = 0x26,
    /// Roll-up, 4 rows.
    Ru4 = 0x27,
    /// Flash on.
    Fon = 0x28,
    /// Resume direct captioning (paint-on).
    Rdc = 0x29,
    /// Text restart.
    Tr = 0x2A,
    /// Resume text display.
    Rtd = 0x2B,
    /// Erase displayed memory.
    Edm = 0x2C,
    /// Carriage return.
    Cr = 0x2D,
    /// Erase non-displayed memory.
    Enm = 0x2E,
    /// End of caption: swap the memories.
    Eoc = 0x2F,
}

impl Misc {
    fn from_code(b: u8) -> Option<Self> {
        Some(match b {
            0x20 => Self::Rcl,
            0x21 => Self::Bs,
            0x24 => Self::Der,
            0x25 => Self::Ru2,
            0x26 => Self::Ru3,
            0x27 => Self::Ru4,
            0x28 => Self::Fon,
            0x29 => Self::Rdc,
            0x2A => Self::Tr,
            0x2B => Self::Rtd,
            0x2C => Self::Edm,
            0x2D => Self::Cr,
            0x2E => Self::Enm,
            0x2F => Self::Eoc,
            _ => return None,
        })
    }
}

/// A two-byte code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    /// Preamble address code: row, attribute, underline (Table 53).
    Pac {
        /// Row.
        row: Row,
        /// Color, italics or indent.
        attr: PacAttr,
        /// Underline.
        underline: bool,
    },
    /// Mid-row code (Table 51).
    MidRow {
        /// Color or italics.
        attr: MidRowAttr,
        /// Underline.
        underline: bool,
    },
    /// Miscellaneous control code (Table 52).
    Misc(Misc),
    /// Tab offset by 1 to 3 columns (Table 52, TO1 to TO3).
    Tab(u8),
    /// A special or extended character.
    Char(Char608),
}

/// PAC first byte and second-byte base per row (Table 53, p.101).
const PAC_ROWS: [(u8, u8); 15] = [
    (0x11, 0x40),
    (0x11, 0x60),
    (0x12, 0x40),
    (0x12, 0x60),
    (0x15, 0x40),
    (0x15, 0x60),
    (0x16, 0x40),
    (0x16, 0x60),
    (0x17, 0x40),
    (0x17, 0x60),
    (0x10, 0x40),
    (0x13, 0x40),
    (0x13, 0x60),
    (0x14, 0x40),
    (0x14, 0x60),
];

impl Code {
    /// The pair of this code on `ch`.
    pub fn pair(self, ch: Channel) -> Pair {
        let [a, b] = self.data_channel_1(ch.field());
        let a = if ch.second_data_channel() {
            a | 0x08
        } else {
            a
        };
        Pair::new(a, b)
    }

    /// The values on data channel 1 of `field`.
    fn data_channel_1(self, field: Field) -> [u8; 2] {
        match self {
            Self::Pac {
                row,
                attr,
                underline,
            } => {
                let (a, base) = PAC_ROWS[usize::from(row.0 - 1)];
                let attr = match attr {
                    PacAttr::Color(c) => 2 * (c as u8),
                    PacAttr::Italics => 0x0E,
                    PacAttr::Indent(n) => 0x10 + 2 * (n & 7),
                };
                [a, base | attr | u8::from(underline)]
            }
            Self::MidRow { attr, underline } => {
                let v = match attr {
                    MidRowAttr::Color(c) => 2 * (c as u8),
                    MidRowAttr::Italics => 0x0E,
                };
                [0x11, 0x20 | v | u8::from(underline)]
            }
            Self::Misc(m) => {
                let a = match field {
                    Field::One => 0x14,
                    Field::Two => 0x15,
                };
                [a, m as u8]
            }
            Self::Tab(n) => [0x17, 0x20 + n.clamp(1, 3)],
            Self::Char(c) => match c.coding() {
                Coding::Code(x) => x,
                // a standard character is not a code; never made by this crate
                Coding::Byte(b) => [b, 0],
            },
        }
    }

    /// The code of a received pair (seven-bit values, either field's form): its data channel
    /// (`true` for channel 2) and the code. `None` if the pair is not a control code this
    /// crate knows (XDS, Text-mode special assignments, reserved codes).
    pub fn parse(v: [u8; 2]) -> Option<(bool, Self)> {
        let [a, b] = [v[0] & 0x7F, v[1] & 0x7F];
        if !(0x10..=0x1F).contains(&a) {
            return None;
        }
        let dc2 = a & 0x08 != 0;
        let a1 = a & !0x08;
        let code = match (a1, b) {
            (_, 0x40..=0x7F) => {
                let base = b & 0x60;
                let row = PAC_ROWS.iter().position(|&(r, s)| r == a1 && s == base)?;
                let attr = b & 0x1F;
                let attr = if attr >= 0x10 {
                    PacAttr::Indent((attr - 0x10) / 2)
                } else if attr >= 0x0E {
                    PacAttr::Italics
                } else {
                    PacAttr::Color(Color::ALL[usize::from(attr / 2)])
                };
                Self::Pac {
                    row: Row(row as u8 + 1),
                    attr,
                    underline: b & 1 != 0,
                }
            }
            (0x11, 0x20..=0x2F) => Self::MidRow {
                attr: if b >= 0x2E {
                    MidRowAttr::Italics
                } else {
                    MidRowAttr::Color(Color::ALL[usize::from((b - 0x20) / 2)])
                },
                underline: b & 1 != 0,
            },
            (0x11, 0x30..=0x3F) | (0x12 | 0x13, 0x20..=0x3F) => {
                if a1 == 0x11 && b == 0x39 {
                    Self::Char(Char608::TRANSPARENT_SPACE)
                } else {
                    Self::Char(Char608::from_code([a1, b])?)
                }
            }
            (0x14 | 0x15, 0x20..=0x2F) => Self::Misc(Misc::from_code(b)?),
            (0x17, 0x21..=0x23) => Self::Tab(b - 0x20),
            _ => return None,
        };
        Some((dc2, code))
    }
}

/// One step of a caption stream as the scheduler places it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// A pair sent once: two characters (or one and a null byte), or a null pair.
    Once(Pair),
    /// A control code sent in two consecutive frames of its field.
    Twice(Pair),
}

impl Unit {
    /// Frames (slots of one field) it takes.
    pub const fn slots(self) -> usize {
        match self {
            Self::Once(_) => 1,
            Self::Twice(_) => 2,
        }
    }
    /// Its pair.
    pub const fn pair(self) -> Pair {
        match self {
            Self::Once(p) | Self::Twice(p) => p,
        }
    }
}

/// A list of units being built for one channel: characters are packed two to a pair, codes
/// are sent once or twice as [`Redundancy`] says, and two equal codes in a row get a null pair
/// between them.
#[derive(Clone, Debug)]
pub struct Units {
    ch: Channel,
    red: Redundancy,
    /// The units so far.
    pub units: Vec<Unit>,
    pending: Option<u8>,
}

impl Units {
    /// Empty, for `ch`.
    pub fn new(ch: Channel, red: Redundancy) -> Self {
        Self {
            ch,
            red,
            units: Vec::new(),
            pending: None,
        }
    }

    /// Slots the units take.
    pub fn slots(&self) -> usize {
        self.units.iter().map(|u| u.slots()).sum::<usize>() + usize::from(self.pending.is_some())
    }

    fn flush(&mut self) {
        if let Some(b) = self.pending.take() {
            self.units.push(Unit::Once(Pair::new(b, 0)));
        }
    }

    /// A control code.
    pub fn code(&mut self, c: Code) {
        self.flush();
        let p = c.pair(self.ch);
        if self.units.last().is_some_and(|u| u.pair() == p) {
            self.units.push(Unit::Once(Pair::NULL));
        }
        self.units.push(match self.red {
            Redundancy::Doubled => Unit::Twice(p),
            Redundancy::Single => Unit::Once(p),
        });
    }

    /// A character: a standard character is a byte of a pair; a special character a code; an
    /// extended character its standard fallback, then its code.
    pub fn char(&mut self, c: Char608) {
        if let Some(f) = c.fallback() {
            self.char(f);
        }
        match c.coding() {
            Coding::Byte(b) => match self.pending.take() {
                Some(a) => self.units.push(Unit::Once(Pair::new(a, b))),
                None => self.pending = Some(b),
            },
            Coding::Code(_) => self.code(Code::Char(c)),
        }
    }

    /// The finished list.
    pub fn finish(mut self) -> Vec<Unit> {
        self.flush();
        self.units
    }
}

/// A character of a row with its style.
pub type StyledChar = (Char608, SpanStyle);

/// Cells a row of characters takes in CEA-608: one per character, plus one per mid-row code
/// [`push_row`] must insert where the style changes and no space can carry it, plus one when
/// the last character is an extended character ("an extended character cannot be implemented
/// in the 32nd position", §6.4.2 NOTE2, p.19: its automatic backspace would erase the 31st).
/// A row that starts in italics away from column 0 also uses the cell left of its column for
/// its mid-row code; that cell is inside the row's 32 columns whenever the column is not 0.
pub fn row_cells(cells: &[StyledChar]) -> usize {
    let toks = tokens(cells);
    let ext_last = matches!(toks.last(), Some(Tok::Char(c)) if c.is_extended());
    toks.len() + usize::from(ext_last)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Char(Char608),
    Mrc(SpanStyle),
}

/// The characters and mid-row codes of a row. A style change takes the cell of the space
/// before it, or of the space it starts with, or a new cell.
fn tokens(cells: &[StyledChar]) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::with_capacity(cells.len() + 2);
    let mut cur = cells.first().map(|c| c.1).unwrap_or_default();
    for (k, &(c, s)) in cells.iter().enumerate() {
        if k > 0 && s != cur {
            cur = s;
            if out.last() == Some(&Tok::Char(Char608::SPACE)) {
                out.pop();
            } else if c == Char608::SPACE {
                out.push(Tok::Mrc(s));
                continue;
            }
            out.push(Tok::Mrc(s));
        }
        out.push(Tok::Char(c));
    }
    out
}

fn mid_row(s: SpanStyle) -> Code {
    Code::MidRow {
        attr: if s.italic {
            MidRowAttr::Italics
        } else {
            MidRowAttr::Color(Color::White)
        },
        underline: s.underline,
    }
}

/// Appends a row of text to `u`: the PAC (and tab offset) that puts the cursor at `col` of
/// `row`, then the characters, with mid-row codes where the style changes.
///
/// The PAC indents to the multiple of four at or left of the column and TO1 to TO3 move the
/// cursor the rest of the way (B.4, p.62: "PAC for Indent 4 ... followed by a TO3"); an
/// indenting PAC sets white, not italic, with its underline flag (Table 53 note; C.7). A row
/// that starts in italics starts with an italics PAC at column 0, or elsewhere with a mid-row
/// italics code in the column before the text.
pub fn push_row(u: &mut Units, row: Row, col: u8, cells: &[StyledChar]) {
    let Some(&(_, first)) = cells.first() else {
        return;
    };
    let col = col.min(31);
    if first.italic && col == 0 {
        u.code(Code::Pac {
            row,
            attr: PacAttr::Italics,
            underline: first.underline,
        });
    } else {
        let at = if first.italic { col - 1 } else { col };
        u.code(Code::Pac {
            row,
            attr: PacAttr::Indent(at / 4),
            underline: !first.italic && first.underline,
        });
        if at % 4 != 0 {
            u.code(Code::Tab(at % 4));
        }
        if first.italic {
            u.code(mid_row(first));
        }
    }
    for t in tokens(cells) {
        match t {
            Tok::Char(c) => u.char(c),
            Tok::Mrc(s) => u.code(mid_row(s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parity() {
        // every value: odd parity, value kept, and check() accepts exactly these
        for v in 0..128u8 {
            let b = OddByte::new(v);
            assert_eq!(b.get().count_ones() % 2, 1);
            assert_eq!(b.value(), v);
            assert_eq!(OddByte::check(b.get()), Ok(b));
            assert_eq!(
                OddByte::check(b.get() ^ 0x80),
                Err(ParityError(b.get() ^ 0x80))
            );
        }
        assert_eq!(Pair::NULL.bytes(), [0x80, 0x80]);
        // RCL on CC1 is 0x94 0x20 with parity (the value the timeline's tests use)
        assert_eq!(
            Code::Misc(Misc::Rcl).pair(Channel::Cc1).bytes(),
            [0x94, 0x20]
        );
        assert_eq!(
            Code::Misc(Misc::Eoc).pair(Channel::Cc1).bytes(),
            [0x94, 0x2F]
        );
    }

    #[test]
    fn codes_follow_the_tables() {
        // Table 52 and B.11.6: 14 / 1C on field 1, 15 / 1D on field 2
        let eoc = Code::Misc(Misc::Eoc);
        let v = |c: Code, ch| c.pair(ch).values();
        assert_eq!(v(eoc, Channel::Cc1), [0x14, 0x2F]);
        assert_eq!(v(eoc, Channel::Cc2), [0x1C, 0x2F]);
        assert_eq!(v(eoc, Channel::Cc3), [0x15, 0x2F]);
        assert_eq!(v(eoc, Channel::Cc4), [0x1D, 0x2F]);
        // Table 53: row 1 white 11 40, row 15 indent 28 underline 14 7F, row 11 10 40
        let pac = |r, attr, underline| Code::Pac {
            row: Row::new(r).unwrap(),
            attr,
            underline,
        };
        assert_eq!(
            v(pac(1, PacAttr::Color(Color::White), false), Channel::Cc1),
            [0x11, 0x40]
        );
        assert_eq!(
            v(pac(15, PacAttr::Indent(7), true), Channel::Cc1),
            [0x14, 0x7F]
        );
        assert_eq!(
            v(pac(11, PacAttr::Italics, false), Channel::Cc2),
            [0x18, 0x4E]
        );
        assert_eq!(
            v(pac(12, PacAttr::Indent(1), false), Channel::Cc1),
            [0x13, 0x52]
        );
        // B.2.2's example: [0x13][0x52] is row 12 indent 4, [0x17][0x72] row 10 indent 4
        assert_eq!(
            v(pac(10, PacAttr::Indent(1), false), Channel::Cc1),
            [0x17, 0x72]
        );
        assert_eq!(
            v(pac(13, PacAttr::Indent(2), false), Channel::Cc1),
            [0x13, 0x74]
        );
        assert_eq!(
            v(pac(11, PacAttr::Indent(2), false), Channel::Cc1),
            [0x10, 0x54]
        );
        // Table 51
        let mrc = |attr, underline| Code::MidRow { attr, underline };
        assert_eq!(
            v(mrc(MidRowAttr::Italics, true), Channel::Cc1),
            [0x11, 0x2F]
        );
        assert_eq!(
            v(mrc(MidRowAttr::Color(Color::Red), false), Channel::Cc4),
            [0x19, 0x28]
        );
        assert_eq!(v(Code::Tab(3), Channel::Cc1), [0x17, 0x23]);
        // every code parses back, on every channel
        let mut codes = vec![Code::Tab(1), Code::Tab(2), Code::Tab(3)];
        for r in 1..=15 {
            for ul in [false, true] {
                for c in Color::ALL {
                    codes.push(pac(r, PacAttr::Color(c), ul));
                    codes.push(Code::MidRow {
                        attr: MidRowAttr::Color(c),
                        underline: ul,
                    });
                }
                codes.push(pac(r, PacAttr::Italics, ul));
                for n in 0..8 {
                    codes.push(pac(r, PacAttr::Indent(n), ul));
                }
            }
        }
        for b in 0x20..=0x2F {
            if let Some(m) = Misc::from_code(b) {
                codes.push(Code::Misc(m));
            }
        }
        for c in Char608::all() {
            if c.coding() != Coding::Byte(0) && matches!(c.coding(), Coding::Code(_)) {
                codes.push(Code::Char(c));
            }
        }
        codes.push(Code::Char(Char608::TRANSPARENT_SPACE));
        for ch in [Channel::Cc1, Channel::Cc2, Channel::Cc3, Channel::Cc4] {
            for &c in &codes {
                let p = c.pair(ch);
                assert!(p.is_control());
                assert_eq!(
                    Code::parse(p.values()),
                    Some((ch.second_data_channel(), c)),
                    "{c:?} on {ch:?}"
                );
            }
        }
    }

    fn plain(s: &str) -> Vec<StyledChar> {
        s.chars()
            .map(|c| (Char608::from_char(c).unwrap(), SpanStyle::default()))
            .collect()
    }

    #[test]
    fn rows() {
        let mut u = Units::new(Channel::Cc1, Redundancy::Doubled);
        push_row(&mut u, Row::BOTTOM, 6, &plain("Hi ♪♪ü"));
        let got: Vec<Unit> = u.finish();
        let p = |a, b| Pair::new(a, b);
        assert_eq!(
            got,
            [
                Unit::Twice(p(0x14, 0x72)), // row 15, indent 4
                Unit::Twice(p(0x17, 0x22)), // TO2
                Unit::Once(p(b'H', b'i')),
                Unit::Once(p(b' ', 0)),
                Unit::Twice(p(0x11, 0x37)),
                Unit::Once(Pair::NULL), // the second note is not a copy
                Unit::Twice(p(0x11, 0x37)),
                Unit::Once(p(b'u', 0)), // fallback, then the extended code
                Unit::Twice(p(0x12, 0x25)),
            ]
        );
        // field 2: codes once; still a null between equal codes
        let mut u = Units::new(Channel::Cc3, Redundancy::Single);
        push_row(&mut u, Row::BOTTOM, 0, &plain("♪♪"));
        assert_eq!(
            u.finish(),
            [
                Unit::Once(p(0x14, 0x70)),
                Unit::Once(p(0x11, 0x37)),
                Unit::Once(Pair::NULL),
                Unit::Once(p(0x11, 0x37)),
            ]
        );
    }

    #[test]
    fn styles_take_spaces_where_they_can() {
        let it = SpanStyle {
            italic: true,
            underline: false,
        };
        let pl = SpanStyle::default();
        let mk = |parts: &[(&str, SpanStyle)]| -> Vec<StyledChar> {
            parts
                .iter()
                .flat_map(|(s, st)| s.chars().map(|c| (Char608::from_char(c).unwrap(), *st)))
                .collect()
        };
        // "Hello world now" with "world" in italics: both changes take a space
        let r = mk(&[("Hello ", pl), ("world", it), (" now", pl)]);
        assert_eq!(row_cells(&r), 15);
        assert_eq!(
            tokens(&r)
                .iter()
                .filter(|t| matches!(t, Tok::Mrc(_)))
                .count(),
            2
        );
        // no space at the change: one more cell
        let r = mk(&[("ab", pl), ("cd", it)]);
        assert_eq!(row_cells(&r), 5);
        // italic from the start: an italics PAC at column 0, a mid-row code elsewhere
        let r = mk(&[("Hi", it)]);
        assert_eq!(row_cells(&r), 2);
        assert_eq!(row_cells(&plain("aü")), 3);
        let mut u = Units::new(Channel::Cc1, Redundancy::Doubled);
        push_row(&mut u, Row::BOTTOM, 5, &r);
        let v: Vec<[u8; 2]> = u.finish().iter().map(|u| u.pair().values()).collect();
        // PAC indent 4, mid-row italics in column 4, text from column 5
        assert_eq!(v, [[0x14, 0x72], [0x11, 0x2E], *b"Hi"]);
    }
}
