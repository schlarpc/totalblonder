//! The statistics of a run: the text report and the `--json` record that `gate/check.py`
//! and `gate/bench.py` read (the keys of the C++ driver, plus the ones that exist
//! only here: `device`, `failed_ticks`, `device_errors`, `device_lost`, `host_between_ms`,
//! `host_submit_ms`, `host_wait_ms`, `host_mb`).

use std::fmt::Write as _;

use tb_m2v_rate::control::Stats as RateStats;

use tb_engine::engine::{CLASS_NAMES, CLASSES, ClassTimes};

/// Pictures of one type of session 0 of group 0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TypeStats {
    /// Pictures.
    pub count: u64,
    /// Their bytes.
    pub bytes: f64,
    /// Sum of their mean quantiser_scale (coded pictures only).
    pub qsum: f64,
}

/// One group of the line-up.
#[derive(Clone, Debug, PartialEq)]
#[allow(missing_docs, reason = "the fields of the C++ report line")]
pub struct GroupReport {
    pub pool: bool,
    pub w: u32,
    pub h: u32,
    pub interlaced: bool,
    pub fps: f64,
    pub coded: (i32, i32),
    pub gop: u32,
    pub m: u32,
    pub stagger: bool,
    pub f_code: i32,
    pub range2: i32,
    pub field_pred: bool,
    pub aq: bool,
    pub sessions: u32,
    pub frames: u64,
    pub priority: i32,
    pub mean_mbps: f64,
    pub repeats: u64,
    pub pictures: u64,
}

/// Everything measured in a run.
#[derive(Clone, Debug, PartialEq)]
#[allow(missing_docs, reason = "the names of the C++ report and its JSON keys")]
pub struct Report {
    pub device: String,
    pub w: u32,
    pub h: u32,
    pub interlaced: bool,
    pub fps: f64,
    /// Sessions of the groups that are not pools (at least 1).
    pub sessions: u64,
    pub frames: u64,
    pub groups: Vec<GroupReport>,
    /// Ticks in the timing statistics (after the warm-up, while input lasts).
    pub ticks: u64,
    pub paced: bool,
    pub overlap: bool,
    pub input_copy: bool,
    /// `--input-mode dev`: the clips are held on the device.
    pub dev_input: bool,
    pub wall_s: f64,
    pub video_s: f64,
    /// Macroblocks coded on the device.
    pub mbs: u64,
    pub pictures: u64,
    pub period_ms: f64,
    pub tick_mean_ms: f64,
    pub tick_p50_ms: f64,
    pub tick_p99_ms: f64,
    pub tick_p999_ms: f64,
    pub tick_max_ms: f64,
    /// Ticks longer than the period.
    pub late: u64,
    /// Ticks whose output did not exist when the next tick was due (paced runs).
    pub late_deadline: u64,
    pub cpu_s: f64,
    pub bits_all: f64,
    /// Bytes of the device arena taken by batches.
    pub dev_bytes: u64,
    /// Bytes of the host arena taken by batches and clips.
    pub host_bytes: u64,
    pub arena_dev_bytes: u64,
    pub arena_host_bytes: u64,
    pub load_before: f64,
    pub load_after: f64,
    pub repeats: u64,
    /// Pictures given up by the deadline policy.
    pub shed: u64,
    pub keys_missed: u64,
    /// Access units by display duration in display units (index 1, 2, 3): fields of an
    /// interlaced session, frame periods of a progressive one. Film mode shows as pictures of
    /// three fields, or of two and three frame periods.
    pub shown: [u64; 4],
    pub churn: bool,
    pub pool_opens: u64,
    pub pool_closes: u64,
    pub pool_waits: u64,
    pub pool_wait_ms: f64,
    pub pool_wait_max: f64,
    pub op_ticks: u64,
    pub op_ticks_late: u64,
    pub up_bytes: f64,
    pub down_bytes: f64,
    /// Rate-control counters summed over the open sessions.
    pub rate: RateStats,
    /// Least room of any rate-controlled session, as a fraction of its buffer (1e18: none).
    pub room_min: f64,
    pub s0: [TypeStats; 4],
    pub s0_cbr: bool,
    pub profile: bool,
    /// Ticks run in all.
    pub total_ticks: u64,
    pub classes: [ClassTimes; CLASSES],
    /// Host time between a completion and the next submit, summed over the run.
    pub host_between_s: f64,
    /// Ticks that ended with a failed plan.
    pub failed_ticks: u64,
    /// Completions that carried an error.
    pub device_errors: u64,
    pub device_lost: bool,
}

