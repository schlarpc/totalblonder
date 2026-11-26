//! The time base, PCR measurements and repetition-interval trackers.
//!
//! The stream's byte positions are its clock. A byte at offset `i` arrives at `i · 8 / R`
//! seconds, where `R` is the nominal rate given by the caller or, without one, the rate the
//! PCRs of one reference PID give (a least-squares line of PCR value against byte position,
//! H.222.0 §2.4.2.2 equations 2-4 and 2-5 with the rate held constant, TR 101 290 §5.3.2
//! equation 3). A program's system time clock at byte `i` is its latest PCR plus the elapsed
//! bytes at that rate.

use std::collections::VecDeque;

use crate::report::{PcrStats, Position, Sink, TableStats};
use crate::rules::Rule;

/// 27 MHz ticks per second.
pub(crate) const SYSTEM_CLOCK: f64 = 27_000_000.0;
/// The PCR wraps at 2^33 · 300 ticks.
pub(crate) const PCR_MODULUS: u64 = (1u64 << 33) * 300;

/// `b - a` modulo the PCR range, as a signed number of ticks in (-M/2, M/2].
pub(crate) fn pcr_diff(b: u64, a: u64) -> i64 {
    let d = (b + PCR_MODULUS - a % PCR_MODULUS) % PCR_MODULUS;
    if d > PCR_MODULUS / 2 {
        d as i64 - PCR_MODULUS as i64
    } else {
        d as i64
    }
}

/// `b - a` modulo 2^33 (PTS/DTS), signed.
pub(crate) fn ts_diff(b: u64, a: u64) -> i64 {
    const M: u64 = 1 << 33;
    let d = (b.wrapping_sub(a)) & (M - 1);
    if d > M / 2 {
        d as i64 - M as i64
    } else {
        d as i64
    }
}

/// Numerically stable running least-squares fit of y against x (Welford).
#[derive(Debug, Clone, Default)]
pub(crate) struct LineFit {
    n: f64,
    mx: f64,
    my: f64,
    sxx: f64,
    sxy: f64,
    pub(crate) min_x: f64,
    pub(crate) max_x: f64,
}

impl LineFit {
    pub(crate) fn push(&mut self, x: f64, y: f64) {
        if self.n == 0.0 {
            self.min_x = x;
            self.max_x = x;
        }
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.n += 1.0;
        let dx = x - self.mx;
        self.mx += dx / self.n;
        let dy = y - self.my;
        self.my += dy / self.n;
        self.sxx += dx * (x - self.mx);
        self.sxy += dx * (y - self.my);
    }

    pub(crate) fn count(&self) -> f64 {
        self.n
    }

    pub(crate) fn slope(&self) -> Option<f64> {
        (self.n >= 2.0 && self.sxx > 0.0).then(|| self.sxy / self.sxx)
    }

    pub(crate) fn predict(&self, x: f64) -> Option<f64> {
        self.slope().map(|b| self.my + b * (x - self.mx))
    }
}

/// Converts byte positions to seconds.
#[derive(Debug, Clone)]
pub(crate) struct TimeBase {
    /// The rate given by the caller, bit/s.
    pub(crate) nominal: Option<f64>,
    /// The PID whose PCRs give the estimate.
    pub(crate) ref_pid: Option<u16>,
    /// Origin of the current fit: byte position and PCR value.
    origin: Option<(u64, u64)>,
    fit: LineFit,
    /// The last PCR of the reference PID (byte position, PCR) and consecutive outliers.
    last: Option<(u64, u64)>,
    outliers: u32,
    /// The estimate in use, bit/s.
    pub(crate) estimated: Option<f64>,
    /// Intervals between reference PCRs, and how many differed from the estimate by more
    /// than 0.5 %.
    pub(crate) intervals: u64,
    pub(crate) deviating: u64,
}

impl TimeBase {
    pub(crate) fn new(nominal: Option<f64>) -> Self {
        Self {
            nominal,
            ref_pid: None,
            origin: None,
            fit: LineFit::default(),
            last: None,
            outliers: 0,
            estimated: None,
            intervals: 0,
            deviating: 0,
        }
    }

