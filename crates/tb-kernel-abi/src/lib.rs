//! The kernel ABI: every record and constant that the host and the CUDA kernels both read.
//!
//! This file is the single definition. `kernels/tb_kernel_abi.h` is generated from it by cbindgen
//! (`nix run .#abi-header`, checked for freshness by `nix flake check`) and is what the kernel
//! sources compile against. Layout is checked on both sides: the tests of this crate pin every size
//! and offset, and compare them with a manifest that a C++ program prints from the generated header
//! (`TB_ABI_LAYOUT`).
//!
//! Rules for this crate:
//! - one file (cbindgen reads it without running Cargo), plain `#[repr(C)]` data only;
//! - records that cross the bus as bytes are [`bytemuck::Pod`]: explicit padding, no pointers;
//! - records that hold device addresses ([`Bufs`], and of the conversion kernels [`Taps`],
//!   [`ConvClass`], [`ConvJob`], [`RectJob`], [`PreJob`]) are built by adapters only, never by the
//!   sans-io core, which names buffers by handle (`tb_device::BufRef`).
//!
//! Section numbers refer to ITU-T H.262 (02/2000).
#![forbid(unsafe_code)]

use bytemuck::{Pod, Zeroable};

/// picture_coding_type (6.3.9): intra.
pub const PIC_I: u8 = 1;
/// picture_coding_type: predictive.
pub const PIC_P: u8 = 2;
/// picture_coding_type: bidirectionally predictive.
pub const PIC_B: u8 = 3;

/// Macroblock mode: intra.
pub const MODE_INTRA: u8 = 0;
/// Macroblock mode: forward prediction (bit 0).
pub const MODE_FWD: u8 = 1;
/// Macroblock mode: backward prediction (bit 1).
pub const MODE_BWD: u8 = 2;
/// Macroblock mode: both directions.
pub const MODE_BI: u8 = 3;

/// Slot selector of a kernel argument: slot 0 (a buffer without a ring).
pub const SL_ZERO: i32 = 0;
/// Slot selector: the session's source slot ([`SessPar::cur`]).
pub const SL_CUR: i32 = 1;
/// Slot selector: the session's reconstruction slot ([`SessPar::rec`]).
pub const SL_REC: i32 = 2;

/// Bins of the row histogram ([`RowStat::nz`]).
pub const NZ_BINS: usize = 16;
/// 32-bit words of scratch per macroblock for the bit packer
/// (worst case: 6 blocks x 64 escapes x 24 bits + headers < 9728 bits).
pub const MB_SCRATCH_WORDS: usize = 304;
/// Most bits of a 4:2:0 macroblock, from the first bit of macroblock_address_increment (H.262
/// 8.2, Table 8-6, p.110).
pub const MB_MAX_BITS: usize = 4608;
/// Macroblocks of one row that may exceed [`MB_MAX_BITS`] (same clause).
pub const MB_MAX_OVER: usize = 2;
/// Bits of a slice header as the kernels write it: slice_start_code, quantiser_scale_code,
/// extra_bit_slice (6.2.4). They precede the first macroblock of a row and are not part of it.
pub const SLICE_HEADER_BITS: usize = 38;
/// Words of the row mask of [`PicStat::big_rows`]: 128 rows, more than any level has.
pub const PIC_ROW_WORDS: usize = 4;
/// Largest search range at quarter resolution ([`Geo::range2`]). The first level of the motion
/// search loads whole words up to `4 * ((range2 + 3) / 4)` bytes before a row of the
/// quarter-resolution reference and `4 * ((range2 + 3) / 4) + 7` bytes past it; for the first
/// and the last plane of the buffer that is in the guard bytes the host leaves around every
/// device buffer, which this bound keeps the loads inside (the host refuses a larger range,
/// the kernel clamps it). It is also the largest range whose vectors the vertical f_code of a
/// frame picture can code (H.262 Table 8-8: f_code 5).
pub const ME_MAX_RANGE2: i32 = 30;
/// Runs covered by the DCT coefficient tables (B.14, B.15) before the escape.
pub const VLC_DCT_MAXRUN: usize = 32;
/// Levels covered by the DCT coefficient tables before the escape.
pub const VLC_DCT_MAXLEV: usize = 41;
/// Entries of one DCT coefficient table: `VLC_DCT_MAXRUN * VLC_DCT_MAXLEV` (a literal, because
/// cbindgen does not evaluate products in array lengths).
pub const VLC_DCT_CODES: usize = 1312;
const _: () = assert!(VLC_DCT_CODES == VLC_DCT_MAXRUN * VLC_DCT_MAXLEV);
/// 8x8 blocks per CUDA block in the transform kernel (`k_xf` runs `XB * 8` threads per block).
pub const XB: usize = 32;

