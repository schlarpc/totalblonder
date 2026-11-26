//! The two allowed writes, as pure step machines (next request ← previous response).
//!
//! Both follow SPEC §10.1's protocol: fetch the form, check its field-name set is exactly the
//! expected one (abort otherwise), change only the intended values, post every field back the
//! way a browser would, fetch again and compare.
//!
//! * `StreamTableWrite` posts `statusedit.cgi` with all 16 rows every time (the CGI zeroes
//!   rows missing from a POST), then reads `statusedit.cgi` and `status.cgi` back.
//! * `RfPlanWrite` is the only way to post `qamconfig.cgi`: it checks the page header says
//!   MPTS, then saves modules 1, 2, 3, 4 in that order every time (defect A mitigation,
//!   SPEC §10.3), then reads all four pages and the status page's RF column back.
//!
//! Both machines are private to the crate. From outside, a write exists only as a whole job
//! ([`crate::client::Job::WriteStreams`], [`crate::client::Job::WriteRfPlan`]) run by the
//! client, and the post authority for a module exists only inside `RfPlanWrite`, so a
//! single-module save is not expressible. (A transport that invents responses can still make
//! the client skip steps; no sans-io design can prevent a lying adapter.)
//!
//! ```compile_fail
//! // The step machines cannot be driven by hand from outside the crate.
//! let s = tb_eqam::types::SessionId::parse("123").unwrap();
//! let plan: tb_eqam::plan::RfPlan = todo!();
//! let (_m, _step) = tb_eqam::write::RfPlanWrite::start(plan, s);
//! ```
//!
//! "Every field" means every control a browser submits: enabled controls with a name, checked
//! boxes, the selected option of each select, and the clicked submit button. Disabled fields
//! (`streamSourceAddress*`, `outputChannelB..D`, `outputLevel` on modules 2–4) are not sent,
//! exactly as the vendor's page does not send them.

use crate::html::{ControlKind, Form};
use crate::pages::ParseError;
use crate::pages::qamconfig::QamConfigPage;
use crate::pages::status::{CarrierState, StatusPage, StreamCell};
use crate::pages::statusedit::{FieldSetMismatch, StatusEditPage};
use crate::plan::{PlanError, RfPlan, SlotConfig, StreamTable, check_unique_channels};
use crate::refusal::{Refusal, Request, screen};
use crate::request::{ReadPage, RequestDraft, Response, TransportFailure, WriteAuthority};
use crate::types::{RfModule, SessionId, Slot, TsMode};

/// Which form a write was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WriteKind {
    /// `statusedit.cgi`.
    StreamTable,
    /// `qamconfig.cgi`, four modules.
    RfPlan,
}

impl WriteKind {
    /// Label for `device_writes_total{form}`.
    pub fn as_str(self) -> &'static str {
        match self {
            WriteKind::StreamTable => "statusedit",
            WriteKind::RfPlan => "qamconfig",
        }
    }
}

/// One value that read back differently from what was written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mismatch {
    /// Where (`slot 3 url`, `module 2 carrier B on`, …).
    pub what: String,
    /// What was written.
    pub expected: String,
    /// What the device shows.
    pub observed: String,
}

