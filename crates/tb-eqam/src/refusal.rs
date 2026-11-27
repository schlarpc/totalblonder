//! The hard refusal list (SPEC §10.1): one function every request passes through.
//!
//! [`screen`] is the only constructor of [`Request`], the only type a [`crate::Transport`]
//! sends. `Request`'s fields are private to this module, it has no `Default`, `Clone` or
//! builder, so neither callers nor the rest of this crate can make one without screening.
//!
//! ```compile_fail
//! // A sendable request cannot be built by hand: every field is private to this module.
//! let r = tb_eqam::Request {
//!     kind: tb_eqam::RequestKind::LoginPage,
//!     method: tb_eqam::Method::Post,
//!     cgi: "admin.cgi",
//!     query: vec![(String::from("btnReboot"), String::from("reboot"))],
//!     body: vec![],
//! };
//! ```
//!
//! ```compile_fail
//! // Write authority cannot be created outside the crate either.
//! let a = tb_eqam::WriteAuthority::status_edit();
//! ```
//!
//! The screen is an allow-list: a request is sendable only if it is exactly one of
//!
//! | Request | Shape |
//! |---|---|
//! | `GET login.cgi` | no query, no body |
//! | `POST login.cgi` | body `txtUserName`, `txtPassword`, `session_id`, `btnSubmit=Submit`, nothing else |
//! | `GET status.cgi`, `statusedit.cgi`, `streaminfo.cgi`, `diagnostic.cgi`, `debug.cgi`, `errorlog.cgi` | query `session_id=<digits>` only |
//! | `GET qamconfig.cgi` | query `session_id=<digits>&slotNumber=<1..4>` only |
//! | `POST statusedit.cgi` | write authority; all 16 rows' `streamGroupAddress<n>` and `streamName<n>` (valid URL and name, or both empty), optional `proIdiomEnable<n>=on`, `session_id`, `btnSave=Save` |
//! | `POST qamconfig.cgi` | write authority for that module; `slotNumber`, `outputChannelA`, `outputChannelControlA..D`, `QAMModeA`, `QAMMap`, `qam_saved=1`, `session_id`, and `outputLevel` exactly when the module is 1 |
//!
//! Everything else is refused, and the refusals that matter most get their own rule so the
//! metric `device_refused_total{rule}` says what was attempted.

use std::collections::BTreeSet;
use std::fmt;

use crate::request::{
    Method, ReadPage, RequestDraft, WriteForm, form_encode, percent_decode, redact,
};
use crate::types::{OutputLevel, QamMap, QamMode, RfModule, SessionId, StreamName, StreamUrl};

/// Which rule refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefusalRule {
    /// `admin.cgi`, `admin_redir.cgi`, `Admin.html` or anything naming "admin", any method:
    /// reboot, flush, delete-config, passwords and every editable network setting live there.
    AdminPage,
    /// A query parameter whose name starts with `btn`: the CGIs act on `btnReboot`,
    /// `btnFlush`, `btnDeleteConfig`, `btnClear` … in a GET query.
    ButtonInQuery,
    /// `logout=` anywhere: ends the session.
    Logout,
    /// `config.cgi` (GET regenerates the export; POST is the restore with parser bugs) or the
    /// export file itself.
    ConfigPage,
    /// A POST to `mode.cgi`: a switch to SPTS would retune RF module 2 from stale bytes.
    ModeWrite,
    /// A POST to any page other than the two allowed forms and the login form (EAS, license,
    /// time, network, event log, diagnostics, debug, stream config, status, …).
    ForbiddenWrite,
    /// A POST to `statusedit.cgi` or `qamconfig.cgi` that did not come from this crate's write
    /// protocol (no or mismatched write authority).
    WriteOutsideProtocol,
    /// A page or method not on the allow-list, or a path that is not a plain CGI name.
    NotAllowListed,
    /// An allowed page with a query or body of the wrong shape or values.
    Malformed,
}

impl RefusalRule {
    /// Every rule.
    pub const ALL: [RefusalRule; 9] = [
        RefusalRule::AdminPage,
        RefusalRule::ButtonInQuery,
        RefusalRule::Logout,
        RefusalRule::ConfigPage,
        RefusalRule::ModeWrite,
        RefusalRule::ForbiddenWrite,
        RefusalRule::WriteOutsideProtocol,
        RefusalRule::NotAllowListed,
        RefusalRule::Malformed,
    ];

    /// The label used in `device_refused_total{rule}`.
    pub fn as_str(self) -> &'static str {
        match self {
            RefusalRule::AdminPage => "admin_page",
            RefusalRule::ButtonInQuery => "button_in_query",
            RefusalRule::Logout => "logout",
            RefusalRule::ConfigPage => "config_page",
            RefusalRule::ModeWrite => "mode_write",
            RefusalRule::ForbiddenWrite => "forbidden_write",
            RefusalRule::WriteOutsideProtocol => "write_outside_protocol",
            RefusalRule::NotAllowListed => "not_allow_listed",
            RefusalRule::Malformed => "malformed",
        }
    }
}

/// A refused request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("refused ({}): {detail}", rule.as_str())]
pub struct Refusal {
    /// The rule.
    pub rule: RefusalRule,
    /// What was wrong (never contains a password or session id).
    pub detail: String,
}

