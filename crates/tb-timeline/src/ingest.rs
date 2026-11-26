//! The decisions of an input's decode thread: a port of `Input::run`, `Input::push` and
//! `Input::take` of the C++ decoder of the CUDA spike (docs/ARCHITECTURE.md, "History") without
//! libav, threads or clocks.
//!
//! - [`PacketGate`]: what to do with a demuxed packet of a paced input that is early or late
//!   (`--degrade`, `--resync`);
//! - [`FrameTimes`]: a decoded frame's presentation time on the input's own clock,
//!   with the constant-rate path that takes times from frame indices;
//! - [`push_bounded`], [`newest_due`], [`take_ready`]: the queue rules of the C++
//!   `Input::push` and `Input::take`.
//!
//! All times are seconds in `f64` and every expression keeps the C++'s operations and their
//! order, so the values are bit-identical to the harness's (Rust never contracts `a * b + c`
//! into a fused multiply-add on its own).

use std::collections::VecDeque;

/// What the decode thread does with one packet, in this order: set the decoder's `skip_frame`
/// if asked, sleep, then either drop the packet or (after a flush if asked) send it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateAction {
    /// Seconds to sleep before going on (0.0: none).
    pub sleep: f64,
    /// `Some(true)`: set `skip_frame = AVDISCARD_NONREF`; `Some(false)`: back to
    /// `AVDISCARD_DEFAULT`; `None`: leave it.
    pub discard_nonref: Option<bool>,
    /// Do not send this packet (a resynchronization skips it).
    pub drop: bool,
    /// Flush the decoder before sending this packet (a resynchronization ends on this key
    /// packet).
    pub flush: bool,
}

/// The pacing gate of a decode thread: degrade and resynchronize.
#[derive(Clone, Debug)]
pub struct PacketGate {
    degrade: f64,
    resync: f64,
    degraded: bool,
    skipping: bool,
    /// Times decoding of non-reference pictures was suspended (`Input::degrades`).
    pub degrades: u64,
    /// Resynchronizations completed (`Input::resyncs`).
    pub resyncs: u64,
    /// Packets skipped by resynchronizations (`Input::skipped`).
    pub skipped: u64,
}

impl PacketGate {
    /// `degrade_s`: seconds behind at which the decoder stops decoding non-reference pictures
    /// (0: never). `resync_s`: seconds behind at which the thread skips to the next key packet
    /// (0: never). (`DecOptions::degrade`, `DecOptions::resync`.)
    pub fn new(degrade_s: f64, resync_s: f64) -> Self {
        Self {
            degrade: degrade_s,
            resync: resync_s,
            degraded: false,
            skipping: false,
            degrades: 0,
            resyncs: 0,
            skipped: 0,
        }
    }

    /// Non-reference pictures are not being decoded.
    pub fn degraded(&self) -> bool {
        self.degraded
    }

    /// Packets are being skipped until the next key packet.
    pub fn skipping(&self) -> bool {
        self.skipping
    }

    /// One video packet. `behind`: `now - due` in seconds for a packet of a paced input that has
    /// a timestamp (`o.paced && dts != AV_NOPTS_VALUE`), else `None`; `key`: `AV_PKT_FLAG_KEY`.
    ///
    /// The order of the C++: the degrade hysteresis (on above `degrade`, off below a
    /// quarter of it); then sleep when the packet is early, else start skipping when it is
    /// more than `resync` late; then, while skipping, non-key packets are dropped and counted
    /// and a key packet flushes the decoder, ends the skipping and counts a resynchronization.
    ///
    /// The C++ tests `due > now` for the sleep; here that is `behind < 0`, the same comparison
    /// for finite times, and the sleep is `-behind`, the same value as `due - now`.
    pub fn packet(&mut self, behind: Option<f64>, key: bool) -> GateAction {
        let mut a = GateAction {
            sleep: 0.0,
            discard_nonref: None,
            drop: false,
            flush: false,
        };
        if let Some(behind) = behind {
            if self.degrade > 0.0 {
                if !self.degraded && behind > self.degrade {
                    a.discard_nonref = Some(true);
                    self.degraded = true;
                    self.degrades += 1;
                } else if self.degraded && behind < 0.25 * self.degrade {
                    a.discard_nonref = Some(false);
                    self.degraded = false;
                }
            }
            if behind < 0.0 {
                a.sleep = -behind;
            } else if self.resync > 0.0 && behind > self.resync && !self.skipping {
                self.skipping = true;
            }
        }
        if self.skipping {
            if !key {
                self.skipped += 1;
                a.drop = true;
                return a;
            }
            a.flush = true;
            self.skipping = false;
            self.resyncs += 1;
        }
        a
    }
}