/// Why a write stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// The session ended.
    #[error("logged out")]
    LoggedOut,
    /// No response.
    #[error("transport: {0}")]
    Transport(TransportFailure),
    /// A non-200 status.
    #[error("HTTP status {0}")]
    Status(u16),
    /// A page did not parse.
    #[error("parse: {0}")]
    Parse(ParseError),
    /// The form's fields are not the expected set.
    #[error("{0}")]
    FieldSet(FieldSetMismatch),
    /// The page header does not say MPTS.
    #[error("the device is not in MPTS mode")]
    NotMpts,
    /// Saving the modules in order would put two carriers on one channel in between.
    #[error("intermediate state: {0}")]
    IntermediateDuplicate(PlanError),
    /// A value to post is not among the select's options.
    #[error("{field}: value {value:?} is not offered by the form")]
    NotOffered {
        /// Field.
        field: String,
        /// Value.
        value: String,
    },
    /// A request was refused by the screen (a bug in this crate if it happens).
    #[error("{0}")]
    Refused(Refusal),
    /// The module page reports a communication error with the QAM module.
    #[error("module {0}: error communicating with the QAM module")]
    ModuleCommunication(u8),
    /// The read-back differs.
    #[error("read-back differs in {} values", .0.len())]
    ReadBack(Vec<Mismatch>),
    /// Posting the form as a browser would would also change this field (a checked box
    /// rendered disabled is not submitted, so the CGI would clear it).
    #[error("posting would also change {0}")]
    WouldChange(String),
    /// A module's current channels are not shown on the status page, so the intermediate
    /// states of the four-module sequence cannot be checked for duplicates.
    #[error("current channels of module {0} are unknown")]
    UnknownCurrentChannels(u8),
    /// The client dropped the write between steps (not reachable in the current code; kept
    /// so that such a path reports instead of losing the write).
    #[error("abandoned between steps")]
    Abandoned,
}

/// How a write ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Posted and read back as intended.
    Verified,
    /// The form already held the intended values; nothing was posted.
    AlreadyInPlace,
    /// Stopped before anything was posted: the device is unchanged.
    Aborted(WriteError),
    /// Stopped after at least one post: the device may be partly changed.
    Failed(WriteError),
}

/// The report of a finished write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    /// Which form.
    pub kind: WriteKind,
    /// Result.
    pub outcome: WriteOutcome,
    /// Number of POSTs sent.
    pub posts: u32,
    /// Values before (`what = value` lines from the first read).
    pub before: Vec<String>,
    /// Values intended.
    pub after: Vec<String>,
    /// The title of each POST response (what the CGI answers with is not known from the
    /// survey; recorded for the lead).
    pub post_responses: Vec<String>,
}

/// What the machine wants next.
#[derive(Debug)]
pub(crate) enum WriteStep {
    /// Send this request and feed its result to `on_response`.
    Send(Request),
    /// Finished.
    Done(WriteReport),
}

fn body_ok<'a>(r: Result<&'a Response, &TransportFailure>) -> Result<&'a [u8], WriteError> {
    match r {
        Err(e) => Err(WriteError::Transport(e.clone())),
        Ok(resp) if resp.status != 200 => Err(WriteError::Status(resp.status)),
        Ok(resp) => Ok(&resp.body),
    }
}

fn parse_err(e: ParseError) -> WriteError {
    match e {
        ParseError::LoggedOut => WriteError::LoggedOut,
        other => WriteError::Parse(other),
    }
}

/// The body a browser would submit for `form`, with `overrides` applied to named fields,
/// `drop` fields left out, and `submit` added as the clicked button. Every overridden select
/// value must be one the select offers.
fn browser_body(
    form: &Form,
    overrides: &[(String, String)],
    drop: &[&str],
    submit: Option<&str>,
) -> Result<Vec<(String, String)>, WriteError> {
    let mut out = Vec::new();
    for c in &form.controls {
        if drop.contains(&c.name.as_str()) {
            continue;
        }
        if let ControlKind::Submit { value } = &c.kind {
            if Some(c.name.as_str()) == submit && !c.disabled {
                out.push((c.name.clone(), value.clone()));
            }
            continue;
        }
        let ov = overrides
            .iter()
            .find(|(k, _)| *k == c.name)
            .map(|(_, v)| v.clone());
        let value = match ov {
            Some(v) => {
                if c.disabled {
                    // Never resurrect a field the page does not submit.
                    continue;
                }
                if let ControlKind::Select { options } = &c.kind
                    && !options.iter().any(|o| o.value == v)
                {
                    return Err(WriteError::NotOffered {
                        field: c.name.clone(),
                        value: v,
                    });
                }
                if let ControlKind::Text {
                    maxlength: Some(m), ..
                } = &c.kind
                    && v.chars().count() > *m
                {
                    return Err(WriteError::NotOffered {
                        field: c.name.clone(),
                        value: v,
                    });
                }
                Some(v)
            }
            None => c.successful_value(),
        };
        if let Some(v) = value {
            out.push((c.name.clone(), v));
        }
    }
    Ok(out)
}

