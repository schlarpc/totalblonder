use super::*;
use crate::sim::normal::normal_form;
use crate::sim::{Compare, Fault, Replay, Scenario, Select};
use crate::{Device, DeviceError};

fn case(name: &str) -> Vec<Gold> {
    tb_golden::case(name).unwrap()
}

fn waits(recs: &[Gold]) -> usize {
    recs.iter()
        .filter(|r| r.kind() == "op" && r.text("op").unwrap() == "wait")
        .count()
}

/// A stand-in for the core: for each plan it writes the bytes the recorded host wrote, submits
/// a plan with the recorded normal form (buffers from `names`, placeholder kernel arguments),
/// waits, and after each `Finish` plan assembles the coded pictures from the host arena as the
/// C++ `collect` does (headers + `h_out` at `h_outpos`, `min(h_pic.bytes, cap)` bytes), which
/// must be the recorded access units.
fn replay_as_recorded(name: &str) -> usize {
    let recs = case(name);
    let plans = golden_plans(&recs).unwrap();
    let names = NameTable::packed(&plans);
    let gt = golden_trace(&recs, &names).unwrap();
    assert_eq!(gt.plans, plans);
    assert_eq!(
        gt.trace.records.len(),
        waits(&recs),
        "{name}: one plan per wait"
    );
    let batch = recs.iter().find(|r| r.kind() == "batch").unwrap();
    let cap = batch.int("cap_words").unwrap() as usize * 4;

    // The coded pictures of each tick, in order, with the headers before them.
    /// Slot, headers, access unit, digest.
    type Coded = (usize, Vec<u8>, Vec<u8>, u64);
    let mut aus: BTreeMap<i64, Vec<Coded>> = BTreeMap::new();
    for (i, r) in recs.iter().enumerate() {
        if r.kind() == "au" && !r.flag("repeat").unwrap() {
            let s = r.int("s").unwrap();
            let hdr = recs[..i]
                .iter()
                .rev()
                .find(|h| h.kind() == "hdr" && h.int("s").unwrap() == s)
                .unwrap()
                .bytes("bytes")
                .unwrap();
            let slot = recs[..i]
                .iter()
                .rev()
                .find(|b| b.kind() == "begin" && b.int("s").unwrap() == s)
                .unwrap()
                .int("slot")
                .unwrap() as usize;
            aus.entry(r.int("tick").unwrap()).or_default().push((
                slot,
                hdr,
                r.bytes("data").unwrap(),
                r.u64_le("fnv").unwrap(),
            ));
        }
    }

    let mut dev = Replay::new(gt.trace.clone()).compare_by(Compare::NormalForm);
    let mut pictures = 0;
    for (g, rec) in gt.plans.iter().zip(&gt.trace.records) {
        for (b, bytes) in &rec.uploads {
            b.of_mut(dev.host_mut()).unwrap().copy_from_slice(bytes);
        }
        let plan = plan_of(g, &names).unwrap();
        assert_eq!(normal_form(&plan), g.normal());
        plan.validate(names.arenas).unwrap();
        let t = dev.submit(&plan).unwrap();
        let c = dev.wait(t, None);
        assert_eq!(c.status, Ok(()), "{name}: plan of {:?}", g.tag);
        for (b, bytes) in &rec.downloads {
            assert_eq!(b.of(dev.host()).unwrap(), bytes.as_slice());
        }
        if g.tag.phase != Phase::Finish {
            continue;
        }
        let host = dev.host();
        let h = |n: &str| names.host[n].off as usize;
        for (slot, hdr, data, fnv) in aus.get(&g.tag.tick).into_iter().flatten() {
            let word = |k: usize| {
                let o = h("h_outpos") + 8 * slot + 4 * k;
                u32::from_le_bytes(host[o..o + 4].try_into().unwrap()) as usize
            };
            let o = h("h_pic") + size_of::<PicStat>() * slot;
            let bytes = u32::from_le_bytes(host[o..o + 4].try_into().unwrap()) as usize;
            let pb = bytes.min(cap);
            let src = h("h_out") + 4 * word(0);
            let mut au = hdr.clone();
            au.extend_from_slice(&host[src..src + pb]);
            assert_eq!(&au, data, "{name}: tick {}", g.tag.tick);
            assert_eq!(tb_golden::fnv1a(&au), *fnv);
            assert_eq!(word(1), pb.div_ceil(4) + 2);
            pictures += 1;
        }
    }
    assert!(dev.exhausted());
    pictures
}

