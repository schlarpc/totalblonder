//! The golden vectors of the conversion path's host arithmetic (`crates/tb-golden/vectors/taps.jsonl`): the
//! filter taps of [`make_taps`] for specs that cover every branch, and every color matrix of
//! [`make_csc`], as `taps` and `csc` records (crates/tb-golden/vectors/README.md). The spec sets are those the
//! C++ spike's dump wrote; the file it wrote is what this writes, byte for byte.

use tb_golden::write::Rec;

use crate::{ConvClass, ConvKey, Filter, Rows, TapSpec, make_csc, make_taps};

fn filter_code(f: Filter) -> i64 {
    match f {
        Filter::Bilinear => 0,
        Filter::Bicubic => 1,
        Filter::Lanczos3 => 2,
    }
}

fn rows_code(r: Rows) -> i64 {
    match r {
        Rows::All => 0,
        Rows::Top => 1,
        Rows::Bottom => 2,
    }
}

const FILTERS: [Filter; 3] = [Filter::Bilinear, Filter::Bicubic, Filter::Lanczos3];

struct Dump {
    out: String,
    seq: i64,
}

impl Dump {
    fn taps(&mut self, s: &TapSpec, pad4: bool, set: &str, class: Option<(&ConvKey, &str)>) {
        let mut r = Rec::new("taps");
        r.i("seq", self.seq)
            .s("set", set)
            .u("n_src", u64::from(s.n_src))
            .u("s_step", u64::from(s.s_step))
            .u("s_off", u64::from(s.s_off))
            .u("n_dst", u64::from(s.n_dst))
            .u("d_step", u64::from(s.d_step))
            .u("d_off", u64::from(s.d_off))
            .d("crop0", s.crop0)
            .d("crop_len", s.crop_len)
            .d("phase", s.phase)
            .i("filter", filter_code(s.filter))
            .b("weave_lp", s.weave_lp)
            .b("pad4", pad4);
        if let Some((k, axis)) = class {
            let u = |x: u32| i64::from(x);
            r.ia(
                "key",
                [
                    u(k.sw),
                    u(k.sh),
                    u(k.bps),
                    u(k.cx),
                    u(k.cy),
                    u(k.cw),
                    u(k.ch),
                    u(k.dw),
                    u(k.dh),
                    rows_code(k.srows),
                    rows_code(k.drows),
                    filter_code(k.filter),
                    i64::from(k.weave_lp),
                ],
            )
            .s("axis", axis);
        }
        match make_taps(s, pad4) {
            Ok(t) => {
                let start: Vec<u8> = t.start.iter().flat_map(|x| x.to_le_bytes()).collect();
                let coef: Vec<u8> = t.coef.iter().flat_map(|x| x.to_le_bytes()).collect();
                r.i("error", 0)
                    .u("n", u64::from(t.n))
                    .u("nt", u64::from(t.nt))
                    .u("n_lat", u64::from(t.n_lat))
                    .hex("start", &start)
                    .hex("coef", &coef);
            }
            Err(_) => {
                r.i("error", 1);
            }
        }
        self.out.push_str(&r.finish());
        self.seq += 1;
    }
}

#[allow(clippy::too_many_arguments, reason = "the fields of a TapSpec")]
fn spec(
    n_src: u32,
    s_step: u32,
    s_off: u32,
    n_dst: u32,
    d_step: u32,
    d_off: u32,
    crop0: f64,
    crop_len: f64,
    phase: f64,
    filter: Filter,
    weave_lp: bool,
) -> TapSpec {
    TapSpec {
        n_src,
        s_step,
        s_off,
        n_dst,
        d_step,
        d_off,
        crop0,
        crop_len,
        phase,
        filter,
        weave_lp,
    }
}

#[allow(clippy::too_many_arguments, reason = "the fields of a ConvKey")]
fn key(
    sw: u32,
    sh: u32,
    bps: u32,
    (cx, cy, cw, ch): (u32, u32, u32, u32),
    dw: u32,
    dh: u32,
    srows: Rows,
    drows: Rows,
    filter: Filter,
    weave_lp: bool,
) -> ConvKey {
    ConvKey {
        sw,
        sh,
        bps,
        cx,
        cy,
        cw,
        ch,
        dw,
        dh,
        srows,
        drows,
        filter,
        weave_lp,
    }
}

