//! The transform kernels: `k_xf` (prediction, residual, forward DCT, the zero-threshold
//! histogram), `k_quant` (quantization) and `k_recon` (inverse quantization, IDCT,
//! reconstruction).
//!
//! These kernels compute in `float`. Two kinds of check:
//!
//! - structural, which hold on any build of the kernels (the card's too): a flat intra block
//!   gives a DC coefficient only and reconstructs to its value; a perfectly predicted block
//!   gives no coefficient and reconstructs to its prediction;
//! - exact against a port of the kernels' arithmetic to Rust, operation for operation in `f32`,
//!   with a fused multiply-add (`mul_add`) exactly where the kernels write `fmaf` / `fmaf_nc`
//!   and nowhere else. Both builds of the kernels compile without implicit contraction, and
//!   those fused operations are where nvcc's default contraction put them (K3), so this pins
//!   the host build and, through the GPU tests of `tb-device-gpu` (`float_parity`), the cards.
#![allow(
    clippy::excessive_precision,
    reason = "the DCT constants are the literals of kernels.h, rounded to f32 as C++ rounds them"
)]
#![allow(
    clippy::needless_range_loop,
    reason = "the models index as the kernels do, by row and column"
)]

mod common;

use common::{BS, Bench, RECON_BS, Rng, geo, pods_of, tables};
use tb_device::kernel::{BufsRef, Kernel, Quant, Recon, Xf};
use tb_kernel_abi::{
    Geo, MODE_BI, MODE_BWD, MODE_FWD, MODE_INTRA, MbInfo, SL_REC, SessPar, Tables, XB,
};

// ---- the kernels' arithmetic, ported (kernels.h: fdct8, idct8, n_act, mb_qcode) ----------

const DC1: f32 = 0.980_785_280_403_230_449_13;
const DC2: f32 = 0.923_879_532_511_286_756_13;
const DC3: f32 = 0.831_469_612_302_545_237_08;
const DC5: f32 = 0.555_570_233_019_602_224_74;
const DC6: f32 = 0.382_683_432_365_089_771_73;
const DC7: f32 = 0.195_090_322_016_128_267_85;
const DC4H: f32 = 0.353_553_390_593_273_762_20;

