//! HLS playlists (RFC 8216bis-16), the subset an input needs.
//!
//! A multivariant playlist gives the variants (`EXT-X-STREAM-INF` and the URI line after it)
//! and the renditions (`EXT-X-MEDIA`: audio with its `LANGUAGE`, `NAME`, `DEFAULT`). A media
//! playlist gives the segments from `EXT-X-MEDIA-SEQUENCE` on (`EXTINF` then the URI line), each
//! with the parts (`EXT-X-PART`) listed before it, the parts of the segment still being made
//! (after the last URI), the `EXT-X-MAP` in force, `EXT-X-DISCONTINUITY`, `EXT-X-PRELOAD-HINT`
//! (`TYPE=PART`), `EXT-X-SERVER-CONTROL` (`CAN-BLOCK-RELOAD`, `PART-HOLD-BACK`, `HOLD-BACK`),
//! `EXT-X-PART-INF` (`PART-TARGET`) and `EXT-X-ENDLIST`. URIs are resolved against the
//! playlist's own URL. Byte ranges (`BYTERANGE` on parts and hints, `EXT-X-BYTERANGE` on
//! segments) are kept; an open-ended hint (`BYTERANGE-START` without a length) too.
//!
//! `EXT-X-PROGRAM-DATE-TIME` gives a segment its wall-clock time; segments after it without
//! one of their own continue from it by their durations (§4.4.4.6). `EXT-X-DATERANGE` spans
//! are kept with their client attributes (`X-…`); tags with the same `ID` merge (§4.4.5.1).
//!
//! Unknown tags are ignored, as the RFC requires of clients.

use url::Url;

/// Why a playlist could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaylistError {
    /// The first line is not `#EXTM3U`.
    #[error("not a playlist (no #EXTM3U)")]
    NotM3u,
    /// A URI did not resolve against the playlist URL.
    #[error("bad URI {0:?}")]
    Uri(String),
    /// A tag's value did not parse.
    #[error("{tag}: {why}")]
    Tag {
        /// The tag.
        tag: &'static str,
        /// What was wrong.
        why: String,
    },
}

/// A playlist of either kind.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "made once per reload and matched at once"
)]
pub enum Playlist {
    /// Variants and renditions.
    Multivariant(Multivariant),
    /// Segments.
    Media(MediaPlaylist),
}

/// A multivariant playlist.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Multivariant {
    /// `EXT-X-STREAM-INF` entries.
    pub variants: Vec<Variant>,
    /// `EXT-X-MEDIA` entries.
    pub renditions: Vec<Rendition>,
}

/// One variant stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    /// `BANDWIDTH`, bit/s.
    pub bandwidth: u64,
    /// `CODECS`.
    pub codecs: Option<String>,
    /// `RESOLUTION`.
    pub resolution: Option<(u32, u32)>,
    /// `AUDIO`: the group of audio renditions it plays with.
    pub audio: Option<String>,
    /// Its media playlist.
    pub uri: Url,
}

/// The kind of a rendition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenditionKind {
    /// `AUDIO`.
    Audio,
    /// `VIDEO`.
    Video,
    /// `SUBTITLES`.
    Subtitles,
    /// `CLOSED-CAPTIONS`.
    ClosedCaptions,
}

/// One `EXT-X-MEDIA` rendition.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    /// `TYPE`.
    pub kind: RenditionKind,
    /// `GROUP-ID`.
    pub group: String,
    /// `LANGUAGE` (RFC 5646; often an ISO 639 code).
    pub language: Option<String>,
    /// `NAME`.
    pub name: String,
    /// `DEFAULT=YES`.
    pub default: bool,
    /// Its media playlist; none when the rendition is in the variant's own stream.
    pub uri: Option<Url>,
}

/// A byte range: `len` bytes from `offset` (`None`: right after the previous range of the
/// same resource, RFC 8216bis §4.4.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Length.
    pub len: u64,
    /// Start.
    pub offset: Option<u64>,
}