#[test]
fn fixedq_replays_as_recorded() {
    let recs = case("fixedq");
    assert_eq!(waits(&recs), 46);
    let coded = recs
        .iter()
        .filter(|r| r.kind() == "au" && !r.flag("repeat").unwrap())
        .count();
    assert_eq!(replay_as_recorded("fixedq"), coded);
}

#[test]
fn every_case_replays_as_recorded() {
    for name in ["cbr", "ladder", "interlaced", "shed"] {
        let recs = case(name);
        let coded = recs
            .iter()
            .filter(|r| r.kind() == "au" && !r.flag("repeat").unwrap())
            .count();
        assert_eq!(replay_as_recorded(name), coded, "{name}");
    }
}

#[test]
fn fixedq_plans_and_phases() {
    let recs = case("fixedq");
    let plans = golden_plans(&recs).unwrap();
    let tags: Vec<_> = plans.iter().map(|p| (p.tag.tick, p.tag.phase)).collect();
    // Two set-up waits, then three per tick, then the two waits of `Encoder::finish`.
    assert_eq!(tags[..2], [(-1, Phase::Other), (-1, Phase::Other)]);
    for t in 0..14 {
        let i = 2 + 3 * t as usize;
        assert_eq!(
            tags[i..i + 3],
            [(t, Phase::Begin), (t, Phase::Quant), (t, Phase::Finish)]
        );
    }
    assert_eq!(tags[44..], [(13, Phase::Other), (13, Phase::Other)]);
    // Tick 3 (a P picture): the Begin plan in normal form.
    let begin = &plans[2 + 3 * 3];
    let nf: Vec<_> = begin
        .normal()
        .iter()
        .map(|o| {
            (
                o.op.name(),
                o.kernel.unwrap_or(""),
                o.grid,
                o.block,
                o.bytes,
            )
        })
        .collect();
    assert_eq!(
        nf,
        [
            ("upload2d", "", 0, 0, 184 * 144),
            ("upload2d", "", 0, 0, 92 * 72),
            ("upload2d", "", 0, 0, 92 * 72),
            ("upload", "", 0, 0, 4),
            ("upload", "", 0, 0, 1),
            ("launch", "k_pad_cols", 5, 256, 0),
            ("launch", "k_pad_cols", 2, 256, 0),
            ("launch", "k_pad_cols", 2, 256, 0),
            ("upload", "", 0, 0, 20),
            ("upload", "", 0, 0, 4),
            ("upload", "", 0, 0, 4),
            ("launch", "k_pyr", 2, 256, 0),
            ("launch", "k_me", 2, 64, 0),
            ("launch", "k_mvsmooth", 1, 256, 0),
            ("launch", "k_mbstat", 1, 256, 0),
            ("launch", "k_xf", 81, 64, 0),
            ("launch", "k_rowstat", 9, 32, 0),
            ("download", "", 0, 0, 720),
        ]
    );
    assert_eq!(begin.ops[8].dst.as_deref(), Some("d_par"));
    assert_eq!(begin.ops[9].src.as_deref(), Some("h_list[L_CODE]"));
    assert_eq!(begin.ops[0].pitches, Some((192, 184, 184, 144)));
}

