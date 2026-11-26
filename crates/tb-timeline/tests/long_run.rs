//! Long runs: 30 days of a program's timeline, for wraparound (33-bit PTS in, 33-bit PTS out)
//! and drift (producer clock off by tens of ppm; exact cadence).

use tb_timeline::{
    Cadence, FrameClock, FrameId, HZ, Mapped, Mapper, MapperConfig, Selection, Ticks27, Unwrapper,
    to_pts90,
};

/// Outcome of [`thirty_days`].
#[derive(Debug)]
struct Outcome {
    ticks: u64,
    frames: u64,
    drops: u64,
    repeats: u64,
    sessions: u64,
    skew_ppb: i64,
    /// Smallest and largest gap between drop events, in ticks.
    drop_gaps: (u64, u64),
    /// Depth (at − arrival) range over the last day, in ticks.
    depth: (i64, i64),
}

/// A producer at `num/den` seconds a frame on its own clock, which runs `ppm` slow, sends 33-bit
/// 90 kHz timestamps starting near the wrap, with 0..5 ms of arrival jitter; the program outputs
/// frames of `out_num/out_den` seconds for `days`. `scale` divides both frame rates (a scaled
/// run has the same magnitudes of time and drift with fewer frames).
fn thirty_days(days: i64, num: i64, den: i64, out_num: u64, out_den: u64, ppm: i64) -> Outcome {
    run(
        days,
        num,
        den,
        out_num,
        out_den,
        ppm,
        MapperConfig::default(),
    )
}

/// [`thirty_days`] with the mapper's parameters given.
fn run(
    days: i64,
    num: i64,
    den: i64,
    out_num: u64,
    out_den: u64,
    ppm: i64,
    cfg: MapperConfig,
) -> Outcome {
    let Ok(cad) = Cadence::new(Ticks27(HZ * 1000), out_num, out_den) else {
        panic!("bad cadence {out_num}/{out_den}");
    };
    let period = cad.period().0;
    let mut m = Mapper::new(cfg);
    let mut fc = FrameClock::new(cad.period());
    let mut unwrap = Unwrapper::mpeg_90khz();
    let pts90_0 = (1u64 << 33) - 90_000 * 60; // wraps after one minute, then every 26.5 h
    let end = cad.p0().0 + days * 86_400 * HZ;
    let mut seed = 42u64;
    let mut out = Outcome {
        ticks: 0,
        frames: 0,
        drops: 0,
        repeats: 0,
        sessions: 0,
        skew_ppb: 0,
        drop_gaps: (u64::MAX, 0),
        depth: (i64::MAX, i64::MIN),
    };
    let mut k_in = 0i64;
    let mut last_new: Option<u64> = None;
    let mut last_drop_tick: Option<u64> = None;
    let start_arrival = cad.p0().0 - HZ; // the producer starts a second before the output
    let mut prev_pts90: Option<u64> = None;
    for k in 0u64.. {
        let t = cad.pts(k).0;
        if t > end {
            break;
        }
        // output PTS on the wire: 33 bits, steps of one frame period across every wrap
        let p90 = to_pts90(Ticks27(t));
        if let Some(p) = prev_pts90 {
            let step: u64 = (p90 + (1 << 33) - p) % (1 << 33);
            assert!(step.abs_diff(period as u64 / 300) <= 1);
        }
        prev_pts90 = Some(p90);
        // frames that arrived by the tick (the mapper puts them J ahead)
        loop {
            // producer time of frame k_in, in its own 27 MHz clock, then in ours
            let own = i128::from(k_in) * i128::from(HZ) * i128::from(num) / i128::from(den);
            let ours = (own * 1_000_000 / i128::from(1_000_000 - ppm)) as i64;
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let arrival = start_arrival + ours + ((seed >> 33) % 135_000) as i64;
            if arrival > t {
                break;
            }
            let raw = (pts90_0 + (own / 300) as u64) % (1 << 33);
            let pts = unwrap.unwrap(raw);
            let r = m.frame(pts, Ticks27(arrival), k_in == 0);
            match r {
                Mapped::Started { .. } => out.sessions += 1,
                Mapped::At(_) => {}
                other => panic!("frame {k_in}: {other:?}"),
            }
            let Some(at) = r.at() else {
                panic!("frame {k_in} not placed: {r:?}");
            };
            if t > end - 86_400 * HZ {
                let d = at.0 - arrival;
                out.depth = (out.depth.0.min(d), out.depth.1.max(d));
            }
            fc.push(FrameId(k_in as u64), at);
            k_in += 1;
            out.frames += 1;
        }
        assert_eq!(m.poll(Ticks27(t)), Ok(()));
        match fc.tick(Ticks27(t)) {
            Selection::New(FrameId(id)) => {
                if let Some(l) = last_new
                    && id > l + 1
                {
                    assert_eq!(id, l + 2, "one drop at a time");
                    if let Some(g) = last_drop_tick {
                        let gap = k - g;
                        out.drop_gaps = (out.drop_gaps.0.min(gap), out.drop_gaps.1.max(gap));
                    }
                    last_drop_tick = Some(k);
                }
                last_new = Some(id);
            }
            Selection::Repeat(_) => {}
            Selection::Nothing => assert!(last_new.is_none()),
        }
        out.ticks += 1;
    }
    out.drops = fc.drops;
    out.repeats = fc.repeats;
    out.skew_ppb = m.skew_ppb().unwrap_or(i64::MIN);
    out
}

