//! Planner tests. The `geom_*` tests are the conversions of `tools/gate/pipe_check.py` `geom-*`: the
//! keys are the ones the C++ pipeline harness made for them (`fit=stretch`, so
//! the whole source into the whole frame; sources tagged with the output's matrix, so no color
//! matrix; full rectangles, so no fill), and the expected launches, orders and counts follow
//! from `Converter::add` / `run` by hand (the arithmetic is in the comments).

use super::*;
use crate::{Filter, Rows};

/// One line per operation: what it does, on what, with which counts.
fn show(ops: &[ConvOp<u64>]) -> Vec<String> {
    let buf = |b: &ConvBuf| match b {
        ConvBuf::TapStart { class, axis } => format!("start {} {axis:?}", class.0),
        ConvBuf::TapCoef { class, axis } => format!("coef {} {axis:?}", class.0),
        ConvBuf::Jobs(c) => format!("jobs {}", c.0),
        ConvBuf::Tmp(c) => format!("tmp {}", c.0),
        ConvBuf::PreJobs { set, block } => format!("prejobs {}.{block}", set.0),
        ConvBuf::Pre { set, block } => format!("pre {}.{block}", set.0),
    };
    ops.iter()
        .map(|op| match op {
            ConvOp::Alloc {
                buf: b @ (ConvBuf::TapStart { .. } | ConvBuf::TapCoef { .. }),
                ..
            } => format!("alloc {}", buf(b)),
            ConvOp::Alloc {
                buf: b,
                bytes,
                replaces,
            } => format!(
                "alloc {} {bytes}{}",
                buf(b),
                if *replaces { " replaces" } else { "" }
            ),
            ConvOp::UploadTaps { class, axis } => format!("taps {} {axis:?}", class.0),
            ConvOp::UploadJobs { class, jobs } => format!("jobs {} x{}", class.0, jobs.len()),
            ConvOp::UploadPreJobs { set, block, jobs } => {
                format!("prejobs {}.{block} x{}", set.0, jobs.len())
            }
            ConvOp::Launch(l) => {
                let what = match l {
                    ConvLaunch::Half { jobs, nj, .. } => format!("{} nj={nj}", buf(jobs)),
                    ConvLaunch::ConvCopy { class, nj, .. }
                    | ConvLaunch::ConvH { class, nj, .. }
                    | ConvLaunch::ConvV { class, nj, .. } => format!("{} nj={nj}", class.0),
                    ConvLaunch::Fill { j, .. }
                    | ConvLaunch::Overlay { j, .. }
                    | ConvLaunch::Csc { j, .. } => format!("{}x{}", j.w, j.h),
                };
                format!("{} {what} grid={}", l.kernel(), l.grid())
            }
        })
        .collect()
}

/// The operations of a new resampling class: per axis two buffers and an upload.
fn new_class(c: u32) -> Vec<String> {
    Axis::ALL
        .iter()
        .flat_map(|a| {
            [
                format!("alloc start {c} {a:?}"),
                format!("alloc coef {c} {a:?}"),
                format!("taps {c} {a:?}"),
            ]
        })
        .collect()
}

fn lines(parts: &[&[&str]]) -> Vec<String> {
    parts
        .iter()
        .flat_map(|p| p.iter().map(|s| s.to_string()))
        .collect()
}

fn key(sw: u32, sh: u32, dw: u32, dh: u32, srows: Rows, drows: Rows) -> ConvKey {
    ConvKey {
        sw,
        sh,
        bps: 1,
        cx: 0,
        cy: 0,
        cw: sw,
        ch: sh,
        dw,
        dh,
        srows,
        drows,
        filter: Filter::Lanczos3,
        weave_lp: false,
    }
}

