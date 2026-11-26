//! Paths of the rate control that the golden vectors do not reach (golden/README.md), tested
//! from the text of the C++ encoder (`Batch::quantize`, `retry`, `collect`,
//! `host_repeat`, `Encoder::open`): the all-skipped rung 37, an I picture replaced by a P
//! picture, the first picture of a stream held on rung 36, rungs past 37, `over_cap`, larger
//! ladder steps, the double step of a model that keeps under-correcting, a plan that starts on
//! the ladder, the scene-cut P picture, buffer violations, and the refusals of `open`.
#![allow(
    clippy::unwrap_used,
    reason = "test code: the helpers are not #[test] functions"
)]

use tb_kernel_abi::{PIC_B, PIC_I, PIC_P, PicStat, RowStat};
use tb_m2v_rate::control::{
    Pass, PicCtx, RUNG_KEEP, RUNG_SKIP, RateConfigError, coarser, first_min, first_picture_bits,
    keep_for_rung, min_rate,
};
use tb_m2v_rate::model::{QSCALE, nz_at};
use tb_m2v_rate::{RateControl, RateModel};

const NMB: u32 = 99;
const MBH: u32 = 9;

fn rc() -> RateControl {
    RateControl::new(300_000, 300_000, 1_835_008, NMB, MBH, 30.0).unwrap()
}

fn ctx(pic_type: u8, have_ref: bool) -> PicCtx {
    PicCtx {
        pic_type,
        key: pic_type == PIC_I,
        have_ref,
        nmb: NMB,
        fps: 30.0,
        rate: 300_000.0,
        gop: 6,
        m: 3,
        to_next_key: 6,
        gop_frames: 6.0,
        frames: 1.0,
        hdr_bits: 0.0,
        cap_bytes: 1 << 20,
        fixed_q: [6, 6, 8],
    }
}

/// Rows whose histogram makes the predicted size strictly decreasing in the quantizer.
fn rows() -> Vec<RowStat> {
    vec![
        RowStat {
            nz: [100; 16],
            ..RowStat::default()
        };
        MBH as usize
    ]
}

fn qs(c: u8) -> f64 {
    f64::from(QSCALE[usize::from(c)])
}

/// A pass on the ladder (`rung` > 0) that is over its limit by `ratio`.
fn ladder_pass(rc: &mut RateControl, c: &mut PicCtx, rung: u8, ratio: f64) -> Pass {
    rc.cur.rung = rung;
    rc.cur.tries = 1;
    rc.cur.target = 1000.0;
    rc.cur.limit = 8000.0;
    rc.cur.hdr_bits = 0.0;
    let bytes = (ratio * 8000.0 / 8.0) as u32 + 1;
    let mut rowq = vec![31; MBH as usize];
    rc.after_pass(
        c,
        &rows(),
        PicStat {
            bytes,
            tex_bits: 100,
            ..PicStat::default()
        },
        &mut rowq,
    )
}

#[test]
fn ladder_steps() {
    // (rung before, bits / limit, rung after): `rung == 31 ? 32 : rung >= 36 ? rung + 1 :
    // min(36, rung + step)`, step 3 above 2.5, 2 above 1.5, else 1.
    let table: &[(u8, f64, u8)] = &[
        (31, 1.1, 32),
        (31, 3.0, 32),
        (32, 1.1, 33),
        (32, 1.6, 34),
        (32, 2.6, 35),
        (33, 2.6, 36),
        (34, 2.6, 36),
        (35, 1.6, 36),
    ];
    for &(from, ratio, to) in table {
        let mut r = rc();
        let mut c = ctx(PIC_P, true);
        let out = ladder_pass(&mut r, &mut c, from, ratio);
        assert_eq!(r.cur.rung, to, "{from} {ratio}");
        assert_eq!(
            out,
            Pass::Again {
                keep: keep_for_rung(to),
                all_skipped: false,
                i_becomes_p: false
            },
            "{from} {ratio}"
        );
        assert_eq!(r.cur.tries, 2);
        assert_eq!(r.stats.retries, 1);
    }
    assert_eq!(keep_for_rung(31), 64);
    assert_eq!(keep_for_rung(0), 64);
    for (i, &k) in RUNG_KEEP[..5].iter().enumerate() {
        assert_eq!(keep_for_rung(32 + i as u8), k);
    }
    assert_eq!(keep_for_rung(RUNG_SKIP), 0);
}

