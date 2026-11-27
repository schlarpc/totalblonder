//! When to step a program to another format (SPEC §12.1, "CPU overload, encoders late"): the
//! video hub's deadline policy gives up pictures when a tick would miss its deadline, lower
//! priority first; that answers a short burst. Sustained, it shows as one program stuttering, and
//! this governor answers it by moving a program one rung down its ladder (a smaller format,
//! less work on the device), and back up once the device has had room for long enough.
//!
//! The governor only decides; the hub carries the switch out (`video.rs`). It sees, once a
//! second, each program's counters of pictures given up and pictures made, and:
//!
//! - **down** when, over the last `window`, at least `threshold` of all pictures were given
//!   up: the program with the lowest priority that has a rung below it (ties: the larger
//!   format, then the later program) moves one rung down. Nothing else is decided for one
//!   window, so the next decision sees the effect of this one.
//! - **up** when no picture was given up for `recover`, and nothing moved for as long: the
//!   program with the highest priority below its top rung (ties: the earlier program) moves
//!   one rung up. A step down within two windows of a step up doubles `recover` (at most an
//!   hour), so a program that does not fit is not switched back and forth.

use std::collections::VecDeque;

use crate::config::OverloadPlan;
use crate::time::HZ;

/// What the governor knows of one program at a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// Its deadline-miss priority (lower gives up pictures first).
    pub priority: i32,
    /// Its rung now (0: its configured format).
    pub rung: usize,
    /// How many rungs its ladder has.
    pub rungs: usize,
    /// Macroblocks per second of its current format (the work a step down saves).
    pub mbs: u64,
    /// A switch of it is in progress.
    pub switching: bool,
}

/// A decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Program `.0` one rung down.
    Down(usize),
    /// Program `.0` one rung up.
    Up(usize),
}

/// The governor of one hub.
#[derive(Debug, Clone)]
pub struct Governor {
    plan: OverloadPlan,
    /// Per program: (time, pictures given up, pictures made), the counters as sampled.
    samples: Vec<VecDeque<(u64, u64, u64)>>,
    /// No decision before this time.
    quiet_until: u64,
    /// Time of the last sample in which a picture was given up (or of the start).
    last_shed: u64,
    last_up: Option<u64>,
    last_move: u64,
    recover: u64,
}

/// The longest `recover` the back-off reaches.
const RECOVER_MAX: u64 = 3600 * HZ;

impl Governor {
    /// A governor for `n` programs, started at `now`.
    pub fn new(plan: OverloadPlan, n: usize, now: u64) -> Self {
        Governor {
            plan,
            samples: vec![VecDeque::new(); n],
            quiet_until: now + plan.window,
            last_shed: now,
            last_up: None,
            last_move: now,
            recover: plan.recover,
        }
    }

    /// The counters of program `i` at `now`: pictures given up and pictures made, since its
    /// session started.
    pub fn sample(&mut self, now: u64, i: usize, shed: u64, made: u64) {
        let Some(q) = self.samples.get_mut(i) else {
            return;
        };
        if q.back().is_some_and(|&(_, s, _)| shed > s) {
            self.last_shed = now;
        }
        q.push_back((now, shed, made));
        while q.len() > 2
            && q.get(1)
                .is_some_and(|&(t, _, _)| t + self.plan.window <= now)
        {
            q.pop_front();
        }
    }

    /// Program `i` starts a new session (its counters start again at zero).
    pub fn reset(&mut self, i: usize) {
        if let Some(q) = self.samples.get_mut(i) {
            q.clear();
        }
    }

    /// What `recover` is now (after back-off).
    pub fn recover(&self) -> u64 {
        self.recover
    }

    /// The decision at `now`, if any, for programs `c` (index = program).
    pub fn decide(&mut self, now: u64, c: &[Candidate]) -> Option<Step> {
        if !self.plan.enabled || now < self.quiet_until || c.iter().any(|x| x.switching) {
            return None;
        }
        // pictures given up and made over the window, where a whole window was seen
        let (mut shed, mut made) = (0u64, 0u64);
        for q in &self.samples {
            if let (Some(&(t0, s0, m0)), Some(&(t1, s1, m1))) = (q.front(), q.back())
                && t1 >= t0 + self.plan.window * 9 / 10
            {
                shed += s1.saturating_sub(s0);
                made += m1.saturating_sub(m0);
            }
        }
        if made > 0 && shed as f64 >= self.plan.threshold * made as f64 {
            let pick = c
                .iter()
                .enumerate()
                .filter(|(_, x)| x.rung + 1 < x.rungs)
                .min_by_key(|(i, x)| (x.priority, std::cmp::Reverse(x.mbs), std::cmp::Reverse(*i)))
                .map(|(i, _)| i)?;
            if self.last_up.is_some_and(|u| now < u + 2 * self.plan.window) {
                self.recover = (self.recover * 2).min(RECOVER_MAX);
            }
            self.moved(now, pick);
            return Some(Step::Down(pick));
        }
        if now >= self.last_shed + self.recover && now >= self.last_move + self.recover {
            let pick = c
                .iter()
                .enumerate()
                .filter(|(_, x)| x.rung > 0)
                .max_by_key(|(i, x)| (x.priority, std::cmp::Reverse(*i)))
                .map(|(i, _)| i)?;
            self.last_up = Some(now);
            self.moved(now, pick);
            return Some(Step::Up(pick));
        }
        None
    }

