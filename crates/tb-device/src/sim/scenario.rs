//! Scenarios for tests of the tick: a trace, the tick and phase of each of its plans, and
//! faults placed by phase ("the second `Quant` plan of tick 7 fails with `Lost`", "every plan
//! from tick 3 to 5 takes 9 ms longer") instead of by submission index.

use core::ops::RangeInclusive;

use super::{Compare, Fault, Replay, Trace};
use crate::{DeviceError, Nanos};

/// The phase of a tick a plan belongs to (the C++ waits of `Encoder::step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Parameters, motion estimation, transform, row statistics (`wait(st, 0)`).
    Begin,
    /// Quantization and entropy coding: the first pass (`wait(st, 1)`) and every correction
    /// pass after it.
    Quant,
    /// Scatter, reconstruction, download of the coded pictures (`wait(st, 2)`).
    Finish,
    /// Any other wait: set-up and shut-down, the host's wait for the upload stream.
    Other,
}

/// Where a plan of a trace belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlanTag {
    /// The encoder's step count (-1 outside a tick).
    pub tick: i64,
    /// The phase.
    pub phase: Phase,
}

/// Which plans of a trace a fault applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Select {
    ticks: RangeInclusive<i64>,
    phase: Option<Phase>,
    nth: Option<usize>,
}

impl Select {
    /// Every plan of one tick.
    pub fn tick(tick: i64) -> Self {
        Self::ticks(tick..=tick)
    }
    /// Every plan of these ticks.
    pub fn ticks(ticks: RangeInclusive<i64>) -> Self {
        Self {
            ticks,
            phase: None,
            nth: None,
        }
    }
    /// Only the plans of this phase.
    pub fn phase(self, phase: Phase) -> Self {
        Self {
            phase: Some(phase),
            ..self
        }
    }
    /// Only the `n`-th (from 0) of the selected plans of each tick: `Select::tick(7)
    /// .phase(Phase::Quant).nth(1)` is the second `Quant` plan of tick 7.
    pub fn nth(self, n: usize) -> Self {
        Self {
            nth: Some(n),
            ..self
        }
    }

    /// Indices (submission order) of the selected plans.
    pub fn plans(&self, tags: &[PlanTag]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut tick = None;
        let mut count = 0;
        for (i, t) in tags.iter().enumerate() {
            if !self.ticks.contains(&t.tick) || self.phase.is_some_and(|p| p != t.phase) {
                continue;
            }
            if tick != Some(t.tick) {
                tick = Some(t.tick);
                count = 0;
            }
            if self.nth.is_none_or(|n| n == count) {
                out.push(i);
            }
            count += 1;
        }
        out
    }
}

/// What is wrong with a scenario.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScenarioError {
    /// The tags do not describe the trace's plans one for one.
    #[error("{tags} tags for {plans} plans")]
    TagCount {
        /// Plans in the trace.
        plans: usize,
        /// Tags given.
        tags: usize,
    },
    /// A fault selects no plan (the test would not test what it says).
    #[error("no plan matches {0:?}")]
    NoMatch(Select),
}

/// A trace, its tags and the faults to inject; [`Scenario::replay`] makes the device.
#[derive(Clone, Debug)]
pub struct Scenario {
    trace: Trace,
    tags: Vec<PlanTag>,
    faults: Vec<Fault>,
    compare: Compare,
}

impl Scenario {
    /// A scenario without faults. `tags` has one entry per plan of the trace.
    pub fn new(trace: Trace, tags: Vec<PlanTag>) -> Result<Self, ScenarioError> {
        if tags.len() != trace.records.len() {
            return Err(ScenarioError::TagCount {
                plans: trace.records.len(),
                tags: tags.len(),
            });
        }
        Ok(Self {
            trace,
            tags,
            faults: Vec::new(),
            compare: Compare::Exact,
        })
    }

    fn each(mut self, select: Select, f: impl Fn(usize) -> Fault) -> Result<Self, ScenarioError> {
        let plans = select.plans(&self.tags);
        if plans.is_empty() {
            return Err(ScenarioError::NoMatch(select));
        }
        self.faults.extend(plans.into_iter().map(f));
        Ok(self)
    }

