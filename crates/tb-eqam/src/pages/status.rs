//! `status.cgi`: one row per MPTS carrier, a lock cell per RF module, and the page header.
//!
//! Row layout (saved page, and the `printf` templates in `status.cgi`): `#`, Stream
//! (`UDP://a.b.c.d:port`, `RTP://…` or `No IP Assigned`), Source 1 (`-` or an IP), Name, IP
//! (an empty cell whose color is the input state: `#00FF00` present, `#FF0000` absent,
//! `#C0C0C0` no address), Bitrate (`-`, `-Avg/-Max` or `%3.1fAvg/%3.1fMax` in Mbit/s),
//! Pro:Idiom, RF Chan. (`%d / On|Off|CW` or `- / …`), and on the first row of each module a
//! Status cell spanning eight rows (`Locked`, `Not Installed`, or a red `ERROR`). Source 2 is
//! on a second row.

use crate::html::{Document, row_cells};
use crate::pages::{PageHeader, ParseError, expect_page};
use crate::types::{Bitrate, Channel, OutputLevel, RfModule, Slot, StreamUrl, parse_decimal};

const PAGE: &str = "status.cgi";

/// What the Stream column shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamCell {
    /// `No IP Assigned`.
    NotAssigned,
    /// A URL in the device's format.
    Url(StreamUrl),
    /// Anything else, verbatim (an address this crate would not write, e.g. port 0).
    Other(String),
}

/// The input state shown by the color of the IP cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputState {
    /// Gray: no address assigned.
    NotAssigned,
    /// Green: the input is arriving.
    Present,
    /// Red: assigned but not arriving.
    Absent,
    /// Another color, verbatim.
    Unknown(String),
}

/// A carrier's on/off state as shown in the RF column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierState {
    /// Carrier on.
    On,
    /// Carrier off.
    Off,
    /// Unmodulated test carrier.
    Cw,
}

/// The RF column of one carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfCell {
    /// The channel, or `None` when the page shows `-`.
    pub channel: Option<Channel>,
    /// On, off or CW.
    pub state: CarrierState,
}

/// A module's Status cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleState {
    /// `Locked`.
    Locked,
    /// `Not Installed`.
    NotInstalled,
    /// `ERROR` (or any other text, verbatim).
    Error(String),
}

/// One carrier row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRow {
    /// Slot / carrier number.
    pub slot: Slot,
    /// Stream URL.
    pub stream: StreamCell,
    /// The two SSM source cells (`None` for `-`).
    pub sources: [Option<String>; 2],
    /// Stream name as shown.
    pub name: String,
    /// Input state.
    pub input: InputState,
    /// Average and peak bit rate, when shown.
    pub bitrate: Option<(Bitrate, Bitrate)>,
    /// The Pro:Idiom cell, verbatim (`Disabled` on this unit).
    pub pro_idiom: String,
    /// RF channel and state.
    pub rf: RfCell,
}

/// The parsed status page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPage {
    /// Page header.
    pub header: PageHeader,
    /// Output level from the "QAM Output (40dBmV)" heading.
    pub output_level: Option<OutputLevel>,
    /// All 16 carrier rows in slot order.
    pub rows: Vec<StatusRow>,
    /// Lock state of modules 1–4.
    pub modules: [ModuleState; 4],
}