#[test]
fn rung_36_to_all_skipped() {
    // P and B pictures: rung 37, keep 0, transform again with zero vectors.
    for t in [PIC_P, PIC_B] {
        let mut r = rc();
        let mut c = ctx(t, true);
        let out = ladder_pass(&mut r, &mut c, 36, 3.0);
        assert_eq!(r.cur.rung, RUNG_SKIP);
        assert_eq!(
            out,
            Pass::Again {
                keep: 0,
                all_skipped: true,
                i_becomes_p: false
            }
        );
        assert_eq!(c, ctx(t, true));
        assert_eq!((r.stats.isub, r.stats.violations), (0, 0));
    }
}

#[test]
fn i_becomes_p() {
    let mut r = rc();
    let mut c = ctx(PIC_I, true);
    let mdl = r.mdl;
    let out = ladder_pass(&mut r, &mut c, 36, 1.1);
    assert_eq!(
        out,
        Pass::Again {
            keep: 0,
            all_skipped: true,
            i_becomes_p: true
        }
    );
    assert_eq!(r.cur.rung, RUNG_SKIP);
    assert_eq!(c.pic_type, PIC_P);
    assert!(!c.key);
    assert_eq!(r.stats.isub, 1);
    assert_eq!(r.stats.violations, 0);
    assert_eq!(r.cur.tries, 2);
    assert_eq!(r.mdl, mdl, "the model is restored on the ladder");

    // The all-skipped P picture still does not fit: rung 37 stays, one violation, accepted.
    let out = ladder_pass(&mut r, &mut c, RUNG_SKIP, 1.1);
    assert_eq!(out, Pass::Accept);
    assert_eq!(r.cur.rung, RUNG_SKIP);
    assert_eq!(r.stats.violations, 1);
    assert_eq!(r.stats.isub, 1);
    assert_eq!(r.keep(), 0);
    assert_eq!(r.cur.tries, 1, "an accepted pass does not count");

    // It fits: accepted as it is.
    let out = ladder_pass(&mut r, &mut c, RUNG_SKIP, 0.5);
    assert_eq!(out, Pass::Accept);
    assert_eq!(r.stats.violations, 1);
}

#[test]
fn first_picture_stays_on_rung_36() {
    let mut r = rc();
    let mut c = ctx(PIC_I, false);
    let out = ladder_pass(&mut r, &mut c, 36, 3.0);
    assert_eq!(out, Pass::Accept);
    assert_eq!(r.cur.rung, 36);
    assert_eq!(r.keep(), 0);
    assert_eq!(r.stats.violations, 1);
    assert_eq!(r.stats.isub, 0);
    assert_eq!(r.cur.tries, 1);
    assert_eq!(c, ctx(PIC_I, false));
}

/// A first pass (rung 0) with the model theta 1, hdr 0, so that the predicted size of the
/// slices is `nz_at(total, quantiser_scale)`.
fn first_pass(
    code0: u8,
    frac0: f64,
    tries: u32,
    target: f64,
    limit: f64,
    pic: PicStat,
    cap_bytes: u32,
) -> (RateControl, Pass, Vec<u8>) {
    let mut r = rc();
    let mut c = ctx(PIC_P, true);
    c.cap_bytes = cap_bytes;
    r.mdl[usize::from(PIC_P)] = RateModel {
        theta: 1.0,
        hdr: 0.0,
    };
    r.cur.code = code0;
    r.cur.frac = frac0;
    r.cur.tries = tries;
    r.cur.target = target;
    r.cur.limit = limit;
    r.cur.hdr_bits = 0.0;
    let mut rowq = vec![code0; MBH as usize];
    let out = r.after_pass(&mut c, &rows(), pic, &mut rowq);
    (r, out, rowq)
}