/// One `EXT-X-PART`.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    /// `URI`.
    pub uri: Url,
    /// `DURATION`, seconds.
    pub duration: f64,
    /// `INDEPENDENT=YES`: it starts with a key frame.
    pub independent: bool,
    /// `BYTERANGE`.
    pub range: Option<ByteRange>,
    /// `GAP=YES`.
    pub gap: bool,
}

/// One media segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Its media sequence number.
    pub msn: u64,
    /// The URI line.
    pub uri: Url,
    /// `EXTINF` duration, seconds.
    pub duration: f64,
    /// `EXT-X-BYTERANGE`.
    pub range: Option<ByteRange>,
    /// `EXT-X-DISCONTINUITY` before it.
    pub discontinuity: bool,
    /// The `EXT-X-MAP` in force (fragmented MP4); none for MPEG-TS.
    pub map: Option<Url>,
    /// Its parts, in order (LL-HLS; empty when not listed).
    pub parts: Vec<Part>,
    /// Its wall-clock start, seconds since the Unix epoch (`EXT-X-PROGRAM-DATE-TIME`, or
    /// carried on from the last one).
    pub pdt: Option<f64>,
}

/// `EXT-X-DATERANGE`.
#[derive(Debug, Clone, PartialEq)]
pub struct DateRange {
    /// `ID`.
    pub id: String,
    /// `CLASS`.
    pub class: Option<String>,
    /// `START-DATE`, seconds since the Unix epoch.
    pub start: f64,
    /// Its length: `DURATION`, else `END-DATE` − `START-DATE`, else `PLANNED-DURATION`.
    pub duration: Option<f64>,
    /// The client attributes (`X-…`), names as written.
    pub attributes: Vec<(String, String)>,
}

impl DateRange {
    /// Client attribute `name` (`X-TITLE`, …).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// `EXT-X-PRELOAD-HINT:TYPE=PART`: the next part, which a request may block on.
#[derive(Debug, Clone, PartialEq)]
pub struct PreloadHint {
    /// `URI`.
    pub uri: Url,
    /// `BYTERANGE-START`.
    pub start: Option<u64>,
    /// `BYTERANGE-LENGTH`.
    pub len: Option<u64>,
}

/// A media playlist.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaPlaylist {
    /// `EXT-X-TARGETDURATION`, seconds.
    pub target_duration: f64,
    /// `EXT-X-MEDIA-SEQUENCE`.
    pub media_sequence: u64,
    /// Complete segments, oldest first.
    pub segments: Vec<Segment>,
    /// Parts of the segment being made (its MSN is [`MediaPlaylist::next_msn`]).
    pub open_parts: Vec<Part>,
    /// The map in force at the end (for the open segment).
    pub map: Option<Url>,
    /// `EXT-X-PRELOAD-HINT` of type `PART`.
    pub preload_hint: Option<PreloadHint>,
    /// `CAN-BLOCK-RELOAD=YES`.
    pub can_block_reload: bool,
    /// `PART-HOLD-BACK`, seconds.
    pub part_hold_back: Option<f64>,
    /// `HOLD-BACK`, seconds.
    pub hold_back: Option<f64>,
    /// `PART-TARGET`, seconds.
    pub part_target: Option<f64>,
    /// `EXT-X-ENDLIST`.
    pub ended: bool,
    /// `EXT-X-DATERANGE` spans, by `ID`, in order of first appearance.
    pub dateranges: Vec<DateRange>,
}

impl MediaPlaylist {
    /// The MSN of the segment after the last complete one.
    pub fn next_msn(&self) -> u64 {
        self.media_sequence + self.segments.len() as u64
    }

    /// Whether the playlist has parts (Low-Latency HLS).
    pub fn low_latency(&self) -> bool {
        self.part_target.is_some()
            || !self.open_parts.is_empty()
            || self.segments.iter().any(|s| !s.parts.is_empty())
    }

