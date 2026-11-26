//! Counters shared between threads and the control API (SPEC §11.3). Rendering as Prometheus
//! text is in [`crate::control`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A monotonically increasing counter.
#[derive(Debug, Clone, Default)]
pub struct Counter(Arc<AtomicU64>);

impl Counter {
    /// Adds one.
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    /// Adds `n`.
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    /// The value.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A value that can go up and down.
#[derive(Debug, Clone, Default)]
pub struct Gauge(Arc<AtomicU64>);

impl Gauge {
    /// Sets it.
    pub fn set(&self, v: u64) {
        self.0.store(v, Ordering::Relaxed);
    }
    /// The value.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}
