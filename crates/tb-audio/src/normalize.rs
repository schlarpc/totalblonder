//! The live loudness normalizer of SPEC §8.3: a slow gain toward −24 LKFS (A/85 §6 p.18),
//! driven by a BS.1770-4 measurement of its own input, with no look-ahead.
//!
//! Every 100 ms the input's 400 ms block loudness (K-weighted, gated as in BS.1770-4) is taken.
//! The measurement is a *gated loudness over the last `window` active blocks*: blocks at or
//! below `freeze_below` (default −50 LUFS: pauses, silence, room tone) are not added, so the
//! gain holds through pauses of any length instead of creeping up on the noise floor; among the
//! blocks in the window, the BS.1770 relative gate (−10 LU) applies as in the integrated
//! measurement. The wanted gain is `target − measured`, clamped to `[−max_cut, +max_boost]`.
//!
//! The applied gain (in dB) follows the wanted gain through a first-order filter whose time
//! constant depends on the direction and the situation:
//!
//! | Situation | Time constant (default) | Why |
//! |---|---|---|
//! | Gain going down | `attack` 2 s | loud material must be brought down promptly |
//! | Gain going up | `release` 8 s | a slow rise does not pump on speech dynamics |
//! | Gain going up by more than `recover_db` (3 dB) | `recover` 2 s | a quiet program after a loud one is not left far below the target for long |
//! | The first `acquire` (3 s) of active input | 0.5 s | a new source reaches the target in a second or two instead of starting several dB off |
//! | Output momentary loudness more than `panic_lu` (8 LU) above the target | 0.3 s | a quiet program followed by a loud one is pulled down within a second |
//!
//! *Program change.* The relative gate keeps the loud blocks of a previous program in the
//! measurement until they leave the window, and gates the new, quieter ones out. So when the
//! last `change_s` (3 s) of active blocks are all more than `change_lu` (15 LU) below the
//! measurement, the older blocks are dropped and the measurement restarts from those 3 s. (A
//! louder program needs no such rule: its blocks dominate the mean at once.) The threshold is
//! high so that the quiet passages of one program (a soft scene, 6–12 LU below its dialogue)
//! are not mistaken for a new program and pulled up. A new input session
//! ([`Normalizer::restart`]) starts the measurement afresh, with the fast acquisition, from the
//! gain in use.
//!
//! Between 100 ms updates the linear gain is interpolated sample by sample, so there is no
//! zipper noise. Peaks are left to the true-peak limiter that follows ([`crate::limiter`]).
//!
//! Defaults \[choice\]: `max_boost` +15 dB (an input at −35 LKFS needs +11 dB, SPEC M9; 15 dB
//! covers material down to −39 LKFS without lifting the noise floor of near-silence much
//! further), `max_cut` 30 dB (anything up to +6 LKFS), `window` 100 active blocks (10 s of
//! program): long enough to average speech and music, short enough to follow a program
//! change within the attack time.

use crate::layout::OutputLayout;
use crate::loudness::{BlockMeter, Loudness, MeterError, STEP};

/// Parameters of a [`Normalizer`] (SPEC §8.3; defaults in [`Default`], reasons in the module
/// documentation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizerConfig {
    /// Target loudness, LUFS (default −24, A/85 §6).
    pub target_lufs: f64,
    /// Largest gain, dB (default 15).
    pub max_boost_db: f64,
    /// Largest attenuation, dB (default 30).
    pub max_cut_db: f64,
    /// Active 400 ms blocks in the measurement (default 100: 10 s).
    pub window_blocks: usize,
    /// Blocks at or below this loudness do not update the measurement (default −50 LUFS).
    pub freeze_below_lufs: f64,
    /// Time constant while the gain goes down, seconds (default 2).
    pub attack_s: f64,
    /// Time constant while the gain goes up, seconds (default 8).
    pub release_s: f64,
    /// Active input at the start that uses the fast time constant, seconds (default 3).
    pub acquire_s: f64,
    /// The fast time constant, seconds (default 0.5).
    pub fast_s: f64,
    /// Output momentary loudness above the target by this much uses the panic constant (8 LU).
    pub panic_lu: f64,
    /// The panic time constant, seconds (default 0.3).
    pub panic_s: f64,
    /// A rise of more than this uses the recovery time constant, dB (default 3).
    pub recover_db: f64,
    /// The recovery time constant, seconds (default 2).
    pub recover_s: f64,
    /// Active input this far below the measurement is a new program, LU (default 15).
    pub change_lu: f64,
    /// ... when it has lasted this long, seconds (default 3).
    pub change_s: f64,
}