fn total() -> RowStat {
    let mut t = RowStat::default();
    for r in rows() {
        for i in 0..16 {
            t.nz[i] += r.nz[i];
        }
    }
    t
}

#[test]
fn under_correcting_model_takes_a_double_step() {
    // choose() lands exactly on code 11 (frac 0): coarser than code 10, but the third pass
    // (tries >= 2) jumps to min(31, code0 + 2) = 12 everywhere.
    let tgt = nz_at(&total(), qs(11));
    let pic = PicStat {
        bytes: (2.5 * tgt / 8.0) as u32,
        tex_bits: 0,
        ..PicStat::default()
    };
    let (r, out, rowq) = first_pass(10, 0.0, 2, tgt, 2.0 * tgt, pic, 1 << 20);
    assert_eq!(
        out,
        Pass::Again {
            keep: 64,
            all_skipped: false,
            i_becomes_p: false
        }
    );
    assert_eq!((r.cur.code, r.cur.frac, r.cur.rung), (12, 0.0, 0));
    assert_eq!(rowq, vec![12; MBH as usize]);
    // With fewer tries the chosen code stands.
    let (r, _, rowq) = first_pass(10, 0.0, 1, tgt, 2.0 * tgt, pic, 1 << 20);
    assert_eq!((r.cur.code, r.cur.frac), (11, 0.0));
    assert_eq!(rowq, vec![11; MBH as usize]);
}

#[test]
fn ladder_entry() {
    // Even code 31 is predicted not to fit: code 31 everywhere, rung 31 first...
    let pic = PicStat {
        bytes: 1000,
        tex_bits: 0,
        ..PicStat::default()
    };
    let (r, out, rowq) = first_pass(29, 0.5, 0, 10.0, 100.0, pic, 1 << 20);
    assert_eq!(
        out,
        Pass::Again {
            keep: 64,
            all_skipped: false,
            i_becomes_p: false
        }
    );
    assert_eq!((r.cur.code, r.cur.frac, r.cur.rung), (31, 0.0, 31));
    assert_eq!(rowq, vec![31; MBH as usize]);
    // ...rung 32 directly when the pass was already plain code 31.
    let (r, out, _) = first_pass(31, 0.0, 0, 10.0, 100.0, pic, 1 << 20);
    assert_eq!(
        out,
        Pass::Again {
            keep: 12,
            all_skipped: false,
            i_becomes_p: false
        }
    );
    assert_eq!(r.cur.rung, 32);
}

#[test]
fn over_cap_forces_a_pass() {
    // Under the limit and on target, but the slices do not fit the output buffer
    // (bytes + 64 > cap_bytes): must, and nothing coarser than code 31 frac 0: rung 32.
    let tgt = nz_at(&total(), qs(31));
    let pic = PicStat {
        bytes: (tgt / 8.0) as u32,
        tex_bits: 0,
        ..PicStat::default()
    };
    let cap = pic.bytes + 63;
    let (r, out, _) = first_pass(31, 0.0, 0, tgt, 1e9, pic, cap);
    assert_eq!(r.cur.rung, 32);
    assert!(matches!(out, Pass::Again { keep: 12, .. }));
    // One byte more room: accepted.
    let (r, out, _) = first_pass(31, 0.0, 0, tgt, 1e9, pic, cap + 1);
    assert_eq!(out, Pass::Accept);
    assert_eq!(r.cur.rung, 0);
}

