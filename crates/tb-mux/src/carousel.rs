//! The table carousel (SPEC §7.2, §7.6): every table has a period and a next due time; due
//! tables queue on their PID; each slot the carousel offers the packet of the most overdue
//! PID whose buffer models admit it.
//!
//! Buffer models per PID (`notes/standards-digest.md` §1.6, §2.3):
//! - every table PID: the transport buffer TBsys, 512 bytes at Rxsys = 1 Mbit/s;
//! - PSIP PIDs (0x1FFB, EIT, ETT): the smoothing buffer, 1,024 bytes at 250 kbit/s (SCTE 54
//!   Table 2; A/65 §7.1), which also caps each at 250 kbit/s;
//! - PAT and PMT: the system buffer Bsys of each program, 1,536 bytes at 80 kbit/s, fed by
//!   the PAT and that program's PMT.
//!
//! Sections on one PID are sent whole and in order; a new table set replaces the old one for
//! everything not yet started, so a section in flight finishes with the content it began with.

use std::collections::VecDeque;

use tb_psip::build::{TableKind, TableSet};
use tb_psip::gps::{DstRule, GpsSeconds, LeapSeconds};
use tb_ts::section::{Cursor, Section};
use tb_ts::{Cc, Packet, Pid, STC_HZ};

use crate::bucket::Leaky;

const MS: u64 = STC_HZ / 1000;

/// Repetition periods, 27 MHz ticks (defaults from SPEC §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Periods {
    /// PAT (≤ 100 ms required).
    pub pat: u64,
    /// Each PMT (≤ 400 ms).
    pub pmt: u64,
    /// MGT (≤ 150 ms).
    pub mgt: u64,
    /// CVCT (≤ 400 ms).
    pub cvct: u64,
    /// STT (≤ 10 s on cable; 1 s is the terrestrial figure receivers are built around).
    pub stt: u64,
    /// EIT-0 (500 ms recommended).
    pub eit0: u64,
    /// EIT-1 to EIT-3.
    pub eit_near: u64,
    /// EIT-4 and later.
    pub eit_far: u64,
    /// Event ETT-0 to ETT-3.
    pub ett_near: u64,
    /// Later event ETTs.
    pub ett_far: u64,
}

impl Default for Periods {
    fn default() -> Self {
        Periods {
            pat: 90 * MS,
            pmt: 300 * MS,
            mgt: 100 * MS,
            cvct: 250 * MS,
            // A little inside the 1 s receivers are built around and the 500 ms A/65
            // recommends, so that scheduling jitter never takes a gap over them.
            stt: 900 * MS,
            eit0: 450 * MS,
            eit_near: 3000 * MS,
            eit_far: 30_000 * MS,
            ett_near: 10_000 * MS,
            ett_far: 60_000 * MS,
        }
    }
}

impl Periods {
    fn of(&self, k: TableKind) -> u64 {
        match k {
            TableKind::Pat => self.pat,
            TableKind::Pmt(_) => self.pmt,
            TableKind::Mgt => self.mgt,
            TableKind::Cvct => self.cvct,
            TableKind::Eit(0) => self.eit0,
            TableKind::Eit(k) if k < 4 => self.eit_near,
            TableKind::Eit(_) => self.eit_far,
            TableKind::EventEtt(k) if k < 4 => self.ett_near,
            TableKind::EventEtt(_) => self.ett_far,
        }
    }
}

/// How the STT is made: GPS time from the wall clock the mux is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SttConfig {
    /// GPS − UTC.
    pub leap: LeapSeconds,
    /// Daylight-saving rule.
    pub dst: DstRule,
}

/// The relation between the carrier's STC and UTC, sampled by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClock {
    /// An STC value.
    pub stc: u64,
    /// Unix time at that STC value, nanoseconds.
    pub unix_nanos: i128,
}

