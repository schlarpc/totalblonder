//! Work package E4: the encoder against the golden records. Every golden case is driven with its
//! recorded inputs (`open`, `submit`, `set_rate`, `force_key`, `flush`, `tick`, and the hold of
//! the `submit` records) against a `Replay` device built from the case.
//!
//! What the replay proves, per plan (see `tb_device::sim::golden`):
//! - operations: the plan's normal form equals the recorded `op` records up to the same host
//!   wait, in queue order (`Compare::NormalForm`), and it waits on the same stream;
//! - bytes up, exactly: `SessPar` blocks (`begin.par`, `qent.par`), the lists, row quantizers
//!   (`rc_plan.rowq`, `pass_out.rowq`), the retry and re-transform lists, `finish.outpos`;
//! - the host-arena rule.
//!
//! What the test compares itself: every field and every byte of every access unit, the
//! session's state after it, the deadline hook's decisions, and the SHA-256 of each stream.
//!
//! The set-up of a batch (clearing its block, uploading the tables) is in the records as the
//! first two operations of the first plan that waits on the kernel stream (`ablk_d`, `d_T`,
//! `e->T`); the table bytes come from `golden/tables.jsonl`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use std::collections::BTreeMap;

use tb_device::sim::golden::{GoldenTrace, NameTable, golden_trace};
use tb_device::sim::{Compare, Phase as SimPhase, Replay, Scenario, Select};
use tb_device::{ArenaSizes, DevBuf, Device, DeviceError, HostBuf, Nanos, Op, Plan, StreamId};
use tb_golden::Record;
use tb_m2v_encoder::layout::{BatchLayout, List};
use tb_m2v_encoder::{AccessUnit, Encoder, EncoderOptions, FrameSource, Phase, SessionId};
use tb_m2v_syntax::headers::SEQUENCE_END;

const ARENAS: ArenaSizes = ArenaSizes {
    device: 64 << 20,
    host: 2 << 20,
};

/// The options of the recorded run, from its command line.
fn options(recs: &[Record]) -> EncoderOptions {
    let args = recs[0].text("args").unwrap();
    let words: Vec<&str> = args.split(' ').collect();
    let after = |flag: &str| {
        words
            .iter()
            .position(|w| *w == flag)
            .map(|i| words[i + 1].to_owned())
    };
    let force_shed = after("--force-shed").map(|v| {
        let (p, seed) = v.split_once(':').unwrap();
        (p.parse::<f64>().unwrap(), seed.parse::<u64>().unwrap())
    });
    EncoderOptions {
        overlap: words.contains(&"--overlap"),
        // The driver seeds the hook's generator with 1 when the option is absent, as the C++
        // driver did; probability 0 leaves the hook off.
        force_shed: force_shed.or(Some((0.0, 1))),
        ..EncoderOptions::default()
    }
}

/// The recorded buffer names at the places of the port's layout.
fn names(lay: &BatchLayout) -> NameTable {
    let mut t = NameTable::new(ARENAS);
    let lists = [
        ("L_UP", List::Up),
        ("L_CODE", List::Code),
        ("L_ME", List::Me),
        ("L_INTRA", List::Intra),
        ("L_RETRY", List::Retry),
        ("L_REC", List::Rec),
        ("L_XF", List::Xf),
        ("L_DEV", List::Dev),
    ];
    for (n, l) in lists {
        t.host
            .insert(format!("h_list[{n}]"), lay.h_list[l as usize]);
        t.dev.insert(format!("d_list[{n}]"), lay.d_list[l as usize]);
    }
    let host: [(&str, HostBuf); 10] = [
        ("h_par", lay.h_par),
        ("h_rowq", lay.h_rowq),
        ("h_upslot", lay.h_upslot),
        ("h_rowstat", lay.h_rowstat),
        ("h_pic", lay.h_pic),
        ("h_out", lay.h_out),
        ("h_outpos", lay.h_outpos),
        // the frames: their bytes are not in the records, any region of the right size does
        ("sy", lay.h_frames),
        ("su_", lay.h_frames),
        ("sv", lay.h_frames),
    ];
    for (n, b) in host {
        t.host.insert(n.to_owned(), b);
    }
    let dev: [(&str, DevBuf); 9] = [
        ("d_par", lay.d_par),
        ("d_rowq", lay.d_rowq),
        ("d_upslot", lay.d_upslot),
        ("d_rowstat", lay.d_rowstat),
        ("d_pic", lay.d_pic),
        ("d_out", lay.d_out),
        ("d_outpos", lay.d_outpos),
        ("dy", lay.bufs.ring_y),
        ("du", lay.bufs.ring_u),
    ];
    for (n, b) in dev {
        t.dev.insert(n.to_owned(), b);
    }
    t.dev.insert("dv".to_owned(), lay.bufs.ring_v);
    t.dev.insert("d_T".to_owned(), lay.d_tables);
    t
}

