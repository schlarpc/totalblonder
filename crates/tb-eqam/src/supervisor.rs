//! The supervisor (SPEC §10.2): a pure state machine fed by parsed polls and the desired
//! state. It decides what to read and when, detects drift, applies it in `apply` mode within
//! the write limits, raises alarms, and keeps the device metrics (SPEC §11.3). Its outputs are
//! [`Action`]s; it does no I/O.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use crate::client::{ClientEvent, Job, Observation, ReadError};
use crate::pages::ParseError;
use crate::pages::debug::{DebugPage, InputFpga, StreamCounter};
use crate::pages::diagnostic::DiagnosticPage;
use crate::pages::eventlog::{EventAlarm, EventLine};
use crate::pages::qamconfig::LockState;
use crate::pages::status::{InputState, ModuleState, StatusPage};
use crate::pages::streaminfo::{DetectedProgram, StreamInfoPage, normalized};
use crate::plan::DesiredState;
use crate::request::ReadPage;
use crate::session::SessionEvent;
use crate::types::{RfModule, Slot, TsMode};
use crate::write::{
    Mismatch, WriteError, WriteKind, WriteOutcome, WriteReport, module_mismatches,
    rf_status_mismatches, stream_mismatches,
};

/// `report` (default until M4) or `apply` (SPEC §13.2 `reconcile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileMode {
    /// Detect and alarm; never write.
    Report,
    /// Rewrite the affected form, within the limits.
    Apply,
}

/// Supervisor settings (SPEC §10.2, §10.4, §13.2 `[device]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorConfig {
    /// Report or apply.
    pub mode: ReconcileMode,
    /// `status.cgi` poll interval (30 s).
    pub status_interval: Duration,
    /// Interval of the other pages (5 min).
    pub slow_interval: Duration,
    /// At most one reconcile per this period (10 min).
    pub min_reconcile_gap: Duration,
    /// At most this many reconciles per 24 h (3).
    pub max_reconciles_per_day: u32,
    /// The same drift may be re-applied this many times; when it returns once more, writes
    /// stop and an alarm is raised (3).
    pub same_drift_limit: u32,
    /// A drift must be seen on this many consecutive polls before it is applied (2).
    pub confirm_polls: u32,
    /// An uptime this much below the expected value means a reboot (60 s).
    pub reboot_tolerance: Duration,
    /// Defect D: alarm when the time since the last table reset exceeds this.
    pub defect_d_limit: Option<Duration>,
    /// Consecutive failed status reads before the device is declared unreachable (3).
    pub unreachable_after: u32,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        SupervisorConfig {
            mode: ReconcileMode::Report,
            status_interval: Duration::from_secs(30),
            slow_interval: Duration::from_secs(300),
            min_reconcile_gap: Duration::from_secs(600),
            max_reconciles_per_day: 3,
            same_drift_limit: 3,
            confirm_polls: 2,
            reboot_tolerance: Duration::from_secs(60),
            defect_d_limit: None,
            unreachable_after: 3,
        }
    }
}

/// Why writes were stopped. Cleared only by [`Supervisor::resume_writes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HaltReason {
    /// The same drift came back after `same_drift_limit` re-applies: something else is
    /// changing the configuration.
    SameDriftReturned {
        /// Form.
        kind: WriteKind,
        /// The drift.
        drift: Vec<Mismatch>,
    },
    /// A write failed in a way that needs a person (unexpected form, refused request,
    /// read-back mismatch, or a failure after posting).
    WriteFailed(Box<WriteReport>),
}

