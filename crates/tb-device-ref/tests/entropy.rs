//! The entropy coder on hand-made macroblock rows: `k_ent_resolve` (skipped macroblocks, DC
//! prediction, "No MC"), `k_ent_pack` (address increments, quantizer changes, vector
//! prediction, the bits of each macroblock), then `k_ent_rowsum`, `k_ent_picsum` and
//! `k_ent_scatter`, all in one plan. The expected bits are written by hand from ITU-T H.262
//! (slice header 6.2.4, macroblock 6.2.5, Tables B.1 to B.4, B.9, B.10, B.12 to B.14 and the
//! escape of B.16, DC prediction 7.2.1, vector prediction 7.6.3, skipped macroblocks 7.6.6),
//! not from the kernels' code. Integer kernels: compared exactly.

mod common;

use common::{BS, Bench, geo, row_bs, tables};
use tb_device::kernel::{EntPack, EntPicSum, EntResolve, EntRowSum, EntScatter, Kernel};
use tb_kernel_abi::{
    Geo, MB_SCRATCH_WORDS, MODE_BI, MODE_BWD, MODE_FWD, MODE_INTRA, MbInfo, MbRes, PIC_B, PIC_I,
    PIC_P, PicStat, SessPar, lev_at, lev_idx, lev_len,
};

/// Session slots of the batch; only `S` is listed, slot 0 must stay untouched.
const N: usize = 2;
const S: usize = 1;

/// One hand-made macroblock.
#[derive(Clone, Default)]
struct Mb {
    mode: u8,
    /// Frame vector, or the top-field vector when `field_pred` (forward, backward).
    mv: [[i16; 2]; 2],
    /// Bottom-field vector (field prediction only).
    mv1: [[i16; 2]; 2],
    field_pred: u8,
    fsel: u8,
    dct_type: u8,
    mbq: u8,
    /// Intra: the DC level of each block.
    dc: [i16; 6],
    /// Non-zero levels: (block, coded position, level).
    levels: Vec<(usize, usize, i16)>,
}

