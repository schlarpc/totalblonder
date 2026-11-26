//! Statistical multiplexing (SPEC §8.4–§8.6): one carrier's video pool divided between its
//! programs by how hard each is to code at the moment, without ever promising the mux more
//! than the pool.
//!
//! A rate change has two halves, the encoder's target and the mux's delivery rate, and their
//! order depends on the direction (SPEC §8.5, "lower before raise"):
//!
//! - **Decrease**: the encoder's target first. The mux keeps delivering at the old rate until the
//!   encoder has *coded a picture under the new target* (the encoder's ticket comes back on an
//!   access unit) **and** one buffer delay `D` has passed since that picture's decoding time
//!   minus `D` (`tb_m2v_rate::vbv`: the mux must drain at ≥ the highest rate of the last `D`).
//!   Only then is the mux lowered and the difference released to the pool.
//! - **Increase**: only from released budget. The mux is raised first, then the encoder.
//!
//! So `Σ mux rates ≤ pool` at every instant, by construction ([`Controller::committed`]).
//!
//! Allocation, once per tick (one GOP, 0.5 s; SPEC §8.4): complexity `C` is the exponentially
//! weighted mean of `bits × mean quantizer` per second of display time; the target is
//! `Rmin + (pool − ΣRmin) · w·C^α / Σ w·C^α`, clamped to `[Rmin, Rmax]`, moved at most
//! `max_step` per tick, and ignored below a hysteresis. A program on its slate is pinned to
//! `Rmin`.
//!
//! Pure: time is 27 MHz ticks passed in; rates are elementary-stream bit/s (the caller turns them
//! into transport rates for the mux). The caller executes [`Command`]s and reports tickets and
//! access units back.
#![forbid(unsafe_code)]

/// A program's limits and weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgramParams {
    /// Floor, bit/s.
    pub min_bps: u64,
    /// Ceiling, bit/s (at most the encoder's sequence-header rate).
    pub max_bps: u64,
    /// Weight `w`.
    pub weight: f64,
}

/// The controller's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// The pool to divide, bit/s (elementary).
    pub pool_bps: u64,
    /// Exponent α (0.7 to 1).
    pub alpha: f64,
    /// Largest relative change per tick (0.15).
    pub max_step: f64,
    /// Changes smaller than this fraction are not made (0.03).
    pub hysteresis: f64,
    /// Weight of the newest tick in the complexity average (0.3).
    pub smoothing: f64,
    /// The encoders' buffer delay D, ticks.
    pub delay: u64,
}

/// A configuration that cannot work.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// The floors do not fit in the pool.
    #[error("floors sum to {0} bit/s, more than the pool of {1}")]
    Floors(u64, u64),
    /// A program's floor is above its ceiling, or zero.
    #[error("program {0}: floor must be positive and at most the ceiling")]
    Limits(usize),
    /// A parameter is out of range.
    #[error("{0}")]
    Range(&'static str),
}

/// What the caller must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Set the encoder's target of program `program` and report the ticket with
    /// [`Controller::ticket`].
    Encoder {
        /// Program index.
        program: usize,
        /// Target, bit/s.
        bps: u64,
    },
    /// Set the mux's delivery rate of program `program` (elementary bit/s).
    Mux {
        /// Program index.
        program: usize,
        /// Rate, bit/s.
        bps: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    /// Lowering to `to`: waiting for the ticket, then for its picture, then for `D`.
    Lowering {
        to: u64,
        ticket: Option<u32>,
        release_at: Option<u64>,
    },
}

#[derive(Debug, Clone)]
struct Prog {
    p: ProgramParams,
    /// Bits × mean quantizer accumulated this tick.
    acc: f64,
    /// Display time accumulated this tick, ticks.
    shown: u64,
    complexity: Option<f64>,
    slate: bool,
    /// What the encoder was last told.
    encoder: u64,
    /// What the mux delivers.
    mux: u64,
    pending: Option<Pending>,
}

/// One carrier's controller.
#[derive(Debug, Clone)]
pub struct Controller {
    cfg: Config,
    progs: Vec<Prog>,
}

