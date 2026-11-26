//! The motion kernels: `k_me` (hierarchical search and mode decision) and `k_mvsmooth`
//! (adopting the left neighbor's vectors).
//!
//! `k_mvsmooth` is integer arithmetic: compared exactly with a model written from its comment
//! and H.262 7.6.4 (half-sample prediction).
//!
//! `k_me` is integer except for the temporal candidate and the scaled second field seed, which
//! use `float`: structural checks only, which hold on any build (the card's too): a picture
//! equal to its reference gives zero vectors at zero cost, also with the temporal candidate
//! and the field search on (so the float paths run); a translated picture gives the
//! translation; a macroblock the search cannot match is intra at the cost of its activity.

mod common;

use common::{BS, Bench, ME_BS, Rng, geo};
use tb_device::DevBuf;
use tb_device::kernel::{BufsRef, Kernel, Me, MvSmooth};
use tb_kernel_abi::{
    Geo, ME_MAX_RANGE2, MODE_BI, MODE_BWD, MODE_FWD, MODE_INTRA, MbInfo, PIC_B, PIC_P, SessPar,
};

const N: usize = 2;
const SLOTS: usize = 2;

/// A smooth texture: noise blurred twice with a 5x5 box, stretched to the full range.
fn texture(rng: &mut Rng, w: usize, h: usize) -> Vec<i32> {
    let mut p: Vec<i32> = (0..w * h).map(|_| i32::from(rng.byte())).collect();
    for _ in 0..2 {
        let q = p.clone();
        for y in 0..h {
            for x in 0..w {
                let mut s = 0;
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                        let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                        s += q[yy * w + xx];
                    }
                }
                p[y * w + x] = s / 25;
            }
        }
    }
    let (lo, hi) = (
        *p.iter().min().unwrap_or(&0),
        *p.iter().max().unwrap_or(&255),
    );
    p.iter()
        .map(|v| (v - lo) * 255 / (hi - lo).max(1))
        .collect()
}

/// 2:1 box decimation with rounding (what `k_pyr` computes; tested in `plumbing`).
fn down(p: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut o = Vec::with_capacity(w * h / 4);
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let s = |dx: usize, dy: usize| u32::from(p[(2 * y + dy) * w + 2 * x + dx]);
            o.push(((s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1) + 2) >> 2) as u8);
        }
    }
    o
}

/// Pictures of one batch for the motion kernels: the source in the ring, the references with
/// their pyramids, the source's pyramid.
struct Pics {
    g: Geo,
    par: Vec<SessPar>,
    /// `[SLOTS][N]` frames of `ring_stride` bytes (luma first).
    ring: Vec<u8>,
    /// `[2][N]` reference luma planes.
    refs: Vec<u8>,
}

impl Pics {
    fn new(g: Geo, par: Vec<SessPar>) -> Self {
        let ysz = (g.w * g.h) as usize;
        Self {
            g,
            par,
            ring: vec![0; SLOTS * N * ysz * 3 / 2],
            refs: vec![0; 2 * N * ysz],
        }
    }
    fn ysz(&self) -> usize {
        (self.g.w * self.g.h) as usize
    }
    fn cur_mut(&mut self, s: usize) -> &mut [u8] {
        let ysz = self.ysz();
        let at = (usize::from(self.par[s].cur) * N + s) * ysz * 3 / 2;
        &mut self.ring[at..at + ysz]
    }
    fn cur(&self, s: usize) -> &[u8] {
        let ysz = self.ysz();
        let at = (usize::from(self.par[s].cur) * N + s) * ysz * 3 / 2;
        &self.ring[at..at + ysz]
    }
    fn ref_mut(&mut self, slot: usize, s: usize) -> &mut [u8] {
        let ysz = self.ysz();
        &mut self.refs[(slot * N + s) * ysz..][..ysz]
    }
    fn reference(&self, slot: usize, s: usize) -> &[u8] {
        let ysz = self.ysz();
        &self.refs[(slot * N + s) * ysz..][..ysz]
    }

