//! Work package E2: batch geometry and layout against the golden `batch` / `open` records and
//! the invariants of the arena layout.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

use tb_device::{DevBuf, HostBuf};
use tb_golden::Record;
use tb_kernel_abi::Geo;
use tb_m2v_encoder::SessionConfig;
use tb_m2v_encoder::layout::{BatchLayout, GUARD, LayoutError, geometry};

/// The configuration of an `open` record, as the C++ `SessionConfig`.
fn config(o: &Record) -> SessionConfig {
    let int = |f: &str| o.int(f).unwrap();
    let alt = int("alt_scan");
    SessionConfig {
        w: int("w").try_into().unwrap(),
        h: int("h").try_into().unwrap(),
        fps_n: int("fps_n").try_into().unwrap(),
        fps_d: int("fps_d").try_into().unwrap(),
        interlaced: o.flag("interlaced").unwrap(),
        tff: o.flag("tff").unwrap(),
        gop: int("gop").try_into().unwrap(),
        m: int("m").try_into().unwrap(),
        gop_phase: int("gop_phase").try_into().unwrap(),
        max_bitrate: int("max_bitrate").try_into().unwrap(),
        vbv_bits: int("vbv_bits").try_into().unwrap(),
        bitrate: int("bitrate").try_into().unwrap(),
        fixed_q: [
            int("qi").try_into().unwrap(),
            int("qp").try_into().unwrap(),
            int("qb").try_into().unwrap(),
        ],
        aq: o.flag("aq").unwrap(),
        aq_strength: o.num("aq_strength").unwrap() as f32,
        field_pred: o.flag("field_pred").unwrap(),
        field_dct: o.flag("field_dct").unwrap(),
        range2: int("range2").try_into().unwrap(),
        lambda: int("lambda").try_into().unwrap(),
        field_bits: int("field_bits").try_into().unwrap(),
        smooth: o.flag("smooth").unwrap(),
        tpred: o.flag("tpred").unwrap(),
        intra_vlc: o.flag("intra_vlc").unwrap(),
        alt_scan: (alt >= 0).then_some(alt != 0),
        capacity: int("capacity").try_into().unwrap(),
        priority: int("priority").try_into().unwrap(),
        aspect: int("aspect").try_into().unwrap(),
        profile: tb_m2v_syntax::profile::Profile::from_name(o.text("profile").unwrap()).unwrap(),
        color: match o.ints("color").unwrap().as_slice() {
            [p, t, m] => Some(tb_m2v_syntax::profile::Color {
                primaries: *p as u8,
                transfer: *t as u8,
                matrix: *m as u8,
            }),
            _ => None,
        },
    }
}

#[test]
fn batch_records_match() {
    let mut checked = 0;
    for name in tb_golden::case_names() {
        let recs = tb_golden::case(name).unwrap();
        for b in recs.iter().filter(|r| r.kind() == "batch") {
            let id = b.int("b").unwrap();
            let o = recs
                .iter()
                .find(|r| r.kind() == "open" && r.int("b").unwrap() == id)
                .expect("an open record for the batch");
            let c = config(o);
            let cap: u32 = b.int("capacity").unwrap().try_into().unwrap();
            let keep_recon = b.flag("keep_recon").unwrap();
            // The Rust recorder's batches are laid out in the arena (the C++ encoder's cases ran
            // without it and recorded the sum of unrounded allocations; the hand-over of X1,
            // docs/port/X1-notes.md, lists the difference)
            assert!(b.flag("arena").unwrap(), "{name}: a batch in the arena");

            let want_geo: Vec<Geo> = b.pods("geo").unwrap();
            assert_eq!(want_geo.len(), 1);
            let (g, row_bs) = geometry(&c, cap).unwrap();
            assert_eq!(g, want_geo[0], "{name}: geo");
            assert_eq!(bytemuck::bytes_of(&g), b.bytes("geo").unwrap(), "{name}");
            assert_eq!(
                i64::from(row_bs),
                b.int("row_bs").unwrap(),
                "{name}: row_bs"
            );

            let l = BatchLayout::new(&c, cap, keep_recon).unwrap();
            assert_eq!(l.geo, g);
            assert_eq!(l.row_bs, row_bs);
            assert_eq!(
                i64::from(l.cap_words),
                b.int("cap_words").unwrap(),
                "{name}"
            );
            assert_eq!(l.fbytes(), b.int("fbytes").unwrap() as u64, "{name}");
            assert_eq!(
                l.dev_bytes,
                b.int("dev_bytes").unwrap() as u64,
                "{name}: dev_bytes"
            );
            check_invariants(&l);
            checked += 1;
        }
    }
    // one per golden case, the two-format case two, the churn case four (batches made and
    // given back at run time)
    assert_eq!(checked, 16);
}

fn cfg(w: u32, h: u32, interlaced: bool) -> SessionConfig {
    SessionConfig {
        w,
        h,
        interlaced,
        ..SessionConfig::default()
    }
}

