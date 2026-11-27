//! `gop::GopState` against the golden `submit`, `emit`, `flush` and `force_key` records.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::{BTreeMap, VecDeque};

use tb_golden::Record;
use tb_m2v_encoder::gop::{Frame, GopState};
use tb_m2v_syntax::PicType;

/// Per session: the GOP state, its parameters, frames produced but not yet matched.
#[derive(Default)]
struct Sess {
    gop: GopState,
    params: Option<(u32, u32, u32)>,
    produced: VecDeque<Frame>,
    force_key: bool,
    per_frame: u8,
    top_first: bool,
}

/// What a replay saw, for checks on the coverage of a case.
#[derive(Default, Debug)]
struct Seen {
    submits: usize,
    emits: usize,
    flushes: usize,
    force_keys: usize,
    off_grid: usize,
    rekeys: usize,
    released: usize,
    phases: Vec<u32>,
}

fn u32_of(r: &Record, f: &str) -> u32 {
    u32::try_from(r.int(f).unwrap()).unwrap()
}

fn u64_of(r: &Record, f: &str) -> u64 {
    u64::try_from(r.int(f).unwrap()).unwrap()
}

fn pic_type(code: i64) -> PicType {
    match code {
        1 => PicType::I,
        2 => PicType::P,
        3 => PicType::B,
        _ => panic!("picture type {code}"),
    }
}

fn replay(name: &str) -> Seen {
    let recs = tb_golden::case(name).unwrap();
    let mut sessions: BTreeMap<i64, Sess> = BTreeMap::new();
    let mut seen = Seen::default();
    // The session whose submit / flush the following emit records belong to.
    let mut producer: Option<i64> = None;
    for (i, r) in recs.iter().enumerate() {
        let at = format!("{name}, record {i} ({})", r.kind());
        match r.kind() {
            "open" if r.text("error").unwrap().is_empty() => {
                let s = sessions.entry(r.int("s").unwrap()).or_default();
                s.params = Some((u32_of(r, "gop"), u32_of(r, "m"), u32_of(r, "gop_phase")));
                // display units per frame period, and the session's field order
                let interlaced = r.flag("interlaced").unwrap();
                s.per_frame = if interlaced { 2 } else { 1 };
                s.top_first = interlaced && r.flag("tff").unwrap();
                seen.phases.push(u32_of(r, "gop_phase"));
            }
            "force_key" => {
                seen.force_keys += 1;
                sessions.get_mut(&r.int("s").unwrap()).unwrap().force_key = true;
            }
            "submit" => {
                seen.submits += 1;
                let id = r.int("s").unwrap();
                let s = sessions.get_mut(&id).unwrap();
                assert!(s.produced.is_empty(), "{at}: unmatched {:?}", s.produced);
                let params = (u32_of(r, "gop"), u32_of(r, "m"), u32_of(r, "gop_phase"));
                assert_eq!(Some(params), s.params, "{at}: parameters");
                // The state the recorded run had on entry.
                assert_eq!(s.gop.n_in, u64_of(r, "disp"), "{at}: disp");
                // (the record has the grid time in frame periods)
                assert_eq!(
                    s.gop.next_key / u64::from(s.per_frame),
                    u64_of(r, "next_key"),
                    "{at}: next_key"
                );
                assert_eq!(s.gop.gop_first, u64_of(r, "gop_first"), "{at}: gop_first");
                assert_eq!(
                    s.gop.pending.len() as u64,
                    u64_of(r, "pending"),
                    "{at}: pending"
                );
                // Set from outside: want_key by force_keyframe, rekey by the tick, the hold flags
                // by the caller.
                let want_key = r.flag("want_key").unwrap();
                assert_eq!(want_key, s.force_key, "{at}: want_key without force_key");
                s.force_key = false;
                s.gop.want_key = want_key;
                s.gop.rekey = r.flag("rekey").unwrap();
                seen.rekeys += usize::from(s.gop.rekey);
                let released = r.flag("hold_prev").unwrap() && !r.flag("hold").unwrap();
                seen.released += usize::from(released);
                let frame = Frame {
                    disp: u64::MAX,
                    slot: u8::try_from(r.int("slot").unwrap()).unwrap(),
                    pts: r.int("pts").unwrap(),
                    avail_step: u64_of(r, "avail_step"),
                    pic_type: PicType::B,
                    gop_start: false,
                    progressive: r.flag("prog").unwrap(),
                    off_grid: false,
                    start: 0,
                    // a frame shown for other than one frame period says so
                    dur: r.int("units").map_or(s.per_frame, |u| u as u8),
                    tff: r.flag("top_first").unwrap_or(s.top_first),
                    cc: r.bytes("cc").unwrap(),
                    afd: None,
                };
                s.gop
                    .enqueue(frame, params.0, params.1, params.2, released, s.per_frame);
                s.produced.extend(s.gop.coded.drain(..));
                producer = Some(id);
            }
            "flush" => {
                seen.flushes += 1;
                let id = r.int("s").unwrap();
                let s = sessions.get_mut(&id).unwrap();
                assert!(s.produced.is_empty(), "{at}: unmatched {:?}", s.produced);
                assert!(!s.gop.flushed, "{at}: flushed twice");
                assert_eq!(
                    s.gop.pending.len() as u64,
                    u64_of(r, "pending"),
                    "{at}: pending"
                );
                s.gop.flush();
                assert!(s.gop.flushed);
                assert!(s.gop.pending.is_empty(), "{at}");
                s.produced.extend(s.gop.coded.drain(..));
                producer = Some(id);
            }
            "emit" => {
                seen.emits += 1;
                let id = r.int("s").unwrap();
                assert_eq!(producer, Some(id), "{at}: emit of another session");
                let s = sessions.get_mut(&id).unwrap();
                let (per_frame, top_first) = (s.per_frame, s.top_first);
                let f = s
                    .produced
                    .pop_front()
                    .unwrap_or_else(|| panic!("{at}: the port emitted nothing more"));
                let want = Frame {
                    disp: u64_of(r, "disp"),
                    slot: u8::try_from(r.int("slot").unwrap()).unwrap(),
                    pts: r.int("pts").unwrap(),
                    avail_step: u64_of(r, "avail_step"),
                    pic_type: pic_type(r.int("type").unwrap()),
                    gop_start: r.flag("gop_start").unwrap(),
                    progressive: r.flag("prog").unwrap(),
                    off_grid: r.flag("off_grid").unwrap(),
                    start: r
                        .int("start")
                        .map_or(u64_of(r, "disp") * u64::from(per_frame), |x| x as u64),
                    dur: r.int("units").map_or(per_frame, |u| u as u8),
                    tff: r.flag("top_first").unwrap_or(top_first),
                    cc: r.bytes("cc").unwrap(),
                    afd: None,
                };
                seen.off_grid += usize::from(want.off_grid);
                assert_eq!(f, want, "{at}");
            }
            _ => producer = None,
        }
    }
    for (id, s) in &sessions {
        assert!(s.produced.is_empty(), "{name}, s {id}: unmatched at end");
        assert!(!s.force_key, "{name}, s {id}: force_key never consumed");
    }
    seen
}

