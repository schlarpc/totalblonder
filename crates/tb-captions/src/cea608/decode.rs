//! A small CEA-608 caption decoder for one channel: displayed and non-displayed memory, pop-on,
//! roll-up and paint-on styles, PACs, mid-row codes, tab offsets, special and extended
//! characters, backspace, the erase commands. It is written from CTA-608-E's receiver rules
//! (C.7 PACs, C.10 style switching, C.12 acting on the first of a redundant pair, C.13 the
//! right margin; §6.4.2 the extended characters' backspace) to read our own output back in
//! tests and to monitor a channel's captions; it is not a renderer (no colors, flashing,
//! smooth scrolling or Text mode).

use super::{Channel, Code, Field, MidRowAttr, Misc, OddByte, PacAttr, Pair};
use crate::cue::SpanStyle;

/// A cell of caption memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The character (a mid-row code shows as a space).
    pub ch: char,
    /// Its style.
    pub style: SpanStyle,
}

type Memory = [[Option<Cell>; 32]; 15];

/// The caption style in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Pop-on: text goes to non-displayed memory until EOC.
    PopOn,
    /// Roll-up with this many rows.
    RollUp(u8),
    /// Paint-on: text goes to displayed memory.
    PaintOn,
    /// Text mode (TR, RTD): ignored.
    Text,
}

/// One displayed row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenRow {
    /// Row 1 to 15.
    pub row: u8,
    /// Column of the first cell that holds something.
    pub col: u8,
    /// The cells from there to the last one that holds something (empty cells inside are
    /// spaces).
    pub cells: Vec<Cell>,
}

impl ScreenRow {
    /// The characters, trailing spaces removed.
    pub fn text(&self) -> String {
        let s: String = self.cells.iter().map(|c| c.ch).collect();
        s.trim_end().to_owned()
    }
}

/// A CEA-608 decoder of one channel.
#[derive(Clone, Debug)]
pub struct Decoder {
    ch: Channel,
    mode: Mode,
    mem: [Memory; 2],
    /// Index of the displayed memory in `mem`.
    shown: usize,
    row: u8,
    col: u8,
    base: u8,
    style: SpanStyle,
    last_code: Option<Pair>,
    ours: bool,
    /// Bytes with even parity seen (their pairs are dropped).
    pub parity_errors: u64,
    /// Control codes this decoder does not know (XDS, reserved).
    pub unknown: u64,
}

impl Decoder {
    /// A decoder of `ch` with empty memories.
    pub fn new(ch: Channel) -> Self {
        Self {
            ch,
            mode: Mode::PopOn,
            mem: [[[None; 32]; 15]; 2],
            shown: 0,
            row: 15,
            col: 0,
            base: 15,
            style: SpanStyle::default(),
            last_code: None,
            ours: !ch.second_data_channel(),
            parity_errors: 0,
            unknown: 0,
        }
    }

    /// The style in effect.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Feeds the pair of this decoder's field in one frame (bytes as received). Returns
    /// whether the displayed memory changed.
    pub fn feed(&mut self, raw: [u8; 2]) -> bool {
        let Ok(pair) = Pair::check(raw) else {
            self.parity_errors += 1;
            self.last_code = None;
            return false;
        };
        let before = self.screen();
        self.step(pair);
        self.screen() != before
    }

    fn step(&mut self, pair: Pair) {
        if pair.is_null() {
            self.last_code = None;
            return;
        }
        let [a, b] = pair.values();
        if a < 0x10 {
            // XDS on field 2, or bytes that never belong on field 1 (D.2)
            self.last_code = None;
            if a != 0 {
                self.unknown += 1;
            }
            return;
        }
        if pair.is_control() {
            // the copy of the code before: ignored, and the next equal code acts (C.12)
            if self.last_code == Some(pair) {
                self.last_code = None;
                return;
            }
            self.last_code = Some(pair);
            let Some((dc2, code)) = Code::parse([a, b]) else {
                self.unknown += 1;
                return;
            };
            if let Code::Misc(_) = code {
                // field 2's miscellaneous codes are 0x15 / 0x1D (B.11.6)
                let field2_form = a & 0x01 == 1;
                if field2_form != (self.ch.field() == Field::Two) {
                    self.unknown += 1;
                }
            }
            self.ours = dc2 == self.ch.second_data_channel();
            if self.ours {
                self.code(code);
            }
            return;
        }
        self.last_code = None;
        if !self.ours || self.mode == Mode::Text {
            return;
        }
        for v in [a, b] {
            if v >= 0x20
                && let Some(c) = super::Char608::from_code([v, 0])
            {
                self.put(c.to_char());
            }
        }
    }

    fn target(&mut self) -> &mut Memory {
        let i = match self.mode {
            Mode::PopOn => 1 - self.shown,
            _ => self.shown,
        };
        &mut self.mem[i]
    }