/// A case set up for a run: the encoder with its sessions open, the recorded trace with the
/// set-up steps of the arena path in place.
struct Case {
    name: &'static str,
    recs: Vec<Record>,
    enc: Encoder,
    sids: Vec<SessionId>,
    gt: GoldenTrace,
}

fn load(name: &'static str) -> Case {
    let recs = tb_golden::case(name).unwrap();
    let opt = options(&recs);
    let mut enc = Encoder::new(opt, ARENAS);
    let mut sids = Vec::new();
    for o in recs.iter().filter(|r| r.kind() == "open") {
        assert!(o.text("error").unwrap().is_empty());
        let id = enc.open(&common::config(o)).unwrap();
        assert_eq!(i64::from(id.0), o.int("s").unwrap());
        let (_, slot) = enc.batch_layout(id).unwrap();
        assert_eq!(slot as i64, o.int("slot").unwrap(), "{name}: slot");
        sids.push(id);
    }
    assert_eq!(enc.batch_count(), 1);
    let (lay, _) = enc.batch_layout(sids[0]).unwrap();
    let mut table = names(lay);
    let h_tables = enc.tables_buffer().unwrap();
    table
        .dev
        .insert("ablk_d".to_owned(), enc.batch_block(sids[0]).unwrap());
    table.host.insert("e->T".to_owned(), h_tables);
    let mut gt = golden_trace(&recs, &table).unwrap();
    // The set-up of the batch is the first plan that waits on the kernel stream; its upload
    // carries the tables.
    let tables = tb_golden::case("tables").unwrap()[1]
        .bytes("bytes")
        .unwrap();
    assert_eq!(h_tables.len as usize, tables.len());
    let first = gt.trace.records[..2]
        .iter()
        .position(|r| r.plan.wait == StreamId::MAIN)
        .unwrap();
    assert_eq!(gt.tags[first].tick, -1);
    let rec = &mut gt.trace.records[first];
    assert!(matches!(
        rec.plan.steps.as_slice(),
        [
            tb_device::Step {
                op: Op::Fill { .. },
                ..
            },
            tb_device::Step {
                op: Op::Upload { .. },
                ..
            }
        ]
    ));
    rec.uploads.push((h_tables, tables));
    Case {
        name,
        recs,
        enc,
        sids,
        gt,
    }
}