thread_local! {
    /// Set by `host_fmaf_is_fused` only: the port's fused operations as a multiply and an add
    /// (two roundings), to show that the test data tells the two apart.
    static UNFUSED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// `fmaf(a, b, c)`: one rounding.
fn fma(a: f32, b: f32, c: f32) -> f32 {
    if UNFUSED.get() {
        a * b + c
    } else {
        a.mul_add(b, c)
    }
}

/// `fmaf_nc(a, b, c)` of kernels.h: `fmaf(a, b, -c)`.
fn fma_nc(a: f32, b: f32, c: f32) -> f32 {
    fma(a, b, -c)
}

fn fdct8(i: &[f32; 8]) -> [f32; 8] {
    let (s07, d07, s16, d16) = (i[0] + i[7], i[0] - i[7], i[1] + i[6], i[1] - i[6]);
    let (s25, d25, s34, d34) = (i[2] + i[5], i[2] - i[5], i[3] + i[4], i[3] - i[4]);
    let (e0, e1, e2, e3) = (s07 + s34, s16 + s25, s16 - s25, s07 - s34);
    let mut o = [0f32; 8];
    o[0] = (e0 + e1) * DC4H;
    o[4] = (e0 - e1) * DC4H;
    o[2] = 0.5 * fma(DC6, e2, DC2 * e3);
    o[6] = 0.5 * fma_nc(DC6, e3, DC2 * e2);
    o[1] = 0.5 * fma(DC7, d34, fma(DC5, d25, fma(DC1, d07, DC3 * d16)));
    o[3] = 0.5 * fma(-DC5, d34, fma(-DC1, d25, fma_nc(DC3, d07, DC7 * d16)));
    o[5] = 0.5 * fma(DC3, d34, fma(DC7, d25, fma_nc(DC5, d07, DC1 * d16)));
    o[7] = 0.5 * fma(-DC1, d34, fma(DC3, d25, fma_nc(DC7, d07, DC5 * d16)));
    o
}

fn idct8(f: &[f32; 8]) -> [f32; 8] {
    let (s04, d04) = (f[0] + f[4], f[0] - f[4]);
    let a2 = 0.5 * fma_nc(DC6, f[2], DC2 * f[6]);
    let a3 = 0.5 * fma(DC6, f[6], DC2 * f[2]);
    let (e0, e1) = (fma(s04, DC4H, a3), fma(d04, DC4H, a2));
    let (e2, e3) = (fma_nc(d04, DC4H, a2), fma_nc(s04, DC4H, a3));
    let p0 = fma(DC7, f[7], fma(DC5, f[5], fma(DC1, f[1], DC3 * f[3])));
    let p1 = fma(-DC5, f[7], fma(-DC1, f[5], fma_nc(DC3, f[1], DC7 * f[3])));
    let p2 = fma(DC3, f[7], fma(DC7, f[5], fma_nc(DC5, f[1], DC1 * f[3])));
    let p3 = fma(-DC1, f[7], fma(DC3, f[5], fma_nc(DC7, f[1], DC5 * f[3])));
    [
        fma(0.5, p0, e0),
        fma(0.5, p1, e1),
        fma(0.5, p2, e2),
        fma(0.5, p3, e3),
        fma(-0.5, p3, e3),
        fma(-0.5, p2, e2),
        fma(-0.5, p1, e1),
        fma(-0.5, p0, e0),
    ]
}

fn n_act(act: i32, avg: i32, s: f32) -> f32 {
    fma(s, act as f32, avg as f32) / fma(s, avg as f32, act as f32)
}

fn mb_qcode(g: &Geo, t: &Tables, sp: &SessPar, rowcode: i32, act: i32) -> i32 {
    if sp.aq == 0 || rowcode >= 31 {
        return rowcode;
    }
    let q = fma(
        f32::from(t.qscale[rowcode as usize]),
        n_act(act, i32::from(sp.avg_act), g.aq_s),
        0.5,
    ) as i32;
    i32::from(t.qcode_near[q.clamp(1, 112) as usize])
}

// ---- the test pictures ----------------------------------------------------------------------

const SLOTS: usize = 2;

/// All inputs of the three kernels.
#[derive(Clone)]
struct Case {
    g: Geo,
    t: Tables,
    list: Vec<i32>,
    par: Vec<SessPar>,
    /// `[SLOTS][n]` frames of `ring_stride` bytes: Y, Cb, Cr.
    ring: Vec<u8>,
    /// Reference planes `[2][n]`.
    ref_y: Vec<u8>,
    ref_u: Vec<u8>,
    ref_v: Vec<u8>,
    info: Vec<MbInfo>,
    act: Vec<u16>,
    rowq: Vec<u8>,
    /// Initial contents of the output buffers (not every element is written).
    coef: Vec<i16>,
    predbuf: Vec<u8>,
    hist: Vec<u64>,
    cmask: Vec<u64>,
    lev: Vec<i16>,
    nzmask: Vec<u64>,
    nz: Vec<u8>,
    mbq: Vec<u8>,
}

impl Case {
    fn ysz(&self) -> usize {
        (self.g.w * self.g.h) as usize
    }
    fn n(&self) -> usize {
        self.g.n as usize
    }
    fn nmbtot(&self) -> usize {
        (self.g.n * self.g.nmb) as usize
    }
    fn ring_stride(&self) -> usize {
        self.ysz() * 3 / 2
    }
}

/// Geometry of block `b` of physical macroblock `mbi` (kernels.h `blk_geo`): plane width and
/// height, position of its first pixel.
fn blk(g: &Geo, b: usize, mb: usize) -> (usize, usize, usize, usize) {
    let (mbx, mby) = (mb % g.mbw as usize, mb / g.mbw as usize);
    if b < 4 {
        (
            g.w as usize,
            g.h as usize,
            mbx * 16 + (b & 1) * 8,
            mby * 16 + (b >> 1) * 8,
        )
    } else {
        (g.w as usize / 2, g.h as usize / 2, mbx * 8, mby * 8)
    }
}

/// Picture line of row `j` of block `b` (field DCT regroups the luma lines by field, 6.1.3).
fn blk_line(b: usize, y0: usize, dct_type: u8, j: usize) -> usize {
    if b < 4 && dct_type != 0 {
        y0 - (b >> 1) * 8 + 2 * j + (b >> 1)
    } else {
        y0 + j
    }
}

/// Eight pixels of a half-sample prediction (7.6.4) from rows `ya`, `yb` of a plane.
fn pred8(p: &[u8], pw: usize, ya: usize, yb: usize, x: usize, fx: bool, fy: bool) -> [u8; 8] {
    let px = |y: usize, i: usize| u32::from(p[y * pw + i]);
    core::array::from_fn(|i| {
        let a = px(ya, x + i);
        (match (fx, fy) {
            (false, false) => a,
            (true, false) => (a + px(ya, x + i + 1) + 1) >> 1,
            (false, true) => (a + px(yb, x + i) + 1) >> 1,
            (true, true) => (a + px(ya, x + i + 1) + px(yb, x + i) + px(yb, x + i + 1) + 2) >> 2,
        }) as u8
    })
}

/// The (session, macroblock) pairs the listed sessions cover, in the kernels' thread order.
fn listed(c: &Case) -> Vec<(usize, usize)> {
    let nmb = c.g.nmb as usize;
    c.list
        .iter()
        .flat_map(|&s| (0..nmb).map(move |mb| (s as usize, mb)))
        .collect()
}

/// `k_xf` on `c` (kernels.h:790), updating coef, predbuf, hist, cmask.
fn model_xf(c: &mut Case) {
    let g = c.g;
    let (n, ysz, nmbtot) = (c.n(), c.ysz(), c.nmbtot());
    let csz = ysz / 4;
    for (s, mb) in listed(c) {
        let sp = c.par[s];
        let mbi = s * g.nmb as usize + mb;
        let mi = c.info[mbi];
        for b in 0..6 {
            let (pw, ph, x0, y0) = blk(&g, b, mb);
            let blkn = b * nmbtot + mbi;
            let psz = pw * ph;
            let cur_off = (usize::from(sp.cur) * n + s) * c.ring_stride()
                + match b {
                    0..4 => 0,
                    4 => ysz,
                    _ => ysz + csz,
                };
            let mut sm = [[0f32; 8]; 8];
            for (j, smj) in sm.iter_mut().enumerate() {
                let line = blk_line(b, y0, mi.dct_type, j);
                let mut pr = [0u8; 8];
                if mi.mode != MODE_INTRA {
                    let mut have = false;
                    for dir in 0..2 {
                        if mi.mode & (if dir == 1 { MODE_BWD } else { MODE_FWD }) == 0 {
                            continue;
                        }
                        let slot = usize::from(if dir == 1 { sp.bwd } else { sp.fwd });
                        let plane = match b {
                            0..4 => &c.ref_y,
                            4 => &c.ref_u,
                            _ => &c.ref_v,
                        };
                        let plane = &plane[(slot * n + s) * psz..][..psz];
                        let r = if mi.field_pred != 0 { line & 1 } else { 0 };
                        let (mut mvx, mut mvy) =
                            (i32::from(mi.mv[r][dir][0]), i32::from(mi.mv[r][dir][1]));
                        if b >= 4 {
                            mvx /= 2;
                            mvy /= 2;
                        }
                        let (ya, yb) = if mi.field_pred != 0 {
                            let sel = i32::from((mi.fsel >> (2 * dir + r)) & 1);
                            let hf = ph as i32 >> 1;
                            let fl = (line as i32 >> 1) + (mvy >> 1);
                            (
                                2 * fl.clamp(0, hf - 1) + sel,
                                2 * (fl + (mvy & 1)).clamp(0, hf - 1) + sel,
                            )
                        } else {
                            let l = line as i32 + (mvy >> 1);
                            (
                                l.clamp(0, ph as i32 - 1),
                                (l + (mvy & 1)).clamp(0, ph as i32 - 1),
                            )
                        };
                        let x = (x0 as i32 + (mvx >> 1)) as usize;
                        let pa = pred8(
                            plane,
                            pw,
                            ya as usize,
                            yb as usize,
                            x,
                            mvx & 1 != 0,
                            mvy & 1 != 0,
                        );
                        if have {
                            for i in 0..8 {
                                pr[i] = ((u32::from(pr[i]) + u32::from(pa[i]) + 1) >> 1) as u8;
                            }
                        } else {
                            pr = pa;
                        }
                        have = true;
                    }
                }
                let cur = &c.ring[cur_off + line * pw + x0..][..8];
                let f: [f32; 8] =
                    core::array::from_fn(|i| (i32::from(cur[i]) - i32::from(pr[i])) as f32);
                if sp.want_pred != 0 {
                    c.predbuf[(blkn * 8 + j) * 8..][..8].copy_from_slice(&pr);
                }
                *smj = fdct8(&f);
            }
            let spos = c.t.scan_pos[g.alt_scan as usize];
            let intra = mi.mode == MODE_INTRA;
            let a_mb = i32::from(c.act[mbi]);
            let inv_n = if sp.aq != 0 {
                1.0 / n_act(a_mb, i32::from(sp.avg_act), g.aq_s)
            } else {
                1.0
            };
            let qf =
                f32::from(c.t.qscale[mb_qcode(&g, &c.t, &sp, i32::from(sp.qfloor), a_mb) as usize]);
            let invf = 0.25 / qf;
            let mut thr = 4 * qf as i32;
            if (thr as f32 * invf) as i32 == 0 {
                thr += 1;
            }
            let (mut mask, mut h0, mut h1) = (0u64, 0u64, 0u64);
            for u in 0..8 {
                let t: [f32; 8] = core::array::from_fn(|v| sm[v][u]);
                let ff = fdct8(&t);
                for (v, &fv) in ff.iter().enumerate() {
                    let r = v * 8 + u;
                    let (l, m, keep) = if intra && r == 0 {
                        (
                            (fma(fv, 0.125, 0.5).floor() as i32).clamp(0, 255),
                            0.0f32,
                            true,
                        )
                    } else {
                        let l = (fma(fv, 4.0, 0.5).floor() as i32).clamp(-32768, 32767);
                        let iq = f32::from(c.t.intra_q[r]);
                        let m = if intra {
                            fv.abs() * (16.0f32 / 0.625f32) / iq * inv_n
                        } else {
                            fv.abs() * inv_n
                        };
                        let keep = if intra {
                            (l.abs() as f32 * 4.0 / (iq * qf) + 0.375) as i32 != 0
                        } else {
                            l.abs() >= thr
                        };
                        (l, m, keep)
                    };
                    if keep {
                        let p = usize::from(spos[r]);
                        mask |= 1 << p;
                        c.coef[(b * 64 + p) * nmbtot + mbi] = l as i16;
                    }
                    if m >= 1.0 {
                        let e = ((m.to_bits() >> 22) as i32 - 254).min(15);
                        if e < 8 {
                            h0 += 1 << (8 * e);
                        } else {
                            h1 += 1 << (8 * (e - 8));
                        }
                    }
                }
            }
            c.cmask[blkn] = mask;
            c.hist[blkn * 2] = h0;
            c.hist[blkn * 2 + 1] = h1;
        }
    }
}

/// `k_quant` on `c` (kernels.h:966), updating lev, nzmask, nz, mbq.
fn model_quant(c: &mut Case) {
    let g = c.g;
    let nmbtot = c.nmbtot();
    for b in 0..6 {
        for (s, mb) in listed(c) {
            let mbi = s * g.nmb as usize + mb;
            let sp = c.par[s];
            let rowcode = i32::from(c.rowq[s * g.mbh as usize + mb / g.mbw as usize]);
            let code = mb_qcode(&g, &c.t, &sp, rowcode, i32::from(c.act[mbi]));
            let q = f32::from(c.t.qscale[code as usize]);
            let intra = c.info[mbi].mode == MODE_INTRA;
            let at = |i: usize| (b * 64 + i) * nmbtot + mbi;
            let ras = c.t.scan_ras[g.alt_scan as usize];
            let mut cm = c.cmask[b * nmbtot + mbi];
            let mut mask = 0u64;
            if intra {
                c.lev[at(0)] = c.coef[at(0)];
                mask = 1;
                cm &= !1;
            }
            while cm != 0 {
                let i = cm.trailing_zeros() as usize;
                cm &= cm - 1;
                if i >= usize::from(sp.keep) {
                    break;
                }
                let cf = i32::from(c.coef[at(i)]);
                let mut a = if intra {
                    (cf.abs() as f32 * 4.0 / (f32::from(c.t.intra_q[usize::from(ras[i])]) * q)
                        + 0.375) as i32
                } else {
                    (cf.abs() as f32 * (0.25 / q)) as i32
                };
                a = a.min(2047);
                if a != 0 {
                    c.lev[at(i)] = (if cf < 0 { -a } else { a }) as i16;
                    mask |= 1 << i;
                }
            }
            c.nzmask[b * nmbtot + mbi] = mask;
            c.nz[b * nmbtot + mbi] = u8::from(mask != 0);
            if b == 0 {
                c.mbq[mbi] = code as u8;
            }
        }
    }
}

/// `k_recon` on `c` with osel = SL_REC (kernels.h:1019), writing the reference planes.
fn model_recon(c: &mut Case) {
    let g = c.g;
    let (n, nmbtot) = (c.n(), c.nmbtot());
    for b in 0..6 {
        for (s, mb) in listed(c) {
            let mbi = s * g.nmb as usize + mb;
            let mi = c.info[mbi];
            let intra = mi.mode == MODE_INTRA;
            let blkn = b * nmbtot + mbi;
            let coded = intra || c.nz[blkn] != 0;
            let (pw, ph, x0, y0) = blk(&g, b, mb);
            let mut px: [[u8; 8]; 8] = core::array::from_fn(|y| {
                core::array::from_fn(|x| c.predbuf[blkn * 64 + y * 8 + x])
            });
            if coded {
                let mut gm = [0f32; 64];
                let q = i32::from(c.t.qscale[usize::from(c.mbq[mbi])]);
                let ras = c.t.scan_ras[g.alt_scan as usize];
                let mut mask = c.nzmask[blkn];
                let (mut sum, mut last) = (0i32, 0i32);
                while mask != 0 {
                    let p = mask.trailing_zeros() as usize;
                    mask &= mask - 1;
                    let r = usize::from(ras[p]);
                    let l = i32::from(c.lev[(b * 64 + p) * nmbtot + mbi]);
                    let val = if intra {
                        if r == 0 {
                            8 * l
                        } else {
                            (2 * l * i32::from(c.t.intra_q[r]) * q) / 32
                        }
                    } else if l == 0 {
                        0
                    } else {
                        ((2 * l + if l > 0 { 1 } else { -1 }) * 16 * q) / 32
                    };
                    let val = val.clamp(-2048, 2047);
                    sum += val;
                    if r == 63 {
                        last = val;
                    }
                    gm[r] = val as f32;
                }
                if sum & 1 == 0 {
                    gm[63] = (last ^ 1) as f32;
                }
                for v in 0..8 {
                    let row: [f32; 8] = core::array::from_fn(|u| gm[v * 8 + u]);
                    gm[v * 8..v * 8 + 8].copy_from_slice(&idct8(&row));
                }
                for x in 0..8 {
                    let t: [f32; 8] = core::array::from_fn(|v| gm[v * 8 + x]);
                    let o = idct8(&t);
                    for y in 0..8 {
                        let r = ((o[y] + 0.5).floor() as i32).clamp(-256, 255);
                        px[y][x] = (r + i32::from(px[y][x])).clamp(0, 255) as u8;
                    }
                }
            }
            let plane = match b {
                0..4 => &mut c.ref_y,
                4 => &mut c.ref_u,
                _ => &mut c.ref_v,
            };
            let base = (usize::from(c.par[s].rec) * n + s) * pw * ph + x0;
            for (y, row) in px.iter().enumerate() {
                let line = blk_line(b, y0, mi.dct_type, y);
                plane[base + line * pw..][..8].copy_from_slice(row);
            }
        }
    }
}

// ---- running the kernels ----------------------------------------------------------------

/// Which kernels a run includes.
#[derive(Clone, Copy, PartialEq)]
enum Upto {
    Xf,
    Quant,
    Recon,
    /// `k_recon` alone, on the case's levels.
    ReconOnly,
}

/// The device's version of every buffer after running the kernels up to `upto` on `c`.
fn run_device(c: &Case, upto: Upto) -> Case {
    let g = c.g;
    let (ysz, rs) = (c.ysz() as u64, c.ring_stride() as u64);
    let mut b = Bench::new();
    let dt = b.pods(std::slice::from_ref(&c.t));
    let dlist = b.pods(&c.list);
    let dpar = b.pods(&c.par);
    let ring = b.bytes(&c.ring);
    let ref_y = b.bytes(&c.ref_y);
    let ref_u = b.bytes(&c.ref_u);
    let ref_v = b.bytes(&c.ref_v);
    let dinfo = b.pods(&c.info);
    let dact = b.pods(&c.act);
    let drowq = b.bytes(&c.rowq);
    let dcoef = b.pods(&c.coef);
    let dpred = b.bytes(&c.predbuf);
    let dhist = b.pods(&c.hist);
    let dcmask = b.pods(&c.cmask);
    let dlev = b.pods(&c.lev);
    let dnzmask = b.pods(&c.nzmask);
    let dnz = b.bytes(&c.nz);
    let dmbq = b.bytes(&c.mbq);
    let bufs = BufsRef {
        ring_y: ring,
        ring_u: ring.slice(ysz, ring.len - ysz).unwrap_or_default(),
        ring_v: ring
            .slice(ysz + ysz / 4, ring.len - ysz - ysz / 4)
            .unwrap_or_default(),
        ring_stride: rs,
        ref_y,
        ref_u,
        ref_v,
        ..BufsRef::default()
    };
    let na = c.list.len() as i32;
    let nblocks = c.list.len() * g.nmb as usize * 6;
    if upto != Upto::ReconOnly {
        b.launch(
            Kernel::Xf(Xf {
                g,
                tables: dt,
                b: bufs,
                par: dpar,
                list: dlist,
                na,
                info: dinfo,
                act: dact,
                coef: dcoef,
                predbuf: dpred,
                hist: dhist,
                cmask: dcmask,
            }),
            nblocks.div_ceil(XB) as u32,
            (XB * 8) as u32,
        );
    }
    if upto == Upto::Quant || upto == Upto::Recon {
        b.cover(
            Kernel::Quant(Quant {
                g,
                tables: dt,
                par: dpar,
                list: dlist,
                na,
                rowq: drowq,
                info: dinfo,
                act: dact,
                coef: dcoef,
                cmask: dcmask,
                lev: dlev,
                nzmask: dnzmask,
                nz: dnz,
                mbq: dmbq,
            }),
            nblocks,
            BS,
        );
    }
    if upto == Upto::Recon || upto == Upto::ReconOnly {
        b.cover(
            Kernel::Recon(Recon {
                g,
                tables: dt,
                par: dpar,
                list: dlist,
                na,
                lev: dlev,
                nzmask: dnzmask,
                nz: dnz,
                predbuf: dpred,
                info: dinfo,
                mbq: dmbq,
                out_y: ref_y,
                out_u: ref_u,
                out_v: ref_v,
                osel: SL_REC,
            }),
            nblocks,
            RECON_BS,
        );
    }
    let o = b.run();
    let mut d = c.clone();
    d.ref_y = o.bytes(ref_y).to_vec();
    d.ref_u = o.bytes(ref_u).to_vec();
    d.ref_v = o.bytes(ref_v).to_vec();
    d.coef = o.pods(dcoef);
    d.predbuf = o.bytes(dpred).to_vec();
    d.hist = o.pods(dhist);
    d.cmask = o.pods(dcmask);
    d.lev = o.pods(dlev);
    d.nzmask = o.pods(dnzmask);
    d.nz = o.bytes(dnz).to_vec();
    d.mbq = o.bytes(dmbq).to_vec();
    d
}

/// A smooth random picture (a sum of a few gradients and noise): DCT coefficients of all sizes.
fn texture(rng: &mut Rng, w: usize, h: usize, amp: u32) -> Vec<u8> {
    let (ax, ay) = (rng.below(5) as usize, rng.below(5) as usize);
    let base = 60 + rng.below(100) as usize;
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let v = base + (x * ax + y * ay) % 64 + rng.below(amp) as usize;
            v.min(255) as u8
        })
        .collect()
}