const MIB: f64 = 1_048_576.0;

impl Report {
    fn per_tick_ms(&self, seconds: f64) -> f64 {
        1e3 * seconds / self.total_ticks.max(1) as f64
    }

    /// Fraction of the timed ticks that were on time.
    pub fn ontime_frac(&self) -> f64 {
        let late = if self.paced {
            self.late_deadline
        } else {
            self.late
        };
        1.0 - late as f64 / self.ticks.max(1) as f64
    }

    /// Macroblocks per second of wall time.
    pub fn mb_per_s(&self) -> f64 {
        self.mbs as f64 / self.wall_s
    }

    /// The stage profile: milliseconds per tick by wait class (device-side time when it was
    /// measured, else the host's wait), and the host's own work.
    pub fn stages_ms(&self) -> Vec<(&'static str, f64)> {
        let mut v = Vec::new();
        for (name, c) in CLASS_NAMES.iter().zip(&self.classes) {
            if c.plans == 0 || *name == "settle" {
                continue;
            }
            let t = if c.device > 0.0 { c.device } else { c.wait };
            v.push((*name, self.per_tick_ms(t)));
        }
        let submit: f64 = self.classes.iter().map(|c| c.submit).sum();
        v.push(("host: queueing plans", self.per_tick_ms(submit)));
        v.push((
            "host: encoder between phases",
            self.per_tick_ms(self.host_between_s),
        ));
        v
    }

