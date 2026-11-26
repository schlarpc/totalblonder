//! Findings and the report.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::rules::{Rule, Severity};

/// Where in the stream a finding is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Position {
    /// Byte offset from the start of the stream.
    pub byte: u64,
    /// Bit within that byte, 0 for the most significant.
    pub bit: u8,
    /// Number of the picture in coded order, from 0 (a field picture counts as a picture).
    pub picture_coded: Option<u32>,
    /// Number of the frame in display order, from 0.
    pub picture_display: Option<u32>,
    /// Number of the slice in the picture, from 0.
    pub slice: Option<u32>,
    /// macroblock_address.
    pub macroblock: Option<u32>,
}

/// One violation of one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The rule.
    pub rule: Rule,
    /// Where.
    pub position: Position,
    /// What, in plain English.
    pub message: String,
}

/// Counts of the syntax a stream uses, to tell which rules it exercised.
pub type Features = BTreeMap<&'static str, u64>;

/// The result of verifying one stream.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The findings kept: the first `max_examples` of every rule.
    pub findings: Vec<Finding>,
    /// The number of violations of every rule that was violated.
    pub counts: BTreeMap<Rule, u64>,
    /// Syntax features seen, with counts.
    pub features: Features,
    /// Number of bytes verified.
    pub bytes: u64,
    /// Number of coded pictures.
    pub pictures: u32,
    /// Number of coded frames.
    pub frames: u32,
    /// Number of sequences (sequence headers after a sequence end count as a new one).
    pub sequences: u32,
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

    /// True if no S1 or S2 rule was violated.
    pub fn conformant(&self) -> bool {
        self.count_severity(Severity::S1) == 0 && self.count_severity(Severity::S2) == 0
    }

    /// The summary table by rule, as text.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} bytes, {} sequences, {} pictures, {} frames; S1 {}  S2 {}  S3 {}  S4 {}",
            self.bytes,
            self.sequences,
            self.pictures,
            self.frames,
            self.count_severity(Severity::S1),
            self.count_severity(Severity::S2),
            self.count_severity(Severity::S3),
            self.count_severity(Severity::S4)
        );
        if self.counts.is_empty() {
            return s;
        }
        let _ = writeln!(s, "{:<26} {:<3} {:>8}  first at", "rule", "sev", "count");
        for (rule, n) in &self.counts {
            let info = rule.info();
            let first = self.findings.iter().find(|f| f.rule == *rule);
            let at = first.map(position_text).unwrap_or_default();
            let _ = writeln!(
                s,
                "{:<26} {:<3} {:>8}  {}",
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
            let info = f.rule.info();
            let _ = writeln!(
                s,
                "{} {} [{} p.{}] {}: {}",
                info.severity.name(),
                info.id,
                info.clause,
                info.page,
                position_text(f),
                f.message
            );
        }
        s
    }

    /// The report as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        #[derive(Serialize)]
        struct F<'a> {
            rule: &'a str,
            severity: &'a str,
            clause: &'a str,
            page: &'a str,
            scope: &'a str,
            #[serde(flatten)]
            position: Position,
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
                    scope: i.scope.name(),
                    position: f.position,
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
        let features: serde_json::Map<String, serde_json::Value> = self
            .features
            .iter()
            .map(|(k, n)| ((*k).to_string(), serde_json::Value::from(*n)))
            .collect();
        serde_json::json!({
            "bytes": self.bytes,
            "sequences": self.sequences,
            "pictures": self.pictures,
            "frames": self.frames,
            "severity": {
                "S1": self.count_severity(Severity::S1),
                "S2": self.count_severity(Severity::S2),
                "S3": self.count_severity(Severity::S3),
                "S4": self.count_severity(Severity::S4),
            },
            "counts": counts,
            "features": features,
            "findings": findings,
        })
    }
}

fn position_text(f: &Finding) -> String {
    let p = &f.position;
    let mut s = format!("byte {}.{}", p.byte, p.bit);
    if let Some(n) = p.picture_coded {
        let _ = write!(s, " pic {n}");
    }
    if let Some(n) = p.picture_display {
        let _ = write!(s, " (display {n})");
    }
    if let Some(n) = p.slice {
        let _ = write!(s, " slice {n}");
    }
    if let Some(n) = p.macroblock {
        let _ = write!(s, " mb {n}");
    }
    s
}

/// Collects findings while a stream is verified.
#[derive(Debug)]
pub(crate) struct Sink {
    pub(crate) report: Report,
    max_examples: usize,
    kept: BTreeMap<Rule, usize>,
}

impl Sink {
    pub(crate) fn new(max_examples: usize) -> Self {
        Self {
            report: Report::default(),
            max_examples,
            kept: BTreeMap::new(),
        }
    }

    /// Records a violation; the message is built only if the finding is kept.
    #[inline]
    pub(crate) fn emit(
        &mut self,
        rule: Rule,
        position: Position,
        message: impl FnOnce() -> String,
    ) {
        *self.report.counts.entry(rule).or_insert(0) += 1;
        let k = self.kept.entry(rule).or_insert(0);
        if *k < self.max_examples {
            *k += 1;
            self.report.findings.push(Finding {
                rule,
                position,
                message: message(),
            });
        }
    }

    #[inline]
    pub(crate) fn feature(&mut self, name: &'static str) {
        *self.report.features.entry(name).or_insert(0) += 1;
    }

    pub(crate) fn feature_n(&mut self, name: &'static str, n: u64) {
        if n > 0 {
            *self.report.features.entry(name).or_insert(0) += n;
        }
    }
}