fn overlap(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Every invariant of a layout at offset 0.
fn check_invariants(l: &BatchLayout) {
    let dev = l.dev_allocs();
    let host = l.host_allocs();
    // Device: guard on both sides, 256-aligned allocation starts, in order, no overlap.
    let mut end = 0;
    for d in &dev {
        assert_eq!((d.off - GUARD) % 256, 0, "{d:?}");
        assert!(
            d.off - GUARD >= end,
            "{d:?} overlaps the guard of the previous buffer"
        );
        end = d.end() + GUARD;
    }
    assert!(end <= l.dev_bytes);
    for (i, a) in dev.iter().enumerate() {
        for b in &dev[i + 1..] {
            assert!(!overlap((a.off - GUARD, a.end() + GUARD), (b.off, b.end())));
            assert!(!overlap((b.off - GUARD, b.end() + GUARD), (a.off, a.end())));
        }
    }
    let mut hend = 0;
    for h in &host {
        assert_eq!(h.off % 64, 0, "{h:?}");
        assert!(h.off >= hend);
        hend = h.end();
    }
    assert!(hend <= l.host_bytes);
    assert_eq!(l.host_bytes % 64, 0);
    assert_eq!(l.dev_bytes % 256, 0);

    // The picture stores lie inside their allocations.
    let b = &l.bufs;
    let ring = dev[1];
    for p in [b.ring_y, b.ring_u, b.ring_v] {
        assert!(
            p.off >= ring.off && p.end() <= ring.end(),
            "{p:?} outside {ring:?}"
        );
    }
    let ysz = u64::from(l.geo.w.unsigned_abs()) * u64::from(l.geo.h.unsigned_abs());
    let csz = ysz / 4;
    let n = u64::from(l.geo.n.unsigned_abs());
    assert_eq!(b.ring_stride, ysz + 2 * csz);
    assert_eq!(ring.len, 8 * n * b.ring_stride);
    assert_eq!(b.ring_u.off, b.ring_y.off + ysz);
    assert_eq!(b.ring_v.off, b.ring_u.off + csz);
    assert_eq!(b.ring_v.end(), ring.end());
    assert_eq!(b.ref_y.len, 2 * n * ysz);
    assert_eq!(b.ref_u.len, 2 * n * csz);
    assert_eq!(b.ref_v.len, 2 * n * csz);
    assert_eq!(b.cur1.len, n * ysz / 4);
    assert_eq!(b.cur2.len, n * ysz / 16);
    for p in [
        b.cur1, b.cur2, b.ref_y, b.ref_u, b.ref_v, b.ref_y1, b.ref_y2,
    ] {
        assert!(dev.contains(&p));
    }
    assert_eq!(l.h_frames, *host.last().unwrap());
    assert_eq!(l.h_frames.len, n * l.fbytes());
}

#[test]
fn at_shifts_every_buffer() {
    for keep in [false, true] {
        let l = BatchLayout::new(&cfg(704, 480, true), 3, keep).unwrap();
        check_invariants(&l);
        let m = l.at(1 << 20, 4096);
        let shift_d = |d: &DevBuf| DevBuf::new(d.off + (1 << 20), d.len);
        let shift_h = |h: &HostBuf| HostBuf::new(h.off + 4096, h.len);
        assert_eq!(
            m.dev_allocs(),
            l.dev_allocs().iter().map(shift_d).collect::<Vec<_>>()
        );
        assert_eq!(
            m.host_allocs(),
            l.host_allocs().iter().map(shift_h).collect::<Vec<_>>()
        );
        for (a, b) in [
            (m.bufs.ring_y, l.bufs.ring_y),
            (m.bufs.ring_u, l.bufs.ring_u),
            (m.bufs.ring_v, l.bufs.ring_v),
        ] {
            assert_eq!(a, shift_d(&b));
        }
        assert_eq!(m.bufs.ring_stride, l.bufs.ring_stride);
        assert_eq!((m.dev_bytes, m.host_bytes), (l.dev_bytes, l.host_bytes));
        assert_eq!(
            (m.geo, m.row_bs, m.cap_words),
            (l.geo, l.row_bs, l.cap_words)
        );
        assert_eq!(m.at(0, 0), m);
        // Every field moved: shifting back by building at a negative offset is not possible,
        // so compare a double shift with one shift of the sum.
        assert_eq!(l.at(5, 7).at(11, 13), l.at(16, 20));
    }
}

#[test]
fn keep_recon_adds_buffers_at_the_end_of_the_device_list() {
    let a = BatchLayout::new(&cfg(176, 144, false), 1, false).unwrap();
    let b = BatchLayout::new(&cfg(176, 144, false), 1, true).unwrap();
    let (da, db) = (a.dev_allocs(), b.dev_allocs());
    assert_eq!(da[..], db[..da.len()]);
    assert_eq!(db.len(), da.len() + 3);
    let r = b.d_brec.unwrap();
    assert_eq!([r[0].len, r[1].len, r[2].len], [25344, 6336, 6336]);
    let hr = b.h_recon.unwrap();
    assert_eq!(hr.len, 25344 + 2 * 6336);
    assert_eq!(b.host_allocs()[9], hr);
    assert!(a.d_brec.is_none() && a.h_recon.is_none());
}

#[test]
fn formats() {
    // (w, h, interlaced, mbw, mbh, coded h, row_bs, alt_scan, f_code)
    let cases = [
        (1920, 1080, true, 120, 68, 1088, 128, 1, 4),
        (1280, 720, false, 80, 45, 720, 96, 0, 4),
        (704, 480, true, 44, 30, 480, 64, 1, 4),
        (184, 144, false, 12, 9, 144, 32, 0, 4),
        (4096, 2160, false, 256, 135, 2160, 256, 0, 4),
    ];
    for (w, h, il, mbw, mbh, gh, row_bs, alt, f_code) in cases {
        let c = cfg(w, h, il);
        let (g, rb) = geometry(&c, 4).unwrap();
        assert_eq!(
            (g.mbw, g.mbh, g.h, g.w),
            (mbw, mbh, gh, mbw * 16),
            "{w}x{h}"
        );
        assert_eq!(g.nmb, mbw * mbh);
        assert_eq!(g.n, 4);
        assert_eq!(rb, row_bs);
        assert_eq!(g.alt_scan, alt);
        assert_eq!(g.f_code, f_code);
        assert_eq!((g.interlaced, g.field_pred), (i32::from(il), i32::from(il)));
        let l = BatchLayout::new(&c, 4, false).unwrap();
        assert_eq!(
            u64::from(l.cap_words),
            u64::from(w) * u64::from(h) * 3 / 2 / 4
        );
        check_invariants(&l);
    }
}

#[test]
fn geometry_options() {
    // Interlaced without field prediction: no field_pred, still interlaced coding with field
    // DCT, and the frame search reach for f_code.
    let mut c = cfg(704, 480, true);
    c.field_pred = false;
    let (g, _) = geometry(&c, 1).unwrap();
    assert_eq!((g.interlaced, g.field_pred), (1, 0));
    c.field_dct = false;
    let (g, _) = geometry(&c, 1).unwrap();
    assert_eq!((g.interlaced, g.field_pred), (0, 0));
    // f_code from the reach: 4*R2+3 (frame), *3/2+2 with field prediction.
    for (r2, fp, want) in [
        (1, false, 2),
        (2, false, 2),
        (4, false, 3),
        (8, false, 4),
        (16, false, 5),
    ] {
        let mut c = cfg(704, 480, true);
        c.range2 = r2;
        c.field_pred = fp;
        assert_eq!(geometry(&c, 1).unwrap().0.f_code, want, "range2 {r2}");
    }
    let mut c = cfg(704, 480, true);
    c.range2 = 16;
    // reach 67*3/2+2 = 102, 2*102+6 = 210 > 128: f_code 5.
    assert_eq!(geometry(&c, 1).unwrap().0.f_code, 5);
    // A range whose vectors need a vertical f_code above 5 is refused, not clamped (H.262
    // Table 8-8): 19 is the last with field prediction, 30 without.
    c.range2 = 19;
    assert_eq!(geometry(&c, 1).unwrap().0.f_code, 5);
    for r2 in [20, 24, 64, 10_000, i32::MAX, -1] {
        c.range2 = r2;
        assert!(
            matches!(
                geometry(&c, 1),
                Err(LayoutError::SearchRange { range2, max: 19, .. }) if range2 == r2
            ),
            "range2 {r2}"
        );
    }
    let mut pr = cfg(704, 480, false);
    pr.range2 = 30;
    assert_eq!(geometry(&pr, 1).unwrap().0.f_code, 5);
    pr.range2 = 31;
    assert_eq!(
        geometry(&pr, 1),
        Err(LayoutError::SearchRange {
            range2: 31,
            f_code: 6,
            max: 30
        })
    );
    c.range2 = 16;
    // alt_scan given explicitly.
    c.alt_scan = Some(false);
    assert_eq!(geometry(&c, 1).unwrap().0.alt_scan, 0);
    let mut p = cfg(1280, 720, false);
    p.alt_scan = Some(true);
    assert_eq!(geometry(&p, 1).unwrap().0.alt_scan, 1);
    // Capacity 0 is one slot, as in the C++.
    assert_eq!(geometry(&p, 0).unwrap().0.n, 1);
}

#[test]
fn refusals() {
    assert_eq!(
        geometry(&cfg(4112, 2160, false), 1),
        Err(LayoutError::TooWide)
    );
    assert_eq!(
        BatchLayout::new(&cfg(4098, 2160, false), 1, false),
        Err(LayoutError::TooWide)
    );
    assert!(geometry(&cfg(4096, 2160, false), 1).is_ok());
    for (w, h) in [(0, 480), (704, 0), (703, 480), (704, 479)] {
        assert_eq!(geometry(&cfg(w, h, false), 1), Err(LayoutError::BadSize));
    }
    assert_eq!(
        BatchLayout::new(&cfg(4096, 4_000_000_000, false), u32::MAX, false),
        Err(LayoutError::BadSize)
    );
}