#[test]
fn cbr() {
    let seen = replay("cbr");
    assert!(seen.submits > 0 && seen.emits == seen.submits, "{seen:?}");
}

#[test]
fn ladder() {
    let seen = replay("ladder");
    assert!(seen.submits > 0 && seen.emits == seen.submits, "{seen:?}");
}

#[test]
fn fixedq_has_a_forced_key_frame() {
    let seen = replay("fixedq");
    assert!(seen.force_keys >= 1, "{seen:?}");
    assert_eq!(seen.emits, seen.submits);
}

#[test]
fn interlaced() {
    let seen = replay("interlaced");
    assert!(seen.submits > 0 && seen.emits == seen.submits, "{seen:?}");
}

#[test]
fn shed_has_missed_off_grid_and_staggered_key_frames() {
    let seen = replay("shed");
    assert_eq!(seen.emits, seen.submits);
    assert!(seen.rekeys > 0, "{seen:?}");
    assert!(seen.released > 0, "{seen:?}");
    assert!(seen.off_grid > 0, "{seen:?}");
    assert_eq!(seen.phases.len(), 2);
    assert_ne!(seen.phases[1], 0, "{seen:?}");
}

/// The cases recorded by the Rust encoder (crates/tb-golden/vectors/generate.sh): every submit emits its frames.
#[test]
fn cases_of_the_rust_recorder() {
    for name in [
        "skip", "deadline", "churn", "devin", "twofmt", "film", "film60",
    ] {
        let seen = replay(name);
        assert!(
            seen.submits > 10 && seen.emits == seen.submits,
            "{name}: {seen:?}"
        );
    }
}
