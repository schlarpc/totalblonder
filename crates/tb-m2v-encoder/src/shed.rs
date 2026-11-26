//! The deadline-miss policy (the C++ spike's `Encoder::plan`).
//!
//! Before a tick's work is queued, the macroblocks that fit into the time left are estimated
//! from the throughput of the last three ticks that coded something (the best of them). If the
//! tick's pictures do not fit, sessions give up theirs: lower priority first; among equals B
//! pictures, then P, key frames last; in rotation. A tick that starts after its deadline gives
//! up everything; while there is time, one picture is always coded so that the next tick has a
//! measurement.

/// Initial state of the test hook's generator (the C++ `Encoder::rng`).
pub const RNG_INIT: u64 = 0x9E37_79B9_7F4A_7C15;
/// Multiplier of the seed added to [`RNG_INIT`] (the C++ driver).
pub const RNG_SEED_MUL: u64 = 0x2545_F491_4F6C_DD1D;
/// Modulus of the rotation value.
pub const ROT_MOD: u64 = 1_000_003;
/// Ticks without shedding at the start (the throughput history is not yet meaningful).
pub const WARMUP_TICKS: u64 = 16;
/// Shortest tick duration the history uses, seconds.
const MIN_TICK_S: f64 = 1e-6;

/// A session with a picture due in this tick that the policy may give up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    /// Caller's index of the session.
    pub session: u32,
    /// The session's priority.
    pub priority: i32,
    /// 0 = B picture, 1 = P picture, 2 = key frame.
    pub class: u8,
    /// Macroblocks the picture costs.
    pub mbs: f64,
    /// The session has no reference picture yet (`!have_ref` in the C++): its key frame is the
    /// first picture of a stream and cannot be a repeat.
    pub first: bool,
}

impl Candidate {
    /// A key frame of a session without reference: never given up.
    fn protected(&self) -> bool {
        self.class == 2 && self.first
    }
}

/// The budget a tick was planned with (the C++ `shed_budget` record).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Seconds left to the deadline, less the margin.
    pub budget: f64,
    /// Macroblocks per second, the best of the last three ticks.
    pub rate: f64,
    /// Macroblocks that fit.
    pub allowed: f64,
    /// Macroblocks of all candidates.
    pub mbs: f64,
}

/// State the policy keeps between ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShedState {
    /// Macroblocks per second of the last three ticks that coded something, newest first.
    pub rate_hist: [f64; 3],
    /// Ticks so far (no shedding during the first 16). The caller advances it at the end of
    /// every tick (`steps++` of `Encoder::step`); [`ShedState::plan`] only reads it.
    pub steps: u64,
    /// State of the test hook's generator.
    pub rng: u64,
}