#[test]
fn plan_starts_on_the_ladder() {
    // choose() fails and the last picture of the type ended on rung r >= 32: start on
    // min(r, 36) with its keep; rung 37 is never a starting point.
    for (last, rung, keep) in [(32, 32, 12), (35, 35, 1), (36, 36, 0), (37, 36, 0)] {
        let mut r = rc();
        r.begin_picture(0.0, None, PIC_P, 0);
        r.last_rung[usize::from(PIC_P)] = last;
        r.mdl[usize::from(PIC_P)] = RateModel {
            theta: 1e6,
            hdr: 0.0,
        };
        let mut rowq = vec![0; MBH as usize];
        let k = r.plan_picture(&ctx(PIC_P, true), &rows(), &mut rowq);
        assert_eq!((r.cur.rung, k), (rung, keep), "{last}");
        assert_eq!(rowq, vec![31; MBH as usize]);
    }
    // choose() succeeds: no ladder, whatever the memory says.
    let mut r = rc();
    r.begin_picture(0.0, None, PIC_P, 0);
    r.last_rung[usize::from(PIC_P)] = 36;
    let mut rowq = vec![0; MBH as usize];
    let k = r.plan_picture(&ctx(PIC_P, true), &[RowStat::default(); 9], &mut rowq);
    assert_eq!((r.cur.rung, k), (0, 64));
}

#[test]
fn scene_cut_p_takes_an_i_share_from_the_buffer() {
    let plan = |intra: u32| {
        let mut r = rc();
        r.begin_picture(0.0, None, PIC_I, 0);
        // A GOP in progress: budget and pictures left as after the key frame.
        r.r = 20_000.0;
        r.left = [0, 0, 1, 3];
        let mut rows = rows();
        for row in &mut rows {
            row.n_intra = intra;
        }
        let mut c = ctx(PIC_P, true);
        c.key = false;
        let mut rowq = vec![0; MBH as usize];
        r.plan_picture(&c, &rows, &mut rowq);
        r
    };
    // 9 rows x 5 = 45 intra macroblocks of 99: no.
    let normal = plan(5);
    assert_eq!(normal.r, 20_000.0);
    // 9 x 6 = 54 > 99 / 2: the target becomes min(0.4 room, rate gop T / (1 + ...)) and the
    // GOP budget grows by the difference.
    let cut = plan(6);
    let room = cut.cur.cap - cut.cur.e_before;
    let xi = cut.x[1];
    let xp = cut.x[2];
    let xb = cut.x[3];
    let ti = (0.4 * room)
        .min(300_000.0 * 6.0 * (1.0 / 30.0) / (1.0 + 1.0 * xp / xi + 3.0 * xb / (xi * 1.4)));
    // The P picture's share: R / (max(Np, 1) + Nb Kp Xb / (Kb Xp)).
    let tt = 20_000.0 / (1.0 + 3.0 * 1.0 * xb / (1.4 * xp));
    assert_eq!(normal.cur.target, tt.max(10_000.0));
    assert!(ti > tt && ti > 10_000.0 && ti < 0.85 * room, "{ti} {tt}");
    assert_eq!(cut.cur.target, ti);
    assert_eq!(cut.r, 20_000.0 + (ti - tt));
}

#[test]
fn finish_on_the_ladder() {
    // (rung, slice bytes, last_rung after): the next picture of the type starts on the same
    // rung, or one easier when this one used less than 60 % of its limit.
    for (rung, bytes, last) in [
        (32u8, 1000u32, 32u8),
        (32, 10, 0),
        (35, 1000, 35),
        (35, 10, 34),
        (RUNG_SKIP, 1000, RUNG_SKIP),
        (RUNG_SKIP, 10, 36),
    ] {
        let mut r = rc();
        r.begin_picture(0.0, None, PIC_P, 0);
        r.cur.rung = rung;
        r.cur.tries = 3;
        r.cur.limit = 10_000.0;
        r.cur.hdr_bits = 0.0;
        let mdl = r.mdl;
        let rowq = vec![31; MBH as usize];
        let pic = PicStat {
            bytes,
            tex_bits: 0,
            ..PicStat::default()
        };
        let res = r.finish_picture(
            &ctx(PIC_P, true),
            pic,
            8.0 * f64::from(bytes) + 400.0,
            &rowq,
        );
        let q = 112.0 * f64::from(1 + i32::from(rung) - 31);
        assert_eq!(res.avg_q, q);
        assert_eq!(r.last_qs, q);
        assert_eq!(res.rung, rung);
        assert_eq!(res.tries, 3);
        assert_eq!(r.last_rung[usize::from(PIC_P)], last, "{rung} {bytes}");
        assert_eq!(r.mdl, mdl, "no model update on the ladder");
        assert_eq!(
            r.x[usize::from(PIC_P)],
            (8.0 * f64::from(bytes) + 400.0) * q
        );
        assert_eq!(r.floor_hist[usize::from(PIC_P)], 31);
        assert_eq!(r.stats.rung_pics, 1);
        assert_eq!(r.stats.skip_pics, u64::from(rung == RUNG_SKIP));
    }
}