    /// The report as the C++ prints it.
    pub fn text(&self) -> String {
        let mut s = String::new();
        let pics = self.pictures.max(1) as f64;
        for (gi, g) in self.groups.iter().enumerate() {
            let ip = if g.interlaced { "i" } else { "p" };
            if g.pool {
                let _ = writeln!(
                    s,
                    "group {gi}: pool of {} slots, {}x{}{ip}",
                    g.sessions, g.w, g.h
                );
                continue;
            }
            if self.repeats > 0 {
                let _ = writeln!(
                    s,
                    "group {gi}: priority {}, repeated pictures {} of {} = {:.2}%",
                    g.priority,
                    g.repeats,
                    g.pictures,
                    100.0 * g.repeats as f64 / g.pictures.max(1) as f64
                );
            }
            let _ = writeln!(
                s,
                "group {gi}: {}x{}{ip} {:.2} fps, coded {}x{}, GOP {} M {}{}, f_code {}, search \
                 +-{} pel, field pred {}, aq {}, sessions {}, frames {}, mean {:.3} Mbit/s",
                g.w,
                g.h,
                g.fps,
                g.coded.0,
                g.coded.1,
                g.gop,
                g.m,
                if g.stagger { " staggered" } else { "" },
                g.f_code,
                4 * g.range2 + 3,
                u8::from(g.field_pred),
                u8::from(g.aq),
                g.sessions,
                g.frames,
                g.mean_mbps
            );
        }
        let _ = writeln!(s, "device: {}", self.device);
        let _ = writeln!(
            s,
            "input {}, upload {}, {}; device memory {:.1} MB in batches ({:.1} MB/session) of \
             {:.1} MB, host arena {:.1} of {:.1} MB",
            if self.dev_input {
                "on the device (no host frames)"
            } else if self.input_copy {
                "copied into the frame buffers"
            } else {
                "uploaded from the clips in the host arena"
            },
            if self.overlap {
                "overlapped (+1 frame delay)"
            } else {
                "serial"
            },
            if self.paced { "paced" } else { "free-run" },
            self.dev_bytes as f64 / MIB,
            self.dev_bytes as f64 / MIB / self.sessions as f64,
            self.arena_dev_bytes as f64 / MIB,
            self.host_bytes as f64 / MIB,
            self.arena_host_bytes as f64 / MIB,
        );
        let _ = writeln!(
            s,
            "wall {:.3} s, {:.2}x real time, {:.0} macroblocks/s, {} pictures",
            self.wall_s,
            self.video_s / self.wall_s,
            self.mb_per_s(),
            self.pictures
        );
        let _ = writeln!(
            s,
            "tick ms: mean {:.3} p50 {:.3} p99 {:.3} p99.9 {:.3} max {:.3}, period {:.3}, late \
             {} (deadline {}) of {}",
            self.tick_mean_ms,
            self.tick_p50_ms,
            self.tick_p99_ms,
            self.tick_p999_ms,
            self.tick_max_ms,
            self.period_ms,
            self.late,
            self.late_deadline,
            self.ticks
        );
        let _ = writeln!(
            s,
            "host cpu {:.3} s = {:.3} cores total, {:.4} per session; pcie up {:.1} MB/s down \
             {:.2} MB/s",
            self.cpu_s,
            self.cpu_s / self.wall_s,
            self.cpu_s / self.wall_s / self.sessions as f64,
            self.up_bytes / self.wall_s / 1e6,
            self.down_bytes / self.wall_s / 1e6
        );
        let _ = writeln!(
            s,
            "repeated pictures (made on the host) {} of {} = {:.2}%, of them by the deadline \
             policy {}; key frames missed {}",
            self.repeats,
            self.pictures,
            100.0 * self.repeats as f64 / pics,
            self.shed,
            self.keys_missed
        );
        if self.shown[3] > 0 {
            let _ = writeln!(
                s,
                "film mode: pictures shown for 1 / 2 / 3 display units {} / {} / {}",
                self.shown[1], self.shown[2], self.shown[3]
            );
        }
        if self.churn {
            let _ = writeln!(
                s,
                "churn: {} opens, {} closes; ticks with an operation {}, of them late {}; from \
                 the request to the open: mean {:.3} ms, max {:.3} ms ({} opens)",
                self.pool_opens,
                self.pool_closes,
                self.op_ticks,
                self.op_ticks_late,
                if self.pool_waits > 0 {
                    self.pool_wait_ms / self.pool_waits as f64
                } else {
                    0.0
                },
                self.pool_wait_max,
                self.pool_waits
            );
        }
        for (t, name) in [(1, 'I'), (2, 'P'), (3, 'B')] {
            let x = &self.s0[t];
            if x.count > 0 {
                let _ = writeln!(
                    s,
                    "session 0 {name}: {} pictures, mean {:.0} bytes, mean quantiser_scale {:.1}",
                    x.count,
                    x.bytes / x.count as f64,
                    x.qsum / x.count as f64
                );
            }
        }
        if self.s0_cbr {
            let r = &self.rate;
            let _ = writeln!(
                s,
                "rate control: buffer violations {}, least room {:.1}% of vbv_buffer_size, extra \
                 passes {} ({:.3} per picture), repeated transforms {} ({:.4} per picture), \
                 ladder pictures {}, skipped pictures {}, I replaced by P {}, passes for the \
                 macroblock size limit {}",
                r.violations,
                100.0 * self.room_min,
                r.retries,
                r.retries as f64 / pics,
                r.rexf,
                r.rexf as f64 / pics,
                r.rung_pics,
                r.skip_pics,
                r.isub,
                r.mbfix
            );
        }
        let _ = writeln!(s, "host and device time, ms per tick:");
        for (name, ms) in self.stages_ms() {
            let _ = writeln!(s, "  {name:<34} {ms:8.3} ms");
        }
        if self.failed_ticks > 0 || self.device_errors > 0 || self.device_lost {
            let _ = writeln!(
                s,
                "device errors: {} failed completions, {} ticks ended with a failed plan{}",
                self.device_errors,
                self.failed_ticks,
                if self.device_lost {
                    "; THE DEVICE WAS LOST"
                } else {
                    ""
                }
            );
        }
        let _ = writeln!(
            s,
            "loadavg before {:.2} after {:.2}",
            self.load_before, self.load_after
        );
        s
    }