    /// Lays the pictures out on a bench: the buffers of `Bufs`.
    fn place(&self, b: &mut Bench) -> BufsRef {
        let (w, h, ysz) = (self.g.w as usize, self.g.h as usize, self.ysz());
        let (mut cur1, mut cur2) = (Vec::new(), Vec::new());
        for s in 0..N {
            let half = down(self.cur(s), w, h);
            cur2.extend(down(&half, w / 2, h / 2));
            cur1.extend(half);
        }
        let (mut ref1, mut ref2) = (Vec::new(), Vec::new());
        for f in self.refs.chunks(ysz) {
            let half = down(f, w, h);
            ref2.extend(down(&half, w / 2, h / 2));
            ref1.extend(half);
        }
        let ring = b.bytes(&self.ring);
        BufsRef {
            ring_y: ring,
            ring_u: ring
                .slice(ysz as u64, ring.len - ysz as u64)
                .unwrap_or_default(),
            ring_v: ring
                .slice(ysz as u64 * 5 / 4, ring.len - ysz as u64 * 5 / 4)
                .unwrap_or_default(),
            ring_stride: ysz as u64 * 3 / 2,
            cur1: b.bytes(&cur1),
            cur2: b.bytes(&cur2),
            ref_y: b.bytes(&self.refs),
            ref_u: DevBuf::default(),
            ref_v: DevBuf::default(),
            ref_y1: b.bytes(&ref1),
            ref_y2: b.bytes(&ref2),
        }
    }
}

fn session(pic_type: u8, cur: u8) -> SessPar {
    SessPar {
        pic_type,
        cur,
        fwd: 0,
        bwd: 1,
        rec: 0,
        keep: 64,
        lam: 4,
        dist_f: 1,
        dist_b: -1,
        pdist_f: 1,
        pdist_b: -1,
        tff: 1,
        ..SessPar::default()
    }
}

/// Runs `k_me` over the listed sessions; returns its output records.
fn run_me(p: &Pics, list: &[i32], prev: &[MbInfo]) -> Vec<MbInfo> {
    let mut b = Bench::new();
    let bufs = p.place(&mut b);
    let dpar = b.pods(&p.par);
    let dlist = b.pods(list);
    let dprev = b.pods(prev);
    let nmb = p.g.nmb as usize;
    let out = b.filled(N * nmb * size_of::<MbInfo>(), 0x77);
    b.cover(
        Kernel::Me(Me {
            g: p.g,
            b: bufs,
            par: dpar,
            list: dlist,
            na: list.len() as i32,
            prev: dprev,
            out,
        }),
        list.len() * nmb,
        ME_BS,
    );
    b.run().pods(out)
}

fn me_geo(mbw: i32, mbh: i32, interlaced: bool, tpred: bool) -> Geo {
    let mut g = geo(N as i32, mbw, mbh);
    g.interlaced = i32::from(interlaced);
    g.field_pred = i32::from(interlaced);
    g.tpred = i32::from(tpred);
    g
}

/// A previous decision with motion, so that the temporal candidate is formed (float path).
fn moving_prev(nmb: usize) -> Vec<MbInfo> {
    (0..N * nmb)
        .map(|i| MbInfo {
            mode: [MODE_FWD, MODE_BWD, MODE_BI][i % 3],
            mv: [[[6, -3], [-5, 2]], [[1, 1], [2, 2]]],
            field_pred: (i % 2) as u8,
            ..MbInfo::default()
        })
        .collect()
}

