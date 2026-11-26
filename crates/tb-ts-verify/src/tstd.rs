//! The transport-stream system target decoder of H.222.0 §2.4.2 (p.13-19), for one
//! elementary stream or one program's system data, and the PSIP smoothing buffer of SCTE 54
//! §7.8.1.2 / A/65 §7.1.
//!
//! Time is stream time in seconds (byte position at the time base's rate). A packet's bytes
//! arrive at the TS rate over the packet's duration. Buffers are fluid: a leak at rate R moves
//! R/8 bytes per second while the buffer holds data.

use std::collections::VecDeque;

use crate::report::{BufferStats, Sink};
use crate::rules::Rule;

/// The transport buffer size, H.222.0 §2.4.2.3 (p.15).
pub(crate) const TB_SIZE: f64 = 512.0;
/// Rx for system data, H.222.0 §2.4.2.3 (p.15).
pub(crate) const RX_SYS: f64 = 1_000_000.0;
/// Rx for audio other than ADTS AAC, H.222.0 §2.4.2.3 (p.15).
pub(crate) const RX_AUDIO: f64 = 2_000_000.0;
/// Bsys, H.222.0 §2.4.2.3 (p.16).
pub(crate) const BSYS_SIZE: f64 = 1536.0;
/// The AC-3 main buffer, A/52 Annex A §A4.4 (p.120) and A/53 Part 5 §5.7 (p.7).
pub(crate) const AC3_B_SIZE: f64 = 2592.0;
/// PSIP smoothing buffer: sb_size 1024 bytes, sb_leak_rate 625 x 400 bit/s (SCTE 54 p.18).
pub(crate) const SB_SIZE: f64 = 1024.0;
pub(crate) const SB_LEAK: f64 = 250_000.0;

/// A FIFO drained at a constant rate while it holds data (TBn, TBsys, SB).
#[derive(Debug, Clone)]
pub(crate) struct Leaky {
    rate: f64,
    size: f64,
    /// When the last byte that entered will have left.
    busy_until: f64,
    last_empty: f64,
    pub(crate) max_fill: f64,
    over: bool,
    not_empty_latched: bool,
}

/// What happened to one packet in a [`Leaky`] buffer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Served {
    /// The packet's bytes leave the buffer over this interval.
    pub(crate) t0: f64,
    pub(crate) t1: f64,
    /// Fullness when the packet's last byte entered, bytes.
    pub(crate) fill: f64,
}

impl Leaky {
    pub(crate) fn new(rate: f64, size: f64, t: f64) -> Self {
        Self {
            rate,
            size,
            busy_until: t,
            last_empty: t,
            max_fill: 0.0,
            over: false,
            not_empty_latched: false,
        }
    }

    /// A packet of `bytes` arriving over [t_start, t_end].
    pub(crate) fn packet(&mut self, t_start: f64, t_end: f64, bytes: f64) -> Served {
        if t_start >= self.busy_until {
            self.last_empty = t_start;
            self.not_empty_latched = false;
        }
        let t0 = self.busy_until.max(t_start);
        let t1 = (t0 + bytes * 8.0 / self.rate).max(t_end);
        self.busy_until = t1;
        let fill = ((t1 - t_end) * self.rate / 8.0).max(0.0);
        if fill <= 1e-9 {
            self.last_empty = t_end;
            self.not_empty_latched = false;
        }
        if fill > self.max_fill {
            self.max_fill = fill;
        }
        Served { t0, t1, fill }
    }

    /// True once per episode in which the fullness exceeded the size.
    pub(crate) fn overflowed(&mut self, s: &Served) -> bool {
        if s.fill > self.size + 1e-6 {
            let first = !self.over;
            self.over = true;
            first
        } else {
            self.over = false;
            false
        }
    }

    /// True once per episode in which the buffer has not been empty for more than a second.
    pub(crate) fn not_empty_for_1s(&mut self, now: f64) -> bool {
        if now - self.last_empty > 1.0 && !self.not_empty_latched && now < self.busy_until {
            self.not_empty_latched = true;
            return true;
        }
        false
    }

    pub(crate) fn stats(&self, name: &str, pid: u16) -> BufferStats {
        BufferStats {
            buffer: name.to_string(),
            pid,
            size: self.size as u64,
            max_fill: self.max_fill.round() as u64,
        }
    }
}

