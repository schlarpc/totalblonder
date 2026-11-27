//! `debug.cgi` (unlinked; read with a bare GET, device §11.3): FPGA versions, error counters
//! per input FPGA stream and clock, bitrates, timestamp errors and raw registers.
//!
//! The page is a sequence of `Label: value` cells under bold section headings. It is kept as
//! that sequence ([`DebugEntry`]) with typed accessors for the counters the supervisor uses.

use std::fmt;

use crate::html::{Document, row_cells};
use crate::pages::{ParseError, expect_page};
use crate::types::{Bitrate, TemperatureF, Uptime, parse_decimal, parse_tenths};

const PAGE: &str = "debug.cgi";

/// One `Label: value` cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugEntry {
    /// The section heading above it (empty before the first).
    pub section: String,
    /// The label.
    pub label: String,
    /// The value, verbatim.
    pub value: String,
    /// Printed in red (the page marks non-zero error counters this way).
    pub highlighted: bool,
}

/// An input FPGA: `FPGA2A` serves GIGE1, `FPGA2B` GIGE2 (device §11.3, inferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputFpga {
    /// FPGA2A.
    A,
    /// FPGA2B.
    B,
}

impl fmt::Display for InputFpga {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InputFpga::A => "FPGA2A",
            InputFpga::B => "FPGA2B",
        })
    }
}

/// The per-stream counter sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamCounter {
    /// "Pre-buffer TS Sync Byte Errors".
    PreBufferSync,
    /// "Post-buffer TS Sync Byte Errors".
    PostBufferSync,
    /// "100ms Timeout Exceeded Errors".
    Timeout100ms,
    /// "Buffer Overflow Errors".
    BufferOverflow,
}

impl StreamCounter {
    fn section(self) -> &'static str {
        match self {
            StreamCounter::PreBufferSync => "Pre-buffer TS Sync Byte Errors",
            StreamCounter::PostBufferSync => "Post-buffer TS Sync Byte Errors",
            StreamCounter::Timeout100ms => "100ms Timeout Exceeded Errors",
            StreamCounter::BufferOverflow => "Buffer Overflow Errors",
        }
    }
}

/// The page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugPage {
    /// "Current System Time", verbatim (`Wed Nov 25 01:14:14 2026`).
    pub system_time: String,
    /// "System Uptime".
    pub uptime: Uptime,
    /// Temperature from the header.
    pub temperature: Option<TemperatureF>,
    /// Every `Label: value` cell in page order.
    pub entries: Vec<DebugEntry>,
}

