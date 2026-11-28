// Shared host/device definitions for the encoder's kernels.
#pragma once
#include <stdint.h>

#ifdef __CUDACC__
#define HD __host__ __device__
#else
#define HD
#endif

#define M2V_TABLE static const
#include "vlc_tables.h"

// Scan patterns, ITU-T H.262 Figures 7-2 and 7-3: value = scan[alternate_scan][v][u],
// the position in coded order of raster coefficient (v,u).
static const uint8_t k_scan_fig[2][64] = {
  { 0,  1,  5,  6, 14, 15, 27, 28,
    2,  4,  7, 13, 16, 26, 29, 42,
    3,  8, 12, 17, 25, 30, 41, 43,
    9, 11, 18, 24, 31, 40, 44, 53,
   10, 19, 23, 32, 39, 45, 52, 54,
   20, 22, 33, 38, 46, 51, 55, 60,
   21, 34, 37, 47, 50, 56, 59, 61,
   35, 36, 48, 49, 57, 58, 62, 63 },
  { 0,  4,  6, 20, 22, 36, 38, 52,
    1,  5,  7, 21, 23, 37, 39, 53,
    2,  8, 19, 24, 34, 40, 50, 54,
    3,  9, 18, 25, 35, 41, 51, 55,
   10, 17, 26, 30, 42, 46, 56, 60,
   11, 16, 27, 31, 43, 47, 57, 61,
   12, 15, 28, 32, 44, 48, 58, 62,
   13, 14, 29, 33, 45, 49, 59, 63 }};

// Default intra quantizer matrix, H.262 6.3.11, raster order [v][u].
static const uint8_t k_intra_q[64] = {
   8, 16, 19, 22, 26, 27, 29, 34,
  16, 16, 22, 24, 27, 29, 34, 37,
  19, 22, 26, 27, 29, 34, 34, 38,
  22, 22, 26, 27, 29, 34, 37, 40,
  22, 26, 27, 29, 32, 35, 40, 48,
  26, 27, 29, 32, 35, 40, 48, 58,
  26, 27, 29, 34, 38, 46, 56, 69,
  27, 29, 35, 38, 46, 56, 69, 83 };

// quantiser_scale for q_scale_type = 1 (H.262 Table 7-6), indexed by quantiser_scale_code.
static const uint8_t k_qscale[32] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24,
                                     28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 104, 112};

// Tables, Geo, SessPar, MbInfo, MbRes, RowStat, PicStat, Bufs and the PIC_*, MODE_*, SL_*, NZ_BINS,
// MB_SCRATCH_WORDS, XB constants are defined once, in crates/tb-kernel-abi/src/lib.rs;
// tb_kernel_abi.h (included by vlc_tables.h above) is generated from it.

// Everything the bit packer needs for one macroblock, with slice-serial state resolved.
struct MbCode {
  int16_t dc_diff[6];
  int16_t pmv[2][2][2];    // prediction of each vector component as the decoder forms it (7.6.3.1)
  uint8_t skipped, cbp, mode, nomc;
  uint8_t first, qcode, row, dct_type;
  uint8_t addr_inc;        // 1..mbw (escape handled by the packer)
  uint8_t quant;           // 1: macroblock_quant, qcode is sent in the macroblock
  uint8_t field_pred, fsel;
};

// Field tools of the picture a session codes: frame_pred_frame_dct = 0, i.e. frame_motion_type
// and dct_type are coded per macroblock and may say "field". A picture whose two fields are one
// instant (progressive_frame = 1 in an interlaced sequence) is coded without them:
// frame_pred_frame_dct shall be 1 when progressive_frame is 1 (A/53 Part 4 6.1.6, SCTE 43 7.1.6).
HD inline int pic_field_tools(const Geo &g, const SessPar &p) { return g.interlaced && !p.frame_only; }
// alternate_scan of the picture (6.3.10): the batch's choice unless the host asks for the zigzag
// scan (a frame-only picture of a session that left the choice to the format).
// Where the stored values of a block are in coef and lev: value k of block blk is at
// lev_at(blk) + LEV_IDX(k) (the layout and its reasons: lev_at in crates/tb-kernel-abi/src/lib.rs,
// which these two repeat).
#define LEV_IDX(k) ((size_t)((k) >> LEV_SH) * (LEV_GROUP << LEV_SH) + ((k) & ((1 << LEV_SH) - 1)))
HD inline size_t lev_at(size_t blk) { return blk / LEV_GROUP * (64 * LEV_GROUP) + ((blk % LEV_GROUP) << LEV_SH); }
HD inline int pic_alt_scan(const Geo &g, const SessPar &p) { return g.alt_scan && !p.zigzag; }

// Lower edges of the histogram bins: a coefficient in bin i becomes zero when the base
// quantiser_scale exceeds a value in [edge[i], edge[i+1]).
static const float k_nz_edge[NZ_BINS + 1] = {1, 1.5f, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 1e9f};