#[test]
fn violations_count_pictures_over_the_room() {
    let mut r = rc();
    r.begin_picture(0.0, None, PIC_I, 0);
    let room = r.cur.cap - r.cur.e_before;
    let rowq = vec![10; MBH as usize];
    let pic = PicStat {
        bytes: 0,
        tex_bits: 0,
        ..PicStat::default()
    };
    // bits > cap - e_before + 0.5
    r.finish_picture(&ctx(PIC_I, false), pic, room + 0.5, &rowq);
    assert_eq!(r.stats.violations, 0);
    let mut r2 = rc();
    r2.begin_picture(0.0, None, PIC_I, 0);
    let res = r2.finish_picture(&ctx(PIC_I, false), pic, room + 1.0, &rowq);
    assert_eq!(r2.stats.violations, 1);
    assert_eq!(res.room_bits, -1.0);
    assert_eq!(r2.stats.room_min, -1.0);
    // A repeat counts the same way and takes one picture of its position type.
    let mut r3 = rc();
    r3.left = [0, 1, 2, 3];
    let res = r3.repeat_picture(0.0, 1e9, PIC_B);
    assert_eq!(r3.stats.violations, 1);
    assert_eq!(r3.left, [0, 1, 2, 2]);
    assert_eq!((res.rung, res.tries, res.avg_q), (RUNG_SKIP, 0, 0.0));
}

#[test]
fn open_refusals() {
    // min_rate: 2 * 8 * (mbh * 10 + 185) * fps, truncated, + 1.
    assert_eq!(min_rate(9, 30.0), (16.0 * 275.0 * 30.0) as u64 + 1);
    assert_eq!(
        RateControl::new(300_000, 300_000, 16383, NMB, MBH, 30.0),
        Err(RateConfigError::NoBuffer)
    );
    // first_min: the DC-only first picture (125 bits a macroblock, 45 a row, 185 bytes of
    // headers) and the 64 bits kept free must fit what the mux delivers in D seconds.
    assert_eq!(first_picture_bits(NMB, MBH), 125 * 99 + 45 * 9 + 1480);
    let need = (first_picture_bits(NMB, MBH) + 64) as f64;
    let min = (need * 300_000.0 / 16384.0).ceil() as u64;
    assert_eq!(first_min(NMB, MBH, 300_000, 16384), min);
    assert!(min > min_rate(MBH, 30.0));
    assert_eq!(
        RateControl::new(min - 1, 300_000, 16384, NMB, MBH, 30.0),
        Err(RateConfigError::RateOutOfRange {
            rate: min - 1,
            min,
            max: 300_000
        })
    );
    let ok = RateControl::new(min, 300_000, 16384, NMB, MBH, 30.0).unwrap();
    // at that rate the room of the empty buffer holds the first picture
    assert!(ok.vbv.cap() - 64.0 >= first_picture_bits(NMB, MBH) as f64);
    assert!(RateControl::new(300_001, 300_000, 1 << 20, NMB, MBH, 30.0).is_err());
    // Fixed quantizer: the same checks at the ceiling rate, and a buffer model.
    assert_eq!(
        RateControl::new(0, 0, 0, NMB, MBH, 30.0),
        Err(RateConfigError::NoBuffer)
    );
    let low = min_rate(MBH, 30.0) - 1;
    assert_eq!(
        RateControl::new(0, low, 1_835_008, NMB, MBH, 30.0),
        Err(RateConfigError::RateOutOfRange {
            rate: low,
            min: low + 1,
            max: low
        })
    );
    let f = RateControl::new(0, 15_000_000, 1_835_008, NMB, MBH, 30.0).unwrap();
    assert!(!f.cbr);
    assert_eq!(f.x, [0.0; 4]);
    assert_eq!(f.vbv.d, 1_835_008.0 / 15_000_000.0);
    assert_eq!(f.vbv.rate(), 15_000_000.0);
}