/// One variable-length code: `len` bits, right-aligned in `code`. `len == 0`: no code.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct VlcCode {
    /// The code bits.
    pub code: u16,
    /// Number of bits.
    pub len: u8,
    /// Always zero.
    pub pad: u8,
}

/// Every table the kernels read, as one record uploaded once per batch.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct Tables {
    /// macroblock_address_increment, Table B.1, by increment (1..=33).
    pub mbai: [VlcCode; 34],
    /// coded_block_pattern, Table B.9, by pattern.
    pub cbp: [VlcCode; 64],
    /// motion_code, Table B.10, by code + 16.
    pub motion: [VlcCode; 33],
    /// dct_dc_size_luminance, Table B.12, by size.
    pub dc_lum: [VlcCode; 12],
    /// dct_dc_size_chrominance, Table B.13, by size.
    pub dc_chr: [VlcCode; 12],
    /// DCT coefficients table zero (B.14), by `run * VLC_DCT_MAXLEV + level`.
    pub dct0: [VlcCode; VLC_DCT_CODES],
    /// DCT coefficients table one (B.15), same index.
    pub dct1: [VlcCode; VLC_DCT_CODES],
    /// Raster index -> coded position, per alternate_scan (Figures 7-2, 7-3).
    pub scan_pos: [[u8; 64]; 2],
    /// Coded position -> raster index.
    pub scan_ras: [[u8; 64]; 2],
    /// Default intra quantizer matrix (6.3.11), raster order.
    pub intra_q: [u8; 64],
    /// quantiser_scale_code -> quantiser_scale for q_scale_type = 1 (Table 7-6).
    pub qscale: [u8; 32],
    /// quantiser_scale value (1..=112, clamped) -> nearest quantiser_scale_code.
    pub qcode_near: [u8; 128],
}

/// Geometry and coding parameters of one batch (all sessions of one format). Passed to the
/// kernels by value.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Geo {
    /// Session slots in the batch.
    pub n: i32,
    /// Coded (padded) luma width, a multiple of 16.
    pub w: i32,
    /// Coded (padded) luma height, a multiple of 16 (32 when interlaced).
    pub h: i32,
    /// Macroblocks per row.
    pub mbw: i32,
    /// Macroblock rows.
    pub mbh: i32,
    /// Macroblocks per picture.
    pub nmb: i32,
    /// One value for all four f_codes.
    pub f_code: i32,
    /// intra_vlc_format (6.3.10).
    pub intra_vlc_format: i32,
    /// alternate_scan (6.3.10).
    pub alt_scan: i32,
    /// 1: frame_pred_frame_dct = 0; dct_type and frame_motion_type are coded per macroblock.
    pub interlaced: i32,
    /// 1: the motion search also tries field-based prediction (interlaced only).
    pub field_pred: i32,
    /// Search range at quarter resolution.
    pub range2: i32,
    /// Motion vector cost weight in the quarter-resolution search.
    pub lambda: i32,
    /// Bits charged to a field-based candidate (its extra vector and select bits).
    pub field_bits: i32,
    /// Strength of the activity modulation: factor range 1/aq_s .. aq_s (Test Model 5: 2).
    pub aq_s: f32,
    /// 1: second pass that lets a macroblock adopt its left neighbor's vectors.
    pub smooth: i32,
    /// 1: the search also tries the vector of the co-located macroblock of the previous coded
    /// picture, scaled by the temporal distances.
    pub tpred: i32,
}