/// Parameters of one elementary stream's model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EsParams {
    /// Rxn, bit/s.
    pub(crate) rx: f64,
    /// MBn size and Rbxn (leak method), bytes and bit/s; `None` for audio (TBn feeds Bn).
    pub(crate) mb: Option<(f64, f64)>,
    /// EBn (video) or Bn (audio) size, bytes.
    pub(crate) eb: f64,
    /// Maximum delay through the buffers, s.
    pub(crate) max_delay: f64,
    /// Underflow allowed (low_delay).
    pub(crate) low_delay: bool,
    /// Rule for a Bn overflow (audio) .
    pub(crate) audio: bool,
}

#[derive(Debug, Clone, Copy)]
struct Seg {
    t0: f64,
    t1: f64,
    rate: f64,
}

#[derive(Debug, Clone, Copy)]
struct Au {
    end: Option<f64>,
    time: f64,
    arrival: f64,
    removed: bool,
    /// Units in the decoding buffer at the decoding time (when the end was not yet known).
    in_at_time: Option<f64>,
}

/// The buffers of one elementary stream: TBn, then MBn and EBn (video) or Bn (audio).
///
/// Units are the bytes that leave TBn for the next buffer: PES payload for video (the PES
/// header bytes that MBn discards are not counted, a simplification of at most the size of
/// one PES header), every PES byte for audio (Bn holds headers until their access unit is
/// removed).
#[derive(Debug, Clone)]
pub(crate) struct EsModel {
    pub(crate) p: EsParams,
    pid: u16,
    tb: Leaky,
    t: f64,
    m: f64,
    pub(crate) max_m: f64,
    mb_over: bool,
    mb_last_empty: f64,
    mb_empty_latched: bool,
    /// Units that have entered EBn / Bn.
    e_in: f64,
    removed_upto: f64,
    pending: bool,
    pub(crate) max_e: f64,
    e_over: bool,
    segs: VecDeque<Seg>,
    aus: VecDeque<Au>,
    /// Units fed to the model so far (from the TS side).
    fed: f64,
}

impl EsModel {
    pub(crate) fn new(p: EsParams, pid: u16, t: f64) -> Self {
        Self {
            p,
            pid,
            tb: Leaky::new(p.rx, TB_SIZE, t),
            t,
            m: 0.0,
            max_m: 0.0,
            mb_over: false,
            mb_last_empty: t,
            mb_empty_latched: false,
            e_in: 0.0,
            removed_upto: 0.0,
            pending: false,
            max_e: 0.0,
            e_over: false,
            segs: VecDeque::new(),
            aus: VecDeque::new(),
            fed: 0.0,
        }
    }

    /// Units fed so far: the offset the next unit will have.
    pub(crate) fn fed(&self) -> f64 {
        self.fed
    }

    /// An access unit starting at unit offset `start` and decoded at `time`; the previous
    /// one ends there if its end was not given.
    pub(crate) fn au_start(&mut self, start: f64, time: f64, arrival: f64, sink: &mut Sink) {
        self.set_end(start, sink);
        // Bounded memory: never more than 4096 queued access units (over a minute of video;
        // a conformant stream holds at most 10 s), whatever the timestamps say.
        if self.aus.len() >= 4096 {
            self.aus.pop_front();
        }
        self.aus.push_back(Au {
            end: None,
            time,
            arrival,
            removed: false,
            in_at_time: None,
        });
    }

    /// The last access unit ends at unit offset `end`.
    pub(crate) fn set_end(&mut self, end: f64, sink: &mut Sink) {
        let low_delay = self.p.low_delay;
        let audio = self.p.audio;
        let pid = self.pid;
        let Some(last) = self.aus.back_mut() else {
            return;
        };
        if last.end.is_some() {
            return;
        }
        last.end = Some(end);
        if last.removed {
            // Removed before its end was known: decide the underflow now.
            if let Some(had) = last.in_at_time
                && had + 1e-6 < end
                && !low_delay
            {
                let short = end - had;
                let rule = if audio {
                    Rule::TstdBUnderflow
                } else {
                    Rule::TstdEbUnderflow
                };
                sink.emit(rule, Some(pid), || {
                    format!(
                        "{short:.0} bytes of the access unit were not in the buffer at its decoding time"
                    )
                });
            }
            self.pending = false;
            self.removed_upto = self.removed_upto.max(end);
        }
    }

