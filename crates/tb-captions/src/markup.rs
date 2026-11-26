//! The inline markup of a cue's text: SubRip's HTML-like tags and `{\an}` overrides, WebVTT's
//! cue text tags and character references.
//!
//! Mapped: `<i>` to italics, `<u>` to underline (both nest; an unclosed tag runs to the end of
//! the cue, as players do). Dropped with their markup, text kept: `<b>` (no bold in
//! CEA-608/708), `<font ...>`, `<c.class>`, `<v Speaker>`, `<lang xx>`, `<ruby>`, WebVTT
//! timestamp tags. Dropped with their text: `<rt>` (ruby annotations; a caption row has no room
//! for them). SubRip `{\anN}` gives the placement (7 to 9 top, 1 to 3 bottom; 1/4/7 left,
//! 3/6/9 right); other `{\...}` override blocks are removed. A `<` that does not start a tag
//! closed on the same line is text. Character references: `&amp; &lt; &gt; &quot; &apos;
//! &nbsp;` and numeric ones; `&lrm; &rlm;` are removed; an unknown reference is text.

use crate::cue::{Align, CueStyle, Line, SpanStyle, Vertical};

/// Which format's rules apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// SubRip: `{\an}` overrides are interpreted.
    Srt,
    /// WebVTT.
    Vtt,
}

/// Lines and placement hints of a cue payload (lines separated by `\n`, `\r\n` or `\r`).
/// Leading and trailing blank lines are removed; white space at the ends of each line is
/// trimmed.
pub fn parse_payload(text: &str, dialect: Dialect) -> (Vec<Line>, CueStyle) {
    let mut style = CueStyle::default();
    let mut st = State::default();
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        for raw in raw.split('\r') {
            lines.push(parse_line(raw, dialect, &mut st, &mut style));
        }
    }
    while lines.first().is_some_and(Line::is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(Line::is_blank) {
        lines.pop();
    }
    (lines, style)
}

#[derive(Default)]
struct State {
    italic: u32,
    underline: u32,
    ruby_text: u32,
}

impl State {
    fn style(&self) -> SpanStyle {
        SpanStyle {
            italic: self.italic > 0,
            underline: self.underline > 0,
        }
    }
}

fn parse_line(raw: &str, dialect: Dialect, st: &mut State, style: &mut CueStyle) -> Line {
    let mut line = Line::default();
    let mut text = String::new();
    let mut rest = raw;
    let flush = |line: &mut Line, text: &mut String, st: &State| {
        if st.ruby_text == 0 {
            line.push(text, st.style());
        }
        text.clear();
    };
    while let Some(c) = rest.chars().next() {
        match c {
            '<' => {
                if let Some(end) = rest.find('>') {
                    let tag = &rest[1..end];
                    if is_tag(tag) {
                        flush(&mut line, &mut text, st);
                        apply_tag(tag, st);
                        rest = &rest[end + 1..];
                        continue;
                    }
                }
                text.push('<');
                rest = &rest[1..];
            }
            '{' if dialect == Dialect::Srt && rest.starts_with("{\\") => match rest.find('}') {
                Some(end) => {
                    apply_override(&rest[2..end], style);
                    rest = &rest[end + 1..];
                }
                None => {
                    text.push('{');
                    rest = &rest[1..];
                }
            },
            '&' => {
                let (s, n) = reference(rest);
                text.push_str(s);
                rest = &rest[n..];
            }
            '\t' => {
                text.push(' ');
                rest = &rest[1..];
            }
            c => {
                text.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut line, &mut text, st);
    trim_line(line)
}

/// A tag: a name of letters (possibly after `/`), or a WebVTT timestamp.
fn is_tag(tag: &str) -> bool {
    let t = tag.strip_prefix('/').unwrap_or(tag);
    let first = t.chars().next();
    match first {
        Some(c) if c.is_ascii_alphabetic() => true,
        Some(c) if c.is_ascii_digit() => t
            .chars()
            .all(|c| c.is_ascii_digit() || c == ':' || c == '.'),
        _ => false,
    }
}

fn apply_tag(tag: &str, st: &mut State) {
    let (close, t) = match tag.strip_prefix('/') {
        Some(t) => (true, t),
        None => (false, tag),
    };
    // the name ends at white space or a class dot
    let name: String = t
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let counter = match name.as_str() {
        "i" => &mut st.italic,
        "u" => &mut st.underline,
        "rt" => &mut st.ruby_text,
        _ => return,
    };
    if close {
        *counter = counter.saturating_sub(1);
    } else {
        *counter += 1;
    }
}

fn apply_override(body: &str, style: &mut CueStyle) {
    // e.g. "an8" or "an8\i1": only the alignment matters
    for part in body.split('\\') {
        if let Some(n) = part
            .strip_prefix("an")
            .and_then(|d| d.trim().parse::<u8>().ok())
            && (1..=9).contains(&n)
        {
            style.vertical = if n >= 7 {
                Vertical::Top
            } else {
                Vertical::Bottom
            };
            style.align = match n % 3 {
                1 => Align::Left,
                0 => Align::Right,
                _ => Align::Center,
            };
        }
    }
}

/// The text of a character reference at the start of `s` and the bytes it takes; `("&", 1)`
/// when it is not one.
fn reference(s: &str) -> (&'static str, usize) {
    const NAMED: [(&str, &str); 8] = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&nbsp;", "\u{a0}"),
        ("&lrm;", ""),
        ("&rlm;", ""),
    ];
    for (name, text) in NAMED {
        if s.get(..name.len())
            .is_some_and(|h| h.eq_ignore_ascii_case(name))
        {
            return (text, name.len());
        }
    }
    // numeric references are rare in captions; the common printable ones are mapped
    if let Some(end) = s.find(';').filter(|&e| e <= 10) {
        let body = &s[1..end];
        let n = if let Some(h) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
            u32::from_str_radix(h, 16).ok()
        } else {
            body.strip_prefix('#').and_then(|d| d.parse::<u32>().ok())
        };
        if let Some(t) = n.and_then(numeric) {
            return (t, end + 1);
        }
    }
    ("&", 1)
}

