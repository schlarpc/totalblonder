//! Work package E3: the deadline policy against the `shed` golden case (test hook) and the
//! C++ text of `Encoder::plan` (deadline path).

use tb_m2v_encoder::shed::{Candidate, ShedState};

/// Case `shed`: per tick, the candidates of the `shed_cand` records and the generator state of
/// the `tick` record give the `shed` record.
#[test]
fn golden_hook() {
    let recs = tb_golden::case("shed").unwrap();
    let args = recs[0].text("args").unwrap().to_owned();
    let spec = args
        .split_whitespace()
        .skip_while(|a| *a != "--force-shed")
        .nth(1)
        .unwrap();
    let (p, seed) = spec.split_once(':').unwrap();
    let p: f64 = p.parse().unwrap();
    let seed: u64 = seed.parse().unwrap();

    let mut st = ShedState::seeded(seed);
    let mut cands: Vec<Candidate> = Vec::new();
    let mut ticks = 0;
    let mut with_shed = 0;
    for r in &recs {
        match r.kind() {
            "tick" => {
                let tick = r.int("tick").unwrap() as u64;
                if tick == 0 {
                    assert_eq!(st.rng, r.u64_le("rng").unwrap(), "seed");
                }
                assert_eq!(r.num("force_shed").unwrap(), p);
                assert!(!r.flag("deadline").unwrap());
                st.steps = tick;
                st.rng = r.u64_le("rng").unwrap();
                cands.clear();
            }
            "shed_cand" => {
                let tick = r.int("tick").unwrap() as u64;
                assert_eq!(tick, st.steps);
                let k = cands.len() as u64;
                assert_eq!(
                    ShedState::rotation(tick, k),
                    r.int("rot").unwrap() as u64,
                    "rot"
                );
                cands.push(Candidate {
                    session: r.int("s").unwrap() as u32,
                    priority: r.int("priority").unwrap() as i32,
                    class: r.int("class").unwrap() as u8,
                    mbs: r.num("mbs").unwrap(),
                    first: !r.flag("have_ref").unwrap(),
                });
            }
            "shed" => {
                let tick = r.int("tick").unwrap() as u64;
                assert_eq!(tick, st.steps);
                assert_eq!(r.int("candidates").unwrap() as usize, cands.len());
                let mut shed = vec![false; cands.len()];
                let n = st.plan(&cands, None, 1.10, Some(p), &mut shed);
                let got: Vec<i64> = cands
                    .iter()
                    .zip(&shed)
                    .filter(|(_, s)| **s)
                    .map(|(c, _)| i64::from(c.session))
                    .collect();
                assert_eq!(got, r.ints("shed").unwrap(), "shed list, tick {tick}");
                assert_eq!(i64::from(n), r.int("shed_count").unwrap(), "tick {tick}");
                assert_eq!(st.rng, r.u64_le("rng").unwrap(), "rng after, tick {tick}");
                ticks += 1;
                with_shed += usize::from(n > 0);
            }
            _ => {}
        }
    }
    assert_eq!(ticks, 32);
    assert!(with_shed > 0);
}

/// Case `deadline` (a real deadline on the reference adapter's step clock): per tick, the
/// throughput history of `shed_hist`, the candidates of `shed_cand` and the budget of
/// `shed_budget` give the `shed` record and the budget itself; without a `shed_budget` record
/// the policy stops before it computes one (the warm-up).
#[test]
fn golden_deadline() {
    let recs = tb_golden::case("deadline").unwrap();
    let mut st = ShedState::seeded(1);
    let mut cands: Vec<Candidate> = Vec::new();
    let mut budget = None;
    let (mut ticks, mut planned, mut with_shed) = (0, 0, 0);
    for r in &recs {
        match r.kind() {
            "tick" => {
                assert!(r.flag("deadline").unwrap());
                st.steps = r.int("tick").unwrap() as u64;
                st.rng = r.u64_le("rng").unwrap();
                cands.clear();
                budget = None;
            }
            "shed_hist" => {
                let h = r.nums("rate_hist").unwrap();
                st.rate_hist = [h[0], h[1], h[2]];
            }
            "shed_cand" => cands.push(Candidate {
                session: r.int("s").unwrap() as u32,
                priority: r.int("priority").unwrap() as i32,
                class: r.int("class").unwrap() as u8,
                mbs: r.num("mbs").unwrap(),
                first: !r.flag("have_ref").unwrap(),
            }),
            "shed_budget" => budget = Some(r.clone()),
            "shed" => {
                let mut shed = vec![false; cands.len()];
                let left = budget.as_ref().map_or(1.0, |b| b.num("budget").unwrap());
                let (n, got) = st.plan_with_budget(&cands, Some(left), 1.10, None, &mut shed);
                match (&budget, got) {
                    (Some(b), Some(g)) => {
                        assert_eq!(g.rate, b.num("rate").unwrap(), "tick {}", st.steps);
                        assert_eq!(g.allowed, b.num("allowed").unwrap());
                        assert_eq!(g.mbs, b.num("mbs").unwrap());
                        planned += 1;
                    }
                    (None, None) => {}
                    (b, g) => panic!("tick {}: budget {b:?} recorded, {g:?} planned", st.steps),
                }
                let ids: Vec<i64> = cands
                    .iter()
                    .zip(&shed)
                    .filter(|(_, s)| **s)
                    .map(|(c, _)| i64::from(c.session))
                    .collect();
                assert_eq!(ids, r.ints("shed").unwrap(), "tick {}", st.steps);
                assert_eq!(i64::from(n), r.int("shed_count").unwrap());
                ticks += 1;
                with_shed += usize::from(n > 0);
            }
            _ => {}
        }
    }
    assert!(
        ticks > 30 && planned > 10 && with_shed > 10,
        "{ticks} {planned} {with_shed}"
    );
}