    fn moved(&mut self, now: u64, i: usize) {
        self.quiet_until = now + self.plan.window;
        self.last_move = now;
        self.reset(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = HZ;

    fn plan() -> OverloadPlan {
        OverloadPlan {
            enabled: true,
            window: 10 * S,
            threshold: 0.02,
            recover: 300 * S,
        }
    }

    fn cand(priority: i32, rung: usize, mbs: u64) -> Candidate {
        Candidate {
            priority,
            rung,
            rungs: 2,
            mbs,
            switching: false,
        }
    }

    /// Feeds `secs` seconds of samples from `t`: program `i` gives up `shed[i]` of 30 pictures
    /// a second. Returns the first decision and the time after.
    fn run(
        g: &mut Governor,
        t: &mut u64,
        secs: u64,
        shed: &[u64],
        counters: &mut [(u64, u64)],
        c: &[Candidate],
    ) -> Option<Step> {
        for _ in 0..secs {
            *t += S;
            for (i, s) in shed.iter().enumerate() {
                counters[i].0 += s;
                counters[i].1 += 30;
                g.sample(*t, i, counters[i].0, counters[i].1);
            }
            if let Some(d) = g.decide(*t, c) {
                return Some(d);
            }
        }
        None
    }

    #[test]
    fn sustained_loss_steps_the_lowest_priority_down() {
        let mut g = Governor::new(plan(), 3, 0);
        let mut t = 0;
        let mut k = [(0, 0); 3];
        let c = [cand(5, 0, 245_000), cand(0, 0, 40_000), cand(0, 0, 245_000)];
        // 1 picture in 30 given up by program 2 only: 1.1 % of all, below 2 %
        assert_eq!(run(&mut g, &mut t, 30, &[0, 0, 1], &mut k, &c), None);
        // 3 a second: 3.3 % of all; the lowest priority, larger format, later program: 2
        assert_eq!(
            run(&mut g, &mut t, 30, &[0, 0, 3], &mut k, &c),
            Some(Step::Down(2))
        );
        // nothing more for a window, and then only with a whole window of new samples
        let c = [cand(5, 0, 245_000), cand(0, 0, 40_000), cand(0, 1, 40_000)];
        k[2] = (0, 0);
        assert_eq!(run(&mut g, &mut t, 9, &[0, 3, 0], &mut k, &c), None);
        assert_eq!(
            run(&mut g, &mut t, 5, &[0, 3, 0], &mut k, &c),
            Some(Step::Down(1))
        );
    }

    #[test]
    fn room_for_long_enough_steps_back_up_with_back_off() {
        let mut g = Governor::new(plan(), 2, 0);
        let mut t = 0;
        let mut k = [(0, 0); 2];
        let c = [cand(1, 1, 40_000), cand(0, 1, 40_000)];
        // nothing given up: up after `recover`, the higher priority first
        assert_eq!(run(&mut g, &mut t, 299, &[0, 0], &mut k, &c), None);
        assert_eq!(
            run(&mut g, &mut t, 2, &[0, 0], &mut k, &c),
            Some(Step::Up(0))
        );
        // it does not fit: loss at once, down again within two windows, recover doubles
        k = [(0, 0); 2];
        let c = [cand(1, 0, 245_000), cand(0, 1, 40_000)];
        assert_eq!(
            run(&mut g, &mut t, 15, &[5, 0], &mut k, &c),
            Some(Step::Down(0))
        );
        assert_eq!(g.recover(), 600 * S);
        // a disabled governor decides nothing
        let mut off = Governor::new(
            OverloadPlan {
                enabled: false,
                ..plan()
            },
            1,
            0,
        );
        let mut k1 = [(0, 0)];
        assert_eq!(
            run(&mut off, &mut 0, 60, &[10], &mut k1, &[cand(0, 0, 1)]),
            None
        );
    }

    #[test]
    fn nothing_while_a_switch_is_in_progress() {
        let mut g = Governor::new(plan(), 1, 0);
        let mut k = [(0, 0)];
        let c = [Candidate {
            switching: true,
            ..cand(0, 0, 245_000)
        }];
        assert_eq!(run(&mut g, &mut 0, 60, &[10], &mut k, &c), None);
    }
}