/// Frame times of one input (the locals of `Input::run`).
///
/// Timestamps are in the stream's time base; `None` is `AV_NOPTS_VALUE`.
#[derive(Clone, Debug)]
pub struct FrameTimes {
    fps: f64,
    tb: f64,
    cfr: bool,
    dur: f64,
    first: Option<i64>,
    off: f64,
    last_t: f64,
    shift: f64,
    pts0: Option<i64>,
    /// The presentation timestamp of the pass's first packet ([`FrameTimes::packet_pts`]).
    lap_pts: Option<i64>,
    loop_max: i64,
    base: f64,
}

impl FrameTimes {
    /// `fps`: the input's frame rate (`Input::fps`); `tb`: seconds per timestamp unit
    /// (`av_q2d(time_base)`); `cfr`: frame times from frame indices (`DecOptions::cfr`).
    pub fn new(fps: f64, tb: f64, cfr: bool) -> Self {
        Self {
            fps,
            tb,
            cfr,
            dur: 1.0 / fps,
            first: None,
            off: 0.0,
            last_t: 0.0,
            shift: 0.0,
            pts0: None,
            lap_pts: None,
            loop_max: -1,
            base: 0.0,
        }
    }

    /// The presentation timestamp of a video packet read from the demuxer, sent or not. The
    /// first one of a pass is remembered as where the pass begins.
    ///
    /// Deviation from the C++ decoder, which takes the first frame the decoder RETURNS as frame 0
    /// of a pass (`pts0`): when a pass begins while the thread skips to a key frame (a
    /// resynchronization across the end of a looping file), the first frame returned is not the
    /// first of the pass, every frame time of the pass and of all later ones is early by the
    /// skipped part, and the input stays that late for the frame clock for good (measured with both
    /// harnesses: 4 of 24 inputs 2.2 s late for the rest of a 120 s run, their channels on the
    /// slate). With the pass's first packet as the reference the frames keep their places. Without
    /// a call (or when the first frame is earlier than the first packet) the C++ rule applies, so
    /// ordinary passes give the same times.
    pub fn packet_pts(&mut self, pts: Option<i64>) {
        if self.lap_pts.is_none() {
            self.lap_pts = pts;
        }
    }

    /// Difference of two timestamps times the time base, as the C++ computes it (an integer
    /// difference converted to double).
    fn span(&self, ts: i64, from: i64) -> f64 {
        ts.wrapping_sub(from) as f64 * self.tb
    }

    /// A video packet read from the demuxer: `dts` is the packet's dts, or its
    /// pts when it has no dts. The first one of a pass latches `first` (0 when it has none).
    ///
    /// Returns the packet's time on the input's clock, `off + (dts - first) * tb`, for a
    /// packet with a timestamp. For the due time of a paced packet use [`FrameTimes::due`],
    /// which adds in the C++'s order.
    pub fn packet(&mut self, dts: Option<i64>) -> Option<f64> {
        let first = *self.first.get_or_insert(dts.unwrap_or(0));
        dts.map(|d| self.off + self.span(d, first))
    }

    /// [`FrameTimes::packet`], then the wall-clock time the packet is due:
    /// `t0 + shift + off + (dts - first) * tb - lead`, evaluated left to right. `t0`: wall-clock
    /// time of stream time 0; `lead`: `DecOptions::lead`.
    pub fn due(&mut self, dts: Option<i64>, t0: f64, lead: f64) -> Option<f64> {
        let first = *self.first.get_or_insert(dts.unwrap_or(0));
        dts.map(|d| t0 + self.shift + self.off + self.span(d, first) - lead)
    }