    /// The parts of segment `msn`, and whether that segment is complete; `None` when it is
    /// not listed (gone, or not yet made).
    pub fn parts_of(&self, msn: u64) -> Option<(&[Part], bool)> {
        if msn == self.next_msn() {
            return Some((&self.open_parts, false));
        }
        let i = usize::try_from(msn.checked_sub(self.media_sequence)?).ok()?;
        self.segments.get(i).map(|s| (s.parts.as_slice(), true))
    }

    /// Whether the segments carry wall-clock times.
    pub fn dated(&self) -> bool {
        self.segments.first().is_some_and(|s| s.pdt.is_some())
    }

    /// The segment airing at `t` (seconds since the Unix epoch), if any.
    pub fn segment_at(&self, t: f64) -> Option<&Segment> {
        self.segments
            .iter()
            .find(|s| s.pdt.is_some_and(|p| p <= t && t < p + s.duration))
    }

    /// Segment `msn`, if listed.
    pub fn segment(&self, msn: u64) -> Option<&Segment> {
        let i = usize::try_from(msn.checked_sub(self.media_sequence)?).ok()?;
        self.segments.get(i)
    }
}

/// Splits an attribute list (`A=1,B="x,y",C=Z`) into pairs; quoted values lose their quotes.
fn attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_owned();
        rest = &rest[eq + 1..];
        let value;
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            value = r[..end].to_owned();
            rest = r.get(end + 1..).unwrap_or("");
            rest = rest.trim_start().strip_prefix(',').unwrap_or(rest);
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_owned();
            rest = rest.get(end + 1..).unwrap_or("");
        }
        out.push((key, value));
        rest = rest.trim_start();
    }
    out
}

fn attr<'a>(a: &'a [(String, String)], k: &str) -> Option<&'a str> {
    a.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

fn num(tag: &'static str, v: &str) -> Result<f64, PlaylistError> {
    v.trim()
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite() && *x >= 0.0)
        .ok_or_else(|| PlaylistError::Tag {
            tag,
            why: format!("{v:?} is not a number"),
        })
}

fn int(tag: &'static str, v: &str) -> Result<u64, PlaylistError> {
    v.trim().parse().map_err(|_| PlaylistError::Tag {
        tag,
        why: format!("{v:?} is not an integer"),
    })
}

/// `<n>[@<o>]`.
fn byte_range(tag: &'static str, v: &str) -> Result<ByteRange, PlaylistError> {
    let (n, o) = match v.split_once('@') {
        Some((n, o)) => (n, Some(int(tag, o)?)),
        None => (v, None),
    };
    Ok(ByteRange {
        len: int(tag, n)?,
        offset: o,
    })
}

/// An RFC 3339 / ISO 8601 date-time (`2026-11-28T02:13:36.106Z`, or with `±hh:mm`) as seconds
/// since the Unix epoch.
pub fn parse_date(s: &str) -> Option<f64> {
    let s = s.trim();
    let num = |a: usize, n: usize| -> Option<i64> { s.get(a..a + n)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, se) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let b = s.as_bytes();
    if b.get(4) != Some(&b'-') || !matches!(b.get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let mut i = 19;
    let mut frac = 0.0;
    if b.get(i) == Some(&b'.') {
        let start = i;
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        frac = s.get(start..i)?.parse::<f64>().ok()?;
    }
    let off = match b.get(i) {
        Some(b'Z' | b'z') => 0,
        Some(c @ (b'+' | b'-')) => {
            let sign = if *c == b'-' { -1 } else { 1 };
            let oh = num(i + 1, 2)?;
            let om = if b.get(i + 3) == Some(&b':') {
                num(i + 4, 2)?
            } else {
                num(i + 3, 2)?
            };
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + se - off) as f64 + frac)
}

fn resolve(base: &Url, uri: &str) -> Result<Url, PlaylistError> {
    base.join(uri.trim())
        .map_err(|_| PlaylistError::Uri(uri.to_owned()))
}

/// Parses `text`, fetched from `base`.
pub fn parse(text: &str, base: &Url) -> Result<Playlist, PlaylistError> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next().map(|l| l.trim_start_matches('\u{feff}')) != Some("#EXTM3U") {
        return Err(PlaylistError::NotM3u);
    }
    let lines: Vec<&str> = lines.collect();
    if lines.iter().any(|l| l.starts_with("#EXT-X-STREAM-INF")) {
        parse_multivariant(&lines, base).map(Playlist::Multivariant)
    } else {
        parse_media(&lines, base).map(Playlist::Media)
    }
}

