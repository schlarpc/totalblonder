//! Findings, counters, statistics and the report.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;

use serde::Serialize;

use crate::rules::{Rule, Severity};

/// The version of the JSON document ([`Report::to_json`]); changed when a field changes
/// meaning or disappears (fields may be added without a change).
pub const JSON_SCHEMA: &str = "tb-ts-verify/1";

/// Where in the stream a finding is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Position {
    /// Byte offset of the start of the packet from the start of the stream.
    pub byte: u64,
    /// Index of the packet: its byte offset divided by 188 (exact while the stream is in
    /// sync from its first byte).
    pub packet: u64,
    /// The PID concerned, when there is one.
    pub pid: Option<u16>,
}

/// One violation of one rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// The rule.
    pub rule: Rule,
    /// Where.
    pub position: Position,
    /// Stream time in seconds (from the first byte, at the time base's rate), when known.
    pub time: Option<f64>,
    /// What, in plain English.
    pub message: String,
}

impl Finding {
    /// One line: severity, rule, clause, position, message.
    pub fn line(&self) -> String {
        let i = self.rule.info();
        let mut s = format!(
            "{} {} [{} p.{}] {}",
            i.severity.name(),
            i.id,
            i.clause,
            i.page,
            position_text(&self.position)
        );
        if let Some(t) = self.time {
            let _ = write!(s, " t={t:.6}s");
        }
        let _ = write!(s, ": {}", self.message);
        s
    }
}

fn position_text(p: &Position) -> String {
    let mut s = format!("packet {} (byte {})", p.packet, p.byte);
    if let Some(pid) = p.pid {
        let _ = write!(s, " PID 0x{pid:04X}");
    }
    s
}

/// Per-PID statistics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PidStats {
    /// What the PID carries, as far as the tables say.
    pub role: String,
    /// Packets.
    pub packets: u64,
    /// Packets with transport_scrambling_control not 00.
    pub scrambled: u64,
    /// Continuity errors (CC-ERROR and CC-DUPLICATE).
    pub cc_errors: u64,
    /// Average rate over the stream, bit/s (when the time base is known).
    pub bitrate: Option<f64>,
}

/// Per-PCR-PID statistics.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PcrStats {
    /// PCRs seen.
    pub count: u64,
    /// Largest interval between successive PCRs, ms (arrival, at the time base's rate).
    pub max_interval_ms: f64,
    /// Intervals above 40 ms.
    pub over_40ms: u64,
    /// Intervals above the configured target.
    pub over_target: u64,
    /// Largest |PCR accuracy| against the fitted line, ns.
    pub max_accuracy_ns: f64,
    /// Largest |PCR accuracy| against the nominal rate, ns (with --rate).
    pub max_accuracy_nominal_ns: Option<f64>,
    /// The rate the PCRs of this PID give, bit/s.
    pub rate: Option<f64>,
}

/// Repetition statistics of one table (or one section of it).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TableStats {
    /// Name, for example "PAT", "PMT program 1", "EIT-0 source 3".
    pub name: String,
    /// PID.
    pub pid: u16,
    /// Occurrences.
    pub count: u64,
    /// Largest interval between the last bytes of successive occurrences, ms.
    pub max_interval_ms: f64,
    /// The limit checked, ms.
    pub limit_ms: f64,
}

/// Statistics of one buffer of the model.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BufferStats {
    /// Name, for example "TB", "MB", "EB", "B", "TBsys", "Bsys", "SB".
    pub buffer: String,
    /// PID (for TBsys and Bsys: the program's PMT PID).
    pub pid: u16,
    /// Size, bytes.
    pub size: u64,
    /// Highest fullness seen, bytes.
    pub max_fill: u64,
}

