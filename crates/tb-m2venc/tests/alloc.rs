//! Hot-path discipline of the driver: in steady state a tick of the run loop allocates the
//! bytes of each access unit (one vector per picture, made by the encoder) and nothing else.
//! Measured as the difference between a long and a short run of the same line-up, so that
//! everything that happens once (clips, files, batches, the growth of working vectors) drops
//! out.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use common::{Scratch, parse};
use tb_device_ref::RefDevice;
use tb_m2venc::{Status, arena_sizes, run};

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);

#[allow(
    unsafe_code,
    reason = "a counting global allocator for this test binary"
)]
// SAFETY: every call is passed to the system allocator unchanged; the counter is an atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller's contract is that of `System.alloc`.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: `p` came from `System.alloc` with this layout.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as `System.realloc`.
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Allocations of a whole run of `frames` frames per session.
fn allocations(dir: &Scratch, frames: u32, extra: &str) -> u64 {
    let clip = dir.clip("tiny-i");
    // fixed quantizer: no rate change, no correction pass, nothing shed
    let o = parse(&format!(
        "--input {clip} --size 64x48 --interlaced --gop 6 --bframes 2 --q 8 --sessions 3 \
         --stagger --cc --frames {frames} --write 3 --output {} --ticklog {} {extra}",
        dir.path("a"),
        dir.path("ticks.txt")
    ));
    // without the check of the host-arena rule, which snapshots every upload
    let dev = RefDevice::new(arena_sizes(&o).unwrap()).unwrap();
    let before = ALLOCS.load(Ordering::Relaxed);
    let out = run(&o, dev, &mut Vec::new()).unwrap();
    let n = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(out.status, Status::Complete);
    assert_eq!(out.report.pictures, 3 * u64::from(frames));
    n
}

#[test]
fn a_tick_allocates_only_its_access_units() {
    let dir = Scratch::new("alloc");
    for extra in ["", "--overlap", "--test-input-copy", "--input-mode dev"] {
        let short = allocations(&dir, 40, extra);
        let long = allocations(&dir, 140, extra);
        let per_picture = (long - short) as f64 / 300.0;
        // One vector per access unit, and the caption bytes of each frame (a vector per
        // submitted frame in the encoder's GOP queue, as the C++ copies them).
        assert!(
            per_picture <= 2.05,
            "{extra:?}: {per_picture:.3} allocations per picture ({short} for 40 frames, \
             {long} for 140)"
        );
    }
}
