//! From cues to screens: what is on the screen from one time to the next, as rows of at most
//! 32 cells on the 15 rows of CEA-608. One layout serves both services, so CEA-608 and CEA-708
//! show the same rows at the same places.
//!
//! **Screens.** Cues may overlap. The time line is cut at every start and end; each piece with
//! at least one cue is a [`Screen`] showing the lines of every cue active then, earlier cues
//! first. Pieces with the same rows are merged. A screen is what a pop-on caption shows
//! between two memory swaps.
//!
//! **Rows.** Rules, from the limits of CEA-608 (which CEA-708 captions meant for 4:3 and 16:9
//! receivers share: 32 characters per row, CTA-708-E §9.4 p.88; four rows, §9.5):
//! - at most 32 cells per row (CTA-608-E C.13, p.75), counted as [`crate::cea608::row_cells`]
//!   counts them: a cell per character, per mid-row code that a style change needs, and one
//!   more after an extended character in the last cell. Lines are wrapped at spaces, a word
//!   longer than a row is cut;
//! - at most four rows on the screen ("the FCC rules require that decoders be able to display
//!   up to four rows", CTA-608-E B.3 p.62, C.6 p.73). More rows: the cue's text is wrapped
//!   again as one paragraph; if that is still more than four rows, the screen is split in time
//!   into parts of four rows, the time shared by characters (reported);
//! - bottom cues end on [`LayoutCfg::bottom_row`] (15), top cues start on
//!   [`LayoutCfg::top_row`] (1); rows are consecutive. When top and bottom cues are on the
//!   screen together, each keeps its place;
//! - a row is centered (column `(32 - cells) / 2`), left aligned (column 0) or right aligned
//!   as its cue says. A row that starts in italics away from column 0 also uses the cell to
//!   its left for the mid-row code that starts the italics.
//!
//! **Characters.** The text keeps its Unicode characters (CEA-708 can show most of Latin-1);
//! the widths are those of CEA-608, where a character it lacks is replaced
//! ([`crate::cea608::chars::substitute`]: `…` by `...`, `’` by `'`) or, failing that, shown
//! as `?` (both counted in [`LayoutReport`]). Control characters are dropped.

use crate::cea608::{self, Char608, Row, StyledChar};
use crate::cue::{Align, Cue, Line, Millis, SpanStyle, Vertical};

/// Columns in a row.
pub const COLUMNS: usize = 32;

/// Where rows go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutCfg {
    /// The row the last line of a bottom cue is on.
    pub bottom_row: Row,
    /// The row the first line of a top cue is on.
    pub top_row: Row,
    /// Most rows on the screen, 1 to 4.
    pub max_rows: u8,
}

impl Default for LayoutCfg {
    fn default() -> Self {
        Self {
            bottom_row: Row::BOTTOM,
            top_row: Row::TOP,
            max_rows: 4,
        }
    }
}

/// A character of a laid-out row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The character as given (CEA-708 shows it if it can).
    pub ch: char,
    /// Its style.
    pub style: SpanStyle,
}

/// A row on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaidRow {
    /// Row 1 to 15.
    pub row: Row,
    /// Column of the first character (0 to 31).
    pub col: u8,
    /// The characters.
    pub cells: Vec<Cell>,
}

impl LaidRow {
    /// The row in CEA-608 characters (replacements applied).
    pub fn cea608(&self) -> Vec<StyledChar> {
        to_608(&self.cells).0
    }

    /// The text.
    pub fn text(&self) -> String {
        self.cells.iter().map(|c| c.ch).collect()
    }
}

/// What is shown from `start` to `end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// Start.
    pub start: Millis,
    /// End.
    pub end: Millis,
    /// Rows, top to bottom (rows are distinct).
    pub rows: Vec<LaidRow>,
}

impl Screen {
    /// The rows' texts joined with `\n`.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(LaidRow::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What the layout changed, for the operator.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutReport {
    /// Characters CEA-608 lacks that were replaced by near ones (counted in every screen
    /// that shows them).
    pub substituted: usize,
    /// Characters with no replacement, shown as `?` in CEA-608 (counted likewise).
    pub unknown: usize,
    /// Screens split in time because they had more than four rows: `(start, parts)`.
    pub split: Vec<(Millis, usize)>,
    /// Cues without text, or that end before they start: dropped.
    pub dropped: usize,
}