/// Statistics of the whole stream.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    /// Bytes pushed.
    pub bytes: u64,
    /// Packets analyzed (in sync).
    pub packets: u64,
    /// Bytes skipped while out of sync.
    pub bytes_out_of_sync: u64,
    /// Stream duration at the time base's rate, s.
    pub duration_s: Option<f64>,
    /// TS rate estimated from the PCRs of the reference PCR PID, bit/s.
    pub rate_estimated: Option<f64>,
    /// The nominal rate given with --rate, bit/s.
    pub rate_nominal: Option<f64>,
    /// Estimated against nominal, ppm.
    pub rate_deviation_ppm: Option<f64>,
    /// The rate the time base used, bit/s.
    pub rate_used: Option<f64>,
    /// False when the rate between PCRs varied by more than 0.5 %.
    pub cbr: bool,
    /// Share of null packets.
    pub null_share: f64,
    /// Per PID.
    pub pids: BTreeMap<u16, PidStats>,
    /// Per PCR PID.
    pub pcr: BTreeMap<u16, PcrStats>,
    /// Table repetition.
    pub tables: Vec<TableStats>,
    /// Buffer model.
    pub buffers: Vec<BufferStats>,
    /// Programs of the last PAT: program_number to PMT PID.
    pub programs: BTreeMap<u16, u16>,
    /// The PAT's transport_stream_id.
    pub tsid: Option<u16>,
    /// Virtual channels of the last VCT: "major.minor name (program n)".
    pub channels: Vec<String>,
    /// Constructs seen, with counts (for knowing which rules a stream exercised).
    pub features: BTreeMap<&'static str, u64>,
}

/// The result of checking a stream (or the state so far, for a running checker).
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The findings kept: the first `max_examples` of every rule.
    pub findings: Vec<Finding>,
    /// The number of violations of every rule that was violated.
    pub counts: BTreeMap<Rule, u64>,
    /// Statistics.
    pub stats: Stats,
}

impl Report {
    /// Number of violations of rules of this severity.
    pub fn count_severity(&self, sev: Severity) -> u64 {
        self.counts
            .iter()
            .filter(|(r, _)| r.info().severity == sev)
            .map(|(_, n)| *n)
            .sum()
    }

    /// Number of violations of one rule.
    pub fn count(&self, rule: Rule) -> u64 {
        self.counts.get(&rule).copied().unwrap_or(0)
    }

    /// The rules violated, in catalog order.
    pub fn rules(&self) -> Vec<Rule> {
        self.counts.keys().copied().collect()
    }

    /// True if no rule of severity `worst` or worse (lower number) was violated.
    pub fn passes(&self, worst: Severity) -> bool {
        !self.counts.keys().any(|r| r.info().severity <= worst)
    }