/// A decoder surface (pitch rounded up to 256 bytes, as NVDEC's).
fn src(n: u64, sw: u32) -> Source<u64> {
    let pitch = sw.div_ceil(256) * 256;
    Source {
        y: 0x1000_0000 * (n + 1),
        sp_y: pitch,
        uv: 0x1000_0000 * (n + 1) + 0x0800_0000,
        sp_uv: pitch,
    }
}

/// An encoder input slot, the rectangle at its origin.
fn dst(w: u32) -> Dest<u64> {
    Dest {
        y: 0x9000_0000,
        u: 0xa000_0000,
        v: 0xb000_0000,
        pitch: w + 32,
    }
}

/// One tick of a single-input channel: `add` for each job, then `run` (as in the C++ harness: no
/// fill, no color matrix, no overlay in the `geom-*` cases).
fn tick(conv: &mut Converter<u64>, jobs: &[ConvKey]) -> Vec<String> {
    let mut out = Vec::new();
    for k in jobs {
        conv.add(*k, src(0, k.sw), dst(k.dw), &mut out).unwrap();
    }
    conv.run(&mut out);
    show(&out)
}

/// A progressive or field conversion that needs one resampling class per job: the first tick
/// makes the class, the second reuses it.
fn one_class(k: ConvKey, tmp_bytes: u64, gh: u32, gv: u32) {
    let mut conv = Converter::new();
    let h = format!("k_conv_h 0 nj=1 grid={gh}");
    let v = format!("k_conv_v 0 nj=1 grid={gv}");
    let tmp = format!("alloc tmp 0 {tmp_bytes}");
    let mut want = new_class(0);
    want.extend(lines(&[&["alloc jobs 0 448", "jobs 0 x1", &tmp, &h, &v]]));
    assert_eq!(tick(&mut conv, &[k]), want);
    assert_eq!(tick(&mut conv, &[k]), lines(&[&["jobs 0 x1", &h, &v]]));
    assert_eq!(conv.classes(), 1);
    assert_eq!(conv.launches(), 4);
    let c = conv.class(ClassId(0)).unwrap();
    assert_eq!(u64::from(c.tstride) * 4 + 64, tmp_bytes);
}

#[test]
fn geom_1080p_720p() {
    // th_y 1080, th_c 540; tstride 1280 * 1620 = 2073600, four jobs' worth: 8294464.
    // k_conv_h: 320 * 1080 + 160 * 540 = 432000 -> 1688 blocks;
    // k_conv_v: 320 * 720 + 2 * 160 * 360 = 345600 -> 1350 blocks.
    one_class(
        key(1920, 1080, 1280, 720, Rows::All, Rows::All),
        8294464,
        1688,
        1350,
    );
}

#[test]
fn geom_1080p_480i_psf() {
    // 29.97p into 29.97i: both fields from one frame, converted as a frame (psf).
    // 1920 < 3 * 704: no pre-stage. tstride 704 * 1620 = 1140480.
    // h: 176 * 1080 + 88 * 540 = 237600 -> 929; v: 176 * 480 + 2 * 88 * 240 = 126720 -> 495.
    one_class(
        key(1920, 1080, 704, 480, Rows::All, Rows::All),
        4561984,
        929,
        495,
    );
}

#[test]
fn geom_720p_1080p() {
    // tstride 1920 * 1080 = 2073600; h: 480 * 720 + 240 * 360 = 432000 -> 1688;
    // v: 480 * 1080 + 2 * 240 * 540 = 777600 -> 3038.
    one_class(
        key(1280, 720, 1920, 1080, Rows::All, Rows::All),
        8294464,
        1688,
        3038,
    );
}