/// A random vector component (half samples) that keeps a block of `size` at `p` inside a plane
/// of `len` (7.6.4: predictions never leave the reference).
fn component(rng: &mut Rng, p: usize, size: usize, len: usize) -> i16 {
    let lo = -2 * p as i32;
    let hi = 2 * (len - size - p) as i32;
    (lo + rng.below((hi - lo + 1) as u32) as i32).clamp(-14, 14) as i16
}

/// A case of `n` sessions of `mbw x mbh` macroblocks, listed as `list`, with random modes and
/// vectors (field prediction and field DCT when `interlaced`).
fn random_case(seed: u64, mbw: i32, mbh: i32, interlaced: bool, list: &[i32]) -> Case {
    let n = 3usize;
    let mut g = geo(n as i32, mbw, mbh);
    g.interlaced = i32::from(interlaced);
    g.field_pred = i32::from(interlaced);
    g.alt_scan = i32::from(interlaced);
    let (w, h, nmb) = (g.w as usize, g.h as usize, g.nmb as usize);
    let nmbtot = n * nmb;
    let mut rng = Rng::new(seed);
    let mut ring = Vec::new();
    for _ in 0..SLOTS * n {
        ring.extend(texture(&mut rng, w, h, 40));
        ring.extend(texture(&mut rng, w / 2, h / 2, 20));
        ring.extend(texture(&mut rng, w / 2, h / 2, 20));
    }
    let mut refs = [Vec::new(), Vec::new(), Vec::new()];
    for _ in 0..2 * n {
        refs[0].extend(texture(&mut rng, w, h, 40));
        refs[1].extend(texture(&mut rng, w / 2, h / 2, 20));
        refs[2].extend(texture(&mut rng, w / 2, h / 2, 20));
    }
    let par: Vec<SessPar> = (0..n)
        .map(|s| SessPar {
            pic_type: 2,
            cur: (s % 2) as u8,
            fwd: (s % 2) as u8,
            bwd: 1 - (s % 2) as u8,
            rec: 1 - (s % 2) as u8,
            keep: if s == 1 { 20 } else { 64 },
            aq: u8::from(s != 1),
            want_pred: 1,
            avg_act: 300 + 200 * s as u16,
            qfloor: [1, 4, 2][s],
            lam: 4,
            ..SessPar::default()
        })
        .collect();
    let mut info = vec![MbInfo::default(); nmbtot];
    for (mbi, m) in info.iter_mut().enumerate() {
        let mb = mbi % nmb;
        let (px, py) = (16 * (mb % mbw as usize), 16 * (mb / mbw as usize));
        m.mode = [MODE_INTRA, MODE_FWD, MODE_BWD, MODE_BI][rng.below(4) as usize];
        m.dct_type = u8::from(interlaced && rng.below(2) == 1);
        m.field_pred = u8::from(interlaced && m.mode != MODE_INTRA && rng.below(2) == 1);
        if m.mode != MODE_INTRA {
            for r in 0..2 {
                for dir in 0..2 {
                    // Frame vectors: rows of the frame; field vectors: lines of the field.
                    let (vh, vp) = if m.field_pred != 0 {
                        (h / 2, py / 2)
                    } else {
                        (h, py)
                    };
                    let vsize = if m.field_pred != 0 { 8 } else { 16 };
                    m.mv[r][dir] = [
                        component(&mut rng, px, 16, w),
                        component(&mut rng, vp, vsize, vh),
                    ];
                }
            }
            m.fsel = rng.below(16) as u8;
        }
        m.cost = rng.next_u32();
    }
    Case {
        g,
        t: tables(),
        list: list.to_vec(),
        par,
        ring,
        ref_y: refs[0].clone(),
        ref_u: refs[1].clone(),
        ref_v: refs[2].clone(),
        info,
        act: (0..nmbtot).map(|_| 1 + rng.below(3000) as u16).collect(),
        rowq: (0..n * mbh as usize)
            .map(|_| 1 + rng.below(31) as u8)
            .collect(),
        coef: pods_of(&rng.bytes(6 * 64 * nmbtot * 2)),
        predbuf: rng.bytes(6 * 64 * nmbtot),
        hist: pods_of(&rng.bytes(6 * nmbtot * 16)),
        cmask: pods_of(&rng.bytes(6 * nmbtot * 8)),
        lev: pods_of(&rng.bytes(6 * 64 * nmbtot * 2)),
        nzmask: pods_of(&rng.bytes(6 * nmbtot * 8)),
        nz: rng.bytes(6 * nmbtot),
        mbq: rng.bytes(nmbtot),
    }
}