#[derive(Debug)]
enum TablePhase {
    FetchForm,
    Post,
    VerifyForm,
    VerifyStatus,
    Finished,
}

/// The stream-table write: all 16 rows of `statusedit.cgi`.
#[derive(Debug)]
pub(crate) struct StreamTableWrite {
    desired: StreamTable,
    session: SessionId,
    phase: TablePhase,
    report: WriteReport,
    /// Pro:Idiom boxes as first read, to check they survive the post.
    pro_idiom_before: Vec<bool>,
}

impl StreamTableWrite {
    /// Start a write of `desired` with `session`; returns the machine and its first step.
    pub(crate) fn start(desired: StreamTable, session: SessionId) -> (Self, WriteStep) {
        let after = Slot::all()
            .map(|s| {
                let c = desired.get(s);
                format!("slot {s} = {:?} {:?}", c.url_text(), c.name_text())
            })
            .collect();
        let mut m = StreamTableWrite {
            desired,
            session,
            pro_idiom_before: vec![],
            phase: TablePhase::FetchForm,
            report: WriteReport {
                kind: WriteKind::StreamTable,
                outcome: WriteOutcome::Verified,
                posts: 0,
                before: vec![],
                after,
                post_responses: vec![],
            },
        };
        let step = m.read(ReadPage::StatusEdit);
        (m, step)
    }

    fn read(&mut self, page: ReadPage) -> WriteStep {
        match screen(RequestDraft::read(page, &self.session)) {
            Ok(r) => WriteStep::Send(r),
            Err(e) => self.stop(WriteError::Refused(e)),
        }
    }

    fn stop(&mut self, e: WriteError) -> WriteStep {
        self.phase = TablePhase::Finished;
        self.report.outcome = if self.report.posts == 0 {
            WriteOutcome::Aborted(e)
        } else {
            WriteOutcome::Failed(e)
        };
        WriteStep::Done(self.report.clone())
    }

    fn finish(&mut self, outcome: WriteOutcome) -> WriteStep {
        self.phase = TablePhase::Finished;
        self.report.outcome = outcome;
        WriteStep::Done(self.report.clone())
    }

    /// End the write without another step (reports `Abandoned`).
    pub(crate) fn abandon(&mut self) -> WriteReport {
        match self.stop(WriteError::Abandoned) {
            WriteStep::Done(r) => r,
            WriteStep::Send(_) => self.report.clone(),
        }
    }

