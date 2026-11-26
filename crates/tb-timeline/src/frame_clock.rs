//! Video frame clock (SPEC §5.4): at each output tick with presentation time `T`, the newest
//! mapped frame due at `T`; repeat if it is the frame already shown, drop the older ones if more
//! than one new frame is due. The only place where frames are dropped or repeated, and both are
//! counted.
//!
//! # Hysteresis [choice: the SPEC does not have it; notes/cuda-mpeg2-spike.md §C.9 asks for it]
//!
//! With the plain rule ("output time ≤ T"), a source at the output's rate whose frames sit on a
//! tick boundary alternates between repeat and drop under the smallest jitter. The clock
//! therefore selects with a *selection offset* `o`: a frame is due at `T` when its output time is
//! ≤ `T + o`.
//!
//! - **Snap.** At the first tick after construction or [`FrameClock::reset`] at which the period
//!   is known and a frame is pending, `o` is set so that the oldest pending frame's phase lies
//!   half a period from the selection boundary. Nothing was shown before, so this costs no drop
//!   and no repeat.
//! - **Lock.** When the input's frame interval (a running average of pushed output-time steps)
//!   is within 2 % of `m` output periods for `m` in 1..=4 (the rates are "equal", or the output
//!   is an integer multiple), `o` follows each newly selected frame's phase, so that frames stay
//!   half a period from the boundary: `o += (target − o) / 8`. Per-frame jitter is averaged out,
//!   and moving `o` toward its target only moves the boundary away from the frames, so it never
//!   causes a drop or repeat by itself.
//! - **Wrap.** `o` is kept in `[−P/2 − H, P/2 + H]` with `H = P/4`. When a source drifts against
//!   the output (30.000 into 29.97, a producer 100 ppm slow), `o` follows the drift until it
//!   leaves that band and is moved by one period `P`: that is exactly one drop or one repeat, and
//!   the next one comes only after the phase has drifted another period. The 2·H overlap is the
//!   hysteresis: jitter would have to move the averaged phase by `P/2` to undo a wrap.
//! - Otherwise (rates that are not near an integer ratio, such as 24 into 59.94) `o` stays where
//!   the snap put it, and the plain rule with a constant offset produces the usual cadence
//!   (3:2 for 23.976 into 59.94).
//!
//! Consequence: a frame may be shown up to `P/2 + H = 3P/4` before or after its output time
//! (the plain rule shows it up to `P` after). Frames are only ever shown after they were pushed,
//! so showing one early never shows a frame that is not there.
//!
//! Other choices:
//! - A pushed frame replaces every pending frame with a later output time (counted as drops):
//!   the newest timeline wins, so a new mapper session whose times lie before an old session's
//!   future frames takes over at once. Within a session times never decrease.
//! - The queue holds at most [`MAX_PENDING`] frames; beyond that the oldest is dropped (counted).
//! - Frames discarded by [`FrameClock::reset`] are not counted as drops: the source changed.

use std::collections::VecDeque;

use crate::{FrameId, Selection, Ticks27};

/// Most frames the clock holds before dropping the oldest (8.5 s at 120 fps; the SPEC's buffer
/// is at most 2·J = 2 s).
pub const MAX_PENDING: usize = 1024;

/// Gain of the phase follower, as a divisor.
const FOLLOW: i64 = 8;
/// Gain of the frame-interval average, as a divisor.
const AVG: i64 = 8;
/// Tolerance of "equal rates" as a divisor of the period (2 %).
const LOCK_TOL: i64 = 50;
/// Largest integer ratio of input interval to output period that locks.
const LOCK_MAX_M: i64 = 4;

/// Chooses the frame for each output tick (SPEC §5.4).
#[derive(Clone, Debug, Default)]
pub struct FrameClock {
    /// Frames repeated so far.
    pub repeats: u64,
    /// Frames dropped so far.
    pub drops: u64,
    /// Output frame period, if given.
    period: Option<i64>,
    /// Period learned from consecutive ticks, if not given.
    learned: Option<i64>,
    last_tick: Option<i64>,
    /// Pending frames, output times non-decreasing.
    pending: VecDeque<(FrameId, i64)>,
    shown: Option<FrameId>,
    /// Selection offset `o`.
    offset: i64,
    snapped: bool,
    last_at: Option<i64>,
    /// Running average of the input frame interval.
    interval: Option<i64>,
}