fn check_au(at: &str, r: &Record, au: &AccessUnit) {
    let int = |f: &str| r.int(f).unwrap();
    let num = |f: &str| r.num(f).unwrap();
    let flag = |f: &str| r.flag(f).unwrap();
    assert_eq!(au.coded as i64, int("coded"), "{at}: coded");
    assert_eq!(au.disp as i64, int("disp"), "{at}: disp");
    assert_eq!(i64::from(au.pic_type), int("type"), "{at}: type");
    assert_eq!(au.keyframe, flag("key"), "{at}: key");
    assert_eq!(au.off_grid, flag("off_grid"), "{at}: off_grid");
    assert_eq!(au.repeat, flag("repeat"), "{at}: repeat");
    assert_eq!(au.key_missed, flag("key_missed"), "{at}: key_missed");
    assert_eq!(au.data.len() as i64, int("len"), "{at}: len");
    assert_eq!(
        tb_golden::fnv1a(&au.data),
        r.u64_le("fnv").unwrap(),
        "{at}: fnv"
    );
    assert_eq!(au.data, r.bytes("data").unwrap(), "{at}: data");
    assert_eq!(au.pts, int("pts"), "{at}: pts");
    assert_eq!(au.dts, int("dts"), "{at}: dts");
    assert_eq!(f64::from(au.avg_q), num("avg_q"), "{at}: avg_q");
    assert_eq!(au.complexity, num("complexity"), "{at}: complexity");
    assert_eq!(au.bits as i64, int("bits"), "{at}: bits");
    assert_eq!(au.rate as i64, int("rate"), "{at}: rate");
    assert_eq!(i64::from(au.rate_ticket), int("ticket"), "{at}: ticket");
    assert_eq!(au.queue_bits, num("queue"), "{at}: queue");
    assert_eq!(au.room_bits, num("room"), "{at}: room");
    assert_eq!(au.cap_bits, num("cap"), "{at}: cap");
    assert_eq!(i64::from(au.rung), int("rung"), "{at}: rung");
    assert_eq!(i64::from(au.tries), int("tries"), "{at}: tries");
    assert_eq!(i64::from(au.intra_mbs), int("intra"), "{at}: intra");
    assert_eq!(i64::from(au.field_mbs), int("field"), "{at}: field");
    assert!(!au.last && au.recon.is_empty(), "{at}");
}

/// The session after the access unit, as the `au` record gives it.
fn check_state(at: &str, r: &Record, enc: &Encoder, s: SessionId) {
    let snap = enc.snapshot(s).unwrap();
    let rc = &snap.rc;
    let nums = |f: &str| r.nums(f).unwrap();
    let ints = |f: &str| r.ints(f).unwrap();
    assert_eq!(rc.cbr, r.flag("cbr").unwrap(), "{at}: cbr");
    let theta: Vec<f64> = rc.mdl.iter().map(|m| m.theta).collect();
    let hdr: Vec<f64> = rc.mdl.iter().map(|m| m.hdr).collect();
    assert_eq!(theta, nums("theta"), "{at}: theta");
    assert_eq!(hdr, nums("mdl_hdr"), "{at}: mdl_hdr");
    assert_eq!(rc.x.to_vec(), nums("X"), "{at}: X");
    assert_eq!(rc.r, r.num("R").unwrap(), "{at}: R");
    let wide = |v: &[u8]| v.iter().map(|x| i64::from(*x)).collect::<Vec<_>>();
    assert_eq!(
        rc.left.iter().map(|x| i64::from(*x)).collect::<Vec<_>>(),
        ints("left"),
        "{at}: left"
    );
    assert_eq!(wide(&rc.floor_hist), ints("floor_hist"), "{at}: floor_hist");
    assert_eq!(wide(&rc.last_rung), ints("last_rung"), "{at}: last_rung");
    assert_eq!(rc.last_qs, r.num("last_qs").unwrap(), "{at}: last_qs");
    assert_eq!(rc.avg_act, r.num("avg_act").unwrap(), "{at}: avg_act");
    assert_eq!(
        rc.stats.violations as i64,
        r.int("violations").unwrap(),
        "{at}: violations"
    );
    assert_eq!(
        i64::from(snap.newer),
        r.int("newer").unwrap(),
        "{at}: newer"
    );
    assert_eq!(snap.ref_disp.to_vec(), ints("ref_disp"), "{at}: ref_disp");
    assert_eq!(snap.dup, r.flag("dup").unwrap(), "{at}: dup");
    assert_eq!(snap.have_ref, r.flag("have_ref").unwrap(), "{at}: have_ref");
    assert_eq!(snap.rekey, r.flag("rekey").unwrap(), "{at}: rekey");
    assert_eq!(
        snap.tref_base as i64,
        r.int("tref_base").unwrap(),
        "{at}: tref_base"
    );
}

/// What a run produced and saw.
#[derive(Default)]
struct Run {
    /// Access units per session, in coded order.
    streams: Vec<Vec<AccessUnit>>,
    /// Ticks run.
    ticks: usize,
    /// Plans submitted, by phase.
    plans: Vec<(i64, Phase, Plan)>,
    /// Errors the device reported.
    errors: Vec<DeviceError>,
    /// Device time at the end of each tick.
    times: Vec<Nanos>,
}