fn parse_multivariant(lines: &[&str], base: &Url) -> Result<Multivariant, PlaylistError> {
    let mut m = Multivariant::default();
    let mut pending: Option<Vec<(String, String)>> = None;
    for l in lines {
        if let Some(v) = l.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(attributes(v));
        } else if let Some(v) = l.strip_prefix("#EXT-X-MEDIA:") {
            let a = attributes(v);
            let kind = match attr(&a, "TYPE") {
                Some("AUDIO") => RenditionKind::Audio,
                Some("VIDEO") => RenditionKind::Video,
                Some("SUBTITLES") => RenditionKind::Subtitles,
                Some("CLOSED-CAPTIONS") => RenditionKind::ClosedCaptions,
                _ => continue,
            };
            m.renditions.push(Rendition {
                kind,
                group: attr(&a, "GROUP-ID").unwrap_or("").to_owned(),
                language: attr(&a, "LANGUAGE").map(str::to_owned),
                name: attr(&a, "NAME").unwrap_or("").to_owned(),
                default: attr(&a, "DEFAULT") == Some("YES"),
                uri: attr(&a, "URI").map(|u| resolve(base, u)).transpose()?,
            });
        } else if !l.starts_with('#')
            && let Some(a) = pending.take()
        {
            let resolution = attr(&a, "RESOLUTION").and_then(|r| {
                let (w, h) = r.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            });
            m.variants.push(Variant {
                bandwidth: attr(&a, "BANDWIDTH")
                    .map(|b| int("EXT-X-STREAM-INF", b))
                    .transpose()?
                    .unwrap_or(0),
                codecs: attr(&a, "CODECS").map(str::to_owned),
                resolution,
                audio: attr(&a, "AUDIO").map(str::to_owned),
                uri: resolve(base, l)?,
            });
        }
    }
    Ok(m)
}