impl Controller {
    /// A controller whose programs start at their equal shares of the pool, within their
    /// limits. Returns the commands that set the encoders and the mux to those rates.
    pub fn new(
        cfg: Config,
        params: &[ProgramParams],
    ) -> Result<(Controller, Vec<Command>), ConfigError> {
        if !(0.0..=1.0).contains(&cfg.alpha) || !(0.0..1.0).contains(&cfg.max_step) {
            return Err(ConfigError::Range(
                "alpha must be in [0, 1] and max_step in [0, 1)",
            ));
        }
        if !(0.0..1.0).contains(&cfg.hysteresis) || !(0.0..=1.0).contains(&cfg.smoothing) {
            return Err(ConfigError::Range(
                "hysteresis in [0, 1), smoothing in [0, 1]",
            ));
        }
        for (i, p) in params.iter().enumerate() {
            if p.min_bps == 0 || p.min_bps > p.max_bps || p.weight.is_nan() || p.weight <= 0.0 {
                return Err(ConfigError::Limits(i));
            }
        }
        let floors: u64 = params.iter().map(|p| p.min_bps).sum();
        if floors > cfg.pool_bps {
            return Err(ConfigError::Floors(floors, cfg.pool_bps));
        }
        let mut c = Controller {
            cfg,
            progs: params
                .iter()
                .map(|&p| Prog {
                    p,
                    acc: 0.0,
                    shown: 0,
                    complexity: None,
                    slate: false,
                    encoder: 0,
                    mux: 0,
                    pending: None,
                })
                .collect(),
        };
        let start = c.allocate(&vec![1.0; params.len()]);
        let mut cmds = Vec::new();
        for (i, r) in start.into_iter().enumerate() {
            c.progs[i].encoder = r;
            c.progs[i].mux = r;
            cmds.push(Command::Mux { program: i, bps: r });
            cmds.push(Command::Encoder { program: i, bps: r });
        }
        Ok((c, cmds))
    }

    /// The sum of the mux rates now: never above the pool.
    pub fn committed(&self) -> u64 {
        self.progs.iter().map(|p| p.mux).sum()
    }

    /// Program `i`'s (encoder target, mux rate).
    pub fn rates(&self, i: usize) -> Option<(u64, u64)> {
        self.progs.get(i).map(|p| (p.encoder, p.mux))
    }

    /// Program `i`'s complexity estimate.
    pub fn complexity(&self, i: usize) -> Option<f64> {
        self.progs.get(i).and_then(|p| p.complexity)
    }

    /// Whether program `i` shows its slate (pinned to its floor).
    pub fn set_slate(&mut self, i: usize, on: bool) {
        if let Some(p) = self.progs.get_mut(i) {
            p.slate = on;
        }
    }

    /// The ticket the encoder returned for the last [`Command::Encoder`] of program `i`.
    pub fn ticket(&mut self, i: usize, ticket: u32) {
        if let Some(Pending::Lowering {
            ticket: t @ None, ..
        }) = self.progs.get_mut(i).and_then(|p| p.pending.as_mut())
        {
            *t = Some(ticket);
        }
    }

    /// An access unit of program `i`: its size, mean quantizer, display time (ticks), its
    /// decoding time and the ticket of the first rate change it was coded under (0: none).
    pub fn access_unit(
        &mut self,
        i: usize,
        bits: u64,
        avg_q: f32,
        shown: u64,
        dts: u64,
        ticket: u32,
    ) {
        let Some(p) = self.progs.get_mut(i) else {
            return;
        };
        p.acc += bits as f64 * f64::from(avg_q.max(1.0));
        p.shown += shown;
        if let Some(Pending::Lowering {
            ticket: Some(t),
            release_at: r @ None,
            ..
        }) = p.pending.as_mut()
            && ticket == *t
        {
            // The picture entered the mux queue at dts − D under the new rate; the old rate is
            // owed for D after that (the encoder's model, `tb_m2v_rate::vbv`): until dts.
            *r = Some(dts);
        }
    }

    /// Completes lowerings whose time has come. Call often (every mux pass is fine).
    pub fn poll(&mut self, now: u64) -> Vec<Command> {
        let mut out = Vec::new();
        for (i, p) in self.progs.iter_mut().enumerate() {
            if let Some(Pending::Lowering {
                to,
                release_at: Some(at),
                ..
            }) = p.pending
                && now >= at
            {
                p.mux = to;
                p.pending = None;
                out.push(Command::Mux {
                    program: i,
                    bps: to,
                });
            }
        }
        out
    }