#[test]
fn fixedq_trace_carries_the_golden_bytes() {
    let recs = case("fixedq");
    let plans = golden_plans(&recs).unwrap();
    let names = NameTable::packed(&plans);
    let gt = golden_trace(&recs, &names).unwrap();
    let h = |n: &str| names.host[n];
    let begin = |tick: i64, k: &str| {
        recs.iter()
            .find(|r| r.kind() == k && r.int("tick").unwrap() == tick)
            .unwrap()
    };
    // Tick 3: Begin uploads the parameters and the lists, downloads the row statistics.
    let [b, q, f] = [11usize, 12, 13].map(|i| &gt.trace.records[i]);
    let par = begin(3, "begin").bytes("par").unwrap();
    assert_eq!(
        b.uploads,
        [
            (h("h_par").slice(0, 20).unwrap(), par.clone()),
            (h("h_list[L_CODE]").slice(0, 4).unwrap(), vec![0; 4]),
            (h("h_list[L_ME]").slice(0, 4).unwrap(), vec![0; 4]),
        ]
    );
    assert_eq!(
        b.downloads,
        [(
            h("h_rowstat").slice(0, 720).unwrap(),
            begin(3, "rowstat").bytes("rows").unwrap()
        )]
    );
    // Quant: parameters again and the row quantizers; the picture size from `au`
    // (no rate control, so no `pass` record).
    assert_eq!(
        q.uploads,
        [
            (h("h_par").slice(0, 20).unwrap(), par),
            (h("h_rowq").slice(0, 9).unwrap(), vec![6; 9]),
        ]
    );
    let au = begin(3, "au");
    let pic = PicStat {
        bytes: au.int("slice_bytes").unwrap() as u32,
        tex_bits: au.int("tex_bits").unwrap() as u32,
        ..PicStat::default()
    };
    assert_eq!(
        pic,
        PicStat {
            bytes: 11257,
            tex_bits: 87526,
            ..PicStat::default()
        }
    );
    assert_eq!(
        q.downloads,
        [(
            h("h_pic").slice(0, size_of::<PicStat>() as u64).unwrap(),
            bytemuck::bytes_of(&pic).to_vec()
        )]
    );
    // Finish: the output positions; the slices.
    assert_eq!(
        f.uploads,
        [(
            h("h_outpos").slice(0, 8).unwrap(),
            [0u32, 2817]
                .iter()
                .flat_map(|x| x.to_le_bytes())
                .collect::<Vec<_>>()
        )]
    );
    assert_eq!(f.downloads.len(), 1);
    assert_eq!(f.downloads[0].0, h("h_out").slice(0, 11257).unwrap());
    assert!(au.bytes("data").unwrap().ends_with(&f.downloads[0].1));
}

#[test]
fn a_core_that_uploads_other_bytes_is_caught() {
    let recs = case("fixedq");
    let plans = golden_plans(&recs).unwrap();
    let names = NameTable::packed(&plans);
    let gt = golden_trace(&recs, &names).unwrap();
    let mut dev = Replay::new(gt.trace.clone()).compare_by(Compare::NormalForm);
    for (k, (g, rec)) in gt.plans.iter().zip(&gt.trace.records).enumerate() {
        for (b, bytes) in &rec.uploads {
            b.of_mut(dev.host_mut()).unwrap().copy_from_slice(bytes);
        }
        let plan = plan_of(g, &names).unwrap();
        if k == 12 {
            // The second row quantizer of tick 3 off by one.
            let rowq = names.host["h_rowq"];
            dev.host_mut()[rowq.off as usize + 1] = 7;
            assert_eq!(
                dev.submit(&plan),
                Err(DeviceError::BadPlan {
                    step: 0,
                    what: "replay: uploaded bytes differ from the recording"
                })
            );
            return;
        }
        let t = dev.submit(&plan).unwrap();
        assert_eq!(dev.wait(t, None).status, Ok(()));
    }
    unreachable!();
}