/// 2160p into a smaller frame: the source is halved first (k_half), then 1920x1080 resampled.
fn halved(dw: u32, dh: u32, tmp_bytes: u64, gh: u32, gv: u32) {
    let mut conv = Converter::new();
    let k = key(3840, 2160, dw, dh, Rows::All, Rows::All);
    // half frame 1920x1080: stride 1920 * 1080 * 3 / 2 = 3110400, a block of eight 24883264
    // (+ 64); k_half: (1920 / 4) * 1080 * 3 / 2 = 777600 per job -> 3038 blocks.
    let h = format!("k_conv_h 0 nj=1 grid={gh}");
    let v = format!("k_conv_v 0 nj=1 grid={gv}");
    let tmp = format!("alloc tmp 0 {tmp_bytes}");
    let mut want = lines(&[&["alloc pre 0.0 24883264", "alloc prejobs 0.0 192"]]);
    want.extend(new_class(0));
    want.extend(lines(&[&[
        "alloc jobs 0 448",
        "prejobs 0.0 x1",
        "k_half prejobs 0.0 nj=1 grid=3038",
        "jobs 0 x1",
        &tmp,
        &h,
        &v,
    ]]));
    let mut out = Vec::new();
    conv.add(k, src(0, 3840), dst(dw), &mut out).unwrap();
    // the class is the half-size one; its job reads the half frame
    let halfk = key(1920, 1080, dw, dh, Rows::All, Rows::All);
    assert_eq!(conv.class_id(&halfk), Some(ClassId(0)));
    conv.run(&mut out);
    assert_eq!(show(&out), want);
    let ConvOp::UploadJobs { jobs, .. } = &out[out.len() - 4] else {
        panic!("{:?}", out[out.len() - 4]);
    };
    let pre = ConvBuf::Pre {
        set: PreId(0),
        block: 0,
    };
    assert_eq!(jobs[0].sy, Plane::Conv { buf: pre, off: 0 });
    assert_eq!(
        jobs[0].suv,
        Plane::Conv {
            buf: pre,
            off: 1920 * 1080
        }
    );
    assert_eq!((jobs[0].sp_y, jobs[0].sp_uv), (1920, 1920));
    let ConvOp::UploadPreJobs { jobs: pj, .. } = &out[out.len() - 6] else {
        panic!("{:?}", out[out.len() - 6]);
    };
    assert_eq!(
        pj[0],
        PreJob {
            sy: src(0, 3840).y,
            suv: src(0, 3840).uv,
            sp_y: 3840,
            sp_uv: 3840
        }
    );
    assert_eq!(conv.pre_jobs(), 1);
    // the next tick reuses everything
    assert_eq!(
        tick(&mut conv, &[k]),
        lines(&[&[
            "prejobs 0.0 x1",
            "k_half prejobs 0.0 nj=1 grid=3038",
            "jobs 0 x1",
            &h,
            &v
        ]])
    );
    // without the pre-stage the source is resampled directly
    let mut direct = Converter::new();
    direct.fast_down = false;
    let shown = tick(&mut direct, &[k]);
    assert!(
        shown
            .iter()
            .all(|l| !l.contains("half") && !l.contains("pre"))
    );
    assert_eq!(direct.class_id(&k), Some(ClassId(0)));
}

#[test]
fn geom_2160p_720p_halved() {
    halved(1280, 720, 8294464, 1688, 1350);
}

#[test]
fn geom_2160p_480i_halved() {
    halved(704, 480, 4561984, 929, 495);
}