/// Drive the case's inputs through the encoder against `dev`. With `compare`, every access
/// unit and decision is checked against the records; without (fault runs), outputs are only
/// collected. `stop_after`: stop after this tick.
fn run(c: &mut Case, dev: &mut Replay, compare: bool, stop_after: Option<i64>) -> Run {
    let name = c.name;
    let mut out = Run {
        streams: vec![Vec::new(); c.sids.len()],
        ..Run::default()
    };
    let mut plan = Plan::new();
    // The access units of each tick, in the order the recorded run made them.
    let mut aus: BTreeMap<i64, Vec<&Record>> = BTreeMap::new();
    for r in c.recs.iter().filter(|r| r.kind() == "au") {
        aus.entry(r.int("tick").unwrap()).or_default().push(r);
    }
    let sheds: BTreeMap<i64, &Record> = c
        .recs
        .iter()
        .filter(|r| r.kind() == "shed")
        .map(|r| (r.int("tick").unwrap(), r))
        .collect();
    let enc = &mut c.enc;
    // The recording driver waits for the streams after opening its sessions.
    let d = common::finish(enc, dev, &mut plan);
    assert!(d.errors.is_empty(), "{name}: set-up {:?}", d.errors);
    assert_eq!(d.phases, [Phase::Settle, Phase::Settle]);
    let sid = |r: &Record| c.sids[r.int("s").unwrap() as usize];
    for (i, r) in c.recs.iter().enumerate() {
        let at = format!("{name}, record {i} ({})", r.kind());
        match r.kind() {
            "submit" => {
                let s = sid(r);
                enc.hold(s, r.flag("hold").unwrap()).unwrap();
                if compare {
                    let snap = enc.snapshot(s).unwrap();
                    assert_eq!(snap.n_in as i64, r.int("disp").unwrap(), "{at}: disp");
                    assert_eq!(snap.rekey, r.flag("rekey").unwrap(), "{at}: rekey");
                    assert_eq!(snap.want_key, r.flag("want_key").unwrap(), "{at}");
                    // the record has the grid time in frame periods
                    assert_eq!(
                        enc.next_key(s).unwrap() as i64,
                        r.int("next_key").unwrap(),
                        "{at}"
                    );
                }
                assert!(enc.frame_buffer(s).is_ok());
                // a frame shown for other than one frame period says so in its record
                let mut shown = enc.ordinary(s).unwrap();
                if let Ok(units) = r.int("units") {
                    shown.units = units as u8;
                    shown.top_first = r.flag("top_first").unwrap();
                    assert_eq!(
                        enc.display_units(s).unwrap() as i64,
                        r.int("start").unwrap(),
                        "{at}: start"
                    );
                }
                shown.progressive = r.flag("prog").unwrap();
                enc.submit_shown(
                    s,
                    FrameSource::Host,
                    r.int("pts").unwrap(),
                    &r.bytes("cc").unwrap(),
                    shown,
                )
                .unwrap();
            }
            "force_key" => enc.force_keyframe(sid(r)).unwrap(),
            "set_rate" => {
                let ticket = enc
                    .set_rate(
                        sid(r),
                        r.int("bps").unwrap() as u64,
                        r.flag("at_gop").unwrap(),
                    )
                    .unwrap();
                assert_eq!(i64::from(ticket), r.int("ticket").unwrap(), "{at}: ticket");
                let pending = enc.snapshot(sid(r)).unwrap().pending_rate.unwrap();
                assert_eq!(pending.0 as i64, r.int("clamped").unwrap(), "{at}: clamped");
            }
            "flush" => enc.flush(sid(r)).unwrap(),
            "tick" => {
                let t = r.int("tick").unwrap();
                if compare {
                    assert_eq!(enc.shed_state().rng, r.u64_le("rng").unwrap(), "{at}: rng");
                    assert_eq!(enc.shed_state().steps as i64, t, "{at}: steps");
                }
                let before: Vec<u64> = c
                    .sids
                    .iter()
                    .map(|s| enc.snapshot(*s).unwrap().stats.shed)
                    .collect();
                let d = common::tick(enc, dev, &mut plan, None);
                out.ticks += 1;
                out.times.push(dev.now());
                out.errors.extend(d.errors.iter().cloned());
                for (p, pl) in d.phases.iter().zip(d.plans) {
                    out.plans.push((t, *p, pl));
                }
                if compare {
                    assert!(d.errors.is_empty(), "{at}: {:?}", d.errors);
                    let sh = sheds[&t];
                    let stats = enc.tick_stats();
                    assert_eq!(
                        i64::from(stats.candidates),
                        sh.int("candidates").unwrap(),
                        "{at}"
                    );
                    assert_eq!(i64::from(stats.shed), sh.int("shed_count").unwrap(), "{at}");
                    assert_eq!(enc.shed_state().rng, sh.u64_le("rng").unwrap(), "{at}");
                    let shed: Vec<i64> = c
                        .sids
                        .iter()
                        .zip(&before)
                        .filter(|(s, b)| enc.snapshot(**s).unwrap().stats.shed != **b)
                        .map(|(s, _)| i64::from(s.0))
                        .collect();
                    assert_eq!(shed, sh.ints("shed").unwrap(), "{at}: sessions shed");
                    for a in aus.get(&t).into_iter().flatten() {
                        let s = sid(a);
                        let at = format!("{name}: tick {t}, session {}", s.0);
                        let au = enc
                            .take_output(s)
                            .unwrap_or_else(|| panic!("{at}: no output"));
                        check_au(&at, a, &au);
                        check_state(&at, a, enc, s);
                        out.streams[s.0 as usize].push(au);
                    }
                }
                for s in &c.sids {
                    while let Some(au) = enc.take_output(*s) {
                        assert!(
                            !compare,
                            "{at}: an access unit the recorded run did not make"
                        );
                        out.streams[s.0 as usize].push(au);
                    }
                }
                if stop_after == Some(t) {
                    return out;
                }
            }
            _ => {}
        }
    }
    let d = common::finish(enc, dev, &mut plan);
    out.errors.extend(d.errors);
    out
}