    /// Fullness of EBn / Bn.
    fn e(&self) -> f64 {
        if self.pending {
            0.0
        } else {
            (self.e_in - self.removed_upto).max(0.0)
        }
    }

    /// A packet of this PID arriving over [t_start, t_end] that delivers `units` downstream.
    pub(crate) fn packet(&mut self, t_start: f64, t_end: f64, units: f64, sink: &mut Sink) {
        let s = self.tb.packet(t_start, t_end, 188.0);
        if self.tb.overflowed(&s) {
            let fill = s.fill;
            sink.emit(Rule::TstdTbOverflow, Some(self.pid), || {
                format!(
                    "TBn holds {fill:.0} bytes (size 512, Rx {:.0} bit/s)",
                    self.p.rx
                )
            });
        }
        if self.tb.not_empty_for_1s(t_end) {
            sink.emit(Rule::TstdTbEmpty, Some(self.pid), || {
                "TBn has not been empty for more than 1 s".into()
            });
        }
        self.advance(s.t0, sink);
        if units > 0.0 {
            self.segs.push_back(Seg {
                t0: s.t0,
                t1: s.t1,
                rate: units / (s.t1 - s.t0).max(1e-12),
            });
            self.fed += units;
        }
    }

    /// Runs the model up to time `t`.
    pub(crate) fn advance(&mut self, t: f64, sink: &mut Sink) {
        loop {
            let next_rm = self.aus.iter().find(|a| !a.removed).map(|a| a.time);
            match next_rm {
                Some(tr) if tr <= t => {
                    self.flow_to(tr, sink);
                    self.remove_next(sink);
                }
                _ => {
                    self.flow_to(t, sink);
                    break;
                }
            }
        }
        while self.aus.len() > 1
            && self
                .aus
                .front()
                .is_some_and(|a| a.removed && a.end.is_some())
        {
            self.aus.pop_front();
        }
    }

    fn remove_next(&mut self, sink: &mut Sink) {
        let pid = Some(self.pid);
        let e_in = self.e_in;
        let fed = self.fed;
        let t = self.t;
        let Some(a) = self.aus.iter_mut().find(|a| !a.removed) else {
            return;
        };
        a.removed = true;
        let delay = a.time - a.arrival;
        if delay > self.p.max_delay + 1e-6 {
            let max = self.p.max_delay;
            sink.emit(Rule::TstdDelay, pid, || {
                format!(
                    "access unit decoded {delay:.3} s after its first byte arrived (limit {max:.0} s)"
                )
            });
        }
        let rule = if self.p.audio {
            Rule::TstdBUnderflow
        } else {
            Rule::TstdEbUnderflow
        };
        match a.end {
            Some(end) => {
                if e_in + 1e-6 < end && !self.p.low_delay {
                    let short = end - e_in;
                    sink.emit(rule, pid, || {
                        format!("{short:.0} bytes of the access unit were not in the buffer at its decoding time {t:.6} s")
                    });
                }
                self.removed_upto = self.removed_upto.max(end);
            }
            None => {
                if e_in + 1e-6 < fed {
                    // Bytes of the access unit that have already arrived are not all in the
                    // buffer: underflow whatever the end turns out to be.
                    if !self.p.low_delay {
                        let short = fed - e_in;
                        sink.emit(rule, pid, || {
                            format!("at least {short:.0} bytes of the access unit were not in the buffer at its decoding time {t:.6} s")
                        });
                    }
                    a.in_at_time = None;
                } else {
                    a.in_at_time = Some(e_in);
                }
                self.pending = true;
                self.removed_upto = f64::INFINITY;
            }
        }
    }