/// A condition the operator should see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Alarm {
    /// Status reads keep failing.
    Unreachable,
    /// The page header does not say MPTS. Nothing is written.
    NotMpts(TsMode),
    /// Desired and observed differ (raised in report mode, and in apply mode while a write
    /// is held back).
    Drift(Vec<(WriteKind, Mismatch)>),
    /// The uptime dropped: the box rebooted. A full comparison follows.
    Rebooted {
        /// Uptime expected from the last reading.
        expected: Duration,
        /// Uptime read.
        observed: Duration,
    },
    /// An event-log line from the alarm catalog.
    EventLog(EventAlarm, EventLine),
    /// Writes are stopped.
    WritesHalted(HaltReason),
    /// A write did not complete; the device is unchanged (aborted before posting) and the
    /// cause is transient.
    WriteAborted(Box<WriteReport>),
    /// An RF module is not locked.
    ModuleNotLocked(RfModule, String),
    /// Defect D: time since the last table reset is past the limit.
    DefectDLimit {
        /// Time since the reset.
        since_reset: Duration,
    },
    /// Stream Info shows programs or elementary streams in a slot's stream that differ from
    /// what the server sends there.
    ProgramsMismatch {
        /// The slot.
        slot: Slot,
        /// What the configuration says the stream carries.
        expected: Vec<DetectedProgram>,
        /// What the box detects.
        detected: Vec<DetectedProgram>,
    },
    /// The status page shows no input for a slot the desired state assigns a stream to.
    InputAbsent(Slot),
    /// The Diagnostics page reports an input as oversubscribed (its cell text, verbatim).
    Oversubscribed(Slot, String),
    /// A slot's TS Sync Errors count went up since the last poll.
    TsSyncErrors {
        /// The slot.
        slot: Slot,
        /// The count read.
        count: u64,
        /// The count at the poll before.
        previous: u64,
    },
    /// The login was refused (wrong credentials).
    LoginRejected,
    /// Another Administrator holds the session; the supervisor waits.
    YieldingToOtherAdmin,
}

/// Identity of a persistent alarm, for clearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlarmKey {
    /// [`Alarm::Unreachable`].
    Unreachable,
    /// [`Alarm::NotMpts`].
    NotMpts,
    /// [`Alarm::Drift`].
    Drift,
    /// [`Alarm::WritesHalted`].
    WritesHalted,
    /// [`Alarm::ModuleNotLocked`].
    ModuleNotLocked(RfModule),
    /// [`Alarm::DefectDLimit`].
    DefectD,
    /// [`Alarm::ProgramsMismatch`].
    ProgramsMismatch(Slot),
    /// [`Alarm::Oversubscribed`].
    Oversubscribed(Slot),
    /// [`Alarm::InputAbsent`].
    InputAbsent(Slot),
}

/// Something the supervisor wants done or known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Give this job to the client.
    Job(Job),
    /// Raise an alarm (persistent ones are raised once, until cleared).
    Alarm(Alarm),
    /// A persistent alarm cleared.
    Clear(AlarmKey),
    /// A write finished (for `device_log` and `device_writes_total`).
    WriteLogged(WriteReport),
    /// An event-log line not seen before (all of them after a reboot or a restart), for the
    /// log store: the box's own syslog is not collected.
    EventLogLine(EventLine),
    /// A write was due but held back by the rate limit.
    RateLimited {
        /// When the next write may start.
        until: Duration,
    },
}

/// The device metrics of SPEC §11.3.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceMetrics {
    /// `device_reachable`.
    pub reachable: bool,
    /// `device_uptime_seconds`.
    pub uptime_seconds: Option<u64>,
    /// `device_drift_items`.
    pub drift_items: usize,
    /// `device_writes_total{form}`.
    pub writes_total: BTreeMap<WriteKind, u64>,
    /// `device_input_present{carrier}` (slots 1–16).
    pub input_present: [Option<bool>; 16],
    /// `device_bitrate_bps{carrier}` (average).
    pub bitrate_bps: [Option<u64>; 16],
    /// `device_overflow_total{carrier}` from debug.cgi's Buffer Overflow Errors, by
    /// [`slot_fpga`].
    pub overflow_total: [Option<u64>; 16],
    /// `device_timestamp_errors_total{carrier}` from debug.cgi's Timestamp Error Count, by
    /// [`slot_fpga`].
    pub timestamp_errors: [Option<u64>; 16],
    /// `device_ts_sync_errors_total{carrier}` from the Diagnostics page.
    pub ts_sync_errors: [Option<u64>; 16],
    /// `device_rf_locked{module}`.
    pub rf_locked: [Option<bool>; 4],
    /// `device_seconds_since_table_reset`.
    pub seconds_since_table_reset: Option<u64>,
    /// `device_temperature_decifahrenheit`: chassis temperature in tenths of °F.
    pub temperature_tenths_f: Option<i32>,
    /// `device_alarms_active`: persistent alarms raised and not yet cleared.
    pub alarms_active: usize,
}