fn stream(aus: &[AccessUnit]) -> Vec<u8> {
    let mut v: Vec<u8> = aus.iter().flat_map(|a| a.data.iter().copied()).collect();
    v.extend_from_slice(&SEQUENCE_END);
    v
}

/// Acceptance 1 to 3 for one case.
fn replay(name: &'static str) -> Run {
    let mut c = load(name);
    let mut dev = Replay::new(c.gt.trace.clone()).compare_by(Compare::NormalForm);
    let out = run(&mut c, &mut dev, true, None);
    assert!(out.errors.is_empty(), "{name}: {:?}", out.errors);
    assert!(
        dev.exhausted(),
        "{name}: the recorded run issued more plans"
    );
    let n_au = c.recs.iter().filter(|r| r.kind() == "au").count();
    assert_eq!(out.streams.iter().map(Vec::len).sum::<usize>(), n_au);
    let sha = tb_golden::digests(name);
    for (s, want) in sha.iter().enumerate() {
        assert_eq!(
            common::sha256(&stream(&out.streams[s])),
            *want,
            "{name}: stream of session {s}"
        );
    }
    assert!(c.enc.idle());
    out
}

#[test]
fn fixedq_equals_the_records() {
    let out = replay("fixedq");
    assert_eq!(out.streams[0].len(), 12);
    // Three plans in every tick, whatever its work.
    assert_eq!(out.plans.len(), 3 * out.ticks);
}

#[test]
fn cbr_equals_the_records() {
    let out = replay("cbr");
    assert_eq!(out.streams[0].len(), 30);
    // The two rate changes took effect at key frames and their pictures carry the tickets.
    let tickets: Vec<u32> = out.streams[0]
        .iter()
        .map(|a| a.rate_ticket)
        .filter(|t| *t != 0)
        .collect();
    assert_eq!(tickets, [1, 2]);
    assert!(out.plans.len() > 3 * out.ticks, "correction passes");
}