impl FrameClock {
    /// A frame clock for an output with frame period `period` (the time between ticks). With
    /// [`FrameClock::default`] the period is learned from the first two ticks.
    pub fn new(period: Ticks27) -> Self {
        Self {
            period: (period.0 > 0).then_some(period.0),
            ..Self::default()
        }
    }

    /// A mapped frame with output time `at` is available.
    pub fn push(&mut self, frame: FrameId, at: Ticks27) {
        let at = at.0;
        while self.pending.back().is_some_and(|&(_, t)| t > at) {
            self.pending.pop_back();
            self.drops += 1;
        }
        if let Some(prev) = self.last_at {
            let d = i128::from(at) - i128::from(prev);
            let p = self.period();
            let plausible = d > 0 && p.is_none_or(|p| d <= 8 * i128::from(p));
            if plausible && let Ok(d) = i64::try_from(d) {
                self.interval = Some(match self.interval {
                    None => d,
                    Some(i) => i + (d - i) / AVG,
                });
            }
        }
        self.last_at = Some(at);
        self.pending.push_back((frame, at));
        if self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
            self.drops += 1;
        }
    }

    /// The output tick with presentation time `t`.
    pub fn tick(&mut self, t: Ticks27) -> Selection {
        let t = t.0;
        if let Some(l) = self.last_tick
            && t > l
        {
            self.learned = Some(t - l);
        }
        self.last_tick = Some(t);
        let p = self.period();
        if !self.snapped
            && let (Some(p), Some(&(_, at))) = (p, self.pending.front())
        {
            self.offset = target(self.offset, at.saturating_sub(t), p);
            self.snapped = true;
        }
        let bound = t.saturating_add(self.offset);
        let n = self.pending.partition_point(|&(_, at)| at <= bound);
        if n > 0 {
            self.drops += (n - 1) as u64;
            let mut sel = None;
            for _ in 0..n {
                sel = self.pending.pop_front();
            }
            if let Some((id, at)) = sel {
                if let Some(p) = p {
                    self.follow(at.saturating_sub(t), p);
                }
                self.shown = Some(id);
                return Selection::New(id);
            }
        }
        match self.shown {
            Some(id) => {
                self.repeats += 1;
                Selection::Repeat(id)
            }
            None => Selection::Nothing,
        }
    }

    /// Forget everything (the source changed). Counters are kept; pending frames are released
    /// without being counted as drops.
    pub fn reset(&mut self) {
        *self = Self {
            repeats: self.repeats,
            drops: self.drops,
            period: self.period,
            learned: self.learned,
            ..Self::default()
        };
    }

    /// Frames waiting to be due.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The frame on screen, if any.
    pub fn shown(&self) -> Option<FrameId> {
        self.shown
    }

    /// The selection offset `o` (see the module documentation).
    pub fn offset(&self) -> Ticks27 {
        Ticks27(self.offset)
    }

    /// Whether the phase follower is engaged (input interval near an integer multiple of the
    /// output period).
    pub fn locked(&self) -> bool {
        self.period().is_some_and(|p| self.is_locked(p))
    }

    fn period(&self) -> Option<i64> {
        self.period.or(self.learned)
    }

    fn is_locked(&self, p: i64) -> bool {
        let Some(i) = self.interval else {
            return false;
        };
        let (i, p) = (i128::from(i), i128::from(p));
        let m = (i + p / 2) / p;
        (1..=i128::from(LOCK_MAX_M)).contains(&m)
            && (i - m * p).abs() * i128::from(LOCK_TOL) <= m * p
    }

    /// A frame with phase `e = at − t` was selected: follow it if locked, then wrap.
    fn follow(&mut self, e: i64, p: i64) {
        if !self.is_locked(p) {
            return;
        }
        let tgt = target(self.offset, e, p);
        self.offset += (tgt - self.offset) / FOLLOW;
        let h = p / 4;
        let hi = p / 2 + h;
        if self.offset > hi {
            self.offset -= p;
        } else if self.offset < -hi {
            self.offset += p;
        }
    }
}

