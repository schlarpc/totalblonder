//! `k_pad_cols`, `k_pad_rows` (edge replication) and `k_set_mode` (constant records), against
//! models written from their comments in kernels.h. Integer kernels: compared exactly.

mod common;

use common::{BS, Bench, Rng, pods_of};
use tb_device::kernel::{Kernel, PadCols, PadRows, SetMode};
use tb_kernel_abi::{MODE_FWD, MODE_INTRA, MbInfo};

const N: usize = 3;
const SLOTS: usize = 2;

/// Pads the frames of the listed sessions of a ring of `SLOTS x N` frames, `stride` bytes apart,
/// from `sw x sh` to `pw x ph`, as the upload does.
fn pad_case(pw: usize, ph: usize, sw: usize, sh: usize, list: &[i32], cols: bool, rows: bool) {
    let stride = pw * ph + 40; // the ring stride also holds the chroma planes
    let mut rng = Rng::new((pw * 1000 + sw) as u64);
    let ring = rng.bytes(SLOTS * N * stride);
    let slot: [u8; N] = [1, 0, 1];

    let mut b = Bench::new();
    let plane = b.bytes(&ring);
    let dlist = b.pods(list);
    let dslot = b.bytes(&slot);
    let na = list.len();
    if cols {
        let k = Kernel::PadCols(PadCols {
            plane,
            stride: stride as u64,
            pw: pw as i32,
            sw: sw as i32,
            sh: sh as i32,
            n: N as i32,
            list: dlist,
            slot: dslot,
            na: na as i32,
        });
        b.cover(k, na * (pw - sw) * sh, BS);
    }
    if rows {
        let k = Kernel::PadRows(PadRows {
            plane,
            stride: stride as u64,
            pw: pw as i32,
            sh: sh as i32,
            ph: ph as i32,
            n: N as i32,
            list: dlist,
            slot: dslot,
            na: na as i32,
        });
        b.cover(k, na * pw * (ph - sh), BS);
    }
    let out = b.run();

    let mut want = ring.clone();
    for &s in list {
        let s = s as usize;
        let f = (usize::from(slot[s]) * N + s) * stride;
        let p = &mut want[f..f + pw * ph];
        if cols {
            for y in 0..sh {
                for x in sw..pw {
                    p[y * pw + x] = p[y * pw + sw - 1];
                }
            }
        }
        if rows {
            for y in sh..ph {
                for x in 0..pw {
                    p[y * pw + x] = p[(sh - 1) * pw + x];
                }
            }
        }
    }
    assert_eq!(out.bytes(plane), &want[..]);
    assert_eq!(out.bytes(dslot), &slot[..]);
}

#[test]
fn pad_cols_repeats_the_last_column() {
    pad_case(32, 24, 21, 17, &[2, 0], true, false);
    pad_case(48, 16, 47, 16, &[1], true, false);
}

#[test]
fn pad_rows_repeats_the_last_row() {
    pad_case(32, 24, 32, 17, &[2, 0], false, true);
    pad_case(16, 48, 16, 1, &[0, 1, 2], false, true);
}

#[test]
fn pad_cols_then_rows_fill_the_corner() {
    pad_case(32, 32, 21, 17, &[2, 0], true, true);
    pad_case(64, 32, 50, 30, &[1, 2], true, true);
}

fn set_mode_case(mode: u8) {
    let nmb = 37;
    let mut rng = Rng::new(u64::from(mode) + 7);
    let init = rng.bytes(N * nmb * size_of::<MbInfo>());
    let list = [2i32, 0];
    let mut b = Bench::new();
    let info = b.bytes(&init);
    let dlist = b.pods(&list);
    let k = Kernel::SetMode(SetMode {
        info,
        nmb: nmb as i32,
        list: dlist,
        na: list.len() as i32,
        mode: i32::from(mode),
    });
    b.cover(k, list.len() * nmb, BS);
    let out: Vec<MbInfo> = b.run().pods(info);

    let mut want: Vec<MbInfo> = pods_of(&init);
    for &s in &list {
        for m in &mut want[s as usize * nmb..(s as usize + 1) * nmb] {
            *m = MbInfo {
                mode,
                ..MbInfo::default()
            };
        }
    }
    assert_eq!(out, want);
}

#[test]
fn set_mode_writes_constant_records() {
    set_mode_case(MODE_INTRA);
    set_mode_case(MODE_FWD);
}

/// `k_tick` does nothing (it marks ticks in a timeline): it runs, and the bench finds no byte
/// changed outside the buffers.
#[test]
fn tick_does_nothing() {
    let mut b = Bench::new();
    let x = b.filled(64, 0x3c);
    b.launch(Kernel::Tick(tb_device::kernel::Tick {}), 1, 1);
    b.launch(Kernel::Tick(tb_device::kernel::Tick {}), 3, 256);
    assert!(b.run().bytes(x).iter().all(|&v| v == 0x3c));
}