    /// Feed the result of the last request.
    pub(crate) fn on_response(
        &mut self,
        result: Result<&Response, &TransportFailure>,
    ) -> WriteStep {
        let body = match body_ok(result) {
            Ok(b) => b,
            Err(e) => return self.stop(e),
        };
        match self.phase {
            TablePhase::FetchForm => {
                let page = match StatusEditPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                if let Err(e) = page.check_fields() {
                    return self.stop(WriteError::FieldSet(e));
                }
                if let Some(c) = page.form().controls.iter().find(|c| {
                    c.disabled && matches!(c.kind, ControlKind::Checkbox { checked: true, .. })
                }) {
                    return self.stop(WriteError::WouldChange(c.name.clone()));
                }
                self.pro_idiom_before = page.rows.iter().map(|r| r.pro_idiom).collect();
                self.report.before = page
                    .rows
                    .iter()
                    .map(|r| format!("slot {} = {:?} {:?}", r.slot, r.url_text, r.name_text))
                    .collect();
                let in_place = page.rows.iter().all(|r| {
                    let d = self.desired.get(r.slot);
                    r.url_text == d.url_text() && r.name_text == d.name_text()
                });
                if in_place {
                    return self.finish(WriteOutcome::AlreadyInPlace);
                }
                let mut overrides = vec![(String::from("session_id"), self.session.wire())];
                for s in Slot::all() {
                    let d = self.desired.get(s);
                    overrides.push((format!("streamGroupAddress{s}"), d.url_text()));
                    overrides.push((format!("streamName{s}"), d.name_text()));
                }
                let body = match browser_body(page.form(), &overrides, &[], Some("btnSave")) {
                    Ok(b) => b,
                    Err(e) => return self.stop(e),
                };
                match screen(RequestDraft::write_post(
                    "statusedit.cgi",
                    body,
                    WriteAuthority::status_edit(),
                )) {
                    Ok(r) => {
                        self.phase = TablePhase::Post;
                        self.report.posts += 1;
                        WriteStep::Send(r)
                    }
                    Err(e) => self.stop(WriteError::Refused(e)),
                }
            }
            TablePhase::Post => {
                let doc = crate::html::Document::parse(body);
                let class = crate::pages::classify(&doc);
                self.report.post_responses.push(format!("{class:?}"));
                match class {
                    crate::pages::PageClass::LoggedOut => return self.stop(WriteError::LoggedOut),
                    crate::pages::PageClass::ModulatorError => {
                        return self.stop(WriteError::Parse(ParseError::ModulatorError));
                    }
                    _ => {}
                }
                self.phase = TablePhase::VerifyForm;
                self.read(ReadPage::StatusEdit)
            }
            TablePhase::VerifyForm => {
                let page = match StatusEditPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                let mut mm = Vec::new();
                for r in &page.rows {
                    let d = self.desired.get(r.slot);
                    if r.url_text != d.url_text() {
                        mm.push(Mismatch {
                            what: format!("slot {} url", r.slot),
                            expected: d.url_text(),
                            observed: r.url_text.clone(),
                        });
                    }
                    if r.name_text != d.name_text() {
                        mm.push(Mismatch {
                            what: format!("slot {} name", r.slot),
                            expected: d.name_text(),
                            observed: r.name_text.clone(),
                        });
                    }
                    let before = self.pro_idiom_before.get(r.slot.index()).copied();
                    if before != Some(r.pro_idiom) {
                        mm.push(Mismatch {
                            what: format!("slot {} Pro:Idiom", r.slot),
                            expected: format!("{before:?}"),
                            observed: format!("{:?}", Some(r.pro_idiom)),
                        });
                    }
                }
                if !mm.is_empty() {
                    return self.stop(WriteError::ReadBack(mm));
                }
                self.phase = TablePhase::VerifyStatus;
                self.read(ReadPage::Status)
            }
            TablePhase::VerifyStatus => {
                let page = match StatusPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                let mm = stream_mismatches(&self.desired, &page);
                if mm.is_empty() {
                    self.finish(WriteOutcome::Verified)
                } else {
                    self.stop(WriteError::ReadBack(mm))
                }
            }
            TablePhase::Finished => self.finish(self.report.outcome.clone()),
        }
    }
}

/// Differences between a stream table and the status page's Stream and Name columns.
pub fn stream_mismatches(desired: &StreamTable, page: &StatusPage) -> Vec<Mismatch> {
    let mut mm = Vec::new();
    for row in &page.rows {
        let d = desired.get(row.slot);
        let (want_url, want_name) = match d {
            SlotConfig::Empty => (StreamCell::NotAssigned, String::new()),
            SlotConfig::Assigned { url, name } => (StreamCell::Url(*url), name.to_string()),
        };
        if row.stream != want_url {
            mm.push(Mismatch {
                what: format!("slot {} url", row.slot),
                expected: d.url_text(),
                observed: match &row.stream {
                    StreamCell::NotAssigned => String::new(),
                    StreamCell::Url(u) => u.to_string(),
                    StreamCell::Other(t) => t.clone(),
                },
            });
        }
        if row.name != want_name {
            mm.push(Mismatch {
                what: format!("slot {} name", row.slot),
                expected: want_name,
                observed: row.name.clone(),
            });
        }
    }
    mm
}