/// Where debug.cgi counts `slot`: FPGA2A serves slots 1–8 as its streams 1–8, FPGA2B slots
/// 9–16 as its streams 1–8. Checked live for slot 1 only (2026-11-27: FPGA2A IP register 0
/// and MPTS stream 1 carried slot 1's stream); the FPGA2B half is still an inference.
pub fn slot_fpga(slot: Slot) -> (InputFpga, u8) {
    if slot.get() <= 8 {
        (InputFpga::A, slot.get())
    } else {
        (InputFpga::B, slot.get() - 8)
    }
}

/// One metric sample: name, labels, value.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Metric name.
    pub name: &'static str,
    /// Labels.
    pub labels: Vec<(&'static str, String)>,
    /// Value.
    pub value: f64,
}

impl DeviceMetrics {
    /// The samples, for a Prometheus text renderer. `refused` comes from the client.
    pub fn samples(&self, refused: &BTreeMap<crate::refusal::RefusalRule, u64>) -> Vec<Sample> {
        let mut out = Vec::new();
        let mut push = |name, labels: Vec<(&'static str, String)>, value: f64| {
            out.push(Sample {
                name,
                labels,
                value,
            })
        };
        push(
            "device_reachable",
            vec![],
            if self.reachable { 1.0 } else { 0.0 },
        );
        if let Some(u) = self.uptime_seconds {
            push("device_uptime_seconds", vec![], u as f64);
        }
        push("device_drift_items", vec![], self.drift_items as f64);
        for k in [WriteKind::StreamTable, WriteKind::RfPlan] {
            let v = self.writes_total.get(&k).copied().unwrap_or(0);
            push(
                "device_writes_total",
                vec![("form", k.as_str().to_owned())],
                v as f64,
            );
        }
        for rule in crate::refusal::RefusalRule::ALL {
            let n = refused.get(&rule).copied().unwrap_or(0);
            push(
                "device_refused_total",
                vec![("rule", rule.as_str().to_owned())],
                n as f64,
            );
        }
        for s in Slot::all() {
            let c = vec![("carrier", s.to_string())];
            if let Some(p) = self.input_present[s.index()] {
                push("device_input_present", c.clone(), if p { 1.0 } else { 0.0 });
            }
            if let Some(b) = self.bitrate_bps[s.index()] {
                push("device_bitrate_bps", c.clone(), b as f64);
            }
            if let Some(o) = self.overflow_total[s.index()] {
                push("device_overflow_total", c.clone(), o as f64);
            }
            if let Some(n) = self.timestamp_errors[s.index()] {
                push("device_timestamp_errors_total", c.clone(), n as f64);
            }
            if let Some(n) = self.ts_sync_errors[s.index()] {
                push("device_ts_sync_errors_total", c, n as f64);
            }
        }
        for m in RfModule::all() {
            if let Some(l) = self.rf_locked[m.index()] {
                push(
                    "device_rf_locked",
                    vec![("module", m.to_string())],
                    if l { 1.0 } else { 0.0 },
                );
            }
        }
        if let Some(s) = self.seconds_since_table_reset {
            push("device_seconds_since_table_reset", vec![], s as f64);
        }
        if let Some(t) = self.temperature_tenths_f {
            push("device_temperature_decifahrenheit", vec![], f64::from(t));
        }
        push("device_alarms_active", vec![], self.alarms_active as f64);
        out
    }
}

/// The supervisor.
#[derive(Debug, Clone)]
pub struct Supervisor {
    config: SupervisorConfig,
    desired: DesiredState,
    due: BTreeMap<ReadPage, Duration>,
    outstanding: BTreeSet<ReadPage>,
    paused_until: Option<Duration>,
    /// Last uptime reading and when it was taken.
    uptime: Option<(Duration, Duration)>,
    mode_seen: Option<TsMode>,
    stream_drift: Vec<Mismatch>,
    rf_status_drift: Vec<Mismatch>,
    rf_module_drift: [Vec<Mismatch>; 4],
    confirmations: BTreeMap<WriteKind, u32>,
    /// Re-applies per drift signature.
    applies: BTreeMap<(WriteKind, Vec<Mismatch>), u32>,
    /// The drift each in-flight write is meant to fix.
    writing: BTreeMap<WriteKind, Vec<Mismatch>>,
    reconciles: VecDeque<Duration>,
    halted: Option<HaltReason>,
    active: BTreeSet<AlarmKey>,
    seen_log: BTreeSet<EventLine>,
    table_reset_at: Option<Duration>,
    status_failures: u32,
    rate_limited_noted: bool,
    metrics: DeviceMetrics,
}