    /// The allocation step, once per GOP period.
    pub fn tick(&mut self, now: u64) -> Vec<Command> {
        let mut out = self.poll(now);
        // Complexity per second of display time.
        for p in &mut self.progs {
            if p.shown > 0 {
                let c = p.acc * 27e6 / p.shown as f64;
                p.complexity = Some(match p.complexity {
                    Some(old) => old + self.cfg.smoothing * (c - old),
                    None => c,
                });
            }
            p.acc = 0.0;
            p.shown = 0;
        }
        let weights: Vec<f64> = self
            .progs
            .iter()
            .map(|p| {
                if p.slate {
                    0.0
                } else {
                    p.p.weight * p.complexity.unwrap_or(1.0).max(1.0).powf(self.cfg.alpha)
                }
            })
            .collect();
        let want = self.allocate(&weights);
        // Decreases first: they start the release of budget.
        for (i, &w) in want.iter().enumerate() {
            let p = &mut self.progs[i];
            if p.pending.is_some() {
                continue;
            }
            let cur = p.encoder;
            let floor = (cur as f64 * (1.0 - self.cfg.max_step)) as u64;
            let to = w.max(floor).max(p.p.min_bps);
            if to < cur && (cur - to) as f64 > cur as f64 * self.cfg.hysteresis {
                p.encoder = to;
                p.pending = Some(Pending::Lowering {
                    to,
                    ticket: None,
                    release_at: None,
                });
                out.push(Command::Encoder {
                    program: i,
                    bps: to,
                });
            }
        }
        // Increases, from what is free now.
        let mut free = self.cfg.pool_bps.saturating_sub(self.committed());
        for (i, &w) in want.iter().enumerate() {
            let p = &mut self.progs[i];
            if p.pending.is_some() || free == 0 {
                continue;
            }
            let cur = p.encoder;
            let ceil = (cur as f64 * (1.0 + self.cfg.max_step)) as u64;
            let to = w.min(ceil).min(p.p.max_bps).min(p.mux + free);
            if to > cur && (to - cur) as f64 > cur as f64 * self.cfg.hysteresis {
                free -= to - p.mux.min(to);
                p.mux = p.mux.max(to);
                p.encoder = to;
                out.push(Command::Mux {
                    program: i,
                    bps: p.mux,
                });
                out.push(Command::Encoder {
                    program: i,
                    bps: to,
                });
            }
        }
        out
    }

