//! The card and the reference adapter compute the same bytes (work package K3).
//!
//! The same plan runs on the CUDA adapter and on the reference adapter (the kernels compiled
//! for the host) and every buffer it writes is compared byte for byte. The plan is the first
//! phase of the encoder and the quantization and reconstruction after it, for one batch of
//! three sessions coding an I, a P and a B picture of real content, twice: the second tick
//! with the temporal candidate on, fed by the first tick's decisions. So it runs the four
//! kernels that compute in `float` (`k_xf`, `k_quant`, `k_recon`, and `k_me` with its temporal
//! candidate and scaled field seed) on what the encoder gives them: intra and inter blocks,
//! forward, backward and bidirectional prediction at half-sample positions, field prediction
//! and field DCT in interlaced pictures, adaptive quantization, and several quantizers
//! (`k_quant` and `k_recon` once per set of row quantizers). The integer kernels on the way
//! (`k_set_mode`, `k_pyr`, `k_mvsmooth`, `k_mbstat`, `k_rowstat`) are compared as well.
//!
//! Content: the gate's clips when `TB_PARITY_MEDIA` names the directory that holds them
//! (on nixark: `~/totalblonder-spike/media`; raw 4:2:0 as `gate/formats.py` names them),
//! otherwise a synthetic moving texture (the test says which it used).

use std::path::PathBuf;

use tb_device::kernel::{
    BufsRef, Kernel, MbStat, Me, MvSmooth, Pyr, Quant, Recon, RowStat, SetMode, Xf,
};
use tb_device::{ArenaSizes, DevBuf, Device, HostBuf, Launch, Op, Plan};
use tb_device_cuda::{CudaDevice, IMAGE};
use tb_device_ref::RefDevice;
use tb_kernel_abi::{
    Geo, MODE_BI, MODE_INTRA, MbInfo, PIC_B, PIC_I, PIC_P, RowStat as RowStatRec, SL_CUR, SL_REC,
    SL_ZERO, SessPar, Tables, XB,
};

use super::card;

/// Threads per block of the C++ launch macros (`BS`, `ME_BS`, `RECON_BS`).
const BS: u32 = 256;
const ME_BS: u32 = 64;
const RECON_BS: u32 = 64;
/// Sessions of the batch: 0 codes an I picture, 1 a P picture, 2 a B picture.
const N: usize = 3;

/// A clip: frames of `w x h` luma, 4:2:0, read or made on demand.
struct Clip {
    name: String,
    w: usize,
    h: usize,
    interlaced: bool,
    tff: bool,
    file: Option<PathBuf>,
}

impl Clip {
    /// Frame `i` padded to `pw x ph` by repeating the last column and row (as k_pad_* do):
    /// Y, then Cb, then Cr.
    fn frame(&self, i: usize, pw: usize, ph: usize) -> Vec<u8> {
        let (w, h) = (self.w, self.h);
        let raw = match &self.file {
            Some(path) => {
                use std::io::{Read, Seek, SeekFrom};
                let fb = w * h * 3 / 2;
                let mut f = std::fs::File::open(path).unwrap();
                f.seek(SeekFrom::Start((i * fb) as u64)).unwrap();
                let mut v = vec![0; fb];
                f.read_exact(&mut v).unwrap();
                v
            }
            None => synthetic(w, h, i, self.interlaced),
        };
        let mut out = Vec::with_capacity(pw * ph * 3 / 2);
        let planes = [
            (0, w, h, pw, ph),
            (w * h, w / 2, h / 2, pw / 2, ph / 2),
            (w * h * 5 / 4, w / 2, h / 2, pw / 2, ph / 2),
        ];
        for (at, sw, sh, dw, dh) in planes {
            for y in 0..dh {
                let row = &raw[at + y.min(sh - 1) * sw..][..sw];
                out.extend((0..dw).map(|x| row[x.min(sw - 1)]));
            }
        }
        out
    }
}