fn refuse<T>(rule: RefusalRule, detail: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        rule,
        detail: detail.into(),
    })
}

/// What a screened request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// `GET login.cgi`.
    LoginPage,
    /// `POST login.cgi`.
    LoginPost,
    /// A read.
    Read(ReadPage),
    /// The stream-table form post.
    StatusEditPost,
    /// One module's QAM form post (a step of the four-module sequence).
    QamModulePost(RfModule),
}

impl RequestKind {
    /// Whether the request changes device state (and so is never retried blindly).
    pub fn is_write(self) -> bool {
        matches!(
            self,
            RequestKind::StatusEditPost | RequestKind::QamModulePost(_)
        )
    }
}

/// A request that passed [`screen`]. The only thing a transport sends.
pub struct Request {
    kind: RequestKind,
    method: Method,
    cgi: &'static str,
    query: Vec<(String, String)>,
    body: Vec<(String, String)>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("kind", &self.kind)
            .field("method", &self.method)
            .field("cgi", &self.cgi)
            .field("query", &redact(&self.query))
            .field("body", &redact(&self.body))
            .finish()
    }
}

impl Request {
    /// What this request is.
    pub fn kind(&self) -> RequestKind {
        self.kind
    }

    /// Method.
    pub fn method(&self) -> Method {
        self.method
    }

    /// Path and query, e.g. `/cgi-bin/status.cgi?session_id=123`. Append to the base URL.
    pub fn target(&self) -> String {
        if self.query.is_empty() {
            format!("/cgi-bin/{}", self.cgi)
        } else {
            format!("/cgi-bin/{}?{}", self.cgi, form_encode(&self.query))
        }
    }

    /// The POST body, `application/x-www-form-urlencoded`; `None` for a GET.
    pub fn body(&self) -> Option<Vec<u8>> {
        match self.method {
            Method::Get => None,
            Method::Post => Some(form_encode(&self.body).into_bytes()),
        }
    }

    /// The content type of [`Request::body`].
    pub fn content_type(&self) -> Option<&'static str> {
        match self.method {
            Method::Get => None,
            Method::Post => Some("application/x-www-form-urlencoded"),
        }
    }

    /// The decoded body fields (for tests and the device log; contains the password for a
    /// login post — do not log it).
    pub fn fields(&self) -> &[(String, String)] {
        &self.body
    }

    /// Whether one retry after a timeout is allowed (reads only; SPEC §10.1).
    pub fn retryable(&self) -> bool {
        !self.kind.is_write() && self.kind != RequestKind::LoginPost
    }
}

const READ_PAGES: &[(&str, ReadPage)] = &[
    ("status.cgi", ReadPage::Status),
    ("statusedit.cgi", ReadPage::StatusEdit),
    ("streaminfo.cgi", ReadPage::StreamInfo),
    ("diagnostic.cgi", ReadPage::Diagnostic),
    ("debug.cgi", ReadPage::Debug),
    ("errorlog.cgi", ReadPage::EventLog),
];

/// Pages that exist on the device and must never be posted.
const NO_WRITE_PAGES: &[&str] = &[
    "eas.cgi",
    "license.cgi",
    "license_redir.cgi",
    "time.cgi",
    "network.cgi",
    "errorlog.cgi",
    "diagnostic.cgi",
    "debug.cgi",
    "streamconfig.cgi",
    "status.cgi",
    "streaminfo.cgi",
    "cgi_demo",
];