/// A fixed-quantizer session: the quantizer stands while the picture fits the buffer; a
/// picture that does not fit is quantized again with every row at the code that doubles its
/// quantiser_scale, then up the ladder, and the buffer model counts every picture. Nothing is
/// accepted over the limit (the C++ accepted every pass and cut what did not fit the output
/// buffer).
#[test]
fn a_fixed_quantizer_picture_that_does_not_fit_gets_coarser() {
    let mut r = RateControl::new(0, 400_000, 196_608, NMB, MBH, 30.0).unwrap();
    let mut c = ctx(PIC_I, false);
    c.fixed_q = [2, 3, 4];
    let par = r.begin_picture(0.0, None, PIC_I, 2);
    assert_eq!(par.qfloor, 2);
    let mut rowq = vec![0u8; MBH as usize];
    assert_eq!(r.plan_picture(&c, &rows(), &mut rowq), 64);
    assert_eq!(rowq, vec![2; MBH as usize]);
    // the room of an empty buffer: what the mux delivers in D seconds
    let room = 196_608.0;
    assert!(
        (r.cur.limit - (room - 64.0)).abs() < 1e-6,
        "{}",
        r.cur.limit
    );
    // fits: accepted, whatever a rate target would say
    let fits = PicStat {
        bytes: 20_000,
        ..PicStat::default()
    };
    assert_eq!(
        r.clone().after_pass(&mut c, &rows(), fits, &mut rowq),
        Pass::Accept
    );
    // does not fit: 2 -> 4 -> 8 -> 12 -> 18 -> 25 -> 31, then the ladder
    let big = PicStat {
        bytes: 30_000,
        ..PicStat::default()
    };
    for want in [4u8, 8, 12, 18, 25, 31] {
        let out = r.after_pass(&mut c, &rows(), big, &mut rowq);
        assert_eq!(
            out,
            Pass::Again {
                keep: 64,
                all_skipped: false,
                i_becomes_p: false
            }
        );
        assert_eq!(rowq, vec![want; MBH as usize]);
        assert_eq!(r.cur.rung, 0);
    }
    let out = r.after_pass(&mut c, &rows(), big, &mut rowq);
    assert_eq!(r.cur.rung, 32);
    assert_eq!(
        out,
        Pass::Again {
            keep: keep_for_rung(32),
            all_skipped: false,
            i_becomes_p: false
        }
    );
    // over the output buffer alone is refused as well
    let mut r2 = RateControl::new(0, 400_000, 196_608, NMB, MBH, 30.0).unwrap();
    r2.begin_picture(0.0, None, PIC_I, 2);
    let mut rowq = vec![0u8; MBH as usize];
    r2.plan_picture(&c, &rows(), &mut rowq);
    c.cap_bytes = 1000;
    let over_cap = PicStat {
        bytes: 990,
        ..PicStat::default()
    };
    assert!(matches!(
        r2.after_pass(&mut c, &rows(), over_cap, &mut rowq),
        Pass::Again { .. }
    ));
    // the buffer counts the picture, and the next picture has less room
    let res = r.finish_picture(&c, fits, 160_000.0, &rowq);
    assert_eq!(res.queue_bits, 160_000.0);
    assert_eq!(r.stats.violations, 0);
    r.begin_picture(1.0 / 30.0, None, PIC_P, 3);
    c.pic_type = PIC_P;
    r.plan_picture(&c, &rows(), &mut rowq);
    let left = 160_000.0 - 400_000.0 / 30.0;
    assert!((r.cur.e_before - left).abs() < 1e-6);
    assert!((r.cur.limit - (room - left - 64.0)).abs() < 1e-6);
}

