//! The fallback chain (SPEC §9.5): live, then standby, then slate; evaluated once per output
//! frame tick.
//!
//! Definitions [choice: the SPEC says "healthy" and "delivered frames continuously" without a
//! measure]:
//! - A source *delivered* at a tick when its frame clock gave [`crate::Selection::New`] there
//!   (`live_ok` / `standby_ok`). Repeats inside a normal cadence (2:1, 3:2, a 10 fps source) are
//!   not delivery, but they never last `hold`.
//! - A source is *alive* from a tick at which it delivered until a tick at which it has not
//!   delivered for `hold` (the demotion rule: "after `hold` of repeats"). That stretch is its
//!   *run*; a gap of `hold` ends the run.
//! - A source is *eligible* when its current run has lasted `promote_after` ("has delivered
//!   frames continuously for `promote_after`"). The slate is always eligible.
//! - The wanted source is the first of live, standby, slate that is eligible, or is the selected
//!   source and still alive. So the selected source is kept through gaps shorter than `hold`
//!   (a freeze, not a slate), a higher source takes over only once eligible, and a demotion goes
//!   to the best *eligible* lower source. [choice: the SPEC's table says standby is selected
//!   when "healthy"; reading "healthy" as eligible means a standby that only just appeared is not
//!   cut to; the slate is shown instead until it has been up for `promote_after`.]
//! - The selected source changes to the wanted one only at a tick that starts an output GOP
//!   ("cuts take effect on the first frame of the next output GOP"). Until then the old source
//!   stays selected (repeating its last frame if it is gone). The wanted source is evaluated
//!   at the cut. A demotion, once due, is not undone by a recovery before the cut: the source's
//!   run has ended, and a new run must last `promote_after` before the source is eligible.
//!
//! A flapping input therefore cannot flap the output faster than once per `promote_after` +
//! `hold`, and not at all if its good stretches are shorter than `promote_after`.

use crate::{Source, Ticks27};

#[derive(Clone, Copy, Debug, Default)]
struct Health {
    last_ok: Option<i64>,
    run_start: Option<i64>,
}

impl Health {
    fn update(&mut self, t: i64, ok: bool, hold: i64) {
        if ok {
            if self.run_start.is_none() {
                self.run_start = Some(t);
            }
            self.last_ok = Some(t);
        } else if let Some(l) = self.last_ok
            && i128::from(t) - i128::from(l) >= i128::from(hold)
        {
            self.run_start = None;
        }
    }
    fn alive(&self) -> bool {
        self.run_start.is_some()
    }
    fn eligible(&self, t: i64, promote_after: i64) -> bool {
        self.run_start
            .is_some_and(|s| i128::from(t) - i128::from(s) >= i128::from(promote_after))
    }
}

/// Chooses the source for each output tick: demotion after `hold` of repeats, promotion after
/// `promote_after` of health, cuts on GOP boundaries (SPEC §9.5; definitions in the module
/// documentation).
#[derive(Clone, Debug)]
pub struct Fallback {
    hold: Ticks27,
    promote_after: Ticks27,
    selected: Source,
    live: Health,
    standby: Health,
    last_t: Option<i64>,
}

impl Fallback {
    /// A selector that starts on the slate.
    pub fn new(hold: Ticks27, promote_after: Ticks27) -> Self {
        Self {
            hold,
            promote_after,
            selected: Source::Slate,
            live: Health::default(),
            standby: Health::default(),
            last_t: None,
        }
    }

    /// Evaluate at output tick `t`: whether live and standby delivered a frame for this tick
    /// (their frame clocks gave [`crate::Selection::New`]), and whether this tick starts an
    /// output GOP. Returns the source to show.
    pub fn tick(&mut self, t: Ticks27, live_ok: bool, standby_ok: bool, gop_start: bool) -> Source {
        let t = t.0;
        self.last_t = Some(t);
        self.live.update(t, live_ok, self.hold.0);
        self.standby.update(t, standby_ok, self.hold.0);
        if gop_start {
            self.selected = self.wanted_at(t);
        }
        self.selected
    }

    /// The source the selector would cut to at a GOP start at the last tick.
    pub fn wanted(&self) -> Source {
        match self.last_t {
            Some(t) => self.wanted_at(t),
            None => self.selected,
        }
    }

    /// The source currently shown.
    pub fn selected(&self) -> Source {
        self.selected
    }

    /// The hold and promotion times.
    pub fn times(&self) -> (Ticks27, Ticks27) {
        (self.hold, self.promote_after)
    }

    fn health(&self, s: Source) -> Option<&Health> {
        match s {
            Source::Live => Some(&self.live),
            Source::Standby => Some(&self.standby),
            Source::Slate => None,
        }
    }