/// Screen a draft. Returns a sendable [`Request`] only for the allow-list above.
pub fn screen(draft: RequestDraft) -> Result<Request, Refusal> {
    let decoded_path = percent_decode(&draft.path).to_ascii_lowercase();
    let (path_part, path_query) = match decoded_path.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(q.to_owned())),
        None => (decoded_path.clone(), None),
    };
    let query_names: Vec<String> = draft
        .query
        .iter()
        .map(|(k, _)| percent_decode(k).to_ascii_lowercase())
        .chain(path_query.iter().flat_map(|q| {
            q.split(['&', ';'])
                .map(|kv| kv.split('=').next().unwrap_or("").to_owned())
        }))
        .collect();
    let body_names: Vec<String> = draft
        .body
        .iter()
        .map(|(k, _)| percent_decode(k).to_ascii_lowercase())
        .collect();

    // 1. Admin, by any method and in any spelling.
    if decoded_path.contains("admin") {
        return refuse(RefusalRule::AdminPage, format!("path {:?}", draft.path));
    }
    // 2. Action buttons in the query string.
    if let Some(n) = query_names
        .iter()
        .find(|n| n.trim_start().starts_with("btn"))
    {
        return refuse(RefusalRule::ButtonInQuery, format!("query parameter {n:?}"));
    }
    // 3. Logout.
    if query_names
        .iter()
        .chain(body_names.iter())
        .any(|n| n.trim() == "logout")
    {
        return refuse(RefusalRule::Logout, "logout parameter");
    }
    let base = path_part.rsplit('/').next().unwrap_or("").trim().to_owned();
    // 4. The config export / restore page and the export file.
    if base == "config.cgi" || decoded_path.contains("config_file") {
        return refuse(RefusalRule::ConfigPage, format!("path {:?}", draft.path));
    }
    // 5. Only plain CGI names, optionally under /cgi-bin/.
    let name = canonical_cgi(&draft.path).ok_or_else(|| Refusal {
        rule: RefusalRule::NotAllowListed,
        detail: format!("path {:?} is not a plain CGI name", draft.path),
    })?;

    match draft.method {
        Method::Post => {
            if !draft.query.is_empty() {
                return refuse(RefusalRule::Malformed, "a POST carries no query string");
            }
            match name {
                "mode.cgi" => refuse(RefusalRule::ModeWrite, "mode.cgi is never written"),
                "login.cgi" => screen_login_post(draft),
                "statusedit.cgi" => screen_status_edit_post(draft),
                "qamconfig.cgi" => screen_qam_post(draft),
                n if NO_WRITE_PAGES.contains(&n) => {
                    refuse(RefusalRule::ForbiddenWrite, format!("{n} is never written"))
                }
                n => refuse(RefusalRule::NotAllowListed, format!("POST {n}")),
            }
        }
        Method::Get => {
            if !draft.body.is_empty() {
                return refuse(RefusalRule::Malformed, "a GET carries no body");
            }
            if draft.authority().is_some() {
                return refuse(RefusalRule::Malformed, "write authority on a GET");
            }
            if name == "login.cgi" {
                if !draft.query.is_empty() {
                    return refuse(RefusalRule::Malformed, "GET login.cgi takes no query");
                }
                return Ok(Request {
                    kind: RequestKind::LoginPage,
                    method: Method::Get,
                    cgi: "login.cgi",
                    query: vec![],
                    body: vec![],
                });
            }
            if name == "qamconfig.cgi" {
                let [(k1, v1), (k2, v2)] = draft.query.as_slice() else {
                    return refuse(
                        RefusalRule::Malformed,
                        "qamconfig.cgi takes session_id and slotNumber",
                    );
                };
                if k1 != "session_id" || k2 != "slotNumber" {
                    return refuse(
                        RefusalRule::Malformed,
                        "qamconfig.cgi takes session_id and slotNumber",
                    );
                }
                session_value(v1)?;
                let m = module_value(v2)?;
                return Ok(Request {
                    kind: RequestKind::Read(ReadPage::QamConfig(m)),
                    method: Method::Get,
                    cgi: "qamconfig.cgi",
                    query: draft.query,
                    body: vec![],
                });
            }
            let Some(&(cgi, page)) = READ_PAGES.iter().find(|(n, _)| *n == name) else {
                return refuse(RefusalRule::NotAllowListed, format!("GET {name}"));
            };
            let [(k, v)] = draft.query.as_slice() else {
                return refuse(
                    RefusalRule::Malformed,
                    format!("{cgi} takes session_id only"),
                );
            };
            if k != "session_id" {
                return refuse(
                    RefusalRule::Malformed,
                    format!("{cgi} takes session_id only"),
                );
            }
            session_value(v)?;
            Ok(Request {
                kind: RequestKind::Read(page),
                method: Method::Get,
                cgi,
                query: draft.query,
                body: vec![],
            })
        }
    }
}

/// The CGI name if `path` is `name.cgi` or `/cgi-bin/name.cgi` with `name` made of lower-case
/// letters and `_` only, and the name is one the device has.
fn canonical_cgi(path: &str) -> Option<&'static str> {
    const KNOWN: &[&str] = &[
        "login.cgi",
        "status.cgi",
        "statusedit.cgi",
        "qamconfig.cgi",
        "streaminfo.cgi",
        "diagnostic.cgi",
        "debug.cgi",
        "errorlog.cgi",
        "mode.cgi",
        "eas.cgi",
        "license.cgi",
        "license_redir.cgi",
        "time.cgi",
        "network.cgi",
        "streamconfig.cgi",
        "cgi_demo",
    ];
    let bare = path.strip_prefix("/cgi-bin/").unwrap_or(path);
    KNOWN.iter().copied().find(|k| *k == bare)
}

/// A session id in exactly the form the device prints (no sign, spaces or leading zeros).
fn session_value(v: &str) -> Result<SessionId, Refusal> {
    let bad = || Refusal {
        rule: RefusalRule::Malformed,
        detail: String::from("session_id must be the device's decimal id"),
    };
    let id = SessionId::parse(v).map_err(|_| bad())?;
    if id.wire() != v {
        return Err(bad());
    }
    Ok(id)
}

fn module_value(v: &str) -> Result<RfModule, Refusal> {
    let bad = || Refusal {
        rule: RefusalRule::Malformed,
        detail: format!("slotNumber {v:?} is not 1..4"),
    };
    if v.len() != 1 {
        return Err(bad());
    }
    let n: u8 = v.parse().map_err(|_| bad())?;
    RfModule::new(n).map_err(|_| bad())
}

fn small_number(v: &str, field: &str) -> Result<u8, Refusal> {
    let bad = || Refusal {
        rule: RefusalRule::Malformed,
        detail: format!("{field} {v:?} is not a number"),
    };
    if v.is_empty() || v.len() > 3 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    v.parse().map_err(|_| bad())
}

fn exactly_once(body: &[(String, String)]) -> Result<(), Refusal> {
    let mut seen = BTreeSet::new();
    for (k, _) in body {
        if !seen.insert(k.as_str()) {
            return refuse(RefusalRule::Malformed, format!("field {k:?} appears twice"));
        }
    }
    Ok(())
}