    /// Water-filling of the pool by weight within the limits.
    fn allocate(&self, weights: &[f64]) -> Vec<u64> {
        let n = self.progs.len();
        let mut r: Vec<u64> = self.progs.iter().map(|p| p.p.min_bps).collect();
        let mut open: Vec<bool> = (0..n)
            .map(|i| self.progs[i].p.max_bps > self.progs[i].p.min_bps && weights[i] > 0.0)
            .collect();
        let mut left = self.cfg.pool_bps.saturating_sub(r.iter().sum());
        for _ in 0..n {
            let total: f64 = (0..n).filter(|&i| open[i]).map(|i| weights[i]).sum();
            if left == 0 || total <= 0.0 {
                break;
            }
            let mut used = 0;
            let mut capped = false;
            for i in 0..n {
                if !open[i] {
                    continue;
                }
                let share = (left as f64 * weights[i] / total) as u64;
                let room = self.progs[i].p.max_bps - r[i];
                let add = share.min(room);
                if share >= room {
                    open[i] = false;
                    capped = true;
                }
                r[i] += add;
                used += add;
            }
            left -= used.min(left);
            if !capped {
                break;
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 27_000_000;

    fn cfg(pool: u64) -> Config {
        Config {
            pool_bps: pool,
            alpha: 0.85,
            max_step: 0.15,
            hysteresis: 0.03,
            smoothing: 0.3,
            delay: S / 2,
        }
    }

    fn p(min: u64, max: u64) -> ProgramParams {
        ProgramParams {
            min_bps: min,
            max_bps: max,
            weight: 1.0,
        }
    }

    /// Plays the caller: executes commands, codes pictures whose size follows the content's
    /// difficulty, checks the invariant at every step.
    struct Sim {
        c: Controller,
        mux: Vec<u64>,
        enc: Vec<u64>,
        ticket: Vec<u32>,
        next_ticket: u32,
    }

    impl Sim {
        fn apply(&mut self, cmds: Vec<Command>) {
            for c in cmds {
                match c {
                    Command::Mux { program, bps } => self.mux[program] = bps,
                    Command::Encoder { program, bps } => {
                        self.enc[program] = bps;
                        self.next_ticket += 1;
                        self.ticket[program] = self.next_ticket;
                        self.c.ticket(program, self.next_ticket);
                    }
                }
                assert!(
                    self.mux.iter().sum::<u64>() <= self.c.cfg.pool_bps,
                    "invariant broken"
                );
            }
            assert_eq!(self.mux.iter().sum::<u64>(), self.c.committed());
        }
    }

    #[test]
    fn follows_complexity_and_never_exceeds_the_pool() {
        let pool = 30_000_000;
        let ps = [p(2_000_000, 15_000_000); 4];
        let (c, cmds) = Controller::new(cfg(pool), &ps).unwrap();
        let mut sim = Sim {
            c,
            mux: vec![0; 4],
            enc: vec![0; 4],
            ticket: vec![0; 4],
            next_ticket: 0,
        };
        sim.apply(cmds);
        assert_eq!(sim.enc, vec![7_500_000; 4]);
        let frame = S / 30;
        let mut seen = [0u32; 4];
        // 60 s: program 0 is hard (q 10 at its rate), the others easy (q 2), then reversed.
        for k in 0..1800u64 {
            let t = k * frame;
            #[allow(clippy::needless_range_loop)]
            for i in 0..4 {
                let hard = if k < 900 { i == 0 } else { i == 3 };
                let q = if hard { 10.0 } else { 2.0 };
                let bits = sim.enc[i] / 30;
                // The encoder applies a new target from the next picture.
                let tk = if seen[i] != sim.ticket[i] {
                    seen[i] = sim.ticket[i];
                    sim.ticket[i]
                } else {
                    0
                };
                sim.c.access_unit(i, bits, q, frame, t + S / 2, tk);
            }
            let cmds = if k % 15 == 14 {
                sim.c.tick(t)
            } else {
                sim.c.poll(t)
            };
            sim.apply(cmds);
        }
        // After the reversal program 3 holds the most, program 0 has come down.
        assert!(sim.enc[3] > sim.enc[1] * 2, "{:?}", sim.enc);
        assert!(sim.enc[0] < sim.enc[3], "{:?}", sim.enc);
        assert!(
            sim.enc
                .iter()
                .all(|&r| (2_000_000..=15_000_000).contains(&r))
        );
        // Most of the pool is in use once things settle.
        assert!(sim.c.committed() > pool * 8 / 10, "{}", sim.c.committed());
    }

    #[test]
    fn a_decrease_releases_only_after_its_picture_and_d() {
        let (mut c, _) = Controller::new(
            cfg(10_000_000),
            &[p(1_000_000, 10_000_000), p(1_000_000, 10_000_000)],
        )
        .unwrap();
        c.set_slate(1, true);
        // Program 1 to its floor: the encoder is lowered, the mux not yet.
        let cmds = c.tick(0);
        assert!(cmds.contains(&Command::Encoder {
            program: 1,
            bps: 4_250_000
        }));
        assert_eq!(c.rates(1), Some((4_250_000, 5_000_000)));
        c.ticket(1, 7);
        // Pictures without the ticket do not release anything.
        c.access_unit(1, 1000, 2.0, S / 30, S, 0);
        assert!(c.poll(10 * S).is_empty());
        // The first picture under the new rate, decoded at 2 s: released at 2 s.
        c.access_unit(1, 1000, 2.0, S / 30, 2 * S, 7);
        assert!(c.poll(2 * S - 1).is_empty());
        assert_eq!(
            c.poll(2 * S),
            vec![Command::Mux {
                program: 1,
                bps: 4_250_000
            }]
        );
        assert_eq!(c.committed(), 5_000_000 + 4_250_000);
    }

    #[test]
    fn refuses_impossible_floors() {
        assert!(
            Controller::new(
                cfg(3_000_000),
                &[p(2_000_000, 5_000_000), p(2_000_000, 5_000_000)]
            )
            .is_err()
        );
        assert!(Controller::new(cfg(9_000_000), &[p(6_000_000, 5_000_000)]).is_err());
    }
}