impl DebugPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "Debug")?;
        let mut system_time = None;
        let mut uptime = None;
        for tr in doc.find_all("tr") {
            let cells = row_cells(tr);
            if cells.len() == 2 {
                match cells[0].text().as_str() {
                    "Current System Time" => system_time = Some(cells[1].text()),
                    "System Uptime" => {
                        uptime = Some(
                            cells[1]
                                .text()
                                .parse::<Uptime>()
                                .map_err(|source| ParseError::Value { page: PAGE, source })?,
                        );
                    }
                    _ => {}
                }
            }
        }
        let mut entries = Vec::new();
        let mut section = String::new();
        let mut temperature = None;
        for td in doc.find_all("td") {
            if !td.find_all("td").is_empty() {
                continue;
            }
            let t = td.text();
            if t.is_empty() {
                continue;
            }
            if !td.find_all("b").is_empty() && !t.contains(": ") {
                section = t;
                continue;
            }
            if let Some(v) = t.strip_prefix("Temperature:") {
                let num: String = v
                    .trim()
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                    .collect();
                temperature = parse_tenths(&num).map(|tenths| TemperatureF { tenths });
            }
            let highlighted = td.find_all("font").iter().any(|f| {
                f.attr("color")
                    .is_some_and(|c| c.eq_ignore_ascii_case("#FF0000"))
            });
            let (label, value) = if let Some(v) = t.strip_prefix("Pro:Idiom Enable ") {
                (String::from("Pro:Idiom Enable"), v.trim().to_owned())
            } else if let Some((l, v)) = t.split_once(": ") {
                (l.trim().to_owned(), v.trim().to_owned())
            } else {
                continue;
            };
            entries.push(DebugEntry {
                section: section.clone(),
                label,
                value,
                highlighted,
            });
        }
        let system_time = system_time
            .ok_or_else(|| ParseError::structure(PAGE, "Current System Time missing"))?;
        let uptime = uptime.ok_or_else(|| ParseError::structure(PAGE, "System Uptime missing"))?;
        if !entries
            .iter()
            .any(|e| e.section == "Buffer Overflow Errors")
        {
            return Err(ParseError::structure(
                PAGE,
                "Buffer Overflow Errors section missing",
            ));
        }
        Ok(DebugPage {
            system_time,
            uptime,
            temperature,
            entries,
        })
    }

    /// The value of `label` in `section`.
    pub fn value(&self, section: &str, label: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.section == section && e.label == label)
            .map(|e| e.value.as_str())
    }

    /// A per-stream counter (`<fpga> Stream <n>`, n = 1..=32).
    pub fn stream_counter(
        &self,
        counter: StreamCounter,
        fpga: InputFpga,
        stream: u8,
    ) -> Option<u64> {
        self.value(counter.section(), &format!("{fpga} Stream {stream}"))?
            .parse()
            .ok()
    }

    /// TS clock errors of `<fpga> Clock <n>` (n = 1..=8).
    pub fn clock_errors(&self, fpga: InputFpga, clock: u8) -> Option<u64> {
        self.value("TS Clock Errors", &format!("{fpga} Clock {clock}"))?
            .parse()
            .ok()
    }

    /// Average and peak bit rate of `<fpga> MPTS Stream <n>`.
    pub fn mpts_bitrate(&self, fpga: InputFpga, stream: u8) -> Option<(Bitrate, Bitrate)> {
        let label = format!("{fpga} MPTS Stream {stream}");
        let bps = |s: &str| parse_decimal(s.strip_suffix("bps")?, 1).map(Bitrate);
        Some((
            bps(self.value("Average Bitrates", &label)?)?,
            bps(self.value("Peak Bitrates", &label)?)?,
        ))
    }

    /// Timestamp errors of `<fpga> MPTS Stream <n>`.
    pub fn timestamp_errors(&self, fpga: InputFpga, stream: u8) -> Option<u64> {
        self.value(
            "Timestamp Error Count",
            &format!("{fpga} MPTS Stream {stream}"),
        )?
        .parse()
        .ok()
    }

    /// The software version (`1.1.13.0`).
    pub fn software_version(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.label == "Software Version")
            .map(|e| e.value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    #[test]
    fn golden_saved_debug_page() {
        let p = DebugPage::parse(fixtures::DEBUG).unwrap();
        assert_eq!(p.system_time, "Wed Nov 25 01:14:14 2026");
        assert_eq!(p.uptime.0.as_secs(), 6 * 86400 + 18 * 3600 + 24 * 60);
        assert_eq!(p.temperature, Some(TemperatureF { tenths: 800 }));
        assert_eq!(p.software_version(), Some("1.1.13.0"));
        assert_eq!(p.value("", "FPGA2A Version"), Some("81030000"));
        assert_eq!(p.value("", "Pro:Idiom Enable"), Some("0"));
        assert_eq!(
            p.value(
                "Error Counters",
                "FPGA2A->FPGA1 Serial Link Packets Dropped"
            ),
            Some("0")
        );
        for c in [
            StreamCounter::PreBufferSync,
            StreamCounter::PostBufferSync,
            StreamCounter::Timeout100ms,
            StreamCounter::BufferOverflow,
        ] {
            for f in [InputFpga::A, InputFpga::B] {
                for n in 1..=32 {
                    assert_eq!(p.stream_counter(c, f, n), Some(0), "{c:?} {f} {n}");
                }
                assert_eq!(p.stream_counter(c, f, 33), None);
            }
        }
        for n in 1..=8 {
            assert_eq!(p.clock_errors(InputFpga::A, n), Some(0));
            assert_eq!(
                p.clock_errors(InputFpga::B, n),
                Some(if n >= 5 { 2 } else { 0 })
            );
        }
        let hl: Vec<&str> = p
            .entries
            .iter()
            .filter(|e| e.highlighted)
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(
            hl,
            [
                "FPGA2B Clock 5",
                "FPGA2B Clock 6",
                "FPGA2B Clock 7",
                "FPGA2B Clock 8"
            ]
        );
        for n in 1..=9 {
            assert_eq!(
                p.mpts_bitrate(InputFpga::A, n),
                Some((Bitrate(0), Bitrate(0)))
            );
        }
        for n in 1..=8 {
            assert_eq!(p.timestamp_errors(InputFpga::B, n), Some(0));
        }
        assert_eq!(
            p.value("Registers", "FPGA2A IP Register 0"),
            Some("0.0.0.0")
        );
        assert_eq!(
            p.value("Registers", "FPGA2B RTP Flag & Port Register 33"),
            Some("00000000")
        );
    }

    #[test]
    fn golden_live_page_with_slot_1_in_use() {
        // Captured 2026-11-27 with slot 1 = UDP://239.10.0.1:5000 arriving on GIGE1. Slot 1
        // shows up as FPGA2A's register 0 and MPTS stream 1 and nowhere else: the evidence for
        // the slot-to-FPGA mapping in the supervisor.
        let p = DebugPage::parse(include_bytes!(
            "../../tests/fixtures/debug.cgi_stream1.html"
        ))
        .unwrap();
        assert_eq!(
            p.value("Registers", "FPGA2A IP Register 0"),
            Some("239.10.0.1")
        );
        assert_eq!(
            p.value("Registers", "FPGA2A RTP Flag & Port Register 0"),
            Some("00001388")
        );
        assert_eq!(
            p.mpts_bitrate(InputFpga::A, 1),
            Some((Bitrate(2_906_503), Bitrate(4_120_960)))
        );
        assert_eq!(p.timestamp_errors(InputFpga::A, 1), Some(83));
        for n in 2..=9 {
            assert_eq!(
                p.mpts_bitrate(InputFpga::A, n),
                Some((Bitrate(0), Bitrate(0)))
            );
            // The timestamp section lists 8 streams per FPGA, the bitrate sections 9.
            assert_eq!(p.timestamp_errors(InputFpga::A, n), (n <= 8).then_some(0));
        }
        for n in 1..=32 {
            assert_eq!(
                p.stream_counter(StreamCounter::BufferOverflow, InputFpga::A, n),
                Some(0)
            );
        }
    }
}