/// Per-session parameters of the picture being coded (uploaded every tick).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct SessPar {
    /// `PIC_*`.
    pub pic_type: u8,
    /// Ring slot of the source frame.
    pub cur: u8,
    /// Forward reference slot (0/1): P and B pictures.
    pub fwd: u8,
    /// Backward reference slot (0/1): B pictures.
    pub bwd: u8,
    /// Reference slot that receives the reconstruction.
    pub rec: u8,
    /// Coefficients kept per block in scan order (64 = all; 0 = none, intra blocks keep DC).
    pub keep: u8,
    /// 1: activity-based quantizer modulation per macroblock.
    pub aq: u8,
    /// 1: store the prediction (needed when the picture is reconstructed).
    pub want_pred: u8,
    /// Average macroblock activity used to normalize (previous picture).
    pub avg_act: u16,
    /// Frames from the forward reference to this picture (display order).
    pub dist_f: i8,
    /// Frames from the backward reference to this picture (display order; negative).
    pub dist_b: i8,
    /// top_field_first.
    pub tff: u8,
    /// No row of this picture will be quantized with a quantiser_scale_code below this one:
    /// `k_xf` need not store coefficients that are zero at that quantizer.
    pub qfloor: u8,
    /// SAD units per bit for motion decisions: about quantiser_scale / 2 of the previous picture.
    pub lam: u16,
    /// `dist_f` of the session's previous coded picture, whose decisions are still in the
    /// `MbInfo` buffer when the search runs (0: none).
    pub pdist_f: i8,
    /// `dist_b` of the session's previous coded picture.
    pub pdist_b: i8,
    /// 1: the picture is coded without field tools: frame prediction and frame DCT only, and
    /// frame_motion_type and dct_type are not in the stream (frame_pred_frame_dct = 1). Set
    /// for a picture flagged progressive_frame in an interlaced session (A/53 Part 4 6.1.6).
    pub frame_only: u8,
    /// 1: the picture is scanned in the zigzag order whatever [`Geo::alt_scan`] says
    /// (alternate_scan = 0 in its picture coding extension).
    pub zigzag: u8,
}

/// Motion/mode decision for one macroblock (output of the motion search).
///
/// `mv[r][s][t]`: r = 0 frame vector or top-field vector, r = 1 bottom-field vector;
/// s = 0 forward, 1 backward; t = 0 horizontal, 1 vertical. Half-sample units; the vertical
/// component of a field vector is in half field lines (7.6.3.1).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct MbInfo {
    /// Motion vectors.
    pub mv: [[[i16; 2]; 2]; 2],
    /// `MODE_*`.
    pub mode: u8,
    /// 1 = field DCT (luma lines regrouped by field), frame pictures only.
    pub dct_type: u8,
    /// 1 = frame_motion_type "Field-based" (two vectors per direction).
    pub field_pred: u8,
    /// Bit (2*s + r): motion_vertical_field_select\[r\]\[s\] (0 top, 1 bottom reference field).
    pub fsel: u8,
    /// SAD of the chosen mode (for statistics).
    pub cost: u32,
}

/// What `k_ent_resolve` leaves per macroblock for `k_ent_pack` and `k_ent_scatter`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct MbRes {
    /// DC differentials of the six blocks (intra).
    pub dc_diff: [i16; 6],
    /// 1: the macroblock is skipped.
    pub skipped: u8,
    /// coded_block_pattern.
    pub cbp: u8,
    /// `MODE_*`.
    pub mode: u8,
    /// 1: "No MC, Coded" (P pictures).
    pub nomc: u8,
    /// 1: first macroblock of its slice.
    pub first: u8,
    /// Macroblock row.
    pub row: u8,
    /// dct_type.
    pub dct_type: u8,
    /// frame_motion_type is field-based.
    pub field_pred: u8,
    /// Field selects, as [`MbInfo::fsel`].
    pub fsel: u8,
    /// Always zero.
    pub pad: [u8; 3],
}