impl WallClock {
    /// Unix nanoseconds at STC `t`.
    pub fn unix_nanos_at(&self, t: u64) -> i128 {
        let dt = i128::from(t) - i128::from(self.stc);
        self.unix_nanos + dt * 1000 / 27
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EntryKind {
    Table(TableKind),
    Stt,
}

#[derive(Debug)]
struct Entry {
    pid: Pid,
    kind: EntryKind,
    sections: Vec<Section>,
    period: u64,
    next_due: u64,
    queued: bool,
    /// STC when the last instance started, for statistics.
    last_start: Option<u64>,
    /// Longest start-to-start gap seen, ticks.
    pub max_gap: u64,
}

#[derive(Debug)]
struct Pending {
    due: u64,
    entry: usize,
    cursor: Cursor<Section>,
    started: bool,
}

#[derive(Debug)]
struct PidQueue {
    pid: Pid,
    cc: Cc,
    tb: Leaky,
    smoothing: Option<Leaky>,
    /// Program indices whose Bsys this PID feeds (all for the PAT, one for a PMT).
    bsys: Vec<usize>,
    pending: VecDeque<Pending>,
}

/// Statistics of one table, for metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableStat {
    /// Its PID.
    pub pid: Pid,
    /// Its configured period, ticks.
    pub period: u64,
    /// The longest start-to-start gap seen, ticks.
    pub max_gap: u64,
}

/// The carousel of one carrier.
#[derive(Debug)]
pub struct Carousel {
    periods: Periods,
    entries: Vec<Entry>,
    pids: Vec<PidQueue>,
    bsys: Vec<Leaky>,
    stt: Option<SttConfig>,
    /// Instances skipped because the previous one had not started yet.
    pub overruns: u64,
    /// STT instances skipped for want of a wall clock.
    pub stt_without_clock: u64,
}

fn is_psip(pid: Pid) -> bool {
    pid == Pid::PSIP_BASE || (0x1D00..=0x1E80).contains(&pid.get())
}

impl Carousel {
    /// An empty carousel for `programs` programs.
    pub fn new(periods: Periods, programs: usize, stt: Option<SttConfig>) -> Self {
        Carousel {
            periods,
            entries: Vec::new(),
            pids: Vec::new(),
            bsys: (0..programs).map(|_| Leaky::new(1536, 80_000, 0)).collect(),
            stt,
            overruns: 0,
            stt_without_clock: 0,
        }
    }

    fn pid_queue(&mut self, pid: Pid, bsys: Vec<usize>) -> usize {
        if let Some(i) = self.pids.iter().position(|q| q.pid == pid) {
            return i;
        }
        self.pids.push(PidQueue {
            pid,
            cc: Cc::new(0),
            tb: Leaky::new(512, 1_000_000, 0),
            smoothing: is_psip(pid).then(|| Leaky::new(1024, 250_000, 0)),
            bsys,
            pending: VecDeque::new(),
        });
        self.pids.len() - 1
    }

    /// Replaces the table set at STC `now`. `pmt_program` maps a PMT's program number to the
    /// mux's program index (for its Bsys). Tables that existed keep their schedule.
    pub fn set(&mut self, set: &TableSet, now: u64, pmt_program: impl Fn(u16) -> Option<usize>) {
        let mut entries = Vec::new();
        let all: Vec<usize> = (0..self.bsys.len()).collect();
        for (i, t) in set.tables.iter().enumerate() {
            let kind = EntryKind::Table(t.kind);
            let bsys = match t.kind {
                TableKind::Pat => all.clone(),
                TableKind::Pmt(p) => pmt_program(p.get()).into_iter().collect(),
                _ => Vec::new(),
            };
            self.pid_queue(t.pid, bsys);
            let period = self.periods.of(t.kind);
            let old = self
                .entries
                .iter()
                .find(|e| e.pid == t.pid && e.kind == kind);
            entries.push(Entry {
                pid: t.pid,
                kind,
                sections: t.sections.clone(),
                period,
                // New tables are spread over their first period so a whole set does not
                // fall due in one slot.
                next_due: old.map_or(now + (i as u64 * 7 * MS) % period, |o| o.next_due),
                queued: old.is_some_and(|o| o.queued),
                last_start: old.and_then(|o| o.last_start),
                max_gap: old.map_or(0, |o| o.max_gap),
            });
        }
        if self.stt.is_some() {
            self.pid_queue(Pid::PSIP_BASE, Vec::new());
            let old = self.entries.iter().find(|e| e.kind == EntryKind::Stt);
            entries.push(Entry {
                pid: Pid::PSIP_BASE,
                kind: EntryKind::Stt,
                sections: Vec::new(),
                period: self.periods.stt,
                next_due: old.map_or(now, |o| o.next_due),
                queued: old.is_some_and(|o| o.queued),
                last_start: old.and_then(|o| o.last_start),
                max_gap: old.map_or(0, |o| o.max_gap),
            });
        }
        // Pending instances refer to entries by index: re-point them, dropping those whose
        // table is gone (unless already started: those finish).
        let remap: Vec<Option<usize>> = self
            .entries
            .iter()
            .map(|o| {
                entries
                    .iter()
                    .position(|n| n.pid == o.pid && n.kind == o.kind)
            })
            .collect();
        for q in &mut self.pids {
            q.pending
                .retain_mut(|p| match remap.get(p.entry).copied().flatten() {
                    Some(n) => {
                        p.entry = n;
                        true
                    }
                    None => {
                        p.entry = usize::MAX;
                        p.started
                    }
                });
        }
        self.entries = entries;
    }

