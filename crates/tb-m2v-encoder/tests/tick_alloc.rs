//! Work package E4, acceptance 6: after warm-up, `begin_tick` and `resume` allocate the bytes
//! of each access unit and nothing else (docs/ARCHITECTURE.md §8.1). A counting global
//! allocator counts the allocations made on this thread while the encoder is being called.
#![allow(
    unsafe_code,
    reason = "a global allocator that counts; it forwards to the system allocator"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::model::Model;
use tb_device::{ArenaSizes, Device, Nanos, Plan, Ticket};
use tb_m2v_encoder::{Encoder, EncoderOptions, FrameSource, SessionConfig, SessionId, Step};

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

fn note() {
    // `try_with`: the allocator also runs while a thread is torn down.
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

// SAFETY: every call is forwarded unchanged to the system allocator, which upholds the
// contract of `GlobalAlloc`; the counting touches only thread-local cells.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's contract is passed on to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` with this layout (see `alloc`, `realloc`).
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller's contract is passed on to the system allocator.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: `ptr` came from `System` with this layout; the caller's contract is passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Run `f` with the counter on; the allocations it made.
fn counted<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = COUNT.with(Cell::get);
    COUNTING.with(|c| c.set(true));
    let out = f();
    COUNTING.with(|c| c.set(false));
    (out, COUNT.with(Cell::get) - before)
}

const ARENAS: ArenaSizes = ArenaSizes {
    device: 8 << 20,
    host: 1 << 20,
};

/// One tick; the allocations inside `begin_tick` and `resume`.
fn tick(enc: &mut Encoder, dev: &mut Model, plan: &mut Plan) -> u64 {
    let now = dev.now();
    let (mut step, mut allocs) =
        counted(|| enc.begin_tick(now, Some(Nanos(now.0 + 50_000_000)), dev.host_mut(), plan));
    let mut ticket = Ticket(0);
    loop {
        match step {
            Step::Done => return allocs,
            Step::Run => ticket = dev.submit(plan).unwrap(),
            Step::Wait => {}
            Step::Queue => panic!("split plans are off in these tests"),
        }
        let done = dev.wait(ticket, None);
        let now = dev.now();
        let (s, n) = counted(|| enc.resume(&done, now, dev.host_mut(), plan));
        step = s;
        allocs += n;
    }
}

fn steady_state(overlap: bool) {
    let opt = EncoderOptions {
        overlap,
        ..EncoderOptions::default()
    };
    let mut enc = Encoder::new(opt, ARENAS);
    let mut dev = Model::new(ARENAS);
    let mut plan = Plan::new();
    let cbr = SessionConfig {
        w: 64,
        h: 48,
        gop: 6,
        m: 3,
        max_bitrate: 400_000,
        vbv_bits: 196_608,
        bitrate: 400_000,
        capacity: 3,
        profile: tb_m2v_syntax::profile::Profile::H262,
        ..SessionConfig::default()
    };
    let fixed = SessionConfig {
        w: 72,
        h: 40,
        bitrate: 0,
        capacity: 2,
        ..cbr
    };
    let sessions: Vec<SessionId> = [cbr, cbr, cbr, fixed, fixed]
        .iter()
        .map(|c| enc.open(c).unwrap())
        .collect();
    let cc = common::captions();
    let mut pictures = 0u64;
    let mut counted_allocs = 0u64;
    let mut counted_pictures = 0u64;
    let mut repeats = 0u64;
    for t in 0..120u64 {
        // one session is on hold for a while (repeats made on the host), another repeats frames
        enc.hold(sessions[2], (60..75).contains(&t)).unwrap();
        for (k, s) in sessions.iter().enumerate() {
            let source = if k == 1 && t % 4 == 3 {
                FrameSource::Again
            } else {
                let buf = enc.frame_buffer(*s).unwrap();
                buf.of_mut(dev.host_mut())
                    .unwrap()
                    .fill(30 + ((t * 5 + k as u64 * 11) % 50) as u8);
                FrameSource::Host
            };
            enc.submit(*s, source, t as i64 * 3003, &cc).unwrap();
        }
        let allocs = tick(&mut enc, &mut dev, &mut plan);
        let mut made = 0;
        for s in &sessions {
            while let Some(au) = enc.take_output(*s) {
                made += 1;
                repeats += u64::from(au.repeat);
                assert!(au.recon.is_empty());
            }
        }
        pictures += made;
        if t >= 30 {
            assert_eq!(
                allocs, made,
                "tick {t} (overlap {overlap}): {allocs} allocations for {made} access units"
            );
            counted_allocs += allocs;
            counted_pictures += made;
        }
    }
    assert!(
        pictures > 500 && repeats >= 10,
        "{pictures} pictures, {repeats} repeats"
    );
    assert_eq!(counted_allocs, counted_pictures);
    assert!(counted_pictures > 400);
}

#[test]
fn begin_tick_and_resume_allocate_only_the_access_units() {
    steady_state(false);
    steady_state(true);
}