#[test]
fn a_still_picture_gives_zero_vectors() {
    for (interlaced, tpred, pic_type) in [
        (false, false, PIC_P),
        (false, true, PIC_P),
        (true, false, PIC_P),
        (true, true, PIC_B),
        (false, false, PIC_B),
    ] {
        let g = me_geo(4, 4, interlaced, tpred);
        let mut p = Pics::new(g, vec![session(pic_type, 1), session(pic_type, 0)]);
        let mut rng = Rng::new(3);
        let (w, h) = (g.w as usize, g.h as usize);
        for s in 0..N {
            let t: Vec<u8> = texture(&mut rng, w, h).iter().map(|&v| v as u8).collect();
            p.cur_mut(s).copy_from_slice(&t);
            p.ref_mut(0, s).copy_from_slice(&t);
            p.ref_mut(1, s).copy_from_slice(&t);
        }
        let nmb = g.nmb as usize;
        let out = run_me(&p, &[1, 0], &moving_prev(nmb));
        for (i, m) in out.iter().enumerate() {
            let what = format!("interlaced {interlaced} tpred {tpred} type {pic_type} mb {i}");
            assert_eq!(m.mode, MODE_FWD, "{what}");
            assert_eq!(m.mv, [[[0; 2]; 2]; 2], "{what}");
            assert_eq!(
                (m.field_pred, m.fsel, m.dct_type, m.cost),
                (0, 0, 0, 0),
                "{what}"
            );
        }
    }
}

#[test]
fn a_translated_picture_gives_the_translation() {
    for (dx, dy, tpred) in [
        (3i32, -2i32, false),
        (-5, 4, false),
        (7, 3, true),
        (-2, -6, true),
    ] {
        let g = me_geo(6, 5, false, tpred);
        let mut p = Pics::new(g, vec![session(PIC_P, 0), session(PIC_P, 1)]);
        let (w, h) = (g.w as usize, g.h as usize);
        let mut rng = Rng::new(((dx + 10) * 31 + dy + 10) as u64);
        for s in 0..N {
            let t = texture(&mut rng, w, h);
            let at = |x: i32, y: i32| {
                t[(y.clamp(0, h as i32 - 1) as usize) * w + x.clamp(0, w as i32 - 1) as usize] as u8
            };
            let r: Vec<u8> = (0..w * h)
                .map(|i| at((i % w) as i32, (i / w) as i32))
                .collect();
            // The source at (x, y) is the reference at (x + dx, y + dy).
            let c: Vec<u8> = (0..w * h)
                .map(|i| at((i % w) as i32 + dx, (i / w) as i32 + dy))
                .collect();
            p.ref_mut(0, s).copy_from_slice(&r);
            p.cur_mut(s).copy_from_slice(&c);
        }
        let nmb = g.nmb as usize;
        // The previous picture moved the same way: the temporal candidate points at the answer.
        let prev: Vec<MbInfo> = (0..N * nmb)
            .map(|_| MbInfo {
                mode: MODE_FWD,
                mv: [[[2 * dx as i16, 2 * dy as i16], [0, 0]], [[0, 0], [0, 0]]],
                ..MbInfo::default()
            })
            .collect();
        let out = run_me(&p, &[0, 1], &prev);
        let mut checked = 0;
        for (i, m) in out.iter().enumerate() {
            let mb = i % nmb;
            let (px, py) = (
                16 * (mb % g.mbw as usize) as i32,
                16 * (mb / g.mbw as usize) as i32,
            );
            // Macroblocks whose match lies inside the reference.
            if px + dx < 0 || px + dx > w as i32 - 16 || py + dy < 0 || py + dy > h as i32 - 16 {
                continue;
            }
            assert_eq!(m.mode, MODE_FWD, "({dx}, {dy}) mb {i}");
            assert_eq!(
                m.mv[0][0],
                [2 * dx as i16, 2 * dy as i16],
                "({dx}, {dy}) mb {i}"
            );
            assert_eq!(m.cost, 0, "({dx}, {dy}) mb {i}");
            checked += 1;
        }
        assert!(checked >= N * 8, "{checked}");
    }
}