    /// The rate in use, bit/s.
    #[inline]
    pub(crate) fn rate(&self) -> Option<f64> {
        self.nominal.or(self.estimated)
    }

    /// Seconds for a number of bytes.
    #[inline]
    pub(crate) fn dur(&self, bytes: u64) -> Option<f64> {
        self.rate().map(|r| bytes as f64 * 8.0 / r)
    }

    /// Bytes for a duration.
    pub(crate) fn bytes_for(&self, secs: f64) -> Option<u64> {
        self.rate().map(|r| (secs * r / 8.0).max(0.0) as u64)
    }

    /// True once the stream looks variable-rate (enough intervals seen, more than a tenth
    /// of them off the estimate).
    pub(crate) fn vbr(&self) -> bool {
        self.intervals >= 20 && self.deviating * 10 > self.intervals
    }

    /// Feeds a PCR of `pid` whose base ends at byte `pos`.
    pub(crate) fn pcr(&mut self, pid: u16, pos: u64, pcr: u64, discontinuity: bool) {
        match self.ref_pid {
            None => self.ref_pid = Some(pid),
            Some(p) if p != pid => return,
            Some(_) => {}
        }
        if discontinuity {
            self.restart(pos, pcr);
            return;
        }
        let Some((o_pos, o_pcr)) = self.origin else {
            self.restart(pos, pcr);
            return;
        };
        let x = (pos - o_pos) as f64;
        let y = pcr_diff(pcr, o_pcr) as f64;
        // Outlier rejection: a PCR more than 10 ms off the current line does not enter the
        // fit; three in a row start a new fit (a real discontinuity).
        if let Some(pred) = self.fit.predict(x)
            && self.fit.count() >= 3.0
            && (y - pred).abs() > 0.010 * SYSTEM_CLOCK
        {
            self.outliers += 1;
            if self.outliers >= 3 {
                self.restart(pos, pcr);
            }
            return;
        }
        self.outliers = 0;
        if let Some((l_pos, l_pcr)) = self.last {
            let dticks = pcr_diff(pcr, l_pcr) as f64;
            if dticks > 0.0
                && let Some(est) = self.estimated
            {
                let local = (pos - l_pos) as f64 * 8.0 * SYSTEM_CLOCK / dticks;
                self.intervals += 1;
                if (local / est - 1.0).abs() > 0.005 {
                    self.deviating += 1;
                }
            }
        }
        self.last = Some((pos, pcr));
        self.fit.push(x, y);
        if let Some(b) = self.fit.slope()
            && b > 0.0
            && self.fit.max_x - self.fit.min_x > 0.0
        {
            // Accept once the fit spans at least 10 ms.
            let span_ticks = b * (self.fit.max_x - self.fit.min_x);
            if span_ticks >= 0.010 * SYSTEM_CLOCK {
                self.estimated = Some(8.0 * SYSTEM_CLOCK / b);
            }
        }
    }

    fn restart(&mut self, pos: u64, pcr: u64) {
        self.origin = Some((pos, pcr));
        self.fit = LineFit::default();
        self.fit.push(0.0, 0.0);
        self.last = Some((pos, pcr));
        self.outliers = 0;
    }
}

/// One PCR kept for the accuracy measurement.
#[derive(Debug, Clone, Copy)]
struct PcrPoint {
    pos: u64,
    pcr: u64,
    packet_pos: u64,
    done: bool,
}

/// Window of PCRs used for the accuracy measurement (about two seconds at 30 ms spacing).
const WINDOW: usize = 64;

/// Per-PCR-PID measurements.
#[derive(Debug, Clone, Default)]
pub(crate) struct PcrTrack {
    pub(crate) pid: u16,
    last: Option<(u64, u64)>,
    window: VecDeque<PcrPoint>,
    /// Programs that name this PID their PCR_PID.
    pub(crate) referenced: bool,
    /// Stream position from which PCRs are expected (when the PMT named the PID).
    pub(crate) expected_since: Option<u64>,
    latched: bool,
    pub(crate) stats: PcrStats,
    rate_fit: LineFit,
    rate_origin: Option<(u64, u64)>,
}