/// Every output of the device run equals the model's.
fn assert_same(dev: &Case, want: &Case, what: &str) {
    assert!(dev.coef == want.coef, "{what}: k_xf coef");
    assert!(dev.predbuf == want.predbuf, "{what}: k_xf predbuf");
    assert!(dev.hist == want.hist, "{what}: k_xf hist");
    assert!(dev.cmask == want.cmask, "{what}: k_xf cmask");
    assert!(dev.lev == want.lev, "{what}: k_quant lev");
    assert!(dev.nzmask == want.nzmask, "{what}: k_quant nzmask");
    assert!(dev.nz == want.nz, "{what}: k_quant nz");
    assert!(dev.mbq == want.mbq, "{what}: k_quant mbq");
    assert!(dev.ref_y == want.ref_y, "{what}: k_recon luma");
    assert!(dev.ref_u == want.ref_u, "{what}: k_recon Cb");
    assert!(dev.ref_v == want.ref_v, "{what}: k_recon Cr");
}

fn chain(c: &Case, what: &str) {
    let dev = run_device(c, Upto::Recon);
    let mut want = c.clone();
    model_xf(&mut want);
    model_quant(&mut want);
    model_recon(&mut want);
    assert_same(&dev, &want, what);
    // The case exercises what it should: stored coefficients, histogram entries, levels.
    assert!(
        want.cmask
            .iter()
            .zip(&c.cmask)
            .any(|(a, b)| a != b && *a > 1)
    );
    assert!(want.nz != c.nz);
}