#[test]
fn ladder_equals_the_records() {
    let out = replay("ladder");
    let rungs: Vec<u8> = out.streams[0].iter().map(|a| a.rung).collect();
    assert!(rungs.iter().any(|r| *r >= 33), "{rungs:?}");
    assert!(out.streams[0].iter().any(|a| a.tries >= 2));
}

#[test]
fn interlaced_with_overlapped_upload_equals_the_records() {
    let out = replay("interlaced");
    // Every tick starts with the wait for the upload stream.
    for t in out.plans.chunk_by(|a, b| a.0 == b.0) {
        assert_eq!(t[0].1, Phase::Sync);
        assert_eq!(t[0].2.wait, StreamId::UPLOAD);
        assert!(t[0].2.steps.is_empty());
        assert_eq!(t[1].1, Phase::Begin);
    }
    assert!(
        out.streams[0]
            .iter()
            .any(|a| a.rate_ticket == 1 && !a.keyframe)
    );
}

#[test]
fn shed_equals_the_records() {
    let out = replay("shed");
    let all: Vec<&AccessUnit> = out.streams.iter().flatten().collect();
    assert_eq!(all.len(), 60);
    assert!(all.iter().filter(|a| a.repeat).count() > 10);
    assert!(all.iter().any(|a| a.key_missed));
    assert!(all.iter().any(|a| a.off_grid));
}

/// Film mode, operation for operation: 3:2 pulldown with a cut, video pictures, a still, a hold
/// and a rate change on an interlaced session; frames shown one, two and three times with
/// pictures given up at random on a progressive one.
#[test]
fn film_equals_the_records() {
    let out = replay("film");
    let s = &out.streams[0];
    assert_eq!(s.len(), 40);
    let by = |u: u8| s.iter().filter(|a| a.units == u).count();
    assert_eq!((by(2), by(3)), (23, 17));
    assert!(
        s.iter().any(|a| a.repeat && a.units == 3),
        "a held film picture"
    );
    assert!(s.iter().any(|a| !a.top_first && a.units == 2));
    // a tick without a picture is a tick like any other: three plans
    assert!(out.ticks > 40);
    assert_eq!(out.plans.len() % out.ticks, 0);

    let out = replay("film60");
    let s = &out.streams[0];
    assert_eq!(s.len(), 40);
    assert!(s.iter().any(|a| a.units == 1) && s.iter().any(|a| a.units == 3));
    assert!(s.iter().any(|a| a.repeat));
}

/// The last rung of the ladder: all-skipped pictures, and I pictures replaced by P pictures
/// (no GOP header, temporal_reference counting on), operation for operation.
#[test]
fn skip_equals_the_records() {
    let out = replay("skip");
    let s = &out.streams[0];
    assert!(s.iter().filter(|a| a.rung == 37 && !a.repeat).count() > 5);
    assert!(
        s.iter()
            .any(|a| a.pic_type == 2 && !a.keyframe && a.disp % 2 == 0 && !a.repeat)
    );
}

