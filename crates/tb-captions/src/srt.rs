//! SubRip (`.srt`) input.
//!
//! A file is blocks separated by blank lines: a counter line, a timing line
//! `00:00:01,000 --> 00:00:02,500` (anything after the second time, such as SubRip's `X1:`
//! coordinates, is ignored), then the text lines (markup: [`crate::markup`]).
//!
//! Parsing never fails as a whole and never panics: a block whose timing cannot be read, or
//! whose end is not after its start, is skipped and reported with its line number in
//! [`Parsed::errors`]. Tolerated without a report, because real files have them: a byte order
//! mark, CRLF or CR line ends, a missing or wrong counter, `.` instead of `,` before the
//! milliseconds, fewer than three digits of fraction (a fraction, so `,5` is 500 ms), times
//! without hours, a missing blank line before the next counter-and-timing pair, several blank
//! lines in a row. Cues come out in file order (not sorted; the layout sorts).

use crate::cue::{Cue, Millis};
use crate::markup::{Dialect, parse_payload};

/// What a timed-text parser found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The cues that could be read, in file order.
    pub cues: Vec<Cue>,
    /// What was skipped, and why.
    pub errors: Vec<ParseError>,
}

/// A part of a file that was skipped.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {kind}")]
pub struct ParseError {
    /// 1-based line number where the problem is.
    pub line: usize,
    /// What it is.
    pub kind: ErrorKind,
}

/// Kinds of [`ParseError`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ErrorKind {
    /// A line with `-->` whose times cannot be read.
    #[error("cannot read the cue timing {0:?}")]
    Timing(String),
    /// The end time is not after the start time.
    #[error("the cue ends at {end} before or when it starts at {start}")]
    Order {
        /// Start.
        start: Millis,
        /// End.
        end: Millis,
    },
    /// Text outside a cue (before any timing line, or after a skipped one).
    #[error("text outside a cue")]
    Stray,
    /// A WebVTT file without the `WEBVTT` signature.
    #[error("no WEBVTT signature on the first line")]
    Signature,
}

/// Parses a SubRip file (see the module documentation).
pub fn parse(text: &str) -> Parsed {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = split_lines(text);
    let mut out = Parsed::default();
    let mut i = 0;
    let mut stray_reported = false;
    while i < lines.len() {
        let l = lines[i].trim();
        if !l.contains("-->") {
            // a counter, a blank line, or text that belongs to nothing
            if !l.is_empty() && !is_counter(l) && !stray_reported {
                out.errors.push(ParseError {
                    line: i + 1,
                    kind: ErrorKind::Stray,
                });
                stray_reported = true;
            }
            i += 1;
            continue;
        }
        stray_reported = false;
        let timing_line = i;
        i += 1;
        // the text runs to a blank line, or to the next counter-and-timing pair
        let start = i;
        while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].contains("-->") {
            if is_counter(lines[i].trim()) && lines.get(i + 1).is_some_and(|n| n.contains("-->")) {
                break;
            }
            i += 1;
        }
        let payload = lines[start..i].join("\n");
        match parse_timing(l) {
            Some((s, e)) if e > s => {
                let (lines, style) = parse_payload(&payload, Dialect::Srt);
                out.cues.push(Cue {
                    start: s,
                    end: e,
                    lines,
                    style,
                });
            }
            Some((start, end)) => out.errors.push(ParseError {
                line: timing_line + 1,
                kind: ErrorKind::Order { start, end },
            }),
            None => out.errors.push(ParseError {
                line: timing_line + 1,
                kind: ErrorKind::Timing(l.to_owned()),
            }),
        }
    }
    out
}

pub(crate) fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .flat_map(|l| {
            let l = l.strip_suffix('\r').unwrap_or(l);
            l.split('\r')
        })
        .collect()
}

fn is_counter(l: &str) -> bool {
    !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())
}

/// `start --> end [anything]`.
fn parse_timing(l: &str) -> Option<(Millis, Millis)> {
    let (a, b) = l.split_once("-->")?;
    let b = b.split_whitespace().next()?;
    Some((parse_timestamp(a.trim())?, parse_timestamp(b)?))
}