#[test]
fn geom_1080i_480i_fields() {
    // Interlaced to interlaced at another size: each field to its field, two classes, top first
    // (the key order). Top: th_y (1080 + 1) / 2 = 540, th_c 270; bottom: (1080 - 1 + 1) / 2 =
    // 540, 270. tstride 704 * 810 = 570240 (x4 + 64 = 2281024); vy.n 240, vc.n 120.
    // h: 176 * 540 + 88 * 270 = 118800 -> 465; v: 176 * 240 + 2 * 88 * 120 = 63360 -> 248.
    let mut conv = Converter::new();
    let top = key(1920, 1080, 704, 480, Rows::Top, Rows::Top);
    let bot = key(1920, 1080, 704, 480, Rows::Bottom, Rows::Bottom);
    let mut want = new_class(0);
    want.push("alloc jobs 0 448".into());
    want.extend(new_class(1));
    want.extend(lines(&[&[
        "alloc jobs 1 448",
        "jobs 0 x1",
        "alloc tmp 0 2281024",
        "k_conv_h 0 nj=1 grid=465",
        "k_conv_v 0 nj=1 grid=248",
        "jobs 1 x1",
        "alloc tmp 1 2281024",
        "k_conv_h 1 nj=1 grid=465",
        "k_conv_v 1 nj=1 grid=248",
    ]]));
    // The C++ harness added the top field's job first; the launch order is the class order anyway
    assert_eq!(tick(&mut conv, &[top, bot]), want);
    assert_eq!(
        tick(&mut conv, &[bot, top]),
        lines(&[&[
            "jobs 0 x1",
            "k_conv_h 0 nj=1 grid=465",
            "k_conv_v 0 nj=1 grid=248",
            "jobs 1 x1",
            "k_conv_h 1 nj=1 grid=465",
            "k_conv_v 1 nj=1 grid=248",
        ]])
    );
    let c = conv.class(ClassId(1)).unwrap();
    assert_eq!((c.s_step, c.s_off, c.d_step, c.d_off), (2, 1, 2, 1));
    assert_eq!((c.th_y, c.th_c), (540, 270));
}

#[test]
fn geom_480i_1080i_fields() {
    // th_y 240, th_c 120; tstride 1920 * 360 = 691200 (x4 + 64 = 2764864); vy.n 540, vc.n 270.
    // h: 480 * 240 + 240 * 120 = 144000 -> 563; v: 480 * 540 + 2 * 240 * 270 = 388800 -> 1519.
    let mut conv = Converter::new();
    let top = key(704, 480, 1920, 1080, Rows::Top, Rows::Top);
    let bot = key(704, 480, 1920, 1080, Rows::Bottom, Rows::Bottom);
    let shown = tick(&mut conv, &[top, bot]);
    assert_eq!(
        shown[shown.len() - 8..].to_vec(),
        lines(&[&[
            "jobs 0 x1",
            "alloc tmp 0 2764864",
            "k_conv_h 0 nj=1 grid=563",
            "k_conv_v 0 nj=1 grid=1519",
            "jobs 1 x1",
            "alloc tmp 1 2764864",
            "k_conv_h 1 nj=1 grid=563",
            "k_conv_v 1 nj=1 grid=1519",
        ]])
    );
}

#[test]
fn geom_1080i_720p_bob() {
    // 59.94p from 29.97i: one field per output frame, the two fields of a frame in turn (top
    // first: the source is top field first), each into all rows. Two classes, one per tick.
    // th_y 540, th_c 270; tstride 1280 * 810 = 1036800 (x4 + 64 = 4147264).
    // h: 320 * 540 + 160 * 270 = 216000 -> 844; v: 320 * 720 + 2 * 160 * 360 = 345600 -> 1350.
    let mut conv = Converter::new();
    let top = key(1920, 1080, 1280, 720, Rows::Top, Rows::All);
    let bot = key(1920, 1080, 1280, 720, Rows::Bottom, Rows::All);
    let run = |c: u32| {
        lines(&[&[
            &format!("jobs {c} x1"),
            &format!("k_conv_h {c} nj=1 grid=844"),
            &format!("k_conv_v {c} nj=1 grid=1350"),
        ]])
    };
    let first = |c: u32| {
        let mut w = new_class(c);
        w.push(format!("alloc jobs {c} 448"));
        let mut r = run(c);
        r.insert(1, format!("alloc tmp {c} 4147264"));
        w.extend(r);
        w
    };
    assert_eq!(tick(&mut conv, &[top]), first(0));
    assert_eq!(tick(&mut conv, &[bot]), first(1));
    assert_eq!(tick(&mut conv, &[top]), run(0));
    assert_eq!(tick(&mut conv, &[bot]), run(1));
    let c = conv.class(ClassId(1)).unwrap();
    assert_eq!((c.s_step, c.s_off, c.d_step, c.d_off), (2, 1, 1, 0));
}