    fn flow_to(&mut self, t1: f64, sink: &mut Sink) {
        while self.t < t1 {
            let (rate, until) = match self.segs.front() {
                Some(s) if s.t0 <= self.t => (s.rate, s.t1.min(t1)),
                Some(s) => (0.0, s.t0.min(t1)),
                None => (0.0, t1),
            };
            let until = until.max(self.t);
            self.integrate(rate, until, sink);
            if self.segs.front().is_some_and(|s| self.t >= s.t1) {
                self.segs.pop_front();
            }
            if until >= t1 {
                break;
            }
        }
        self.t = self.t.max(t1);
    }

    fn integrate(&mut self, r_in: f64, until: f64, sink: &mut Sink) {
        match self.p.mb {
            None => {
                let dt = until - self.t;
                self.e_in += r_in * dt;
                self.t = until;
                self.check_e(sink);
            }
            Some((mb_size, rbx)) => {
                let out_max = rbx / 8.0;
                let mut guard = 0;
                while self.t < until && guard < 64 {
                    guard += 1;
                    let room = self.p.eb - self.e();
                    let out = if room <= 1e-6 {
                        0.0
                    } else if self.m > 1e-9 {
                        out_max
                    } else {
                        r_in.min(out_max)
                    };
                    let mut dt = until - self.t;
                    if out > 0.0 {
                        dt = dt.min(room / out);
                    }
                    if self.m > 1e-9 && out > r_in {
                        dt = dt.min(self.m / (out - r_in));
                    }
                    if dt <= 1e-12 {
                        dt = (until - self.t).min(1e-9);
                    }
                    self.m = (self.m + (r_in - out) * dt).max(0.0);
                    if self.m < 1e-6 {
                        self.m = 0.0;
                    }
                    self.e_in += out * dt;
                    self.t += dt;
                    if self.m > self.max_m {
                        self.max_m = self.m;
                    }
                    if self.m > mb_size + 1e-6 {
                        if !self.mb_over {
                            self.mb_over = true;
                            let m = self.m;
                            sink.emit(Rule::TstdMbOverflow, Some(self.pid), || {
                                format!("MBn holds {m:.0} bytes (size {mb_size:.0})")
                            });
                        }
                    } else {
                        self.mb_over = false;
                    }
                    if self.m == 0.0 {
                        self.mb_last_empty = self.t;
                        self.mb_empty_latched = false;
                    } else if self.t - self.mb_last_empty > 1.0 && !self.mb_empty_latched {
                        self.mb_empty_latched = true;
                        sink.emit(Rule::TstdMbEmpty, Some(self.pid), || {
                            "MBn has not been empty for more than 1 s".into()
                        });
                    }
                    self.check_e(sink);
                }
                self.t = self.t.max(until);
            }
        }
    }

    fn check_e(&mut self, sink: &mut Sink) {
        let e = self.e();
        if e > self.max_e {
            self.max_e = e;
        }
        if self.p.audio {
            if e > self.p.eb + 1e-6 {
                if !self.e_over {
                    self.e_over = true;
                    let size = self.p.eb;
                    sink.emit(Rule::TstdBOverflow, Some(self.pid), || {
                        format!("Bn holds {e:.0} bytes (size {size:.0})")
                    });
                }
            } else {
                self.e_over = false;
            }
        }
    }

    /// End of the stream at time `t`: an audio frame decoded before the end whose bytes
    /// never all arrived underflowed (its size is known from its header; a video access
    /// unit's end is only known from the next one, so the last is not judged).
    pub(crate) fn finish(&mut self, t: f64, sink: &mut Sink) {
        self.advance(t, sink);
        if !self.p.audio || self.p.low_delay {
            return;
        }
        let pid = Some(self.pid);
        for a in &self.aus {
            if a.removed && a.end.is_none() && a.time <= t {
                let at = a.time;
                sink.emit(Rule::TstdBUnderflow, pid, || {
                    format!("the audio frame decoded at {at:.6} s never arrived whole")
                });
            }
        }
    }

    pub(crate) fn stats(&self, out: &mut Vec<BufferStats>) {
        out.push(self.tb.stats("TB", self.pid));
        if let Some((mb, _)) = self.p.mb {
            out.push(BufferStats {
                buffer: "MB".into(),
                pid: self.pid,
                size: mb as u64,
                max_fill: self.max_m.round() as u64,
            });
        }
        out.push(BufferStats {
            buffer: if self.p.audio { "B" } else { "EB" }.into(),
            pid: self.pid,
            size: self.p.eb as u64,
            max_fill: self.max_e.round() as u64,
        });
    }
}