/// The CEA-608 characters of a run of cells, with the counts of replaced and unknown ones.
fn to_608(cells: &[Cell]) -> (Vec<StyledChar>, usize, usize) {
    let mut out = Vec::with_capacity(cells.len());
    let (mut sub, mut unk) = (0, 0);
    for c in cells {
        if let Some(x) = Char608::from_char(c.ch) {
            out.push((x, c.style));
        } else if let Some(s) = cea608::chars::substitute(c.ch) {
            sub += 1;
            out.extend(
                s.chars()
                    .filter_map(Char608::from_char)
                    .map(|x| (x, c.style)),
            );
        } else {
            unk += 1;
            if let Some(q) = Char608::from_char('?') {
                out.push((q, c.style));
            }
        }
    }
    (out, sub, unk)
}

fn width(cells: &[Cell]) -> usize {
    cea608::row_cells(&to_608(cells).0)
}

/// The cells of a line: control characters dropped, white space runs made one space.
fn cells_of(line: &Line) -> Vec<Cell> {
    let mut out: Vec<Cell> = Vec::new();
    for s in &line.spans {
        for ch in s.text.chars() {
            let ch = if ch.is_whitespace() && ch != '\u{a0}' {
                ' '
            } else {
                ch
            };
            if ch.is_control() {
                continue;
            }
            if ch == ' ' && out.last().is_none_or(|c| c.ch == ' ') {
                continue;
            }
            out.push(Cell { ch, style: s.style });
        }
    }
    while out.last().is_some_and(|c| c.ch == ' ') {
        out.pop();
    }
    punctuation_follows_words(&mut out);
    out
}