#[test]
fn geom_1080i_1080p_bob() {
    // 29.97p from 29.97i: the top field of each frame into all rows, same size: still a
    // resampling class (rows differ), not a copy. tstride 1920 * 810 = 1555200 (x4 + 64 =
    // 6220864). h: 480 * 540 + 240 * 270 = 324000 -> 1266; v: 480 * 1080 + 2 * 240 * 540 =
    // 777600 -> 3038.
    one_class(
        key(1920, 1080, 1920, 1080, Rows::Top, Rows::All),
        6220864,
        1266,
        3038,
    );
}

#[test]
fn same_size_is_a_copy() {
    // (pipe-1080i-1080i-fields): (1920 / 4) * 1080 + (1920 / 8) * 540 = 648000 -> 2532 blocks.
    let mut conv = Converter::new();
    let k = key(1920, 1080, 1920, 1080, Rows::All, Rows::All);
    assert_eq!(
        tick(&mut conv, &[k]),
        lines(&[&[
            "alloc jobs 0 448",
            "jobs 0 x1",
            "k_conv_copy 0 nj=1 grid=2532"
        ]])
    );
    let c = conv.class(ClassId(0)).unwrap();
    assert!(c.copy && c.taps.is_none());
    assert_eq!((c.copy_w, c.copy_h), (1920, 1080));
    assert_eq!(conv.dev_bytes(), 0);
    // the low-pass flag alone makes it a resampling class
    let lp = ConvKey {
        weave_lp: true,
        ..k
    };
    assert!(!ConvClass::new(lp).unwrap().copy);
}

#[test]
fn letterbox_tick_order() {
    // pipe-1080p-480i-letterbox as the C++ harness queued it: the fill of the whole frame, the
    // conversion into 704x360 at row 60, the color matrix over that rectangle (BT.709 ->
    // BT.601), an overlay of the whole frame.
    let mut conv = Converter::new();
    let mut out = Vec::new();
    let frame = Dest {
        y: 0x9000_0000u64,
        u: 0xa000_0000,
        v: 0xb000_0000,
        pitch: 768,
    };
    let rect = Dest {
        y: frame.y + 60 * 768,
        u: frame.u + 30 * 384,
        v: frame.v + 30 * 384,
        pitch: 768,
    };
    let fill = RectJob {
        dst: frame,
        w: 704,
        h: 480,
        yuv: 16 | (128 << 8) | (128 << 16),
        overlay: None,
    };
    let k = key(1920, 1080, 704, 360, Rows::All, Rows::All);
    conv.add(k, src(0, 1920), rect, &mut out).unwrap();
    let setup = out.len();
    conv.fill(fill, &mut out);
    conv.run(&mut out);
    let m = crate::make_csc(true, false, false);
    let csc = RectJob {
        dst: rect,
        w: 704,
        h: 360,
        yuv: 0,
        overlay: None,
    };
    conv.csc(csc, m, true, &mut out);
    let ov = OverlayPlanes {
        oy: 1u64 << 32,
        ou: 2 << 32,
        ov: 3 << 32,
        oa: 4 << 32,
        oa2: 5 << 32,
    };
    conv.overlay(
        RectJob {
            overlay: Some(ov),
            ..fill
        },
        &mut out,
    );
    // fill and overlay: 176 * 480 + 2 * 88 * 240 = 126720 -> 495; csc: 88 * 180 = 15840 -> 62.
    // tstride 704 * 1620 = 1140480; h: 176 * 1080 + 88 * 540 -> 929; v: 176 * 360 + 2 * 88 *
    // 180 = 95040 -> 372.
    assert_eq!(
        show(&out[setup..]),
        lines(&[&[
            "k_fill 704x480 grid=495",
            "jobs 0 x1",
            "alloc tmp 0 4561984",
            "k_conv_h 0 nj=1 grid=929",
            "k_conv_v 0 nj=1 grid=372",
            "k_csc 704x360 grid=62",
            "k_overlay 704x480 grid=495",
        ]])
    );
    assert_eq!(conv.launches(), 5);
    let Some(ConvOp::Launch(ConvLaunch::Csc {
        interlaced, m: mm, ..
    })) = out.get(setup + 5)
    else {
        panic!("{:?}", out.get(setup + 5));
    };
    assert!(*interlaced);
    assert_eq!(*mm, m);
}