fn check_30fps_into_2997(o: &Outcome, ppm: i64, below_ms: i64) {
    // one session for the whole run: the input PTS wrapped 27 times without a discontinuity
    assert_eq!(o.sessions, 1, "{o:?}");
    // the skew found the producer's clock error (ppm slow -> positive skew)
    assert!((o.skew_ppb - ppm * 1000).abs() < 15_000, "{o:?}");
    // the buffer depth stays at J. The skew estimate corrects the slope only, and its
    // estimation noise used to integrate into a slow random walk of the depth (36 to 41 ms
    // after 30 days in the full-rate run); the depth term pulls the depth back to J with a
    // time constant of ten minutes. On the last day the depth of the fastest frames is within
    // J + 0.2 ms (0.13 ms after 30 days), and the others are up to their 5 ms of arrival
    // jitter below it (the depth here is `at − arrival`, jitter included). `below_ms`: 6 for
    // a month; 8 for a single day, whose "last day" has the start in it (the first skew window
    // fills before the slope is known).
    let j = HZ;
    assert!(
        o.depth.0 > j - below_ms * HZ / 1000 && o.depth.1 < j + HZ / 1000,
        "{o:?}"
    );
    // input faster than output: drops only, evenly spread, and every frame accounted for
    assert_eq!(o.repeats, 0, "{o:?}");
    let pending_max = 2 * 30 + 2;
    assert!(o.frames - o.drops >= o.ticks - 1, "{o:?}");
    assert!(o.frames - o.drops <= o.ticks + pending_max, "{o:?}");
    // 30 fps 37 ppm slow into 29.97: one drop every 1/(1.001·(1−37e-6) − 1) ≈ 1038 ticks
    assert!(o.drop_gaps.0 >= 1000 && o.drop_gaps.1 <= 1080, "{o:?}");
}

/// 30 days at the real rates (30.000 fps, 37 ppm slow, into 29.97): 77.7 million ticks. Slow in
/// a debug build; run with `cargo test --release -- --ignored`.
#[test]
#[ignore = "77.7 million ticks; run in release with --ignored"]
fn thirty_days_full_rate_30fps_into_2997() {
    let o = thirty_days(30, 1, 30, 1001, 30000, 37);
    eprintln!("30 days at full rate: {o:?}");
    check_30fps_into_2997(&o, 37, 6);
}

/// One day at the real rates (2.6 million ticks), and 30 days with both rates divided by 30
/// (one frame a second: the same time magnitudes, the same number of PTS wraps, the same drift
/// in ppm, 2.6 million ticks). With one frame a second a skew bucket holds two frames, so the
/// estimate is noisier: without the depth term the depth wandered by tens of ms over the month
/// (67 ms); with it the depth stays within J − 6 ms (5 ms of that is arrival jitter) and
/// J + 5 ms. The same run without the term (`depth_tau` zero) shows the random walk it bounds.
#[test]
fn thirty_days_scaled_and_one_day_full_rate() {
    let o = thirty_days(1, 1, 30, 1001, 30000, 37);
    check_30fps_into_2997(&o, 37, 8);
    let o = thirty_days(30, 1, 1, 1001, 1000, -23);
    eprintln!("30 days scaled: {o:?}");
    assert_eq!(o.sessions, 1, "{o:?}");
    assert!((o.skew_ppb + 23_000).abs() < 15_000, "{o:?}");
    assert!(
        o.depth.0 > HZ - 6 * HZ / 1000 && o.depth.1 < HZ + 5 * HZ / 1000,
        "{o:?}"
    );
    let free = run(
        30,
        1,
        1,
        1001,
        1000,
        -23,
        MapperConfig {
            depth_tau: Ticks27(0),
            ..MapperConfig::default()
        },
    );
    eprintln!("30 days scaled, no depth term: {free:?}");
    assert_eq!(free.sessions, 1, "{free:?}");
    // without the term the depth of the last day is tens of ms from J (inside the depth
    // rule's [0, 2J], which is why this was not a fault, only an unbounded walk)
    let off = (free.depth.0 - HZ).abs().max((free.depth.1 - HZ).abs());
    assert!(off > 20 * HZ / 1000 && off < HZ / 10, "{free:?}");
    assert_eq!(o.repeats, 0, "{o:?}");
    assert!(o.drop_gaps.0 >= 900 && o.drop_gaps.1 <= 1100, "{o:?}");
}