/// A `PicStat` of `bytes` bytes whose rows `big` break the macroblock size rule.
fn pic_with_big_rows(bytes: u32, big: &[usize]) -> PicStat {
    let mut p = PicStat {
        bytes,
        tex_bits: 0,
        ..PicStat::default()
    };
    for r in big {
        p.big_rows[r / 32] |= 1 << (r % 32);
    }
    p
}

/// The step by which a row is made coarser: the next code whose quantiser_scale is at least
/// double (Table 7-6, q_scale_type 1), 31 at the end.
#[test]
fn coarser_doubles_the_scale() {
    for c in 1..=31u8 {
        let n = coarser(c);
        assert!(n > c || c == 31);
        assert!(qs(n) >= 2.0 * qs(c) || n == 31, "{c} -> {n}");
        if n > c + 1 {
            assert!(
                qs(n - 1) < 2.0 * qs(c),
                "{c} -> {n} is the first that doubles"
            );
        }
    }
    assert_eq!(
        [1, 2, 4, 8, 16, 24, 31].map(coarser),
        [2, 4, 8, 12, 22, 31, 31]
    );
}

/// H.262 Table 8-6: a pass with a row of more than two macroblocks above 4608 bits is never
/// accepted, although its size is on target. Only the rows named get coarser (to the code that
/// doubles their quantiser_scale), the floor holds for the rest of the picture, and nothing
/// else of the choice changes.
#[test]
fn a_row_above_the_macroblock_limit_is_quantized_again() {
    let tgt = 8000.0;
    let on_target = (tgt / 8.0) as u32;
    // without big rows this pass is accepted
    let (_, out, _) = first_pass(
        3,
        0.0,
        0,
        tgt,
        2.0 * tgt,
        pic_with_big_rows(on_target, &[]),
        1 << 20,
    );
    assert_eq!(out, Pass::Accept);
    let pic = pic_with_big_rows(on_target, &[1, 2]);
    assert!(pic.any_big_row() && pic.big_row(1) && pic.big_row(2) && !pic.big_row(0));
    let (mut r, out, mut rowq) = first_pass(3, 0.0, 0, tgt, 2.0 * tgt, pic, 1 << 20);
    assert_eq!(
        out,
        Pass::Again {
            keep: 64,
            all_skipped: false,
            i_becomes_p: false
        }
    );
    let mut want = vec![3u8; MBH as usize];
    want[1] = 6;
    want[2] = 6;
    assert_eq!(rowq, want);
    assert_eq!(
        (r.cur.code, r.cur.frac, r.cur.rung, r.cur.tries),
        (3, 0.0, 0, 1)
    );
    assert_eq!((r.stats.mbfix, r.stats.retries), (1, 1));
    // the next pass: row 2 still breaks the rule and the picture is now over its limit as
    // well; whatever the model chooses, rows 1 and 2 stay at or above their floors
    let mut c = ctx(PIC_P, true);
    let over = pic_with_big_rows((2.5 * tgt / 8.0) as u32, &[2]);
    let out = r.after_pass(&mut c, &rows(), over, &mut rowq);
    assert!(matches!(out, Pass::Again { .. }));
    assert_eq!(r.cur.rung, 0);
    assert!(rowq[1] >= 6 && rowq[2] >= 10, "{rowq:?}");
    assert_eq!(r.row_floor[1], 6);
    assert_eq!(r.row_floor[2], 10);
    assert_eq!(r.stats.mbfix, 2);
    // clean now: accepted
    let fits = pic_with_big_rows(on_target, &[]);
    assert_eq!(r.after_pass(&mut c, &rows(), fits, &mut rowq), Pass::Accept);
    // the floors last for one picture
    r.begin_picture(1.0, None, PIC_P, 0);
    assert!(r.row_floor.iter().all(|f| *f == 0));
}

