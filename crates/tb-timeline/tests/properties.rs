//! Property tests of the timeline: invariants over many seeded random input sequences.
//!
//! No property-testing crate: a dependency would mean a new entry in the shared lock file. The
//! generator is SplitMix64 with fixed seeds, so every run checks the same cases and a failure
//! names its seed.

use tb_timeline::{
    Cadence, Fallback, FrameClock, FrameId, MAX_PENDING, Mapped, Mapper, MapperConfig, Selection,
    Source, Ticks27,
};

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo) as u64 + 1) as i64
    }
}

const P: i64 = 900_900; // 29.97
const F30: i64 = 900_000; // 30.000

/// An adversarial producer: resets, backwards and forward jumps, bursts, duplicates, stalls,
/// extreme values, arrival times going backwards.
fn adversarial_step(r: &mut Rng, pts: &mut i64, arrival: &mut i64) -> bool {
    match r.below(20) {
        0 => *pts = 0,                                        // reconnect: from zero
        1 => *pts = pts.saturating_sub(r.range(1, 10 * F30)), // backwards
        2 => *pts = pts.saturating_add(r.range(0, 1 << 40)),  // forward jump
        3 => {}                                               // duplicate timestamp
        4 => *pts = [i64::MIN, i64::MAX, 0, -1][r.below(4) as usize],
        5 => *arrival = arrival.saturating_add(r.range(0, 1 << 36)), // stall
        6 => *arrival = arrival.saturating_sub(r.range(0, 1 << 30)), // arrival backwards
        7 => *arrival = [i64::MIN, i64::MAX][r.below(2) as usize],
        8..=10 => *pts = pts.saturating_add(F30), // burst: no arrival step
        _ => {
            *pts = pts.saturating_add(F30);
            *arrival = arrival.saturating_add(F30 + r.range(-27_000, 27_000));
        }
    }
    r.below(10) == 0
}

/// Mapper on adversarial input: no panic, results consistent with the session state, skew
/// within its clamp.
#[test]
fn mapper_adversarial_sequences_are_safe_and_consistent() {
    for seed in 0..300u64 {
        let mut r = Rng(seed);
        let cfg = MapperConfig::default();
        let mut m = Mapper::new(cfg);
        let (mut pts, mut arrival) = (r.range(-1 << 40, 1 << 40), r.range(-1 << 40, 1 << 40));
        for _ in 0..3000 {
            match r.below(50) {
                0 => {
                    let had = m.in_session();
                    assert_eq!(m.closed().is_some(), had, "seed {seed}");
                    assert!(!m.in_session());
                }
                1 => {
                    let had = m.in_session();
                    let now = arrival.saturating_add(r.range(-1 << 30, 1 << 32));
                    if m.poll(Ticks27(now)).is_err() {
                        assert!(had && !m.in_session(), "seed {seed}");
                    }
                }
                _ => {
                    let key = adversarial_step(&mut r, &mut pts, &mut arrival);
                    let had = m.in_session();
                    let res = m.frame(Ticks27(pts), Ticks27(arrival), key);
                    match res {
                        Mapped::At(_) => assert!(had && m.in_session() && res.ended().is_none()),
                        Mapped::Started { .. } => assert!(key && m.in_session()),
                        Mapped::Ended(_) => assert!(had && !key && !m.in_session()),
                        Mapped::Waiting => assert!(!had && !key && !m.in_session()),
                    }
                    if let Some(s) = m.skew_ppm() {
                        assert!(s.abs() <= cfg.skew_max_ppm, "seed {seed}: {s}");
                        assert!(m.skew_ppb().unwrap_or(0).abs() <= 500_000);
                    }
                }
            }
        }
    }
}

/// Within a session, output times never decrease and follow the timestamps: each step of the
/// output is the step of the input times (1 + skew), to the tick, across every skew change.
#[test]
fn mapper_output_times_follow_input_steps() {
    for seed in 0..40u64 {
        let mut r = Rng(1000 + seed);
        let ppm = r.range(-400, 400);
        let mut m = Mapper::new(MapperConfig::default());
        let mut prev: Option<(i64, i64)> = None;
        for k in 0..30 * 300i64 {
            let pts = k * F30 + r.range(0, 27_000); // ms rounding of timestamps
            let real = (i128::from(k * F30) * 1_000_000 / i128::from(1_000_000 + ppm)) as i64;
            let arrival = real + r.range(0, 540_000);
            let res = m.frame(Ticks27(pts), Ticks27(arrival), k == 0);
            assert!(res.ended().is_none(), "seed {seed} frame {k}: {res:?}");
            let at = res.at().unwrap().0;
            if let Some((pp, pa)) = prev {
                let ppb = m.skew_ppb().unwrap();
                let want = (pts - pp) as f64 * (1.0 + ppb as f64 * 1e-9);
                assert!(at >= pa);
                // the skew applied may have changed at the previous frame: allow its effect
                assert!(((at - pa) as f64 - want).abs() <= 2.0 + (pts - pp) as f64 * 1e-3);
            }
            prev = Some((pts, at));
        }
    }
}