    /// A frame returned by the decoder: its presentation time on the input's
    /// clock, seconds since its start, continuous over loops. `ts`: the frame's
    /// `best_effort_timestamp`.
    ///
    /// Without a timestamp: the last frame's time plus one frame. With one: its distance from
    /// the pass's first packet; and for constant-rate inputs the frame's index in the pass
    /// (from the timestamp, rounded as C's `lround`: half away from zero, which is
    /// `f64::round`) over the rate, so that timestamps rounded to milliseconds give exact times
    /// and frames the decoder never returns leave a gap.
    ///
    /// (A frame cannot come before the pass's first packet; if it did, `first` counts as 0.)
    pub fn frame(&mut self, ts: Option<i64>) -> f64 {
        let mut t = match ts {
            None => self.last_t + self.dur,
            Some(ts) => self.off + self.span(ts, self.first.unwrap_or(0)),
        };
        self.last_t = t;
        if let (true, Some(ts)) = (self.cfr, ts) {
            let pts0 = *self.pts0.get_or_insert(match self.lap_pts {
                Some(p) if p < ts => p,
                _ => ts,
            });
            // `as` saturates where the C++ would be undefined
            let idx = ((self.span(ts, pts0) * self.fps).round() as i64).max(0);
            self.loop_max = self.loop_max.max(idx);
            t = self.base + idx as f64 / self.fps;
        }
        t += self.shift;
        t
    }

    /// The input ended and starts again from its first packet. Call it after
    /// the decoder was drained (every frame of the pass went through [`FrameTimes::frame`]).
    pub fn restart(&mut self) {
        self.off = self.last_t + self.dur;
        self.first = None;
        self.base += (self.loop_max + 1) as f64 / self.fps;
        self.pts0 = None;
        self.lap_pts = None;
        self.loop_max = -1;
    }

    /// The source delivered nothing for `seconds`: everything after is that much
    /// later.
    pub fn add_shift(&mut self, seconds: f64) {
        self.shift += seconds;
    }

    /// The sum of the stalls so far.
    pub fn shift(&self) -> f64 {
        self.shift
    }
}

/// Append a decoded frame to an input's queue: a queue longer than `qmax` loses
/// its oldest entries. Returns how many were dropped (`Input::dropped`); they are dropped here.
///
/// (Offline runs wait for room instead, as in the C++: the caller waits while
/// `q.len() >= qmax` before it calls this.)
pub fn push_bounded<T>(q: &mut VecDeque<(f64, T)>, qmax: usize, t: f64, frame: T) -> usize {
    q.push_back((t, frame));
    let mut dropped = 0;
    while q.len() > qmax {
        q.pop_front();
        dropped += 1;
    }
    dropped
}

/// The newest queued frame with `t <= limit` (the inner loop of `Input::take`):
/// pops every entry that is due from the front, returns the last of them with its time, and the
/// number of older ones that were released on the way (they are dropped here).
///
/// Offline runs call it in rounds (as in the C++: wait for [`take_ready`], take, and go round
/// again while the queue is empty and the input has not ended). A frame returned by an earlier
/// round is skipped when a later round returns one: the caller adds 1 to the count then.
pub fn newest_due<T>(q: &mut VecDeque<(f64, T)>, limit: f64) -> (Option<(f64, T)>, usize) {
    let mut got = None;
    let mut skipped = 0;
    while q.front().is_some_and(|e| e.0 <= limit) {
        if got.is_some() {
            skipped += 1;
        }
        got = q.pop_front();
    }
    (got, skipped)
}