/// A static string for a numeric character reference of a character the encoders know.
fn numeric(n: u32) -> Option<&'static str> {
    // Every character of the CEA-608 and CEA-708 sets is in the Basic Multilingual Plane;
    // keep a small table rather than allocate.
    Some(match n {
        0x20..=0x7E => {
            const ASCII: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
            let i = (n - 0x20) as usize;
            &ASCII[i..i + 1]
        }
        0xA0 => "\u{a0}",
        0x266A => "\u{266a}",
        0x2014 => "\u{2014}",
        0x2018 => "\u{2018}",
        0x2019 => "\u{2019}",
        0x201C => "\u{201c}",
        0x201D => "\u{201d}",
        0x2026 => "\u{2026}",
        _ => return None,
    })
}

fn trim_line(mut line: Line) -> Line {
    if let Some(first) = line.spans.first_mut() {
        let t = first.text.trim_start().to_owned();
        first.text = t;
    }
    if let Some(last) = line.spans.last_mut() {
        let t = last.text.trim_end().to_owned();
        last.text = t;
    }
    line.spans.retain(|s| !s.text.is_empty());
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(l: &Line) -> Vec<(String, bool, bool)> {
        l.spans
            .iter()
            .map(|s| (s.text.clone(), s.style.italic, s.style.underline))
            .collect()
    }

    #[test]
    fn tags() {
        let (l, _) = parse_payload("A <i>b <u>c</u></i> d", Dialect::Srt);
        assert_eq!(
            styled(&l[0]),
            [
                ("A ".into(), false, false),
                ("b ".into(), true, false),
                ("c".into(), true, true),
                (" d".into(), false, false)
            ]
        );
        // unclosed italics run to the end of the cue, over line breaks
        let (l, _) = parse_payload("<i>one\ntwo", Dialect::Srt);
        assert!(l[1].spans[0].style.italic);
        // bold, font, classes and voices keep their text
        let (l, _) = parse_payload(
            "<b>B</b> <font color=\"red\">R</font> <c.yellow>Y</c> <v Bob>hi</v>",
            Dialect::Vtt,
        );
        assert_eq!(l[0].text(), "B R Y hi");
        // ruby text is dropped, timestamps are not text
        let (l, _) = parse_payload("<ruby>漢<rt>kan</rt></ruby> <00:01.500>x", Dialect::Vtt);
        assert_eq!(l[0].text(), "漢 x");
        // not tags
        let (l, _) = parse_payload("1 < 2 and 3 > 2, <3, a<", Dialect::Vtt);
        assert_eq!(l[0].text(), "1 < 2 and 3 > 2, <3, a<");
        // a closing tag never seen opened does not underflow
        let (l, _) = parse_payload("</i>x<i>y", Dialect::Srt);
        assert_eq!(
            styled(&l[0]),
            [("x".into(), false, false), ("y".into(), true, false)]
        );
    }

    #[test]
    fn references_and_overrides() {
        let (l, _) = parse_payload(
            "a &amp; b &lt;c&gt; &#9834; &#x2014; &bogus; &",
            Dialect::Vtt,
        );
        assert_eq!(l[0].text(), "a & b <c> \u{266a} \u{2014} &bogus; &");
        let (l, s) = parse_payload("{\\an8}Top line", Dialect::Srt);
        assert_eq!(l[0].text(), "Top line");
        assert_eq!(s.vertical, Vertical::Top);
        assert_eq!(s.align, Align::Center);
        let (_, s) = parse_payload("{\\an1}x", Dialect::Srt);
        assert_eq!((s.vertical, s.align), (Vertical::Bottom, Align::Left));
        let (l, s) = parse_payload("{\\pos(10,20)}{\\an9}x{", Dialect::Srt);
        assert_eq!((l[0].text(), s.align), ("x{".into(), Align::Right));
        // WebVTT does not know the overrides
        let (l, _) = parse_payload("{\\an8}x", Dialect::Vtt);
        assert_eq!(l[0].text(), "{\\an8}x");
    }

    #[test]
    fn lines() {
        let (l, _) = parse_payload("\r\n  one  \r\ntwo\rthree\n\n", Dialect::Srt);
        let t: Vec<String> = l.iter().map(Line::text).collect();
        assert_eq!(t, ["one", "two", "three"]);
        let (l, _) = parse_payload("", Dialect::Srt);
        assert!(l.is_empty());
        let (l, _) = parse_payload("a\tb", Dialect::Srt);
        assert_eq!(l[0].text(), "a b");
    }
}