/// Parameters of the PCR checks.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PcrParams {
    pub(crate) target_ms: f64,
    pub(crate) nominal: Option<f64>,
}

impl PcrTrack {
    pub(crate) fn new(pid: u16) -> Self {
        Self {
            pid,
            ..Self::default()
        }
    }

    /// A PCR whose base ends at byte `pos`, in the packet starting at `packet_pos`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pcr(
        &mut self,
        pos: u64,
        packet_pos: u64,
        pcr: u64,
        discontinuity: bool,
        tb: &TimeBase,
        params: PcrParams,
        sink: &mut Sink,
        gate_accuracy: bool,
    ) {
        let pid = Some(self.pid);
        self.stats.count += 1;
        if let Some((l_pos, l_pcr)) = self.last {
            // Intervals matter only on a PID that a PMT names as its PCR_PID (H.222.0 §2.7.2).
            if let Some(gap) = tb.dur(pos - l_pos).filter(|_| self.referenced) {
                let ms = gap * 1000.0;
                if ms > self.stats.max_interval_ms {
                    self.stats.max_interval_ms = ms;
                }
                if ms > 100.0 && !self.latched {
                    sink.emit(Rule::PcrRepetition, pid, || {
                        format!("{ms:.2} ms since the previous PCR on this PID (limit 100 ms)")
                    });
                }
                if ms > 40.0 {
                    self.stats.over_40ms += 1;
                    sink.emit(Rule::PcrInterval40, pid, || {
                        format!("{ms:.2} ms between PCRs (40 ms)")
                    });
                }
                if ms > params.target_ms {
                    self.stats.over_target += 1;
                    sink.emit(Rule::PcrIntervalTarget, pid, || {
                        format!("{ms:.2} ms between PCRs (target {} ms)", params.target_ms)
                    });
                }
            }
            let d = pcr_diff(pcr, l_pcr);
            if !discontinuity && !(0..=(SYSTEM_CLOCK / 10.0) as i64).contains(&d) {
                sink.emit(Rule::PcrDiscontinuity, pid, || {
                    format!(
                        "PCR moved by {:.3} ms from the previous one without discontinuity_indicator (allowed 0 to 100 ms)",
                        d as f64 / SYSTEM_CLOCK * 1000.0
                    )
                });
            }
            if discontinuity || !(0..=(SYSTEM_CLOCK / 10.0) as i64).contains(&d) {
                // A new time base: measure accuracy separately on each side.
                self.flush(tb, params, sink, gate_accuracy);
                self.window.clear();
                self.rate_origin = None;
                self.rate_fit = LineFit::default();
            }
        }
        self.latched = false;
        self.last = Some((pos, pcr));
        match self.rate_origin {
            None => {
                self.rate_origin = Some((pos, pcr));
                self.rate_fit.push(0.0, 0.0);
            }
            Some((op, oc)) => {
                self.rate_fit
                    .push((pos - op) as f64, pcr_diff(pcr, oc) as f64);
                if let Some(b) = self.rate_fit.slope()
                    && b > 0.0
                {
                    self.stats.rate = Some(8.0 * SYSTEM_CLOCK / b);
                }
            }
        }
        self.window.push_back(PcrPoint {
            pos,
            pcr,
            packet_pos,
            done: false,
        });
        if self.window.len() >= WINDOW {
            self.evaluate(WINDOW / 2, tb, params, sink, gate_accuracy);
            self.window.pop_front();
        }
    }

    /// Forgets the PCRs so far (after a loss of sync), keeping the statistics.
    pub(crate) fn rebase(&mut self, pos: u64) {
        self.last = None;
        self.window.clear();
        self.latched = false;
        self.rate_fit = LineFit::default();
        self.rate_origin = None;
        self.expected_since = Some(pos);
    }

    /// Checks for an overdue PCR (the next one has not come within 100 ms).
    pub(crate) fn tick(&mut self, now: u64, tb: &TimeBase, sink: &mut Sink) {
        if self.latched || !self.referenced {
            return;
        }
        let since = match (self.last, self.expected_since) {
            (Some((p, _)), _) => p,
            (None, Some(p)) if self.referenced => p,
            _ => return,
        };
        if now <= since {
            return;
        }
        if let Some(gap) = tb.dur(now - since)
            && gap > 0.1
        {
            self.latched = true;
            if self.last.is_some() {
                sink.emit(Rule::PcrRepetition, Some(self.pid), || {
                    format!("no PCR for {:.1} ms (limit 100 ms)", gap * 1000.0)
                });
            } else {
                sink.emit(Rule::PcrMissing, Some(self.pid), || {
                    format!(
                        "the PCR_PID has carried no PCR for {:.1} ms since the PMT named it",
                        gap * 1000.0
                    )
                });
            }
        }
    }

    /// Evaluates every point not yet evaluated up to index `upto` of the window.
    fn evaluate(
        &mut self,
        upto: usize,
        tb: &TimeBase,
        params: PcrParams,
        sink: &mut Sink,
        gate: bool,
    ) {
        if self.window.len() < 4 {
            for p in self.window.iter_mut() {
                p.done = true;
            }
            return;
        }
        let Some(first) = self.window.front().copied() else {
            return;
        };
        let pts: Vec<(f64, f64)> = self
            .window
            .iter()
            .map(|p| {
                (
                    (p.pos - first.pos) as f64,
                    pcr_diff(p.pcr, first.pcr) as f64,
                )
            })
            .collect();
        let mut fit = LineFit::default();
        for &(x, y) in &pts {
            fit.push(x, y);
        }
        let nominal_slope = params.nominal.map(|r| 8.0 * SYSTEM_CLOCK / r);
        let nominal_mean = nominal_slope
            .map(|b| pts.iter().map(|&(x, y)| y - b * x).sum::<f64>() / pts.len() as f64);
        let skip = gate && tb.vbr() && params.nominal.is_none();
        for (i, p) in self.window.iter_mut().enumerate().take(upto + 1) {
            if p.done {
                continue;
            }
            p.done = true;
            let (x, y) = pts[i];
            if let Some(pred) = fit.predict(x) {
                let ns = (y - pred) / SYSTEM_CLOCK * 1e9;
                if ns.abs() > self.stats.max_accuracy_ns && !skip {
                    self.stats.max_accuracy_ns = ns.abs();
                }
                if ns.abs() > 500.0 && !skip {
                    let pos = Position {
                        byte: p.packet_pos,
                        packet: p.packet_pos / 188,
                        pid: Some(self.pid),
                    };
                    sink.emit_at(Rule::PcrAccuracy, pos, || {
                        format!("PCR off its byte position by {ns:+.0} ns (limit +-500 ns)")
                    });
                }
            }
            if let (Some(b), Some(m)) = (nominal_slope, nominal_mean) {
                let ns = (y - b * x - m) / SYSTEM_CLOCK * 1e9;
                let e = self.stats.max_accuracy_nominal_ns.get_or_insert(0.0);
                if ns.abs() > *e {
                    *e = ns.abs();
                }
                if ns.abs() > 500.0 {
                    let pos = Position {
                        byte: p.packet_pos,
                        packet: p.packet_pos / 188,
                        pid: Some(self.pid),
                    };
                    sink.emit_at(Rule::PcrAccuracyNominal, pos, || {
                        format!(
                            "PCR off its byte position at the nominal rate by {ns:+.0} ns (limit +-500 ns)"
                        )
                    });
                }
            }
        }
    }

    /// Evaluates the remaining points (end of stream or of a time base).
    pub(crate) fn flush(&mut self, tb: &TimeBase, params: PcrParams, sink: &mut Sink, gate: bool) {
        let n = self.window.len();
        if n > 0 {
            self.evaluate(n - 1, tb, params, sink, gate);
        }
    }
}