    /// The selected plans fail with `error`.
    pub fn fail(self, select: Select, error: DeviceError) -> Result<Self, ScenarioError> {
        self.each(select, |submit| Fault::Fail {
            submit,
            error: error.clone(),
        })
    }

    /// The selected plans take `extra` longer than recorded.
    pub fn delay(self, select: Select, extra: Nanos) -> Result<Self, ScenarioError> {
        self.each(select, |submit| Fault::Delay { submit, extra })
    }

    /// How the replay compares submitted plans with the trace (default [`Compare::Exact`]).
    pub fn compare(self, compare: Compare) -> Self {
        Self { compare, ..self }
    }

    /// The tags.
    pub fn tags(&self) -> &[PlanTag] {
        &self.tags
    }

    /// The faults so far, by submission index.
    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }

    /// The device that replays the scenario.
    pub fn replay(self) -> Replay {
        Replay::with_faults(self.trace, self.faults).compare_by(self.compare)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::tests::trace;
    use crate::{Device, selftest};

    fn tags() -> Vec<PlanTag> {
        let t = |tick, phase| PlanTag { tick, phase };
        vec![
            t(-1, Phase::Other),
            t(6, Phase::Begin),
            t(6, Phase::Quant),
            t(6, Phase::Finish),
            t(7, Phase::Other),
            t(7, Phase::Begin),
            t(7, Phase::Quant),
            t(7, Phase::Quant),
            t(7, Phase::Quant),
            t(7, Phase::Finish),
            t(8, Phase::Begin),
        ]
    }

    #[test]
    fn selections() {
        let t = tags();
        assert_eq!(Select::tick(7).phase(Phase::Quant).nth(1).plans(&t), [7]);
        assert_eq!(Select::tick(7).phase(Phase::Quant).plans(&t), [6, 7, 8]);
        assert_eq!(
            Select::ticks(6..=7).phase(Phase::Quant).nth(0).plans(&t),
            [2, 6]
        );
        assert_eq!(Select::ticks(7..=8).plans(&t), [4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(Select::tick(7).nth(0).plans(&t), [4]);
        assert!(Select::tick(9).plans(&t).is_empty());
        assert!(
            Select::tick(6)
                .phase(Phase::Quant)
                .nth(1)
                .plans(&t)
                .is_empty()
        );
    }

    #[test]
    fn faults_by_phase() {
        let mut tr = trace();
        let one = tr.records[0].clone();
        tr.records = vec![one; 11];
        let s = Scenario::new(tr.clone(), tags())
            .unwrap()
            .fail(
                Select::tick(7).phase(Phase::Quant).nth(1),
                DeviceError::Lost { code: 999 },
            )
            .unwrap()
            .delay(Select::ticks(6..=7).phase(Phase::Finish), Nanos(9_000_000))
            .unwrap();
        assert_eq!(
            s.faults(),
            [
                Fault::Fail {
                    submit: 7,
                    error: DeviceError::Lost { code: 999 }
                },
                Fault::Delay {
                    submit: 3,
                    extra: Nanos(9_000_000)
                },
                Fault::Delay {
                    submit: 9,
                    extra: Nanos(9_000_000)
                },
            ]
        );
        let mut dev = s.replay();
        let mut results = Vec::new();
        for _ in 0..11 {
            let before = dev.now();
            let r = selftest::run(&mut dev);
            results.push((r.map(|_| ()), dev.now().0 - before.0));
        }
        let ok = |ns| (Ok(()), ns);
        assert_eq!(results[3], ok(9_250_000));
        assert_eq!(results[7], (Err(DeviceError::Lost { code: 999 }), 250_000));
        assert_eq!(results[9], ok(9_250_000));
        assert_eq!(results[0], ok(250_000));
        assert!(dev.exhausted());

        assert_eq!(
            Scenario::new(tr.clone(), tags()[..3].to_vec()).unwrap_err(),
            ScenarioError::TagCount { plans: 11, tags: 3 }
        );
        assert!(matches!(
            Scenario::new(tr, tags())
                .unwrap()
                .delay(Select::tick(12), Nanos(1)),
            Err(ScenarioError::NoMatch(_))
        ));
    }
}