impl Default for NormalizerConfig {
    fn default() -> Self {
        Self {
            target_lufs: -24.0,
            max_boost_db: 15.0,
            max_cut_db: 30.0,
            window_blocks: 100,
            freeze_below_lufs: -50.0,
            attack_s: 2.0,
            release_s: 8.0,
            acquire_s: 3.0,
            fast_s: 0.5,
            panic_lu: 8.0,
            panic_s: 0.3,
            recover_db: 3.0,
            recover_s: 2.0,
            change_lu: 15.0,
            change_s: 3.0,
        }
    }
}

/// A [`NormalizerConfig`] value out of range.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum NormalizerConfigError {
    /// A field is outside its allowed range.
    #[error("{field} = {value} is outside {lo}..={hi}")]
    Range {
        /// The field.
        field: &'static str,
        /// Its value.
        value: f64,
        /// Lowest allowed.
        lo: f64,
        /// Highest allowed.
        hi: f64,
    },
}

impl NormalizerConfig {
    /// Check every field against its range.
    pub fn validate(&self) -> Result<(), NormalizerConfigError> {
        let checks: [(&'static str, f64, f64, f64); 15] = [
            ("target_lufs", self.target_lufs, -40.0, -10.0),
            ("max_boost_db", self.max_boost_db, 0.0, 30.0),
            ("max_cut_db", self.max_cut_db, 0.0, 60.0),
            ("window_blocks", self.window_blocks as f64, 4.0, 6000.0),
            ("freeze_below_lufs", self.freeze_below_lufs, -70.0, -20.0),
            ("attack_s", self.attack_s, 0.05, 600.0),
            ("release_s", self.release_s, 0.05, 600.0),
            ("acquire_s", self.acquire_s, 0.0, 600.0),
            ("fast_s", self.fast_s, 0.05, 600.0),
            ("panic_lu", self.panic_lu, 1.0, 40.0),
            ("panic_s", self.panic_s, 0.05, 600.0),
            ("recover_db", self.recover_db, 0.0, 60.0),
            ("recover_s", self.recover_s, 0.05, 600.0),
            ("change_lu", self.change_lu, 3.0, 60.0),
            (
                "change_s",
                self.change_s,
                0.4,
                self.window_blocks as f64 / 10.0,
            ),
        ];
        for (field, value, lo, hi) in checks {
            // `!(a <= b)` so that NaN fails too.
            if !(value >= lo && value <= hi) {
                return Err(NormalizerConfigError::Range {
                    field,
                    value,
                    lo,
                    hi,
                });
            }
        }
        Ok(())
    }
}

/// The normalizer of one source: measures its input and applies a slowly varying gain in place.
#[derive(Clone, Debug)]
pub struct Normalizer {
    cfg: NormalizerConfig,
    meter: BlockMeter,
    /// Powers of the last active blocks, a ring of `window_blocks`.
    window: Vec<f64>,
    head: usize,
    filled: usize,
    active_steps: u64,
    /// Applied gain in dB (filter state) and the linear gains at the start and end of the
    /// current 100 ms step.
    gain_db: f64,
    from: f32,
    to: f32,
}

impl Normalizer {
    /// A normalizer for `layout` with `cfg`, starting at 0 dB.
    pub fn new(layout: OutputLayout, cfg: NormalizerConfig) -> Result<Self, NormalizerConfigError> {
        cfg.validate()?;
        Ok(Self {
            cfg,
            meter: BlockMeter::new(layout),
            window: vec![0.0; cfg.window_blocks],
            head: 0,
            filled: 0,
            active_steps: 0,
            gain_db: 0.0,
            from: 1.0,
            to: 1.0,
        })
    }

    /// The configuration.
    pub fn config(&self) -> &NormalizerConfig {
        &self.cfg
    }

    /// The gain being applied, dB.
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// The gated loudness of the input over the window, if there is an active block in it.
    pub fn measured(&self) -> Option<Loudness> {
        gated(|| self.recent(self.filled))
    }

    /// The last `k` blocks of the window, oldest first.
    fn recent(&self, k: usize) -> impl Iterator<Item = f64> + '_ {
        let n = self.window.len();
        let k = k.min(self.filled);
        (n - k..n).map(move |i| self.window[(self.head + i) % n])
    }