/// Every page polled, in the order a full comparison reads them.
pub fn all_pages() -> Vec<ReadPage> {
    let mut v = vec![ReadPage::Status];
    v.extend(RfModule::all().map(ReadPage::QamConfig));
    v.extend([
        ReadPage::StreamInfo,
        ReadPage::Diagnostic,
        ReadPage::Debug,
        ReadPage::EventLog,
    ]);
    v
}

impl Supervisor {
    /// A supervisor keeping the device at `desired`, starting at `now` with every page due.
    pub fn new(config: SupervisorConfig, desired: DesiredState, now: Duration) -> Self {
        Supervisor {
            config,
            desired,
            due: all_pages().into_iter().map(|p| (p, now)).collect(),
            outstanding: BTreeSet::new(),
            paused_until: None,
            uptime: None,
            mode_seen: None,
            stream_drift: vec![],
            rf_status_drift: vec![],
            rf_module_drift: Default::default(),
            confirmations: BTreeMap::new(),
            applies: BTreeMap::new(),
            writing: BTreeMap::new(),
            reconciles: VecDeque::new(),
            halted: None,
            active: BTreeSet::new(),
            seen_log: BTreeSet::new(),
            table_reset_at: None,
            status_failures: 0,
            rate_limited_noted: false,
            metrics: DeviceMetrics::default(),
        }
    }

    /// Metrics as of the last observation.
    pub fn metrics(&self) -> &DeviceMetrics {
        &self.metrics
    }

    /// Why writes are stopped, if they are.
    pub fn halted(&self) -> Option<&HaltReason> {
        self.halted.as_ref()
    }

    /// The current drift: `(form, mismatch)`.
    pub fn drift(&self) -> Vec<(WriteKind, Mismatch)> {
        let mut v: Vec<(WriteKind, Mismatch)> = self
            .stream_drift
            .iter()
            .cloned()
            .map(|m| (WriteKind::StreamTable, m))
            .collect();
        v.extend(self.rf_drift().into_iter().map(|m| (WriteKind::RfPlan, m)));
        v
    }

    fn rf_drift(&self) -> Vec<Mismatch> {
        let mut v = self.rf_status_drift.clone();
        for d in &self.rf_module_drift {
            v.extend(d.iter().cloned());
        }
        v.sort();
        v.dedup();
        v
    }

    /// Stop polling and writing until `until` (`POST /api/device/pause`), so a person can use
    /// the web UI without being logged out.
    pub fn pause(&mut self, until: Duration) {
        self.paused_until = Some(until);
    }

    /// The operator cleared a halt.
    pub fn resume_writes(&mut self) -> Vec<Action> {
        self.halted = None;
        self.applies.clear();
        self.clear(AlarmKey::WritesHalted).into_iter().collect()
    }

    /// Run a full comparison now (`POST /api/device/reconcile`, or after a reboot).
    pub fn compare_now(&mut self, now: Duration) {
        for p in all_pages() {
            self.due.insert(p, now);
        }
    }

    /// The server restarted `mux_sync` (defect D options 1 and 2): the table was reset.
    pub fn note_table_reset(&mut self, now: Duration) {
        self.table_reset_at = Some(now);
    }

    /// Replace the desired state (configuration reload). Re-apply counts start over.
    pub fn set_desired(&mut self, desired: DesiredState, now: Duration) {
        self.desired = desired;
        self.applies.clear();
        self.confirmations.clear();
        self.compare_now(now);
    }

    fn raise(&mut self, key: AlarmKey, alarm: Alarm) -> Option<Action> {
        if self.active.insert(key) {
            self.metrics.alarms_active = self.active.len();
            Some(Action::Alarm(alarm))
        } else {
            None
        }
    }

