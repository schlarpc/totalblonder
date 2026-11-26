//! The row and picture reductions: `k_rowstat` (row statistics), `k_ent_rowsum` (bit offsets
//! inside a slice, slice sizes), `k_ent_picsum` (slice offsets, picture size) and
//! `k_ent_scatter` (macroblock bits into the stream), on hand-made inputs, against models
//! written from the kernels' comments. `k_rowstat` and `k_ent_rowsum` are the first real
//! kernels on the launcher's fibers: block sizes 32 and 64 (one and two warps), as `row_bs`
//! makes them for 11 and 45 macroblocks per row. Integer kernels: compared exactly.

mod common;

use common::{BS, Bench, Rng, geo, pods_of, row_bs};
use tb_device::kernel::{EntPicSum, EntRowSum, EntScatter, Kernel, RowStat as RowStatK};
use tb_kernel_abi::{
    MB_MAX_OVER, MB_SCRATCH_WORDS, MODE_INTRA, MbInfo, MbRes, NZ_BINS, PicStat, RowStat,
};

const N: usize = 3;
const LIST: [i32; 2] = [2, 0];

fn rowstat_case(mbw: i32, mbh: i32) {
    let g = geo(N as i32, mbw, mbh);
    let nmb = g.nmb as usize;
    let nmbtot = N * nmb;
    let mut rng = Rng::new(mbw as u64);
    // Histograms of six blocks per macroblock: eight 8-bit counters per word, at most 64
    // coefficients per block in all (the kernel's sums cannot carry between counters).
    let hist: Vec<u64> = (0..6 * nmbtot * 2)
        .map(|_| (0..8).fold(0u64, |h, i| h | (u64::from(rng.below(9) as u8) << (8 * i))))
        .collect();
    let act: Vec<u16> = (0..nmbtot).map(|_| rng.next_u32() as u16).collect();
    let info: Vec<MbInfo> = (0..nmbtot)
        .map(|_| MbInfo {
            mode: rng.below(4) as u8,
            field_pred: rng.below(2) as u8,
            cost: rng.next_u32(),
            ..MbInfo::default()
        })
        .collect();
    let out0 = rng.bytes(N * mbh as usize * size_of::<RowStat>());

    let mut b = Bench::new();
    let dlist = b.pods(&LIST);
    let dhist = b.pods(&hist);
    let dact = b.pods(&act);
    let dinfo = b.pods(&info);
    let dout = b.bytes(&out0);
    let k = Kernel::RowStat(RowStatK {
        g,
        list: dlist,
        na: LIST.len() as i32,
        hist: dhist,
        act: dact,
        info: dinfo,
        out: dout,
    });
    b.launch(k, LIST.len() as u32 * mbh as u32, row_bs(mbw));
    let got: Vec<RowStat> = b.run().pods(dout);

    let mut want: Vec<RowStat> = pods_of(&out0);
    for &s in &LIST {
        let s = s as usize;
        for row in 0..mbh as usize {
            let mut r = RowStat::default();
            for x in 0..mbw as usize {
                let mbi = s * nmb + row * mbw as usize + x;
                for blk in 0..6 {
                    for half in 0..2 {
                        let h = hist[(blk * nmbtot + mbi) * 2 + half];
                        for i in 0..8 {
                            r.nz[half * 8 + i] += ((h >> (8 * i)) & 255) as u32;
                        }
                    }
                }
                r.act_sum += u32::from(act[mbi]);
                r.n_intra += u32::from(info[mbi].mode == MODE_INTRA);
                r.n_field += u32::from(info[mbi].field_pred);
            }
            want[s * mbh as usize + row] = r;
        }
    }
    assert_eq!(got, want);
    assert!(want.iter().any(|r| r.nz.iter().sum::<u32>() > 0));
    assert_eq!(NZ_BINS, 16);
}

#[test]
fn rowstat_one_warp() {
    assert_eq!(row_bs(11), 32);
    rowstat_case(11, 3);
}

#[test]
fn rowstat_two_warps() {
    assert_eq!(row_bs(45), 64);
    rowstat_case(45, 2);
}

#[test]
fn rowstat_full_warp_and_eight_warps() {
    rowstat_case(32, 2);
    assert_eq!(row_bs(250), 256);
    rowstat_case(250, 1);
}