#[test]
fn job_lists_and_intermediates_grow() {
    // A mosaic of nine tiles of one class: the job list doubles from 8 to 16 (the C++ waits
    // for the stream before freeing the old one); the intermediate planes grow to
    // max(nj, old + 4) jobs.
    let mut conv = Converter::new();
    let k = key(1280, 720, 640, 360, Rows::All, Rows::All);
    let ts = u64::from(ConvClass::new(k).unwrap().tstride);
    let mut out = Vec::new();
    for n in 0..9 {
        conv.add(k, src(n, 1280), dst(640), &mut out).unwrap();
    }
    conv.run(&mut out);
    let shown = show(&out);
    let alloc: Vec<_> = shown
        .iter()
        .filter(|l| l.starts_with("alloc jobs") || l.starts_with("alloc tmp"))
        .cloned()
        .collect();
    assert_eq!(
        alloc,
        vec![
            "alloc jobs 0 448".to_string(),
            "alloc jobs 0 896 replaces".into(),
            format!("alloc tmp 0 {}", 9 * ts + 64),
        ]
    );
    assert!(shown.contains(&"jobs 0 x9".to_string()));
    let taps: u64 = {
        let c = conv.class(ClassId(0)).unwrap();
        let t = c.taps.as_ref().unwrap();
        Axis::ALL
            .iter()
            .map(|&a| {
                let h = t.get(a);
                u64::from(h.n) * 4 + u64::from(h.n) * u64::from(h.nt) * 2 + 128
            })
            .sum()
    };
    assert_eq!(conv.dev_bytes(), taps + 9 * ts);
    // ten jobs: 10 > 9 -> max(10, 13) = 13
    let mut out = Vec::new();
    for n in 0..10 {
        conv.add(k, src(n, 1280), dst(640), &mut out).unwrap();
    }
    conv.run(&mut out);
    let shown = show(&out);
    assert!(shown.contains(&format!("alloc tmp 0 {} replaces", 13 * ts + 64)));
    assert_eq!(conv.dev_bytes(), taps + 13 * ts);
}

