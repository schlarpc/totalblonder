//! An offline stand-in for the modulator: it renders the saved pages with its own state
//! substituted, applies the two allowed form posts the way the firmware does (a row missing
//! from a `statusedit.cgi` POST is zeroed), checks the session id, and records every request.
#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper: failures fail the test"
)]

use std::time::Duration;

use tb_eqam::{Method, Request, Response, TransportFailure};

pub const SID: &str = "123456789";

pub const LOGIN: &[u8] = include_bytes!("../fixtures/login.cgi.html");
pub const LOGIN_POST: &[u8] = include_bytes!("../fixtures/login_post.html");
pub const LOGGED_OUT: &[u8] = include_bytes!("../fixtures/synthetic/logged_out.html");
pub const STATUS: &[u8] = include_bytes!("../fixtures/status.cgi.html");
pub const STATUS_EDIT: &[u8] = include_bytes!("../fixtures/statusedit.cgi.html");
pub const QAM: [&[u8]; 4] = [
    include_bytes!("../fixtures/qamconfig.cgi_slot1.html"),
    include_bytes!("../fixtures/qamconfig.cgi_slot2.html"),
    include_bytes!("../fixtures/qamconfig.cgi_slot3.html"),
    include_bytes!("../fixtures/qamconfig.cgi_slot4.html"),
];
pub const STREAM_INFO: &[u8] = include_bytes!("../fixtures/streaminfo.cgi.html");
pub const DIAGNOSTIC: &[u8] = include_bytes!("../fixtures/diagnostic.cgi.html");
pub const DEBUG: &[u8] = include_bytes!("../fixtures/debug.cgi.html");
pub const EVENT_LOG: &[u8] = include_bytes!("../fixtures/errorlog.cgi.html");

/// Copy of the device table, for deriving carriers B-D.
const NEXT: [u8; 157] = [
    3, 4, 5, 6, 95, 8, 9, 10, 11, 12, 13, 23, 15, 16, 17, 18, 19, 20, 21, 22, 7, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51,
    52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75,
    76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 100, 96, 97, 98,
    99, 14, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117,
    118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136,
    137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155,
    156, 157, 158, 0,
];