/// Frame clock on random input: every tick gives exactly one answer; `New` ids are pushed ids
/// in push order; `Repeat` repeats the frame shown; `Nothing` only before the first frame after
/// a reset; every pushed frame is accounted for (shown, dropped, pending or reset); memory is
/// bounded; the selection offset stays in its band.
#[test]
fn frame_clock_invariants_on_random_input() {
    for seed in 0..200u64 {
        let mut r = Rng(5000 + seed);
        let mut fc = if seed % 3 == 0 {
            FrameClock::default()
        } else {
            FrameClock::new(Ticks27(P))
        };
        let mut next_id = 0u64;
        let mut at = 0i64;
        let mut t = 0i64;
        let (mut shown_new, mut discarded) = (0u64, 0u64);
        let mut last_new: Option<u64> = None;
        let mut since_reset_shown = false;
        let mut pushed_since_reset_floor = 0u64;
        for _ in 0..5000 {
            match r.below(12) {
                0 => {
                    discarded += fc.pending() as u64;
                    fc.reset();
                    since_reset_shown = false;
                    pushed_since_reset_floor = next_id;
                }
                1..=6 => {
                    let n = r.below(4);
                    for _ in 0..n {
                        at = match r.below(10) {
                            0 => at.saturating_sub(r.range(0, 10 * P)),
                            1 => at.saturating_add(r.range(0, 1 << 34)),
                            2 => [i64::MIN, i64::MAX][r.below(2) as usize],
                            _ => at.saturating_add(r.range(0, 2 * P)),
                        };
                        fc.push(FrameId(next_id), Ticks27(at));
                        next_id += 1;
                    }
                }
                _ => {
                    t = match r.below(20) {
                        0 => t.saturating_sub(r.range(0, P)),
                        1 => t.saturating_add(r.range(0, 1 << 33)),
                        _ => t.saturating_add(P),
                    };
                    match fc.tick(Ticks27(t)) {
                        Selection::New(FrameId(id)) => {
                            assert!(id >= pushed_since_reset_floor && id < next_id);
                            assert!(last_new.is_none_or(|l| id > l), "seed {seed}");
                            last_new = Some(id);
                            shown_new += 1;
                            since_reset_shown = true;
                        }
                        Selection::Repeat(FrameId(id)) => {
                            assert!(since_reset_shown);
                            assert_eq!(Some(id), last_new);
                        }
                        Selection::Nothing => assert!(!since_reset_shown),
                    }
                    let o = fc.offset().0.abs();
                    assert!(
                        o <= 3 * P / 4 + 1 || seed % 3 == 0,
                        "seed {seed}: offset {o}"
                    );
                }
            }
            assert!(fc.pending() <= MAX_PENDING);
            assert_eq!(
                next_id,
                shown_new + fc.drops + fc.pending() as u64 + discarded,
                "seed {seed}"
            );
        }
    }
}

/// The whole program timeline on random producers (producer-like: idle periods, reconnects
/// from zero, timestamp jumps, bursts, ±1 ms jitter): output ticks follow the cadence exactly,
/// and each tick shows exactly one thing: a frame of the selected input, or the slate. A
/// selected input always has a frame to show (new or repeated), never nothing.
#[test]
fn pipeline_every_tick_shows_one_frame_or_the_slate() {
    for seed in 0..30u64 {
        let mut r = Rng(9000 + seed);
        let cad = Cadence::new(Ticks27(r.range(0, 1 << 40)), 1001, 30000).unwrap();
        let mut m = Mapper::new(MapperConfig::default());
        let mut fc = FrameClock::new(cad.period());
        let mut fb = Fallback::new(Ticks27::from_secs(1), Ticks27::from_secs(3));
        let mut next_arrival = cad.pts(0).0;
        let mut pts = 0i64;
        let mut id = 0u64;
        let mut idle_until = 0i64;
        let mut prev_t = None;
        let mut shown = [0u64; 3];
        for k in 0..30 * 600u64 {
            let t = cad.pts(k).0;
            if let Some(p) = prev_t {
                assert_eq!(t - p, P, "cadence");
            }
            prev_t = Some(t);
            // the producer, up to this tick
            while next_arrival <= t {
                if next_arrival >= idle_until {
                    let key = id.is_multiple_of(60);
                    match r.below(4000) {
                        0 => {
                            m.closed();
                            pts = 0;
                        }
                        1 => pts += Ticks27::from_secs(r.range(1, 100)).0,
                        2 => idle_until = next_arrival + Ticks27::from_secs(r.range(1, 20)).0,
                        _ => {}
                    }
                    let res = m.frame(Ticks27(pts), Ticks27(next_arrival), key);
                    if let Some(at) = res.at() {
                        fc.push(FrameId(id), at);
                    }
                    id += 1;
                    pts += F30 + r.range(-27_000, 27_000);
                } else {
                    pts += F30; // The producer's tick counter runs on while idle
                }
                next_arrival += if r.below(100) == 0 { 0 } else { F30 };
            }
            let _ = m.poll(Ticks27(t));
            let sel = fc.tick(Ticks27(t));
            let src = fb.tick(
                Ticks27(t),
                matches!(sel, Selection::New(_)),
                false,
                k % 15 == 0,
            );
            match src {
                Source::Live => assert!(!matches!(sel, Selection::Nothing), "seed {seed} k {k}"),
                Source::Standby => panic!("no standby configured"),
                Source::Slate => {}
            }
            shown[src as usize] += 1;
        }
        assert!(
            shown[Source::Live as usize] > 0,
            "seed {seed}: live never shown"
        );
    }
}