/// A row at code 31 that still breaks the rule sends the picture up the ladder (rung 32 keeps
/// 12 coefficients per block: at most 72 x 24 bits a macroblock), and on the ladder the rule
/// climbs like a picture that does not fit.
#[test]
fn a_row_at_the_coarsest_code_enters_the_ladder() {
    let pic = pic_with_big_rows(100, &[0]);
    let (r, out, rowq) = first_pass(31, 0.0, 0, 8000.0, 16000.0, pic, 1 << 20);
    assert_eq!(
        out,
        Pass::Again {
            keep: keep_for_rung(32),
            all_skipped: false,
            i_becomes_p: false
        }
    );
    assert_eq!(r.cur.rung, 32);
    assert!(rowq.iter().all(|q| *q == 31));
    let mut r = rc();
    let mut c = ctx(PIC_P, true);
    r.cur.rung = 32;
    r.cur.tries = 1;
    r.cur.target = 1000.0;
    r.cur.limit = 8000.0;
    let mut rowq = vec![31; MBH as usize];
    let out = r.after_pass(&mut c, &rows(), pic_with_big_rows(10, &[3]), &mut rowq);
    assert!(matches!(out, Pass::Again { .. }));
    assert_eq!(r.cur.rung, 33);
}

/// A correction that would end with the same code and nearly the same fraction is not made:
/// the pass is accepted, and what describes it stays as the pass had it. (Ported bug: the C++
/// accepted the pass after `choose` had already rewritten the row codes, `frac` and `nzpred`,
/// so the model update and the mean quantizer of the picture used values of a correction that
/// was never coded.)
#[test]
fn a_correction_not_worth_a_pass_leaves_the_pass_as_it_was() {
    let tgt = 6000.0;
    // what the model (theta 1, hdr 0) chooses for the target
    let mut probe = rc();
    probe.mdl[usize::from(PIC_P)] = RateModel {
        theta: 1.0,
        hdr: 0.0,
    };
    let mut chosen = vec![0u8; MBH as usize];
    assert!(probe.choose(PIC_P, tgt, &rows(), &mut chosen));
    let (code0, frac0) = (probe.cur.code, probe.cur.frac);
    assert!(frac0 > 0.1 && frac0 < 0.9, "{frac0}: the rows must differ");
    assert!(chosen.iter().any(|q| *q != code0));
    // the pass was coded with every row at code0 and came out 40 % over the target: a
    // correction is wanted, the model has nothing new (no texture bits), and it chooses the
    // same code and fraction again
    let mut r = rc();
    let mut c = ctx(PIC_P, true);
    r.mdl[usize::from(PIC_P)] = RateModel {
        theta: 1.0,
        hdr: 0.0,
    };
    r.cur.code = code0;
    r.cur.frac = frac0;
    r.cur.nzpred = 12_345.0;
    r.cur.target = tgt;
    r.cur.limit = 4.0 * tgt;
    r.cur.hdr_bits = 0.0;
    let coded = vec![code0; MBH as usize];
    let mut rowq = coded.clone();
    let pic = PicStat {
        bytes: (1.4 * tgt / 8.0) as u32,
        tex_bits: 0,
        ..PicStat::default()
    };
    assert_eq!(r.after_pass(&mut c, &rows(), pic, &mut rowq), Pass::Accept);
    assert_eq!(rowq, coded, "the row codes of the pass that stands");
    assert_eq!(
        (r.cur.code, r.cur.frac, r.cur.nzpred),
        (code0, frac0, 12_345.0)
    );
    assert_eq!((r.cur.tries, r.stats.retries), (0, 0));
}
