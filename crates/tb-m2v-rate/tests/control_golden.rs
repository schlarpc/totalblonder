//! Rate control against the golden records (work package R2): every session of the cases,
//! replayed through a `RateControl` driven by its `begin`, `rowstat`, `rc_plan`, `pass` and
//! `au` records in file order, every decision compared exactly. `choose` records are replayed
//! on a copy of the state with the model the record names.
#![allow(
    clippy::unwrap_used,
    reason = "test code: the replay helpers are not #[test] functions"
)]

use tb_golden::{GoldenError, Record, case};
use tb_kernel_abi::{PicStat, RowStat, SessPar};
use tb_m2v_rate::control::{Pass, PicCtx, min_rate};
use tb_m2v_rate::{RateControl, RateModel};

/// How many records of each kind a replay checked.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    open: usize,
    begin: usize,
    choose: usize,
    plan: usize,
    pass: usize,
    au: usize,
    repeat: usize,
}

fn u8s(v: Vec<i64>) -> Vec<u8> {
    v.into_iter().map(|x| u8::try_from(x).unwrap()).collect()
}

fn u32s(v: Vec<i64>) -> Vec<u32> {
    v.into_iter().map(|x| u32::try_from(x).unwrap()).collect()
}

fn ty(r: &Record) -> u8 {
    u8::try_from(r.int("type").unwrap()).unwrap()
}