    /// Lays the schedule out again from `now` (the output starts later than the tables were
    /// set): nothing has been sent, so every table starts within its first period, spread.
    pub fn restart(&mut self, now: u64) {
        for (i, e) in self.entries.iter_mut().enumerate() {
            e.next_due = now + (i as u64 * 7 * MS) % e.period.max(1);
        }
    }

    fn enqueue_due(&mut self, now: u64, wall: Option<&WallClock>) {
        for i in 0..self.entries.len() {
            let e = &mut self.entries[i];
            if e.next_due > now {
                continue;
            }
            let due = e.next_due;
            // Next instance one period after this one was due, never in the past by more than
            // a period (after a stall the schedule resumes instead of bursting).
            e.next_due = (due + e.period).max(now.saturating_sub(e.period / 2));
            if e.queued {
                self.overruns += 1;
                continue;
            }
            let sections = match e.kind {
                EntryKind::Table(_) => e.sections.clone(),
                EntryKind::Stt => match (self.stt, wall) {
                    (Some(cfg), Some(w)) => {
                        let unix = w.unix_nanos_at(now).div_euclid(1_000_000_000) as i64;
                        let Some(g) = GpsSeconds::from_unix(unix, cfg.leap) else {
                            continue;
                        };
                        match tb_psip::tables::stt(g, cfg.leap.0, cfg.dst.at(unix)) {
                            Ok(s) => vec![s],
                            Err(_) => continue,
                        }
                    }
                    _ => {
                        self.stt_without_clock += 1;
                        continue;
                    }
                },
            };
            e.queued = true;
            let pid = e.pid;
            let Some(q) = self.pids.iter_mut().find(|q| q.pid == pid) else {
                continue;
            };
            let p = Pending {
                due,
                entry: i,
                cursor: Cursor::new(sections),
                started: false,
            };
            // Keep due order behind a started head.
            let skip = usize::from(q.pending.front().is_some_and(|h| h.started));
            let at = q
                .pending
                .iter()
                .skip(skip)
                .position(|x| x.due > due)
                .map_or(q.pending.len(), |k| k + skip);
            q.pending.insert(at, p);
        }
    }

    /// The table packet for the slot at STC `now`, if one is due and admitted.
    pub fn next_packet(&mut self, now: u64, wall: Option<&WallClock>) -> Option<Packet> {
        self.enqueue_due(now, wall);
        let mut best: Option<(u64, usize)> = None;
        for (i, q) in self.pids.iter_mut().enumerate() {
            let Some(head) = q.pending.front() else {
                continue;
            };
            if best.is_some_and(|(d, _)| d <= head.due) {
                continue;
            }
            if !q.tb.admits(now, 188) {
                continue;
            }
            if let Some(sb) = &mut q.smoothing
                && !sb.admits(now, 184)
            {
                continue;
            }
            if !q
                .bsys
                .iter()
                .all(|&b| self.bsys.get_mut(b).is_some_and(|l| l.admits(now, 184)))
            {
                continue;
            }
            best = Some((head.due, i));
        }
        let (_, i) = best?;
        let q = &mut self.pids[i];
        let head = q.pending.front_mut()?;
        if !head.started {
            head.started = true;
            if let Some(e) = self.entries.get_mut(head.entry) {
                if let Some(ls) = e.last_start {
                    e.max_gap = e.max_gap.max(now - ls);
                }
                e.last_start = Some(now);
            }
        }
        let pkt = head.cursor.next_packet(q.pid, &mut q.cc)?;
        q.tb.add(now, 188);
        if let Some(sb) = &mut q.smoothing {
            sb.add(now, 184);
        }
        for &b in &q.bsys {
            if let Some(l) = self.bsys.get_mut(b) {
                l.add(now, 184);
            }
        }
        if head.cursor.done() {
            let entry = head.entry;
            q.pending.pop_front();
            if let Some(e) = self.entries.get_mut(entry) {
                e.queued = false;
            }
        }
        Some(pkt)
    }

    /// Per-table statistics.
    pub fn stats(&self) -> Vec<TableStat> {
        self.entries
            .iter()
            .map(|e| TableStat {
                pid: e.pid,
                period: e.period,
                max_gap: e.max_gap,
            })
            .collect()
    }
}