/// One macroblock's bits: a random bit string of `nbits` bits, MSB first in 32-bit words, the
/// bits after the end zero (as `bw_finish` leaves them).
fn mb_bits(rng: &mut Rng, nbits: usize) -> Vec<u32> {
    let mut w = vec![0u32; nbits.div_ceil(32)];
    for i in 0..nbits {
        if rng.below(2) == 1 {
            w[i / 32] |= 1 << (31 - i % 32);
        }
    }
    w
}

/// rowsum -> picsum -> scatter in one plan, as `Batch::launch_qent` and `finish_gpu` queue
/// them, every intermediate result compared with its model.
fn entropy_sums_case(mbw: i32, mbh: i32) {
    let g = geo(N as i32, mbw, mbh);
    let (nmb, mbh_u) = (g.nmb as usize, mbh as usize);
    let nmbtot = N * nmb;
    let mut rng = Rng::new(1000 + mbw as u64);
    let mut code = vec![MbRes::default(); nmbtot];
    let mut mbbits = vec![0u32; nmbtot];
    let mut mbtex = vec![0u16; nmbtot];
    let mut scratch = vec![0u32; nmbtot * MB_SCRATCH_WORDS];
    for mbi in 0..nmbtot {
        let skipped = rng.below(5) == 0;
        code[mbi].skipped = u8::from(skipped);
        let nbits = if skipped {
            0
        } else {
            1 + rng.below(700) as usize
        };
        mbbits[mbi] = nbits as u32;
        // bit 15: the macroblock is above the limit of H.262 Table 8-6 (set by k_ent_pack; here
        // at random, often enough that some rows have more than two)
        let over = u16::from(!skipped && rng.below(6) == 0);
        mbtex[mbi] = rng.below(nbits as u32 + 1) as u16 | over << 15;
        let w = mb_bits(&mut rng, nbits);
        scratch[mbi * MB_SCRATCH_WORDS..][..w.len()].copy_from_slice(&w);
    }

    // Models of the sums.
    let mut mboff = vec![0u32; nmbtot];
    let mut slice_bytes = vec![0u32; N * mbh_u];
    let mut slice_tex = vec![0u32; N * mbh_u];
    let mut slice_off = vec![0u32; N * mbh_u];
    let mut pic = vec![PicStat::default(); N];
    let mut n_big = 0;
    for &s in &LIST {
        let s = s as usize;
        let (mut off, mut tex_pic) = (0u32, 0u32);
        let mut big_rows = [0u32; 4];
        for row in 0..mbh_u {
            let (mut acc, mut tex, mut over) = (0u32, 0u32, 0u32);
            for x in 0..mbw as usize {
                let mbi = s * nmb + row * mbw as usize + x;
                mboff[mbi] = acc;
                acc += mbbits[mbi];
                tex += u32::from(mbtex[mbi] & 0x7fff);
                over += u32::from(mbtex[mbi] >> 15);
            }
            let r = s * mbh_u + row;
            slice_bytes[r] = acc.div_ceil(8);
            // bit 31: more than two macroblocks of the row above the limit
            let big = over > MB_MAX_OVER as u32;
            slice_tex[r] = tex | u32::from(big) << 31;
            slice_off[r] = off;
            off += slice_bytes[r];
            tex_pic += tex;
            if big {
                big_rows[row / 32] |= 1 << (row % 32);
                n_big += 1;
            }
        }
        pic[s] = PicStat {
            bytes: off,
            tex_bits: tex_pic,
            big_rows,
        };
        assert_eq!(pic[s].any_big_row(), big_rows != [0; 4]);
        assert!((0..mbh_u).all(|r| pic[s].big_row(r) == (big_rows[r / 32] >> (r % 32) & 1 != 0)));
    }
    // Output: the pictures one after the other, each with its size plus two words reserved.
    let mut outpos = vec![0u32; 2 * N];
    let mut words = 0u32;
    for s in 0..N {
        let reserve = pic[s].bytes.div_ceil(4) + 2;
        outpos[2 * s] = words;
        outpos[2 * s + 1] = reserve;
        words += reserve;
    }
    // The stream of each picture: every coded macroblock's bits at its slice's byte offset plus
    // its offset in the slice, zero bits elsewhere.
    let mut stream = vec![0u8; words as usize * 4];
    for &s in &LIST {
        let s = s as usize;
        let base = outpos[2 * s] as usize * 32;
        for mbi in s * nmb..(s + 1) * nmb {
            if code[mbi].skipped != 0 {
                continue;
            }
            let row = (mbi - s * nmb) / mbw as usize;
            let at = base + slice_off[s * mbh_u + row] as usize * 8 + mboff[mbi] as usize;
            for i in 0..mbbits[mbi] as usize {
                let bit = (scratch[mbi * MB_SCRATCH_WORDS + i / 32] >> (31 - i % 32)) & 1;
                stream[(at + i) / 8] |= (bit as u8) << (7 - (at + i) % 8);
            }
        }
    }

    let mut b = Bench::new();
    let dlist = b.pods(&LIST);
    let na = LIST.len() as i32;
    let dcode = b.pods(&code);
    let dmbbits = b.pods(&mbbits);
    let dmbtex = b.pods(&mbtex);
    let dscratch = b.pods(&scratch);
    let dmboff = b.filled(nmbtot * 4, 0xee);
    let dsb = b.filled(N * mbh_u * 4, 0xee);
    let dst = b.filled(N * mbh_u * 4, 0xee);
    let dso = b.filled(N * mbh_u * 4, 0xee);
    let dpic = b.filled(N * size_of::<PicStat>(), 0xee);
    let dout = b.zeros(words as usize * 4);
    let doutpos = b.pods(&outpos);
    b.launch(
        Kernel::EntRowSum(EntRowSum {
            g,
            list: dlist,
            na,
            mbbits: dmbbits,
            mbtex: dmbtex,
            mboff: dmboff,
            slice_bytes: dsb,
            slice_tex: dst,
        }),
        LIST.len() as u32 * mbh as u32,
        row_bs(mbw),
    );
    b.cover(
        Kernel::EntPicSum(EntPicSum {
            g,
            list: dlist,
            na,
            slice_bytes: dsb,
            slice_tex: dst,
            slice_off: dso,
            pic: dpic,
        }),
        LIST.len(),
        BS,
    );
    b.cover(
        Kernel::EntScatter(EntScatter {
            g,
            list: dlist,
            na,
            code: dcode,
            scratch: dscratch,
            mbbits: dmbbits,
            mboff: dmboff,
            slice_off: dso,
            out: dout,
            outpos: doutpos,
        }),
        LIST.len() * nmb,
        BS,
    );
    let out = b.run();

    // Entries of unlisted sessions keep the fill.
    let keep = |v: &mut Vec<u32>, per: usize| {
        for s in 0..N {
            if !LIST.contains(&(s as i32)) {
                v[s * per..(s + 1) * per].fill(0xeeee_eeee);
            }
        }
    };
    keep(&mut mboff, nmb);
    keep(&mut slice_bytes, mbh_u);
    keep(&mut slice_tex, mbh_u);
    keep(&mut slice_off, mbh_u);
    assert!(n_big > 0 || mbw < 8, "the case has no row above the limit");
    let unlisted = PicStat {
        bytes: 0xeeee_eeee,
        tex_bits: 0xeeee_eeee,
        big_rows: [0xeeee_eeee; 4],
    };
    for (s, p) in pic.iter_mut().enumerate() {
        if !LIST.contains(&(s as i32)) {
            *p = unlisted;
        }
    }
    assert_eq!(out.pods::<u32>(dmboff), mboff, "k_ent_rowsum: mboff");
    assert_eq!(
        out.pods::<u32>(dsb),
        slice_bytes,
        "k_ent_rowsum: slice_bytes"
    );
    assert_eq!(out.pods::<u32>(dst), slice_tex, "k_ent_rowsum: slice_tex");
    assert_eq!(out.pods::<u32>(dso), slice_off, "k_ent_picsum: slice_off");
    assert_eq!(out.pods::<PicStat>(dpic), pic, "k_ent_picsum: pic");
    assert_eq!(out.bytes(dout), &stream[..], "k_ent_scatter");
}

#[test]
fn entropy_sums_one_warp() {
    entropy_sums_case(11, 3);
}

#[test]
fn entropy_sums_two_warps() {
    entropy_sums_case(45, 2);
}

#[test]
fn entropy_sums_eight_warps() {
    entropy_sums_case(250, 1);
}
