//! `streaminfo.cgi` ("Stream Information"): what the box detects in each MPTS input.
//!
//! The saved page has no streams (no inputs configured), so the row layout comes from the
//! format strings in `streaminfo.cgi`: a stream line `Stream %d - UDP://a.b.c.d:port`, per
//! program a cell `%d PMT %d` (PMT PID, then program number; manual p.20 shows "48 PMT 1"), per
//! elementary stream a cell `%d <type>` with type one of the labels below. The parser reads
//! those cells in document order and ignores layout cells, so it does not depend on the exact
//! nesting, which has not been seen live.

use crate::html::Document;
use crate::pages::{ParseError, expect_page};
use crate::types::{StreamUrl, ValueError};

const PAGE: &str = "streaminfo.cgi";

/// The stream types the page names.
pub const STREAM_TYPE_LABELS: &[&str] = &[
    "Reserved Video",
    "MPEG-1 Video",
    "MPEG-2 Video",
    "MPEG-4 Video",
    "AVC/H.264 Video",
    "MPEG-1 Audio",
    "MPEG-2 Audio",
    "AC3 Audio",
    "Unknown",
];

/// One elementary stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedEs {
    /// PID.
    pub pid: u16,
    /// The label as shown (one of [`STREAM_TYPE_LABELS`]).
    pub kind: String,
}

/// One program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedProgram {
    /// PMT PID.
    pub pmt_pid: u16,
    /// Program number.
    pub program_number: u16,
    /// Elementary streams.
    pub streams: Vec<DetectedEs>,
}

/// One input stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedStream {
    /// The stream index shown (MPTS slot).
    pub index: u8,
    /// URL, when in the device's format.
    pub url: Option<StreamUrl>,
    /// Programs.
    pub programs: Vec<DetectedProgram>,
}

/// The page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfoPage {
    /// Streams in page order.
    pub streams: Vec<DetectedStream>,
}

fn pid(s: &str) -> Result<u16, ParseError> {
    let v: u16 = s
        .parse()
        .map_err(|_| ParseError::structure(PAGE, format!("PID {s:?}")))?;
    if v > 0x1FFF {
        return Err(ParseError::Value {
            page: PAGE,
            source: ValueError::OutOfRange {
                what: "PID",
                value: v.into(),
                min: 0,
                max: 0x1FFF,
            },
        });
    }
    Ok(v)
}

impl StreamInfoPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "Stream Information")?;
        if !doc.text().contains("Streams Detected") {
            return Err(ParseError::structure(
                PAGE,
                "\"Streams Detected\" table missing",
            ));
        }
        let mut streams: Vec<DetectedStream> = Vec::new();
        for td in doc.find_all("td") {
            // Leaf cells only: a cell holding a nested table repeats its descendants' text.
            if !td.find_all("td").is_empty() {
                continue;
            }
            let t = td.text();
            if let Some(rest) = t.strip_prefix("Stream ") {
                let (n, url) = rest.split_once(" - ").unwrap_or((rest, ""));
                let Ok(index) = n.trim().parse::<u8>() else {
                    continue;
                };
                streams.push(DetectedStream {
                    index,
                    url: url.trim().parse().ok(),
                    programs: vec![],
                });
                continue;
            }
            let words: Vec<&str> = t.split_whitespace().collect();
            if let [p, "PMT", num] = words.as_slice() {
                let program_number: u16 = num
                    .parse()
                    .map_err(|_| ParseError::structure(PAGE, format!("program {num:?}")))?;
                let stream = streams
                    .last_mut()
                    .ok_or_else(|| ParseError::structure(PAGE, "program before any stream"))?;
                stream.programs.push(DetectedProgram {
                    pmt_pid: pid(p)?,
                    program_number,
                    streams: vec![],
                });
                continue;
            }
            if let Some((p, label)) = t.split_once(' ')
                && STREAM_TYPE_LABELS.contains(&label.trim())
                && p.bytes().all(|b| b.is_ascii_digit())
            {
                let program = streams
                    .last_mut()
                    .and_then(|s| s.programs.last_mut())
                    .ok_or_else(|| {
                        ParseError::structure(PAGE, "elementary stream before any program")
                    })?;
                program.streams.push(DetectedEs {
                    pid: pid(p)?,
                    kind: label.trim().to_owned(),
                });
            }
        }
        Ok(StreamInfoPage { streams })
    }
}

/// `programs` in a fixed order (by program number, streams by PID), so two listings compare
/// equal whatever order the page or the configuration gives them in.
pub fn normalized(programs: &[DetectedProgram]) -> Vec<DetectedProgram> {
    let mut v = programs.to_vec();
    for p in &mut v {
        p.streams
            .sort_by(|a, b| (a.pid, &a.kind).cmp(&(b.pid, &b.kind)));
    }
    v.sort_by_key(|p| (p.program_number, p.pmt_pid));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    #[test]
    fn golden_saved_page_is_empty() {
        assert_eq!(
            StreamInfoPage::parse(fixtures::STREAM_INFO)
                .unwrap()
                .streams,
            vec![]
        );
    }

    #[test]
    fn golden_live_page_with_one_stream() {
        // Captured 2026-11-27 with carrier 1 (two slates) on slot 1.
        let p = StreamInfoPage::parse(include_bytes!(
            "../../tests/fixtures/streaminfo.cgi_stream1.html"
        ))
        .unwrap();
        assert_eq!(p.streams.len(), 1);
        let s = &p.streams[0];
        assert_eq!(s.index, 1);
        assert_eq!(s.url, Some("UDP://239.10.0.1:5000".parse().unwrap()));
        let es = |pid, kind: &str| DetectedEs {
            pid,
            kind: kind.into(),
        };
        assert_eq!(
            s.programs,
            vec![
                DetectedProgram {
                    pmt_pid: 48,
                    program_number: 1,
                    streams: vec![es(49, "MPEG-2 Video"), es(52, "AC3 Audio")],
                },
                DetectedProgram {
                    pmt_pid: 64,
                    program_number: 2,
                    streams: vec![es(65, "MPEG-2 Video"), es(68, "AC3 Audio")],
                },
            ]
        );
        let mut reversed = s.programs.clone();
        reversed.reverse();
        reversed[0].streams.reverse();
        assert_eq!(normalized(&reversed), s.programs);
    }

    #[test]
    fn synthetic_page_with_two_streams() {
        let p = StreamInfoPage::parse(include_bytes!(
            "../../tests/fixtures/synthetic/streaminfo_streams.html"
        ))
        .unwrap();
        assert_eq!(p.streams.len(), 2);
        let s1 = &p.streams[0];
        assert_eq!(s1.index, 1);
        assert_eq!(s1.url, Some("UDP://239.10.0.1:5000".parse().unwrap()));
        assert_eq!(s1.programs.len(), 2);
        assert_eq!(s1.programs[0].pmt_pid, 48);
        assert_eq!(s1.programs[0].program_number, 1);
        assert_eq!(
            s1.programs[0].streams,
            vec![
                DetectedEs {
                    pid: 49,
                    kind: "MPEG-2 Video".into()
                },
                DetectedEs {
                    pid: 52,
                    kind: "AC3 Audio".into()
                },
            ]
        );
        assert_eq!(s1.programs[1].pmt_pid, 64);
        assert_eq!(
            p.streams[1].url,
            Some("RTP://239.10.0.2:5000".parse().unwrap())
        );
        assert_eq!(p.streams[1].programs[0].streams[0].kind, "AVC/H.264 Video");
    }
}