/// Row statistics for rate control (one per session and macroblock row).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct RowStat {
    /// Histogram: coefficients whose level is non-zero only while quantiser_scale is below the
    /// bin's upper edge (edges: 1, 1.5, 2, 3, 4, 6, ... half octaves; `tb_m2v_rate::NZ_EDGE`).
    pub nz: [u32; NZ_BINS],
    /// Sum of macroblock activities.
    pub act_sum: u32,
    /// Intra macroblocks.
    pub n_intra: u32,
    /// Macroblocks with field-based prediction.
    pub n_field: u32,
    /// Always zero.
    pub pad: u32,
}

/// Size of one coded picture as the entropy kernels measured it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct PicStat {
    /// Bytes of all slices.
    pub bytes: u32,
    /// Bits of run/level codes: the part that scales with the number of non-zero levels.
    pub tex_bits: u32,
    /// Bit `r` (bit `r % 32` of word `r / 32`): macroblock row `r` has more than
    /// [`MB_MAX_OVER`] macroblocks above [`MB_MAX_BITS`] bits, which H.262 Table 8-6 forbids.
    /// The picture must not be written as it is.
    pub big_rows: [u32; PIC_ROW_WORDS],
}

impl PicStat {
    /// Some row breaks the macroblock size rule of H.262 Table 8-6.
    pub fn any_big_row(&self) -> bool {
        self.big_rows.iter().any(|w| *w != 0)
    }

    /// Row `r` breaks it.
    pub fn big_row(&self, r: usize) -> bool {
        let b = r.min(PIC_ROW_WORDS * 32 - 1);
        self.big_rows[b / 32] >> (b % 32) & 1 != 0
    }
}

/// Base addresses of the picture stores of one batch, as the kernels take them (by value).
///
/// Holds device addresses: only an adapter builds one, from the handles of
/// `tb_device::kernel::BufsRef`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Bufs {
    /// Source frames `[RING][n]{Y, U, V}`: luma plane of frame (0, 0).
    pub ring_y: *mut u8,
    /// Cb plane of frame (0, 0).
    pub ring_u: *mut u8,
    /// Cr plane of frame (0, 0).
    pub ring_v: *mut u8,
    /// Bytes from one (slot, session) frame of the ring to the next.
    pub ring_stride: usize,
    /// Half-resolution luma of the picture being searched `[n]`.
    pub cur1: *mut u8,
    /// Quarter-resolution luma of the picture being searched `[n]`.
    pub cur2: *mut u8,
    /// Reconstructed anchors `[2][n]`, luma.
    pub ref_y: *mut u8,
    /// Reconstructed anchors, Cb.
    pub ref_u: *mut u8,
    /// Reconstructed anchors, Cr.
    pub ref_v: *mut u8,
    /// Reconstructed anchors, half-resolution luma.
    pub ref_y1: *mut u8,
    /// Reconstructed anchors, quarter-resolution luma.
    pub ref_y2: *mut u8,
}

/// One tap table of the resampler on the device (`pipe_kernels.h`): `n` outputs with `nt` taps
/// each (Q14, sum 16384). Holds device addresses: built by adapters only.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Taps {
    /// First source sample of each output (`n` entries).
    pub start: *const i32,
    /// Coefficients, `nt` per output.
    pub coef: *const i16,
    /// Outputs.
    pub n: i32,
    /// Taps per output.
    pub nt: i32,
}

/// A conversion class as the conversion kernels take it (by value): geometry of the source
/// lattice, the destination rows and the four tap tables. Holds device addresses.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ConvClass {
    /// Bytes per source sample: 1 (NV12), 2 (P010 / P016, value in the high bits).
    pub bps: i32,
    /// Luma width of the destination rectangle, a multiple of 8.
    pub dw: i32,
    /// Rows of the intermediate luma plane: the source lattice rows.
    pub th_y: i32,
    /// Rows of the intermediate chroma planes.
    pub th_c: i32,
    /// Source rows used: `s_off + s_step * i` (luma and chroma alike).
    pub s_step: i32,
    /// Source lattice offset.
    pub s_off: i32,
    /// Destination rows written inside the rectangle: `d_off + d_step * k`.
    pub d_step: i32,
    /// Destination lattice offset.
    pub d_off: i32,
    /// Bytes of intermediate per job.
    pub tstride: i32,
    /// Copy path (same size, no filter): luma width.
    pub copy_w: i32,
    /// Copy path: luma height.
    pub copy_h: i32,
    /// Luma, horizontal.
    pub hy: Taps,
    /// Chroma, horizontal.
    pub hc: Taps,
    /// Luma, vertical.
    pub vy: Taps,
    /// Chroma, vertical.
    pub vc: Taps,
}