/// golden/make_clip.py's picture, in short: a textured background that pans, a block moving the
/// other way, a little noise; with `interlaced` the bottom field is the next phase of the motion.
fn synthetic(w: usize, h: usize, f: usize, interlaced: bool) -> Vec<u8> {
    let mut state = (f as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut noise = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 59) as usize
    };
    let mut v = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        let t = 2 * f + if interlaced { y & 1 } else { 0 };
        for x in 0..w {
            let mut l =
                96 + ((x + 3 * t) * 5 + y * 3) % 97 + (((x + 3 * t) / 16 + y / 16) % 2) * 40;
            let (bx, by) = (20 + (400 - 2 * t) % (w - 48), 24 + t % 40);
            if (bx..bx + 32).contains(&x) && (by..by + 32).contains(&y) {
                l = 40 + ((x - bx) * 7 + (y - by) * 11) % 150;
            }
            v.push((l + noise()).min(255) as u8);
        }
    }
    for c in 0..2 {
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                v.push((128 + ((x + 2 * f) * (3 + c) + y * 2) % 40) as u8);
            }
        }
    }
    v
}

/// The clips the test runs on: three of the gate's when the media are there.
fn clips() -> Vec<Clip> {
    let dir = std::env::var_os("TB_PARITY_MEDIA").map(PathBuf::from);
    let gate = [
        ("crowd_run_480i", 704, 480, true, true),
        ("mobcal_bff_480i", 704, 480, true, false),
        ("park_joy_720p", 1280, 720, false, false),
    ];
    gate.iter()
        .map(|&(name, w, h, interlaced, tff)| {
            let file = dir.as_ref().map(|d| d.join(format!("{name}.yuv")));
            if let Some(f) = &file {
                assert!(f.exists(), "TB_PARITY_MEDIA: {} is missing", f.display());
            }
            Clip {
                name: if file.is_some() {
                    name.to_string()
                } else {
                    format!("synthetic {w}x{h}")
                },
                w,
                h,
                interlaced,
                tff,
                file,
            }
        })
        .collect()
}

/// The batch geometry as the C++ `Batch::init` made it with the encoder's defaults.
fn geo(c: &Clip, tpred: bool) -> Geo {
    let mbw = c.w.div_ceil(16) as i32;
    let mbh = if c.interlaced {
        2 * c.h.div_ceil(32) as i32
    } else {
        c.h.div_ceil(16) as i32
    };
    let range2 = 8;
    let mut reach = 4 * range2 + 3;
    if c.interlaced {
        reach = reach * 3 / 2 + 2;
    }
    let mut f_code = 1;
    while (16 << (f_code - 1)) < 2 * reach + 6 && f_code < 9 {
        f_code += 1;
    }
    Geo {
        n: N as i32,
        w: mbw * 16,
        h: mbh * 16,
        mbw,
        mbh,
        nmb: mbw * mbh,
        f_code,
        intra_vlc_format: 1,
        alt_scan: i32::from(c.interlaced),
        interlaced: i32::from(c.interlaced),
        field_pred: i32::from(c.interlaced),
        range2,
        lambda: 4,
        field_bits: 12,
        aq_s: 1.5,
        smooth: 1,
        tpred: i32::from(tpred),
    }
}

/// The table record the encoder uploads (golden/tables.jsonl).
fn tables() -> Tables {
    let recs = tb_golden::case("tables").unwrap();
    let r = recs.iter().find(|r| r.kind() == "tables").unwrap();
    bytemuck::pod_read_unaligned(&r.bytes("bytes").unwrap())
}

