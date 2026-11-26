//! A small CEA-708 decoder: packets from `cc_data()` constructs, the blocks of one service, its
//! commands interpreted on eight windows (text, pen location and style, visibility). It
//! reads our own output back in tests and can monitor a channel; it does not render (no
//! colors, borders, effects, fonts; justification is taken as left; Delay is not modeled,
//! which our encoder never sends).

use super::packet::{Reassembler, blocks};
use super::{Anchor, Char708, element_len};
use crate::cea608::decode::{Cell, ScreenRow};
use crate::cue::SpanStyle;

/// What a window remembers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// Shown.
    pub visible: bool,
    /// Rows.
    pub rows: u8,
    /// Columns.
    pub cols: u8,
    /// Anchor point.
    pub anchor: Anchor,
    /// Vertical anchor position.
    pub av: u8,
    /// Horizontal anchor position.
    pub ah: u8,
    cells: Vec<Vec<Option<Cell>>>,
    pen_row: u8,
    pen_col: u8,
    pen: SpanStyle,
    def: [u8; 6],
}

const ANCHORS: [Anchor; 9] = [
    Anchor::TopLeft,
    Anchor::TopCenter,
    Anchor::TopRight,
    Anchor::MiddleLeft,
    Anchor::Center,
    Anchor::MiddleRight,
    Anchor::BottomLeft,
    Anchor::BottomCenter,
    Anchor::BottomRight,
];