    fn wanted_at(&self, t: i64) -> Source {
        for s in [Source::Live, Source::Standby] {
            let Some(h) = self.health(s) else { continue };
            if h.eligible(t, self.promote_after.0) || (s == self.selected && h.alive()) {
                return s;
            }
        }
        Source::Slate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 29.97 fps output period.
    const P: i64 = 900_900;
    /// Output GOP: 15 frames (0.5 s).
    const GOP: i64 = 15;

    fn fb() -> Fallback {
        Fallback::new(Ticks27::from_secs(1), Ticks27::from_secs(3))
    }

    /// Runs ticks `k0..k1`, with `live(k)` / `standby(k)` delivery, GOP every 15 ticks.
    fn run(
        f: &mut Fallback,
        k0: i64,
        k1: i64,
        live: impl Fn(i64) -> bool,
        standby: impl Fn(i64) -> bool,
    ) -> Vec<Source> {
        (k0..k1)
            .map(|k| f.tick(Ticks27(k * P), live(k), standby(k), k % GOP == 0))
            .collect()
    }

    /// First tick index at which `src` is shown.
    fn first(v: &[Source], k0: i64, src: Source) -> Option<i64> {
        v.iter().position(|&s| s == src).map(|i| k0 + i as i64)
    }

    /// SPEC §9.5: starts on the slate; live is selected after it has delivered continuously for
    /// promote_after, at the next GOP start.
    #[test]
    fn spec_9_5_promotion_after_promote_after_of_health() {
        let mut f = fb();
        assert_eq!(f.selected(), Source::Slate);
        assert_eq!(f.times(), (Ticks27::from_secs(1), Ticks27::from_secs(3)));
        // live starts at tick 7
        let v = run(&mut f, 0, 300, |k| k >= 7, |_| false);
        // eligible at tick 7 + ceil(3 s / P) = 7 + 90 (90 P = 3.003 s >= 3 s); next GOP start 105
        assert_eq!(first(&v, 0, Source::Live), Some(105));
        assert!(v[..105].iter().all(|&s| s == Source::Slate));
        assert!(v[105..].iter().all(|&s| s == Source::Live));
    }

    /// SPEC §9.5: demotion after `hold` of repeats; short gaps show a freeze, not a slate.
    #[test]
    fn spec_9_5_demotion_after_hold_of_repeats() {
        let mut f = fb();
        run(&mut f, 0, 150, |_| true, |_| false);
        assert_eq!(f.selected(), Source::Live);
        // a gap of 29 ticks (0.97 s < hold): still live
        let v = run(&mut f, 150, 400, |k| !(200..229).contains(&k), |_| false);
        assert!(v.iter().all(|&s| s == Source::Live));
        // live stops at tick 400 (last delivery 399): dead at 399 + 30 (30 P = 1.001 s >= 1 s)
        let v = run(&mut f, 400, 600, |_| false, |_| false);
        assert_eq!(f.wanted(), Source::Slate);
        // the cut waits for the GOP start at 435
        assert_eq!(first(&v, 400, Source::Slate), Some(435));
        assert!(v[..35].iter().all(|&s| s == Source::Live));
    }

    /// SPEC §9.5: demotion moves to standby when it is eligible; a standby that has not been up
    /// for promote_after is not cut to (the slate is), and is promoted to once it has been.
    #[test]
    fn spec_9_5_demotion_to_standby_or_slate() {
        let mut f = fb();
        run(&mut f, 0, 150, |_| true, |_| true);
        assert_eq!(f.selected(), Source::Live);
        let v = run(&mut f, 150, 300, |_| false, |_| true);
        assert_eq!(first(&v, 150, Source::Standby), Some(180));
        assert!(!v.contains(&Source::Slate));
        // standby only just appeared: slate first, standby after promote_after
        let mut f = fb();
        run(&mut f, 0, 150, |_| true, |_| false);
        let v = run(&mut f, 150, 400, |_| false, |k| k >= 170);
        assert_eq!(first(&v, 150, Source::Slate), Some(180));
        assert_eq!(first(&v, 150, Source::Standby), Some(270));
        // live comes back: promoted over standby after promote_after
        let v = run(&mut f, 400, 600, |_| true, |_| true);
        assert_eq!(first(&v, 400, Source::Live), Some(495));
    }

    /// SPEC §9.5: a flapping input does not flap the output: up 2 s, down 1.5 s forever never
    /// reaches promote_after; the output stays on standby.
    #[test]
    fn spec_9_5_flapping_input_does_not_flap_output() {
        let mut f = fb();
        let flap = |k: i64| (k % 105) < 60; // 2 s up, 1.5 s down
        let v = run(&mut f, 0, 30_000, flap, |_| true);
        let changes = v.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(changes, 1, "slate -> standby once");
        assert_eq!(*v.last().unwrap(), Source::Standby);
        // the same with live selected first: one demotion, then no more changes
        let mut f = fb();
        run(&mut f, 0, 150, |_| true, |_| true);
        let v = run(&mut f, 150, 30_000, flap, |_| true);
        let changes = v.windows(2).filter(|w| w[0] != w[1]).count();
        assert_eq!(changes, 1, "live -> standby once");
        // flapping with gaps shorter than hold keeps live (a freeze each time)
        let mut f = fb();
        run(&mut f, 0, 150, |_| true, |_| true);
        let v = run(&mut f, 150, 30_000, |k| (k % 60) < 35, |_| true);
        assert!(v.iter().all(|&s| s == Source::Live));
    }

    /// SPEC §9.5: cuts take effect only on the first frame of an output GOP.
    #[test]
    fn spec_9_5_cuts_only_on_gop_starts() {
        let mut f = fb();
        let mut seed = 1u64;
        let mut prev = f.selected();
        let mut cuts = 0;
        for k in 0..200_000i64 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            // inputs that come and go in stretches of 0..10 s
            let live = (seed >> 40) % 1000 < 997 && (k / 211) % 3 != 0;
            let standby = (k / 347) % 5 != 0;
            let gop = k % GOP == 0;
            let s = f.tick(Ticks27(k * P), live, standby, gop);
            if s != prev {
                assert!(gop, "cut at {k} off a GOP start");
                cuts += 1;
            }
            prev = s;
        }
        assert!(cuts > 10);
    }
}
