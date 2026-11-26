//! The tables of H.262 as the host and the kernels use them.
//!
//! `QSCALE`, `INTRA_Q` and the scan patterns are typed from the standard and checked against the
//! golden `tables` record (first recorded by the C++ encoder). The variable-length code tables of
//! Annex B are generated from the text of the standard (work package S1 adds `vlc.rs`, written by
//! `crates/tb-kernel-abi/kernels/gen_tables.py --rust`).

use tb_kernel_abi::Tables;

use crate::vlc;

/// quantiser_scale for q_scale_type = 1 (Table 7-6), indexed by quantiser_scale_code.
pub const QSCALE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64,
    72, 80, 88, 96, 104, 112,
];

/// Default intra quantizer matrix (6.3.11), raster order `[v][u]`.
pub const INTRA_Q: [u8; 64] = [
    8, 16, 19, 22, 26, 27, 29, 34, 16, 16, 22, 24, 27, 29, 34, 37, 19, 22, 26, 27, 29, 34, 34, 38,
    22, 22, 26, 27, 29, 34, 37, 40, 22, 26, 27, 29, 32, 35, 40, 48, 26, 27, 29, 32, 35, 40, 48, 58,
    26, 27, 29, 34, 38, 46, 56, 69, 27, 29, 35, 38, 46, 56, 69, 83,
];

/// Scan patterns, Figures 7-2 and 7-3: `SCAN[alternate_scan][v * 8 + u]` is the position in
/// coded order of raster coefficient (v, u).
pub const SCAN: [[u8; 64]; 2] = [
    [
        0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9,
        11, 18, 24, 31, 40, 44, 53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60,
        21, 34, 37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
    ],
    [
        0, 4, 6, 20, 22, 36, 38, 52, 1, 5, 7, 21, 23, 37, 39, 53, 2, 8, 19, 24, 34, 40, 50, 54, 3,
        9, 18, 25, 35, 41, 51, 55, 10, 17, 26, 30, 42, 46, 56, 60, 11, 16, 27, 31, 43, 47, 57, 61,
        12, 15, 28, 32, 44, 48, 58, 62, 13, 14, 29, 33, 45, 49, 59, 63,
    ],
];

/// The complete table record, as the C++ `Encoder::Encoder` builds it: the VLC tables,
/// the scan patterns and their inverses, the intra matrix, the quantizer scale and the map from
/// a quantiser_scale value to the nearest code.
///
/// Acceptance: the bytes equal the `tables` record of `crates/tb-golden/vectors/`.
pub fn tables() -> Tables {
    let mut scan_ras = [[0u8; 64]; 2];
    for (ras, pos) in scan_ras.iter_mut().zip(SCAN.iter()) {
        for (r, &p) in pos.iter().enumerate() {
            ras[usize::from(p)] = r as u8;
        }
    }
    let mut qcode_near = [0u8; 128];
    for (q, near) in qcode_near.iter_mut().enumerate() {
        let dist = |c: usize| (i32::from(QSCALE[c]) - q as i32).abs();
        let mut best = 1;
        for c in 1..32 {
            if dist(c) < dist(best) {
                best = c;
            }
        }
        *near = best as u8;
    }
    Tables {
        mbai: vlc::MBAI,
        cbp: vlc::CBP,
        motion: vlc::MOTION,
        dc_lum: vlc::DC_LUM,
        dc_chr: vlc::DC_CHR,
        dct0: vlc::DCT0,
        dct1: vlc::DCT1,
        scan_pos: SCAN,
        scan_ras,
        intra_q: INTRA_Q,
        qscale: QSCALE,
        qcode_near,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constants typed here equal the golden `tables` record.
    #[test]
    fn constants_match_the_record() {
        let recs = tb_golden::case("tables").unwrap();
        let t = recs[1].pods::<Tables>("bytes").unwrap()[0];
        assert_eq!(t.qscale, QSCALE);
        assert_eq!(t.intra_q, INTRA_Q);
        assert_eq!(t.scan_pos, SCAN);
    }

    /// Acceptance 1: the whole record, byte for byte.
    #[test]
    fn tables_equal_the_record() {
        let recs = tb_golden::case("tables").unwrap();
        let rec = recs.iter().find(|r| r.kind() == "tables").unwrap();
        let want = rec.bytes("bytes").unwrap();
        assert_eq!(rec.int("size").unwrap(), want.len() as i64);
        let t = tables();
        assert_eq!(bytemuck::bytes_of(&t), want.as_slice());
    }

    /// Each scan is a permutation of 0..64.
    #[test]
    fn scans_are_permutations() {
        for s in SCAN {
            let mut seen = [false; 64];
            for p in s {
                seen[usize::from(p)] = true;
            }
            assert!(seen.iter().all(|&x| x));
        }
    }
}