    /// The `--json` record.
    pub fn json(&self) -> String {
        let mut s = String::new();
        let pics = self.pictures.max(1) as f64;
        let r = &self.rate;
        let _ = writeln!(
            s,
            "{{\"w\":{},\"h\":{},\"interlaced\":{},\"fps\":{:.4},\"sessions\":{},\"frames\":{},\
             \"groups\":{},\"ticks\":{},\"paced\":{},\"overlap\":{},",
            self.w,
            self.h,
            u8::from(self.interlaced),
            self.fps,
            self.sessions,
            self.frames,
            self.groups.len(),
            self.ticks,
            u8::from(self.paced),
            u8::from(self.overlap)
        );
        let _ = writeln!(
            s,
            " \"wall_s\":{:.4},\"rt\":{:.4},\"mb_per_s\":{:.0},\"period_ms\":{:.4},\
             \"tick_mean_ms\":{:.4},\"tick_p50_ms\":{:.4},\"tick_p99_ms\":{:.4},\
             \"tick_p999_ms\":{:.4},\"tick_max_ms\":{:.4},",
            self.wall_s,
            self.video_s / self.wall_s,
            self.mb_per_s(),
            self.period_ms,
            self.tick_mean_ms,
            self.tick_p50_ms,
            self.tick_p99_ms,
            self.tick_p999_ms,
            self.tick_max_ms
        );
        let _ = writeln!(
            s,
            " \"late_proc\":{},\"late_deadline\":{},\"ontime_frac\":{:.6},\"cpu_cores\":{:.5},\
             \"mbps_per_session\":{:.4},\"dev_mb\":{:.1},\"load_before\":{:.2},\
             \"load_after\":{:.2},",
            self.late,
            self.late_deadline,
            self.ontime_frac(),
            self.cpu_s / self.wall_s,
            self.bits_all / self.sessions as f64 / self.video_s / 1e6,
            self.dev_bytes as f64 / MIB,
            self.load_before,
            self.load_after
        );
        let _ = writeln!(
            s,
            " \"repeats\":{},\"shed\":{},\"keys_missed\":{},\"dev_input\":{},\"pool_opens\":{},\
             \"pool_closes\":{},\"op_ticks\":{},\"op_ticks_late\":{},\"up_mb_s\":{:.2},\
             \"down_mb_s\":{:.3},\"shown\":[{},{},{}],",
            self.repeats,
            self.shed,
            self.keys_missed,
            u8::from(self.dev_input),
            self.pool_opens,
            self.pool_closes,
            self.op_ticks,
            self.op_ticks_late,
            self.up_bytes / self.wall_s / 1e6,
            self.down_bytes / self.wall_s / 1e6,
            self.shown[1],
            self.shown[2],
            self.shown[3]
        );
        let submit: f64 = self.classes.iter().map(|c| c.submit).sum();
        let wait: f64 = self.classes.iter().map(|c| c.wait).sum();
        let _ = writeln!(
            s,
            " \"device\":\"{}\",\"failed_ticks\":{},\"device_errors\":{},\"device_lost\":{},\
             \"host_between_ms\":{:.4},\"host_submit_ms\":{:.4},\"host_wait_ms\":{:.4},\
             \"host_mb\":{:.1},\"input_copy\":{},",
            self.device.replace('\\', "\\\\").replace('"', "\\\""),
            self.failed_ticks,
            self.device_errors,
            u8::from(self.device_lost),
            self.per_tick_ms(self.host_between_s),
            self.per_tick_ms(submit),
            self.per_tick_ms(wait),
            self.host_bytes as f64 / MIB,
            u8::from(self.input_copy)
        );
        let _ = write!(
            s,
            " \"underflows\":{},\"retries_per_picture\":{:.4},\"ladder_pictures\":{},\
             \"skipped_pictures\":{},\"i_replaced\":{},\"mb_limit_passes\":{},\"room_min_frac\":{:.4},\"pictures\":{},\
             \"stages_ms\":{{",
            r.violations,
            r.retries as f64 / pics,
            r.rung_pics,
            r.skip_pics,
            r.isub,
            r.mbfix,
            if self.room_min > 1e17 {
                0.0
            } else {
                self.room_min
            },
            self.pictures
        );
        if self.profile {
            for (i, (name, ms)) in self.stages_ms().iter().enumerate() {
                let _ = write!(s, "{}\"{name}\":{ms:.4}", if i > 0 { "," } else { "" });
            }
        }
        s.push_str("}}\n");
        s
    }
}