impl StatusPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "Status")?;
        let header = PageHeader::parse(&doc, PAGE)?;
        let output_level = doc.find_all("td").into_iter().find_map(|td| {
            let t = td.text();
            let rest = t.strip_prefix("QAM Output (")?;
            let n = rest.strip_suffix("dBmV)")?;
            OutputLevel::new(n.trim().parse().ok()?).ok()
        });
        let mut rows: Vec<StatusRow> = Vec::new();
        let mut modules: [Option<ModuleState>; 4] = [None, None, None, None];
        for tr in doc.find_all("tr") {
            let cells = row_cells(tr);
            if cells.len() < 8 {
                continue;
            }
            let first = cells[0].text();
            let Ok(n) = first.parse::<u8>() else { continue };
            if cells[0].attr("rowspan") != Some("2") {
                continue;
            }
            let slot = Slot::new(n).map_err(|source| ParseError::Value { page: PAGE, source })?;
            if cells.len() > 9 {
                return Err(ParseError::structure(
                    PAGE,
                    format!("row {n}: {} cells", cells.len()),
                ));
            }
            let stream_text = cells[1].text();
            let stream = if stream_text == "No IP Assigned" {
                StreamCell::NotAssigned
            } else {
                match stream_text.parse::<StreamUrl>() {
                    Ok(u) => StreamCell::Url(u),
                    Err(_) => StreamCell::Other(stream_text),
                }
            };
            let source = |t: String| {
                if t == "-" || t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            };
            let input = match cells[4]
                .attr("bgcolor")
                .map(|c| c.trim_start_matches('#').to_ascii_uppercase())
            {
                Some(c) if c == "C0C0C0" => InputState::NotAssigned,
                Some(c) if c == "00FF00" => InputState::Present,
                Some(c) if c == "FF0000" => InputState::Absent,
                other => InputState::Unknown(other.unwrap_or_default()),
            };
            let bitrate = parse_bitrate(&cells[5].text());
            let rf = parse_rf(&cells[7].text()).ok_or_else(|| {
                ParseError::structure(PAGE, format!("row {n}: RF cell {:?}", cells[7].text()))
            })?;
            if cells.len() == 9 {
                let m = slot.module().index();
                let t = cells[8].text();
                modules[m] = Some(match t.as_str() {
                    "Locked" => ModuleState::Locked,
                    "Not Installed" => ModuleState::NotInstalled,
                    _ => ModuleState::Error(t),
                });
            }
            rows.push(StatusRow {
                slot,
                stream,
                sources: [source(cells[2].text()), None],
                name: cells[3].text(),
                input,
                bitrate,
                pro_idiom: cells[6].text(),
                rf,
            });
            continue;
        }
        // Second source line: the row right after each carrier row has one cell.
        let trs = doc.find_all("tr");
        for (k, tr) in trs.iter().enumerate() {
            let cells = row_cells(tr);
            if cells.len() < 8 || cells[0].attr("rowspan") != Some("2") {
                continue;
            }
            let Ok(n) = cells[0].text().parse::<u8>() else {
                continue;
            };
            if let (Some(next), Some(row)) =
                (trs.get(k + 1), rows.iter_mut().find(|r| r.slot.get() == n))
            {
                let nc = row_cells(next);
                if nc.len() == 1 {
                    let t = nc[0].text();
                    row.sources[1] = if t == "-" || t.is_empty() {
                        None
                    } else {
                        Some(t)
                    };
                }
            }
        }
        if rows.len() != 16 {
            return Err(ParseError::structure(
                PAGE,
                format!("{} carrier rows, expected 16", rows.len()),
            ));
        }
        if rows.iter().zip(Slot::all()).any(|(r, s)| r.slot != s) {
            return Err(ParseError::structure(PAGE, "carrier rows out of order"));
        }
        let mut out = Vec::with_capacity(4);
        for (m, s) in RfModule::all().iter().zip(modules) {
            out.push(s.ok_or_else(|| {
                ParseError::structure(PAGE, format!("module {m} status cell missing"))
            })?);
        }
        let modules: [ModuleState; 4] = out
            .try_into()
            .map_err(|_| ParseError::structure(PAGE, "module cells"))?;
        Ok(StatusPage {
            header,
            output_level,
            rows,
            modules,
        })
    }

    /// The row of `slot`.
    pub fn row(&self, slot: Slot) -> &StatusRow {
        &self.rows[slot.index()]
    }
}

