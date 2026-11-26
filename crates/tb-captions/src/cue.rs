//! The timed-text model every input format is parsed into: a [`Cue`] is text shown from a start
//! time to an end time, as lines of styled spans, with optional placement hints.
//!
//! **Time** is in milliseconds ([`Millis`]), on the channel's caption clock: 0 is the display
//! time of the first picture the [`crate::Captioner`] writes. SubRip and WebVTT give
//! milliseconds, so nothing is rounded on input; the scheduler converts to field periods of
//! 1001/60000 s with exact integer arithmetic ([`Millis::to_field_periods`]). Milliseconds
//! rather than 27 MHz ticks because no source of cue times is finer and the output grid (a
//! field period, 16.68 ms) is far coarser; a [`tb_media::Packet`] in any time base is
//! converted with [`Cue::from_text_packet`].

use std::fmt;

use tb_media::{Packet, Rational};

/// A time or duration in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Millis(pub i64);

impl Millis {
    /// Field periods of `rate` from time 0 to this time, rounded to nearest (ties up). Exact:
    /// `ms x num / (1000 x den)` in 128-bit integers.
    pub fn to_field_periods(self, rate: FieldRate) -> i64 {
        let n = i128::from(self.0) * i128::from(rate.num);
        let d = 1000 * i128::from(rate.den);
        let q = (2 * n + d).div_euclid(2 * d);
        i64::try_from(q).unwrap_or(if q < 0 { i64::MIN } else { i64::MAX })
    }

    /// The time of the start of field period `p` of `rate`, rounded to nearest.
    pub fn of_field_period(p: i64, rate: FieldRate) -> Self {
        let n = i128::from(p) * 1000 * i128::from(rate.den);
        let d = i128::from(rate.num);
        let q = (2 * n + d).div_euclid(2 * d);
        Self(i64::try_from(q).unwrap_or(if q < 0 { i64::MIN } else { i64::MAX }))
    }
}

impl fmt::Display for Millis {
    /// `HH:MM:SS.mmm` (negative times with a leading `-`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let t = self.0.unsigned_abs();
        write!(
            f,
            "{sign}{:02}:{:02}:{:02}.{:03}",
            t / 3_600_000,
            t / 60_000 % 60,
            t / 1000 % 60,
            t % 1000
        )
    }
}

/// The field rate of the output: field periods per second as `num / den`. Every output format
/// of the encoder is of the 59.94 Hz family (29.97 Hz frames of two fields, 59.94 Hz frames,
/// film pictures of two and three fields); the caption bandwidth of CTA-708-E §4.3.6 is fixed
/// per field period (10 constructs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldRate {
    num: u32,
    den: u32,
}

impl FieldRate {
    /// 60000/1001: the rate of 29.97 Hz and 59.94 Hz video.
    pub const NTSC: Self = Self {
        num: 60000,
        den: 1001,
    };
    /// Exactly 60 (30 Hz and 60 Hz video).
    pub const INTEGER: Self = Self { num: 60, den: 1 };

    /// Field periods per second, as a rational.
    pub fn as_rational(self) -> (u32, u32) {
        (self.num, self.den)
    }

    /// The field rate of an MPEG-2 `frame_rate_code` (H.262 Table 6-4) whose pictures carry 10
    /// caption constructs per field period: codes 4 and 7 (29.97 and 59.94 Hz) and 5 and 8 (30
    /// and 60 Hz). Others (23.976, 24, 25, 50 Hz) are not output formats of the encoder.
    pub fn of_frame_rate_code(code: u8) -> Option<Self> {
        match code {
            4 | 7 => Some(Self::NTSC),
            5 | 8 => Some(Self::INTEGER),
            _ => None,
        }
    }
}

/// Emphasis of a span of text. Bold has no counterpart in CEA-608 or CEA-708 (CTA-708-E
/// §8.10.5.9 has italics and underline; CTA-608-E Tables 51 and 53 the same) and is dropped by
/// the parsers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    /// Italics.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
}

/// A run of text with one style. The text has no line breaks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// The characters, as Unicode.
    pub text: String,
    /// Their style.
    pub style: SpanStyle,
}

/// One line of a cue as the author broke it (longer lines are wrapped by the layout).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// The spans, in order. Adjacent spans have different styles.
    pub spans: Vec<Span>,
}

impl Line {
    /// The text without styles.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// Appends text with a style, merging with the last span when the style is the same.
    pub fn push(&mut self, text: &str, style: SpanStyle) {
        if text.is_empty() {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => self.spans.push(Span {
                text: text.to_owned(),
                style,
            }),
        }
    }

    /// A line of unstyled text.
    pub fn plain(text: &str) -> Self {
        let mut l = Self::default();
        l.push(text, SpanStyle::default());
        l
    }

    /// No visible character (empty, or only white space).
    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }
}

/// Where a cue would like to be on the screen. Hints only: the layout honors them within the
/// limits of CEA-608 (rows 1 to 15, 32 columns).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Vertical {
    /// The bottom of the caption area (the usual place).
    #[default]
    Bottom,
    /// The top of the caption area (SubRip `{\an8}`, WebVTT `line:0` and small percentages).
    Top,
}

/// Horizontal alignment of a cue's lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    /// Each line centered (the usual pop-on layout).
    #[default]
    Center,
    /// Lines start at the left edge of the caption area.
    Left,
    /// Lines end at the right edge.
    Right,
}