    fn put(&mut self, ch: char) {
        let (r, c, style) = (usize::from(self.row - 1), usize::from(self.col), self.style);
        self.target()[r][c] = Some(Cell { ch, style });
        if self.col < 31 {
            self.col += 1;
        }
    }

    fn clear(m: &mut Memory) {
        *m = [[None; 32]; 15];
    }

    fn code(&mut self, code: Code) {
        match code {
            Code::Misc(m) => self.misc(m),
            Code::Pac {
                row,
                attr,
                underline,
            } => {
                let mut r = row.get();
                if let Mode::RollUp(n) = self.mode {
                    // a PAC moves the roll-up window to a new base row (C.7); a base row
                    // above the depth gives way to the depth (C.4)
                    r = r.max(n);
                    if r != self.base {
                        let shown = self.shown;
                        let old = self.mem[shown];
                        Self::clear(&mut self.mem[shown]);
                        for k in 0..n {
                            let (from, to) = (
                                i32::from(self.base) - i32::from(k),
                                i32::from(r) - i32::from(k),
                            );
                            if from >= 1 && to >= 1 {
                                self.mem[shown][(to - 1) as usize] = old[(from - 1) as usize];
                            }
                        }
                        self.base = r;
                    }
                }
                self.row = r;
                self.col = match attr {
                    PacAttr::Indent(n) => 4 * n,
                    _ => 0,
                };
                self.style = SpanStyle {
                    italic: attr == PacAttr::Italics,
                    underline,
                };
            }
            Code::MidRow { attr, underline } => {
                self.style = SpanStyle {
                    italic: attr == MidRowAttr::Italics,
                    underline,
                };
                self.put(' ');
            }
            Code::Tab(n) => self.col = (self.col + n).min(31),
            Code::Char(c) => {
                if self.mode == Mode::Text {
                    return;
                }
                if c.is_extended() && self.col > 0 {
                    // the automatic backspace (§6.4.2)
                    self.col -= 1;
                    let (r, k) = (usize::from(self.row - 1), usize::from(self.col));
                    self.target()[r][k] = None;
                }
                self.put(c.to_char());
            }
        }
    }

    fn misc(&mut self, m: Misc) {
        match m {
            Misc::Rcl => self.mode = Mode::PopOn,
            Misc::Rdc => self.mode = Mode::PaintOn,
            Misc::Tr | Misc::Rtd => self.mode = Mode::Text,
            Misc::Ru2 | Misc::Ru3 | Misc::Ru4 => {
                let n = m as u8 - Misc::Ru2 as u8 + 2;
                if !matches!(self.mode, Mode::RollUp(_)) {
                    // C.10: pop-on or paint-on captions in either memory are erased
                    Self::clear(&mut self.mem[0]);
                    Self::clear(&mut self.mem[1]);
                    self.base = 15;
                }
                self.mode = Mode::RollUp(n);
                self.base = self.base.max(n);
                self.row = self.base;
                self.col = 0;
            }
            Misc::Edm => {
                let s = self.shown;
                Self::clear(&mut self.mem[s]);
            }
            Misc::Enm => {
                let s = 1 - self.shown;
                Self::clear(&mut self.mem[s]);
            }
            Misc::Eoc => {
                self.shown = 1 - self.shown;
                self.mode = Mode::PopOn;
            }
            Misc::Cr => {
                if let Mode::RollUp(n) = self.mode {
                    let s = self.shown;
                    let top = usize::from(self.base.saturating_sub(n));
                    let base = usize::from(self.base - 1);
                    for r in top..base {
                        self.mem[s][r] = self.mem[s][r + 1];
                    }
                    self.mem[s][base] = [None; 32];
                    // rows above the window are not part of a roll-up caption
                    if top > 0 {
                        self.mem[s][top - 1] = [None; 32];
                    }
                    self.row = self.base;
                    self.col = 0;
                    self.style = SpanStyle::default();
                }
            }
            Misc::Bs => {
                if self.col > 0 {
                    self.col -= 1;
                    let (r, c) = (usize::from(self.row - 1), usize::from(self.col));
                    self.target()[r][c] = None;
                }
            }
            Misc::Der => {
                let (r, c) = (usize::from(self.row - 1), usize::from(self.col));
                for k in c..32 {
                    self.target()[r][k] = None;
                }
            }
            Misc::Fon => {}
        }
    }