    /// The human summary: stream figures, then a table of the rules violated.
    pub fn summary(&self) -> String {
        let st = &self.stats;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} bytes, {} packets{}; S1 {}  S2 {}  S3 {}  S4 {}",
            st.bytes,
            st.packets,
            st.duration_s
                .map(|d| format!(", {d:.3} s"))
                .unwrap_or_default(),
            self.count_severity(Severity::S1),
            self.count_severity(Severity::S2),
            self.count_severity(Severity::S3),
            self.count_severity(Severity::S4)
        );
        let mut rate = String::new();
        if let Some(r) = st.rate_estimated {
            let _ = write!(rate, "rate from PCR {r:.0} bit/s");
        }
        if let Some(r) = st.rate_nominal {
            let _ = write!(
                rate,
                "{}nominal {r:.0} bit/s",
                if rate.is_empty() { "" } else { ", " }
            );
        }
        if let Some(p) = st.rate_deviation_ppm {
            let _ = write!(rate, " ({p:+.2} ppm)");
        }
        if !rate.is_empty() {
            let _ = writeln!(
                s,
                "{rate}; {}; nulls {:.2} %",
                if st.cbr {
                    "constant rate"
                } else {
                    "VARIABLE rate"
                },
                st.null_share * 100.0
            );
        }
        if let Some(t) = st.tsid {
            let progs: Vec<String> = st
                .programs
                .iter()
                .map(|(p, pid)| format!("{p}->0x{pid:04X}"))
                .collect();
            let _ = writeln!(s, "TSID 0x{t:04X}; programs {}", progs.join(" "));
        }
        if !st.channels.is_empty() {
            let _ = writeln!(s, "channels: {}", st.channels.join("; "));
        }
        for (pid, p) in &st.pcr {
            let _ = writeln!(
                s,
                "PCR 0x{pid:04X}: {} PCRs, max interval {:.2} ms ({} over 40 ms, {} over target), max |accuracy| {:.0} ns{}",
                p.count,
                p.max_interval_ms,
                p.over_40ms,
                p.over_target,
                p.max_accuracy_ns,
                p.max_accuracy_nominal_ns
                    .map(|v| format!(", against nominal {v:.0} ns"))
                    .unwrap_or_default()
            );
        }
        if self.counts.is_empty() {
            return s;
        }
        let _ = writeln!(s, "{:<22} {:<3} {:>10}  first at", "rule", "sev", "count");
        for (rule, n) in &self.counts {
            let info = rule.info();
            let first = self.findings.iter().find(|f| f.rule == *rule);
            let at = first
                .map(|f| position_text(&f.position))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "{:<22} {:<3} {:>10}  {}",
                info.id,
                info.severity.name(),
                n,
                at
            );
        }
        s
    }

    /// Every kept finding, one per line.
    pub fn details(&self) -> String {
        let mut s = String::new();
        for f in &self.findings {
            let _ = writeln!(s, "{}", f.line());
        }
        s
    }

    /// The table statistics as text.
    pub fn tables_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{:<28} {:>6} {:>8} {:>10} {:>9}",
            "table", "PID", "count", "max ms", "limit ms"
        );
        for t in &self.stats.tables {
            let _ = writeln!(
                s,
                "{:<28} 0x{:04X} {:>8} {:>10.1} {:>9.0}",
                t.name, t.pid, t.count, t.max_interval_ms, t.limit_ms
            );
        }
        for b in &self.stats.buffers {
            let _ = writeln!(
                s,
                "buffer {:<6} PID 0x{:04X}: max {} of {} bytes",
                b.buffer, b.pid, b.max_fill, b.size
            );
        }
        s
    }

    /// The report as JSON (schema [`JSON_SCHEMA`], documented in the crate's README).
    pub fn to_json(&self) -> serde_json::Value {
        #[derive(Serialize)]
        struct F<'a> {
            rule: &'a str,
            severity: &'a str,
            clause: &'a str,
            page: &'a str,
            packet: u64,
            byte: u64,
            pid: Option<u16>,
            time: Option<f64>,
            message: &'a str,
        }
        let findings: Vec<serde_json::Value> = self
            .findings
            .iter()
            .filter_map(|f| {
                let i = f.rule.info();
                serde_json::to_value(F {
                    rule: i.id,
                    severity: i.severity.name(),
                    clause: i.clause,
                    page: i.page,
                    packet: f.position.packet,
                    byte: f.position.byte,
                    pid: f.position.pid,
                    time: f.time,
                    message: &f.message,
                })
                .ok()
            })
            .collect();
        let counts: serde_json::Map<String, serde_json::Value> = self
            .counts
            .iter()
            .map(|(r, n)| (r.info().id.to_string(), serde_json::Value::from(*n)))
            .collect();
        let st = &self.stats;
        let pids: Vec<serde_json::Value> = st
            .pids
            .iter()
            .map(|(pid, p)| {
                serde_json::json!({
                    "pid": pid, "role": p.role, "packets": p.packets, "scrambled": p.scrambled,
                    "cc_errors": p.cc_errors, "bitrate": p.bitrate,
                })
            })
            .collect();
        let pcr: Vec<serde_json::Value> = st
            .pcr
            .iter()
            .map(|(pid, p)| {
                serde_json::json!({
                    "pid": pid, "count": p.count, "max_interval_ms": p.max_interval_ms,
                    "over_40ms": p.over_40ms, "over_target": p.over_target,
                    "max_accuracy_ns": p.max_accuracy_ns,
                    "max_accuracy_nominal_ns": p.max_accuracy_nominal_ns, "rate": p.rate,
                })
            })
            .collect();
        let features: serde_json::Map<String, serde_json::Value> = st
            .features
            .iter()
            .map(|(k, n)| ((*k).to_string(), serde_json::Value::from(*n)))
            .collect();
        let programs: serde_json::Map<String, serde_json::Value> = st
            .programs
            .iter()
            .map(|(p, pid)| (p.to_string(), serde_json::Value::from(*pid)))
            .collect();
        serde_json::json!({
            "schema": JSON_SCHEMA,
            "bytes": st.bytes,
            "packets": st.packets,
            "bytes_out_of_sync": st.bytes_out_of_sync,
            "duration_s": st.duration_s,
            "rate": {
                "estimated": st.rate_estimated,
                "nominal": st.rate_nominal,
                "deviation_ppm": st.rate_deviation_ppm,
                "used": st.rate_used,
                "cbr": st.cbr,
            },
            "null_share": st.null_share,
            "tsid": st.tsid,
            "programs": programs,
            "channels": st.channels,
            "severity": {
                "S1": self.count_severity(Severity::S1),
                "S2": self.count_severity(Severity::S2),
                "S3": self.count_severity(Severity::S3),
                "S4": self.count_severity(Severity::S4),
            },
            "counts": counts,
            "findings": findings,
            "pids": pids,
            "pcr": pcr,
            "tables": st.tables,
            "buffers": st.buffers,
            "features": features,
        })
    }
}