/// A search range beyond `ME_MAX_RANGE2` never reaches the kernel from the host (the session
/// is refused: its vectors would have no legal f_code). If one did, the first search level
/// would load words far outside a row of the quarter-resolution reference, at the first plane
/// outside the buffer (the bug ported from the C++, at ranges above about 60). The kernel
/// clamps the range: any value above the limit searches exactly as the limit does, and no
/// vector exceeds the reach of f_code 5.
#[test]
fn a_range_beyond_the_limit_is_clamped() {
    let run = |range2: i32| {
        let mut g = me_geo(20, 4, false, false);
        g.range2 = range2;
        let mut p = Pics::new(g, vec![session(PIC_P, 0), session(PIC_P, 1)]);
        let (w, h) = (g.w as usize, g.h as usize);
        let mut rng = Rng::new(77);
        for s in 0..N {
            let t = texture(&mut rng, w, h);
            let at = |x: i32, y: i32| {
                t[(y.clamp(0, h as i32 - 1) as usize) * w + x.clamp(0, w as i32 - 1) as usize] as u8
            };
            // the source is the reference 136 samples to the right: 34 at quarter resolution,
            // which a range of 30 does not reach and a range of 34 would
            let r: Vec<u8> = (0..w * h)
                .map(|i| at((i % w) as i32, (i / w) as i32))
                .collect();
            let c: Vec<u8> = (0..w * h)
                .map(|i| at((i % w) as i32 + 136, (i / w) as i32))
                .collect();
            p.ref_mut(0, s).copy_from_slice(&r);
            p.cur_mut(s).copy_from_slice(&c);
        }
        run_me(&p, &[0, 1], &vec![MbInfo::default(); N * g.nmb as usize])
    };
    let at_limit = run(ME_MAX_RANGE2);
    // the limit itself does not find the far match
    assert!(at_limit.iter().all(|m| m.mv[0][0][0] != 272));
    let reach = 2 * (4 * ME_MAX_RANGE2 + 3) + 1;
    for m in &at_limit {
        assert!(i32::from(m.mv[0][0][0]).abs() <= reach && i32::from(m.mv[0][0][1]).abs() <= reach);
    }
    for beyond in [ME_MAX_RANGE2 + 1, 34, 61, 250, 10_000, i32::MAX] {
        assert_eq!(run(beyond), at_limit, "range2 {beyond}");
    }
    // a negative range is no range
    assert_eq!(run(-5), run(0));
}

/// `k_me`'s activity: the sum of absolute differences from the rounded mean (kernels.h:535).
fn activity(p: &[u8], w: usize, px: usize, py: usize) -> u32 {
    let px_at = |j: usize, i: usize| u32::from(p[(py + j) * w + px + i]);
    let sum: u32 = (0..256).map(|k| px_at(k / 16, k % 16)).sum();
    let m = (sum + 128) >> 8;
    (0..256).map(|k| px_at(k / 16, k % 16).abs_diff(m)).sum()
}

#[test]
fn an_unmatched_macroblock_is_intra_at_the_cost_of_its_activity() {
    let g = me_geo(4, 3, false, false);
    let mut p = Pics::new(g, vec![session(PIC_P, 1), session(PIC_B, 0)]);
    let (w, h) = (g.w as usize, g.h as usize);
    let mut rng = Rng::new(99);
    for s in 0..N {
        // A bright source of little activity over dark references: nothing matches.
        let c: Vec<u8> = (0..w * h).map(|_| 200 + rng.below(40) as u8).collect();
        p.cur_mut(s).copy_from_slice(&c);
        for slot in 0..2 {
            let r: Vec<u8> = (0..w * h).map(|_| rng.below(30) as u8).collect();
            p.ref_mut(slot, s).copy_from_slice(&r);
        }
    }
    let nmb = g.nmb as usize;
    let out = run_me(&p, &[0, 1], &vec![MbInfo::default(); N * nmb]);
    let mut intra = 0;
    for (i, m) in out.iter().enumerate() {
        let (s, mb) = (i / nmb, i % nmb);
        if m.mode == MODE_INTRA {
            let a = activity(
                p.cur(s),
                w,
                16 * (mb % g.mbw as usize),
                16 * (mb / g.mbw as usize),
            );
            assert_eq!(m.cost, a, "mb {i}");
            assert_eq!(m.mv, [[[0; 2]; 2]; 2]);
            assert_eq!((m.field_pred, m.fsel), (0, 0));
            intra += 1;
        }
    }
    assert_eq!(intra, out.len());
}

// ---- k_mvsmooth: exact -----------------------------------------------------------------

