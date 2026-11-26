//! WebVTT (`.vtt`) input.
//!
//! The signature line `WEBVTT`, then blocks separated by blank lines: `NOTE`, `STYLE` and
//! `REGION` blocks (skipped), and cues: an optional identifier line, a timing line
//! `[hh:]mm:ss.ttt --> [hh:]mm:ss.ttt [settings]`, the payload (markup: [`crate::markup`]).
//!
//! Positioning hints from the cue settings: `line:` as a percentage below 50 or a
//! non-negative line number puts the cue at the top, otherwise at the bottom (WebVTT's
//! default `line:auto` is the bottom); `align:start|left` and `align:end|right` align the
//! lines. `position:`, `size:`, `vertical:` and `region:` are ignored (CEA-608 has 32 fixed
//! columns; vertical text has no counterpart).
//!
//! As for SubRip, nothing fails as a whole or panics; skipped parts are in
//! [`crate::srt::Parsed::errors`]. A missing signature is reported and the file parsed
//! anyway. The timestamp reader is SubRip's, which accepts WebVTT's forms and a few more.

use crate::cue::{Align, Cue, CueStyle, Vertical};
use crate::markup::{Dialect, parse_payload};
use crate::srt::{ErrorKind, ParseError, Parsed, parse_timestamp, split_lines};

/// Parses a WebVTT file (see the module documentation).
pub fn parse(text: &str) -> Parsed {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines = split_lines(text);
    let mut out = Parsed::default();
    let sig = lines.first().copied().unwrap_or("");
    let signed = sig
        .strip_prefix("WEBVTT")
        .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t']));
    if !signed {
        out.errors.push(ParseError {
            line: 1,
            kind: ErrorKind::Signature,
        });
    }
    // blocks: runs of non-blank lines
    let mut i = usize::from(signed);
    // the header (text after the signature up to the first blank line) is not a cue
    if signed {
        while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].contains("-->") {
            i += 1;
        }
    }
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && !lines[i].trim().is_empty() {
            i += 1;
        }
        block(&lines[start..i], start, &mut out);
    }
    out
}

fn block(b: &[&str], first: usize, out: &mut Parsed) {
    let head = b[0].trim_start();
    if !b[0].contains("-->")
        && (head.starts_with("NOTE") || head.starts_with("STYLE") || head.starts_with("REGION"))
    {
        return;
    }
    // the timing line is the first or (after an identifier) the second
    let Some(t) = b.iter().take(2).position(|l| l.contains("-->")) else {
        out.errors.push(ParseError {
            line: first + 1,
            kind: ErrorKind::Stray,
        });
        return;
    };
    let line = b[t];
    let Some((s, e, style)) = timing(line) else {
        out.errors.push(ParseError {
            line: first + t + 1,
            kind: ErrorKind::Timing(line.trim().to_owned()),
        });
        return;
    };
    if e <= s {
        out.errors.push(ParseError {
            line: first + t + 1,
            kind: ErrorKind::Order { start: s, end: e },
        });
        return;
    }
    let (lines, _) = parse_payload(&b[t + 1..].join("\n"), Dialect::Vtt);
    out.cues.push(Cue {
        start: s,
        end: e,
        lines,
        style,
    });
}

fn timing(l: &str) -> Option<(crate::cue::Millis, crate::cue::Millis, CueStyle)> {
    let (a, rest) = l.split_once("-->")?;
    let mut words = rest.split_whitespace();
    let b = words.next()?;
    let (s, e) = (parse_timestamp(a.trim())?, parse_timestamp(b)?);
    let mut style = CueStyle::default();
    for w in words {
        let Some((k, v)) = w.split_once(':') else {
            continue;
        };
        match k {
            "line" => {
                let v = v.split(',').next().unwrap_or("");
                let top = if let Some(p) = v.strip_suffix('%') {
                    p.parse::<f64>().is_ok_and(|p| p < 50.0)
                } else {
                    v.parse::<i64>().is_ok_and(|n| n >= 0)
                };
                style.vertical = if top { Vertical::Top } else { Vertical::Bottom };
            }
            "align" => {
                style.align = match v {
                    "start" | "left" => Align::Left,
                    "end" | "right" => Align::Right,
                    _ => Align::Center,
                };
            }
            _ => {}
        }
    }
    Some((s, e, style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::Millis;

    #[test]
    fn a_file() {
        let f = "WEBVTT - some title\nKind: captions\n\nNOTE a comment\nspanning --> lines\n\n\
                 STYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.500 line:0 align:start\n\
                 <v Ann>Hello &amp; <i>welcome</i>\n\n00:00:03.000 --> 00:00:04.000 line:-1\n\
                 Bottom\n\n00:05.000 --> 00:06.000 line:10%\nTop again\n";
        let p = parse(f);
        assert_eq!(p.errors, []);
        assert_eq!(p.cues.len(), 3);
        let c = &p.cues[0];
        assert_eq!((c.start, c.end), (Millis(1000), Millis(2500)));
        assert_eq!(c.text(), "Hello & welcome");
        assert_eq!(
            c.style,
            CueStyle {
                vertical: Vertical::Top,
                align: Align::Left
            }
        );
        assert_eq!(p.cues[1].style.vertical, Vertical::Bottom);
        assert_eq!(p.cues[2].style.vertical, Vertical::Top);
    }

    #[test]
    fn malformed() {
        let f = "not a signature\n\n00:01.000 --> 00:00.500\nx\n\nfoo\nbar\nbaz\n\n\
                 00:02.000 --> 00:03.000\nok\n\nid\n00:xx --> 00:04.000\ny\n";
        let p = parse(f);
        let kinds: Vec<_> = p.errors.iter().map(|e| (e.line, e.kind.clone())).collect();
        assert_eq!(
            kinds,
            [
                (1, ErrorKind::Signature),
                (1, ErrorKind::Stray),
                (
                    3,
                    ErrorKind::Order {
                        start: Millis(1000),
                        end: Millis(500)
                    }
                ),
                (6, ErrorKind::Stray),
                (14, ErrorKind::Timing("00:xx --> 00:04.000".into())),
            ]
        );
        assert_eq!(p.cues.len(), 1);
        assert_eq!(p.cues[0].text(), "ok");
        assert_eq!(parse("WEBVTT").cues, []);
        assert_eq!(parse("WEBVTTX").errors.len(), 2);
    }
}