/// Placement hints of a cue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CueStyle {
    /// Top or bottom.
    pub vertical: Vertical,
    /// Alignment of the lines.
    pub align: Align,
}

/// A cue: lines shown from `start` (inclusive) to `end` (exclusive).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cue {
    /// When the text appears.
    pub start: Millis,
    /// When it goes away.
    pub end: Millis,
    /// The lines, top to bottom.
    pub lines: Vec<Line>,
    /// Placement hints.
    pub style: CueStyle,
}

/// Why a timed-text packet could not become a cue.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    /// The packet has no presentation time.
    #[error("a timed-text packet without a presentation time")]
    NoPts,
    /// The packet has no duration (a cue's end is its start plus the packet's duration).
    #[error("a timed-text packet without a duration")]
    NoDuration,
    /// The payload is not UTF-8.
    #[error("a timed-text packet that is not UTF-8")]
    NotUtf8,
}

impl Cue {
    /// A cue with lines of plain text.
    pub fn plain(start: Millis, end: Millis, lines: &[&str]) -> Self {
        Self {
            start,
            end,
            lines: lines.iter().map(|l| Line::plain(l)).collect(),
            style: CueStyle::default(),
        }
    }

    /// The cue moved by `delta` (from the source's time line to the channel's).
    pub fn shifted(mut self, delta: Millis) -> Self {
        self.start.0 = self.start.0.saturating_add(delta.0);
        self.end.0 = self.end.0.saturating_add(delta.0);
        self
    }

    /// The cue of a [`tb_media::CodecId::Text`] packet ("timed text in UTF-8, one cue per
    /// packet; the packet's duration is the cue's"): `time_base` is the stream's. The payload
    /// may carry the inline tags of SubRip and WebVTT (`<i>`, `<u>`, `<b>`, entities), which
    /// are interpreted as in [`crate::srt`]; line breaks separate lines.
    pub fn from_text_packet(p: &Packet, time_base: Rational) -> Result<Self, PacketError> {
        let pts = p.pts.ok_or(PacketError::NoPts)?;
        let dur = p.duration.ok_or(PacketError::NoDuration)?;
        let text = std::str::from_utf8(&p.data).map_err(|_| PacketError::NotUtf8)?;
        let start = time_base.rescale(pts, Rational::MILLIS);
        let end = time_base.rescale(pts.saturating_add(dur), Rational::MILLIS);
        let (lines, style) = crate::markup::parse_payload(text, crate::markup::Dialect::Srt);
        Ok(Self {
            start: Millis(start),
            end: Millis(end),
            lines,
            style,
        })
    }

    /// The text without styles, lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb_media::{CodecId, StreamIndex};

    #[test]
    fn field_periods_are_exact() {
        let r = FieldRate::NTSC;
        assert_eq!(Millis(0).to_field_periods(r), 0);
        // 1001 ms is exactly 60 field periods
        assert_eq!(Millis(1001).to_field_periods(r), 60);
        assert_eq!(Millis(1001 * 3600).to_field_periods(r), 60 * 3600);
        // 8.34 ms is half a field period: ties go up
        assert_eq!(Millis(8).to_field_periods(r), 0);
        assert_eq!(Millis(9).to_field_periods(r), 1);
        assert_eq!(Millis(-9).to_field_periods(r), -1);
        assert_eq!(Millis::of_field_period(60, r), Millis(1001));
        assert_eq!(Millis::of_field_period(1, r), Millis(17));
        for ms in [0i64, 1, 17, 500, 33_367, 3_600_000] {
            let p = Millis(ms).to_field_periods(r);
            let back = Millis::of_field_period(p, r).0;
            assert!((back - ms).abs() <= 9, "{ms} -> {p} -> {back}");
        }
        assert_eq!(Millis(1000).to_field_periods(FieldRate::INTEGER), 60);
        assert_eq!(FieldRate::of_frame_rate_code(4), Some(FieldRate::NTSC));
        assert_eq!(FieldRate::of_frame_rate_code(1), None);
    }

    #[test]
    fn display() {
        assert_eq!(Millis(3_723_004).to_string(), "01:02:03.004");
        assert_eq!(Millis(-1500).to_string(), "-00:00:01.500");
    }

    #[test]
    fn text_packets() {
        let p = Packet {
            stream: StreamIndex(2),
            pts: Some(90_000),
            dts: None,
            duration: Some(45_000),
            keyframe: true,
            data: b"<i>Hello</i>\nworld &amp; you".to_vec(),
        };
        let c = Cue::from_text_packet(&p, Rational::MPEG_90K).unwrap();
        assert_eq!((c.start, c.end), (Millis(1000), Millis(1500)));
        assert_eq!(c.text(), "Hello\nworld & you");
        assert!(c.lines[0].spans[0].style.italic);
        let mut q = p.clone();
        q.pts = None;
        assert_eq!(
            Cue::from_text_packet(&q, Rational::MPEG_90K),
            Err(PacketError::NoPts)
        );
        let mut q = p.clone();
        q.duration = None;
        assert_eq!(
            Cue::from_text_packet(&q, Rational::MPEG_90K),
            Err(PacketError::NoDuration)
        );
        let mut q = p;
        q.data = vec![0xFF, 0xFE];
        assert_eq!(
            Cue::from_text_packet(&q, Rational::MPEG_90K),
            Err(PacketError::NotUtf8)
        );
        let _ = CodecId::Text;
    }
}
