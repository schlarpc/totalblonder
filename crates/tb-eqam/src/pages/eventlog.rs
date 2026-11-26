//! `errorlog.cgi` ("Event Log"): the syslog lines kept in RAM (`/tmp/syslog.dat`), newest
//! first, colored by severity (red error, green feedback, blue information; manual p.37).
//! The log is lost on power loss and (being in RAM) on any reboot.

use crate::html::Document;
use crate::pages::{ParseError, expect_page};

/// Severity from the line's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Red (`#FF0000`).
    Error,
    /// Green (`#008000`).
    Feedback,
    /// Blue (`#0000FF`).
    Info,
    /// No color.
    Other,
}

/// One line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventLine {
    /// Timestamp text, whitespace collapsed (`Thu Jan 1 00:00:20 1970` before NTP).
    pub time: String,
    /// Message.
    pub message: String,
    /// Severity.
    pub severity: Severity,
}

/// Event-log lines the supervisor treats as alarms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventAlarm {
    /// "Difference detected in Config File …": `update_config` overwrote the stream table
    /// from the flash file at boot (defect B; SPEC §10.3).
    ConfigFileReconcile,
    /// "Difference detected in modulator settings. Changing RF output channel": `mux_sync`
    /// retuned a modulator from the stored table (defect A's knock-on; corruption-48 §1).
    ModulatorRetune,
    /// "CRITICAL ERROR …" or an EEPROM error: the corruption guard or a failed EEPROM write.
    Eeprom,
    /// "Default IP Used for ETHn" or "Invalid Ethernet 1 Gateway": network settings were lost.
    NetworkDefaulted,
}

impl EventLine {
    /// Whether this line is an alarm, and which.
    pub fn alarm(&self) -> Option<EventAlarm> {
        let m = &self.message;
        if m.contains("Difference detected in Config File") {
            Some(EventAlarm::ConfigFileReconcile)
        } else if m.contains("Difference detected in modulator settings") {
            Some(EventAlarm::ModulatorRetune)
        } else if m.contains("CRITICAL ERROR") || (m.contains("EEPROM") && m.contains("ERROR")) {
            Some(EventAlarm::Eeprom)
        } else if m.contains("Default IP Used") || m.contains("Invalid Ethernet") {
            Some(EventAlarm::NetworkDefaulted)
        } else {
            None
        }
    }
}

/// The page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLogPage {
    /// "Event Log Destination".
    pub destination: String,
    /// "Log Destination Port #".
    pub port: String,
    /// Lines, newest first as shown.
    pub lines: Vec<EventLine>,
    /// The page said the log could not be shown ("System low on resources").
    pub unavailable: bool,
}

impl EventLogPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "Event Log")?;
        let mut destination = String::new();
        let mut port = String::new();
        let mut lines = Vec::new();
        let mut unavailable = false;
        let mut saw_lines_field = false;
        for tr in doc.find_all("tr") {
            let cells = crate::html::row_cells(tr);
            if cells.len() == 2 {
                match cells[0].text().as_str() {
                    "Event Log Destination:" => destination = cells[1].text(),
                    "Log Destination Port #:" => port = cells[1].text(),
                    "Lines to Display:" => saw_lines_field = true,
                    _ => {}
                }
                continue;
            }
            if cells.len() != 1
                || cells[0].attr("colspan") != Some("2")
                || !cells[0].find_all("hr").is_empty()
            {
                continue;
            }
            let td = cells[0];
            let text = td.text();
            if text.is_empty() {
                continue;
            }
            let color = td
                .find_all("font")
                .first()
                .and_then(|f| f.attr("color"))
                .map(str::to_ascii_uppercase);
            if text.contains("System low on resources") {
                unavailable = true;
                continue;
            }
            if text.contains("event log empty or does not exist") {
                continue;
            }
            let severity = match color.as_deref() {
                Some("#FF0000") => Severity::Error,
                Some("#008000") => Severity::Feedback,
                Some("#0000FF") => Severity::Info,
                _ => Severity::Other,
            };
            let (time, message) = match text.split_once(" : ") {
                Some((t, m)) => (t.to_owned(), m.to_owned()),
                None => (String::new(), text),
            };
            lines.push(EventLine {
                time,
                message,
                severity,
            });
        }
        if !saw_lines_field {
            return Err(ParseError::structure(
                "errorlog.cgi",
                "\"Lines to Display\" row missing",
            ));
        }
        Ok(EventLogPage {
            destination,
            port,
            lines,
            unavailable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    #[test]
    fn golden_saved_event_log() {
        let p = EventLogPage::parse(fixtures::EVENT_LOG).unwrap();
        assert_eq!(p.destination, "10.42.0.53");
        assert_eq!(p.port, "514");
        assert!(!p.unavailable);
        assert_eq!(p.lines.len(), 2);
        assert_eq!(p.lines[0].time, "Thu Jan 1 00:00:20 1970");
        assert_eq!(
            p.lines[0].message,
            "ERROR: Difference detected in Config File versus current system settings. Correcting differences."
        );
        assert_eq!(p.lines[0].severity, Severity::Error);
        assert_eq!(p.lines[0].alarm(), Some(EventAlarm::ConfigFileReconcile));
        assert_eq!(p.lines[1].message, "Power Up Started");
        assert_eq!(p.lines[1].severity, Severity::Info);
        assert_eq!(p.lines[1].alarm(), None);
    }

    #[test]
    fn alarm_catalog() {
        let l = |m: &str| EventLine {
            time: String::new(),
            message: m.into(),
            severity: Severity::Error,
        };
        assert_eq!(
            l("ERROR: Difference detected in modulator settings. Changing RF output channel. (141)").alarm(),
            Some(EventAlarm::ModulatorRetune)
        );
        assert_eq!(
            l("CRITICAL ERROR: Possible EEPROM corruption detected!").alarm(),
            Some(EventAlarm::Eeprom)
        );
        assert_eq!(
            l("ERROR: EEPROM write verify failed!").alarm(),
            Some(EventAlarm::Eeprom)
        );
        assert_eq!(
            l("ERROR: Default IP Used for ETH1.").alarm(),
            Some(EventAlarm::NetworkDefaulted)
        );
        assert_eq!(
            l("ERROR: Stream 3 from UDP://239.1.1.1:5000 is assigned but not streaming.").alarm(),
            None
        );
    }
}