/// The fence of the overlapped upload (notes/cuda-mpeg2-optlog.md §41) in every `Begin` plan
/// of the overlapped case: the event is recorded on the kernel stream after every parameter
/// upload and before the first kernel; the upload stream's first step waits for it, after the
/// record in queue order; everything else on the upload stream follows that wait.
#[test]
fn the_frame_upload_waits_for_the_parameters_in_every_begin_plan() {
    let out = replay("interlaced");
    let mut begins = 0;
    let mut with_frames = 0;
    for (t, phase, plan) in &out.plans {
        if *phase != Phase::Begin {
            assert!(
                plan.steps.iter().all(|s| s.stream == StreamId::MAIN),
                "tick {t}: only Begin uses the upload stream"
            );
            assert!(
                !plan
                    .steps
                    .iter()
                    .any(|s| matches!(s.op, Op::Record(_) | Op::WaitFor(_))),
                "tick {t}"
            );
            continue;
        }
        begins += 1;
        let pos = |f: &dyn Fn(&tb_device::Step) -> bool| plan.steps.iter().position(f);
        let record = pos(&|s| matches!(s.op, Op::Record(_))).expect("a record");
        assert_eq!(plan.steps[record].stream, StreamId::MAIN);
        assert_eq!(
            plan.steps[record].op,
            Op::Record(tb_device::EventId::PARAMS)
        );
        let main = |s: &tb_device::Step| s.stream == StreamId::MAIN;
        let last_upload_on_main = plan
            .steps
            .iter()
            .rposition(|s| main(s) && matches!(s.op, Op::Upload { .. } | Op::Upload2d { .. }));
        assert!(last_upload_on_main.is_none_or(|i| i < record), "tick {t}");
        let first_launch = pos(&|s| main(s) && matches!(s.op, Op::Launch(_)));
        assert!(first_launch.is_none_or(|i| i > record), "tick {t}");
        let first_up = pos(&|s| s.stream == StreamId::UPLOAD);
        if let Some(i) = first_up {
            with_frames += 1;
            assert_eq!(
                plan.steps[i].op,
                Op::WaitFor(tb_device::EventId::PARAMS),
                "tick {t}"
            );
            assert!(i > record, "tick {t}: the wait follows the record");
            assert!(
                plan.steps[i + 1..]
                    .iter()
                    .all(|s| s.stream == StreamId::UPLOAD
                        && !matches!(s.op, Op::Record(_) | Op::WaitFor(_))),
                "tick {t}"
            );
        }
        assert_eq!(plan.wait, StreamId::MAIN);
    }
    assert_eq!(begins, out.ticks);
    assert_eq!(
        with_frames, out.ticks,
        "the recorded run queues the wait in every tick"
    );
}

fn scenario(c: &Case) -> Scenario {
    Scenario::new(c.gt.trace.clone(), c.gt.tags.clone())
        .unwrap()
        .compare(Compare::NormalForm)
}

/// Acceptance 4: a delayed plan changes nothing but times.
#[test]
fn a_delayed_plan_changes_nothing_but_times() {
    for name in ["cbr", "interlaced", "shed"] {
        let base = replay(name);
        let mut c = load(name);
        let mut dev = scenario(&c)
            .delay(Select::ticks(3..=5), Nanos(9_000_000))
            .unwrap()
            .delay(
                Select::tick(8).phase(SimPhase::Quant).nth(0),
                Nanos(40_000_000),
            )
            .unwrap()
            .replay();
        let out = run(&mut c, &mut dev, true, None);
        assert!(dev.exhausted());
        assert_eq!(out.streams, base.streams, "{name}");
        // The driver waits 2 ms at a time: the late plans were waited for more than once.
        assert!(out.times[5] > base.times[5]);
        assert!(out.times[8].0 - out.times[7].0 >= 40_000_000);
    }
}

/// The start code values of an access unit, in order.
fn start_codes(data: &[u8]) -> Vec<u8> {
    data.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| w[3])
        .collect()
}

/// A repeat made on the host: picture header, coding extension, optional user data, one slice
/// per macroblock row, nothing else.
fn is_skip_picture(au: &AccessUnit, mbh: u8) -> bool {
    let codes = start_codes(&au.data);
    let slices: Vec<u8> = (1..=mbh).collect();
    let head: &[u8] = if codes.get(2) == Some(&0xB2) {
        &[0x00, 0xB5, 0xB2]
    } else {
        &[0x00, 0xB5]
    };
    au.repeat && codes.starts_with(head) && codes[head.len()..] == slices
}