/// What the kernels must produce for one macroblock.
struct Want {
    res: MbRes,
    /// Header bits and texture bits (run/level codes, and End of Block in non-intra blocks),
    /// in stream order: (bits, is texture). Empty for a skipped macroblock.
    bits: Vec<(&'static str, bool)>,
}

fn hdr(b: &'static str) -> (&'static str, bool) {
    (b, false)
}
fn tex(b: &'static str) -> (&'static str, bool) {
    (b, true)
}

/// The record `k_ent_resolve` leaves for a macroblock that is not skipped, before DC
/// differentials and "No MC".
fn res(mb: &Mb, x: usize, cbp: u8) -> MbRes {
    MbRes {
        mode: mb.mode,
        first: u8::from(x == 0),
        row: 0,
        cbp,
        dct_type: mb.dct_type,
        field_pred: mb.field_pred,
        fsel: mb.fsel,
        skipped: 0,
        nomc: 0,
        dc_diff: [0; 6],
        pad: [0; 3],
    }
}

/// Runs the five entropy kernels over one slice of `mbs` and compares everything with `want`.
fn run_slice(mut g: Geo, pic_type: u8, mbs: &[Mb], want: &[Want]) {
    let mbw = mbs.len();
    g.mbw = mbw as i32;
    g.mbh = 1;
    g.nmb = mbw as i32;
    g.w = 16 * mbw as i32;
    g.h = 16;
    let nmb = mbw;
    let stride = N * nmb;
    let t = tables();

    let mut info = vec![MbInfo::default(); stride];
    let mut nz = vec![0u8; 6 * stride];
    let mut lev = vec![0i16; lev_len(6 * stride)];
    let mut nzmask = vec![0u64; 6 * stride];
    let mut mbq = vec![0u8; stride];
    // Session 0 holds data too, which the kernels must not read: garbage records.
    for (i, m) in info.iter_mut().enumerate().take(nmb) {
        m.mode = MODE_INTRA;
        m.cost = 0xdead_0000 + i as u32;
        mbq[i] = 31;
    }
    for (x, mb) in mbs.iter().enumerate() {
        let mbi = S * nmb + x;
        let mut m = MbInfo {
            mode: mb.mode,
            dct_type: mb.dct_type,
            field_pred: mb.field_pred,
            fsel: mb.fsel,
            cost: 1234,
            ..MbInfo::default()
        };
        for s in 0..2 {
            m.mv[0][s] = mb.mv[s];
            m.mv[1][s] = mb.mv1[s];
        }
        info[mbi] = m;
        mbq[mbi] = mb.mbq;
        // The levels of a block one after the other in coded order (`lev_at`), an intra
        // block's DC level first.
        let mut of_block: [Vec<(usize, i16)>; 6] = Default::default();
        if mb.mode == MODE_INTRA {
            for (b, dc) in mb.dc.iter().enumerate() {
                of_block[b].push((0, *dc));
                nzmask[b * stride + mbi] |= 1;
            }
        }
        for &(b, p, l) in &mb.levels {
            of_block[b].push((p, l));
            nzmask[b * stride + mbi] |= 1 << p;
            nz[b * stride + mbi] = 1;
        }
        for (b, levels) in of_block.iter_mut().enumerate() {
            levels.sort_unstable();
            for (k, &(_, l)) in levels.iter().enumerate() {
                lev[lev_at(b * stride + mbi) + lev_idx(k)] = l;
            }
        }
    }
    let par = [
        SessPar {
            pic_type: PIC_I,
            ..SessPar::default()
        },
        SessPar {
            pic_type,
            ..SessPar::default()
        },
    ];

    // The expected stream: the macroblocks' bits one after the other, zero-stuffed to a byte.
    let mut stream = String::new();
    let mut mbbits = vec![0u32; stride];
    let mut mbtex = vec![0u16; stride];
    for (x, w) in want.iter().enumerate() {
        let mut n = 0;
        for (b, is_tex) in &w.bits {
            assert!(b.chars().all(|c| c == '0' || c == '1'), "{b}");
            stream.push_str(b);
            n += b.len();
            if *is_tex {
                mbtex[S * nmb + x] += b.len() as u16;
            }
        }
        mbbits[S * nmb + x] = n as u32;
    }
    while !stream.len().is_multiple_of(8) {
        stream.push('0');
    }
    let bytes: Vec<u8> = stream
        .as_bytes()
        .chunks(8)
        .map(|c| c.iter().fold(0u8, |a, &d| (a << 1) | (d - b'0')))
        .collect();
    let words = bytes.len().div_ceil(4) + 2;
    let outpos = [0u32, 0, 0, words as u32];

    let mut b = Bench::new();
    let list = b.pods(&[S as i32]);
    let dt = b.pods(std::slice::from_ref(&t));
    let dpar = b.pods(&par);
    let dinfo = b.pods(&info);
    let dnz = b.pods(&nz);
    let dlev = b.pods(&lev);
    let dnzmask = b.pods(&nzmask);
    let dmbq = b.pods(&mbq);
    let dcode = b.filled(stride * size_of::<MbRes>(), 0xcc);
    let dscratch = b.zeros(stride * MB_SCRATCH_WORDS * 4);
    let dmbbits = b.filled(stride * 4, 0xcc);
    let dmbtex = b.filled(stride * 2, 0xcc);
    let dmboff = b.zeros(stride * 4);
    let dsb = b.zeros(N * 4);
    let dst = b.zeros(N * 4);
    let dso = b.zeros(N * 4);
    let dpic = b.zeros(N * size_of::<PicStat>());
    let dout = b.zeros(words * 4);
    let doutpos = b.pods(&outpos);
    let na = 1;
    b.cover(
        Kernel::EntResolve(EntResolve {
            g,
            par: dpar,
            list,
            na,
            info: dinfo,
            nz: dnz,
            lev: dlev,
            code: dcode,
        }),
        nmb,
        BS,
    );
    b.cover(
        Kernel::EntPack(EntPack {
            g,
            tables: dt,
            par: dpar,
            list,
            na,
            code: dcode,
            info: dinfo,
            lev: dlev,
            nzmask: dnzmask,
            mbq: dmbq,
            scratch: dscratch,
            mbbits: dmbbits,
            mbtex: dmbtex,
        }),
        nmb,
        BS,
    );
    b.launch(
        Kernel::EntRowSum(EntRowSum {
            g,
            list,
            na,
            mbbits: dmbbits,
            mbtex: dmbtex,
            mboff: dmboff,
            slice_bytes: dsb,
            slice_tex: dst,
        }),
        1,
        row_bs(g.mbw),
    );
    b.cover(
        Kernel::EntPicSum(EntPicSum {
            g,
            list,
            na,
            slice_bytes: dsb,
            slice_tex: dst,
            slice_off: dso,
            pic: dpic,
        }),
        1,
        BS,
    );
    b.cover(
        Kernel::EntScatter(EntScatter {
            g,
            list,
            na,
            code: dcode,
            scratch: dscratch,
            mbbits: dmbbits,
            mboff: dmboff,
            slice_off: dso,
            out: dout,
            outpos: doutpos,
        }),
        nmb,
        BS,
    );
    let out = b.run();

    let code: Vec<MbRes> = out.pods(dcode);
    let fill = MbRes {
        dc_diff: [0xcccc_u16 as i16; 6],
        skipped: 0xcc,
        cbp: 0xcc,
        mode: 0xcc,
        nomc: 0xcc,
        first: 0xcc,
        row: 0xcc,
        dct_type: 0xcc,
        field_pred: 0xcc,
        fsel: 0xcc,
        pad: [0xcc; 3],
    };
    for x in 0..nmb {
        assert_eq!(code[x], fill, "k_ent_resolve wrote an unlisted session");
        assert_eq!(
            code[S * nmb + x],
            want[x].res,
            "k_ent_resolve: macroblock {x}"
        );
    }
    let got_bits: Vec<u32> = out.pods(dmbbits);
    let got_tex: Vec<u16> = out.pods(dmbtex);
    let scratch: Vec<u32> = out.pods(dscratch);
    for (x, w) in want.iter().enumerate() {
        let i = S * nmb + x;
        // The bits of this macroblock, as the scratch holds them.
        let n = got_bits[i] as usize;
        let mine: String = (0..n)
            .map(|k| {
                if (scratch[i * MB_SCRATCH_WORDS + k / 32] >> (31 - k % 32)) & 1 == 1 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        let expect: String = w.bits.iter().map(|(b, _)| *b).collect();
        assert_eq!(mine, expect, "k_ent_pack: bits of macroblock {x}");
        assert_eq!(
            got_bits[i], mbbits[i],
            "k_ent_pack: bit count of macroblock {x}"
        );
        assert_eq!(
            got_tex[i], mbtex[i],
            "k_ent_pack: texture bits of macroblock {x}"
        );
    }
    let pic: Vec<PicStat> = out.pods(dpic);
    assert_eq!(pic[S].bytes as usize, bytes.len());
    assert_eq!(
        pic[S].tex_bits,
        mbtex.iter().map(|&t| u32::from(t)).sum::<u32>()
    );
    let o = out.bytes(dout);
    assert_eq!(&o[..bytes.len()], &bytes[..], "the slice in the output");
    assert!(o[bytes.len()..].iter().all(|&x| x == 0));
}

/// slice_start_code of slice 1 (6.2.4: 0x00000101), quantiser_scale_code `q` (5 bits),
/// extra_bit_slice 0.
const SLICE1: &str = "00000000000000000000000100000001";

fn g0() -> Geo {
    let mut g = geo(N as i32, 1, 1);
    g.f_code = 1;
    g
}

/// An I picture: DC prediction along the row (7.2.1), a quantizer change, an escape.
#[test]
fn intra_row() {
    let mbs = [
        Mb {
            mode: MODE_INTRA,
            mbq: 5,
            dc: [130, 130, 131, 127, 128, 120],
            levels: vec![(0, 1, 1), (5, 1, 100), (5, 3, -1)],
            ..Mb::default()
        },
        Mb {
            mode: MODE_INTRA,
            mbq: 7,
            dc: [127, 127, 127, 127, 128, 121],
            levels: vec![(4, 2, 1)],
            ..Mb::default()
        },
        Mb {
            mode: MODE_INTRA,
            mbq: 7,
            dc: [127, 127, 127, 127, 128, 121],
            ..Mb::default()
        },
    ];
    let mut r0 = res(&mbs[0], 0, 63);
    r0.dc_diff = [2, 0, 1, -4, 0, -8];
    let mut r1 = res(&mbs[1], 1, 63);
    r1.dc_diff = [0, 0, 0, 0, 0, 1];
    let r2 = res(&mbs[2], 2, 63);
    let want = [
        Want {
            res: r0,
            bits: vec![
                hdr(SLICE1),
                hdr("00101"), // quantiser_scale_code 5
                hdr("0"),     // extra_bit_slice
                hdr("1"),     // macroblock_address_increment 1 (B.1)
                hdr("1"),     // macroblock_type Intra (B.2)
                // Y0: dct_dc_size_luminance 2 (B.12) '01', differential +2 '10'
                hdr("01"),
                hdr("10"),
                tex("110"), // run 0 level +1, not first: '11' s (B.14)
                hdr("10"),  // End of Block
                // Y1: size 0 '100'
                hdr("100"),
                hdr("10"),
                // Y2: size 1 '00', +1 '1'
                hdr("00"),
                hdr("1"),
                hdr("10"),
                // Y3: -4: size 3 '101', differential -4 + 7 = 3 '011'
                hdr("101"),
                hdr("011"),
                hdr("10"),
                // Cb: dct_dc_size_chrominance 0 (B.13) '00'
                hdr("00"),
                hdr("10"),
                // Cr: -8: size 4 '1110', -8 + 15 = 7 '0111'
                hdr("1110"),
                hdr("0111"),
                // escape '000001', run 0 (6 bits), level +100 (12 bits) (B.16)
                tex("000001"),
                tex("000000"),
                tex("000001100100"),
                tex("0111"), // run 1 level -1: '011' s
                hdr("10"),
            ],
        },
        Want {
            res: r1,
            bits: vec![
                hdr("1"),
                hdr("01"),    // Intra, quant (B.2)
                hdr("00111"), // quantiser_scale_code 7
                // Y0..Y3: predicted from Y3 of the left macroblock (127): differential 0
                hdr("100"),
                hdr("10"),
                hdr("100"),
                hdr("10"),
                hdr("100"),
                hdr("10"),
                hdr("100"),
                hdr("10"),
                hdr("00"),
                tex("0110"), // run 1 level +1
                hdr("10"),
                hdr("01"), // Cr: +1 from 120: size 1 '01', '1'
                hdr("1"),
                hdr("10"),
            ],
        },
        Want {
            res: r2,
            bits: vec![
                hdr("1"),
                hdr("1"), // Intra; the quantizer is the left macroblock's
                hdr("100"),
                hdr("10"),
                hdr("100"),
                hdr("10"),
                hdr("100"),
                hdr("10"),
                hdr("100"),
                hdr("10"),
                hdr("00"),
                hdr("10"),
                hdr("00"),
                hdr("10"),
            ],
        },
    ];
    run_slice(g0(), PIC_I, &mbs, &want);
}

/// A P picture: a skipped macroblock (7.6.6.2) and the address increment over it, vector
/// prediction reset by the skip (7.6.3.4), "No MC" with a quantizer change, a coded block
/// pattern.
#[test]
fn predicted_row() {
    let fwd = |mv: [i16; 2], mbq: u8, levels: Vec<(usize, usize, i16)>| Mb {
        mode: MODE_FWD,
        mv: [mv, [0, 0]],
        mbq,
        levels,
        ..Mb::default()
    };
    let mbs = [
        fwd([2, -1], 5, vec![]),
        fwd([0, 0], 5, vec![]),
        fwd([2, -1], 5, vec![(0, 0, 1), (0, 3, -2)]),
        fwd([0, 0], 9, vec![(5, 0, -1)]),
        fwd([2, -1], 9, vec![]),
    ];
    let mut r1 = res(&mbs[1], 1, 0);
    r1.skipped = 1;
    let mut r3 = res(&mbs[3], 3, 1);
    r3.nomc = 1;
    let want = [
        Want {
            res: res(&mbs[0], 0, 0),
            bits: vec![
                hdr(SLICE1),
                hdr("00101"),
                hdr("0"),
                hdr("1"),
                hdr("001"),  // MC, Not Coded (B.3)
                hdr("0010"), // motion_code +2 (B.10)
                hdr("011"),  // motion_code -1
            ],
        },
        Want {
            res: r1,
            bits: vec![],
        },
        Want {
            res: res(&mbs[2], 2, 32),
            bits: vec![
                hdr("011"),      // macroblock_address_increment 2
                hdr("1"),        // MC, Coded
                hdr("0010"),     // the predictor was reset by the skipped macroblock: +2
                hdr("011"),      // -1
                hdr("1010"),     // coded_block_pattern 32 (B.9)
                tex("10"),       // first coefficient of a non-intra block, +1: '1' s
                tex("00001001"), // run 2 level -2: '0000 100' s
                tex("10"),       // End of Block
            ],
        },
        Want {
            res: r3,
            bits: vec![
                hdr("1"),
                hdr("00001"), // No MC, Coded, quant (B.3)
                hdr("01001"), // quantiser_scale_code 9
                hdr("01011"), // coded_block_pattern 1
                tex("11"),    // first coefficient -1
                tex("10"),
            ],
        },
        Want {
            res: res(&mbs[4], 4, 0),
            bits: vec![
                hdr("1"),
                hdr("001"),
                hdr("0010"), // the predictor after "No MC" is zero
                hdr("011"),
            ],
        },
    ];
    run_slice(g0(), PIC_P, &mbs, &want);
}

/// A B picture: a skipped macroblock that repeats its left neighbor (7.6.6.4), vector
/// predictors kept across it, per direction.
#[test]
fn bidirectional_row() {
    let mb = |mode: u8, f: [i16; 2], b: [i16; 2]| Mb {
        mode,
        mv: [f, b],
        mbq: 5,
        ..Mb::default()
    };
    let mbs = [
        mb(MODE_BI, [4, 2], [-2, 0]),
        mb(MODE_BI, [4, 2], [-2, 0]),
        mb(MODE_FWD, [5, 2], [0, 0]),
        mb(MODE_BWD, [0, 0], [-2, 0]),
    ];
    let mut r1 = res(&mbs[1], 1, 0);
    r1.skipped = 1;
    let want = [
        Want {
            res: res(&mbs[0], 0, 0),
            bits: vec![
                hdr(SLICE1),
                hdr("00101"),
                hdr("0"),
                hdr("1"),
                hdr("10"),      // Interp, Not Coded (B.4)
                hdr("0000110"), // forward +4: '0000 11' s
                hdr("0010"),    // +2
                hdr("0011"),    // backward -2
                hdr("1"),       // 0
            ],
        },
        Want {
            res: r1,
            bits: vec![],
        },
        Want {
            res: res(&mbs[2], 2, 0),
            bits: vec![
                hdr("011"),  // increment 2
                hdr("0010"), // Fwd, Not Coded
                hdr("010"),  // 5 - 4 = +1
                hdr("1"),    // 2 - 2 = 0
            ],
        },
        Want {
            res: res(&mbs[3], 3, 0),
            bits: vec![
                hdr("1"),
                hdr("010"), // Bwd, Not Coded
                hdr("1"),   // predicted from the first macroblock's backward vector
                hdr("1"),
            ],
        },
    ];
    run_slice(g0(), PIC_B, &mbs, &want);
}

/// An interlaced P picture: field-based prediction with field selects and field DCT, then a
/// frame macroblock whose predictor is the field vector with its vertical component doubled
/// (7.6.3.1).
#[test]
fn field_prediction_row() {
    let mut g = g0();
    g.interlaced = 1;
    g.field_pred = 1;
    let mbs = [
        Mb {
            mode: MODE_FWD,
            field_pred: 1,
            mv: [[2, 1], [0, 0]],
            mv1: [[3, -1], [0, 0]],
            fsel: 1, // top field from the bottom reference field, bottom field from the top
            dct_type: 1,
            mbq: 5,
            levels: vec![(0, 0, 1)],
            ..Mb::default()
        },
        Mb {
            mode: MODE_FWD,
            mv: [[2, 2], [0, 0]],
            mbq: 5,
            ..Mb::default()
        },
    ];
    let want = [
        Want {
            res: res(&mbs[0], 0, 32),
            bits: vec![
                hdr(SLICE1),
                hdr("00101"),
                hdr("0"),
                hdr("1"),
                hdr("1"),  // MC, Coded
                hdr("01"), // frame_motion_type: field-based (Table 6-17)
                hdr("1"),  // dct_type: field
                hdr("1"),  // motion_vertical_field_select[0][0]
                hdr("0010"),
                hdr("010"),   // +1
                hdr("0"),     // motion_vertical_field_select[1][0]
                hdr("00010"), // +3: '0001' s
                hdr("011"),
                hdr("1010"),
                tex("10"),
                tex("10"),
            ],
        },
        Want {
            res: res(&mbs[1], 1, 0),
            bits: vec![
                hdr("1"),
                hdr("001"), // MC, Not Coded
                hdr("10"),  // frame_motion_type: frame-based
                hdr("1"),   // 2 - 2
                hdr("1"),   // 2 - 2 * 1
            ],
        },
    ];
    run_slice(g, PIC_P, &mbs, &want);
}