fn replay(name: &str) -> Result<Counts, GoldenError> {
    let recs = case(name)?;
    let mut n = Counts::default();
    for open in recs.iter().filter(|r| r.kind() == "open") {
        let s = open.int("s")?;
        let u = |f: &str| open.int(f).map(|v| u64::try_from(v).unwrap());
        let nmb = u32::try_from(open.int("nmb")?).unwrap();
        let mbh = u32::try_from(open.int("mbh")?).unwrap();
        let fps = open.num("fps")?;
        assert_eq!(min_rate(mbh, fps), u("min_rate")?, "{name} s{s} min_rate");
        let made = RateControl::new(
            u("bitrate")?,
            u("max_bitrate")?,
            u("vbv_bits")?,
            nmb,
            mbh,
            fps,
        );
        if !open.text("error")?.is_empty() {
            assert!(made.is_err(), "{name} s{s}: opened, recorded run refused");
            continue;
        }
        let mut rc = made.unwrap();
        n.open += 1;
        assert_eq!(rc.x.to_vec(), open.nums("X")?, "{name} s{s} X");
        assert_eq!(rc.vbv.d, open.num("D")?, "{name} s{s} D");
        let fixed_q = [
            u8::try_from(open.int("qi")?).unwrap(),
            u8::try_from(open.int("qp")?).unwrap(),
            u8::try_from(open.int("qb")?).unwrap(),
        ];

        let mut rows: Vec<RowStat> = Vec::new();
        let mut rowq = vec![0u8; mbh as usize];
        let mut have_ref = false;
        let mut ctx: Option<PicCtx> = None;
        let mut last_pass: Option<Pass> = None;
        for (i, r) in recs
            .iter()
            .filter(|r| r.int("s").ok() == Some(s))
            .enumerate()
        {
            let at = format!("{name} s{s} #{i} {}", r.kind());
            match r.kind() {
                "begin" => {
                    assert_eq!(rc.cbr, r.flag("cbr")?, "{at}");
                    assert_eq!(u8s(r.ints("floor_hist")?), rc.floor_hist, "{at}");
                    assert_eq!(rc.avg_act, r.num("avg_act")?, "{at}");
                    assert_eq!(rc.last_qs, r.num("last_qs")?, "{at}");
                    let new_rate = (r.int("ticket")? != 0).then(|| r.num("rate")).transpose()?;
                    let fq = u8::try_from(r.int("fixed_q")?).unwrap();
                    let par = rc.begin_picture(r.num("t")?, new_rate, ty(r), fq);
                    assert_eq!(rc.cur.e_before, r.num("e_before")?, "{at}");
                    assert_eq!(rc.cur.cap, r.num("cap")?, "{at}");
                    let p: SessPar = r.pods("par")?[0];
                    assert_eq!(par.avg_act, p.avg_act, "{at}");
                    assert_eq!(par.lam, p.lam, "{at}");
                    assert_eq!(par.qfloor, p.qfloor, "{at}");
                    assert_eq!(p.keep, 64, "{at}");
                    if rc.cbr {
                        assert_eq!(rc.vbv.rate(), r.num("rate")?, "{at}");
                    }
                    n.begin += 1;
                }
                "rowstat" => {
                    rows = r.pods("rows")?;
                    assert_eq!(rows.len(), mbh as usize, "{at}");
                }
                "choose" => {
                    let t = ty(r);
                    let mut c = rc.clone();
                    c.mdl[usize::from(t)] = RateModel {
                        theta: r.num("theta")?,
                        hdr: r.num("hdr")?,
                    };
                    let mut q = rowq.clone();
                    let ok = c.choose(t, r.num("tgt")?, &rows, &mut q);
                    assert_eq!(ok, r.flag("ok")?, "{at}");
                    assert_eq!(i64::from(c.cur.code), r.int("code")?, "{at}");
                    assert_eq!(c.cur.frac, r.num("frac")?, "{at}");
                    assert_eq!(c.cur.nzpred, r.num("nzpred")?, "{at}");
                    assert_eq!(q, r.bytes("rowq")?, "{at}");
                    n.choose += 1;
                }
                "rc_plan" => {
                    let c = PicCtx {
                        pic_type: ty(r),
                        key: r.flag("key")?,
                        have_ref,
                        nmb,
                        fps: r.num("fps")?,
                        rate: r.num("rate")?,
                        gop: u32::try_from(r.int("gop")?).unwrap(),
                        m: u32::try_from(r.int("m")?).unwrap(),
                        to_next_key: u32::try_from(r.int("to_next_key")?).unwrap(),
                        // records of frames shown for other than one frame period say so
                        gop_frames: r
                            .num("gop_frames")
                            .unwrap_or(r.int("to_next_key")?.max(1) as f64),
                        frames: r.num("frames").unwrap_or(1.0),
                        hdr_bits: r.num("hdr_bits")?,
                        cap_bytes: u32::try_from(r.int("cap_bytes")?).unwrap(),
                        fixed_q,
                    };
                    let keep = rc.plan_picture(&c, &rows, &mut rowq);
                    ctx = Some(c);
                    assert_eq!(rc.cbr, r.flag("cbr")?, "{at}");
                    assert_eq!(rc.cur.hdr_bits, r.num("hdr_bits")?, "{at}");
                    assert_eq!(rc.cur.target, r.num("target")?, "{at}");
                    assert_eq!(rc.cur.hard, r.num("hard")?, "{at}");
                    assert_eq!(rc.cur.limit, r.num("limit")?, "{at}");
                    assert_eq!(i64::from(rc.cur.code), r.int("code")?, "{at}");
                    assert_eq!(rc.cur.frac, r.num("frac")?, "{at}");
                    assert_eq!(rc.cur.nzpred, r.num("nzpred")?, "{at}");
                    assert_eq!(i64::from(rc.cur.rung), r.int("rung")?, "{at}");
                    assert_eq!(i64::from(keep), r.int("keep")?, "{at}");
                    assert_eq!(keep, rc.keep(), "{at}");
                    assert_eq!(rc.r, r.num("R")?, "{at}");
                    assert_eq!(rc.x.to_vec(), r.nums("X")?, "{at}");
                    assert_eq!(rc.left.to_vec(), u32s(r.ints("left")?), "{at}");
                    assert_eq!(rc.avg_act, r.num("avg_act")?, "{at}");
                    assert_eq!(u8s(r.ints("last_rung")?), rc.last_rung, "{at}");
                    assert_eq!(rc.last_qs, r.num("last_qs")?, "{at}");
                    assert_eq!(rc.cur.e_before, r.num("e_before")?, "{at}");
                    assert_eq!(rc.cur.cap, r.num("cap")?, "{at}");
                    assert_eq!(rowq, r.bytes("rowq")?, "{at}");
                    n.plan += 1;
                }
                "pass" => {
                    let c = ctx.as_mut().unwrap();
                    let t = usize::from(c.pic_type);
                    assert_eq!(c.pic_type, ty(r), "{at}");
                    assert_eq!(c.have_ref, r.flag("have_ref")?, "{at}");
                    assert_eq!(i64::from(rc.cur.tries), r.int("tries")?, "{at}");
                    assert_eq!(i64::from(rc.cur.rung), r.int("rung")?, "{at}");
                    assert_eq!(i64::from(rc.cur.code), r.int("code")?, "{at}");
                    assert_eq!(rc.cur.frac, r.num("frac")?, "{at}");
                    assert_eq!(rc.cur.nzpred, r.num("nzpred")?, "{at}");
                    assert_eq!(rc.mdl[t].theta, r.num("theta")?, "{at}");
                    assert_eq!(rc.mdl[t].hdr, r.num("hdr")?, "{at}");
                    assert_eq!(rc.cur.target - rc.cur.hdr_bits, r.num("tgt")?, "{at}");
                    assert_eq!(rc.cur.limit - rc.cur.hdr_bits, r.num("limit")?, "{at}");
                    let bytes = u32::try_from(r.int("bytes")?).unwrap();
                    assert_eq!(
                        u64::from(bytes) + 64 > u64::from(c.cap_bytes),
                        r.flag("over_cap")?,
                        "{at}"
                    );
                    let mut pic = PicStat {
                        bytes,
                        tex_bits: u32::try_from(r.int("tex_bits")?).unwrap(),
                        ..PicStat::default()
                    };
                    for (w, v) in pic.big_rows.iter_mut().zip(r.ints("big_rows")?) {
                        *w = u32::try_from(v).unwrap();
                    }
                    last_pass = Some(rc.after_pass(c, &rows, pic, &mut rowq));
                    n.pass += 1;
                }
                "pass_out" => {
                    let c = ctx.as_ref().unwrap();
                    let t = usize::from(c.pic_type);
                    let out = last_pass.take().unwrap();
                    match (r.text("action")?, out) {
                        ("accept", Pass::Accept) => {}
                        (
                            "again",
                            Pass::Again {
                                keep,
                                all_skipped,
                                i_becomes_p,
                            },
                        ) => {
                            assert_eq!(i64::from(keep), r.int("keep")?, "{at}");
                            if all_skipped {
                                assert!(r.flag("want_pred")?, "{at}");
                                assert_eq!(keep, 0, "{at}");
                            }
                            // An I picture replaced by P (case `skip`): the record has the
                            // new type and no key frame, checked below.
                            if i_becomes_p {
                                assert_eq!(c.pic_type, 2, "{at}");
                            }
                        }
                        (a, o) => panic!("{at}: recorded {a}, port {o:?}"),
                    }
                    assert_eq!(c.pic_type, ty(r), "{at}");
                    assert_eq!(c.key, r.flag("key")?, "{at}");
                    assert_eq!(i64::from(rc.cur.rung), r.int("rung")?, "{at}");
                    assert_eq!(i64::from(rc.keep()), r.int("keep")?, "{at}");
                    assert_eq!(i64::from(rc.cur.code), r.int("code")?, "{at}");
                    assert_eq!(rc.cur.frac, r.num("frac")?, "{at}");
                    assert_eq!(rc.cur.nzpred, r.num("nzpred")?, "{at}");
                    assert_eq!(i64::from(rc.cur.tries), r.int("tries")?, "{at}");
                    assert_eq!(rc.stats.violations, r.int("violations")? as u64, "{at}");
                    assert_eq!(rc.stats.isub, r.int("isub")? as u64, "{at}");
                    assert_eq!(rc.mdl[t].theta, r.num("theta")?, "{at}");
                    assert_eq!(rc.mdl[t].hdr, r.num("hdr")?, "{at}");
                    assert_eq!(rowq, r.bytes("rowq")?, "{at}");
                }
                "au" => {
                    let bits = 8.0 * r.num("len")?;
                    assert_eq!(bits, r.num("bits")?, "{at}");
                    let res = if r.flag("repeat")? {
                        n.repeat += 1;
                        // the decoding time: the record of a frame shown for other than one
                        // frame period has it
                        let t = r.num("t").unwrap_or(r.num("coded")? / fps);
                        let pt = u8::try_from(r.int("position_type")?).unwrap();
                        rc.repeat_picture(t, bits, pt)
                    } else {
                        let c = ctx.take().unwrap();
                        assert_eq!(c.pic_type, ty(r), "{at}");
                        assert_eq!(rowq, r.bytes("rowq")?, "{at}");
                        let pic = PicStat {
                            bytes: u32::try_from(r.int("slice_bytes")?).unwrap(),
                            tex_bits: u32::try_from(r.int("tex_bits")?).unwrap(),
                            ..PicStat::default()
                        };
                        rc.finish_picture(&c, pic, bits, &rowq)
                    };
                    assert_eq!(res.avg_q, r.num("avg_q")?, "{at}");
                    assert_eq!(res.queue_bits, r.num("queue")?, "{at}");
                    assert_eq!(res.room_bits, r.num("room")?, "{at}");
                    assert_eq!(res.cap_bits, r.num("cap")?, "{at}");
                    assert_eq!(i64::from(res.rung), r.int("rung")?, "{at}");
                    assert_eq!(i64::from(res.tries), r.int("tries")?, "{at}");
                    let theta: Vec<f64> = rc.mdl.iter().map(|m| m.theta).collect();
                    let hdr: Vec<f64> = rc.mdl.iter().map(|m| m.hdr).collect();
                    assert_eq!(theta, r.nums("theta")?, "{at}");
                    assert_eq!(hdr, r.nums("mdl_hdr")?, "{at}");
                    assert_eq!(rc.x.to_vec(), r.nums("X")?, "{at}");
                    assert_eq!(rc.r, r.num("R")?, "{at}");
                    assert_eq!(rc.left.to_vec(), u32s(r.ints("left")?), "{at}");
                    assert_eq!(u8s(r.ints("floor_hist")?), rc.floor_hist, "{at}");
                    assert_eq!(u8s(r.ints("last_rung")?), rc.last_rung, "{at}");
                    assert_eq!(rc.last_qs, r.num("last_qs")?, "{at}");
                    assert_eq!(rc.avg_act, r.num("avg_act")?, "{at}");
                    assert_eq!(rc.stats.violations, r.int("violations")? as u64, "{at}");
                    have_ref = r.flag("have_ref")?;
                    n.au += 1;
                }
                _ => {}
            }
        }
        assert!(ctx.is_none(), "{name} s{s}: a picture without its au");
    }
    Ok(n)
}