    /// A new input session: the measurement starts afresh, with the fast acquisition, from the
    /// gain in use.
    pub fn restart(&mut self) {
        self.filled = 0;
        self.active_steps = 0;
    }

    /// Measure `planes` (one per channel of the layout, equal lengths) and apply the gain to
    /// them in place.
    pub fn process(&mut self, planes: &mut [Vec<f32>]) -> Result<(), MeterError> {
        let ch = self.meter.channels();
        if planes.len() != ch {
            return Err(MeterError::Planes {
                expected: ch,
                got: planes.len(),
            });
        }
        let n = planes.first().map_or(0, Vec::len);
        if planes.iter().any(|p| p.len() != n) {
            return Err(MeterError::Ragged);
        }
        let mut off = 0;
        while off < n {
            let k = self.meter.until_step().min(n - off);
            let pos = STEP - self.meter.until_step();
            // Measure before the gain (feed-forward on the input).
            let done = {
                let mut refs: [&[f32]; 6] = [&[]; 6];
                for (r, p) in refs.iter_mut().zip(planes.iter()) {
                    *r = p;
                }
                self.meter.feed(&refs[..ch], off, k)
            };
            self.apply(planes, off, k, pos);
            if done {
                self.update();
            }
            off += k;
        }
        Ok(())
    }

    /// Apply the ramp from `from` to `to` over samples `off..off + k`, which are at positions
    /// `pos..pos + k` within the current step.
    fn apply(&self, planes: &mut [Vec<f32>], off: usize, k: usize, pos: usize) {
        let d = (self.to - self.from) / STEP as f32;
        if d == 0.0 {
            if self.to != 1.0 {
                for p in planes.iter_mut() {
                    for x in &mut p[off..off + k] {
                        *x *= self.to;
                    }
                }
            }
            return;
        }
        for p in planes.iter_mut() {
            for (i, x) in p[off..off + k].iter_mut().enumerate() {
                *x *= self.from + d * (pos + i) as f32;
            }
        }
    }

    /// A 100 ms step completed: update the measurement and the gain.
    fn update(&mut self) {
        let c = self.cfg;
        let mut tau = None;
        if let Some(p) = self.meter.mean_of_last(4) {
            let block = Loudness::from_power(p);
            if block.0 > c.freeze_below_lufs {
                self.window[self.head] = p;
                self.head = (self.head + 1) % self.window.len();
                self.filled = (self.filled + 1).min(self.window.len());
                self.active_steps += 1;
                self.program_change();
                if let Some(m) = self.measured() {
                    let want = (c.target_lufs - m.0).clamp(-c.max_cut_db, c.max_boost_db);
                    let out_now = block.0 + self.gain_db;
                    let t = if (self.active_steps as f64) * 0.1 <= c.acquire_s {
                        c.fast_s
                    } else if out_now > c.target_lufs + c.panic_lu && want < self.gain_db {
                        c.panic_s
                    } else if want < self.gain_db {
                        c.attack_s
                    } else if want > self.gain_db + c.recover_db {
                        c.recover_s
                    } else {
                        c.release_s
                    };
                    tau = Some((want, t));
                }
            }
        }
        if let Some((want, t)) = tau {
            let a = 1.0 - (-0.1 / t).exp();
            self.gain_db += (want - self.gain_db) * a;
        }
        self.from = self.to;
        self.to = 10f64.powf(self.gain_db / 20.0) as f32;
    }
}

impl Normalizer {
    /// Drop the older blocks when the last `change_s` are all far below the measurement
    /// (module documentation, "Program change").
    fn program_change(&mut self) {
        let k = (self.cfg.change_s * 10.0).round() as usize;
        if self.filled <= k {
            return;
        }
        let Some(m) = self.measured() else {
            return;
        };
        let limit = Loudness(m.0 - self.cfg.change_lu).to_power();
        if self.recent(k).all(|p| p < limit) {
            self.filled = k;
        }
    }
}