/// Tracks the repetition of one table (or table section) against one or two limits.
#[derive(Debug, Clone)]
pub(crate) struct Repeat {
    pub(crate) name: String,
    pub(crate) pid: u16,
    /// Byte position of the last occurrence, or of the moment the table became expected.
    last: Option<u64>,
    limits: &'static [(f64, Rule)],
    latched: u8,
    pub(crate) count: u64,
    max_gap: f64,
}

impl Repeat {
    pub(crate) fn new(name: String, pid: u16, limits: &'static [(f64, Rule)]) -> Self {
        Self {
            name,
            pid,
            last: None,
            limits,
            latched: 0,
            count: 0,
            max_gap: 0.0,
        }
    }

    /// Starts the clock without an occurrence (the table is expected from now on).
    pub(crate) fn expect_from(&mut self, pos: u64) {
        if self.last.is_none() {
            self.last = Some(pos);
        }
    }

    /// Moves the reference forward to `pos` (after a loss of sync, nothing before counts).
    pub(crate) fn rebase(&mut self, pos: u64) {
        if let Some(l) = self.last
            && l < pos
        {
            self.last = Some(pos);
        }
        self.latched = 0;
    }

    /// An occurrence whose last byte is at `pos`.
    pub(crate) fn occur(&mut self, pos: u64, tb: &TimeBase, sink: &mut Sink) {
        if let Some(last) = self.last
            && pos > last
            && let Some(gap) = tb.dur(pos - last)
        {
            if self.count > 0 && gap > self.max_gap {
                self.max_gap = gap;
            }
            for (i, &(lim, rule)) in self.limits.iter().enumerate() {
                if gap > lim && self.latched & (1 << i) == 0 {
                    let name = &self.name;
                    sink.emit(rule, Some(self.pid), || {
                        format!(
                            "{name}: {:.3} ms since the previous one (limit {:.0} ms)",
                            gap * 1000.0,
                            lim * 1000.0
                        )
                    });
                }
            }
        }
        self.latched = 0;
        self.last = Some(pos);
        self.count += 1;
    }