pub fn carriers(first: u8) -> [u8; 4] {
    let b = NEXT[usize::from(first - 2)];
    let c = NEXT[usize::from(b - 2)];
    let d = NEXT[usize::from(c - 2)];
    [first, b, c, d]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub first: u8,
    pub on: [bool; 4],
    pub qam: u8,
    pub map: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logged {
    pub method: Method,
    pub target: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct FakeDevice {
    pub logged_in: bool,
    pub rows: Vec<(String, String)>,
    pub modules: Vec<Module>,
    pub level: u8,
    pub mpts: bool,
    pub uptime: Duration,
    pub log: Vec<Logged>,
    /// Posts are accepted but change nothing.
    pub ignore_writes: bool,
    /// Event-log lines, newest first: (color, text).
    pub event_lines: Vec<(String, String)>,
    /// Transport failures to return for the next requests.
    pub failures: Vec<TransportFailure>,
    /// Log the session out after this many more requests.
    pub logout_after: Option<usize>,
    /// Extra HTML inserted into the statusedit form.
    pub statusedit_extra: String,
    /// Show module 4's channels as `-` on the status page.
    pub module4_unknown: bool,
    /// Render slot 3's Pro:Idiom box checked and disabled.
    pub pro_idiom3_checked_disabled: bool,
}

impl FakeDevice {
    /// The unit as surveyed: no inputs, the live RF plan.
    pub fn surveyed() -> Self {
        let m = |first| Module {
            first,
            on: [true; 4],
            qam: 1,
            map: 10,
        };
        FakeDevice {
            logged_in: false,
            rows: vec![(String::new(), String::new()); 16],
            modules: vec![m(2), m(10), m(6), m(140)],
            level: 40,
            mpts: true,
            uptime: Duration::from_secs(6 * 86400),
            log: vec![],
            ignore_writes: false,
            event_lines: vec![
                ("#FF0000".into(), "Thu Jan  1 00:00:20 1970 : ERROR: Difference detected in Config File versus current system settings.  Correcting differences.".into()),
                ("#0000FF".into(), "Thu Jan  1 00:00:20 1970 : Power Up Started".into()),
            ],
            failures: vec![],
            logout_after: None,
            statusedit_extra: String::new(),
            module4_unknown: false,
            pro_idiom3_checked_disabled: false,
        }
    }

    pub fn posts(&self) -> Vec<&Logged> {
        self.log
            .iter()
            .filter(|l| l.method == Method::Post && !l.target.contains("login"))
            .collect()
    }

    pub fn targets(&self) -> Vec<String> {
        self.log
            .iter()
            .map(|l| {
                format!(
                    "{} {}",
                    if l.method == Method::Get {
                        "GET"
                    } else {
                        "POST"
                    },
                    short(&l.target)
                )
            })
            .collect()
    }

    pub fn handle(&mut self, req: &Request) -> Result<Response, TransportFailure> {
        let target = req.target();
        let fields: Vec<(String, String)> = req.fields().to_vec();
        self.log.push(Logged {
            method: req.method(),
            target: target.clone(),
            fields: fields.clone(),
        });
        if !self.failures.is_empty() {
            return Err(self.failures.remove(0));
        }
        if let Some(n) = self.logout_after.as_mut() {
            if *n == 0 {
                self.logged_in = false;
                self.logout_after = None;
            } else {
                *n -= 1;
            }
        }
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let q: Vec<(&str, &str)> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect();
        let qget = |k: &str| q.iter().find(|(a, _)| *a == k).map(|(_, v)| *v);
        let fget = |k: &str| fields.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let page = path.trim_start_matches("/cgi-bin/");
        if page == "login.cgi" {
            return Ok(match req.method() {
                Method::Get => Response::ok(LOGIN),
                Method::Post => {
                    self.logged_in = fget("session_id").as_deref() == Some(SID);
                    Response::ok(LOGIN_POST)
                }
            });
        }
        let sid = match req.method() {
            Method::Get => qget("session_id").map(str::to_owned),
            Method::Post => fget("session_id"),
        };
        if !self.logged_in || sid.as_deref() != Some(SID) {
            return Ok(Response::ok(LOGGED_OUT));
        }
        let body = match (req.method(), page) {
            (Method::Get, "status.cgi") => self.status(),
            (Method::Get, "statusedit.cgi") => self.statusedit(),
            (Method::Get, "qamconfig.cgi") => {
                let m: usize = qget("slotNumber").and_then(|v| v.parse().ok()).unwrap_or(1);
                self.qam(m)
            }
            (Method::Get, "streaminfo.cgi") => self.header(STREAM_INFO),
            (Method::Get, "diagnostic.cgi") => self.header(DIAGNOSTIC),
            (Method::Get, "debug.cgi") => self.debug(),
            (Method::Get, "errorlog.cgi") => self.eventlog(),
            (Method::Post, "statusedit.cgi") => {
                if !self.ignore_writes {
                    for n in 1..=16 {
                        // The CGI zeroes any stream whose address field is absent.
                        let url = fget(&format!("streamGroupAddress{n}")).unwrap_or_default();
                        let name = fget(&format!("streamName{n}")).unwrap_or_default();
                        self.rows[n - 1] = (url, name);
                    }
                }
                self.statusedit()
            }
            (Method::Post, "qamconfig.cgi") => {
                let m: usize = fget("slotNumber").and_then(|v| v.parse().ok()).unwrap_or(1);
                if !self.ignore_writes && fget("qam_saved").as_deref() == Some("1") {
                    let md = &mut self.modules[m - 1];
                    md.first = fget("outputChannelA")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(md.first);
                    for (i, l) in ["A", "B", "C", "D"].iter().enumerate() {
                        md.on[i] =
                            fget(&format!("outputChannelControl{l}")).as_deref() == Some("1");
                    }
                    md.qam = fget("QAMModeA")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(md.qam);
                    md.map = fget("QAMMap")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(md.map);
                    if let Some(l) = fget("outputLevel").and_then(|v| v.parse().ok()) {
                        self.level = l;
                    }
                }
                self.qam(m)
            }
            _ => panic!("fake device: unexpected {target}"),
        };
        Ok(Response::ok(body.into_bytes()))
    }

    fn header(&self, page: &[u8]) -> String {
        let mut s = String::from_utf8_lossy(page).into_owned();
        let u = self.uptime.as_secs();
        let up = format!(
            "Uptime: {}d {}h {}m {}s</td>",
            u / 86400,
            u % 86400 / 3600,
            u % 3600 / 60,
            u % 60
        );
        if let Some(i) = s.find("Uptime: ") {
            let j = s[i..].find("</td>").map(|j| i + j + 5).unwrap();
            s.replace_range(i..j, &up);
        }
        if !self.mpts {
            s = s.replace("Mode: MPTS", "Mode: SPTS");
        }
        s
    }

    fn status(&self) -> String {
        let mut s = self.header(STATUS);
        s = s.replace("(40dBmV)", &format!("({}dBmV)", self.level));
        for n in 1..=16usize {
            let marker = format!("height=\"30\">{n}</td>");
            let i = s.find(&marker).unwrap();
            let (head, tail) = s.split_at(i);
            let mut tail = tail.to_owned();
            let (url, name) = &self.rows[n - 1];
            if !url.is_empty() {
                tail = tail.replacen("No IP Assigned</td>", &format!("\n{url}</td>"), 1);
                tail = tail.replacen(
                    "<td rowspan=\"2\" align=\"center\" bgcolor=\"#C0C0C0\"></td>",
                    &format!("<td rowspan=\"2\" align=\"center\" bgcolor=\"#C0C0C0\">{name}</td>"),
                    1,
                );
                tail = tail.replacen(
                    "<td rowspan=\"2\" bgcolor=\"#C0C0C0\"></td>",
                    "<td rowspan=\"2\" bgcolor=\"#FF0000\"></td>",
                    1,
                );
            }
            let m = &self.modules[(n - 1) / 4];
            let k = (n - 1) % 4;
            let orig = carriers(match (n - 1) / 4 {
                0 => 2,
                1 => 10,
                2 => 6,
                _ => 140,
            })[k];
            let ch = if self.module4_unknown && n > 12 {
                String::from("-")
            } else {
                carriers(m.first)[k].to_string()
            };
            let on = if m.on[k] { "On" } else { "Off" };
            tail = tail.replacen(
                &format!(">{orig} / On</td>"),
                &format!(">{ch} / {on}</td>"),
                1,
            );
            s = format!("{head}{tail}");
        }
        s
    }

    fn statusedit(&self) -> String {
        let mut s = String::from_utf8_lossy(STATUS_EDIT).into_owned();
        for n in 1..=16 {
            let (url, name) = &self.rows[n - 1];
            s = s.replace(
                &format!("name=\"streamGroupAddress{n}\" maxlength=\"27\" size=\"30\" value=\"\""),
                &format!(
                    "name=\"streamGroupAddress{n}\" maxlength=\"27\" size=\"30\" value=\"{url}\""
                ),
            );
            s = s.replace(
                &format!("name=\"streamName{n}\" maxlength=\"10\" size=\"12\" value=\"\""),
                &format!("name=\"streamName{n}\" maxlength=\"10\" size=\"12\" value=\"{name}\""),
            );
        }
        if self.pro_idiom3_checked_disabled {
            s = s.replace(
                "<input  name=\"proIdiomEnable3\"",
                "<input disabled checked=\"true\" name=\"proIdiomEnable3\"",
            );
        }
        if !self.statusedit_extra.is_empty() {
            s = s.replace(
                "<input name=\"session_id\" value=\"\" type=\"hidden\">",
                &format!(
                    "<input name=\"session_id\" value=\"\" type=\"hidden\">{}",
                    self.statusedit_extra
                ),
            );
        }
        s
    }

    fn qam(&self, m: usize) -> String {
        let mut s = self.header(QAM[m - 1]);
        let md = &self.modules[m - 1];
        let ch = carriers(md.first);
        for (i, l) in ["A", "B", "C", "D"].iter().enumerate() {
            s = select(&s, &format!("outputChannel{l}"), &ch[i].to_string());
            s = select(
                &s,
                &format!("outputChannelControl{l}"),
                if md.on[i] { "1" } else { "0" },
            );
        }
        s = select(&s, "QAMModeA", &md.qam.to_string());
        s = select(&s, "QAMMap", &md.map.to_string());
        select(&s, "outputLevel", &self.level.to_string())
    }

    fn debug(&self) -> String {
        let mut s = String::from_utf8_lossy(DEBUG).into_owned();
        let u = self.uptime.as_secs();
        s = s.replace(
            "6d 18h 24m 0s",
            &format!(
                "{}d {}h {}m {}s",
                u / 86400,
                u % 86400 / 3600,
                u % 3600 / 60,
                u % 60
            ),
        );
        s
    }

    fn eventlog(&self) -> String {
        let s = String::from_utf8_lossy(EVENT_LOG).into_owned();
        let start = s
            .find("<td colspan=\"2\"><font color=\"#FF0000\">Thu Jan")
            .unwrap();
        let start = s[..start].rfind("<tr>").unwrap();
        let end = start + s[start..].find("</table>").unwrap();
        let mut rows = String::new();
        for (c, t) in &self.event_lines {
            rows.push_str(&format!(
                "<tr><td colspan=\"2\"><font color=\"{c}\">{t}\n</font></td></tr>\n"
            ));
        }
        format!("{}{}{}", &s[..start], rows, &s[end..])
    }
}

/// Move `selected` in the select named `name` to the option with `value`.
pub fn select(html: &str, name: &str, value: &str) -> String {
    let key = format!("name=\"{name}\"");
    let Some(i) = html.find(&key) else {
        return html.to_owned();
    };
    let j = html[i..].find("</select>").map(|j| i + j).unwrap();
    let seg = html[i..j].replace("<option selected ", "<option ");
    let want = format!("value=\"{value}\">");
    let seg = seg.replacen(&want, &format!("selected value=\"{value}\">"), 1);
    format!("{}{}{}", &html[..i], seg, &html[j..])
}

fn short(target: &str) -> String {
    let (p, q) = target.split_once('?').unwrap_or((target, ""));
    let p = p.trim_start_matches("/cgi-bin/");
    match q.split('&').find(|kv| kv.starts_with("slotNumber=")) {
        Some(s) => format!("{p}?{s}"),
        None => p.to_owned(),
    }
}

/// Drive a step machine against the fake until it finishes.
pub fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}