#[test]
fn a_golden_scenario_with_faults_by_phase() {
    let recs = case("fixedq");
    let plans = golden_plans(&recs).unwrap();
    let names = NameTable::packed(&plans);
    let gt = golden_trace(&recs, &names).unwrap();
    let s = Scenario::new(gt.trace.clone(), gt.tags.clone())
        .unwrap()
        .compare(Compare::NormalForm)
        .fail(
            Select::tick(7).phase(Phase::Quant).nth(0),
            DeviceError::Lost { code: 999 },
        )
        .unwrap()
        .delay(Select::ticks(3..=5), Nanos(9_000_000))
        .unwrap();
    assert_eq!(
        s.faults()[0],
        Fault::Fail {
            submit: 2 + 3 * 7 + 1,
            error: DeviceError::Lost { code: 999 }
        }
    );
    assert_eq!(s.faults().len(), 1 + 9);
    let mut dev = s.replay();
    for (k, (g, rec)) in gt.plans.iter().zip(&gt.trace.records).enumerate() {
        for (b, bytes) in &rec.uploads {
            b.of_mut(dev.host_mut()).unwrap().copy_from_slice(bytes);
        }
        let before = dev.now();
        let t = dev.submit(&plan_of(g, &names).unwrap()).unwrap();
        let c = dev.wait(t, None);
        let took = dev.now().0 - before.0;
        match k {
            24 => assert_eq!(c.status, Err(DeviceError::Lost { code: 999 })),
            11..=19 => assert_eq!((c.status, took), (Ok(()), 9_000_000)),
            _ => assert_eq!((c.status, took), (Ok(()), 0)),
        }
    }
}

/// Hand-made records: the parts of a tick the module reads, small enough to check by eye.
const HAND: &str = r#"{"k":"golden","version":1}
{"k":"batch","b":0,"capacity":2,"cap_words":4,"geo":"__GEO__"}
{"k":"op","tick":-1,"op":"wait","st":"main"}
{"k":"tick","tick":0}
{"k":"begin","tick":0,"s":5,"slot":1,"par":"0101010101010101010101010101010101010101"}
{"k":"lists","tick":0,"b":0,"code":[1],"intra":[1],"me":[]}
{"k":"op","tick":0,"op":"upload","dst":"d_par","src":"h_par","n":40,"st":"main"}
{"k":"op","tick":0,"op":"upload","dst":"d_list[L_CODE]","src":"h_list[L_CODE]","n":4,"st":"main"}
{"k":"op","tick":0,"op":"record","st":"main"}
{"k":"op","tick":0,"op":"waitfor","st":"upload"}
{"k":"op","tick":0,"op":"upload2d","dst":"dy","src":"sy","dst_pitch":8,"src_pitch":6,"width":6,"rows":2,"st":"upload"}
{"k":"op","tick":0,"op":"launch","kernel":"k_set_mode","grid":1,"block":256,"st":"main"}
{"k":"op","tick":0,"op":"download","dst":"h_rowstat","src":"d_rowstat","n":320,"st":"main"}
{"k":"op","tick":0,"op":"wait","st":"main","which":0}
{"k":"rowstat","tick":0,"s":5,"rows":"__ROWS__"}
{"k":"rc_plan","tick":0,"s":5,"rowq":"0303"}
{"k":"qent","tick":0,"b":0,"list":[1],"xf":[],"par":"0202020202020202020202020202020202020202"}
{"k":"op","tick":0,"op":"upload","dst":"d_par","src":"h_par","n":40,"st":"main"}
{"k":"op","tick":0,"op":"upload","dst":"d_rowq","src":"h_rowq","n":4,"st":"main"}
{"k":"op","tick":0,"op":"download","dst":"h_pic","src":"d_pic","n":48,"st":"main"}
{"k":"op","tick":0,"op":"wait","st":"main","which":1}
{"k":"pass","tick":0,"s":5,"bytes":30,"tex_bits":100}
{"k":"pass_out","tick":0,"s":5,"action":"again","rowq":"0404"}
{"k":"op","tick":0,"op":"upload","dst":"d_list[L_RETRY]","src":"h_list[L_RETRY]","n":4,"st":"main"}
{"k":"qent","tick":0,"b":0,"list":[1],"xf":[1],"par":"0303030303030303030303030303030303030303"}
{"k":"op","tick":0,"op":"upload","dst":"d_par","src":"h_par","n":40,"st":"main"}
{"k":"op","tick":0,"op":"upload","dst":"d_list[L_XF]","src":"h_list[L_XF]","n":4,"st":"main"}
{"k":"op","tick":0,"op":"upload","dst":"d_rowq","src":"h_rowq","n":4,"st":"main"}
{"k":"op","tick":0,"op":"download","dst":"h_pic","src":"d_pic","n":48,"st":"main"}
{"k":"op","tick":0,"op":"wait","st":"main"}
{"k":"pass","tick":0,"s":5,"bytes":5,"tex_bits":20}
{"k":"pass_out","tick":0,"s":5,"action":"accept","rowq":"0404"}
{"k":"hdr","s":5,"bytes":"00000100"}
{"k":"finish","tick":0,"b":0,"out_words":4,"outpos":[0,4]}
{"k":"op","tick":0,"op":"upload","dst":"d_outpos","src":"h_outpos","n":16,"st":"main"}
{"k":"op","tick":0,"op":"fill","dst":"d_out","byte":0,"n":16,"st":"main"}
{"k":"op","tick":0,"op":"download","dst":"h_out","src":"d_out","n":16,"st":"main"}
{"k":"op","tick":0,"op":"wait","st":"main","which":2}
{"k":"au","tick":0,"s":5,"repeat":0,"slice_bytes":5,"tex_bits":20,"data":"00000100aabbccddee","fnv":"0000000000000000"}
{"k":"op","tick":1,"op":"wait","st":"upload"}
"#;