impl Window {
    /// The rows that hold something (row index 0 is the window's top row).
    pub fn text_rows(&self) -> Vec<ScreenRow> {
        let mut out = Vec::new();
        for (r, cells) in self.cells.iter().enumerate() {
            let Some(first) = cells.iter().position(Option::is_some) else {
                continue;
            };
            let last = cells.iter().rposition(Option::is_some).unwrap_or(first);
            out.push(ScreenRow {
                row: r as u8,
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

    fn resize(&mut self) {
        self.cells.resize(usize::from(self.rows), Vec::new());
        for r in &mut self.cells {
            r.resize(usize::from(self.cols), None);
        }
    }
}

/// One service's windows.
#[derive(Clone, Debug, Default)]
pub struct ServiceDecoder {
    windows: [Option<Window>; 8],
    current: Option<usize>,
    carry: Vec<u8>,
    /// Codes this decoder does not interpret (C2, C3, P16, reserved C1 codes).
    pub unknown: u64,
    /// Syntactic elements cut by the end of a packet (§6.3 says they shall not be).
    pub cut: u64,
}

impl ServiceDecoder {
    /// No windows.
    pub fn new() -> Self {
        Self::default()
    }

    /// The window with this id, if defined.
    pub fn window(&self, id: u8) -> Option<&Window> {
        self.windows.get(usize::from(id)).and_then(Option::as_ref)
    }

    /// The data of one service block of this service.
    pub fn feed(&mut self, data: &[u8]) {
        self.carry.extend_from_slice(data);
        let buf = std::mem::take(&mut self.carry);
        let mut i = 0;
        while i < buf.len() {
            let Some(n) = element_len(buf[i], buf.get(i + 1).copied()) else {
                break;
            };
            if i + n > buf.len() {
                break;
            }
            self.element(&buf[i..i + n]);
            i += n;
        }
        self.carry = buf[i..].to_vec();
    }

    /// The end of a packet: a partial element is discarded (§4.3.3).
    pub fn end_of_packet(&mut self) {
        if !self.carry.is_empty() {
            self.cut += 1;
            self.carry.clear();
        }
    }

    fn cur(&mut self) -> Option<&mut Window> {
        let c = self.current?;
        self.windows[c].as_mut()
    }

    fn each(&mut self, map: u8, f: impl Fn(&mut Option<Window>)) {
        for (i, w) in self.windows.iter_mut().enumerate() {
            if map & (1 << i) != 0 {
                f(w);
            }
        }
    }

    fn element(&mut self, e: &[u8]) {
        let b = e[0];
        match b {
            0x00 | 0x03 => {}
            0x08 => {
                if let Some(w) = self.cur()
                    && w.pen_col > 0
                {
                    w.pen_col -= 1;
                    let (r, c) = (usize::from(w.pen_row), usize::from(w.pen_col));
                    if let Some(cell) = w.cells.get_mut(r).and_then(|row| row.get_mut(c)) {
                        *cell = None;
                    }
                }
            }
            0x0C => {
                if let Some(w) = self.cur() {
                    for r in &mut w.cells {
                        r.fill(None);
                    }
                    w.pen_row = 0;
                    w.pen_col = 0;
                }
            }
            0x0D => {
                if let Some(w) = self.cur() {
                    if w.pen_row + 1 >= w.rows {
                        if !w.cells.is_empty() {
                            w.cells.remove(0);
                            w.cells.push(vec![None; usize::from(w.cols)]);
                        }
                    } else {
                        w.pen_row += 1;
                    }
                    w.pen_col = 0;
                }
            }
            0x0E => {
                if let Some(w) = self.cur() {
                    if let Some(row) = w.cells.get_mut(usize::from(w.pen_row)) {
                        row.fill(None);
                    }
                    w.pen_col = 0;
                }
            }
            0x10 => match Char708::decode(e[1], true) {
                Some(c) if e.len() == 2 => self.put(c),
                _ => self.unknown += 1,
            },
            0x01..=0x1F => self.unknown += 1,
            0x20..=0x7F | 0xA0..=0xFF => {
                if let Some(c) = Char708::decode(b, false) {
                    self.put(c);
                }
            }
            0x80..=0x87 => {
                let id = usize::from(b & 7);
                if self.windows[id].is_some() {
                    self.current = Some(id);
                }
            }
            0x88 => self.each(e[1], |w| {
                if let Some(w) = w {
                    for r in &mut w.cells {
                        r.fill(None);
                    }
                    w.pen_row = 0;
                    w.pen_col = 0;
                }
            }),
            0x89 => self.each(e[1], |w| {
                if let Some(w) = w {
                    w.visible = true;
                }
            }),
            0x8A => self.each(e[1], |w| {
                if let Some(w) = w {
                    w.visible = false;
                }
            }),
            0x8B => self.each(e[1], |w| {
                if let Some(w) = w {
                    w.visible = !w.visible;
                }
            }),
            0x8C => {
                if self.current.is_some_and(|c| e[1] & (1 << c) != 0) {
                    self.current = None;
                }
                self.each(e[1], |w| *w = None);
            }
            0x8D | 0x8E => {}
            0x8F => {
                self.windows = Default::default();
                self.current = None;
            }
            0x90 => {
                if let Some(w) = self.cur() {
                    w.pen = SpanStyle {
                        italic: e[2] & 0x80 != 0,
                        underline: e[2] & 0x40 != 0,
                    };
                }
            }
            0x91 | 0x97 => {}
            0x92 => {
                if let Some(w) = self.cur() {
                    w.pen_row = (e[1] & 0x0F).min(w.rows.saturating_sub(1));
                    w.pen_col = (e[2] & 0x3F).min(w.cols.saturating_sub(1));
                }
            }
            0x98..=0x9F => self.define(e),
            _ => self.unknown += 1,
        }
    }

    fn define(&mut self, e: &[u8]) {
        let id = usize::from(e[0] & 7);
        let def = [e[1], e[2], e[3], e[4], e[5], e[6]];
        let rows = (e[4] & 0x0F) + 1;
        let cols = (e[5] & 0x3F) + 1;
        let visible = e[1] & 0x20 != 0;
        let anchor = ANCHORS[usize::from((e[4] >> 4).min(8))];
        match &mut self.windows[id] {
            Some(w) => {
                // unchanged: ignored; changed: updated, pen kept (p.66)
                if w.def != def {
                    w.def = def;
                    w.rows = rows;
                    w.cols = cols;
                    w.visible = visible;
                    w.anchor = anchor;
                    w.av = e[2] & 0x7F;
                    w.ah = e[3];
                    w.resize();
                }
            }
            slot @ None => {
                let mut w = Window {
                    visible,
                    rows,
                    cols,
                    anchor,
                    av: e[2] & 0x7F,
                    ah: e[3],
                    cells: Vec::new(),
                    pen_row: 0,
                    pen_col: 0,
                    pen: SpanStyle::default(),
                    def,
                };
                w.resize();
                *slot = Some(w);
            }
        }
        self.current = Some(id);
    }

    fn put(&mut self, ch: char) {
        if let Some(w) = self.cur() {
            let (r, c) = (usize::from(w.pen_row), usize::from(w.pen_col));
            let style = w.pen;
            if let Some(cell) = w.cells.get_mut(r).and_then(|row| row.get_mut(c)) {
                *cell = Some(Cell { ch, style });
            }
            if w.pen_col + 1 < w.cols {
                w.pen_col += 1;
            }
        }
    }

    /// The visible windows' rows, windows in id order: `(window id, rows)`.
    pub fn screen(&self) -> Vec<(u8, Vec<ScreenRow>)> {
        self.windows
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                let w = w.as_ref()?;
                (w.visible).then(|| (i as u8, w.text_rows()))
            })
            .filter(|(_, r)| !r.is_empty())
            .collect()
    }

    /// The visible text: every visible window's rows joined with `\n`, windows in id order.
    pub fn text(&self) -> String {
        self.screen()
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(ScreenRow::text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Decodes one service of a DTVCC stream fed construct by construct.
#[derive(Clone, Debug)]
pub struct Decoder {
    service: u8,
    reasm: Reassembler,
    /// The service's windows.
    pub svc: ServiceDecoder,
    packets: Vec<Vec<u8>>,
    /// Packets received.
    pub packets_in: u64,
}

impl Decoder {
    /// A decoder of `service`.
    pub fn new(service: super::ServiceNumber) -> Self {
        Self {
            service: service.get(),
            reasm: Reassembler::new(),
            svc: ServiceDecoder::new(),
            packets: Vec::new(),
            packets_in: 0,
        }
    }

    /// The reassembler's counters.
    pub fn reassembler(&self) -> &Reassembler {
        &self.reasm
    }

    /// One `cc_data()` construct of `cc_type` 2 or 3 (others are ignored). Returns whether the
    /// visible text changed.
    pub fn feed(&mut self, valid: bool, ty: u8, data: [u8; 2]) -> bool {
        self.reasm.feed(valid, ty, data, &mut self.packets);
        if self.packets.is_empty() {
            return false;
        }
        let before = self.svc.screen();
        for p in std::mem::take(&mut self.packets) {
            self.packets_in += 1;
            for b in blocks(&p) {
                if b.service == self.service {
                    self.svc.feed(b.data);
                }
            }
            self.svc.end_of_packet();
        }
        self.svc.screen() != before
    }
}

#[cfg(test)]
mod tests {
    use super::super::packet::Packer;
    use super::super::*;
    use super::*;

    fn run(elements: Vec<Vec<u8>>, d: &mut Decoder, seq: &mut u8) {
        let mut p = Packer::new(ServiceNumber::PRIMARY, elements);
        while let Some(k) = p.next_packet(64) {
            for (i, pair) in k.pairs(*seq).enumerate() {
                d.feed(true, if i == 0 { 3 } else { 2 }, pair);
            }
            *seq = (*seq + 1) & 3;
        }
    }

    fn text(s: &str, out: &mut Vec<Vec<u8>>) {
        for c in s.chars() {
            out.push(Command::Char(Char708::from_char(c).unwrap()).bytes());
        }
    }

    #[test]
    fn pop_on_with_two_windows() {
        let mut d = Decoder::new(ServiceNumber::PRIMARY);
        let mut seq = 0;
        let def = |id: u8, visible| WindowDef {
            id: WindowId::new(id).unwrap(),
            priority: 0,
            anchor: Anchor::BottomCenter,
            relative: false,
            av: 74,
            ah: 104,
            rows: 2,
            cols: 32,
            row_lock: true,
            col_lock: true,
            visible,
            window_style: 2,
            pen_style: 1,
        };
        let mut load = vec![
            Command::DeleteWindows(WindowId::new(1).unwrap().map()).bytes(),
            Command::DefineWindow(def(1, false)).bytes(),
            Command::SetPenLocation { row: 1, col: 3 }.bytes(),
            Command::SetPenAttributes(PenAttrs::plain(true, false)).bytes(),
        ];
        text("Šé…♪", &mut load);
        load.push(Command::Etx.bytes());
        run(load, &mut d, &mut seq);
        assert_eq!(d.svc.text(), "", "hidden");
        let w = d.svc.window(1).unwrap();
        assert_eq!(w.text_rows()[0].text(), "Šé…♪");
        assert_eq!((w.text_rows()[0].row, w.text_rows()[0].col), (1, 3));
        assert!(w.text_rows()[0].cells[0].style.italic);
        run(
            vec![
                Command::DeleteWindows(WindowId::new(0).unwrap().map()).bytes(),
                Command::DisplayWindows(WindowId::new(1).unwrap().map()).bytes(),
            ],
            &mut d,
            &mut seq,
        );
        assert_eq!(d.svc.text(), "Šé…♪");
        run(
            vec![Command::DeleteWindows(WindowId::new(1).unwrap().map()).bytes()],
            &mut d,
            &mut seq,
        );
        assert_eq!(d.svc.text(), "");
        assert_eq!(d.reassembler().seq_breaks, 0);
        assert_eq!(d.svc.unknown, 0);
    }

    #[test]
    fn roll_up_and_other_services() {
        let mut d = Decoder::new(ServiceNumber::PRIMARY);
        let mut seq = 0;
        let def = WindowDef {
            id: WindowId::new(0).unwrap(),
            priority: 0,
            anchor: Anchor::BottomLeft,
            relative: false,
            av: 74,
            ah: 0,
            rows: 2,
            cols: 32,
            row_lock: true,
            col_lock: true,
            visible: true,
            window_style: 5,
            pen_style: 1,
        };
        for line in ["one", "two", "three"] {
            let mut e = vec![
                Command::DefineWindow(def).bytes(),
                Command::SetPenLocation { row: 1, col: 0 }.bytes(),
                Command::Cr.bytes(),
            ];
            text(line, &mut e);
            run(e, &mut d, &mut seq);
        }
        assert_eq!(d.svc.text(), "two\nthree");
        // another service's blocks are not ours
        let mut p = Packer::new(ServiceNumber::new(2).unwrap(), vec![Command::Ff.bytes()]);
        let k = p.next_packet(64).unwrap();
        for (i, pair) in k.pairs(seq).enumerate() {
            d.feed(true, if i == 0 { 3 } else { 2 }, pair);
        }
        assert_eq!(d.svc.text(), "two\nthree");
    }
}