/// Acceptance 4: a failed plan, in each phase, yields repeats for that tick and a key frame
/// next. Up to the failure the run equals the recorded one.
#[test]
fn a_failed_plan_in_each_phase_yields_repeats_and_a_key_frame_next() {
    for (phase, expect) in [
        (SimPhase::Begin, Phase::Begin),
        (SimPhase::Quant, Phase::Quant),
        (SimPhase::Finish, Phase::Finish),
    ] {
        let base = replay("cbr");
        let mut c = load("cbr");
        let tick = 7;
        let mut dev = scenario(&c)
            .fail(
                Select::tick(tick).phase(phase).nth(0),
                DeviceError::Failed { step: 3, code: 2 },
            )
            .unwrap()
            .replay();
        let out = run(&mut c, &mut dev, false, Some(tick));
        assert_eq!(
            out.errors,
            [DeviceError::Failed { step: 3, code: 2 }],
            "{phase:?}"
        );
        assert_eq!(out.plans.last().unwrap().1, expect);
        assert!(c.enc.tick_stats().failed);
        assert!(!c.enc.is_lost());
        let got = &out.streams[0];
        let n = got.len();
        assert_eq!(got[..n - 1], base.streams[0][..n - 1], "{phase:?}: before");
        let (last, was) = (&got[n - 1], &base.streams[0][n - 1]);
        assert!(is_skip_picture(last, 9), "{phase:?}: {last:?}");
        assert_eq!(
            (last.disp, last.coded, last.pts, last.dts),
            (was.disp, was.coded, was.pts, was.dts)
        );
        assert_eq!(last.key_missed, was.keyframe);
        let snap = c.enc.snapshot(c.sids[0]).unwrap();
        assert!(snap.rekey, "{phase:?}: the next frame is a key frame");
        assert_eq!(snap.stats.failed, 1);
        assert_eq!(snap.n_coded as usize, n);
        // The next frame submitted is typed as a key frame off the grid, and the tick after
        // codes: the encoder goes on.
        let s = c.sids[0];
        let grid = snap.next_key;
        c.enc.submit(s, FrameSource::Host, 0, &[]).unwrap();
        let after = c.enc.snapshot(s).unwrap();
        assert!(!after.rekey);
        assert_eq!(after.next_key, grid, "the cadence stays on its grid");
        assert_eq!(after.gop_first, snap.n_in);
        let mut plan = Plan::new();
        let mut host = vec![0u8; ARENAS.host as usize];
        assert_eq!(
            c.enc.begin_tick(Nanos(0), None, &mut host, &mut plan),
            tb_m2v_encoder::Step::Run
        );
        assert!(plan.validate(ARENAS).is_ok());
    }
}

/// Acceptance 4: `Lost` holds all sessions and the streams stay decodable (headers and skip
/// slices only); no plan is written until the device is restored.
#[test]
fn a_lost_device_holds_every_session() {
    let base = replay("shed");
    let mut c = load("shed");
    let tick = 9;
    let mut dev = scenario(&c)
        .fail(
            Select::tick(tick).phase(SimPhase::Quant).nth(0),
            DeviceError::Lost { code: 999 },
        )
        .unwrap()
        .replay();
    let out = run(&mut c, &mut dev, false, None);
    assert_eq!(out.errors, [DeviceError::Lost { code: 999 }]);
    assert!(c.enc.is_lost());
    // Nothing was submitted after the failed plan.
    assert_eq!(out.plans.last().unwrap().0, tick);
    for (s, got) in out.streams.iter().enumerate() {
        // Every frame still has its access unit, in order, with the recorded timeline.
        assert_eq!(got.len(), 30, "session {s}");
        for (k, (a, b)) in got.iter().zip(&base.streams[s]).enumerate() {
            assert_eq!(
                (a.coded, a.dts),
                (b.coded, b.dts),
                "session {s}, picture {k}"
            );
            assert_eq!(a.coded, k as u64);
        }
        let mut disp: Vec<u64> = got.iter().map(|a| a.disp).collect();
        disp.sort_unstable();
        assert_eq!(disp, (0..30).collect::<Vec<u64>>());
        let first_bad = got
            .iter()
            .position(|a| a != &base.streams[s][a.coded as usize])
            .unwrap();
        assert!(first_bad >= 5, "session {s}: {first_bad}");
        assert!(got[first_bad..].iter().all(|a| is_skip_picture(a, 9)));
        let snap = c.enc.snapshot(SessionId(s as u32)).unwrap();
        assert_eq!(snap.queued, 0);
        assert_eq!(snap.n_coded, 30);
        // The buffer model went on: the queue drains by what each repeat cannot fill.
        assert!(got.last().unwrap().queue_bits < got[first_bad].queue_bits);
    }
}