fn hand_text(rows: &str) -> String {
    let geo = tb_kernel_abi::Geo {
        n: 2,
        w: 32,
        h: 32,
        mbw: 2,
        mbh: 2,
        nmb: 4,
        ..Default::default()
    };
    let hex: String = bytemuck::bytes_of(&geo)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    HAND.replace("__ROWS__", rows).replace("__GEO__", &hex)
}

fn hand() -> Vec<Gold> {
    let rows = "00".repeat(2 * 80).replacen("00", "07", 1);
    tb_golden::parse("hand", &hand_text(&rows)).unwrap()
}

#[test]
fn hand_made_records_make_this_trace() {
    let recs = hand();
    let plans = golden_plans(&recs).unwrap();
    let tags: Vec<_> = plans
        .iter()
        .map(|p| (p.tag.tick, p.tag.phase, p.wait))
        .collect();
    assert_eq!(
        tags,
        [
            (-1, Phase::Other, StreamId::MAIN),
            (0, Phase::Begin, StreamId::MAIN),
            (0, Phase::Quant, StreamId::MAIN),
            (0, Phase::Quant, StreamId::MAIN),
            (0, Phase::Finish, StreamId::MAIN),
            (1, Phase::Other, StreamId::UPLOAD),
        ]
    );
    let streams: Vec<_> = plans[1]
        .normal()
        .iter()
        .map(|o| (o.stream.0, o.op))
        .collect();
    assert_eq!(
        streams,
        [
            (0, OpKind::Upload),
            (0, OpKind::Upload),
            (0, OpKind::Record),
            (1, OpKind::WaitFor),
            (1, OpKind::Upload2d),
            (0, OpKind::Launch),
            (0, OpKind::Download),
        ]
    );
    let names = NameTable::packed(&plans);
    // Upload2d: 6 bytes twice, 6 apart on the host and 8 on the device.
    assert_eq!(names.host["sy"].len, 12);
    assert_eq!(names.dev["dy"].len, 14);
    let gt = golden_trace(&recs, &names).unwrap();
    let h = |n: &str, off, len| names.host[n].slice(off, len).unwrap();
    let r = &gt.trace.records;
    assert_eq!(r.len(), 6);
    assert!(r[0].uploads.is_empty() && r[0].downloads.is_empty());
    // Slot 1: its parameters at 20, its rows at 160, its picture statistics at 8.
    assert_eq!(
        r[1].uploads,
        [
            (h("h_par", 20, 20), vec![1; 20]),
            (h("h_list[L_CODE]", 0, 4), vec![1, 0, 0, 0])
        ]
    );
    let mut rows = vec![0; 160];
    rows[0] = 7;
    assert_eq!(r[1].downloads, [(h("h_rowstat", 160, 160), rows)]);
    assert_eq!(
        r[2].uploads,
        [
            (h("h_par", 20, 20), vec![2; 20]),
            (h("h_rowq", 2, 2), vec![3, 3])
        ]
    );
    let pic = |b: u32, t: u32| {
        bytemuck::bytes_of(&PicStat {
            bytes: b,
            tex_bits: t,
            ..PicStat::default()
        })
        .to_vec()
    };
    assert_eq!(r[2].downloads, [(h("h_pic", 24, 24), pic(30, 100))]);
    // The correction pass: the retry list from `pass_out`, the parameters and the transform
    // list from `qent`, the row quantizers after the first pass.
    assert_eq!(
        r[3].uploads,
        [
            (h("h_list[L_RETRY]", 0, 4), vec![1, 0, 0, 0]),
            (h("h_par", 20, 20), vec![3; 20]),
            (h("h_list[L_XF]", 0, 4), vec![1, 0, 0, 0]),
            (h("h_rowq", 2, 2), vec![4, 4]),
        ]
    );
    assert_eq!(r[3].downloads, [(h("h_pic", 24, 24), pic(5, 20))]);
    assert_eq!(
        r[4].uploads,
        [(h("h_outpos", 8, 8), vec![0, 0, 0, 0, 4, 0, 0, 0])]
    );
    assert_eq!(
        r[4].downloads,
        [(h("h_out", 0, 5), vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee])]
    );
    assert!(r[5].uploads.is_empty() && r[5].plan.steps.is_empty());
    assert_eq!(r[5].plan.wait, StreamId::UPLOAD);
    assert_eq!(gt.tags, plans.iter().map(|p| p.tag).collect::<Vec<_>>());
}

