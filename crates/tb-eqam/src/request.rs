//! Requests before screening, responses, and the transport seam.
//!
//! A [`RequestDraft`] can describe any request at all — that is the point: the refusal
//! function [`crate::refusal::screen`] is the only way to turn one into a sendable
//! [`crate::refusal::Request`], and its tests feed it drafts of every refused kind.

use crate::types::{Credentials, RfModule, SessionId};

/// HTTP method. Only the two the forms use exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// GET.
    Get,
    /// POST with an `application/x-www-form-urlencoded` body.
    Post,
}

/// A page this crate reads (each one a bare GET with only `session_id`, plus `slotNumber` for
/// the QAM pages; SPEC §10.1 "Reads", device §3 and §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReadPage {
    /// `status.cgi`
    Status,
    /// `statusedit.cgi` (read as the first and last step of a stream-table write)
    StatusEdit,
    /// `qamconfig.cgi?slotNumber=<m>`
    QamConfig(RfModule),
    /// `streaminfo.cgi`
    StreamInfo,
    /// `diagnostic.cgi`
    Diagnostic,
    /// `debug.cgi`
    Debug,
    /// `errorlog.cgi` (the Event Log)
    EventLog,
}

impl ReadPage {
    /// The CGI file name.
    pub fn cgi(self) -> &'static str {
        match self {
            ReadPage::Status => "status.cgi",
            ReadPage::StatusEdit => "statusedit.cgi",
            ReadPage::QamConfig(_) => "qamconfig.cgi",
            ReadPage::StreamInfo => "streaminfo.cgi",
            ReadPage::Diagnostic => "diagnostic.cgi",
            ReadPage::Debug => "debug.cgi",
            ReadPage::EventLog => "errorlog.cgi",
        }
    }
}

/// Permission to post one of the two allowed forms. Only the write step machines of this
/// crate can create one, so a hand-built draft that posts `statusedit.cgi` or `qamconfig.cgi`
/// is refused as [`crate::refusal::RefusalRule::WriteOutsideProtocol`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteAuthority {
    form: WriteForm,
}

/// Which form a [`WriteAuthority`] is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteForm {
    /// The 16-row stream table.
    StatusEdit,
    /// One module's QAM page, as one step of the four-module sequence.
    QamModule(RfModule),
}

impl WriteAuthority {
    pub(crate) fn status_edit() -> Self {
        WriteAuthority {
            form: WriteForm::StatusEdit,
        }
    }

    pub(crate) fn qam_module(m: RfModule) -> Self {
        WriteAuthority {
            form: WriteForm::QamModule(m),
        }
    }

    pub(crate) fn form(&self) -> WriteForm {
        self.form
    }
}

/// An unchecked request: anything can be written here. Only [`crate::refusal::screen`] turns
/// it into something a transport accepts.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestDraft {
    /// Method.
    pub method: Method,
    /// Path: a CGI name (`status.cgi`) or `/cgi-bin/<name>`.
    pub path: String,
    /// Query parameters, decoded (they are encoded when sent).
    pub query: Vec<(String, String)>,
    /// Form fields of a POST, decoded, in the order they are sent.
    pub body: Vec<(String, String)>,
    authority: Option<WriteAuthority>,
}

impl std::fmt::Debug for RequestDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestDraft")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("query", &redact(&self.query))
            .field("body", &redact(&self.body))
            .field("authority", &self.authority)
            .finish()
    }
}

/// Replace secret values (password, session id) for logging.
pub(crate) fn redact(fields: &[(String, String)]) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|(k, v)| {
            let secret =
                k.eq_ignore_ascii_case("txtPassword") || k.eq_ignore_ascii_case("session_id");
            (
                k.clone(),
                if secret {
                    String::from("<redacted>")
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}

impl RequestDraft {
    /// Any request, without write authority.
    pub fn new(method: Method, path: &str, query: &[(&str, &str)], body: &[(&str, &str)]) -> Self {
        let own = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect::<Vec<_>>()
        };
        RequestDraft {
            method,
            path: path.to_owned(),
            query: own(query),
            body: own(body),
            authority: None,
        }
    }

    /// The canonical GET of a read page.
    pub fn read(page: ReadPage, session: &SessionId) -> Self {
        let mut query = vec![(String::from("session_id"), session.wire())];
        if let ReadPage::QamConfig(m) = page {
            query.push((String::from("slotNumber"), m.get().to_string()));
        }
        RequestDraft {
            method: Method::Get,
            path: page.cgi().to_owned(),
            query,
            body: vec![],
            authority: None,
        }
    }

    /// `GET login.cgi`, which hands out a fresh session id.
    pub fn login_page() -> Self {
        RequestDraft::new(Method::Get, "login.cgi", &[], &[])
    }

    /// The login form post, exactly as the browser sends it (device §2).
    pub fn login_post(credentials: &Credentials, session: &SessionId) -> Self {
        RequestDraft {
            method: Method::Post,
            path: String::from("login.cgi"),
            query: vec![],
            body: vec![
                (
                    String::from("txtUserName"),
                    credentials.username().to_owned(),
                ),
                (
                    String::from("txtPassword"),
                    credentials.password().to_owned(),
                ),
                (String::from("session_id"), session.wire()),
                (String::from("btnSubmit"), String::from("Submit")),
            ],
            authority: None,
        }
    }

    /// A form post carrying write authority (only the write machines call this).
    pub(crate) fn write_post(
        path: &str,
        body: Vec<(String, String)>,
        authority: WriteAuthority,
    ) -> Self {
        RequestDraft {
            method: Method::Post,
            path: path.to_owned(),
            query: vec![],
            body,
            authority: Some(authority),
        }
    }

    pub(crate) fn authority(&self) -> Option<&WriteAuthority> {
        self.authority.as_ref()
    }
}

/// What came back: status code and body bytes (the device serves ISO-8859-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Body.
    pub body: Vec<u8>,
}

impl Response {
    /// A 200 response with `body`.
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Response {
            status: 200,
            body: body.into(),
        }
    }
}

/// Why a request produced no response.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportFailure {
    /// No response within the timeout (30 s; SPEC §10.1).
    #[error("request timed out")]
    Timeout,
    /// Connection refused, reset, unreachable, or similar.
    #[error("connection failed: {0}")]
    Connection(String),
}

/// The seam to the HTTP adapter in the server binary: it sends one screened request and
/// returns the status and body. It must not follow redirects, add query parameters or
/// retry; timing (spacing and the 30 s timeout) is the adapter's, decided by
/// [`crate::session::Pacer`].
pub trait Transport {
    /// Send `request` and wait for its response.
    fn send(&mut self, request: &crate::refusal::Request) -> Result<Response, TransportFailure>;
}

/// `application/x-www-form-urlencoded`, as a browser encodes it: unreserved characters as
/// they are, space as `+`, everything else as `%XX`.
pub(crate) fn form_encode(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decode `%XX` and `+` leniently (for rule checks only: what the device would see).
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.iter().map(|&c| char::from(c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_browser() {
        let f = vec![
            (
                String::from("streamGroupAddress1"),
                String::from("UDP://239.10.0.1:5000"),
            ),
            (String::from("a b"), String::from("x&y=z")),
        ];
        assert_eq!(
            form_encode(&f),
            "streamGroupAddress1=UDP%3A%2F%2F239.10.0.1%3A5000&a+b=x%26y%3Dz"
        );
    }

    #[test]
    fn decoding_for_rules() {
        assert_eq!(percent_decode("%62tnReboot"), "btnReboot");
        assert_eq!(percent_decode("a+b%2"), "a b%2");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