#[test]
fn pre_stage_blocks_of_eight() {
    // Nine 2160p sources in one tick: two blocks of half frames, two k_half launches.
    let mut conv = Converter::new();
    let k = key(3840, 2160, 1280, 720, Rows::All, Rows::All);
    let mut out = Vec::new();
    for n in 0..9 {
        conv.add(k, src(n, 3840), dst(1280), &mut out).unwrap();
    }
    conv.run(&mut out);
    let shown = show(&out);
    let pre: Vec<_> = shown
        .iter()
        .filter(|l| l.contains("pre"))
        .cloned()
        .collect();
    assert_eq!(
        pre,
        lines(&[&[
            "alloc pre 0.0 24883264",
            "alloc prejobs 0.0 192",
            "alloc pre 0.1 24883264",
            "alloc prejobs 0.1 192",
            "prejobs 0.0 x8",
            "k_half prejobs 0.0 nj=8 grid=24300",
            "prejobs 0.1 x1",
            "k_half prejobs 0.1 nj=1 grid=3038",
        ]])
    );
    let ConvOp::UploadJobs { jobs, .. } = out
        .iter()
        .find(|o| matches!(o, ConvOp::UploadJobs { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        jobs[8].sy,
        Plane::Conv {
            buf: ConvBuf::Pre {
                set: PreId(0),
                block: 1
            },
            off: 0
        }
    );
    assert_eq!(
        jobs[7].sy,
        Plane::Conv {
            buf: ConvBuf::Pre {
                set: PreId(0),
                block: 0
            },
            off: 7 * 3110400
        }
    );
    // the blocks stay: the next tick allocates nothing
    let mut out = Vec::new();
    conv.add(k, src(0, 3840), dst(1280), &mut out).unwrap();
    assert!(out.is_empty());
}

#[test]
fn pre_stage_conditions() {
    let mut conv: Converter<u64> = Converter::new();
    let mut out = Vec::new();
    // a crop origin not a multiple of 4: resampled directly
    let k = ConvKey {
        cx: 2,
        cw: 3836,
        ..key(3840, 2160, 1280, 720, Rows::All, Rows::All)
    };
    conv.add(k, src(0, 3840), dst(1280), &mut out).unwrap();
    assert_eq!(conv.class_id(&k), Some(ClassId(0)));
    // a field source: resampled directly
    let f = key(3840, 2160, 1280, 720, Rows::Top, Rows::All);
    conv.add(f, src(0, 3840), dst(1280), &mut out).unwrap();
    assert_eq!(conv.class_id(&f), Some(ClassId(1)));
    // a ratio under 3 in one direction
    let r = key(3840, 2160, 1280, 728, Rows::All, Rows::All);
    conv.add(r, src(0, 3840), dst(1280), &mut out).unwrap();
    assert_eq!(conv.class_id(&r), Some(ClassId(2)));
    assert_eq!(conv.pre_jobs(), 0);
    // P010 at 2160p: halved to 8 bits (the half class has bps 1)
    let p = ConvKey {
        bps: 2,
        ..key(3840, 2160, 1280, 720, Rows::All, Rows::All)
    };
    conv.add(p, src(0, 7680), dst(1280), &mut out).unwrap();
    let half = key(1920, 1080, 1280, 720, Rows::All, Rows::All);
    assert_eq!(conv.class_id(&half), Some(ClassId(3)));
    conv.run(&mut out);
    let Some(ConvOp::Launch(ConvLaunch::Half { bps, .. })) = out
        .iter()
        .find(|o| matches!(o, ConvOp::Launch(ConvLaunch::Half { .. })))
    else {
        panic!("no k_half");
    };
    assert_eq!(*bps, 2);
}

#[test]
fn refusals_change_nothing() {
    let mut conv: Converter<u64> = Converter::new();
    let mut out = Vec::new();
    let bad = key(1920, 1080, 100, 720, Rows::All, Rows::All);
    assert_eq!(
        conv.add(bad, src(0, 1920), dst(104), &mut out),
        Err(ConvertError::BadRect { dw: 100, dh: 720 })
    );
    let odd = ConvKey {
        cy: 1,
        ch: 1078,
        ..key(1920, 1080, 1280, 720, Rows::All, Rows::All)
    };
    assert!(matches!(
        conv.add(odd, src(0, 1920), dst(1280), &mut out),
        Err(ConvertError::BadRect { .. })
    ));
    let k = key(1920, 1080, 1280, 720, Rows::All, Rows::All);
    let mut s = src(0, 1920);
    s.sp_uv = 1922;
    assert_eq!(
        conv.add(k, s, dst(1280), &mut out),
        Err(ConvertError::Unaligned)
    );
    let mut s = src(0, 1920);
    s.y += 2;
    assert_eq!(
        conv.add(k, s, dst(1280), &mut out),
        Err(ConvertError::Unaligned)
    );
    // a halved source whose half class is refused: no pre-stage job either
    let bad_half = ConvKey {
        dw: 1004,
        ..key(3840, 2160, 1000, 700, Rows::All, Rows::All)
    };
    assert!(
        conv.add(bad_half, src(0, 3840), dst(1008), &mut out)
            .is_err()
    );
    out.clear();
    let pre_before = conv.pre_jobs();
    conv.run(&mut out);
    assert_eq!(conv.pre_jobs(), pre_before);
    let mut fresh: Converter<u64> = Converter::new();
    assert!(
        fresh
            .add(bad_half, src(0, 3840), dst(1008), &mut out)
            .is_err()
    );
    assert_eq!(
        (fresh.classes(), fresh.pre_jobs(), fresh.dev_bytes()),
        (0, 0, 0)
    );
    let mut none = Vec::new();
    fresh.run(&mut none);
    assert!(none.is_empty());
}

#[test]
fn device_buffers_as_addresses() {
    let mut conv: Converter<DevBuf> = Converter::new();
    let mut out = Vec::new();
    let k = key(1280, 720, 1280, 720, Rows::All, Rows::All);
    let s = Source {
        y: DevBuf::new(4096, 1280 * 720),
        sp_y: 1280,
        uv: DevBuf::new(4096 + 1280 * 720, 1280 * 360),
        sp_uv: 1280,
    };
    let d = Dest {
        y: DevBuf::new(0x10_0000, 1312 * 720),
        u: DevBuf::new(0x20_0000, 656 * 360),
        v: DevBuf::new(0x30_0000, 656 * 360),
        pitch: 1312,
    };
    conv.add(k, s, d, &mut out).unwrap();
    let s2 = Source {
        y: DevBuf::new(4098, 16),
        ..s
    };
    assert_eq!(conv.add(k, s2, d, &mut out), Err(ConvertError::Unaligned));
}

#[test]
fn class_taps_match_the_golden_records() {
    // The `class` records: the four axes of a key as Converter::get builds them.
    let rows = |v: i64| match v {
        0 => Rows::All,
        1 => Rows::Top,
        _ => Rows::Bottom,
    };
    let mut n = 0;
    for r in tb_golden::case("taps").unwrap().iter() {
        if r.kind() != "taps" || r.text("set").unwrap() != "class" {
            continue;
        }
        let v = r.ints("key").unwrap();
        let u = |i: usize| u32::try_from(v[i]).unwrap();
        let k = ConvKey {
            sw: u(0),
            sh: u(1),
            bps: u(2),
            cx: u(3),
            cy: u(4),
            cw: u(5),
            ch: u(6),
            dw: u(7),
            dh: u(8),
            srows: rows(v[9]),
            drows: rows(v[10]),
            filter: match v[11] {
                0 => Filter::Bilinear,
                1 => Filter::Bicubic,
                _ => Filter::Lanczos3,
            },
            weave_lp: v[12] != 0,
        };
        let axis = match r.text("axis").unwrap() {
            "hy" => Axis::Hy,
            "hc" => Axis::Hc,
            "vy" => Axis::Vy,
            _ => Axis::Vc,
        };
        let (spec, pad4) = crate::taps::tests::spec_of(r);
        let i = Axis::ALL.iter().position(|&a| a == axis).unwrap();
        assert_eq!(ConvClass::specs(&k)[i], spec, "{k:?} {axis:?}");
        assert_eq!(pad4, axis == Axis::Hy || (axis == Axis::Hc && k.bps == 1));
        assert!(!r.flag("error").unwrap());
        let c = ConvClass::new(k).unwrap();
        let t = c.taps.as_ref().unwrap();
        assert_eq!(
            t.get(axis),
            &crate::taps::tests::taps_of(r),
            "{k:?} {axis:?}"
        );
        assert_eq!((c.th_y, c.th_c), (t.vy.n_lat, t.vc.n_lat));
        n += 1;
    }
    assert_eq!(n, 48);
}