#[test]
fn records_that_contradict_each_other_are_reported() {
    let with = |from: &str, to: &str| {
        let rows = "00".repeat(2 * 80);
        let text = hand_text(&rows).replace(from, to);
        let recs = tb_golden::parse("hand", &text).unwrap();
        let names = NameTable::packed(&golden_plans(&recs)?);
        golden_trace(&recs, &names).map(|_| ())
    };
    assert_eq!(with("", ""), Ok(()));
    // The slices are not what `slice_bytes` says.
    assert!(matches!(
        with(r#""slice_bytes":5"#, r#""slice_bytes":6"#),
        Err(GoldenTraceError::Record { .. })
    ));
    // The access unit does not start with the headers.
    assert!(matches!(
        with(r#""data":"00000100"#, r#""data":"00000101"#),
        Err(GoldenTraceError::Record { .. })
    ));
    // An operation the protocol does not have has no plan step.
    assert!(matches!(
        with(r#""op":"fill""#, r#""op":"scan""#),
        Err(GoldenTraceError::Record { .. })
    ));
    // Operations after the last wait.
    let recs = tb_golden::parse(
        "tail",
        "{\"k\":\"op\",\"tick\":0,\"op\":\"launch\",\"kernel\":\"k_tick\",\"grid\":1,\"block\":1,\"st\":\"main\"}\n",
    )
    .unwrap();
    assert!(golden_plans(&recs).is_err());
    // A name missing from the table.
    let recs = hand();
    let mut names = NameTable::packed(&golden_plans(&recs).unwrap());
    names.host.remove("h_rowq");
    assert!(matches!(
        golden_trace(&recs, &names),
        Err(GoldenTraceError::Name(_))
    ));
}