/// `std::max(a, b)` for doubles: `a` unless `a < b`.
fn cmax(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

impl ShedState {
    /// A fresh state with the test hook's generator seeded as the C++ driver does
    /// (`rng = 0x9E3779B97F4A7C15 + seed * 0x2545F4914F6CDD1D`, wrapping). The C++ seed is an
    /// `int` converted to `uint64_t`; a caller with a negative seed passes `seed as i64 as u64`.
    pub fn seeded(seed: u64) -> Self {
        Self {
            rate_hist: [0.0; 3],
            steps: 0,
            rng: RNG_INIT.wrapping_add(seed.wrapping_mul(RNG_SEED_MUL)),
        }
    }

    /// The rotation value of candidate `k` (its index in collection order) at tick `steps`:
    /// `(k + steps * 7) % 1000003`. Among candidates of equal priority and class the lower
    /// value is given up first.
    pub fn rotation(steps: u64, k: u64) -> u64 {
        k.wrapping_add(steps.wrapping_mul(7)) % ROT_MOD
    }

    /// Decide which candidates give up their picture. `left_s`: seconds from now to the
    /// deadline, already reduced by the margin (`None`: no deadline, or the policy is off).
    /// Sets `shed[i]` for candidate i (clearing it otherwise) and returns how many were shed.
    /// `force`: the test hook's probability (`Some(p)` with `p > 0` replaces the policy).
    ///
    /// Candidates must be in collection order (batch by batch, slot by slot): the order sets
    /// the rotation and the hook's draws. `shed` should have one entry per candidate; a
    /// candidate without an entry is never given up.
    pub fn plan(
        &mut self,
        cands: &[Candidate],
        left_s: Option<f64>,
        safety: f64,
        force: Option<f64>,
        shed: &mut [bool],
    ) -> u32 {
        self.plan_with_budget(cands, left_s, safety, force, shed).0
    }

    /// [`ShedState::plan`], and the budget it planned with when it got as far as computing one
    /// (a deadline, candidates, a measured rate, past the warm-up; the golden record
    /// `shed_budget`).
    pub fn plan_with_budget(
        &mut self,
        cands: &[Candidate],
        left_s: Option<f64>,
        safety: f64,
        force: Option<f64>,
        shed: &mut [bool],
    ) -> (u32, Option<Budget>) {
        shed.iter_mut().for_each(|s| *s = false);
        let n = cands.len().min(shed.len());
        let cands = &cands[..n];
        let mut count: u32 = 0;

        if let Some(p) = force.filter(|p| *p > 0.0) {
            for (c, s) in cands.iter().zip(shed.iter_mut()) {
                self.rng = self
                    .rng
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                if (self.rng >> 40) as f64 / (1u64 << 24) as f64 >= p || c.protected() {
                    continue;
                }
                *s = true;
                count += 1;
            }
            return (count, None);
        }

        let mut mb = 0.0;
        for c in cands {
            mb += c.mbs;
        }
        let rate = cmax(
            self.rate_hist[0],
            cmax(self.rate_hist[1], self.rate_hist[2]),
        );
        let Some(budget) = left_s else {
            return (0, None);
        };
        if cands.is_empty() || rate <= 0.0 || self.steps < WARMUP_TICKS {
            return (0, None);
        }
        let allowed = if budget > 0.0 {
            rate * budget / safety
        } else {
            0.0
        };
        let planned = Some(Budget {
            budget,
            rate,
            allowed,
            mbs: mb,
        });
        if mb <= allowed {
            return (0, planned);
        }

        let mut order: Vec<(usize, u64)> = (0..n)
            .map(|k| (k, Self::rotation(self.steps, k as u64)))
            .collect();
        order.sort_by(|&(a, ra), &(b, rb)| {
            let (ca, cb) = (&cands[a], &cands[b]);
            ca.priority
                .cmp(&cb.priority)
                .then(ca.class.cmp(&cb.class))
                .then(ra.cmp(&rb))
        });

        let mut left = mb;
        let mut last = None;
        for &(i, _) in &order {
            if left <= allowed {
                break;
            }
            let c = &cands[i];
            if c.protected() {
                continue;
            }
            shed[i] = true;
            left -= c.mbs;
            count += 1;
            last = Some(i);
        }
        // Never give up everything while there is time left: one picture is coded, so that the
        // next tick has a fresh measurement to plan with.
        if let Some(i) = last
            && budget > 0.0
            && left <= 0.0
        {
            shed[i] = false;
            count -= 1;
        }
        (count, planned)
    }

    /// Record what a tick did: `mbs` macroblocks coded in `seconds`. A tick that coded nothing
    /// leaves the history as it is; the duration is taken as at least a microsecond.
    pub fn observe(&mut self, mbs: f64, seconds: f64) {
        if mbs > 0.0 {
            let tick_s = cmax(MIN_TICK_S, seconds);
            self.rate_hist[2] = self.rate_hist[1];
            self.rate_hist[1] = self.rate_hist[0];
            self.rate_hist[0] = mbs / tick_s;
        }
    }
}
