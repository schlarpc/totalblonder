//! Typed pages parsed from the device's HTML.
//!
//! Every parser first classifies the page ([`classify`]) so that a "Logged Out" page, a
//! "Modulator Error" page or the login form is never mistaken for an empty table.

pub mod admin;
pub mod debug;
pub mod diagnostic;
pub mod eventlog;
pub mod qamconfig;
pub mod status;
pub mod statusedit;
pub mod streaminfo;

use crate::html::{Document, collapse_ws};
use crate::types::{SessionId, TemperatureF, TsMode, Uptime, ValueError, parse_tenths};

/// A page that could not be read as what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The "Logged Out" page: the session id is not (or no longer) valid.
    #[error("the device answered with the Logged Out page")]
    LoggedOut,
    /// The "Modulator Error" page ("error reading the QAM Modulators"); transient.
    #[error("the device could not read its QAM modulators")]
    ModulatorError,
    /// A different page than expected (by title).
    #[error("expected page {expected:?}, got title {got:?}")]
    WrongPage {
        /// Title prefix expected.
        expected: &'static str,
        /// Title received.
        got: Option<String>,
    },
    /// The page has the expected title but not the expected structure.
    #[error("{page}: {what}")]
    Structure {
        /// Page.
        page: &'static str,
        /// What was missing or odd.
        what: String,
    },
    /// The account may not read this page (a Guest asking for `admin.cgi`).
    #[error("the account may not read this page")]
    Unauthorized,
    /// A value did not parse.
    #[error("{page}: {source}")]
    Value {
        /// Page.
        page: &'static str,
        /// The value error.
        source: ValueError,
    },
}

impl ParseError {
    pub(crate) fn structure(page: &'static str, what: impl Into<String>) -> Self {
        ParseError::Structure {
            page,
            what: what.into(),
        }
    }
}

/// What kind of page came back, before parsing it as anything specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageClass {
    /// "Logged Out": the session is gone.
    LoggedOut,
    /// "Modulator Error".
    ModulatorError,
    /// The login form. `admin_present` is true when the page warns that an Administrator is
    /// logged in (logging in as Admin will end that session); `failed` when it says
    /// "Login failed".
    LoginForm {
        /// The session id handed out in the hidden field.
        session: Option<SessionId>,
        /// Another Administrator session exists.
        admin_present: bool,
        /// The page reports a failed login.
        failed: bool,
    },
    /// "Welcome `<user>`!" after a successful login, with the session id from its refresh URL.
    Welcome {
        /// The session id to use.
        session: Option<SessionId>,
    },
    /// "Maximum number of users reached".
    TooManyUsers,
    /// Any other page, by title.
    Titled(String),
    /// No title at all.
    Untitled,
}

/// Classify a page.
pub fn classify(doc: &Document) -> PageClass {
    let title = doc.title();
    match title.as_deref() {
        Some("Logged Out") => PageClass::LoggedOut,
        Some("Modulator Error") => PageClass::ModulatorError,
        Some("Welcome") => {
            let session = doc
                .find_all("meta")
                .into_iter()
                .filter_map(|m| m.attr("content"))
                .find_map(|c| c.split("session_id=").nth(1).map(str::to_owned))
                .and_then(|s| {
                    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
                    SessionId::parse(&digits).ok()
                });
            PageClass::Welcome { session }
        }
        Some("Error") if doc.text().contains("Maximum number of") => PageClass::TooManyUsers,
        Some(t) if t == "Login" || t.starts_with("Login - ") => {
            let session = doc
                .forms()
                .iter()
                .find_map(|f| {
                    f.control("session_id")
                        .and_then(|c| c.text_value())
                        .map(str::to_owned)
                })
                .and_then(|s| SessionId::parse(&s).ok());
            let scripts: String = doc
                .find_all("script")
                .into_iter()
                .map(|s| s.text())
                .collect::<Vec<_>>()
                .join(" ");
            PageClass::LoginForm {
                session,
                admin_present: scripts
                    .contains("An Administrator is currently logged into the system"),
                failed: doc.text().contains("Login failed"),
            }
        }
        Some(t) => PageClass::Titled(t.to_owned()),
        None => {
            if doc.text().contains("Maximum number of") {
                PageClass::TooManyUsers
            } else {
                PageClass::Untitled
            }
        }
    }
}

/// Check the page is the one asked for (title is `prefix` or starts with `prefix - `).
pub(crate) fn expect_page(doc: &Document, prefix: &'static str) -> Result<(), ParseError> {
    match classify(doc) {
        PageClass::LoggedOut => Err(ParseError::LoggedOut),
        PageClass::ModulatorError => Err(ParseError::ModulatorError),
        PageClass::Titled(t) if t == prefix || t.starts_with(&format!("{prefix} - ")) => Ok(()),
        PageClass::Titled(t) => Err(ParseError::WrongPage {
            expected: prefix,
            got: Some(t),
        }),
        _ => Err(ParseError::WrongPage {
            expected: prefix,
            got: doc.title(),
        }),
    }
}

/// The header every main page prints: ESN, temperature, uptime, headend name, mode, location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHeader {
    /// Electronic serial number.
    pub esn: String,
    /// Chassis temperature.
    pub temperature: Option<TemperatureF>,
    /// Uptime.
    pub uptime: Uptime,
    /// Headend name (Unit Name). Defect A can put raw bytes here; kept as decoded Latin-1.
    pub headend_name: String,
    /// TS mode.
    pub mode: TsMode,
    /// Location.
    pub location: String,
}