/// SAD of the macroblock at (px, py) against the frame prediction with the vectors of `m`
/// (H.262 7.6.4, averaged when bidirectional); `u32::MAX` when a vector leaves the picture.
fn frame_pred_sad(p: &Pics, s: usize, px: usize, py: usize, m: &MbInfo) -> u32 {
    let (w, h) = (p.g.w as usize, p.g.h as usize);
    let sp = p.par[s];
    let dirs: Vec<usize> = (0..2)
        .filter(|&d| m.mode & (if d == 1 { MODE_BWD } else { MODE_FWD }) != 0)
        .collect();
    for &d in &dirs {
        let x = 2 * px as i32 + i32::from(m.mv[0][d][0]);
        let y = 2 * py as i32 + i32::from(m.mv[0][d][1]);
        if x < 0 || y < 0 || ((x + 1) >> 1) + 16 > w as i32 || ((y + 1) >> 1) + 16 > h as i32 {
            return u32::MAX;
        }
    }
    let cur = p.cur(s);
    let mut sad = 0;
    for j in 0..16 {
        for i in 0..16 {
            let mut pred: Option<u32> = None;
            for &d in &dirs {
                let r = p.reference(usize::from(if d == 1 { sp.bwd } else { sp.fwd }), s);
                let xh = 2 * (px + i) as i32 + i32::from(m.mv[0][d][0]);
                let yh = 2 * (py + j) as i32 + i32::from(m.mv[0][d][1]);
                let (x, y) = ((xh >> 1) as usize, (yh >> 1) as usize);
                let at = |dx: usize, dy: usize| u32::from(r[(y + dy) * w + x + dx]);
                let v = match (xh & 1, yh & 1) {
                    (0, 0) => at(0, 0),
                    (1, 0) => (at(0, 0) + at(1, 0) + 1) >> 1,
                    (0, _) => (at(0, 0) + at(0, 1) + 1) >> 1,
                    _ => (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) >> 2,
                };
                pred = Some(pred.map_or(v, |q| (q + v + 1) >> 1));
            }
            sad += u32::from(cur[(py + j) * w + px + i]).abs_diff(pred.unwrap_or(0));
        }
    }
    sad
}

/// `k_mvsmooth` as its comment says: a macroblock that is not intra adopts the mode and
/// vectors of its left neighbor (frame-predicted, not intra, different) when the
/// neighbor's prediction costs at most its own cost plus the bits saved. Returns (adopted,
/// considered): how many macroblocks took their neighbor's vectors, and how many were
/// candidates.
fn model_mvsmooth(p: &Pics, list: &[i32], input: &[MbInfo], out: &mut [MbInfo]) -> (usize, usize) {
    let g = p.g;
    let nmb = g.nmb as usize;
    let (mut adopted, mut considered) = (0, 0);
    for &s in list {
        let s = s as usize;
        for mb in 0..nmb {
            let mbi = s * nmb + mb;
            let mut o = input[mbi];
            let mbx = mb % g.mbw as usize;
            if g.smooth != 0 && mbx > 0 && o.mode != MODE_INTRA {
                let l = input[mbi - 1];
                let same = l.mode == o.mode
                    && l.field_pred == o.field_pred
                    && l.fsel == o.fsel
                    && l.mv == o.mv;
                if !same && l.mode != MODE_INTRA && l.field_pred == 0 {
                    let (px, py) = (16 * mbx, 16 * (mb / g.mbw as usize));
                    let sl = frame_pred_sad(p, s, px, py, &l);
                    considered += 1;
                    let sp = p.par[s];
                    let save = u32::from(if sp.pic_type == PIC_B { 12u16 } else { 6 } * sp.lam);
                    if sl != u32::MAX && u64::from(sl) <= u64::from(o.cost) + u64::from(save) {
                        o.mode = l.mode;
                        o.field_pred = 0;
                        o.fsel = 0;
                        o.mv = l.mv;
                        o.cost = sl;
                        adopted += 1;
                    }
                }
            }
            out[mbi] = o;
        }
    }
    (adopted, considered)
}