/// One conversion job: a source surface and the destination planes at the rectangle's origin.
/// An array of these is uploaded per class and tick. Holds device addresses: the host writes
/// the handle form (`tb_device::kernel::ConvJobRef`, the same layout) and the adapter resolves
/// it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ConvJob {
    /// Source luma plane (frame origin).
    pub sy: *const u8,
    /// Source interleaved chroma plane (frame origin).
    pub suv: *const u8,
    /// Destination luma at the rectangle's origin.
    pub dy: *mut u8,
    /// Destination Cb.
    pub du: *mut u8,
    /// Destination Cr.
    pub dv: *mut u8,
    /// Source luma pitch in bytes.
    pub sp_y: i32,
    /// Source chroma pitch in bytes.
    pub sp_uv: i32,
    /// Destination luma pitch (the chroma pitch is half).
    pub dp: i32,
    /// Always zero.
    pub pad: i32,
}

/// A rectangle of a destination picture for `k_fill`, `k_overlay` and `k_csc` (by value).
/// Holds device addresses.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RectJob {
    /// Destination luma at the rectangle's origin.
    pub dy: *mut u8,
    /// Destination Cb.
    pub du: *mut u8,
    /// Destination Cr.
    pub dv: *mut u8,
    /// Destination luma pitch.
    pub dp: i32,
    /// Width, a multiple of 8.
    pub w: i32,
    /// Height, even.
    pub h: i32,
    /// Fill color: `Y | U << 8 | V << 16`.
    pub yuv: u32,
    /// Overlay luma, pitch `w`.
    pub oy: *const u8,
    /// Overlay Cb, pitch `w / 2`.
    pub ou: *const u8,
    /// Overlay Cr, pitch `w / 2`.
    pub ov: *const u8,
    /// Alpha per luma sample, pitch `w`.
    pub oa: *const u8,
    /// Alpha per chroma sample, pitch `w / 2`.
    pub oa2: *const u8,
}

/// Color matrix and range for `k_csc` (by value): Q12 matrix on (Y, Cb - 128, Cr - 128),
/// offsets in Q12.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct CscPar {
    /// The matrix, row by row.
    pub m: [i32; 9],
    /// Offsets per output component.
    pub off: [i32; 3],
    /// Non-zero: the frame is interlaced (the luma correction uses the chroma of the same
    /// field).
    pub interlaced: i32,
}