/// TBsys and Bsys of one program (fed by PIDs 0 to 3 and the program's PMT PID).
#[derive(Debug, Clone)]
pub(crate) struct SysModel {
    pub(crate) pmt_pid: u16,
    tb: Leaky,
    b: f64,
    t: f64,
    pub(crate) max_b: f64,
    over: bool,
}

impl SysModel {
    pub(crate) fn new(pmt_pid: u16, t: f64) -> Self {
        Self {
            pmt_pid,
            tb: Leaky::new(RX_SYS, TB_SIZE, t),
            b: 0.0,
            t,
            max_b: 0.0,
            over: false,
        }
    }

    /// A packet entering TBsys, with `payload` bytes passed on to Bsys; `rsys` in bit/s.
    pub(crate) fn packet(
        &mut self,
        t_start: f64,
        t_end: f64,
        payload: f64,
        rsys: f64,
        pid: u16,
        sink: &mut Sink,
    ) {
        let s = self.tb.packet(t_start, t_end, 188.0);
        if self.tb.overflowed(&s) {
            let fill = s.fill;
            sink.emit(Rule::TstdTbsysOverflow, Some(pid), || {
                format!(
                    "TBsys of the program with PMT PID 0x{:04X} holds {fill:.0} bytes (size 512)",
                    self.pmt_pid
                )
            });
        }
        if self.tb.not_empty_for_1s(t_end) {
            sink.emit(Rule::TstdTbEmpty, Some(pid), || {
                "TBsys has not been empty for more than 1 s".into()
            });
        }
        let leak = rsys / 8.0;
        // Drain until the packet's bytes start leaving TBsys, then fill over its service.
        self.b = (self.b - leak * (s.t0 - self.t).max(0.0)).max(0.0);
        let dur = s.t1 - s.t0;
        self.b = (self.b + payload - leak * dur).max(0.0);
        self.t = s.t1;
        if self.b > self.max_b {
            self.max_b = self.b;
        }
        if self.b > BSYS_SIZE + 1e-6 {
            if !self.over {
                self.over = true;
                let b = self.b;
                sink.emit(Rule::TstdBsysOverflow, Some(pid), || {
                    format!(
                        "Bsys of the program with PMT PID 0x{:04X} holds {b:.0} bytes (size 1536)",
                        self.pmt_pid
                    )
                });
            }
        } else {
            self.over = false;
        }
    }

    pub(crate) fn stats(&self, out: &mut Vec<BufferStats>) {
        out.push(self.tb.stats("TBsys", self.pmt_pid));
        out.push(BufferStats {
            buffer: "Bsys".into(),
            pid: self.pmt_pid,
            size: BSYS_SIZE as u64,
            max_fill: self.max_b.round() as u64,
        });
    }
}

/// Rmax and VBVmax of H.262 Tables 8-13 and 8-14 (p.115-116) for the profiles and levels
/// the model knows, by profile_and_level_indication; and whether the level is High-1440 or
/// High (the MBn and Rbx formulas differ, H.222.0 §2.4.2.3).
pub(crate) fn m2v_limits(pl: u8) -> Option<(f64, f64, bool)> {
    Some(match pl {
        0x58 => (15e6, 1_835_008.0, false),  // Simple@Main
        0x4A => (4e6, 475_136.0, false),     // Main@Low
        0x48 => (15e6, 1_835_008.0, false),  // Main@Main
        0x46 => (60e6, 7_340_032.0, true),   // Main@High-1440
        0x44 => (80e6, 9_781_248.0, true),   // Main@High
        0x85 => (50e6, 9_437_184.0, false),  // 4:2:2@Main
        0x82 => (300e6, 47_185_920.0, true), // 4:2:2@High
        _ => return None,
    })
}