/// Collects findings while a stream is checked.
#[derive(Debug)]
pub(crate) struct Sink {
    pub(crate) findings: Vec<Finding>,
    pub(crate) counts: BTreeMap<Rule, u64>,
    pub(crate) features: BTreeMap<&'static str, u64>,
    max_examples: usize,
    kept: BTreeMap<Rule, usize>,
    /// Every finding (not only the kept ones), for a running checker to drain; bounded.
    events: VecDeque<Finding>,
    events_capacity: usize,
    pub(crate) events_dropped: u64,
    /// The current packet: set by the checker before each packet is analyzed.
    pub(crate) pos: Position,
    /// The stream time of the current packet, when known.
    pub(crate) time: Option<f64>,
}

impl Sink {
    pub(crate) fn new(max_examples: usize, events_capacity: usize) -> Self {
        Self {
            findings: Vec::new(),
            counts: BTreeMap::new(),
            features: BTreeMap::new(),
            max_examples,
            kept: BTreeMap::new(),
            events: VecDeque::new(),
            events_capacity,
            events_dropped: 0,
            pos: Position::default(),
            time: None,
        }
    }

    /// Records a violation at the current packet with the given PID; the message is built
    /// only if the finding is kept.
    #[inline]
    pub(crate) fn emit(&mut self, rule: Rule, pid: Option<u16>, message: impl FnOnce() -> String) {
        let pos = Position { pid, ..self.pos };
        self.emit_at(rule, pos, message);
    }

    /// Records a violation at an explicit position.
    pub(crate) fn emit_at(
        &mut self,
        rule: Rule,
        position: Position,
        message: impl FnOnce() -> String,
    ) {
        *self.counts.entry(rule).or_insert(0) += 1;
        let k = self.kept.entry(rule).or_insert(0);
        let keep = *k < self.max_examples;
        let event = self.events_capacity > 0;
        if !keep && !event {
            return;
        }
        let f = Finding {
            rule,
            position,
            time: self.time,
            message: message(),
        };
        if event {
            if self.events.len() >= self.events_capacity {
                self.events.pop_front();
                self.events_dropped += 1;
            }
            self.events.push_back(f.clone());
        }
        if keep {
            *k += 1;
            self.findings.push(f);
        }
    }

    #[inline]
    pub(crate) fn feature(&mut self, name: &'static str) {
        *self.features.entry(name).or_insert(0) += 1;
    }

    pub(crate) fn drain_events(&mut self) -> Vec<Finding> {
        self.events.drain(..).collect()
    }
}