impl PageHeader {
    /// Read the header from the cells `ESN: …`, `Temperature: …`, `Uptime: …`,
    /// `Headend Name: …`, `Mode: …`, `Location: …`.
    pub fn parse(doc: &Document, page: &'static str) -> Result<Self, ParseError> {
        let cells: Vec<String> = doc
            .find_all("td")
            .into_iter()
            .filter(|td| td.find_all("td").is_empty())
            .map(|td| td.text())
            .collect();
        let field = |label: &str| {
            cells
                .iter()
                .find_map(|c| c.strip_prefix(label).map(collapse_ws))
        };
        let need = |label: &'static str| {
            field(label).ok_or_else(|| {
                ParseError::structure(page, format!("header cell {label:?} missing"))
            })
        };
        let uptime = need("Uptime:")?
            .parse::<Uptime>()
            .map_err(|source| ParseError::Value { page, source })?;
        let mode = match need("Mode:")?.as_str() {
            "MPTS" => TsMode::Mpts,
            "SPTS" => TsMode::Spts,
            other => return Err(ParseError::structure(page, format!("mode {other:?}"))),
        };
        let temperature = field("Temperature:").and_then(|t| {
            let num: String = t
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                .collect();
            parse_tenths(&num).map(|tenths| TemperatureF { tenths })
        });
        Ok(PageHeader {
            esn: need("ESN:")?,
            temperature,
            uptime,
            headend_name: field("Headend Name:").unwrap_or_default(),
            mode,
            location: field("Location:").unwrap_or_default(),
        })
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! The saved pages (tests/fixtures; session ids replaced by 123456789).
    pub const LOGIN: &[u8] = include_bytes!("../../tests/fixtures/login.cgi.html");
    pub const LOGIN_POST: &[u8] = include_bytes!("../../tests/fixtures/login_post.html");
    pub const STATUS: &[u8] = include_bytes!("../../tests/fixtures/status.cgi.html");
    pub const STATUS_BOX2: &[u8] = include_bytes!("../../tests/fixtures/status.cgi_box2.html");
    pub const STATUS_EDIT: &[u8] = include_bytes!("../../tests/fixtures/statusedit.cgi.html");
    pub const QAM: [&[u8]; 4] = [
        include_bytes!("../../tests/fixtures/qamconfig.cgi_slot1.html"),
        include_bytes!("../../tests/fixtures/qamconfig.cgi_slot2.html"),
        include_bytes!("../../tests/fixtures/qamconfig.cgi_slot3.html"),
        include_bytes!("../../tests/fixtures/qamconfig.cgi_slot4.html"),
    ];
    pub const STREAM_INFO: &[u8] = include_bytes!("../../tests/fixtures/streaminfo.cgi.html");
    pub const DIAGNOSTIC: &[u8] = include_bytes!("../../tests/fixtures/diagnostic.cgi.html");
    pub const DEBUG: &[u8] = include_bytes!("../../tests/fixtures/debug.cgi.html");
    pub const EVENT_LOG: &[u8] = include_bytes!("../../tests/fixtures/errorlog.cgi.html");
    /// Synthetic: the exact text of the Logged Out page from the CGI binaries' strings.
    pub const LOGGED_OUT: &[u8] = include_bytes!("../../tests/fixtures/synthetic/logged_out.html");
    /// Synthetic: the Modulator Error page from the strings.
    pub const MODULATOR_ERROR: &[u8] =
        include_bytes!("../../tests/fixtures/synthetic/modulator_error.html");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_saved_pages() {
        let login = Document::parse(fixtures::LOGIN);
        match classify(&login) {
            PageClass::LoginForm {
                session,
                admin_present,
                failed,
            } => {
                assert_eq!(session, Some(SessionId::parse("123456789").unwrap()));
                assert!(!admin_present);
                assert!(!failed);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            classify(&Document::parse(fixtures::LOGIN_POST)),
            PageClass::Welcome {
                session: Some(SessionId::parse("123456789").unwrap())
            }
        );
        assert_eq!(
            classify(&Document::parse(fixtures::LOGGED_OUT)),
            PageClass::LoggedOut
        );
        assert_eq!(
            classify(&Document::parse(fixtures::MODULATOR_ERROR)),
            PageClass::ModulatorError
        );
        assert_eq!(
            classify(&Document::parse(fixtures::STATUS)),
            PageClass::Titled(String::from("Status - Ball Holdings"))
        );
    }

    #[test]
    fn header_of_saved_status_page() {
        let h = PageHeader::parse(&Document::parse(fixtures::STATUS), "status").unwrap();
        assert_eq!(h.esn, "2013090274");
        assert_eq!(h.temperature, Some(TemperatureF { tenths: 808 }));
        assert_eq!(h.uptime.0.as_secs(), 6 * 86400 + 17 * 3600 + 50 * 60 + 6);
        assert_eq!(h.headend_name, "Ball Holdings");
        assert_eq!(h.mode, TsMode::Mpts);
        assert_eq!(h.location, "");
        let h2 = PageHeader::parse(&Document::parse(fixtures::STATUS_BOX2), "status").unwrap();
        assert_eq!(h2.temperature, Some(TemperatureF { tenths: 812 }));
    }

    #[test]
    fn logged_out_is_an_error_not_an_empty_page() {
        let d = Document::parse(fixtures::LOGGED_OUT);
        assert_eq!(expect_page(&d, "Status"), Err(ParseError::LoggedOut));
        let d = Document::parse(fixtures::STATUS_EDIT);
        assert!(matches!(
            expect_page(&d, "Status"),
            Err(ParseError::WrongPage { .. })
        ));
        assert!(expect_page(&d, "Status Edit").is_ok());
    }
}