/// Exact against the f32 port (host build only; see the module comment).
#[test]
fn transform_chain_progressive_equals_the_f32_port() {
    for seed in 0..4 {
        chain(&random_case(seed, 3, 2, false, &[2, 0]), "progressive");
    }
}

/// Exact against the f32 port (host build only): field prediction, field DCT, alternate scan.
#[test]
fn transform_chain_interlaced_equals_the_f32_port() {
    for seed in 10..14 {
        chain(&random_case(seed, 2, 2, true, &[0, 1]), "interlaced");
    }
}

/// Exact against the f32 port (host build only): more blocks than one CUDA block of `k_xf`
/// holds (XB = 32 blocks of 8 threads), so the blocks of a macroblock span CUDA blocks.
#[test]
fn transform_chain_spanning_cuda_blocks_equals_the_f32_port() {
    chain(&random_case(20, 5, 3, false, &[1, 2, 0]), "large");
}

/// The host build's `fmaf` is a fused operation: the kernels' outputs differ from the port with
/// every fused operation split into a multiply and an add, on the same cases that match the
/// fused port exactly (and `mul_add` is one rounding: a case where the two differ).
#[test]
fn host_fmaf_is_fused() {
    let (a, b, c) = (1.0f32 + f32::EPSILON, 1.0f32 - f32::EPSILON, -1.0f32);
    assert_eq!(a.mul_add(b, c), -f32::EPSILON * f32::EPSILON);
    assert_eq!(a * b + c, 0.0);
    let cases = (0..4)
        .map(|seed| random_case(seed, 3, 2, false, &[2, 0]))
        .chain((10..14).map(|seed| random_case(seed, 2, 2, true, &[0, 1])));
    let (mut xf_differs, mut recon_differs) = (false, false);
    for c in cases {
        let dev = run_device(&c, Upto::Recon);
        let mut fused = c.clone();
        model_xf(&mut fused);
        model_quant(&mut fused);
        model_recon(&mut fused);
        assert_same(&dev, &fused, "fused port");
        UNFUSED.set(true);
        let mut split = c.clone();
        model_xf(&mut split);
        model_quant(&mut split);
        model_recon(&mut split);
        UNFUSED.set(false);
        xf_differs |= dev.coef != split.coef || dev.hist != split.hist;
        recon_differs |= dev.ref_y != split.ref_y || dev.ref_u != split.ref_u;
    }
    // The fused operations of fdct8 / the roundings (k_xf) and of idct8 (k_recon) change
    // results on these cases, and the host build computes the fused ones.
    assert!(xf_differs, "k_xf: the cases do not tell fused from unfused");
    // The reconstruction rounds to whole samples, so the last bit of the IDCT rarely shows:
    // search random dense levels for blocks where it does, and run k_recon on those alone.
    let mut found = 0;
    for seed in 100..400 {
        let mut c = random_case(seed, 2, 2, seed % 2 == 1, &[0, 1, 2]);
        let mut rng = Rng::new(seed);
        for m in &mut c.nzmask {
            *m = u64::from(rng.next_u32()) << 32 | u64::from(rng.next_u32());
        }
        for l in &mut c.lev {
            *l = rng.below(161) as i16 - 80;
        }
        c.nz.fill(1);
        for q in &mut c.mbq {
            *q = 1 + rng.below(31) as u8;
        }
        let mut fused = c.clone();
        model_recon(&mut fused);
        UNFUSED.set(true);
        let mut split = c.clone();
        model_recon(&mut split);
        UNFUSED.set(false);
        if (&fused.ref_y, &fused.ref_u, &fused.ref_v) == (&split.ref_y, &split.ref_u, &split.ref_v)
        {
            continue;
        }
        let dev = run_device(&c, Upto::ReconOnly);
        assert!(
            (&dev.ref_y, &dev.ref_u, &dev.ref_v) == (&fused.ref_y, &fused.ref_u, &fused.ref_v),
            "k_recon on seed {seed}: not the fused port's reconstruction"
        );
        found += 1;
        if found == 4 {
            break;
        }
    }
    assert!(
        recon_differs || found > 0,
        "k_recon: no case tells fused from unfused"
    );
}