    /// Reports a table that is overdue at `now` (once per gap).
    pub(crate) fn tick(&mut self, now: u64, tb: &TimeBase, sink: &mut Sink) {
        let Some(last) = self.last else {
            return;
        };
        if now <= last {
            return;
        }
        let Some(gap) = tb.dur(now - last) else {
            return;
        };
        for (i, &(lim, rule)) in self.limits.iter().enumerate() {
            if gap > lim && self.latched & (1 << i) == 0 {
                self.latched |= 1 << i;
                let name = &self.name;
                sink.emit(rule, Some(self.pid), || {
                    format!(
                        "{name}: none for {:.3} ms (limit {:.0} ms)",
                        gap * 1000.0,
                        lim * 1000.0
                    )
                });
            }
        }
    }

    pub(crate) fn stats(&self) -> TableStats {
        TableStats {
            name: self.name.clone(),
            pid: self.pid,
            count: self.count,
            max_interval_ms: self.max_gap * 1000.0,
            limit_ms: self
                .limits
                .iter()
                .map(|l| l.0 * 1000.0)
                .fold(f64::INFINITY, f64::min),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_wrap() {
        assert_eq!(pcr_diff(5, PCR_MODULUS - 5), 10);
        assert_eq!(pcr_diff(PCR_MODULUS - 5, 5), -10);
        assert_eq!(ts_diff(3, (1 << 33) - 2), 5);
    }

    #[test]
    fn rate_from_pcrs() {
        let mut tb = TimeBase::new(None);
        let r = 38_800_000.0f64;
        for k in 0..100u64 {
            let pos = k * 188 * 800 + 10;
            let pcr = (pos as f64 * 8.0 * SYSTEM_CLOCK / r).round() as u64;
            tb.pcr(0x31, pos, pcr, false);
        }
        let est = tb.estimated.unwrap_or(0.0);
        assert!((est / r - 1.0).abs() < 1e-7, "{est}");
        assert!(!tb.vbr());
    }
}