/// Differences between an RF plan and the status page's RF column and level.
pub fn rf_status_mismatches(plan: &RfPlan, page: &StatusPage) -> Vec<Mismatch> {
    let mut mm = Vec::new();
    for row in &page.rows {
        let ch = plan.channel_of(row.slot);
        if row.rf.channel != Some(ch) {
            mm.push(Mismatch {
                what: format!("slot {} channel", row.slot),
                expected: ch.to_string(),
                observed: row
                    .rf
                    .channel
                    .map_or_else(|| String::from("-"), |c| c.to_string()),
            });
        }
        let want = if plan.carrier_on(row.slot) {
            CarrierState::On
        } else {
            CarrierState::Off
        };
        if row.rf.state != want {
            mm.push(Mismatch {
                what: format!("slot {} carrier", row.slot),
                expected: format!("{want:?}"),
                observed: format!("{:?}", row.rf.state),
            });
        }
    }
    if let Some(l) = page.output_level
        && l != plan.level()
    {
        mm.push(Mismatch {
            what: String::from("output level"),
            expected: plan.level().get().to_string(),
            observed: l.get().to_string(),
        });
    }
    mm
}

/// Differences between a module's plan and its page.
pub fn module_mismatches(plan: &RfPlan, page: &QamConfigPage) -> Vec<Mismatch> {
    let m = page.module;
    let want = plan.module(m);
    let mut mm = Vec::new();
    let mut push = |what: String, expected: String, observed: String| {
        if expected != observed {
            mm.push(Mismatch {
                what,
                expected,
                observed,
            });
        }
    };
    let show =
        |c: Option<crate::types::Channel>| c.map_or_else(|| String::from("-"), |c| c.to_string());
    for (i, l) in ["A", "B", "C", "D"].iter().enumerate() {
        push(
            format!("module {m} carrier {l} channel"),
            want.channels()[i].to_string(),
            show(page.channels[i]),
        );
        push(
            format!("module {m} carrier {l} on"),
            want.carriers_on[i].to_string(),
            page.carriers_on[i].to_string(),
        );
    }
    push(
        format!("module {m} QAM mode"),
        format!("{:?}", Some(want.qam)),
        format!("{:?}", page.qam),
    );
    push(
        format!("module {m} QAM map"),
        format!("{:?}", Some(want.map)),
        format!("{:?}", page.map),
    );
    push(
        format!("module {m} output level"),
        format!("{:?}", Some(plan.level())),
        format!("{:?}", page.level),
    );
    push(
        format!("module {m} CW"),
        String::from("false"),
        page.cw.to_string(),
    );
    mm
}

#[derive(Debug)]
enum PlanPhase {
    CheckMode,
    FetchModule(RfModule),
    PostModule(RfModule),
    ReadBack(RfModule),
    VerifyStatus,
    Finished,
}

/// The RF-plan write: the four-module sequence of SPEC §10.3.
#[derive(Debug)]
pub(crate) struct RfPlanWrite {
    plan: RfPlan,
    session: SessionId,
    phase: PlanPhase,
    readback: Vec<Mismatch>,
    report: WriteReport,
}

impl RfPlanWrite {
    /// Start writing `plan` with `session`.
    pub(crate) fn start(plan: RfPlan, session: SessionId) -> (Self, WriteStep) {
        let after = RfModule::all()
            .iter()
            .map(|m| {
                let p = plan.module(*m);
                format!(
                    "module {m} = first {} {:?} {:?} on {:?} level {}",
                    p.first_channel.channel(),
                    p.qam,
                    p.map,
                    p.carriers_on,
                    plan.level().get()
                )
            })
            .collect();
        let mut w = RfPlanWrite {
            plan,
            session,
            phase: PlanPhase::CheckMode,
            readback: vec![],
            report: WriteReport {
                kind: WriteKind::RfPlan,
                outcome: WriteOutcome::Verified,
                posts: 0,
                before: vec![],
                after,
                post_responses: vec![],
            },
        };
        let step = w.read(ReadPage::Status);
        (w, step)
    }