/// `k_xf` without `want_pred` leaves the prediction buffer alone (f32 port, host build only).
#[test]
fn xf_without_prediction_store() {
    let mut c = random_case(30, 2, 1, false, &[1]);
    c.par[1].want_pred = 0;
    let dev = run_device(&c, Upto::Xf);
    let mut want = c.clone();
    model_xf(&mut want);
    assert!(dev.predbuf == c.predbuf);
    assert_same(&dev, &want, "xf only");
}

// ---- structural checks: hold on any build ------------------------------------------------

/// A case of one session of one macroblock, every plane flat at `v`.
fn flat_case(mode: u8, v: u8) -> Case {
    let mut c = random_case(40, 1, 1, false, &[0]);
    c.ring.fill(v);
    c.ref_y.fill(v);
    c.ref_u.fill(v);
    c.ref_v.fill(v);
    c.info[0] = MbInfo {
        mode,
        ..MbInfo::default()
    };
    c.par[0].aq = 0;
    c.par[0].keep = 64;
    c.rowq[0] = 8;
    c
}

#[test]
fn flat_intra_block_gives_dc_only_and_reconstructs_to_its_value() {
    for v in [0u8, 1, 100, 128, 254, 255] {
        let c = flat_case(MODE_INTRA, v);
        let d = run_device(&c, Upto::Recon);
        let nmbtot = c.nmbtot();
        for b in 0..6 {
            let blkn = b * nmbtot;
            assert_eq!(d.cmask[blkn], 1, "v {v} block {b}: DC only");
            assert_eq!(
                d.coef[b * 64 * nmbtot],
                i16::from(v),
                "the DC level is the value"
            );
            assert_eq!((d.hist[blkn * 2], d.hist[blkn * 2 + 1]), (0, 0));
            assert_eq!(d.nzmask[blkn], 1);
            assert_eq!(d.lev[b * 64 * nmbtot], i16::from(v));
            assert!(d.predbuf[blkn * 64..blkn * 64 + 64].iter().all(|&p| p == 0));
        }
        let rec = usize::from(c.par[0].rec);
        let ysz = c.ysz();
        let n = c.n();
        assert!(
            d.ref_y[rec * n * ysz..][..ysz].iter().all(|&p| p == v),
            "v {v}: luma"
        );
        assert!(
            d.ref_u[rec * n * ysz / 4..][..ysz / 4]
                .iter()
                .all(|&p| p == v)
        );
        assert!(
            d.ref_v[rec * n * ysz / 4..][..ysz / 4]
                .iter()
                .all(|&p| p == v)
        );
    }
}

