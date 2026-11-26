//! `diagnostic.cgi` ("Diagnostics"): per input `#`, Stream, IP, Ping (two cells),
//! Oversubscription, TS Sync Errors.
//!
//! The saved page has the header row only (no inputs configured). The data-row templates in
//! `diagnostic.cgi` are `%d` (index), `%d.%d.%d.%d` (address), an empty cell colored
//! `#00FF00` / `#FF0000`, a Ping button cell, a result cell (`Passed`, `Failed`, `-` or
//! `%d / 4`), and further cells whose order could not be fixed from the strings alone. The
//! parser therefore keeps every row's cell texts and reads the typed fields by the column
//! order of the header; this is listed for a live check once an input is configured.

use crate::html::{Document, row_cells};
use crate::pages::status::InputState;
use crate::pages::{ParseError, expect_page};

/// One input row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRow {
    /// The `#` column.
    pub index: u8,
    /// The Stream column, verbatim.
    pub stream: String,
    /// The IP column's color.
    pub input: InputState,
    /// The Ping result cell, verbatim.
    pub ping: String,
    /// The Oversubscription column, verbatim.
    pub oversubscription: String,
    /// The TS Sync Errors column as a number, when it is one.
    pub ts_sync_errors: Option<u64>,
    /// Every cell's text, for anything the typed fields miss.
    pub cells: Vec<String>,
}

/// The page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticPage {
    /// Input rows.
    pub rows: Vec<DiagnosticRow>,
}

impl DiagnosticPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "Diagnostics")?;
        if !doc.text().contains("TS Sync Errors") {
            return Err(ParseError::structure(
                "diagnostic.cgi",
                "diagnostics table missing",
            ));
        }
        let mut rows = Vec::new();
        for tr in doc.find_all("tr") {
            let cells = row_cells(tr);
            if cells.len() < 5 || cells.iter().any(|c| !c.find_all("td").is_empty()) {
                continue;
            }
            let Ok(index) = cells[0].text().parse::<u8>() else {
                continue;
            };
            if !(1..=16).contains(&index) {
                continue;
            }
            let texts: Vec<String> = cells.iter().map(|c| c.text()).collect();
            let input = match cells[2]
                .attr("bgcolor")
                .map(|c| c.trim_start_matches('#').to_ascii_uppercase())
            {
                Some(c) if c == "00FF00" => InputState::Present,
                Some(c) if c == "FF0000" => InputState::Absent,
                Some(c) if c == "C0C0C0" => InputState::NotAssigned,
                other => InputState::Unknown(other.unwrap_or_default()),
            };
            let at = |k: usize| texts.get(k).cloned().unwrap_or_default();
            rows.push(DiagnosticRow {
                index,
                stream: at(1),
                input,
                ping: at(4),
                oversubscription: at(5),
                ts_sync_errors: texts.get(6).and_then(|t| t.parse().ok()),
                cells: texts,
            });
        }
        Ok(DiagnosticPage { rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    #[test]
    fn golden_saved_page_has_no_rows() {
        assert_eq!(
            DiagnosticPage::parse(fixtures::DIAGNOSTIC).unwrap().rows,
            vec![]
        );
    }

    #[test]
    fn synthetic_row() {
        let s = String::from_utf8_lossy(fixtures::DIAGNOSTIC).replace(
            "                                  </tbody>",
            "<tr>\
             <td align=\"center\" bgcolor=\"#C0C0C0\">1</td>\
             <td align=\"center\" bgcolor=\"#C0C0C0\">239.10.0.1</td>\
             <td bgcolor=\"#00FF00\"></td>\
             <td bgcolor=\"#C0C0C0\" align=\"center\"><input type=\"submit\" disabled value=\"Ping\" style=\"width: 50px\" name=\"btnPing1\"></td>\
             <td width=\"9%\" bgcolor=\"#C0C0C0\" align=\"center\">-</td>\
             <td align=\"center\" bgcolor=\"#COCOCO\"></td>\
             <td align=\"center\" bgcolor=\"#C0C0C0\">3</td></tr></tbody>",
        );
        let p = DiagnosticPage::parse(s.as_bytes()).unwrap();
        assert_eq!(p.rows.len(), 1);
        let r = &p.rows[0];
        assert_eq!(r.index, 1);
        assert_eq!(r.stream, "239.10.0.1");
        assert_eq!(r.input, InputState::Present);
        assert_eq!(r.ping, "-");
        assert_eq!(r.ts_sync_errors, Some(3));
    }
}
