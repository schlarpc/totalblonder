//! The buffer model against the golden `vbv` records (work package R1): every session of the
//! cases that run rate control, replayed on a fresh model, compared exactly.

use tb_golden::{GoldenError, Record, case};
use tb_m2v_rate::vbv::{RateChange, Vbv};

/// The rate history as the records print it: `[t0, r0, t1, r1, ...]`.
fn history(v: &Vbv) -> Vec<f64> {
    v.changes.iter().flat_map(|c| [c.t, c.rate]).collect()
}

fn replay(name: &str) -> Result<usize, GoldenError> {
    let recs = case(name)?;
    let opens: Vec<&Record> = recs.iter().filter(|r| r.kind() == "open").collect();
    let mut checked = 0;
    for open in opens {
        let s = open.int("s")?;
        if !open.text("error")?.is_empty() {
            continue;
        }
        let d = open.num("D")?;
        let mut v = Vbv::new(d, open.num("bitrate")?);
        assert_eq!(
            v.changes,
            [RateChange {
                t: -1e18,
                rate: v.rate()
            }]
        );
        for (i, r) in recs
            .iter()
            .filter(|r| r.kind() == "vbv" && r.int("s").ok() == Some(s))
            .enumerate()
        {
            let at = format!("{name} s{s} vbv #{i}: {r:?}");
            match r.text("op")? {
                "advance" => {
                    v.advance(r.num("to")?);
                    assert_eq!(v.cap(), r.num("cap")?, "{at}");
                }
                "set_rate" => v.set_rate(r.num("at")?, r.num("rate")?),
                "add" => {
                    v.add(r.num("bits")?);
                    assert_eq!(v.room(), r.num("room")?, "{at}");
                }
                op => panic!("{at}: unknown op {op}"),
            }
            assert_eq!(v.d, r.num("D")?, "{at}");
            assert_eq!(v.queue, r.num("queue")?, "{at}");
            assert_eq!(v.t, r.num("t")?, "{at}");
            assert_eq!(history(&v), r.nums("chg")?, "{at}");
            checked += 1;
        }
    }
    Ok(checked)
}

/// The reader must parse the recorded doubles to the same IEEE value (serde_json needs its
/// `float_roundtrip` feature for that; see Cargo.toml).
#[test]
fn reader_round_trips_doubles() {
    let recs = tb_golden::parse("t", r#"{"k":"x","t":0.10010000000000001}"#).unwrap();
    assert_eq!(
        recs[0].num("t").unwrap(),
        "0.10010000000000001".parse::<f64>().unwrap()
    );
}

#[test]
fn cbr() {
    assert_eq!(replay("cbr").unwrap(), 62);
}

#[test]
fn ladder() {
    assert_eq!(replay("ladder").unwrap(), 48);
}

#[test]
fn interlaced() {
    assert_eq!(replay("interlaced").unwrap(), 49);
}

#[test]
fn shed() {
    assert_eq!(replay("shed").unwrap(), 120);
}

/// The cases recorded by the Rust encoder (crates/tb-golden/vectors/generate.sh): all-skipped pictures and I
/// pictures replaced, a real deadline, sessions at run time, device-written frames, two formats.
#[test]
fn cases_of_the_rust_recorder() {
    for name in [
        "skip", "deadline", "churn", "devin", "twofmt", "film", "film60",
    ] {
        assert!(replay(name).unwrap() > 20, "{name}");
    }
}