#[test]
fn cbr() {
    let n = replay("cbr").unwrap();
    assert_eq!(
        n,
        Counts {
            open: 1,
            begin: 30,
            choose: 38,
            plan: 30,
            pass: 33,
            au: 30,
            repeat: 0
        }
    );
}

#[test]
fn ladder() {
    let n = replay("ladder").unwrap();
    assert_eq!(
        n,
        Counts {
            open: 1,
            begin: 24,
            choose: 33,
            plan: 24,
            pass: 42,
            au: 24,
            repeat: 0
        }
    );
}

#[test]
fn fixedq() {
    let n = replay("fixedq").unwrap();
    assert_eq!(
        n,
        Counts {
            open: 1,
            begin: 12,
            choose: 0,
            plan: 12,
            pass: 12,
            au: 12,
            repeat: 0
        }
    );
}

#[test]
fn interlaced() {
    let n = replay("interlaced").unwrap();
    assert_eq!(
        n,
        Counts {
            open: 1,
            begin: 24,
            choose: 28,
            plan: 24,
            pass: 27,
            au: 24,
            repeat: 0
        }
    );
}

#[test]
fn shed() {
    let n = replay("shed").unwrap();
    assert_eq!(
        n,
        Counts {
            open: 2,
            begin: 34,
            choose: 40,
            plan: 34,
            pass: 39,
            au: 60,
            repeat: 26
        }
    );
}

/// The cases recorded by the Rust encoder (golden/generate.sh). `skip` reaches the all-skipped
/// rung and replaces I pictures by P pictures.
#[test]
fn cases_of_the_rust_recorder() {
    for name in [
        "skip", "deadline", "churn", "devin", "twofmt", "film", "film60",
    ] {
        let n = replay(name).unwrap();
        assert!(n.begin > 10 && n.au > 10, "{name}: {n:?}");
    }
    let recs = case("skip").unwrap();
    let au = |f: &dyn Fn(&Record) -> bool| recs.iter().filter(|r| r.kind() == "au" && f(r)).count();
    assert!(au(&|r| r.int("rung").unwrap() == 37 && !r.flag("repeat").unwrap()) > 5);
    assert!(
        au(&|r| r.int("position_type").unwrap() == 1
            && r.int("type").unwrap() == 2
            && !r.flag("repeat").unwrap())
            > 0
    );
}