fn screen_login_post(draft: RequestDraft) -> Result<Request, Refusal> {
    if draft.authority().is_some() {
        return refuse(RefusalRule::Malformed, "write authority on the login form");
    }
    let [(k1, user), (k2, pass), (k3, sid), (k4, btn)] = draft.body.as_slice() else {
        return refuse(
            RefusalRule::Malformed,
            "login form must be exactly four fields",
        );
    };
    if (k1.as_str(), k2.as_str(), k3.as_str(), k4.as_str())
        != ("txtUserName", "txtPassword", "session_id", "btnSubmit")
        || btn != "Submit"
    {
        return refuse(RefusalRule::Malformed, "login form fields out of shape");
    }
    if crate::types::Credentials::new(user, pass).is_err() {
        return refuse(RefusalRule::Malformed, "login credentials out of shape");
    }
    session_value(sid)?;
    Ok(Request {
        kind: RequestKind::LoginPost,
        method: Method::Post,
        cgi: "login.cgi",
        query: vec![],
        body: draft.body,
    })
}

fn screen_status_edit_post(draft: RequestDraft) -> Result<Request, Refusal> {
    match draft.authority().map(|a| a.form()) {
        Some(WriteForm::StatusEdit) => {}
        _ => {
            return refuse(
                RefusalRule::WriteOutsideProtocol,
                "statusedit.cgi is posted only by the stream-table write",
            );
        }
    }
    exactly_once(&draft.body)?;
    let get = |k: &str| {
        draft
            .body
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    let mut expected: BTreeSet<String> = [String::from("session_id"), String::from("btnSave")]
        .into_iter()
        .collect();
    for n in 1..=16 {
        let url_key = format!("streamGroupAddress{n}");
        let name_key = format!("streamName{n}");
        let (Some(url), Some(name)) = (get(&url_key), get(&name_key)) else {
            // A missing row is zeroed by the CGI (corruption-48 §3 "POST truncation").
            return refuse(
                RefusalRule::Malformed,
                format!("row {n} missing: every POST carries all 16 rows"),
            );
        };
        if url.is_empty() != name.is_empty() {
            return refuse(
                RefusalRule::Malformed,
                format!("row {n}: URL and name must both be set or both empty"),
            );
        }
        if !url.is_empty() {
            if url.parse::<StreamUrl>().is_err() {
                return refuse(
                    RefusalRule::Malformed,
                    format!("row {n}: invalid stream URL {url:?}"),
                );
            }
            if name.parse::<StreamName>().is_err() {
                return refuse(
                    RefusalRule::Malformed,
                    format!("row {n}: invalid stream name {name:?}"),
                );
            }
        }
        let pi = format!("proIdiomEnable{n}");
        if let Some(v) = get(&pi) {
            if v != "on" {
                return refuse(
                    RefusalRule::Malformed,
                    format!("{pi} must be \"on\" when present"),
                );
            }
            expected.insert(pi);
        }
        expected.insert(url_key);
        expected.insert(name_key);
    }
    if get("btnSave") != Some("Save") {
        return refuse(RefusalRule::Malformed, "btnSave=Save is required");
    }
    session_value(get("session_id").unwrap_or(""))?;
    let names: BTreeSet<String> = draft.body.iter().map(|(k, _)| k.clone()).collect();
    if names != expected {
        let extra: Vec<_> = names.difference(&expected).cloned().collect();
        return refuse(
            RefusalRule::Malformed,
            format!("unexpected fields {extra:?}"),
        );
    }
    Ok(Request {
        kind: RequestKind::StatusEditPost,
        method: Method::Post,
        cgi: "statusedit.cgi",
        query: vec![],
        body: draft.body,
    })
}

fn screen_qam_post(draft: RequestDraft) -> Result<Request, Refusal> {
    let module = match draft.authority().map(|a| a.form()) {
        Some(WriteForm::QamModule(m)) => m,
        _ => {
            return refuse(
                RefusalRule::WriteOutsideProtocol,
                "qamconfig.cgi is posted only as a step of the four-module sequence",
            );
        }
    };
    exactly_once(&draft.body)?;
    let get = |k: &str| {
        draft
            .body
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    let need = |k: &str| {
        get(k).ok_or_else(|| Refusal {
            rule: RefusalRule::Malformed,
            detail: format!("{k} missing"),
        })
    };
    let slot = module_value(need("slotNumber")?)?;
    if slot != module {
        return refuse(
            RefusalRule::WriteOutsideProtocol,
            format!(
                "slotNumber {} does not match the step's module {}",
                slot.get(),
                module.get()
            ),
        );
    }
    let first = small_number(need("outputChannelA")?, "outputChannelA")?;
    if crate::types::FirstChannel::new(first).is_err() {
        return refuse(
            RefusalRule::Malformed,
            format!("outputChannelA {first} outside 2..155"),
        );
    }
    for l in ["A", "B", "C", "D"] {
        let k = format!("outputChannelControl{l}");
        let v = need(&k)?;
        if v != "0" && v != "1" {
            return refuse(RefusalRule::Malformed, format!("{k} must be 0 or 1"));
        }
    }
    if QamMode::from_form_value(small_number(need("QAMModeA")?, "QAMModeA")?).is_err() {
        return refuse(RefusalRule::Malformed, "QAMModeA out of range");
    }
    if QamMap::from_form_value(small_number(need("QAMMap")?, "QAMMap")?).is_err() {
        return refuse(RefusalRule::Malformed, "QAMMap out of range");
    }
    if need("qam_saved")? != "1" {
        return refuse(RefusalRule::Malformed, "qam_saved must be 1");
    }
    session_value(need("session_id")?)?;
    let mut expected: BTreeSet<&str> = [
        "slotNumber",
        "outputChannelA",
        "outputChannelControlA",
        "outputChannelControlB",
        "outputChannelControlC",
        "outputChannelControlD",
        "QAMModeA",
        "QAMMap",
        "qam_saved",
        "session_id",
    ]
    .into_iter()
    .collect();
    if module.get() == 1 {
        // The level select is enabled on module 1 only, so only module 1's form sends it.
        let lv = small_number(need("outputLevel")?, "outputLevel")?;
        if OutputLevel::new(lv).is_err() {
            return refuse(RefusalRule::Malformed, "outputLevel outside 28..41");
        }
        expected.insert("outputLevel");
    }
    let names: BTreeSet<&str> = draft.body.iter().map(|(k, _)| k.as_str()).collect();
    if names != expected {
        let extra: Vec<_> = names.difference(&expected).collect();
        return refuse(
            RefusalRule::Malformed,
            format!("unexpected or missing fields (extra {extra:?})"),
        );
    }
    Ok(Request {
        kind: RequestKind::QamModulePost(module),
        method: Method::Post,
        cgi: "qamconfig.cgi",
        query: vec![],
        body: draft.body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::WriteAuthority;
    use crate::types::Credentials;

    const SID: &str = "123456789";

    fn sid() -> SessionId {
        SessionId::parse(SID).unwrap()
    }

    fn rule(d: RequestDraft) -> RefusalRule {
        match screen(d) {
            Ok(r) => panic!("accepted: {r:?}"),
            Err(e) => e.rule,
        }
    }

    fn get(path: &str, q: &[(&str, &str)]) -> RequestDraft {
        RequestDraft::new(Method::Get, path, q, &[])
    }

    fn post(path: &str, b: &[(&str, &str)]) -> RequestDraft {
        RequestDraft::new(Method::Post, path, &[], b)
    }

    fn status_edit_body() -> Vec<(String, String)> {
        let mut b = Vec::new();
        for n in 1..=16 {
            b.push((format!("streamGroupAddress{n}"), String::new()));
            b.push((format!("streamName{n}"), String::new()));
        }
        b.push((String::from("session_id"), SID.to_owned()));
        b.push((String::from("btnSave"), String::from("Save")));
        b
    }

    fn qam_body(m: u8) -> Vec<(String, String)> {
        let mut b: Vec<(String, String)> = vec![
            ("slotNumber".into(), m.to_string()),
            ("outputChannelA".into(), "2".into()),
            ("outputChannelControlA".into(), "1".into()),
            ("outputChannelControlB".into(), "1".into()),
            ("outputChannelControlC".into(), "1".into()),
            ("outputChannelControlD".into(), "1".into()),
        ];
        if m == 1 {
            b.push(("outputLevel".into(), "40".into()));
        }
        b.extend([
            ("QAMModeA".into(), "1".into()),
            ("QAMMap".into(), "10".into()),
            ("qam_saved".into(), "1".into()),
            ("session_id".into(), SID.into()),
        ]);
        b
    }

    #[test]
    fn admin_is_refused_by_any_method_and_spelling() {
        let s = [("session_id", SID)];
        for p in [
            "admin.cgi",
            "/cgi-bin/admin.cgi",
            "admin_redir.cgi",
            "/cgi-bin/admin_redir.cgi",
            "ADMIN.CGI",
            "Admin.html",
            "/Admin.html",
            "%61dmin.cgi",
            "/cgi-bin/../cgi-bin/admin.cgi",
            "status.cgi/../admin.cgi",
        ] {
            assert_eq!(rule(get(p, &s)), RefusalRule::AdminPage, "{p}");
            assert_eq!(rule(get(p, &[])), RefusalRule::AdminPage, "{p}");
            assert_eq!(
                rule(post(p, &[("session_id", SID)])),
                RefusalRule::AdminPage,
                "{p}"
            );
        }
        // The dangerous GET actions themselves.
        for a in ["btnReboot", "btnFlush", "btnDeleteConfig", "commitConfig"] {
            assert_eq!(
                rule(get("admin.cgi", &[("session_id", SID), (a, "x")])),
                RefusalRule::AdminPage
            );
        }
    }

    #[test]
    fn any_btn_query_parameter_is_refused() {
        for page in [
            "status.cgi",
            "statusedit.cgi",
            "qamconfig.cgi",
            "errorlog.cgi",
            "diagnostic.cgi",
            "debug.cgi",
            "login.cgi",
            "eas.cgi",
            "nonexistent.cgi",
        ] {
            for b in [
                "btnReboot",
                "btnClear",
                "btnClearLog",
                "btnSave",
                "btnPing3",
                "BTNSAVE",
                "%62tnSave",
                "btn",
            ] {
                assert_eq!(
                    rule(get(page, &[("session_id", SID), (b, "1")])),
                    RefusalRule::ButtonInQuery,
                    "{page} {b}"
                );
            }
        }
        // Smuggled into the path.
        assert_eq!(
            rule(get("status.cgi?btnReboot=reboot", &[("session_id", SID)])),
            RefusalRule::ButtonInQuery
        );
        assert_eq!(
            rule(get("debug.cgi%3FbtnClear%3D1", &[("session_id", SID)])),
            RefusalRule::ButtonInQuery
        );
        // A btn query on a POST too.
        let mut d = post("statusedit.cgi", &[]);
        d.query.push(("btnSave".into(), "Save".into()));
        assert_eq!(rule(d), RefusalRule::ButtonInQuery);
    }

    #[test]
    fn logout_is_refused() {
        assert_eq!(
            rule(get("status.cgi", &[("logout", "1"), ("session_id", SID)])),
            RefusalRule::Logout
        );
        assert_eq!(
            rule(post("status.cgi", &[("logout", "1")])),
            RefusalRule::Logout
        );
    }

    #[test]
    fn config_page_is_refused() {
        assert_eq!(
            rule(get("config.cgi", &[("session_id", SID)])),
            RefusalRule::ConfigPage
        );
        assert_eq!(
            rule(post(
                "config.cgi",
                &[("fileConfig", "x"), ("btnSubmitConfig", "Submit")]
            )),
            RefusalRule::ConfigPage
        );
        assert_eq!(rule(get("/config_file.dat", &[])), RefusalRule::ConfigPage);
        assert_eq!(
            rule(get("/cgi-bin/config.cgi", &[])),
            RefusalRule::ConfigPage
        );
    }

    #[test]
    fn mode_writes_are_refused() {
        assert_eq!(
            rule(post(
                "mode.cgi",
                &[("tsMode", "0"), ("session_id", SID), ("btnSave", "Save")]
            )),
            RefusalRule::ModeWrite
        );
        assert_eq!(
            rule(post("mode.cgi", &[("tsMode", "1")])),
            RefusalRule::ModeWrite
        );
        // And mode.cgi is not even read.
        assert_eq!(
            rule(get("mode.cgi", &[("session_id", SID)])),
            RefusalRule::NotAllowListed
        );
    }

    #[test]
    fn other_writes_are_refused() {
        for p in [
            "eas.cgi",
            "license.cgi",
            "time.cgi",
            "network.cgi",
            "errorlog.cgi",
            "diagnostic.cgi",
            "debug.cgi",
            "streamconfig.cgi",
            "status.cgi",
            "streaminfo.cgi",
            "license_redir.cgi",
        ] {
            assert_eq!(
                rule(post(p, &[("session_id", SID), ("btnSave", "Save")])),
                RefusalRule::ForbiddenWrite,
                "{p}"
            );
            assert_eq!(rule(post(p, &[])), RefusalRule::ForbiddenWrite, "{p}");
        }
        // Clear Log / Clear Errors / EAS test as POSTs.
        assert_eq!(
            rule(post("errorlog.cgi", &[("btnClearLog", "Clear Log")])),
            RefusalRule::ForbiddenWrite
        );
        assert_eq!(
            rule(post("diagnostic.cgi", &[("btnClear", "Clear Errors")])),
            RefusalRule::ForbiddenWrite
        );
        assert_eq!(
            rule(post("eas.cgi", &[("easForceOn", "1")])),
            RefusalRule::ForbiddenWrite
        );
        // Network and login settings.
        assert_eq!(
            rule(post("network.cgi", &[("loginTimeout", "60")])),
            RefusalRule::ForbiddenWrite
        );
    }

    #[test]
    fn allowed_forms_without_authority_are_refused() {
        let body = status_edit_body();
        let mut d = post("statusedit.cgi", &[]);
        d.body = body.clone();
        assert_eq!(rule(d), RefusalRule::WriteOutsideProtocol);
        let mut d = post("qamconfig.cgi", &[]);
        d.body = qam_body(1);
        assert_eq!(rule(d), RefusalRule::WriteOutsideProtocol);
        // Authority for the other form, or another module, does not help.
        let d = RequestDraft::write_post(
            "statusedit.cgi",
            body,
            WriteAuthority::qam_module(RfModule::new(1).unwrap()),
        );
        assert_eq!(rule(d), RefusalRule::WriteOutsideProtocol);
        let d = RequestDraft::write_post(
            "qamconfig.cgi",
            qam_body(2),
            WriteAuthority::qam_module(RfModule::new(3).unwrap()),
        );
        assert_eq!(rule(d), RefusalRule::WriteOutsideProtocol);
        let d =
            RequestDraft::write_post("qamconfig.cgi", qam_body(2), WriteAuthority::status_edit());
        assert_eq!(rule(d), RefusalRule::WriteOutsideProtocol);
    }

    #[test]
    fn everything_else_is_not_allow_listed() {
        for p in [
            "cgi_demo",
            "/cgi-bin/cgi_demo",
            "index.html",
            "/",
            "",
            "../etc/passwd",
            "status.CGI",
            "status.cgi/",
            "/cgi-bin//status.cgi",
            "http://10.42.0.184/cgi-bin/status.cgi",
            "eas.cgi",
            "network.cgi",
            "time.cgi",
            "license.cgi",
            "streamconfig.cgi",
            "/qamchart.html",
            "status.cgi#x",
            "status.cgi%00",
        ] {
            assert_eq!(
                rule(get(p, &[("session_id", SID)])),
                RefusalRule::NotAllowListed,
                "{p:?}"
            );
        }
        assert_eq!(rule(post("unknown.cgi", &[])), RefusalRule::NotAllowListed);
    }

    #[test]
    fn malformed_reads_are_refused() {
        assert_eq!(rule(get("status.cgi", &[])), RefusalRule::Malformed);
        for odd in [" 123456789", "0123456789", "+123456789", "123456789 ", "0"] {
            assert_eq!(
                rule(get("status.cgi", &[("session_id", odd)])),
                RefusalRule::Malformed,
                "{odd:?}"
            );
        }
        assert_eq!(
            rule(get("status.cgi", &[("session_id", "abc")])),
            RefusalRule::Malformed
        );
        assert_eq!(
            rule(get(
                "status.cgi",
                &[("session_id", SID), ("boxNumber", "2")]
            )),
            RefusalRule::Malformed
        );
        assert_eq!(
            rule(get("qamconfig.cgi", &[("session_id", SID)])),
            RefusalRule::Malformed
        );
        assert_eq!(
            rule(get(
                "qamconfig.cgi",
                &[("session_id", SID), ("slotNumber", "5")]
            )),
            RefusalRule::Malformed
        );
        assert_eq!(
            rule(get(
                "qamconfig.cgi",
                &[("session_id", SID), ("slotNumber", "0")]
            )),
            RefusalRule::Malformed
        );
        assert_eq!(
            rule(get(
                "qamconfig.cgi",
                &[("slotNumber", "1"), ("session_id", SID)]
            )),
            RefusalRule::Malformed
        );
        assert_eq!(
            rule(get("login.cgi", &[("session_id", SID)])),
            RefusalRule::Malformed
        );
        let mut d = get("status.cgi", &[("session_id", SID)]);
        d.body.push(("x".into(), "y".into()));
        assert_eq!(rule(d), RefusalRule::Malformed);
    }

    #[test]
    fn malformed_writes_are_refused() {
        let auth = WriteAuthority::status_edit;
        // Truncated: row 16 missing.
        let mut b = status_edit_body();
        b.retain(|(k, _)| k != "streamGroupAddress16");
        assert_eq!(
            rule(RequestDraft::write_post("statusedit.cgi", b, auth())),
            RefusalRule::Malformed
        );
        // Name with a defect-B delimiter.
        let mut b = status_edit_body();
        b[0].1 = "UDP://239.1.1.1:5000".into();
        b[1].1 = "ESPN-HD".into();
        assert_eq!(
            rule(RequestDraft::write_post("statusedit.cgi", b, auth())),
            RefusalRule::Malformed
        );
        // URL set, name empty.
        let mut b = status_edit_body();
        b[0].1 = "UDP://239.1.1.1:5000".into();
        assert_eq!(
            rule(RequestDraft::write_post("statusedit.cgi", b, auth())),
            RefusalRule::Malformed
        );
        // Source fields (disabled on the page) and unknown fields.
        for extra in ["streamSourceAddress1A", "foo", "tsMode"] {
            let mut b = status_edit_body();
            b.push((extra.into(), String::new()));
            assert_eq!(
                rule(RequestDraft::write_post("statusedit.cgi", b, auth())),
                RefusalRule::Malformed,
                "{extra}"
            );
        }
        // Duplicate field.
        let mut b = status_edit_body();
        b.push(("streamName1".into(), String::new()));
        assert_eq!(
            rule(RequestDraft::write_post("statusedit.cgi", b, auth())),
            RefusalRule::Malformed
        );
        // QAM: CW checkbox, missing level on module 1, level on module 2, bad values.
        let m1 = || WriteAuthority::qam_module(RfModule::new(1).unwrap());
        let m2 = || WriteAuthority::qam_module(RfModule::new(2).unwrap());
        let mut b = qam_body(1);
        b.push(("chkCW1".into(), "on".into()));
        assert_eq!(
            rule(RequestDraft::write_post("qamconfig.cgi", b, m1())),
            RefusalRule::Malformed
        );
        let mut b = qam_body(1);
        b.retain(|(k, _)| k != "outputLevel");
        assert_eq!(
            rule(RequestDraft::write_post("qamconfig.cgi", b, m1())),
            RefusalRule::Malformed
        );
        let mut b = qam_body(2);
        b.push(("outputLevel".into(), "40".into()));
        assert_eq!(
            rule(RequestDraft::write_post("qamconfig.cgi", b, m2())),
            RefusalRule::Malformed
        );
        for (k, v) in [
            ("outputChannelA", "156"),
            ("outputChannelA", "1"),
            ("QAMModeA", "7"),
            ("QAMMap", "13"),
            ("qam_saved", "0"),
            ("outputChannelControlB", "2"),
        ] {
            let mut b = qam_body(2);
            for f in &mut b {
                if f.0 == k {
                    f.1 = v.into();
                }
            }
            assert_eq!(
                rule(RequestDraft::write_post("qamconfig.cgi", b, m2())),
                RefusalRule::Malformed,
                "{k}={v}"
            );
        }
        // A write post with a query string.
        let mut d = RequestDraft::write_post("qamconfig.cgi", qam_body(2), m2());
        d.query.push(("session_id".into(), SID.into()));
        assert_eq!(rule(d), RefusalRule::Malformed);
    }

    #[test]
    fn the_allow_list_passes() {
        let s = sid();
        for page in [
            ReadPage::Status,
            ReadPage::StatusEdit,
            ReadPage::StreamInfo,
            ReadPage::Diagnostic,
            ReadPage::Debug,
            ReadPage::EventLog,
        ] {
            let r = screen(RequestDraft::read(page, &s)).unwrap();
            assert_eq!(r.kind(), RequestKind::Read(page));
            assert_eq!(
                r.target(),
                format!("/cgi-bin/{}?session_id={SID}", page.cgi())
            );
            assert!(r.body().is_none());
            assert!(r.retryable());
        }
        for m in RfModule::all() {
            let r = screen(RequestDraft::read(ReadPage::QamConfig(m), &s)).unwrap();
            assert_eq!(
                r.target(),
                format!("/cgi-bin/qamconfig.cgi?session_id={SID}&slotNumber={m}")
            );
        }
        let r = screen(RequestDraft::login_page()).unwrap();
        assert_eq!(r.target(), "/cgi-bin/login.cgi");
        let c = Credentials::new("Admin", "pass").unwrap();
        let r = screen(RequestDraft::login_post(&c, &s)).unwrap();
        assert_eq!(r.kind(), RequestKind::LoginPost);
        assert!(!r.retryable());
        assert_eq!(
            String::from_utf8(r.body().unwrap()).unwrap(),
            format!("txtUserName=Admin&txtPassword=pass&session_id={SID}&btnSubmit=Submit")
        );
        assert!(!format!("{r:?}").contains("pass\""));
        let r = screen(RequestDraft::write_post(
            "statusedit.cgi",
            status_edit_body(),
            WriteAuthority::status_edit(),
        ))
        .unwrap();
        assert!(r.kind().is_write());
        assert!(!r.retryable());
        assert_eq!(r.target(), "/cgi-bin/statusedit.cgi");
        for m in RfModule::all() {
            let r = screen(RequestDraft::write_post(
                "qamconfig.cgi",
                qam_body(m.get()),
                WriteAuthority::qam_module(m),
            ))
            .unwrap();
            assert_eq!(r.kind(), RequestKind::QamModulePost(m));
        }
    }

    /// Every combination of path, method, query and body from a vocabulary of the device's
    /// pages and parameters: whatever is accepted without write authority is a read of an
    /// allowed page with only `session_id` (and `slotNumber`), or the login page.
    #[test]
    fn exhaustive_combinations_accept_only_the_allow_list() {
        let pages = [
            "admin.cgi",
            "admin_redir.cgi",
            "config.cgi",
            "mode.cgi",
            "eas.cgi",
            "license.cgi",
            "time.cgi",
            "network.cgi",
            "errorlog.cgi",
            "diagnostic.cgi",
            "debug.cgi",
            "streamconfig.cgi",
            "status.cgi",
            "statusedit.cgi",
            "qamconfig.cgi",
            "streaminfo.cgi",
            "login.cgi",
            "cgi_demo",
            "Admin.html",
        ];
        let prefixes = ["", "/cgi-bin/", "/", "cgi-bin/", "/cgi-bin/../cgi-bin/"];
        let params: [(&str, &str); 9] = [
            ("session_id", SID),
            ("slotNumber", "2"),
            ("boxNumber", "2"),
            ("btnSave", "Save"),
            ("btnReboot", "reboot"),
            ("logout", "1"),
            ("tsMode", "0"),
            ("commitConfig", "1"),
            ("qam_saved", "1"),
        ];
        let mut accepted = 0;
        for page in pages {
            for prefix in prefixes {
                let path = format!("{prefix}{page}");
                for method in [Method::Get, Method::Post] {
                    // Every subset of the parameters, as query or as body.
                    for mask in 0u32..(1 << params.len()) {
                        let set: Vec<(&str, &str)> = params
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| mask & (1 << i) != 0)
                            .map(|(_, p)| *p)
                            .collect();
                        for as_body in [false, true] {
                            if as_body && set.is_empty() {
                                continue; // the same request as the empty query
                            }
                            let d = if as_body {
                                RequestDraft::new(method, &path, &[], &set)
                            } else {
                                RequestDraft::new(method, &path, &set, &[])
                            };
                            let Ok(r) = screen(d) else { continue };
                            accepted += 1;
                            assert_eq!(method, Method::Get, "{path} {set:?}");
                            assert!(!as_body);
                            assert!(prefix.is_empty() || prefix == "/cgi-bin/");
                            match r.kind() {
                                RequestKind::Read(ReadPage::QamConfig(_)) => {
                                    assert_eq!(set, [("session_id", SID), ("slotNumber", "2")]);
                                }
                                RequestKind::Read(_) => assert_eq!(set, [("session_id", SID)]),
                                RequestKind::LoginPage => assert!(set.is_empty()),
                                k => panic!("{k:?} accepted from {path} {set:?}"),
                            }
                        }
                    }
                }
            }
        }
        // 6 read pages + qamconfig + the login page, under two spellings of the path.
        assert_eq!(accepted, 16);
    }
}