/// One job of the 2:1 pre-stage (`k_half`): a source frame to halve. Uploaded as an array;
/// holds device addresses (handle form: `tb_device::kernel::PreJobRef`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PreJob {
    /// Source luma plane.
    pub sy: *const u8,
    /// Source interleaved chroma plane.
    pub suv: *const u8,
    /// Luma pitch.
    pub sp_y: i32,
    /// Chroma pitch.
    pub sp_uv: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    /// Every (record, field, offset, size) of the ABI, and (record, "", size, alignment).
    /// The same lines are printed by `kernels/abi_layout.cc` from the generated header.
    fn layout() -> Vec<String> {
        let mut v = Vec::new();
        macro_rules! rec {
            ($t:ident { $($f:ident),* $(,)? }) => {{
                v.push(format!("{} = {} {}", stringify!($t), size_of::<$t>(), align_of::<$t>()));
                // Size of a field without a value of the record: through a pointer projection.
                fn field_size<T, F>(_: fn(&T) -> &F) -> usize { size_of::<F>() }
                $( v.push(format!("{}.{} {} {}", stringify!($t), stringify!($f),
                    offset_of!($t, $f), field_size(|r: &$t| &r.$f))); )*
            }};
        }
        rec!(VlcCode { code, len, pad });
        rec!(Tables {
            mbai,
            cbp,
            motion,
            dc_lum,
            dc_chr,
            dct0,
            dct1,
            scan_pos,
            scan_ras,
            intra_q,
            qscale,
            qcode_near
        });
        rec!(Geo {
            n,
            w,
            h,
            mbw,
            mbh,
            nmb,
            f_code,
            intra_vlc_format,
            alt_scan,
            interlaced,
            field_pred,
            range2,
            lambda,
            field_bits,
            aq_s,
            smooth,
            tpred
        });
        rec!(SessPar {
            pic_type,
            cur,
            fwd,
            bwd,
            rec,
            keep,
            aq,
            want_pred,
            avg_act,
            dist_f,
            dist_b,
            tff,
            qfloor,
            lam,
            pdist_f,
            pdist_b,
            frame_only,
            zigzag
        });
        rec!(MbInfo {
            mv,
            mode,
            dct_type,
            field_pred,
            fsel,
            cost
        });
        rec!(MbRes {
            dc_diff,
            skipped,
            cbp,
            mode,
            nomc,
            first,
            row,
            dct_type,
            field_pred,
            fsel,
            pad
        });
        rec!(RowStat {
            nz,
            act_sum,
            n_intra,
            n_field,
            pad
        });
        rec!(PicStat {
            bytes,
            tex_bits,
            big_rows
        });
        rec!(Bufs {
            ring_y,
            ring_u,
            ring_v,
            ring_stride,
            cur1,
            cur2,
            ref_y,
            ref_u,
            ref_v,
            ref_y1,
            ref_y2
        });
        rec!(Taps { start, coef, n, nt });
        rec!(ConvClass {
            bps,
            dw,
            th_y,
            th_c,
            s_step,
            s_off,
            d_step,
            d_off,
            tstride,
            copy_w,
            copy_h,
            hy,
            hc,
            vy,
            vc
        });
        rec!(ConvJob {
            sy,
            suv,
            dy,
            du,
            dv,
            sp_y,
            sp_uv,
            dp,
            pad
        });
        rec!(RectJob {
            dy,
            du,
            dv,
            dp,
            w,
            h,
            yuv,
            oy,
            ou,
            ov,
            oa,
            oa2
        });
        rec!(CscPar { m, off, interlaced });
        rec!(PreJob {
            sy,
            suv,
            sp_y,
            sp_uv
        });
        v
    }

    /// The sizes the C++ encoder of the spike was validated with (at commit 6b5cb0c).
    #[test]
    fn sizes_are_pinned() {
        assert_eq!(size_of::<VlcCode>(), 4);
        assert_eq!(size_of::<Tables>(), 11_596);
        assert_eq!(size_of::<Geo>(), 68);
        assert_eq!(size_of::<SessPar>(), 20);
        assert_eq!(size_of::<MbInfo>(), 24);
        assert_eq!(size_of::<MbRes>(), 24);
        assert_eq!(size_of::<RowStat>(), 80);
        assert_eq!(size_of::<PicStat>(), 24);
        assert_eq!(size_of::<Bufs>(), 88);
        // The conversion records, as the C++ spike had them before K2.
        assert_eq!(size_of::<Taps>(), 24);
        assert_eq!(size_of::<ConvClass>(), 144);
        assert_eq!(size_of::<ConvJob>(), 56);
        assert_eq!(size_of::<RectJob>(), 80);
        assert_eq!(size_of::<CscPar>(), 52);
        assert_eq!(size_of::<PreJob>(), 24);
    }

    /// The C++ view of the generated header (`TB_ABI_LAYOUT`, set by the flake) agrees with
    /// Rust on every offset and size. Skipped when the variable is not set (bare `cargo test`
    /// outside the dev shell).
    #[test]
    fn layout_matches_the_c_header() {
        let Some(path) = option_env!("TB_ABI_LAYOUT") else {
            eprintln!("TB_ABI_LAYOUT not set at build time: layout comparison skipped");
            return;
        };
        let c = std::fs::read_to_string(path).unwrap();
        let c: Vec<&str> = c.lines().filter(|l| !l.is_empty()).collect();
        let rust = layout();
        assert_eq!(rust.iter().map(String::as_str).collect::<Vec<_>>(), c);
    }
}