/// The wait condition of an offline `take` (the C++ one without `stop`): the queue shows a frame
/// beyond the limit, or is full, or the input has ended. Until then "the newest frame up to the
/// limit" would depend on thread timing.
pub fn take_ready<T>(q: &VecDeque<(f64, T)>, limit: f64, qmax: usize, ended: bool) -> bool {
    q.back().is_some_and(|e| e.0 > limit) || q.len() >= qmax || ended
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: GateAction = GateAction {
        sleep: 0.0,
        discard_nonref: None,
        drop: false,
        flush: false,
    };

    #[test]
    fn early_packets_sleep_and_unpaced_packets_pass() {
        let mut g = PacketGate::new(0.25, 0.6);
        assert_eq!(g.packet(None, false), NONE);
        assert_eq!(
            g.packet(Some(-0.125), false),
            GateAction {
                sleep: 0.125,
                ..NONE
            }
        );
        assert_eq!(g.packet(Some(0.0), true), NONE);
        assert_eq!(g.packet(Some(0.2), false), NONE);
        assert_eq!((g.degrades, g.resyncs, g.skipped), (0, 0, 0));
    }

    #[test]
    fn degrade_goes_on_above_the_threshold_and_off_below_a_quarter() {
        let mut g = PacketGate::new(0.25, 0.0);
        assert_eq!(g.packet(Some(0.25), false), NONE, "not above");
        assert_eq!(
            g.packet(Some(0.26), false),
            GateAction {
                discard_nonref: Some(true),
                ..NONE
            }
        );
        assert!(g.degraded());
        // stays on between the thresholds, without repeating the order
        for b in [0.5, 0.2, 0.07, 0.0625] {
            assert_eq!(g.packet(Some(b), false), NONE);
        }
        assert_eq!(
            g.packet(Some(0.06), false),
            GateAction {
                discard_nonref: Some(false),
                ..NONE
            }
        );
        assert!(!g.degraded());
        assert_eq!(g.packet(Some(0.2), false), NONE);
        // caught up so far that the packet is early: both in one action
        assert_eq!(g.packet(Some(0.3), false).discard_nonref, Some(true));
        assert_eq!(
            g.packet(Some(-0.01), false),
            GateAction {
                sleep: 0.01,
                discard_nonref: Some(false),
                ..NONE
            }
        );
        assert_eq!(g.degrades, 2);
        // unpaced packets leave the state alone
        assert_eq!(g.packet(Some(0.3), false).discard_nonref, Some(true));
        assert_eq!(g.packet(None, false), NONE);
        assert!(g.degraded());
        // off: never
        let mut g = PacketGate::new(0.0, 0.0);
        assert_eq!(g.packet(Some(100.0), false), NONE);
        assert_eq!(g.degrades, 0);
    }

    #[test]
    fn resync_skips_to_the_next_key_packet() {
        let mut g = PacketGate::new(0.0, 0.6);
        assert_eq!(g.packet(Some(0.6), false), NONE, "not above");
        let skip = GateAction { drop: true, ..NONE };
        // the packet that trips it is itself skipped when it is not a key packet
        assert_eq!(g.packet(Some(0.61), false), skip);
        assert!(g.skipping());
        // whatever the lateness now, also without a timestamp, also when early (it still
        // sleeps: the thread stays paced while it skips)
        assert_eq!(g.packet(Some(0.1), false), skip);
        assert_eq!(g.packet(None, false), skip);
        assert_eq!(
            g.packet(Some(-0.5), false),
            GateAction { sleep: 0.5, ..skip }
        );
        assert_eq!((g.skipped, g.resyncs), (4, 0));
        assert_eq!(
            g.packet(Some(0.2), true),
            GateAction {
                flush: true,
                ..NONE
            }
        );
        assert!(!g.skipping());
        assert_eq!((g.skipped, g.resyncs), (4, 1));
        assert_eq!(g.packet(Some(0.2), false), NONE);
        // tripped by a key packet: flushed at once, nothing skipped
        assert_eq!(
            g.packet(Some(0.7), true),
            GateAction {
                flush: true,
                ..NONE
            }
        );
        assert_eq!((g.skipped, g.resyncs), (4, 2));
        // still late at the key packet: it does not trip again in the same call, but the next
        // late packet does
        assert_eq!(g.packet(Some(0.7), false), skip);
        assert_eq!(
            g.packet(Some(0.9), true),
            GateAction {
                flush: true,
                ..NONE
            }
        );
        assert_eq!(g.packet(Some(0.9), false), skip);
        assert_eq!((g.skipped, g.resyncs), (6, 3));
    }

    #[test]
    fn degrade_and_resync_together() {
        let mut g = PacketGate::new(0.25, 0.6);
        assert_eq!(
            g.packet(Some(0.3), false),
            GateAction {
                discard_nonref: Some(true),
                ..NONE
            }
        );
        assert_eq!(
            g.packet(Some(0.7), false),
            GateAction { drop: true, ..NONE }
        );
        // the key packet arrives when the thread has caught up
        assert_eq!(
            g.packet(Some(0.01), true),
            GateAction {
                discard_nonref: Some(false),
                flush: true,
                ..NONE
            }
        );
        assert_eq!((g.degrades, g.resyncs, g.skipped), (1, 1, 1));
    }

    /// Matroska: time base 1 ms, timestamps rounded to milliseconds. The constant-rate path
    /// gives exactly `idx / fps`.
    #[test]
    fn cfr_times_are_exact_from_millisecond_timestamps() {
        let fps = 30000.0 / 1001.0;
        let mut ft = FrameTimes::new(fps, 0.001, true);
        let ms = |k: i64| (k as f64 * 1001.0 / 30.0).round() as i64;
        assert_eq!(ft.packet(Some(ms(0))), Some(0.0));
        for k in 0..1000 {
            assert_eq!(
                ft.frame(Some(ms(k))).to_bits(),
                (k as f64 / fps).to_bits(),
                "frame {k}"
            );
        }
        // without cfr the times carry the rounding
        let mut ft = FrameTimes::new(fps, 0.001, false);
        ft.packet(Some(0));
        assert_eq!(ft.frame(Some(33)), 33.0 * 0.001);
        assert_eq!(ft.frame(Some(67)), 67.0 * 0.001);
        // and a frame without a timestamp follows the last one by a frame
        assert_eq!(ft.frame(None), 67.0 * 0.001 + 1.0 / fps);
    }

    #[test]
    fn cfr_index_counts_from_the_first_frame_and_leaves_gaps() {
        // 25 fps in a 90 kHz time base starting at 10 s; B pictures: decode order differs
        let mut ft = FrameTimes::new(25.0, 1.0 / 90000.0, true);
        let ts = |k: i64| 900_000 + k * 3600;
        // the first packet's dts is two frames before the first pts
        assert_eq!(ft.packet(Some(ts(-2))), Some(0.0));
        assert_eq!(ft.frame(Some(ts(0))), 0.0);
        assert_eq!(ft.frame(Some(ts(1))), 1.0 / 25.0);
        // frames 2 to 4 never returned (a resynchronization): the times do not shift
        assert_eq!(ft.frame(Some(ts(5))), 5.0 / 25.0);
        // a frame before the first one (open GOP after a seek) is clamped to index 0
        assert_eq!(ft.frame(Some(ts(-1))), 0.0);
        // no timestamp in cfr mode: the uncorrected path
        let t = ft.frame(None);
        assert_eq!(t, (ts(-1) - ts(-2)) as f64 * (1.0 / 90000.0) + 1.0 / 25.0);
    }

    #[test]
    fn cfr_times_continue_across_a_loop() {
        let fps = 30000.0 / 1001.0;
        let mut ft = FrameTimes::new(fps, 0.001, true);
        let ms = |k: i64| (k as f64 * 1001.0 / 30.0).round() as i64;
        let mut times = Vec::new();
        for _pass in 0..3 {
            for k in 0..300 {
                ft.packet(Some(ms(k)));
                times.push(ft.frame(Some(ms(k))));
            }
            ft.restart();
        }
        // frame n of the run is at base + k / fps with base = 300 / fps added per pass, in the
        // C++'s operations
        let mut base = 0.0;
        for pass in 0..3 {
            for k in 0..300 {
                let want: f64 = base + k as f64 / fps;
                assert_eq!(times[pass * 300 + k].to_bits(), want.to_bits());
            }
            base += 300.0 / fps;
        }
        // monotonic, one frame apart within float error
        for w in times.windows(2) {
            assert!((w[1] - w[0] - 1.0 / fps).abs() < 1e-9);
        }
    }

    #[test]
    fn loop_restart_and_shift_without_cfr() {
        let mut ft = FrameTimes::new(50.0, 0.001, false);
        let d = 20.0 * 0.001;
        assert_eq!(ft.due(Some(1000), 100.0, 0.0), Some(100.0));
        assert_eq!(ft.due(Some(1020), 100.0, 0.25), Some(100.0 + d - 0.25));
        assert_eq!(ft.frame(Some(1000)), 0.0);
        assert_eq!(ft.frame(Some(1020)), d);
        ft.restart();
        // the next pass starts one frame after the last frame
        let off = d + 1.0 / 50.0;
        assert_eq!(ft.packet(Some(1000)), Some(off));
        assert_eq!(ft.frame(Some(1000)), off);
        // a stall moves frames and due times
        ft.add_shift(1.5);
        ft.add_shift(0.25);
        assert_eq!(ft.shift(), 1.75);
        assert_eq!(ft.frame(Some(1020)), off + d + 1.75);
        assert_eq!(ft.due(Some(1020), 100.0, 0.0), Some(100.0 + 1.75 + off + d));
        // a packet without a timestamp is not paced; as the first of a pass it latches 0
        ft.restart();
        assert_eq!(ft.packet(None), None);
        let off2 = (off + d) + 1.0 / 50.0;
        assert_eq!(ft.packet(Some(40)), Some(off2 + 40.0 * 0.001));
    }

    #[test]
    fn lround_is_half_away_from_zero() {
        // 2.5 and -2.5 as C's lround: 3 and -3 (clamped to 0 by the max)
        let mut ft = FrameTimes::new(1.0, 0.5, true);
        ft.packet(Some(0));
        assert_eq!(ft.frame(Some(0)), 0.0);
        assert_eq!(ft.frame(Some(5)), 3.0);
        assert_eq!(ft.frame(Some(1)), 1.0);
        assert_eq!(ft.frame(Some(-5)), 0.0);
        assert_eq!((2.5f64).round(), 3.0);
        assert_eq!((-2.5f64).round(), -3.0);
    }

    #[test]
    fn newest_due_pops_everything_due_and_counts_the_skipped() {
        let mut q: VecDeque<(f64, u32)> = VecDeque::new();
        assert_eq!(newest_due(&mut q, 1.0), (None, 0));
        for k in 0..5 {
            assert_eq!(push_bounded(&mut q, 26, f64::from(k) * 0.1, k), 0);
        }
        assert_eq!(newest_due(&mut q, -0.01), (None, 0));
        assert_eq!(newest_due(&mut q, 0.0), (Some((0.0, 0)), 0));
        assert_eq!(newest_due(&mut q, 0.05), (None, 0));
        // 0.1, 0.2 and 0.3 are due: the newest, two skipped
        assert_eq!(
            newest_due(&mut q, 0.35),
            (Some((f64::from(3u32) * 0.1, 3)), 2)
        );
        assert_eq!(q.len(), 1);
        assert_eq!(
            newest_due(&mut q, 9.0),
            (Some((f64::from(4u32) * 0.1, 4)), 0)
        );
        assert!(q.is_empty());
    }

    #[test]
    fn bounded_push_drops_the_oldest() {
        let mut q: VecDeque<(f64, u32)> = VecDeque::new();
        let mut dropped = 0;
        for k in 0..10 {
            dropped += push_bounded(&mut q, 4, f64::from(k), k);
        }
        assert_eq!(dropped, 6);
        assert_eq!(q.iter().map(|e| e.1).collect::<Vec<_>>(), [6, 7, 8, 9]);
    }

    #[test]
    fn offline_take_waits_for_a_frame_beyond_the_limit() {
        let mut q: VecDeque<(f64, u32)> = VecDeque::new();
        assert!(!take_ready(&q, 0.5, 4, false));
        assert!(take_ready(&q, 0.5, 4, true), "ended");
        push_bounded(&mut q, 4, 0.2, 0);
        push_bounded(&mut q, 4, 0.4, 1);
        assert!(
            !take_ready(&q, 0.5, 4, false),
            "a newer frame up to the limit may still come"
        );
        push_bounded(&mut q, 4, 0.6, 2);
        assert!(take_ready(&q, 0.5, 4, false));
        assert_eq!(newest_due(&mut q, 0.5), (Some((0.4, 1)), 1));
        // a full queue ends the wait too
        let mut q: VecDeque<(f64, u32)> = (0..4).map(|k| (0.1, k)).collect();
        assert!(take_ready(&q, 0.5, 4, false));
        assert_eq!(newest_due(&mut q, 0.5), (Some((0.1, 3)), 3));
    }
    /// A pass that begins while the thread skips to a key frame: with the pass's first packet
    /// known, the frames keep their nominal times and the next pass starts where it should;
    /// without it (the C++ rule) the pass and everything after it is early by the skipped part.
    #[test]
    fn a_pass_that_begins_in_a_skip_keeps_its_times() {
        let run = |tell: bool| {
            let mut t = FrameTimes::new(30.0, 0.001, true);
            // first pass, complete: 60 frames
            let mut out = Vec::new();
            for k in 0..60i64 {
                if tell {
                    t.packet_pts(Some(k * 100 / 3));
                }
                t.packet(Some(k * 100 / 3));
                out.push(t.frame(Some(k * 100 / 3)));
            }
            t.restart();
            // second pass: the first 45 packets are skipped (read, not sent), frames 45.. come
            for k in 0..60i64 {
                if tell {
                    t.packet_pts(Some(k * 100 / 3));
                }
                t.packet(Some(k * 100 / 3));
                if k >= 45 {
                    out.push(t.frame(Some(k * 100 / 3)));
                }
            }
            t.restart();
            t.packet_pts(Some(0));
            t.packet(Some(0));
            out.push(t.frame(Some(0)));
            out
        };
        let (cpp, fixed) = (run(false), run(true));
        // the first pass is the same either way
        assert_eq!(cpp[..60], fixed[..60]);
        // frame 45 of the second pass is at 2 s + 45 frames; the C++ rule puts it at 2 s
        assert_eq!(fixed[60], 2.0 + 45.0 / 30.0);
        assert_eq!(cpp[60], 2.0);
        // and the third pass begins at 4 s, not 1.5 s early
        assert_eq!(*fixed.last().unwrap(), 4.0);
        assert_eq!(*cpp.last().unwrap(), 2.5);
    }
}