/// `[H+:]MM:SS[,.]f+` or `[H+:]MM:SS`: hours of any length, minutes and seconds below 60, a
/// fraction of up to three digits read as a fraction (more digits are truncated).
pub(crate) fn parse_timestamp(s: &str) -> Option<Millis> {
    let (clock, frac) = match s.find([',', '.']) {
        Some(k) => (&s[..k], Some(&s[k + 1..])),
        None => (s, None),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let num = |p: &str| -> Option<i64> {
        if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (num(h)?, num(m)?, num(s)?),
        [m, s] => (0, num(m)?, num(s)?),
        _ => return None,
    };
    if m >= 60 || sec >= 60 {
        return None;
    }
    let ms = match frac {
        None => 0,
        Some(f) => {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let d: String = f.chars().chain("00".chars()).take(3).collect();
            d.parse::<i64>().ok()?
        }
    };
    Some(Millis(((h * 60 + m) * 60 + sec) * 1000 + ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::Vertical;

    #[test]
    fn a_plain_file() {
        let f = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nHello\r\n<i>world</i>\r\n\r\n2\r\n\
                 00:00:03,000 --> 00:00:04,000 X1:10 X2:20 Y1:1 Y2:2\r\n{\\an8}Top\r\n";
        let p = parse(f);
        assert_eq!(p.errors, []);
        assert_eq!(p.cues.len(), 2);
        assert_eq!(
            (p.cues[0].start, p.cues[0].end),
            (Millis(1000), Millis(2500))
        );
        assert_eq!(p.cues[0].text(), "Hello\nworld");
        assert!(p.cues[0].lines[1].spans[0].style.italic);
        assert_eq!(p.cues[1].style.vertical, Vertical::Top);
        assert_eq!(p.cues[1].text(), "Top");
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("01:02:03,004"), Some(Millis(3_723_004)));
        assert_eq!(parse_timestamp("1:02:03.4"), Some(Millis(3_723_400)));
        assert_eq!(parse_timestamp("02:03,45"), Some(Millis(123_450)));
        assert_eq!(parse_timestamp("100:00:00,000"), Some(Millis(360_000_000)));
        assert_eq!(parse_timestamp("00:00:01"), Some(Millis(1000)));
        assert_eq!(parse_timestamp("00:00:01,12345"), Some(Millis(1123)));
        for bad in [
            "",
            "1",
            "00:60:00,000",
            "00:00:61,000",
            "a:b:c",
            "00:00:01,",
            "-1:00:00",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn malformed_input_is_reported_and_skipped() {
        let f = "junk before\n\n1\n00:00:01,000 --> nonsense\ntext of a bad cue\n\n\
                 2\n00:00:05,000 --> 00:00:04,000\nbackwards\n\n\
                 3\n00:00:06,000 --> 00:00:07,000\ngood\n4\n00:00:08,000 --> 00:00:09,000\n\
                 no blank line before me\n\n\n\n5\n00:00:10,000 --> 00:00:11,000\n";
        let p = parse(f);
        let kinds: Vec<_> = p.errors.iter().map(|e| (e.line, e.kind.clone())).collect();
        assert_eq!(
            kinds,
            [
                (1, ErrorKind::Stray),
                (4, ErrorKind::Timing("00:00:01,000 --> nonsense".into())),
                (
                    8,
                    ErrorKind::Order {
                        start: Millis(5000),
                        end: Millis(4000)
                    }
                ),
            ]
        );
        let texts: Vec<String> = p.cues.iter().map(Cue::text).collect();
        assert_eq!(texts, ["good", "no blank line before me", ""]);
    }

    /// Arbitrary bytes never panic.
    #[test]
    fn fuzz() {
        let mut x = 0x1234_5678_9abc_def0u64;
        let alphabet: Vec<char> = "0123456789:,.-> \n\r<>/iub{}\\an&;#xé♪\u{feff}"
            .chars()
            .collect();
        for _ in 0..3000 {
            let mut s = String::new();
            for _ in 0..(x % 200) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push(alphabet[(x % alphabet.len() as u64) as usize]);
            }
            let p = parse(&s);
            for c in &p.cues {
                assert!(c.end > c.start);
            }
            let _ = crate::vtt::parse(&s);
        }
    }
}