fn parse_bitrate(t: &str) -> Option<(Bitrate, Bitrate)> {
    let (avg, max) = t.split_once("Avg/")?;
    let max = max.strip_suffix("Max")?;
    Some((
        Bitrate(parse_decimal(avg, 1_000_000)?),
        Bitrate(parse_decimal(max, 1_000_000)?),
    ))
}

fn parse_rf(t: &str) -> Option<RfCell> {
    let (ch, state) = t.split_once('/')?;
    let state = match state.trim() {
        "On" => CarrierState::On,
        "Off" => CarrierState::Off,
        "CW" => CarrierState::Cw,
        _ => return None,
    };
    let ch = ch.trim();
    let channel = if ch == "-" {
        None
    } else {
        Some(Channel::new(ch.parse().ok()?).ok()?)
    };
    Some(RfCell { channel, state })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;
    use crate::types::TsMode;

    #[test]
    fn golden_saved_status_page() {
        let p = StatusPage::parse(fixtures::STATUS).unwrap();
        assert_eq!(p.header.mode, TsMode::Mpts);
        assert_eq!(p.output_level, Some(OutputLevel::new(40).unwrap()));
        let channels: Vec<u8> = p.rows.iter().map(|r| r.rf.channel.unwrap().get()).collect();
        assert_eq!(
            channels,
            [
                2, 3, 4, 5, 10, 11, 12, 13, 6, 95, 96, 97, 140, 141, 142, 143
            ]
        );
        for r in &p.rows {
            assert_eq!(r.stream, StreamCell::NotAssigned);
            assert_eq!(r.sources, [None, None]);
            assert_eq!(r.name, "");
            assert_eq!(r.input, InputState::NotAssigned);
            assert_eq!(r.bitrate, None);
            assert_eq!(r.pro_idiom, "Disabled");
            assert_eq!(r.rf.state, CarrierState::On);
        }
        assert_eq!(
            p.modules,
            [
                ModuleState::Locked,
                ModuleState::Locked,
                ModuleState::Locked,
                ModuleState::Locked
            ]
        );
        // The boxNumber=2 capture is the same page (boxNumber is ignored in MPTS mode).
        let p2 = StatusPage::parse(fixtures::STATUS_BOX2).unwrap();
        assert_eq!(p2.rows, p.rows);
    }

    #[test]
    fn assigned_rows_from_the_templates() {
        let p = StatusPage::parse(include_bytes!(
            "../../tests/fixtures/synthetic/status_assigned.html"
        ))
        .unwrap();
        let r1 = p.row(Slot::new(1).unwrap());
        assert_eq!(
            r1.stream,
            StreamCell::Url("UDP://239.10.0.1:5000".parse().unwrap())
        );
        assert_eq!(r1.name, "WALL");
        assert_eq!(r1.input, InputState::Present);
        assert_eq!(r1.bitrate, Some((Bitrate(8_400_000), Bitrate(9_700_000))));
        let r2 = p.row(Slot::new(2).unwrap());
        assert_eq!(
            r2.stream,
            StreamCell::Url("RTP://239.10.0.2:5000".parse().unwrap())
        );
        assert_eq!(r2.input, InputState::Absent);
        assert_eq!(r2.bitrate, None);
        assert_eq!(
            r2.rf,
            RfCell {
                channel: Some(Channel::new(3).unwrap()),
                state: CarrierState::Off
            }
        );
        assert_eq!(p.modules[1], ModuleState::Error(String::from("ERROR")));
        assert_eq!(p.modules[2], ModuleState::NotInstalled);
    }

    #[test]
    fn logged_out_page_is_reported() {
        assert_eq!(
            StatusPage::parse(fixtures::LOGGED_OUT),
            Err(ParseError::LoggedOut)
        );
        assert_eq!(
            StatusPage::parse(fixtures::MODULATOR_ERROR),
            Err(ParseError::ModulatorError)
        );
    }
}