fn parse_media(lines: &[&str], base: &Url) -> Result<MediaPlaylist, PlaylistError> {
    let mut p = MediaPlaylist::default();
    let mut parts: Vec<Part> = Vec::new();
    let mut duration: Option<f64> = None;
    let mut range: Option<ByteRange> = None;
    let mut discontinuity = false;
    // the wall-clock time of the next segment
    let mut pdt: Option<f64> = None;
    for l in lines {
        if let Some(v) = l.strip_prefix("#EXT-X-TARGETDURATION:") {
            p.target_duration = num("EXT-X-TARGETDURATION", v)?;
        } else if let Some(v) = l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            p.media_sequence = int("EXT-X-MEDIA-SEQUENCE", v)?;
        } else if let Some(v) = l.strip_prefix("#EXT-X-MAP:") {
            let a = attributes(v);
            p.map = attr(&a, "URI").map(|u| resolve(base, u)).transpose()?;
        } else if let Some(v) = l.strip_prefix("#EXTINF:") {
            let d = v.split(',').next().unwrap_or("");
            duration = Some(num("EXTINF", d)?);
        } else if let Some(v) = l.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(byte_range("EXT-X-BYTERANGE", v)?);
        } else if *l == "#EXT-X-DISCONTINUITY" {
            discontinuity = true;
        } else if let Some(v) = l.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            pdt = Some(parse_date(v).ok_or_else(|| PlaylistError::Tag {
                tag: "EXT-X-PROGRAM-DATE-TIME",
                why: format!("{v:?} is not a date-time"),
            })?);
        } else if let Some(v) = l.strip_prefix("#EXT-X-DATERANGE:") {
            let a = attributes(v);
            let date = |k: &str| attr(&a, k).and_then(parse_date);
            let (Some(id), Some(start)) = (attr(&a, "ID"), date("START-DATE")) else {
                continue;
            };
            let secs = |k: &str| attr(&a, k).and_then(|x| x.parse::<f64>().ok());
            let duration = secs("DURATION")
                .or_else(|| date("END-DATE").map(|e| e - start))
                .or_else(|| secs("PLANNED-DURATION"));
            let x: Vec<(String, String)> = a
                .iter()
                .filter(|(k, _)| k.starts_with("X-"))
                .cloned()
                .collect();
            match p.dateranges.iter_mut().find(|d| d.id == id) {
                Some(d) => {
                    d.duration = duration.or(d.duration);
                    for (k, v) in x {
                        match d.attributes.iter_mut().find(|(n, _)| *n == k) {
                            Some(slot) => slot.1 = v,
                            None => d.attributes.push((k, v)),
                        }
                    }
                }
                None => p.dateranges.push(DateRange {
                    id: id.to_owned(),
                    class: attr(&a, "CLASS").map(str::to_owned),
                    start,
                    duration,
                    attributes: x,
                }),
            }
        } else if let Some(v) = l.strip_prefix("#EXT-X-PART:") {
            let a = attributes(v);
            let Some(uri) = attr(&a, "URI") else {
                return Err(PlaylistError::Tag {
                    tag: "EXT-X-PART",
                    why: "no URI".into(),
                });
            };
            parts.push(Part {
                uri: resolve(base, uri)?,
                duration: num("EXT-X-PART", attr(&a, "DURATION").unwrap_or("0"))?,
                independent: attr(&a, "INDEPENDENT") == Some("YES"),
                range: attr(&a, "BYTERANGE")
                    .map(|r| byte_range("EXT-X-PART", r))
                    .transpose()?,
                gap: attr(&a, "GAP") == Some("YES"),
            });
        } else if let Some(v) = l.strip_prefix("#EXT-X-PRELOAD-HINT:") {
            let a = attributes(v);
            if attr(&a, "TYPE") == Some("PART")
                && let Some(uri) = attr(&a, "URI")
            {
                p.preload_hint = Some(PreloadHint {
                    uri: resolve(base, uri)?,
                    start: attr(&a, "BYTERANGE-START").and_then(|v| v.parse().ok()),
                    len: attr(&a, "BYTERANGE-LENGTH").and_then(|v| v.parse().ok()),
                });
            }
        } else if let Some(v) = l.strip_prefix("#EXT-X-SERVER-CONTROL:") {
            let a = attributes(v);
            p.can_block_reload = attr(&a, "CAN-BLOCK-RELOAD") == Some("YES");
            p.part_hold_back = attr(&a, "PART-HOLD-BACK").and_then(|v| v.parse().ok());
            p.hold_back = attr(&a, "HOLD-BACK").and_then(|v| v.parse().ok());
        } else if let Some(v) = l.strip_prefix("#EXT-X-PART-INF:") {
            let a = attributes(v);
            p.part_target = attr(&a, "PART-TARGET").and_then(|v| v.parse().ok());
        } else if *l == "#EXT-X-ENDLIST" {
            p.ended = true;
        } else if !l.starts_with('#') {
            let msn = p.media_sequence + p.segments.len() as u64;
            let d = duration.unwrap_or(0.0);
            let at = pdt;
            pdt = pdt.map(|t| t + d);
            p.segments.push(Segment {
                msn,
                uri: resolve(base, l)?,
                duration: d,
                range: range.take(),
                discontinuity: std::mem::take(&mut discontinuity),
                map: p.map.clone(),
                parts: std::mem::take(&mut parts),
                pdt: at,
            });
            duration = None;
        }
    }
    p.open_parts = parts;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/live/stream.m3u8").unwrap()
    }

    /// The LL-HLS example of RFC 8216bis Appendix (shortened), with a map and a byte-range
    /// hint.
    const LL: &str = "#EXTM3U