/// The offset that puts a frame of phase `e` (relative to the tick) half a period from the
/// selection boundary, within half a period of `o`.
fn target(o: i64, e: i64, p: i64) -> i64 {
    // r in (−P, 0], r ≡ e − o (mod P): where the frame sits below the boundary o.
    let r = -((i128::from(o) - i128::from(e)).rem_euclid(i128::from(p)));
    // |r| < p and o is within a period or so: fits.
    let r = i64::try_from(r).unwrap_or(0);
    o.saturating_add(r).saturating_add(p / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 29.97 fps output period.
    const P2997: i64 = 900_900;
    /// 59.94 fps output period.
    const P5994: i64 = 450_450;

    /// Runs `ticks` output ticks of period `p` from `t0 = 10·p`, feeding frames whose output
    /// time is `at(i)` up to 2 s ahead of each tick (as the mapper would, J ahead). Returns the
    /// selections.
    fn run(p: i64, ticks: i64, mut at: impl FnMut(u64) -> i64) -> (FrameClock, Vec<Selection>) {
        let mut fc = FrameClock::new(Ticks27(p));
        let mut next = 0u64;
        let mut out = Vec::with_capacity(ticks as usize);
        for k in 0..ticks {
            let t = (10 + k) * p;
            loop {
                let a = at(next);
                if a > t + Ticks27::from_secs(2).0 {
                    break;
                }
                fc.push(FrameId(next), Ticks27(a));
                next += 1;
            }
            out.push(fc.tick(Ticks27(t)));
        }
        (fc, out)
    }

    /// A deterministic jitter in [−r, r].
    fn jitter(i: u64, r: i64) -> i64 {
        let mut z = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z % (2 * r as u64 + 1)) as i64 - r
    }

    /// SPEC §5.4: the newest due frame is taken; older due frames are dropped and counted;
    /// nothing new due repeats the frame shown and counts it; before any frame, nothing.
    #[test]
    fn spec_5_4_newest_due_frame_repeat_and_drop() {
        let mut fc = FrameClock::new(Ticks27(100));
        assert_eq!(fc.tick(Ticks27(0)), Selection::Nothing);
        fc.push(FrameId(1), Ticks27(1000));
        fc.push(FrameId(2), Ticks27(1100));
        fc.push(FrameId(3), Ticks27(1200));
        fc.push(FrameId(4), Ticks27(5000));
        // the snap puts frame 1 (phase 0) half a period from the boundary: o = 50
        assert_eq!(fc.tick(Ticks27(100)), Selection::Nothing);
        assert_eq!(fc.offset(), Ticks27(50));
        assert_eq!(fc.tick(Ticks27(1200)), Selection::New(FrameId(3)));
        assert_eq!(fc.drops, 2);
        assert_eq!(fc.tick(Ticks27(1300)), Selection::Repeat(FrameId(3)));
        assert_eq!(fc.tick(Ticks27(1400)), Selection::Repeat(FrameId(3)));
        assert_eq!(fc.repeats, 2);
        assert_eq!(fc.pending(), 1);
        assert_eq!(fc.shown(), Some(FrameId(3)));
        // reset forgets the picture and the pending frames, keeps the counters
        fc.reset();
        assert_eq!(fc.tick(Ticks27(9000)), Selection::Nothing);
        assert_eq!((fc.repeats, fc.drops, fc.pending()), (2, 2, 0));
    }

    /// Checks that the gaps between drops (`New` ids skipping) or repeats are all within
    /// `lo..=hi` ticks: one event at a time, evenly spread, no flip-flop.
    fn evenly(sel: &[Selection], lo: usize, hi: usize) {
        let mut last: Option<u64> = None;
        let mut prev_event = None;
        for (k, s) in sel.iter().enumerate() {
            let event = match *s {
                Selection::New(FrameId(id)) => {
                    let skip = last.is_some_and(|l| id != l + 1);
                    if let Some(l) = last {
                        assert!(id <= l + 2, "two drops at tick {k}");
                    }
                    last = Some(id);
                    skip
                }
                Selection::Repeat(_) => true,
                Selection::Nothing => false,
            };
            if event {
                if let Some(g) = prev_event {
                    let gap = k - g;
                    assert!((lo..=hi).contains(&gap), "events {gap} ticks apart at {k}");
                }
                prev_event = Some(k);
            }
        }
    }

    /// SPEC §5.4: "An input at 30.000 fps feeding a 29.97 fps output drops one frame in 1001",
    /// also with ±1 ms of jitter on the frame times (one drop at a time, 1001 ticks apart).
    #[test]
    fn spec_5_4_30fps_into_2997_drops_one_in_1001() {
        let n = 1001 * 100;
        for j in [0, 27_000] {
            let (fc, sel) = run(P2997, n, |i| {
                900_000 * i as i64 + 10 * P2997 + 12_345 + if j > 0 { jitter(i, j) } else { 0 }
            });
            assert_eq!(fc.repeats, 0);
            assert!((99..=101).contains(&fc.drops), "drops {}", fc.drops);
            evenly(&sel, 950, 1050);
        }
    }

    /// SPEC §5.4: "a producer running 100 ppm slow repeats one frame in 10,000", also with ±1 ms
    /// of jitter.
    #[test]
    fn spec_5_4_producer_100ppm_slow_repeats_one_in_10000() {
        let n = 200_000;
        for j in [0, 27_000] {
            let (fc, sel) = run(P2997, n, |i| {
                (i128::from(i) * i128::from(P2997) * 10_001 / 10_000) as i64
                    + 10 * P2997
                    + 777
                    + if j > 0 { jitter(i, j) } else { 0 }
            });
            assert_eq!(fc.drops, 0);
            assert!((19..=21).contains(&fc.repeats), "repeats {}", fc.repeats);
            evenly(&sel, 9_500, 10_500);
        }
    }

    /// The plain rule (`at ≤ T`) on the same frames: how many flips it would make.
    fn plain_flips(p: i64, n: i64, at: impl Fn(u64) -> i64) -> u64 {
        let mut next = 0u64;
        let mut have = false;
        let mut flips = 0;
        for k in 0..n {
            let t = (10 + k) * p;
            let mut due = 0;
            while at(next) <= t {
                next += 1;
                due += 1;
            }
            if have && due != 1 {
                flips += 1;
            }
            have |= due > 0;
        }
        flips
    }

    /// Hysteresis (notes/cuda-mpeg2-spike.md §C.9): equal rates whose phase sits on the tick
    /// boundary, with ±1 ms of jitter on every frame time: no drop, no repeat. The plain rule
    /// of SPEC §5.4 alternates between repeat and drop on the same input.
    #[test]
    fn hysteresis_equal_rates_phase_on_boundary_with_1ms_jitter_no_drop_or_repeat() {
        let n = 100_000;
        for p in [P2997, P5994] {
            for phase in [-27_000, -1, 0, 1, 13_500, 27_000, p / 2, p - 1] {
                let at = |i: u64| (10 + i as i64) * p + phase + jitter(i, 27_000);
                let (fc, sel) = run(p, n, at);
                assert_eq!((fc.drops, fc.repeats), (0, 0), "period {p} phase {phase}");
                assert!(fc.locked());
                // nothing only before the first frame
                let first = sel.iter().position(|s| !matches!(s, Selection::Nothing));
                assert!(first.is_some_and(|f| f <= 1));
                if phase.abs() <= 1 {
                    assert!(plain_flips(p, n, at) > 1000, "the test must be meaningful");
                }
            }
        }
    }

    /// SPEC §5.4 + 3:2: 23.976 fps into 59.94 shows frames for 3 and 2 ticks in turn; 24.000
    /// fps gives runs of 2 or 3 and drops nothing.
    #[test]
    fn spec_5_4_24fps_into_5994_gives_3_2_cadence() {
        for phase in [0, 1, 100_000, 225_225, 450_449] {
            let (fc, sel) = run(P5994, 10_000, |i| 1_126_125 * i as i64 + 4_504_500 + phase);
            assert_eq!(fc.drops, 0);
            assert!(!fc.locked());
            let runs = run_lengths(&sel);
            // the first run may be cut by the start
            for w in runs[1..runs.len() - 1].windows(2) {
                assert!(
                    (w[0] == 3 && w[1] == 2) || (w[0] == 2 && w[1] == 3),
                    "phase {phase}: {w:?}"
                );
            }
        }
        let (fc, sel) = run(P5994, 100_000, |i| 1_125_000 * i as i64 + 4_504_500);
        assert_eq!(fc.drops, 0);
        let runs = run_lengths(&sel);
        assert!(runs[1..runs.len() - 1].iter().all(|&r| r == 2 || r == 3));
    }

    fn run_lengths(sel: &[Selection]) -> Vec<usize> {
        let mut runs = Vec::new();
        for s in sel {
            match s {
                Selection::New(_) => runs.push(1),
                Selection::Repeat(_) => {
                    if let Some(r) = runs.last_mut() {
                        *r += 1;
                    }
                }
                Selection::Nothing => {}
            }
        }
        runs
    }

    /// 60 fps into 59.94 drops one in 1001; 30 fps into 59.94 (with ±1 ms jitter) shows every
    /// frame twice, except one tick in 1001 that is missing (a frame shown once; the lock with
    /// m = 2), never three times.
    #[test]
    fn integer_ratios_lock() {
        let (fc, _) = run(P5994, 100_100, |i| 450_000 * i as i64 + 10 * P5994);
        assert_eq!(fc.repeats, 0);
        assert!((99..=101).contains(&fc.drops), "drops {}", fc.drops);
        let (fc, sel) = run(P5994, 100_100, |i| {
            900_000 * i as i64 + 10 * P5994 + jitter(i, 27_000)
        });
        assert!(fc.locked());
        assert_eq!(fc.drops, 0);
        let runs = run_lengths(&sel);
        let ones = runs[1..runs.len() - 1].iter().filter(|&&r| r == 1).count();
        assert!(runs[1..runs.len() - 1].iter().all(|&r| r == 1 || r == 2));
        assert!((99..=101).contains(&ones), "{ones}");
    }

    /// A newer timeline replaces pending frames with later output times (a new mapper session
    /// placed before the old one's future frames).
    #[test]
    fn push_replaces_later_pending_frames() {
        let mut fc = FrameClock::new(Ticks27(100));
        fc.push(FrameId(1), Ticks27(1000));
        fc.push(FrameId(2), Ticks27(9000));
        fc.push(FrameId(3), Ticks27(9100));
        fc.push(FrameId(4), Ticks27(1100));
        assert_eq!(fc.drops, 2);
        assert_eq!(fc.pending(), 2);
        assert_eq!(fc.tick(Ticks27(2000)), Selection::New(FrameId(4)));
        assert_eq!(fc.drops, 3);
    }

    /// Memory is bounded: at most MAX_PENDING frames are held.
    #[test]
    fn pending_is_bounded() {
        let mut fc = FrameClock::default();
        for i in 0..10_000u64 {
            fc.push(FrameId(i), Ticks27(i as i64 * 1_000_000 + 1));
        }
        assert_eq!(fc.pending(), MAX_PENDING);
        assert_eq!(fc.drops, 10_000 - MAX_PENDING as u64);
        // the period is learned from the ticks
        assert_eq!(fc.tick(Ticks27(0)), Selection::Nothing);
        fc.tick(Ticks27(100));
        assert!(fc.period().is_some());
    }
}