/// The arena image of one plan: named buffers as `Batch::init` lays them out (256-byte blocks
/// with 64 guard bytes), and the launches.
#[derive(Default)]
struct Layout {
    image: Vec<u8>,
    bufs: Vec<(String, &'static str, DevBuf)>,
    launches: Vec<Launch>,
}

impl Layout {
    fn put(&mut self, name: String, kernel: &'static str, data: &[u8]) -> DevBuf {
        let at = self.image.len() as u64 + 64;
        let block = (data.len() as u64 + 128 + 255) & !255;
        self.image.resize(self.image.len() + block as usize, 0xa5);
        self.image[at as usize..at as usize + data.len()].copy_from_slice(data);
        let b = DevBuf::new(at, data.len() as u64);
        self.bufs.push((name, kernel, b));
        b
    }
    fn input<T: bytemuck::Pod>(&mut self, name: &str, v: &[T]) -> DevBuf {
        self.put(name.to_string(), "", bytemuck::cast_slice(v))
    }
    /// An output buffer of `len` bytes, written by `kernel`, filled with a byte no kernel
    /// writes everywhere so that elements a kernel leaves alone compare as well.
    fn output(&mut self, name: String, kernel: &'static str, len: usize) -> DevBuf {
        self.put(name, kernel, &vec![0x3c; len])
    }
    fn launch(&mut self, kernel: Kernel, count: usize, block: u32) {
        self.launches
            .push(Launch::covering(kernel, count as u64, block));
    }
}

/// Runs the layout's plan on `dev`; returns the arena afterwards.
fn run<D: Device>(dev: &mut D, l: &Layout) -> Vec<u8> {
    let size = l.image.len() as u64;
    dev.host_mut()[..l.image.len()].copy_from_slice(&l.image);
    let mut p = Plan::new();
    p.main(Op::Upload {
        dst: DevBuf::new(0, size),
        src: HostBuf::new(0, size),
    });
    for &launch in &l.launches {
        p.main(Op::Launch(launch));
    }
    p.main(Op::Download {
        dst: HostBuf::new(0, size),
        src: DevBuf::new(0, size),
    });
    let t = dev.submit(&p).unwrap();
    dev.wait(t, None).status.unwrap();
    dev.host()[..l.image.len()].to_vec()
}

/// One tick's frames: the source and the two references (display order indices).
struct TickFrames {
    cur: usize,
    fwd: usize,
    bwd: usize,
}

/// Lays out the plan of one clip: two ticks, each the first phase, the transform and four
/// sets of row quantizers through `k_quant` and `k_recon`.
fn layout(c: &Clip) -> (Layout, Vec<Geo>) {
    let t = tables();
    let mut l = Layout::default();
    let ticks = [
        TickFrames {
            cur: 10,
            fwd: 8,
            bwd: 11,
        },
        TickFrames {
            cur: 13,
            fwd: 11,
            bwd: 14,
        },
    ];
    let g0 = geo(c, false);
    let (pw, ph) = (g0.w as usize, g0.h as usize);
    let (ysz, csz) = (pw * ph, pw * ph / 4);
    let (nmb, mbh) = (g0.nmb as usize, g0.mbh as usize);
    let nmbtot = N * nmb;
    let dt = l.input("tables", std::slice::from_ref(&t));
    // Ring: [2 slots][N sessions], slot k holds tick k's source; every session the same clip.
    let mut ring = Vec::new();
    for tf in &ticks {
        let f = c.frame(tf.cur, pw, ph);
        for _ in 0..N {
            ring.extend_from_slice(&f);
        }
    }
    let ring = l.input("ring", &ring);
    let ring_stride = (ysz * 3 / 2) as u64;
    let list_i = l.input("list intra", &[0i32]);
    let list_me = l.input("list me", &[1i32, 2]);
    let list_code = l.input("list code", &[0i32, 1, 2]);
    let mut geos = Vec::new();
    let mut prev = l.input("prev info", &vec![MbInfo::default(); nmbtot]);
    let mut rng = 0x1234_5678u32;
    for (k, tf) in ticks.iter().enumerate() {
        let tpred = k == 1;
        let g = geo(c, tpred);
        geos.push(g);
        // References: slot 0 the forward, slot 1 the backward one, as reconstructions would be.
        let (mut ry, mut ru, mut rv) = (Vec::new(), Vec::new(), Vec::new());
        for fi in [tf.fwd, tf.bwd] {
            let f = c.frame(fi, pw, ph);
            for _ in 0..N {
                ry.extend_from_slice(&f[..ysz]);
                ru.extend_from_slice(&f[ysz..ysz + csz]);
                rv.extend_from_slice(&f[ysz + csz..]);
            }
        }
        let (dist_f, dist_b) = ((tf.cur - tf.fwd) as i8, -((tf.bwd - tf.cur) as i8));
        let par: Vec<SessPar> = (0..N)
            .map(|s| SessPar {
                pic_type: [PIC_I, PIC_P, PIC_B][s],
                cur: k as u8,
                fwd: 0,
                bwd: 1,
                rec: 0,
                keep: 64,
                aq: 1,
                want_pred: 1,
                avg_act: [400, 900, 2500][s],
                dist_f,
                dist_b,
                tff: u8::from(c.tff),
                qfloor: 1,
                lam: [4, 6, 9][s],
                // The previous tick's distances (the C++ `last_dist_f`, `last_dist_b`).
                pdist_f: if tpred && s > 0 { dist_f } else { 0 },
                pdist_b: if tpred && s == 2 { dist_b } else { 0 },
                frame_only: 0,
                zigzag: 0,
            })
            .collect();
        let tick = format!("tick {k}");
        let dpar = l.input(&format!("{tick} par"), &par);
        let bufs = BufsRef {
            ring_y: ring,
            ring_u: ring.slice(ysz as u64, ring.len - ysz as u64).unwrap(),
            ring_v: ring
                .slice((ysz + csz) as u64, ring.len - (ysz + csz) as u64)
                .unwrap(),
            ring_stride,
            cur1: l.output(format!("{tick} cur1"), "k_pyr", N * ysz / 4),
            cur2: l.output(format!("{tick} cur2"), "k_pyr", N * ysz / 16),
            ref_y: l.input(&format!("{tick} ref_y"), &ry),
            ref_u: l.input(&format!("{tick} ref_u"), &ru),
            ref_v: l.input(&format!("{tick} ref_v"), &rv),
            ref_y1: l.output(format!("{tick} ref_y1"), "k_pyr", 2 * N * ysz / 4),
            ref_y2: l.output(format!("{tick} ref_y2"), "k_pyr", 2 * N * ysz / 16),
        };
        // The references' pyramids (as the C++ after k_recon: k_pyr from ref_y with SL_REC), one
        // launch per reference slot.
        for slot in 0..2u8 {
            let pp: Vec<SessPar> = par.iter().map(|p| SessPar { rec: slot, ..*p }).collect();
            let dpp = l.input(&format!("{tick} par rec {slot}"), &pp);
            l.launch(
                Kernel::Pyr(Pyr {
                    src: bufs.ref_y,
                    sstride: ysz as u64,
                    sw: g.w,
                    sh: g.h,
                    d1: bufs.ref_y1,
                    d1stride: (ysz / 4) as u64,
                    d2: bufs.ref_y2,
                    d2stride: (ysz / 16) as u64,
                    n: N as i32,
                    list: list_code,
                    na: N as i32,
                    par: dpp,
                    ssel: SL_REC,
                    dsel: SL_REC,
                }),
                N * ysz / 64,
                BS,
            );
        }
        let info = l.output(
            format!("{tick} info"),
            "k_set_mode/k_mvsmooth",
            nmbtot * size_of::<MbInfo>(),
        );
        let info_me = l.output(
            format!("{tick} info_me"),
            "k_me",
            nmbtot * size_of::<MbInfo>(),
        );
        let act = l.output(format!("{tick} act"), "k_mbstat", nmbtot * 2);
        // The C++ `Batch::launch_begin`.
        l.launch(
            Kernel::SetMode(SetMode {
                info,
                nmb: g.nmb,
                list: list_i,
                na: 1,
                mode: i32::from(MODE_INTRA),
            }),
            nmb,
            BS,
        );
        l.launch(
            Kernel::Pyr(Pyr {
                src: ring,
                sstride: ring_stride,
                sw: g.w,
                sh: g.h,
                d1: bufs.cur1,
                d1stride: (ysz / 4) as u64,
                d2: bufs.cur2,
                d2stride: (ysz / 16) as u64,
                n: N as i32,
                list: list_me,
                na: 2,
                par: dpar,
                ssel: SL_CUR,
                dsel: SL_ZERO,
            }),
            2 * ysz / 64,
            BS,
        );
        l.launch(
            Kernel::Me(Me {
                g,
                b: bufs,
                par: dpar,
                list: list_me,
                na: 2,
                prev,
                out: info_me,
            }),
            2 * nmb,
            ME_BS,
        );
        l.launch(
            Kernel::MvSmooth(MvSmooth {
                g,
                b: bufs,
                par: dpar,
                list: list_me,
                na: 2,
                input: info_me,
                out: info,
            }),
            2 * nmb,
            BS,
        );
        l.launch(
            Kernel::MbStat(MbStat {
                g,
                ring_y: ring,
                ring_stride,
                par: dpar,
                list: list_code,
                na: N as i32,
                info,
                act,
            }),
            N * nmb,
            BS,
        );
        let nblk = N * nmb * 6;
        let coef = l.output(format!("{tick} coef"), "k_xf", 6 * 64 * nmbtot * 2);
        let pred = l.output(format!("{tick} pred"), "k_xf", 6 * 64 * nmbtot);
        let hist = l.output(format!("{tick} hist"), "k_xf", 6 * nmbtot * 16);
        let cmask = l.output(format!("{tick} cmask"), "k_xf", 6 * nmbtot * 8);
        let rowstat = l.output(
            format!("{tick} rowstat"),
            "k_rowstat",
            N * mbh * size_of::<RowStatRec>(),
        );
        l.launches.push(Launch {
            kernel: Kernel::Xf(Xf {
                g,
                tables: dt,
                b: bufs,
                par: dpar,
                list: list_code,
                na: N as i32,
                info,
                act,
                coef,
                predbuf: pred,
                hist,
                cmask,
            }),
            grid: nblk.div_ceil(XB) as u32,
            block: (XB * 8) as u32,
        });
        l.launches.push(Launch {
            kernel: Kernel::RowStat(RowStat {
                g,
                list: list_code,
                na: N as i32,
                hist,
                act,
                info,
                out: rowstat,
            }),
            grid: (N * mbh) as u32,
            block: (32 * (g.mbw + 31) / 32) as u32,
        });
        // Row quantizer codes: fine, middle, coarse, and a different one on every row.
        let sets: [(&str, Vec<u8>); 4] = [
            ("q2", vec![2; N * mbh]),
            ("q8", vec![8; N * mbh]),
            ("q31", vec![31; N * mbh]),
            (
                "qrows",
                (0..N * mbh)
                    .map(|_| {
                        rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        1 + (rng >> 24) as u8 % 31
                    })
                    .collect(),
            ),
        ];
        for (qname, rowq) in sets {
            let at = format!("{tick} {qname}");
            let drowq = l.input(&format!("{at} rowq"), &rowq);
            let lev = l.output(format!("{at} lev"), "k_quant", 6 * 64 * nmbtot * 2);
            let nzmask = l.output(format!("{at} nzmask"), "k_quant", 6 * nmbtot * 8);
            let nz = l.output(format!("{at} nz"), "k_quant", 6 * nmbtot);
            let mbq = l.output(format!("{at} mbq"), "k_quant", nmbtot);
            let (out_y, out_u, out_v) = (
                l.output(format!("{at} recon y"), "k_recon", N * ysz),
                l.output(format!("{at} recon u"), "k_recon", N * csz),
                l.output(format!("{at} recon v"), "k_recon", N * csz),
            );
            l.launch(
                Kernel::Quant(Quant {
                    g,
                    tables: dt,
                    par: dpar,
                    list: list_code,
                    na: N as i32,
                    rowq: drowq,
                    info,
                    act,
                    coef,
                    cmask,
                    lev,
                    nzmask,
                    nz,
                    mbq,
                }),
                nblk,
                BS,
            );
            l.launch(
                Kernel::Recon(Recon {
                    g,
                    tables: dt,
                    par: dpar,
                    list: list_code,
                    na: N as i32,
                    lev,
                    nzmask,
                    nz,
                    predbuf: pred,
                    info,
                    mbq,
                    out_y,
                    out_u,
                    out_v,
                    osel: SL_REC,
                }),
                nblk,
                RECON_BS,
            );
        }
        prev = info;
    }
    (l, geos)
}

/// What the decisions exercise (the card's `MbInfo` records, equal to the reference's when the
/// test passes): field DCT over all three sessions, the rest over the P and B sessions.
#[derive(Debug, Default)]
struct Coverage {
    bi: usize,
    half_pel: usize,
    field_pred: usize,
    field_dct: usize,
    intra_in_inter: usize,
}

fn coverage(info: &[MbInfo], nmb: usize) -> Coverage {
    let mut c = Coverage {
        field_dct: info.iter().filter(|m| m.dct_type != 0).count(),
        ..Coverage::default()
    };
    for m in &info[nmb..] {
        if m.mode == MODE_INTRA {
            c.intra_in_inter += 1;
            continue;
        }
        c.bi += usize::from(m.mode == MODE_BI);
        c.half_pel += usize::from(m.mv.iter().flatten().flatten().any(|v| v & 1 != 0));
        c.field_pred += usize::from(m.field_pred != 0);
    }
    c
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn float_kernels_on_the_card_equal_the_reference_adapter() {
    let _card = card();
    let (mut failures, mut field_dct) = (Vec::new(), 0);
    for clip in clips() {
        let (l, geos) = layout(&clip);
        let size = (l.image.len() as u64 + 4095) & !4095;
        let arenas = ArenaSizes {
            device: size,
            host: size,
        };
        let card_out = run(&mut CudaDevice::new(0, arenas, &IMAGE).unwrap(), &l);
        let ref_out = run(&mut RefDevice::new(arenas).unwrap(), &l);
        let mut differing = Vec::new();
        for (name, kernel, b) in &l.bufs {
            let (x, y) = (b.off as usize, b.end() as usize);
            let (a, r) = (&card_out[x..y], &ref_out[x..y]);
            if a != r {
                let n = a.iter().zip(r).filter(|(p, q)| p != q).count();
                let first = a.iter().zip(r).position(|(p, q)| p != q).unwrap_or(0);
                differing.push(format!(
                    "{name} ({kernel}): {n} of {} bytes, first at {first}",
                    a.len()
                ));
            }
        }
        if differing.is_empty() && card_out != ref_out {
            differing.push("bytes outside the buffers".to_string());
        }
        let nmb = geos[0].nmb as usize;
        for (k, _) in geos.iter().enumerate() {
            let (_, _, b) = l
                .bufs
                .iter()
                .find(|(n, _, _)| *n == format!("tick {k} info"))
                .unwrap();
            let info: Vec<MbInfo> = card_out[b.off as usize..b.end() as usize]
                .as_chunks::<{ size_of::<MbInfo>() }>()
                .0
                .iter()
                .map(|c| bytemuck::pod_read_unaligned(c))
                .collect();
            let c = coverage(&info, nmb);
            println!(
                "{} tick {k}: {} macroblocks per session; {:?}",
                clip.name, nmb, c
            );
            assert!(
                c.bi > 0 && c.half_pel > 0,
                "{}: the case is too easy",
                clip.name
            );
            if clip.interlaced {
                assert!(c.field_pred > 0, "{}: no field prediction", clip.name);
            }
            field_dct += c.field_dct;
        }
        let compared: usize = l.bufs.iter().filter(|(_, k, _)| !k.is_empty()).count();
        if differing.is_empty() {
            println!(
                "{}: {compared} output buffers ({} MB) identical on the card and the reference \
                 adapter",
                clip.name,
                l.image.len() >> 20
            );
        } else {
            for d in &differing {
                println!("{}: differs: {d}", clip.name);
            }
            failures.push(clip.name.clone());
        }
    }
    assert!(
        failures.is_empty(),
        "card and reference differ on {failures:?}"
    );
    assert!(field_dct > 0, "no clip has field DCT");
}