    fn clear(&mut self, key: AlarmKey) -> Option<Action> {
        if self.active.remove(&key) {
            self.metrics.alarms_active = self.active.len();
            Some(Action::Clear(key))
        } else {
            None
        }
    }

    /// When the next poll is due (for the adapter's sleep), if not paused.
    pub fn next_due(&self) -> Option<Duration> {
        self.due
            .iter()
            .filter(|(p, _)| !self.outstanding.contains(p))
            .map(|(_, t)| *t)
            .min()
            .map(|t| self.paused_until.map_or(t, |u| t.max(u)))
    }

    /// Advance to `now`: polls that are due, a reconcile if one is needed and allowed, the
    /// defect-D check.
    pub fn tick(&mut self, now: Duration) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(u) = self.paused_until {
            if now < u {
                return out;
            }
            self.paused_until = None;
        }
        let due: Vec<ReadPage> = self
            .due
            .iter()
            .filter(|(p, t)| **t <= now && !self.outstanding.contains(p))
            .map(|(p, _)| *p)
            .collect();
        for p in due {
            self.outstanding.insert(p);
            out.push(Action::Job(Job::Read(p)));
        }
        self.defect_d(now, &mut out);
        self.reconcile(now, &mut out);
        out
    }

    fn defect_d(&mut self, now: Duration, out: &mut Vec<Action>) {
        let Some((uptime, at)) = self.uptime else {
            return;
        };
        let up_now = uptime + now.saturating_sub(at);
        let since = match self.table_reset_at {
            Some(r) => up_now.min(now.saturating_sub(r)),
            None => up_now,
        };
        self.metrics.seconds_since_table_reset = Some(since.as_secs());
        if let Some(limit) = self.config.defect_d_limit {
            if since > limit {
                out.extend(self.raise(
                    AlarmKey::DefectD,
                    Alarm::DefectDLimit { since_reset: since },
                ));
            } else {
                out.extend(self.clear(AlarmKey::DefectD));
            }
        }
    }

    fn reconcile(&mut self, now: Duration, out: &mut Vec<Action>) {
        let drift = self.drift();
        self.metrics.drift_items = drift.len();
        if drift.is_empty() {
            out.extend(self.clear(AlarmKey::Drift));
            self.rate_limited_noted = false;
            return;
        }
        if self.mode_seen != Some(TsMode::Mpts) {
            return;
        }
        let ready: Vec<WriteKind> = [WriteKind::StreamTable, WriteKind::RfPlan]
            .into_iter()
            .filter(|k| {
                self.confirmations.get(k).copied().unwrap_or(0) >= self.config.confirm_polls
                    && !self.writing.contains_key(k)
            })
            .collect();
        let held = self.config.mode == ReconcileMode::Report
            || self.halted.is_some()
            || !self.writing.is_empty();
        if held || ready.is_empty() {
            if self.config.mode == ReconcileMode::Report || self.halted.is_some() {
                out.extend(self.raise(AlarmKey::Drift, Alarm::Drift(drift)));
            }
            return;
        }
        // Same drift back after `same_drift_limit` re-applies: stop.
        for k in &ready {
            let sig = (*k, self.drift_of(*k));
            if self.applies.get(&sig).copied().unwrap_or(0) >= self.config.same_drift_limit {
                let reason = HaltReason::SameDriftReturned {
                    kind: *k,
                    drift: sig.1,
                };
                self.halted = Some(reason.clone());
                out.extend(self.raise(AlarmKey::WritesHalted, Alarm::WritesHalted(reason)));
                out.extend(self.raise(AlarmKey::Drift, Alarm::Drift(drift)));
                return;
            }
        }
        // Rate limits: one reconcile per gap, so many per 24 h.
        while self
            .reconciles
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= Duration::from_secs(86_400))
        {
            self.reconciles.pop_front();
        }
        let gap_ok = self
            .reconciles
            .back()
            .is_none_or(|t| now.saturating_sub(*t) >= self.config.min_reconcile_gap);
        let day_ok = self.reconciles.len() < self.config.max_reconciles_per_day as usize;
        if !gap_ok || !day_ok {
            let until = if !day_ok {
                self.reconciles
                    .front()
                    .map_or(now, |t| *t + Duration::from_secs(86_400))
            } else {
                self.reconciles
                    .back()
                    .map_or(now, |t| *t + self.config.min_reconcile_gap)
            };
            if !self.rate_limited_noted {
                self.rate_limited_noted = true;
                out.push(Action::RateLimited { until });
            }
            out.extend(self.raise(AlarmKey::Drift, Alarm::Drift(drift)));
            return;
        }
        self.rate_limited_noted = false;
        self.reconciles.push_back(now);
        // The RF plan first: the stream table maps slots onto carriers that must exist.
        for k in [WriteKind::RfPlan, WriteKind::StreamTable] {
            if ready.contains(&k) {
                self.writing.insert(k, self.drift_of(k));
                out.push(Action::Job(match k {
                    WriteKind::StreamTable => Job::WriteStreams(self.desired.streams.clone()),
                    WriteKind::RfPlan => Job::WriteRfPlan(self.desired.rf),
                }));
            }
        }
    }

    fn drift_of(&self, k: WriteKind) -> Vec<Mismatch> {
        match k {
            WriteKind::StreamTable => self.stream_drift.clone(),
            WriteKind::RfPlan => self.rf_drift(),
        }
    }

    fn confirm(&mut self, k: WriteKind, has_drift: bool) {
        let c = self.confirmations.entry(k).or_insert(0);
        *c = if has_drift { c.saturating_add(1) } else { 0 };
    }

    fn observe_uptime(&mut self, now: Duration, observed: Duration, out: &mut Vec<Action>) {
        self.metrics.uptime_seconds = Some(observed.as_secs());
        if let Some((up, at)) = self.uptime {
            let expected = up + now.saturating_sub(at);
            if observed + self.config.reboot_tolerance < expected {
                out.push(Action::Alarm(Alarm::Rebooted { expected, observed }));
                // The event log lives in RAM; after a boot every line is new.
                self.seen_log.clear();
                // A boot resets mux_sync's table (defect D).
                self.table_reset_at = Some(now.saturating_sub(observed));
                self.compare_now(now);
            }
        }
        self.uptime = Some((observed, now));
    }

    /// Feed a client event.
    pub fn on_event(&mut self, now: Duration, event: ClientEvent) -> Vec<Action> {
        let mut out = Vec::new();
        match event {
            ClientEvent::Observed(o) => self.on_observation(now, *o, &mut out),
            ClientEvent::ReadFailed { page, error } => {
                self.outstanding.remove(&page);
                let retry = match page {
                    ReadPage::Status => self.config.status_interval,
                    _ => self.config.slow_interval,
                };
                if all_pages().contains(&page) {
                    self.due.insert(page, now + retry);
                }
                if page == ReadPage::Status {
                    let transient_parse = matches!(
                        error,
                        ReadError::Parse(ParseError::LoggedOut | ParseError::ModulatorError)
                    );
                    if !transient_parse {
                        self.status_failures = self.status_failures.saturating_add(1);
                        if self.status_failures >= self.config.unreachable_after {
                            self.metrics.reachable = false;
                            out.extend(self.raise(AlarmKey::Unreachable, Alarm::Unreachable));
                        }
                    }
                }
            }
            ClientEvent::WriteFinished(report) => self.on_write(now, report, &mut out),
            ClientEvent::Session(SessionEvent::LoginRejected) => {
                out.push(Action::Alarm(Alarm::LoginRejected))
            }
            ClientEvent::Session(SessionEvent::YieldingToOtherAdmin { .. }) => {
                out.push(Action::Alarm(Alarm::YieldingToOtherAdmin));
            }
            ClientEvent::Session(_) => {}
        }
        out
    }

    fn on_observation(&mut self, now: Duration, o: Observation, out: &mut Vec<Action>) {
        let page = match &o {
            Observation::Status(_) => ReadPage::Status,
            Observation::StatusEdit(_) => ReadPage::StatusEdit,
            Observation::QamConfig(p) => ReadPage::QamConfig(p.module),
            Observation::StreamInfo(_) => ReadPage::StreamInfo,
            Observation::Diagnostic(_) => ReadPage::Diagnostic,
            Observation::Debug(_) => ReadPage::Debug,
            Observation::EventLog(_) => ReadPage::EventLog,
        };
        self.outstanding.remove(&page);
        let interval = if page == ReadPage::Status {
            self.config.status_interval
        } else {
            self.config.slow_interval
        };
        // A reboot detected below may pull this forward again.
        if all_pages().contains(&page) {
            self.due.insert(page, now + interval);
        }
        match o {
            Observation::Status(p) => self.on_status(now, &p, out),
            Observation::QamConfig(p) => {
                self.observe_uptime(now, p.header.uptime.0, out);
                let m = p.module;
                self.rf_module_drift[m.index()] = module_mismatches(&self.desired.rf, &p);
                let any = !self.rf_drift().is_empty();
                self.confirm(WriteKind::RfPlan, any);
                if p.lock != LockState::Lock {
                    let text = format!("{:?}", p.lock);
                    out.extend(self.raise(
                        AlarmKey::ModuleNotLocked(m),
                        Alarm::ModuleNotLocked(m, text),
                    ));
                }
            }
            Observation::Debug(p) => {
                self.observe_uptime(now, p.uptime.0, out);
                self.on_debug(&p);
            }
            Observation::EventLog(p) => {
                // The page lists the newest line first; pass new lines on oldest first.
                for line in p.lines.into_iter().rev() {
                    if self.seen_log.contains(&line) {
                        continue;
                    }
                    if let Some(kind) = line.alarm() {
                        out.push(Action::Alarm(Alarm::EventLog(kind, line.clone())));
                    }
                    out.push(Action::EventLogLine(line.clone()));
                    self.seen_log.insert(line);
                }
            }
            Observation::StreamInfo(p) => self.on_stream_info(&p, out),
            Observation::Diagnostic(p) => self.on_diagnostic(&p, out),
            Observation::StatusEdit(_) => {}
        }
    }

    fn on_status(&mut self, now: Duration, p: &StatusPage, out: &mut Vec<Action>) {
        self.status_failures = 0;
        self.metrics.reachable = true;
        out.extend(self.clear(AlarmKey::Unreachable));
        self.observe_uptime(now, p.header.uptime.0, out);
        self.metrics.temperature_tenths_f = p.header.temperature.map(|t| t.tenths);
        self.mode_seen = Some(p.header.mode);
        if p.header.mode == TsMode::Mpts {
            out.extend(self.clear(AlarmKey::NotMpts));
        } else {
            out.extend(self.raise(AlarmKey::NotMpts, Alarm::NotMpts(p.header.mode)));
        }
        for row in &p.rows {
            let i = row.slot.index();
            self.metrics.input_present[i] = match row.input {
                InputState::Present => Some(true),
                InputState::Absent => Some(false),
                InputState::NotAssigned | InputState::Unknown(_) => None,
            };
            self.metrics.bitrate_bps[i] = row.bitrate.map(|(avg, _)| avg.0);
            let assigned = matches!(
                self.desired.streams.get(row.slot),
                crate::plan::SlotConfig::Assigned { .. }
            );
            if assigned && row.input == InputState::Absent {
                out.extend(self.raise(
                    AlarmKey::InputAbsent(row.slot),
                    Alarm::InputAbsent(row.slot),
                ));
            } else {
                out.extend(self.clear(AlarmKey::InputAbsent(row.slot)));
            }
        }
        for m in RfModule::all() {
            let locked = p.modules[m.index()] == ModuleState::Locked;
            self.metrics.rf_locked[m.index()] = Some(locked);
            if locked {
                out.extend(self.clear(AlarmKey::ModuleNotLocked(m)));
            } else {
                let text = format!("{:?}", p.modules[m.index()]);
                out.extend(self.raise(
                    AlarmKey::ModuleNotLocked(m),
                    Alarm::ModuleNotLocked(m, text),
                ));
            }
        }
        self.stream_drift = stream_mismatches(&self.desired.streams, p);
        self.rf_status_drift = rf_status_mismatches(&self.desired.rf, p);
        let s = !self.stream_drift.is_empty();
        let r = !self.rf_drift().is_empty();
        self.confirm(WriteKind::StreamTable, s);
        self.confirm(WriteKind::RfPlan, r);
    }

    fn on_debug(&mut self, p: &DebugPage) {
        for s in Slot::all() {
            let (fpga, n) = slot_fpga(s);
            self.metrics.overflow_total[s.index()] =
                p.stream_counter(StreamCounter::BufferOverflow, fpga, n);
            self.metrics.timestamp_errors[s.index()] = p.timestamp_errors(fpga, n);
        }
    }

    /// Compare what the box detects in each stream with what the server sends. A stream the
    /// page does not list is not compared: its input is absent, which the status page shows.
    fn on_stream_info(&mut self, p: &StreamInfoPage, out: &mut Vec<Action>) {
        let expected: Vec<(Slot, Vec<DetectedProgram>)> = self
            .desired
            .programs
            .iter()
            .map(|(s, v)| (*s, normalized(v)))
            .collect();
        for (slot, want) in expected {
            let crate::plan::SlotConfig::Assigned { url, .. } = self.desired.streams.get(slot)
            else {
                continue;
            };
            let Some(seen) = p.streams.iter().find(|d| d.url.as_ref() == Some(url)) else {
                continue;
            };
            let got = normalized(&seen.programs);
            if got == want {
                out.extend(self.clear(AlarmKey::ProgramsMismatch(slot)));
            } else {
                out.extend(self.raise(
                    AlarmKey::ProgramsMismatch(slot),
                    Alarm::ProgramsMismatch {
                        slot,
                        expected: want,
                        detected: got,
                    },
                ));
            }
        }
    }

    fn on_diagnostic(&mut self, p: &DiagnosticPage, out: &mut Vec<Action>) {
        for row in &p.rows {
            let Ok(slot) = Slot::new(row.index) else {
                continue;
            };
            let i = slot.index();
            if let (Some(now), Some(before)) = (row.ts_sync_errors, self.metrics.ts_sync_errors[i])
                && now > before
            {
                out.push(Action::Alarm(Alarm::TsSyncErrors {
                    slot,
                    count: now,
                    previous: before,
                }));
            }
            self.metrics.ts_sync_errors[i] = row.ts_sync_errors;
            if row.oversubscribed {
                out.extend(self.raise(
                    AlarmKey::Oversubscribed(slot),
                    Alarm::Oversubscribed(slot, row.oversubscription.clone()),
                ));
            } else {
                out.extend(self.clear(AlarmKey::Oversubscribed(slot)));
            }
        }
    }

    fn on_write(&mut self, now: Duration, report: WriteReport, out: &mut Vec<Action>) {
        let kind = report.kind;
        let fixed = self.writing.remove(&kind).unwrap_or_default();
        out.push(Action::WriteLogged(report.clone()));
        if report.posts > 0 {
            *self.metrics.writes_total.entry(kind).or_insert(0) += 1;
        }
        // Drift must be seen afresh before anything is written again.
        self.confirmations.insert(kind, 0);
        match &report.outcome {
            WriteOutcome::Verified | WriteOutcome::AlreadyInPlace => {
                *self.applies.entry((kind, fixed)).or_insert(0) += 1;
                self.due.insert(ReadPage::Status, now);
                if kind == WriteKind::RfPlan {
                    // The write read all four pages back; stale per-module drift must not
                    // trigger another write before the pages are polled again.
                    self.rf_module_drift = Default::default();
                    for m in RfModule::all() {
                        self.due.insert(ReadPage::QamConfig(m), now);
                    }
                }
            }
            WriteOutcome::Aborted(e) if transient(e) => {
                out.push(Action::Alarm(Alarm::WriteAborted(Box::new(report.clone()))));
            }
            WriteOutcome::Aborted(_) | WriteOutcome::Failed(_) => {
                let posted = matches!(report.outcome, WriteOutcome::Failed(_));
                let reason = HaltReason::WriteFailed(Box::new(report));
                self.halted = Some(reason.clone());
                out.extend(self.raise(AlarmKey::WritesHalted, Alarm::WritesHalted(reason)));
                if posted {
                    self.compare_now(now);
                }
            }
        }
    }
}

/// Errors that leave the device unchanged and may clear by themselves.
fn transient(e: &WriteError) -> bool {
    matches!(
        e,
        WriteError::LoggedOut
            | WriteError::Transport(_)
            | WriteError::Status(_)
            | WriteError::Parse(ParseError::ModulatorError)
            | WriteError::ModuleCommunication(_)
    )
}