/// The whole of `crates/tb-golden/vectors/taps.jsonl`.
pub fn taps_records() -> String {
    let mut d = Dump {
        out: String::new(),
        seq: 0,
    };
    let mut head = Rec::new("golden");
    head.i("version", 1).s("case", "taps");
    d.out.push_str(&head.finish());

    // source / destination sample counts: down by 2, 1.5, 2.25, 4.5; up by the same; same size
    const SC: [(u32, u32); 9] = [
        (48, 24),
        (36, 24),
        (54, 24),
        (108, 24),
        (24, 48),
        (24, 36),
        (24, 54),
        (24, 108),
        (24, 24),
    ];
    // row lattices: all, top field, bottom field (step, offset)
    const LAT: [(u32, u32); 3] = [(1, 0), (2, 0), (2, 1)];

    // every filter, scale and pair of lattices, without and with the low-pass
    for (set, lp) in [("grid", false), ("weave", true)] {
        for f in FILTERS {
            for (ns, nd) in SC {
                for (ss, so) in LAT {
                    for (ds, dof) in LAT {
                        let s = spec(ns, ss, so, nd, ds, dof, 0.0, f64::from(ns), 0.5, f, lp);
                        d.taps(&s, false, set, None);
                    }
                }
            }
        }
    }
    // the horizontal axes: co-sited chroma phase, and taps padded to a multiple of four
    for f in FILTERS {
        for (ns, nd) in SC {
            for ph in [0.5, 0.25] {
                for pad4 in [false, true] {
                    if ph == 0.5 && !pad4 {
                        continue; // in "grid"
                    }
                    let s = spec(ns, 1, 0, nd, 1, 0, 0.0, f64::from(ns), ph, f, false);
                    d.taps(&s, pad4, "phase", None);
                }
            }
        }
    }
    // crops, some with a fractional origin and length (chroma of an odd luma crop)
    const CR: [(u32, u32, f64, f64); 6] = [
        (48, 24, 3.5, 37.0),
        (48, 40, 0.5, 20.5),
        (64, 30, 7.25, 40.5),
        (96, 64, 5.0, 70.0),
        (40, 40, 1.5, 36.0),
        (160, 24, 12.0, 126.0),
    ];
    for f in FILTERS {
        for (ns, nd, c0, cl) in CR {
            for (ss, so) in LAT {
                for ph in [0.5, 0.25] {
                    let fld = ss == 2;
                    let s = spec(ns, ss, so, nd, ss, so, c0, cl, ph, f, false);
                    d.taps(&s, if fld { false } else { ph == 0.25 }, "crop", None);
                    if fld {
                        // field of the source into every second line of a frame (bob), and
                        // frame -> field with the low-pass
                        let b = spec(ns, ss, so, nd, 1, 0, c0, cl, ph, f, false);
                        d.taps(&b, false, "crop", None);
                        let w = spec(ns, 1, 0, nd, ss, so, c0, cl, ph, f, true);
                        d.taps(&w, false, "crop", None);
                    }
                }
            }
        }
    }
    // tiny sources: only the rounding to a multiple of four can make make_taps refuse a spec
    {
        let a = spec(3, 1, 0, 1, 1, 0, 0.0, 3.0, 0.5, Filter::Lanczos3, false);
        d.taps(&a, false, "small", None);
        d.taps(&a, true, "small", None); // 3 taps -> 4: refused
        let b = spec(6, 1, 0, 2, 1, 0, 0.0, 6.0, 0.25, Filter::Lanczos3, false);
        d.taps(&b, true, "small", None); // 6 -> 8: refused
        let c = spec(10, 2, 1, 5, 2, 1, 0.0, 10.0, 0.5, Filter::Bicubic, false);
        d.taps(&c, false, "small", None);
        d.taps(&c, true, "small", None); // a lattice of 5: refused
        let e = spec(8, 1, 0, 6, 1, 0, 0.0, 8.0, 0.25, Filter::Bilinear, false);
        d.taps(&e, true, "small", None);
    }

    // conversion classes: the four axes as the converter makes them
    use Filter::{Bicubic, Bilinear, Lanczos3};
    use Rows::{All, Bottom, Top};
    let full = (0, 0, 176, 144);
    let keys = [
        key(
            1920,
            1080,
            1,
            (0, 0, 1920, 1080),
            1280,
            720,
            All,
            All,
            Lanczos3,
            false,
        ), // 1080p -> 720p
        key(176, 144, 1, full, 88, 72, All, All, Lanczos3, false),
        key(176, 144, 2, full, 88, 72, All, All, Lanczos3, false), // P010: chroma taps not padded
        key(176, 144, 1, full, 352, 288, All, All, Bicubic, false),
        key(176, 144, 1, full, 120, 96, Bottom, Bottom, Lanczos3, false),
        key(176, 144, 1, full, 176, 144, Top, All, Lanczos3, false), // bob, same size
        key(176, 144, 1, full, 128, 96, Bottom, All, Bilinear, false),
        key(176, 144, 1, full, 176, 144, All, Top, Lanczos3, true), // weave with low-pass
        key(176, 144, 2, full, 352, 288, All, Bottom, Bicubic, true),
        key(
            176,
            144,
            1,
            (22, 6, 132, 132),
            88,
            88,
            All,
            All,
            Lanczos3,
            false,
        ), // crop, odd chroma origin
        key(
            176,
            144,
            1,
            (30, 0, 118, 144),
            64,
            48,
            All,
            All,
            Lanczos3,
            false,
        ),
        key(176, 144, 1, full, 176, 144, All, All, Lanczos3, true), // weave flag alone: not a copy
    ];
    for k in &keys {
        let [hy, hc, vy, vc] = ConvClass::specs(k);
        d.taps(&hy, true, "class", Some((k, "hy")));
        d.taps(&hc, k.bps == 1, "class", Some((k, "hc")));
        d.taps(&vy, false, "class", Some((k, "vy")));
        d.taps(&vc, false, "class", Some((k, "vc")));
    }

    // every color matrix
    for from709 in [false, true] {
        for to709 in [false, true] {
            for full in [false, true] {
                for il in [false, true] {
                    let p = make_csc(from709, to709, full);
                    let mut r = Rec::new("csc");
                    r.b("from709", from709)
                        .b("to709", to709)
                        .b("from_full", full)
                        .b("interlaced", il)
                        .ia("m", p.m.map(i64::from))
                        .ia("off", p.off.map(i64::from))
                        .b("interlaced_out", il);
                    d.out.push_str(&r.finish());
                }
            }
        }
    }
    d.out
}

#[cfg(test)]
mod tests {
    /// The committed file is what this writes.
    #[test]
    fn the_committed_taps_are_fresh() {
        let committed = tb_golden::text("taps").unwrap_or_default();
        let ours = super::taps_records();
        if let Some((i, (a, b))) = committed
            .lines()
            .zip(ours.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
        {
            panic!(
                "crates/tb-golden/vectors/taps.jsonl line {}: committed\n{a}\nwritten\n{b}",
                i + 1
            );
        }
        assert_eq!(committed.len(), ours.len());
    }
}
