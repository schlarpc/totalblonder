//! Process time: 27 MHz ticks since the process epoch. Every carrier's STC is its own offset
//! plus this, and its pacer sends slot `n` at `epoch + n · 1504 / R`, so all of them advance
//! with the same clock (SPEC §5.1).
//!
//! Two implementations of [`Clock`]:
//! - [`RealClock`]: `CLOCK_MONOTONIC` through `Instant`;
//! - [`VirtualClock`]: time that moves only when the offline driver advances it, and only once
//!   every registered worker is waiting: the whole server then runs as fast as the work allows
//!   and produces the same stream every time (offline mode, for tests on machines without the
//!   card and for files).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 27 MHz.
pub const HZ: u64 = 27_000_000;

/// Process time for the workers.
pub trait Clock: Send + Sync {
    /// Ticks since the epoch.
    fn now(&self) -> u64;
    /// Waits until `t`; `false` when the server is stopping.
    fn sleep_until(&self, t: u64) -> bool;
    /// Unix time in nanoseconds at process time `t` (for the STT and the guide).
    fn unix_nanos_at(&self, t: u64) -> i128;
    /// Whether the server is stopping.
    fn stopping(&self) -> bool;
}

/// Wall-clock process time.
pub struct RealClock {
    epoch: Instant,
    unix_at_epoch: i128,
    stop: Arc<AtomicBool>,
}

impl RealClock {
    /// A clock whose epoch is `epoch`.
    pub fn new(epoch: Instant, stop: Arc<AtomicBool>) -> Self {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as i128);
        let unix_at_epoch = since
            - Instant::now().saturating_duration_since(epoch).as_nanos() as i128
            + epoch.saturating_duration_since(Instant::now()).as_nanos() as i128;
        RealClock {
            epoch,
            unix_at_epoch,
            stop,
        }
    }

    /// The epoch.
    pub fn epoch(&self) -> Instant {
        self.epoch
    }

    fn instant(&self, t: u64) -> Instant {
        self.epoch + Duration::from_nanos((u128::from(t) * 1000 / 27) as u64)
    }
}

impl Clock for RealClock {
    fn now(&self) -> u64 {
        let d = Instant::now().saturating_duration_since(self.epoch);
        (d.as_nanos() * 27 / 1000) as u64
    }
    fn sleep_until(&self, t: u64) -> bool {
        let at = self.instant(t);
        loop {
            if self.stopping() {
                return false;
            }
            let now = Instant::now();
            if now >= at {
                return true;
            }
            std::thread::sleep((at - now).min(Duration::from_millis(50)));
        }
    }
    fn unix_nanos_at(&self, t: u64) -> i128 {
        self.unix_at_epoch + i128::from(t) * 1000 / 27
    }
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct VState {
    now: u64,
    /// For each registered worker: `Some(t)` while it waits for time `t`, `None` while busy.
    waits: Vec<Option<u64>>,
    stop: bool,
}

/// Time that moves only when the driver says so and every worker is waiting.
pub struct VirtualClock {
    s: Mutex<VState>,
    cv: Condvar,
    unix0: i128,
}

/// A worker's registration with a [`VirtualClock`].
pub struct Worker {
    clock: Arc<VirtualClock>,
    id: usize,
}

impl VirtualClock {
    /// A clock at 0 whose epoch is Unix time `unix0` (nanoseconds).
    pub fn new(unix0: i128) -> Arc<Self> {
        Arc::new(VirtualClock {
            s: Mutex::new(VState {
                now: 0,
                waits: Vec::new(),
                stop: false,
            }),
            cv: Condvar::new(),
            unix0,
        })
    }

    /// Registers a worker (busy until its first wait).
    pub fn register(self: &Arc<Self>) -> Worker {
        let mut s = self.s.lock().unwrap_or_else(|e| e.into_inner());
        s.waits.push(None);
        Worker {
            clock: self.clone(),
            id: s.waits.len() - 1,
        }
    }

    /// Advances time to `t` once every worker is waiting; workers whose time comes are woken,
    /// and time stops at each of their wake times on the way (so their output for an earlier
    /// time is produced before time passes it). Returns `false` if stopping.
    pub fn advance_to(&self, t: u64) -> bool {
        let mut s = self.s.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            while !s.stop && s.waits.iter().any(Option::is_none) {
                s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if s.stop {
                return false;
            }
            if s.now >= t {
                return true;
            }
            let next = s
                .waits
                .iter()
                .flatten()
                .copied()
                .filter(|&w| w > s.now)
                .min()
                .unwrap_or(t)
                .min(t);
            s.now = next;
            // Workers whose time has come are busy from now until they wait again: time does
            // not move on until they have.
            for w in s.waits.iter_mut() {
                if w.is_some_and(|x| x <= next) {
                    *w = None;
                }
            }
            self.cv.notify_all();
        }
    }

    /// Stops every worker.
    pub fn stop(&self) {
        let mut s = self.s.lock().unwrap_or_else(|e| e.into_inner());
        s.stop = true;
        self.cv.notify_all();
    }

    /// Unix time in nanoseconds at virtual time `t`.
    pub fn unix_nanos_at(&self, t: u64) -> i128 {
        self.unix0 + i128::from(t) * 1000 / 27
    }

    /// Current time.
    pub fn time(&self) -> u64 {
        self.s.lock().unwrap_or_else(|e| e.into_inner()).now
    }
}

impl Clock for Worker {
    fn now(&self) -> u64 {
        self.clock.time()
    }
    fn sleep_until(&self, t: u64) -> bool {
        let c = &self.clock;
        let mut s = c.s.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = s.waits.get_mut(self.id) {
            *w = Some(t);
        }
        c.cv.notify_all();
        while !s.stop && s.now < t {
            s = c.cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        if let Some(w) = s.waits.get_mut(self.id) {
            *w = None;
        }
        !s.stop
    }
    fn unix_nanos_at(&self, t: u64) -> i128 {
        self.clock.unix0 + i128::from(t) * 1000 / 27
    }
    fn stopping(&self) -> bool {
        self.clock.s.lock().map(|s| s.stop).unwrap_or(true)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // A finished worker no longer holds time back.
        let mut s = self.clock.s.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = s.waits.get_mut(self.id) {
            *w = Some(u64::MAX);
        }
        self.clock.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_time_waits_for_workers() {
        let c = VirtualClock::new(0);
        let w = c.register();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let h = std::thread::spawn(move || {
            for k in 1..=5u64 {
                if !w.sleep_until(k * 100) {
                    break;
                }
                // "Work" done at time k·100, before time moves on.
                std::thread::sleep(Duration::from_millis(2));
                l2.lock().unwrap().push((k * 100, w.now()));
            }
        });
        assert!(c.advance_to(1000));
        h.join().unwrap();
        let v = log.lock().unwrap();
        assert_eq!(v.len(), 5);
        for (want, saw) in v.iter() {
            assert_eq!(want, saw, "time moved while the worker was busy");
        }
    }
}
