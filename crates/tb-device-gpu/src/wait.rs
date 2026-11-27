//! How the host waits for a plan: a hybrid wait (from the C++
//! spike's `Encoder::wait`).
//!
//! A blocking wait wakes the host about 0.1 ms after the device has finished, four times per
//! tick; spinning costs a whole core. The hybrid wait sleeps for 85 % of the time the same kind
//! of wait took recently, less 0.1 ms, then polls, and blocks after all if the wait takes more
//! than twice as long as expected plus 1 ms. "The same kind" is a wait class: the C++ keys on
//! the phase (`which`: begin, quantize, finish); here the caller names a class with
//! [`crate::CudaDevice::submit_class`], or the adapter keys on the plan's wait stream and its
//! number of steps (which is the same for the same phase while the line-up does not change).

/// How [`crate::CudaDevice`] waits for a plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WaitMode {
    /// Sleep for most of the expected time, then poll, then block (the C++ default).
    #[default]
    Hybrid,
    /// Block in the driver at once (the C++ `--blocking-wait`, and its `--profile`).
    Blocking,
}

/// Classes remembered at once. A class not seen recently starts again from no estimate.
const SLOTS: usize = 16;

/// Moving averages of wait durations per class, in seconds (`double` in the C++).
#[derive(Clone, Debug)]
pub(crate) struct WaitStats {
    keys: [u64; SLOTS],
    ema: [f64; SLOTS],
    used: [u64; SLOTS],
    clock: u64,
}

impl Default for WaitStats {
    fn default() -> Self {
        Self {
            keys: [u64::MAX; SLOTS],
            ema: [0.0; SLOTS],
            used: [0; SLOTS],
            clock: 0,
        }
    }
}

/// What one hybrid wait does, from the class's average (seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Schedule {
    /// Sleep this long before the first poll (0: do not sleep).
    pub nap: f64,
    /// Block once this much time has passed since the wait began.
    pub block_after: f64,
}

impl Schedule {
    /// `nap = 0.85 * ema - 100e-6`, slept only above 50 µs; block after `2 * ema + 1e-3`
    /// (the C++ `Encoder::wait`: the same operations in the same order).
    pub fn of(ema: f64) -> Self {
        let nap = 0.85 * ema - 100e-6;
        Self {
            nap: if nap > 50e-6 { nap } else { 0.0 },
            block_after: 2.0 * ema + 1e-3,
        }
    }
}

/// The class of a plan the caller did not classify.
pub(crate) fn implicit_class(wait_stream: u8, steps: usize) -> u64 {
    (u64::from(wait_stream) << 32) | (steps as u64 & 0xffff_ffff)
}

/// The class the caller named.
pub(crate) fn explicit_class(class: u8) -> u64 {
    (1 << 63) | u64::from(class)
}

impl WaitStats {
    fn slot(&mut self, key: u64) -> usize {
        self.clock += 1;
        let i = match self.keys.iter().position(|k| *k == key) {
            Some(i) => i,
            None => {
                // Replace the class used least recently.
                let mut i = 0;
                for j in 1..SLOTS {
                    if self.used[j] < self.used[i] {
                        i = j;
                    }
                }
                self.keys[i] = key;
                self.ema[i] = 0.0;
                i
            }
        };
        self.used[i] = self.clock;
        i
    }
    /// The class's average wait in seconds; 0 when it has none yet.
    pub fn ema(&mut self, key: u64) -> f64 {
        let i = self.slot(key);
        self.ema[i]
    }
    /// Account a completed wait of `d` seconds.
    pub fn update(&mut self, key: u64, d: f64) {
        let i = self.slot(key);
        let e = self.ema[i];
        self.ema[i] = if e == 0.0 { d } else { 0.8 * e + 0.2 * d };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_is_the_cxx_formula() {
        // No estimate yet: no sleep, poll, block after 1 ms.
        assert_eq!(
            Schedule::of(0.0),
            Schedule {
                nap: 0.0,
                block_after: 1e-3
            }
        );
        let s = Schedule::of(4e-3);
        assert_eq!(s.nap, 0.85 * 4e-3 - 100e-6);
        assert_eq!(s.block_after, 2.0 * 4e-3 + 1e-3);
        // A nap of 50 µs or less is not taken.
        assert_eq!(Schedule::of(150e-6 / 0.85).nap, 0.0);
    }

    #[test]
    fn average_and_eviction() {
        let mut w = WaitStats::default();
        let a = explicit_class(0);
        assert_eq!(w.ema(a), 0.0);
        w.update(a, 2e-3);
        assert_eq!(w.ema(a), 2e-3);
        w.update(a, 4e-3);
        assert_eq!(w.ema(a), 0.8 * 2e-3 + 0.2 * 4e-3);
        // Sixteen other classes push `a` out; it starts again from nothing.
        for c in 0..SLOTS {
            w.update(implicit_class(0, c), 1e-3);
        }
        assert_eq!(w.ema(a), 0.0);
        assert_ne!(implicit_class(0, 3), explicit_class(3));
    }
}
