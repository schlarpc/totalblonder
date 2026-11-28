//! `admin.cgi` ("Admin"), read with a bare GET only (the refusal screen allows nothing else):
//! the configuration mode and the syslog settings. Only an Administrator session may read it;
//! a Guest gets an "Unauthorized Access" page ([`ParseError::Unauthorized`]).
//!
//! The configuration mode decides whether `update_config` overwrites the stream records from
//! the flash file at boot (defect B, SPEC §10.3): the supervisor writes only in Manual mode.

use crate::html::Document;
use crate::pages::{PageClass, ParseError, classify, expect_page};

const PAGE: &str = "admin.cgi";

/// The `configMode` select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigMode {
    /// 0: the stream records are rebuilt from the flash config file at boot.
    Automatic,
    /// 1: they are not.
    Manual,
}

/// What the supervisor reads from the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminPage {
    /// The configuration mode, when exactly one option is marked selected.
    pub config_mode: Option<ConfigMode>,
    /// `logDestination`.
    pub log_destination: String,
    /// `logPort`.
    pub log_port: Option<u16>,
    /// The three syslog categories (errors, informational, feedback), when a radio is checked.
    pub syslog: [Option<bool>; 3],
}

impl AdminPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        if matches!(classify(&doc), PageClass::Titled(t) if t == "Unauthorized Access") {
            return Err(ParseError::Unauthorized);
        }
        expect_page(&doc, "Admin")?;
        let selects: Vec<_> = doc
            .find_all("select")
            .into_iter()
            .filter(|s| s.attr("name") == Some("configMode"))
            .collect();
        let [sel] = selects.as_slice() else {
            return Err(ParseError::structure(PAGE, "configMode select missing"));
        };
        let marked: Vec<_> = sel
            .find_all("option")
            .into_iter()
            .filter(|o| o.has_attr("selected"))
            .filter_map(|o| o.attr("value"))
            .collect();
        let config_mode = match marked.as_slice() {
            ["0"] => Some(ConfigMode::Automatic),
            ["1"] => Some(ConfigMode::Manual),
            _ => None,
        };
        let inputs = doc.find_all("input");
        let text = |name: &str| {
            inputs
                .iter()
                .find(|i| i.attr("name") == Some(name))
                .and_then(|i| i.attr("value"))
                .map(str::to_owned)
        };
        let radio = |name: &str| {
            inputs
                .iter()
                .filter(|i| i.attr("name") == Some(name) && i.has_attr("checked"))
                .find_map(|i| i.attr("value"))
                .map(|v| v == "1")
        };
        let log_destination = text("logDestination")
            .ok_or_else(|| ParseError::structure(PAGE, "logDestination missing"))?;
        Ok(AdminPage {
            config_mode,
            log_destination,
            log_port: text("logPort").and_then(|p| p.trim().parse().ok()),
            syslog: [radio("errors"), radio("info"), radio("feedback")],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_live_admin_page() {
        // 2026-11-27, after the Manual-mode save and the syslog change.
        let p = AdminPage::parse(include_bytes!("../../tests/fixtures/admin.cgi.html")).unwrap();
        assert_eq!(p.config_mode, Some(ConfigMode::Manual));
        assert_eq!(p.log_destination, "10.42.0.10");
        assert_eq!(p.log_port, Some(514));
        assert_eq!(p.syslog, [Some(true); 3]);
    }

    #[test]
    fn automatic_and_unmarked_modes() {
        let s = String::from_utf8_lossy(include_bytes!("../../tests/fixtures/admin.cgi.html"))
            .into_owned();
        let auto = s
            .replace("<option value=\"0\">", "<option selected value=\"0\">")
            .replace("<option selected value=\"1\">", "<option value=\"1\">");
        assert_eq!(
            AdminPage::parse(auto.as_bytes()).unwrap().config_mode,
            Some(ConfigMode::Automatic)
        );
        // Nothing marked: unknown, not the first option.
        let none = s.replace("<option selected value=\"1\">", "<option value=\"1\">");
        assert_eq!(AdminPage::parse(none.as_bytes()).unwrap().config_mode, None);
    }

    #[test]
    fn a_guest_is_not_authorized() {
        assert_eq!(
            AdminPage::parse(include_bytes!("../../tests/fixtures/admin.cgi_guest.html")),
            Err(ParseError::Unauthorized)
        );
    }
}