#EXT-X-TARGETDURATION:4
#EXT-X-VERSION:6
#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=1.0,CAN-SKIP-UNTIL=12.0
#EXT-X-PART-INF:PART-TARGET=0.33334
#EXT-X-MEDIA-SEQUENCE:266
#EXT-X-MAP:URI=\"init.mp4\"
#EXT-X-PROGRAM-DATE-TIME:2019-02-14T02:13:36.106Z
#EXTINF:4.00008,
fileSequence266.mp4
#EXT-X-PART:DURATION=0.33334,URI=\"filePart267.0.mp4\",INDEPENDENT=YES
#EXT-X-PART:DURATION=0.33334,URI=\"filePart267.1.mp4\"
#EXT-X-PART:DURATION=0.33334,URI=\"filePart267.2.mp4\"
#EXTINF:1.00002,
fileSequence267.mp4
#EXT-X-PART:DURATION=0.33334,URI=\"filePart268.0.mp4\",INDEPENDENT=YES
#EXT-X-PART:DURATION=0.33334,URI=\"seg268.mp4\",BYTERANGE=\"1200@3400\"
#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"seg268.mp4\",BYTERANGE-START=4600
";

    #[test]
    fn low_latency_media_playlist() {
        let Playlist::Media(p) = parse(LL, &base()).unwrap() else {
            panic!()
        };
        assert_eq!(p.target_duration, 4.0);
        assert_eq!(p.media_sequence, 266);
        assert!(p.can_block_reload && p.low_latency());
        assert_eq!(p.part_hold_back, Some(1.0));
        assert_eq!(p.part_target, Some(0.33334));
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.next_msn(), 268);
        let s267 = p.segment(267).unwrap();
        assert_eq!(s267.parts.len(), 3);
        assert!(s267.parts[0].independent && !s267.parts[1].independent);
        assert_eq!(
            s267.map.as_ref().unwrap().as_str(),
            "https://example.com/live/init.mp4"
        );
        let (open, complete) = p.parts_of(268).unwrap();
        assert!(!complete);
        assert_eq!(open.len(), 2);
        assert_eq!(
            open[1].range,
            Some(ByteRange {
                len: 1200,
                offset: Some(3400)
            })
        );
        let h = p.preload_hint.as_ref().unwrap();
        assert_eq!(h.uri.as_str(), "https://example.com/live/seg268.mp4");
        assert_eq!((h.start, h.len), (Some(4600), None));
        assert!(p.parts_of(265).is_none() && p.parts_of(269).is_none());
    }

    #[test]
    fn multivariant_with_audio_renditions() {
        let text = "#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",LANGUAGE=\"en\",NAME=\"English\",DEFAULT=YES,URI=\"a/en.m3u8\"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",LANGUAGE=\"es\",NAME=\"Espa\u{f1}ol, Latino\",URI=\"a/es.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=2500000,CODECS=\"avc1.64001f,mp4a.40.2\",RESOLUTION=1280x720,AUDIO=\"aud\"
v720.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080,AUDIO=\"aud\"
https://cdn.example.net/v1080.m3u8
";
        let Playlist::Multivariant(m) = parse(text, &base()).unwrap() else {
            panic!()
        };
        assert_eq!(m.variants.len(), 2);
        assert_eq!(m.variants[0].resolution, Some((1280, 720)));
        assert_eq!(
            m.variants[0].codecs.as_deref(),
            Some("avc1.64001f,mp4a.40.2")
        );
        assert_eq!(
            m.variants[1].uri.as_str(),
            "https://cdn.example.net/v1080.m3u8"
        );
        assert_eq!(m.renditions.len(), 2);
        assert_eq!(m.renditions[1].name, "Espa\u{f1}ol, Latino");
        assert_eq!(m.renditions[1].language.as_deref(), Some("es"));
        assert!(m.renditions[0].default && !m.renditions[1].default);
        assert_eq!(
            m.renditions[0].uri.as_ref().unwrap().as_str(),
            "https://example.com/live/a/en.m3u8"
        );
    }

    /// A generated VOD channel: dated segments (one tag, the rest carried on, a new one after
    /// a discontinuity) and date ranges naming the programs.
    #[test]
    fn dated_vod_with_dateranges() {
        let text = "#EXTM3U
#EXT-X-TARGETDURATION:6
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-PROGRAM-DATE-TIME:2026-11-28T03:00:00Z
#EXT-X-DATERANGE:ID=\"ep1\",CLASS=\"com.example.program\",START-DATE=\"2026-11-28T03:00:00Z\",DURATION=12,X-TITLE=\"Cowboy Bebop\",X-DESCRIPTION=\"Asteroid Blues\",X-RATING=\"TV-14\"
#EXTINF:6,
a0.ts
#EXTINF:6,
a1.ts
#EXT-X-DISCONTINUITY
#EXT-X-PROGRAM-DATE-TIME:2026-11-28T03:00:12.500+00:00
#EXT-X-DATERANGE:ID=\"ep2\",START-DATE=\"2026-11-28T03:00:12.5Z\",END-DATE=\"2026-11-28T03:00:18.5Z\",X-TITLE=\"Outlaw Star\"
#EXTINF:6,
b0.ts
#EXT-X-DATERANGE:ID=\"ep2\",START-DATE=\"2026-11-28T03:00:12.5Z\",X-DESCRIPTION=\"Outlaw World\"
#EXT-X-ENDLIST
";
        let Playlist::Media(p) = parse(text, &base()).unwrap() else {
            panic!()
        };
        let t0 = parse_date("2026-11-28T03:00:00Z").unwrap();
        assert_eq!(t0, 1_795_834_800.0);
        assert!(p.dated() && p.ended);
        let pdts: Vec<f64> = p.segments.iter().map(|s| s.pdt.unwrap() - t0).collect();
        assert_eq!(pdts, [0.0, 6.0, 12.5]);
        assert_eq!(p.segment_at(t0 + 7.0).unwrap().msn, 1);
        assert_eq!(
            p.segment_at(t0 + 12.4),
            None,
            "the half second between them is a gap"
        );
        assert_eq!(p.dateranges.len(), 2);
        let ep1 = &p.dateranges[0];
        assert_eq!((ep1.start - t0, ep1.duration), (0.0, Some(12.0)));
        assert_eq!(ep1.attr("X-TITLE"), Some("Cowboy Bebop"));
        assert_eq!(ep1.attr("X-RATING"), Some("TV-14"));
        let ep2 = &p.dateranges[1];
        assert_eq!(ep2.duration, Some(6.0));
        assert_eq!(
            ep2.attr("X-DESCRIPTION"),
            Some("Outlaw World"),
            "merged by ID"
        );
        assert_eq!(ep2.attr("X-TITLE"), Some("Outlaw Star"));
    }

    #[test]
    fn classic_ts_playlist_and_errors() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:6.0,\na.ts\n#EXT-X-DISCONTINUITY\n#EXTINF:5.5,\nb.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(p) = parse(text, &base()).unwrap() else {
            panic!()
        };
        assert!(!p.low_latency() && p.ended);
        assert_eq!(p.segments[1].msn, 8);
        assert!(p.segments[1].discontinuity && !p.segments[0].discontinuity);
        assert!(p.segments[0].map.is_none());
        assert_eq!(parse("hello", &base()), Err(PlaylistError::NotM3u));
        assert!(parse("#EXTM3U\n#EXTINF:abc,\nx.ts\n", &base()).is_err());
    }
}