    fn read(&mut self, page: ReadPage) -> WriteStep {
        match screen(RequestDraft::read(page, &self.session)) {
            Ok(r) => WriteStep::Send(r),
            Err(e) => self.stop(WriteError::Refused(e)),
        }
    }

    fn stop(&mut self, e: WriteError) -> WriteStep {
        self.phase = PlanPhase::Finished;
        self.report.outcome = if self.report.posts == 0 {
            WriteOutcome::Aborted(e)
        } else {
            WriteOutcome::Failed(e)
        };
        WriteStep::Done(self.report.clone())
    }

    fn next_module(m: RfModule) -> Option<RfModule> {
        RfModule::new(m.get() + 1).ok()
    }

    fn first() -> RfModule {
        RfModule::all()[0]
    }

    /// End the write without another step (reports `Abandoned`).
    pub(crate) fn abandon(&mut self) -> WriteReport {
        match self.stop(WriteError::Abandoned) {
            WriteStep::Done(r) => r,
            WriteStep::Send(_) => self.report.clone(),
        }
    }

    /// Feed the result of the last request.
    pub(crate) fn on_response(
        &mut self,
        result: Result<&Response, &TransportFailure>,
    ) -> WriteStep {
        let body = match body_ok(result) {
            Ok(b) => b,
            Err(e) => return self.stop(e),
        };
        match self.phase {
            PlanPhase::CheckMode => {
                let page = match StatusPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                if page.header.mode != TsMode::Mpts {
                    return self.stop(WriteError::NotMpts);
                }
                // Saving 1..4 in order passes through states where modules 1..k are new and
                // k+1..4 are as now; none of them may put two carriers on one channel.
                let current: Vec<Option<[crate::types::Channel; 4]>> = RfModule::all()
                    .iter()
                    .map(|m| {
                        let chans: Vec<_> =
                            m.slots().iter().map(|s| page.row(*s).rf.channel).collect();
                        let [a, b, c, d] = chans.as_slice() else {
                            return None;
                        };
                        Some([(*a)?, (*b)?, (*c)?, (*d)?])
                    })
                    .collect();
                self.report.before = page
                    .rows
                    .iter()
                    .map(|r| format!("slot {} rf = {:?}", r.slot, r.rf))
                    .collect();
                if let Some(i) = current.iter().position(Option::is_none) {
                    let m = u8::try_from(i + 1).unwrap_or(0);
                    return self.stop(WriteError::UnknownCurrentChannels(m));
                }
                for k in 1..=4 {
                    let mix: [Option<[crate::types::Channel; 4]>; 4] = std::array::from_fn(|i| {
                        if i < k {
                            Some(self.plan.modules()[i].channels())
                        } else {
                            current[i]
                        }
                    });
                    if let Err(e) = check_unique_channels(&mix) {
                        return self.stop(WriteError::IntermediateDuplicate(e));
                    }
                }
                self.phase = PlanPhase::FetchModule(Self::first());
                self.read(ReadPage::QamConfig(Self::first()))
            }
            PlanPhase::FetchModule(m) => {
                let page = match QamConfigPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                if page.module != m {
                    return self.stop(WriteError::Parse(ParseError::structure(
                        "qamconfig.cgi",
                        format!("asked for module {m}, got {}", page.module),
                    )));
                }
                if page.header.mode != TsMode::Mpts {
                    return self.stop(WriteError::NotMpts);
                }
                if page.communication_error {
                    return self.stop(WriteError::ModuleCommunication(m.get()));
                }
                if let Err(e) = page.check_fields() {
                    return self.stop(WriteError::FieldSet(e));
                }
                let p = self.plan.module(m);
                let mut overrides = vec![
                    (String::from("slotNumber"), m.get().to_string()),
                    (
                        String::from("outputChannelA"),
                        p.first_channel.channel().get().to_string(),
                    ),
                    (String::from("QAMModeA"), p.qam.form_value().to_string()),
                    (String::from("QAMMap"), p.map.form_value().to_string()),
                    (
                        String::from("outputLevel"),
                        self.plan.level().get().to_string(),
                    ),
                    // validate_qam() sets this to 1 before submitting.
                    (String::from("qam_saved"), String::from("1")),
                    (String::from("session_id"), self.session.wire()),
                ];
                for (i, l) in ["A", "B", "C", "D"].iter().enumerate() {
                    overrides.push((
                        format!("outputChannelControl{l}"),
                        u8::from(p.carriers_on[i]).to_string(),
                    ));
                }
                // CW is never sent: leaving the box out turns a test carrier off.
                let body = match browser_body(page.form(), &overrides, &["chkCW1"], None) {
                    Ok(b) => b,
                    Err(e) => return self.stop(e),
                };
                match screen(RequestDraft::write_post(
                    "qamconfig.cgi",
                    body,
                    WriteAuthority::qam_module(m),
                )) {
                    Ok(r) => {
                        self.phase = PlanPhase::PostModule(m);
                        self.report.posts += 1;
                        WriteStep::Send(r)
                    }
                    Err(e) => self.stop(WriteError::Refused(e)),
                }
            }
            PlanPhase::PostModule(m) => {
                let doc = crate::html::Document::parse(body);
                let class = crate::pages::classify(&doc);
                self.report
                    .post_responses
                    .push(format!("module {m}: {class:?}"));
                match class {
                    crate::pages::PageClass::LoggedOut => return self.stop(WriteError::LoggedOut),
                    crate::pages::PageClass::ModulatorError => {
                        return self.stop(WriteError::ModuleCommunication(m.get()));
                    }
                    _ => {}
                }
                if doc
                    .text()
                    .contains("There was an error communicating with the QAM Module")
                {
                    return self.stop(WriteError::ModuleCommunication(m.get()));
                }
                match Self::next_module(m) {
                    Some(n) => {
                        self.phase = PlanPhase::FetchModule(n);
                        self.read(ReadPage::QamConfig(n))
                    }
                    None => {
                        self.phase = PlanPhase::ReadBack(Self::first());
                        self.read(ReadPage::QamConfig(Self::first()))
                    }
                }
            }
            PlanPhase::ReadBack(m) => {
                let page = match QamConfigPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                if page.module != m {
                    return self.stop(WriteError::Parse(ParseError::structure(
                        "qamconfig.cgi",
                        format!("asked for module {m}, got {}", page.module),
                    )));
                }
                if page.communication_error {
                    self.readback.push(Mismatch {
                        what: format!("module {m}"),
                        expected: String::from("readable"),
                        observed: String::from("error communicating with the QAM module"),
                    });
                }
                self.readback.extend(module_mismatches(&self.plan, &page));
                match Self::next_module(m) {
                    Some(n) => {
                        self.phase = PlanPhase::ReadBack(n);
                        self.read(ReadPage::QamConfig(n))
                    }
                    None => {
                        self.phase = PlanPhase::VerifyStatus;
                        self.read(ReadPage::Status)
                    }
                }
            }
            PlanPhase::VerifyStatus => {
                let page = match StatusPage::parse(body) {
                    Ok(p) => p,
                    Err(e) => return self.stop(parse_err(e)),
                };
                let mut mm = std::mem::take(&mut self.readback);
                if page.header.mode != TsMode::Mpts {
                    mm.push(Mismatch {
                        what: String::from("mode"),
                        expected: String::from("Mpts"),
                        observed: format!("{:?}", page.header.mode),
                    });
                }
                mm.extend(rf_status_mismatches(&self.plan, &page));
                if mm.is_empty() {
                    self.phase = PlanPhase::Finished;
                    self.report.outcome = WriteOutcome::Verified;
                    WriteStep::Done(self.report.clone())
                } else {
                    self.stop(WriteError::ReadBack(mm))
                }
            }
            PlanPhase::Finished => WriteStep::Done(self.report.clone()),
        }
    }
}