fn mvsmooth_case(pic_type: u8, smooth: bool, seed: u64) -> (usize, usize) {
    let mut g = me_geo(5, 4, true, false);
    g.smooth = i32::from(smooth);
    let mut p = Pics::new(g, vec![session(pic_type, 1), session(pic_type, 0)]);
    let (w, h) = (g.w as usize, g.h as usize);
    let mut rng = Rng::new(seed);
    for s in 0..N {
        let t: Vec<u8> = texture(&mut rng, w, h).iter().map(|&v| v as u8).collect();
        p.cur_mut(s).copy_from_slice(&t);
        for slot in 0..2 {
            // References: the source moved by a little, plus noise.
            let r: Vec<u8> = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w + slot + 1).min(w - 1), i / w);
                    t[y * w + x].saturating_add(rng.below(6) as u8)
                })
                .collect();
            p.ref_mut(slot, s).copy_from_slice(&r);
        }
    }
    let nmb = g.nmb as usize;
    let modes: &[u8] = if pic_type == PIC_B {
        &[MODE_INTRA, MODE_FWD, MODE_BWD, MODE_BI]
    } else {
        &[MODE_INTRA, MODE_FWD]
    };
    let mut input = Vec::with_capacity(N * nmb);
    for i in 0..N * nmb {
        let mut m = MbInfo {
            mode: modes[rng.below(modes.len() as u32) as usize],
            field_pred: u8::from(rng.below(5) == 0),
            ..MbInfo::default()
        };
        if m.mode != MODE_INTRA {
            for r in 0..2 {
                for d in 0..2 {
                    // Mostly inside the picture; sometimes (+-40 half samples) outside.
                    let range = if rng.below(6) == 0 { 40 } else { 6 };
                    m.mv[r][d] = [
                        rng.below(2 * range + 1) as i16 - range as i16,
                        rng.below(2 * range + 1) as i16 - range as i16,
                    ];
                }
            }
            m.fsel = if m.field_pred != 0 {
                rng.below(16) as u8
            } else {
                0
            };
        }
        // Repeat the left neighbor now and then.
        let mbx = i % nmb % g.mbw as usize;
        if mbx > 0 && rng.below(5) == 0 {
            let left: MbInfo = input[i - 1];
            m = left;
        }
        m.cost = rng.below(40_000);
        input.push(m);
    }
    let list = [1i32, 0];
    let mut b = Bench::new();
    let bufs = p.place(&mut b);
    let dpar = b.pods(&p.par);
    let dlist = b.pods(&list);
    let din = b.pods(&input);
    let out = b.filled(N * nmb * size_of::<MbInfo>(), 0x55);
    b.cover(
        Kernel::MvSmooth(MvSmooth {
            g,
            b: bufs,
            par: dpar,
            list: dlist,
            na: list.len() as i32,
            input: din,
            out,
        }),
        list.len() * nmb,
        BS,
    );
    let got: Vec<MbInfo> = b.run().pods(out);
    let mut want = input.clone();
    let counts = model_mvsmooth(&p, &list, &input, &mut want);
    assert_eq!(got, want, "type {pic_type} smooth {smooth} seed {seed}");
    counts
}

/// Both outcomes occur: some candidates adopt their neighbor's vectors, some do not.
fn both_outcomes(pic_type: u8, seeds: std::ops::Range<u64>) {
    let (mut adopted, mut considered) = (0, 0);
    for seed in seeds {
        let (a, c) = mvsmooth_case(pic_type, true, seed);
        adopted += a;
        considered += c;
    }
    assert!(
        adopted > 3 && considered > adopted + 3,
        "{adopted} of {considered}"
    );
}

#[test]
fn mvsmooth_p_pictures() {
    both_outcomes(PIC_P, 0..6);
}

#[test]
fn mvsmooth_b_pictures() {
    both_outcomes(PIC_B, 10..16);
}

#[test]
fn mvsmooth_off_copies_its_input() {
    assert_eq!(mvsmooth_case(PIC_B, false, 20), (0, 0));
}