    /// The displayed rows, top to bottom (rows with nothing in them are left out).
    pub fn screen(&self) -> Vec<ScreenRow> {
        let m = &self.mem[self.shown];
        let mut out = Vec::new();
        for (r, cells) in m.iter().enumerate() {
            let Some(first) = cells.iter().position(Option::is_some) else {
                continue;
            };
            let last = cells.iter().rposition(Option::is_some).unwrap_or(first);
            out.push(ScreenRow {
                row: r as u8 + 1,
                col: first as u8,
                cells: cells[first..=last]
                    .iter()
                    .map(|c| {
                        c.unwrap_or(Cell {
                            ch: ' ',
                            style: SpanStyle::default(),
                        })
                    })
                    .collect(),
            });
        }
        out
    }

    /// The displayed text: the rows' texts joined with `\n`.
    pub fn text(&self) -> String {
        self.screen()
            .iter()
            .map(ScreenRow::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Whether a raw byte has odd parity (for counters on received data).
pub fn has_odd_parity(b: u8) -> bool {
    OddByte::check(b).is_ok()
}

#[cfg(test)]
mod tests {
    use super::super::{Char608, Redundancy, Row, Units, push_row};
    use super::*;

    fn feed_units(d: &mut Decoder, units: &[super::super::Unit]) {
        for u in units {
            for _ in 0..u.slots() {
                d.feed(u.pair().bytes());
            }
        }
    }

    #[test]
    fn pop_on_round_trip_with_every_character() {
        // every character of the repertoire, in rows of at most 31, with the styles
        let all: Vec<Char608> = Char608::all().collect();
        for (k, chunk) in all.chunks(28).enumerate() {
            let ch = [Channel::Cc1, Channel::Cc2, Channel::Cc3, Channel::Cc4][k % 4];
            let style = SpanStyle {
                italic: k % 2 == 1,
                underline: k % 3 == 1,
            };
            let cells: Vec<_> = chunk.iter().map(|&c| (c, style)).collect();
            let mut u = Units::new(ch, ch.default_redundancy());
            u.code(Code::Misc(Misc::Rcl));
            u.code(Code::Misc(Misc::Enm));
            push_row(&mut u, Row::new(14).unwrap(), 2, &cells);
            let load = u.finish();
            let mut d = Decoder::new(ch);
            feed_units(&mut d, &load);
            assert_eq!(d.text(), "", "nothing shown before EOC");
            let mut u = Units::new(ch, ch.default_redundancy());
            u.code(Code::Misc(Misc::Eoc));
            feed_units(&mut d, &u.finish());
            let s = d.screen();
            assert_eq!(s.len(), 1);
            let want: String = chunk.iter().map(|c| c.to_char()).collect();
            let start = if style.italic { 1 } else { 0 };
            let got: String = s[0].cells[start..].iter().map(|c| c.ch).collect();
            assert_eq!(got, want, "chunk {k} on {ch:?}");
            assert_eq!(s[0].row, 14);
            assert_eq!(s[0].col + start as u8, 2);
            for c in &s[0].cells[start..] {
                assert_eq!(c.style, style);
            }
            assert_eq!(d.parity_errors, 0);
        }
    }

    #[test]
    fn redundant_codes_act_once() {
        let mut d = Decoder::new(Channel::Cc1);
        let p = |c: Code| c.pair(Channel::Cc1).bytes();
        let cr = p(Code::Misc(Misc::Cr));
        d.feed(p(Code::Misc(Misc::Ru2)));
        d.feed(p(Code::Misc(Misc::Ru2)));
        d.feed(Pair::new(b'a', b'b').bytes());
        // three CRs to get two (§8.3, B.14)
        d.feed(cr);
        d.feed(cr);
        d.feed(cr);
        d.feed(Pair::new(b'c', b'd').bytes());
        assert_eq!(
            d.text(),
            "cd",
            "two CRs rolled 'ab' out of the two-row window"
        );
        // a null pair between equal codes makes both act
        let note = p(Code::Char(Char608::from_char('♪').unwrap()));
        d.feed(note);
        d.feed(note);
        d.feed(Pair::NULL.bytes());
        d.feed(note);
        d.feed(note);
        assert_eq!(d.text(), "cd♪♪");
        // parity errors are counted and dropped
        d.feed([0x00, 0x80]);
        assert_eq!(d.parity_errors, 1);
    }

    #[test]
    fn data_channels_and_fields_are_kept_apart() {
        let mut u = Units::new(Channel::Cc2, Redundancy::Doubled);
        u.code(Code::Misc(Misc::Rcl));
        push_row(
            &mut u,
            Row::BOTTOM,
            0,
            &[(Char608::from_char('x').unwrap(), SpanStyle::default())],
        );
        u.code(Code::Misc(Misc::Eoc));
        let units = u.finish();
        let mut d1 = Decoder::new(Channel::Cc1);
        let mut d2 = Decoder::new(Channel::Cc2);
        feed_units(&mut d1, &units);
        feed_units(&mut d2, &units);
        assert_eq!(d1.text(), "");
        assert_eq!(d2.text(), "x");
    }
}