/// BS.1770-4 gated loudness (absolute −70, relative −10 LU) of block powers (`blocks` gives the
/// same sequence each time it is called).
fn gated<I: Iterator<Item = f64>>(blocks: impl Fn() -> I) -> Option<Loudness> {
    let abs_t = Loudness(-70.0).to_power();
    let (mut n, mut s) = (0usize, 0.0);
    for p in blocks().filter(|&p| p > abs_t) {
        n += 1;
        s += p;
    }
    if n == 0 {
        return None;
    }
    let rel_t = s / n as f64 * 0.1;
    let (mut n, mut s) = (0usize, 0.0);
    for p in blocks().filter(|&p| p > abs_t && p > rel_t) {
        n += 1;
        s += p;
    }
    (n > 0).then(|| Loudness::from_power(s / n as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loudness::LoudnessMeter;
    use crate::signal::{db_to_amp, program, sine};

    fn run(n: &mut Normalizer, x: &[Vec<f32>], meter: &mut LoudnessMeter) {
        for s in (0..x[0].len()).step_by(1000) {
            let e = (s + 1000).min(x[0].len());
            let mut b: Vec<Vec<f32>> = x.iter().map(|p| p[s..e].to_vec()).collect();
            n.process(&mut b).unwrap();
            let r: Vec<&[f32]> = b.iter().map(|p| p.as_slice()).collect();
            meter.push(&r).unwrap();
        }
    }

    #[test]
    fn config_is_validated() {
        let mut c = NormalizerConfig::default();
        assert!(c.validate().is_ok());
        c.max_boost_db = f64::NAN;
        assert!(c.validate().is_err());
        let c = NormalizerConfig {
            window_blocks: 1,
            ..Default::default()
        };
        assert!(Normalizer::new(OutputLayout::Stereo, c).is_err());
    }

    /// A −34 dBFS stereo tone (−34 LUFS) is brought to −24 within a few seconds and held.
    #[test]
    fn tone_is_brought_to_target() {
        let mut n = Normalizer::new(OutputLayout::Stereo, NormalizerConfig::default()).unwrap();
        let x = sine(1000.0, db_to_amp(-34.0), 48_000, 48_000 * 10, 0.0);
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
        run(&mut n, &[x.clone(), x], &mut m);
        assert!((n.gain_db() - 10.0).abs() < 0.1, "{}", n.gain_db());
        assert!((m.short_term().unwrap().0 + 24.0).abs() < 0.2);
    }

    /// A loud program followed by a quiet one: the change is detected after 3 s and the gain
    /// recovers within about 10 s more.
    #[test]
    fn loud_to_quiet_program_recovers() {
        let mut n = Normalizer::new(OutputLayout::Stereo, NormalizerConfig::default()).unwrap();
        let loud = sine(1000.0, db_to_amp(-10.0), 48_000, 48_000 * 20, 0.0);
        let quiet = sine(1000.0, db_to_amp(-35.0), 48_000, 48_000 * 15, 0.0);
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
        run(&mut n, &[loud.clone(), loud], &mut m);
        assert!((n.gain_db() + 14.0).abs() < 0.1);
        run(&mut n, &[quiet.clone(), quiet], &mut m);
        assert!((n.gain_db() - 11.0).abs() < 2.0, "{}", n.gain_db());
        // Without the change rule the window would still hold loud blocks and the gain be near
        // −14 dB.
        n.restart();
        assert_eq!(n.measured(), None);
    }

    /// The gain holds through silence: a long pause does not lift it.
    #[test]
    fn gain_holds_through_silence() {
        let mut n = Normalizer::new(OutputLayout::Stereo, NormalizerConfig::default()).unwrap();
        let x = program(3, 2, 48_000 * 8);
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
        run(&mut n, &x, &mut m);
        let g = n.gain_db();
        let z = vec![vec![0.0f32; 48_000 * 30]; 2];
        run(&mut n, &z, &mut m);
        assert!((n.gain_db() - g).abs() < 1e-9);
    }

    /// The gain never exceeds its bounds, and the per-sample gain is continuous.
    #[test]
    fn gain_is_bounded_and_smooth() {
        let mut n = Normalizer::new(OutputLayout::Stereo, NormalizerConfig::default()).unwrap();
        let quiet = vec![vec![1e-3f32; 48_000 * 20]; 2]; // −60 dBFS DC: below the freeze
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
        run(&mut n, &quiet, &mut m);
        assert_eq!(n.gain_db(), 0.0);
        let x = sine(1000.0, db_to_amp(-45.0), 48_000, 48_000 * 60, 0.0);
        let mut b = vec![x.clone(), x];
        n.process(&mut b).unwrap();
        assert!((n.gain_db() - 15.0).abs() < 0.01);
        // Sample-to-sample change: no step beyond the slope of the tone at its final level.
        let worst = b[0]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let slope = db_to_amp(-30.0) * 2.0 * std::f64::consts::PI * 1000.0 / 48_000.0;
        assert!(f64::from(worst) < 1.01 * slope, "{worst} {slope}");
    }
}