#[test]
fn perfectly_predicted_block_gives_nothing_and_reconstructs_to_the_prediction() {
    for mode in [MODE_FWD, MODE_BWD, MODE_BI] {
        let mut c = flat_case(mode, 0);
        let mut rng = Rng::new(u64::from(mode));
        // The source equals both references, textured; zero vectors.
        let n = c.n();
        let ysz = c.ysz();
        let pic = texture(&mut rng, 16, 16, 60);
        let chroma = texture(&mut rng, 8, 8, 60);
        let cur = usize::from(c.par[0].cur) * n * c.ring_stride();
        c.ring[cur..cur + ysz].copy_from_slice(&pic);
        c.ring[cur + ysz..cur + ysz * 5 / 4].copy_from_slice(&chroma);
        c.ring[cur + ysz * 5 / 4..cur + ysz * 3 / 2].copy_from_slice(&chroma);
        for slot in 0..2 {
            c.ref_y[slot * n * ysz..][..ysz].copy_from_slice(&pic);
            c.ref_u[slot * n * ysz / 4..][..ysz / 4].copy_from_slice(&chroma);
            c.ref_v[slot * n * ysz / 4..][..ysz / 4].copy_from_slice(&chroma);
        }
        let d = run_device(&c, Upto::Recon);
        let nmbtot = c.nmbtot();
        for b in 0..6 {
            assert_eq!(d.cmask[b * nmbtot], 0, "mode {mode} block {b}");
            assert_eq!((d.hist[b * nmbtot * 2], d.hist[b * nmbtot * 2 + 1]), (0, 0));
            assert_eq!((d.nzmask[b * nmbtot], d.nz[b * nmbtot]), (0, 0));
        }
        let rec = usize::from(c.par[0].rec);
        assert_eq!(&d.ref_y[rec * n * ysz..][..ysz], &pic[..]);
        assert_eq!(&d.ref_u[rec * n * ysz / 4..][..ysz / 4], &chroma[..]);
    }
}

/// `keep = 0` (the last rung of the buffer ladder): no levels survive but the DC of intra
/// blocks; the quantizer of every macroblock is still recorded.
#[test]
fn keep_zero_leaves_intra_dc_only() {
    let mut c = random_case(50, 3, 2, false, &[0, 2]);
    for s in [0, 2] {
        c.par[s].keep = 0;
    }
    let d = run_device(&c, Upto::Quant);
    let (nmb, nmbtot) = (c.g.nmb as usize, c.nmbtot());
    for s in [0usize, 2] {
        for mb in 0..nmb {
            let mbi = s * nmb + mb;
            let intra = c.info[mbi].mode == MODE_INTRA;
            for b in 0..6 {
                assert_eq!(d.nzmask[b * nmbtot + mbi], u64::from(intra), "{s} {mb} {b}");
                assert_eq!(d.nz[b * nmbtot + mbi], u8::from(intra));
            }
            assert!(d.mbq[mbi] >= 1 && d.mbq[mbi] <= 31);
        }
    }
}