fn cand(session: u32, priority: i32, class: u8, mbs: f64, first: bool) -> Candidate {
    Candidate {
        session,
        priority,
        class,
        mbs,
        first,
    }
}

/// A state past the warm-up with a throughput of 1000 macroblocks per second.
fn warm(steps: u64) -> ShedState {
    ShedState {
        rate_hist: [500.0, 1000.0, 0.0],
        steps,
        rng: 0,
    }
}

fn run(st: &mut ShedState, cands: &[Candidate], left_s: Option<f64>) -> (u32, Vec<u32>) {
    let mut shed = vec![true; cands.len()];
    let n = st.plan(cands, left_s, 1.0, None, &mut shed);
    let ids = cands
        .iter()
        .zip(&shed)
        .filter(|(_, s)| **s)
        .map(|(c, _)| c.session)
        .collect();
    (n, ids)
}

#[test]
fn deadline_table() {
    // name, steps, candidates, left_s, sessions shed (in candidate order)
    type Row = (&'static str, u64, Vec<Candidate>, Option<f64>, Vec<u32>);
    let two = || vec![cand(0, 0, 1, 100.0, false), cand(1, 0, 1, 100.0, false)];
    let rows: Vec<Row> = vec![
        // 200 mbs, 0.2 s * 1000 mb/s = 200 allowed: fits (mb <= allowed).
        ("fits exactly", 20, two(), Some(0.2), vec![]),
        ("fits", 20, two(), Some(1.0), vec![]),
        ("no deadline", 20, two(), None, vec![]),
        ("warm-up tick 15", 15, two(), Some(0.0), vec![]),
        ("no candidates", 20, vec![], Some(0.0), vec![]),
        // Priority first: the lower priority gives up, whatever its class.
        (
            "priority",
            20,
            vec![cand(0, 1, 0, 100.0, false), cand(1, 0, 2, 100.0, false)],
            Some(0.15),
            vec![1],
        ),
        // Class next: B before P before key.
        (
            "class B before P",
            20,
            vec![
                cand(0, 0, 2, 100.0, false),
                cand(1, 0, 1, 100.0, false),
                cand(2, 0, 0, 100.0, false),
            ],
            Some(0.25),
            vec![2],
        ),
        (
            "class P before key",
            20,
            vec![
                cand(0, 0, 2, 100.0, false),
                cand(1, 0, 1, 100.0, false),
                cand(2, 0, 0, 100.0, false),
            ],
            Some(0.15),
            vec![1, 2],
        ),
        // Rotation: at tick 20, rot = k + 140; the lowest k goes first.
        (
            "rotation",
            20,
            vec![
                cand(0, 0, 0, 100.0, false),
                cand(1, 0, 0, 100.0, false),
                cand(2, 0, 0, 100.0, false),
            ],
            Some(0.25),
            vec![0],
        ),
        // Rotation wraps: at steps 142857, rot = (k + 999999) % 1000003, so k = 4 gives 0.
        (
            "rotation wraps",
            142_857,
            vec![
                cand(0, 0, 0, 100.0, false),
                cand(1, 0, 0, 100.0, false),
                cand(2, 0, 0, 100.0, false),
                cand(3, 0, 0, 100.0, false),
                cand(4, 0, 0, 100.0, false),
            ],
            Some(0.45),
            vec![4],
        ),
        // A key frame without reference is never given up.
        (
            "first key kept",
            20,
            vec![cand(0, 0, 2, 100.0, true), cand(1, 0, 2, 100.0, false)],
            Some(0.05),
            vec![1],
        ),
        // Time left: everything would go, so the last one shed is coded after all.
        ("one coded while time left", 20, two(), Some(0.01), vec![0]),
        // Past the deadline: everything that may be given up is.
        ("late: all", 20, two(), Some(-0.5), vec![0, 1]),
        ("late at zero", 20, two(), Some(0.0), vec![0, 1]),
        (
            "late: first key kept",
            20,
            vec![
                cand(0, 0, 2, 100.0, true),
                cand(1, 0, 0, 100.0, false),
                cand(2, 0, 2, 100.0, false),
            ],
            Some(-1.0),
            vec![1, 2],
        ),
        // Time left but a protected key frame remains: left > 0, nothing is taken back.
        (
            "time left, protected remains",
            20,
            vec![cand(0, 0, 2, 100.0, true), cand(1, 0, 1, 100.0, false)],
            Some(0.01),
            vec![1],
        ),
    ];
    for (name, steps, cands, left, want) in rows {
        let mut st = warm(steps);
        let (n, got) = run(&mut st, &cands, left);
        assert_eq!(got, want, "{name}");
        assert_eq!(n as usize, want.len(), "{name}");
        assert_eq!(st, warm(steps), "{name}: state unchanged");
    }
}

#[test]
fn deadline_safety_and_best_rate() {
    // The best of the three rates counts: 1000 mb/s; safety 2 halves what fits.
    let cands = [cand(0, 0, 1, 100.0, false), cand(1, 0, 0, 100.0, false)];
    let mut st = warm(16);
    let mut shed = [false; 2];
    assert_eq!(st.plan(&cands, Some(0.2), 1.0, None, &mut shed), 0);
    assert_eq!(st.plan(&cands, Some(0.2), 2.0, None, &mut shed), 1);
    assert_eq!(shed, [false, true]);
}

#[test]
fn empty_history_never_sheds() {
    let cands = [cand(0, 0, 1, 100.0, false)];
    let mut st = ShedState {
        steps: 100,
        ..ShedState::default()
    };
    let mut shed = [true];
    assert_eq!(st.plan(&cands, Some(-1.0), 1.0, None, &mut shed), 0);
    assert_eq!(shed, [false]);
}

#[test]
fn hook_ignores_deadline_and_warmup() {
    // p = 1: every candidate but a first key frame is given up, at tick 0, without deadline.
    let cands = [
        cand(0, 0, 2, 99.0, true),
        cand(1, 0, 2, 99.0, false),
        cand(2, 0, 0, 99.0, false),
    ];
    let mut st = ShedState::seeded(1);
    let rng0 = st.rng;
    let mut shed = [false; 3];
    assert_eq!(st.plan(&cands, None, 1.1, Some(1.0), &mut shed), 2);
    assert_eq!(shed, [false, true, true]);
    // One draw per candidate, the protected one included.
    let mut r = rng0;
    for _ in 0..3 {
        r = r
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
    }
    assert_eq!(st.rng, r);
    // p <= 0 is no hook (C++: force_shed > 0).
    let mut st2 = warm(20);
    assert_eq!(st2.plan(&cands, None, 1.1, Some(0.0), &mut shed), 0);
    assert_eq!(st2.rng, 0);
}

#[test]
fn observe_shifts_history() {
    let mut st = ShedState::default();
    st.observe(100.0, 0.5);
    assert_eq!(st.rate_hist, [200.0, 0.0, 0.0]);
    st.observe(300.0, 1.0);
    assert_eq!(st.rate_hist, [300.0, 200.0, 0.0]);
    st.observe(0.0, 1.0); // coded nothing: unchanged
    assert_eq!(st.rate_hist, [300.0, 200.0, 0.0]);
    st.observe(50.0, 0.25);
    assert_eq!(st.rate_hist, [200.0, 300.0, 200.0]);
    st.observe(1.0, 0.0); // at least a microsecond
    assert_eq!(st.rate_hist, [1.0 / 1e-6, 200.0, 300.0]);
    st.observe(1.0, -3.0);
    assert_eq!(st.rate_hist[0], 1.0 / 1e-6);
    assert_eq!(st.steps, 0);
}