/// Punctuation joined to a word takes the word's style: in `<i>world</i>!` the `!` is set in
/// italics too. CEA-608 changes style with a mid-row code that takes a cell and shows as a
/// space (CTA-608-E Table 51), so a change inside a word would split it; a change of style
/// at a space costs nothing ([`crate::cea608::row_cells`]).
fn punctuation_follows_words(cells: &mut [Cell]) {
    let punct = |c: char| !c.is_alphanumeric() && c != ' ';
    let mut i = 0;
    while i < cells.len() {
        if cells[i].ch == ' ' {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < cells.len() && cells[j].ch != ' ' {
            j += 1;
        }
        let word = &mut cells[i..j];
        // the style of the first and last letter or digit
        if let (Some(a), Some(b)) = (
            word.iter().position(|c| !punct(c.ch)),
            word.iter().rposition(|c| !punct(c.ch)),
        ) {
            let (sa, sb) = (word[a].style, word[b].style);
            for c in &mut word[..a] {
                c.style = sa;
            }
            for c in &mut word[b + 1..] {
                c.style = sb;
            }
        }
        i = j;
    }
}

/// Greedy word wrap of one line into rows of at most [`COLUMNS`] cells.
fn wrap(cells: &[Cell]) -> Vec<Vec<Cell>> {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut cur: Vec<Cell> = Vec::new();
    // words with the space before them
    let mut words: Vec<&[Cell]> = Vec::new();
    let mut start = 0;
    for (i, c) in cells.iter().enumerate() {
        if c.ch == ' ' && i > start {
            words.push(&cells[start..i]);
            start = i + 1;
        } else if c.ch == ' ' {
            start = i + 1;
        }
    }
    if start < cells.len() {
        words.push(&cells[start..]);
    }
    for w in words {
        let mut trial = cur.clone();
        if !trial.is_empty() {
            // the space takes the style of the word before it
            let style = trial.last().map(|c: &Cell| c.style).unwrap_or_default();
            trial.push(Cell { ch: ' ', style });
        }
        trial.extend_from_slice(w);
        if width(&trial) <= COLUMNS {
            cur = trial;
            continue;
        }
        if !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
        }
        // a word longer than a row is cut
        let mut rest = w;
        while width(rest) > COLUMNS {
            let mut n = rest.len().min(COLUMNS);
            while n > 1 && width(&rest[..n]) > COLUMNS {
                n -= 1;
            }
            rows.push(rest[..n].to_vec());
            rest = &rest[n..];
        }
        cur = rest.to_vec();
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// Rows of a group of cues sharing a placement, wrapped.
fn group_rows(lines: &[&Line]) -> Vec<Vec<Cell>> {
    lines.iter().flat_map(|l| wrap(&cells_of(l))).collect()
}

/// The same text as one paragraph.
fn paragraph(lines: &[&Line]) -> Vec<Vec<Cell>> {
    let mut all: Vec<Cell> = Vec::new();
    for l in lines {
        let c = cells_of(l);
        if c.is_empty() {
            continue;
        }
        if let Some(last) = all.last().copied() {
            all.push(Cell {
                ch: ' ',
                style: last.style,
            });
        }
        all.extend(c);
    }
    wrap(&all)
}

fn place(row: Row, cells: Vec<Cell>, align: Align) -> LaidRow {
    let w = width(&cells).min(COLUMNS);
    let col = match align {
        Align::Center => (COLUMNS - w) / 2,
        Align::Left => 0,
        Align::Right => COLUMNS - w,
    };
    LaidRow {
        row,
        col: col as u8,
        cells,
    }
}

/// The screens of a list of cues (in any order), and what the layout changed.
pub fn screens(cues: &[Cue], cfg: &LayoutCfg) -> (Vec<Screen>, LayoutReport) {
    let mut rep = LayoutReport::default();
    let mut cues: Vec<&Cue> = cues
        .iter()
        .filter(|c| {
            let keep = c.end > c.start && c.lines.iter().any(|l| !l.is_blank());
            if !keep {
                rep.dropped += 1;
            }
            keep
        })
        .collect();
    cues.sort_by_key(|c| c.start);
    let mut cuts: Vec<Millis> = cues.iter().flat_map(|c| [c.start, c.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<Screen> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let active: Vec<&Cue> = cues
            .iter()
            .copied()
            .filter(|c| c.start <= a && c.end >= b)
            .collect();
        if active.is_empty() {
            continue;
        }
        for rows in compose(&active, cfg) {
            match out.last_mut() {
                Some(last) if last.end == a && last.rows == rows => last.end = b,
                _ => out.push(Screen {
                    start: a,
                    end: b,
                    rows,
                }),
            }
        }
    }
    // splits: a composed screen of more than max rows comes back as several row sets; share
    // the time between them
    let out = share_splits(out, &mut rep);
    for s in &out {
        for r in &s.rows {
            let (_, sub, unk) = to_608(&r.cells);
            rep.substituted += sub;
            rep.unknown += unk;
        }
    }
    (out, rep)
}

/// Rows of the active cues; more than one set when they must be split in time.
fn compose(active: &[&Cue], cfg: &LayoutCfg) -> Vec<Vec<LaidRow>> {
    let max = usize::from(cfg.max_rows.clamp(1, 4));
    let pick = |v: Vertical| -> (Vec<&Line>, Align) {
        let mut lines = Vec::new();
        let mut align = None;
        for c in active.iter().filter(|c| c.style.vertical == v) {
            align.get_or_insert(c.style.align);
            lines.extend(c.lines.iter().filter(|l| !l.is_blank()));
        }
        (lines, align.unwrap_or_default())
    };
    let (top, top_align) = pick(Vertical::Top);
    let (bot, bot_align) = pick(Vertical::Bottom);
    let mut t = group_rows(&top);
    let mut b = group_rows(&bot);
    if t.len() + b.len() > max {
        t = paragraph(&top);
        b = paragraph(&bot);
    }
    if t.len() + b.len() > max {
        // everything at the bottom, in parts of `max` rows
        let mut all: Vec<&Line> = top;
        all.extend(bot);
        let rows = paragraph(&all);
        return rows
            .chunks(max)
            .map(|chunk| bottom_rows(chunk.to_vec(), bot_align, cfg))
            .collect();
    }
    let mut rows = Vec::new();
    let top_first = cfg.top_row.get();
    for (i, r) in t.into_iter().enumerate() {
        if let Some(row) = Row::new(top_first + i as u8) {
            rows.push(place(row, r, top_align));
        }
    }
    let mut bottom = bottom_rows(b, bot_align, cfg);
    // a top group that reaches the bottom group's rows is moved up by the overlap (only with
    // a top row far down)
    if let (Some(lt), Some(fb)) = (
        rows.last().map(|r: &LaidRow| r.row),
        bottom.first().map(|r| r.row),
    ) && lt >= fb
    {
        let shift = lt.get() - fb.get() + 1;
        for r in &mut rows {
            if let Some(n) = Row::new(r.row.get().saturating_sub(shift)) {
                r.row = n;
            }
        }
    }
    rows.append(&mut bottom);
    vec![rows]
}

fn bottom_rows(rows: Vec<Vec<Cell>>, align: Align, cfg: &LayoutCfg) -> Vec<LaidRow> {
    let n = rows.len() as u8;
    let last = cfg.bottom_row.get().max(n);
    rows.into_iter()
        .enumerate()
        .filter_map(|(i, r)| Row::new(last + 1 - n + i as u8).map(|row| place(row, r, align)))
        .collect()
}

/// Screens with the same start and end come from one split; their time is shared by the
/// number of characters each shows.
fn share_splits(screens: Vec<Screen>, rep: &mut LayoutReport) -> Vec<Screen> {
    let mut out: Vec<Screen> = Vec::with_capacity(screens.len());
    let mut i = 0;
    while i < screens.len() {
        let mut j = i + 1;
        while j < screens.len()
            && screens[j].start == screens[i].start
            && screens[j].end == screens[i].end
        {
            j += 1;
        }
        if j - i == 1 {
            out.push(screens[i].clone());
        } else {
            let (a, b) = (screens[i].start.0, screens[i].end.0);
            let weights: Vec<i64> = screens[i..j]
                .iter()
                .map(|s| {
                    s.rows
                        .iter()
                        .map(|r| r.cells.len() as i64)
                        .sum::<i64>()
                        .max(1)
                })
                .collect();
            let total: i64 = weights.iter().sum();
            let mut t = a;
            let mut acc = 0;
            for (k, s) in screens[i..j].iter().enumerate() {
                acc += weights[k];
                let end = if k + 1 == j - i {
                    b
                } else {
                    a + (b - a) * acc / total
                };
                out.push(Screen {
                    start: Millis(t),
                    end: Millis(end),
                    rows: s.rows.clone(),
                });
                t = end;
            }
            rep.split.push((Millis(a), j - i));
        }
        i = j;
    }
    out.retain(|s| s.end > s.start);
    out
}

/// The rows of one cue for roll-up captions: its lines wrapped, left to right as the cue
/// aligns them, without a row number (roll-up rows always end on the base row).
pub fn roll_up_rows(cue: &Cue) -> Vec<LaidRow> {
    let lines: Vec<&Line> = cue.lines.iter().filter(|l| !l.is_blank()).collect();
    group_rows(&lines)
        .into_iter()
        .map(|r| place(Row::BOTTOM, r, cue.style.align))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{CueStyle, Span};

    fn texts(s: &Screen) -> Vec<(u8, u8, String)> {
        s.rows
            .iter()
            .map(|r| (r.row.get(), r.col, r.text()))
            .collect()
    }

    #[test]
    fn centered_bottom_rows() {
        let cues = [Cue::plain(Millis(1000), Millis(2000), &["Hello", "world!"])];
        let (s, rep) = screens(&cues, &LayoutCfg::default());
        assert_eq!(rep, LayoutReport::default());
        assert_eq!(s.len(), 1);
        assert_eq!(
            texts(&s[0]),
            [(14, 13, "Hello".into()), (15, 13, "world!".into())]
        );
    }

    #[test]
    fn wrapping_and_limits() {
        let long = "The quick brown fox jumps over the lazy dog and keeps on running far away";
        let cues = [Cue::plain(Millis(0), Millis(4000), &[long])];
        let (s, _) = screens(&cues, &LayoutCfg::default());
        let t = texts(&s[0]);
        assert_eq!(t.len(), 3);
        for (_, col, text) in &t {
            assert!(text.chars().count() <= 32);
            assert!(usize::from(*col) + text.chars().count() <= 32);
        }
        assert_eq!(
            t.iter().map(|x| x.2.as_str()).collect::<Vec<_>>().join(" "),
            long
        );
        // a word longer than a row
        let w = "x".repeat(70);
        let (s, _) = screens(
            &[Cue::plain(Millis(0), Millis(1), &[&w])],
            &LayoutCfg::default(),
        );
        let t = texts(&s[0]);
        assert_eq!(
            t.iter().map(|x| x.2.chars().count()).collect::<Vec<_>>(),
            [32, 32, 6]
        );
        // more than four rows: one paragraph, then split in time
        let lines: Vec<String> = (0..6).map(|i| format!("line {i}")).collect();
        let lr: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (s, rep) = screens(
            &[Cue::plain(Millis(0), Millis(600), &lr)],
            &LayoutCfg::default(),
        );
        assert_eq!(
            s.len(),
            1,
            "six short lines fit in one paragraph of two rows"
        );
        assert_eq!(texts(&s[0]).len(), 2);
        assert!(rep.split.is_empty());
        let words = "word ".repeat(60);
        let (s, rep) = screens(
            &[Cue::plain(Millis(0), Millis(1000), &[&words])],
            &LayoutCfg::default(),
        );
        assert_eq!(rep.split, [(Millis(0), 3)]);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].start, Millis(0));
        assert_eq!(s[2].end, Millis(1000));
        assert!(s.iter().all(|x| x.rows.len() <= 4));
        assert!(s.windows(2).all(|w| w[0].end == w[1].start));
    }

    #[test]
    fn overlaps_make_screens() {
        let mut top = Cue::plain(Millis(500), Millis(2500), &["Sign: EXIT"]);
        top.style = CueStyle {
            vertical: Vertical::Top,
            align: Align::Left,
        };
        let cues = [
            Cue::plain(Millis(0), Millis(1000), &["A"]),
            Cue::plain(Millis(1000), Millis(2000), &["B"]),
            Cue::plain(Millis(1500), Millis(3000), &["C"]),
            top,
            Cue::plain(Millis(4000), Millis(5000), &[""]),
        ];
        let (s, rep) = screens(&cues, &LayoutCfg::default());
        assert_eq!(rep.dropped, 1);
        type Rows = Vec<(u8, u8, String)>;
        let got: Vec<(i64, i64, Rows)> = s.iter().map(|x| (x.start.0, x.end.0, texts(x))).collect();
        assert_eq!(
            got,
            [
                (0, 500, vec![(15, 15, "A".into())]),
                (
                    500,
                    1000,
                    vec![(1, 0, "Sign: EXIT".into()), (15, 15, "A".into())]
                ),
                (
                    1000,
                    1500,
                    vec![(1, 0, "Sign: EXIT".into()), (15, 15, "B".into())]
                ),
                (
                    1500,
                    2000,
                    vec![
                        (1, 0, "Sign: EXIT".into()),
                        (14, 15, "B".into()),
                        (15, 15, "C".into())
                    ]
                ),
                (
                    2000,
                    2500,
                    vec![(1, 0, "Sign: EXIT".into()), (15, 15, "C".into())]
                ),
                (2500, 3000, vec![(15, 15, "C".into())]),
            ]
        );
    }

    #[test]
    fn characters_and_styles() {
        let mut l = Line::default();
        l.spans.push(Span {
            text: "It’s… ".into(),
            style: SpanStyle::default(),
        });
        l.spans.push(Span {
            text: "漢 \u{7}fine".into(),
            style: SpanStyle {
                italic: true,
                underline: false,
            },
        });
        let c = Cue {
            start: Millis(0),
            end: Millis(10),
            lines: vec![l],
            style: CueStyle::default(),
        };
        let (s, rep) = screens(&[c], &LayoutCfg::default());
        assert_eq!(s[0].rows[0].text(), "It’s… 漢 fine");
        assert_eq!((rep.substituted, rep.unknown), (2, 1));
        let r: String = s[0].rows[0]
            .cea608()
            .iter()
            .map(|c| c.0.to_char())
            .collect();
        assert_eq!(r, "It's... ? fine");
        // the 608 width decides the column: 14 characters, the change to italics on a space
        assert_eq!(s[0].rows[0].col, 9);
    }
}
