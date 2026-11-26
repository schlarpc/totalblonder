//! `k_mbstat`: the activity (1 + the smallest variance of the frame, and when interlaced also
//! the field, 8x8 luma blocks) and the frame/field DCT decision, against a model written from
//! the kernel's comment. Integer kernel: compared exactly.

mod common;

use common::{BS, Bench, Rng, geo, pods_of};
use tb_device::kernel::{Kernel, MbStat};
use tb_kernel_abi::{MbInfo, SessPar};

const SLOTS: usize = 2;

/// A picture whose macroblocks are, in turn: noise, flat, two interleaved fields of different
/// brightness (field DCT wins), smooth vertical gradient (frame DCT wins), noise of small
/// amplitude.
fn picture(w: usize, h: usize, rng: &mut Rng) -> Vec<u8> {
    let mut p = vec![0u8; w * h];
    let mbw = w / 16;
    for mby in 0..h / 16 {
        for mbx in 0..mbw {
            let kind = (mby * mbw + mbx) % 5;
            for j in 0..16 {
                for i in 0..16 {
                    let v = match kind {
                        0 => rng.byte(),
                        1 => 77,
                        2 => (if j & 1 == 0 { 40 } else { 200 }) + (rng.below(9) as u8),
                        3 => (j * 12 + i) as u8,
                        _ => 120 + (rng.below(16) as u8),
                    };
                    p[(mby * 16 + j) * w + mbx * 16 + i] = v;
                }
            }
        }
    }
    p
}

fn sad(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| u32::from(x.abs_diff(*y)))
        .sum()
}

/// The kernel's definition for the macroblock whose top-left pixel is at `p[0]`, rows `w`
/// apart: (activity, dct_type).
fn model(p: &[u8], w: usize, interlaced: bool) -> (u16, u8) {
    let row = |j: usize| &p[j * w..j * w + 16];
    let (mut dframe, mut dfield) = (0u32, 0u32);
    let (mut sum, mut sq) = ([0u32; 8], [0u32; 8]);
    for j in 0..16 {
        if j >= 1 {
            dframe += sad(row(j), row(j - 1));
        }
        if j >= 2 {
            dfield += sad(row(j), row(j - 2));
        }
        for hb in 0..2 {
            let half = &row(j)[8 * hb..8 * hb + 8];
            let sm: u32 = half.iter().map(|&x| u32::from(x)).sum();
            let qq: u32 = half.iter().map(|&x| u32::from(x) * u32::from(x)).sum();
            let fb = (j >> 3) * 2 + hb; // frame block: top/bottom half, left/right half
            let ib = 4 + (j & 1) * 2 + hb; // field block: line parity, left/right half
            for k in [fb, ib] {
                sum[k] += sm;
                sq[k] += qq;
            }
        }
    }
    let blocks = if interlaced { 8 } else { 4 };
    let best = (0..blocks)
        .map(|k| (sq[k] - ((sum[k] * sum[k]) >> 6)) >> 6)
        .min()
        .unwrap_or(u32::MAX);
    let act = if best > 65534 { 65535 } else { best + 1 };
    let dct = interlaced && dfield * 15 + 14 * 15 * 64 < dframe * 14;
    (act as u16, u8::from(dct))
}

fn case(interlaced: bool) {
    let n = 3usize;
    let mut g = geo(n as i32, 5, 4);
    g.interlaced = i32::from(interlaced);
    let (w, h, nmb) = (g.w as usize, g.h as usize, g.nmb as usize);
    let ysz = w * h;
    let ring_stride = ysz * 3 / 2;
    let mut rng = Rng::new(u64::from(interlaced) + 11);
    let mut ring = rng.bytes(SLOTS * n * ring_stride);
    let cur = [1u8, 0, 1];
    for s in 0..n {
        for slot in 0..SLOTS {
            let f = (slot * n + s) * ring_stride;
            ring[f..f + ysz].copy_from_slice(&picture(w, h, &mut rng));
        }
    }
    let par: Vec<SessPar> = cur
        .iter()
        .map(|&c| SessPar {
            cur: c,
            ..SessPar::default()
        })
        .collect();
    let list = [2i32, 0];
    let info0 = rng.bytes(n * nmb * size_of::<MbInfo>());
    let act0 = rng.bytes(n * nmb * 2);

    let mut b = Bench::new();
    let ring_y = b.bytes(&ring);
    let dpar = b.pods(&par);
    let dlist = b.pods(&list);
    let info = b.bytes(&info0);
    let act = b.bytes(&act0);
    let k = Kernel::MbStat(MbStat {
        g,
        ring_y,
        ring_stride: ring_stride as u64,
        par: dpar,
        list: dlist,
        na: list.len() as i32,
        info,
        act,
    });
    b.cover(k, list.len() * nmb, BS);
    let out = b.run();

    let mut want_info: Vec<MbInfo> = pods_of(&info0);
    let mut want_act: Vec<u16> = pods_of(&act0);
    let mut field_dct = 0;
    for &s in &list {
        let s = s as usize;
        let f = (usize::from(cur[s]) * n + s) * ring_stride;
        for mb in 0..nmb {
            let (mbx, mby) = (mb % g.mbw as usize, mb / g.mbw as usize);
            let at = f + mby * 16 * w + mbx * 16;
            let (a, d) = model(&ring[at..], w, interlaced);
            want_act[s * nmb + mb] = a;
            want_info[s * nmb + mb].dct_type = d;
            field_dct += usize::from(d);
        }
    }
    assert_eq!(out.pods::<u16>(act), want_act);
    assert_eq!(out.pods::<MbInfo>(info), want_info);
    if interlaced {
        // The picture has macroblocks of both kinds.
        assert!(field_dct > 0 && field_dct < list.len() * nmb, "{field_dct}");
    }
}

#[test]
fn progressive_activity() {
    case(false);
}

#[test]
fn interlaced_activity_and_field_dct() {
    case(true);
}