/// The model parameters for an MPEG-2 video stream (H.222.0 §2.4.2.3, p.14-17).
pub(crate) fn m2v_params(pl: u8, vbv_bits: f64, es_rate: f64, low_delay: bool) -> Option<EsParams> {
    let (rmax, vbv_max, high) = m2v_limits(pl)?;
    let bs_mux = 0.004 * rmax;
    let bs_oh = rmax / 750.0;
    let (mbs, rbx) = if high {
        (bs_mux + bs_oh, (1.05 * es_rate).min(rmax))
    } else {
        (bs_mux + bs_oh + vbv_max - vbv_bits, rmax)
    };
    Some(EsParams {
        rx: 1.2 * rmax,
        mb: Some((mbs / 8.0, rbx)),
        eb: vbv_bits / 8.0,
        max_delay: 1.0,
        low_delay,
        audio: false,
    })
}

/// MaxBR and MaxCPB (in 1000 bit/s and 1000 bits for cpbBrVclFactor 1000, as Table A-1 of
/// H.264 gives them) by level_idc; level 1b as level_idc 11 with constraint_set3.
fn avc_level(level_idc: u8, c3: bool) -> Option<(f64, f64)> {
    Some(match (level_idc, c3) {
        (9, _) | (11, true) => (128.0, 350.0),
        (10, _) => (64.0, 175.0),
        (11, false) => (192.0, 500.0),
        (12, _) => (384.0, 1000.0),
        (13, _) => (768.0, 2000.0),
        (20, _) => (2000.0, 2000.0),
        (21, _) | (22, _) => (4000.0, 4000.0),
        (30, _) => (10000.0, 10000.0),
        (31, _) => (14000.0, 14000.0),
        (32, _) => (20000.0, 20000.0),
        (40, _) => (20000.0, 25000.0),
        (41, _) | (42, _) => (50000.0, 62500.0),
        (50, _) => (135000.0, 135000.0),
        (51, _) | (52, _) => (240000.0, 240000.0),
        _ => return None,
    })
}

/// The model parameters for an AVC stream without NAL HRD parameters (H.222.0 §2.14.3.1,
/// p.151-152): EBn = 1200 x MaxCPB, Rbx = 1200 x MaxBR, Rx = 1.2 x the default
/// BitRate (cpbBrNalFactor x MaxBR: 1200, or 1500 for High profile).
pub(crate) fn avc_params(profile_idc: u8, level_idc: u8, c3: bool) -> Option<EsParams> {
    let (max_br, max_cpb) = avc_level(level_idc, c3)?;
    let factor = match profile_idc {
        100 => 1500.0,
        110 => 3600.0,
        122 | 244 => 4800.0,
        _ => 1200.0,
    };
    let br = 1200.0 * max_br;
    let cpb = 1200.0 * max_cpb;
    let base = br.max(2_000_000.0);
    let bs_mux = 0.004 * base;
    let bs_oh = base / 750.0;
    Some(EsParams {
        rx: 1.2 * factor * max_br,
        mb: Some(((bs_mux + bs_oh) / 8.0, br)),
        eb: cpb / 8.0,
        max_delay: 10.0,
        low_delay: false,
        audio: false,
    })
}

/// The model parameters for AC-3 (A/52 Annex A §A4.4; Rx of H.222.0 §2.4.2.3).
pub(crate) fn ac3_params() -> EsParams {
    EsParams {
        rx: RX_AUDIO,
        mb: None,
        eb: AC3_B_SIZE,
        max_delay: 1.0,
        low_delay: false,
        audio: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaky_fill() {
        // 512-byte buffer at 2 Mbit/s fed at 38.8 Mbit/s: three back-to-back packets fit,
        // the fourth does not.
        let mut l = Leaky::new(RX_AUDIO, TB_SIZE, 0.0);
        let d = 1504.0 / 38_800_000.0;
        let mut over = Vec::new();
        for k in 0..4 {
            let s = l.packet(k as f64 * d, (k + 1) as f64 * d, 188.0);
            over.push(l.overflowed(&s));
        }
        assert_eq!(over, vec![false, false, true, false]);
    }

    #[test]
    fn mpeg2_main_level_constants() {
        let p = m2v_params(0x48, 1_835_008.0, 15e6, false);
        let p = p.map(|p| (p.rx, p.mb, p.eb));
        let bs = 0.004 * 15e6 + 15e6 / 750.0;
        assert_eq!(p, Some((18e6, Some((bs / 8.0, 15e6)), 1_835_008.0 / 8.0)));
    }
}
