// MPEG-2 slice/macroblock/block bit packing, written once and compiled for both the host
// and the device (ITU-T H.262 6.2.4-6.2.6, 7.2, 7.6.3, 7.6.6, Annex B).
//
// The work is split so that it parallelizes on a GPU:
//   (k_ent_resolve in kernels.h: one thread per macroblock resolves the state that the slice
//                   syntax carries from macroblock to macroblock: skipped macroblocks, DC
//                   predictors, motion vector predictors.)
//   encode_mb       one thread per macroblock: writes the macroblock's bits into its own
//                   fixed-size scratch area, returns the bit count.
//   (prefix sum over bit counts, then) append_bits: one thread per macroblock, ORs the
//                   scratch bits into the output at the macroblock's bit offset.
#pragma once
#include "common.h"

struct BitW {
  uint32_t *buf;
  uint64_t acc;
  int nacc;
  int pos;
};

HD inline void bw_init(BitW &w, uint32_t *buf) { w.buf = buf; w.acc = 0; w.nacc = 0; w.pos = 0; }
HD inline void bw_put(BitW &w, uint32_t code, int len) {
  w.acc = (w.acc << len) | code;
  w.nacc += len;
  if (w.nacc >= 32) {
    w.nacc -= 32;
    w.buf[w.pos++] = (uint32_t)(w.acc >> w.nacc);
  }
}
HD inline int bw_finish(BitW &w) {
  int bits = w.pos * 32 + w.nacc;
  if (w.nacc > 0) w.buf[w.pos] = (uint32_t)(w.acc << (32 - w.nacc));
  return bits;
}

HD inline void put_vlc(BitW &w, const VlcCode &c) { bw_put(w, c.code, c.len); }

// 6.2.5.2.1 / 7.6.3.1: code one motion vector component.
HD inline void put_mv_comp(BitW &w, const Tables *T, int mv, int pmv, int f_code) {
  int r_size = f_code - 1;
  int f = 1 << r_size;
  int low = -16 * f, high = 16 * f - 1, range = 32 * f;
  int delta = mv - pmv;
  if (delta < low) delta += range;
  if (delta > high) delta -= range;
  if (delta == 0) { put_vlc(w, T->motion[16]); return; }
  int a = delta < 0 ? -delta : delta;
  int code = ((a - 1) >> r_size) + 1;
  int res = (a - 1) & (f - 1);
  put_vlc(w, T->motion[16 + (delta < 0 ? -code : code)]);
  if (r_size) bw_put(w, res, r_size);
}

HD inline int ctz64(uint64_t m) {
#ifdef __CUDA_ARCH__
  return __ffsll((long long)m) - 1;
#else
  return __builtin_ctzll(m);
#endif
}

// 7.2.2: run/level coding of the non-zero coefficients of a block (coded order), then End of
// Block. Bit i of mask says that position i holds a non-zero level; lev holds those levels one
// after the other (LEV_IDX). An intra block starts after its DC coefficient (bit 0,
// always set, and the first level are passed over).
HD inline void put_block_coefs(BitW &w, const Tables *T, const int16_t *lev, uint64_t mask, bool intra,
                               int table1) {
  const VlcCode *tab = table1 ? T->dct1 : T->dct0;
  int prev = -1, k = 0;
  if (intra) { mask &= ~1ull; prev = 0; k = 1; }
  while (mask) {
    int i = ctz64(mask);
    mask &= mask - 1;
    int run = i - prev - 1;
    prev = i;
    int l = lev[LEV_IDX(k)];
    k++;
    int a = l < 0 ? -l : l;
    int s = l < 0;
    if (!intra && i == 0 && a == 1) {
      bw_put(w, 2 | s, 2);  // '1 s': first coefficient of a non-intra block (7.2.2.2)
    } else if (a < VLC_DCT_MAXLEV && run < VLC_DCT_MAXRUN && tab[run * VLC_DCT_MAXLEV + a].len) {
      VlcCode c = tab[run * VLC_DCT_MAXLEV + a];
      bw_put(w, ((uint32_t)c.code << 1) | s, c.len + 1);
    } else {
      // Table B.16: escape (0000 01), 6-bit run, 12-bit signed level.
      bw_put(w, (1u << 18) | ((uint32_t)run << 12) | ((uint32_t)l & 0xfff), 24);
    }
  }
  if (table1) bw_put(w, 6, 4); else bw_put(w, 2, 2);
}

// Pack one macroblock (physical index mbi). The non-zero levels of its block b are those of
// block b * stride + mbi in lev (lev_at), in coded order; the mask of their positions is
// nzmask[b * stride].
// Returns the number of bits written to scratch (0 for a skipped macroblock); *tex_bits gets
// the run/level bits the rate model scales with the non-zero levels: without End of Block for
// intra blocks, with it for the others (measured: neither uniform rule does better, see log).
// field_tools: frame_pred_frame_dct of the picture is 0 (pic_field_tools).
HD inline int encode_mb(const Tables *T, const Geo &g, int pic_type, int field_tools, const MbCode &c, const MbInfo &mi,
                        const int16_t *lev, size_t mbi, const uint64_t *nzmask, size_t stride, uint32_t *scratch,
                        int *tex_bits) {
  *tex_bits = 0;
  if (c.skipped) return 0;
  BitW w;
  bw_init(w, scratch);
  if (c.first) {
    // slice(): slice_start_code 0x000001 + row (1-based), quantiser_scale_code, extra_bit_slice
    bw_put(w, 0x00000100u | (uint32_t)(c.row + 1), 32);
    bw_put(w, (uint32_t)c.qcode << 1, 6);
  }
  int inc = c.addr_inc;
  while (inc > 33) { bw_put(w, 0x08, 11); inc -= 33; }  // macroblock_escape
  put_vlc(w, T->mbai[inc]);

  bool intra = c.mode == MODE_INTRA;
  bool pattern = !intra && c.cbp != 0;
  bool quant = c.quant && !c.first;  // the first macroblock's quantizer is the slice's
  // macroblock_type, Tables B.2-B.4
  if (pic_type == PIC_I) {
    if (quant) bw_put(w, 1, 2); else bw_put(w, 1, 1);
  } else if (pic_type == PIC_P) {
    if (intra) { if (quant) bw_put(w, 1, 6); else bw_put(w, 3, 5); }
    else if (c.nomc) { if (quant) bw_put(w, 1, 5); else bw_put(w, 1, 2); }   // No MC, Coded
    else if (pattern) { if (quant) bw_put(w, 2, 5); else bw_put(w, 1, 1); }  // MC, Coded
    else bw_put(w, 1, 3);                                                    // MC, Not Coded
  } else {
    if (intra) { if (quant) bw_put(w, 1, 6); else bw_put(w, 3, 5); }
    else if (pattern && quant) {
      if (c.mode == MODE_BI) bw_put(w, 2, 5); else if (c.mode == MODE_BWD) bw_put(w, 2, 6); else bw_put(w, 3, 6);
    }
    else if (c.mode == MODE_BI) bw_put(w, 2 | pattern, 2);
    else if (c.mode == MODE_BWD) bw_put(w, 2 | pattern, 3);
    else bw_put(w, 2 | pattern, 4);
  }
  // macroblock_modes(), 6.2.5.1: with frame_pred_frame_dct = 0 a macroblock that carries motion
  // vectors has frame_motion_type (Table 6-17: '10' frame-based, '01' field-based), and an
  // intra or pattern macroblock has dct_type.
  if (field_tools) {
    bool motion = !intra && (pic_type == PIC_B || !c.nomc);
    if (motion) bw_put(w, c.field_pred ? 1 : 2, 2);
    if (intra || pattern) bw_put(w, c.dct_type, 1);
  }
  if (quant) bw_put(w, c.qcode, 5);
  if (!intra) {
    // motion_vectors(s), 6.2.5.2: field-based prediction in a frame picture sends two vectors,
    // each preceded by motion_vertical_field_select.
    for (int s = 0; s < 2; s++) {
      if (!(c.mode & (s ? MODE_BWD : MODE_FWD)) || (s == 0 && c.nomc)) continue;
      int nv = c.field_pred ? 2 : 1;
      for (int r = 0; r < nv; r++) {
        if (c.field_pred) bw_put(w, (c.fsel >> (2 * s + r)) & 1, 1);
        put_mv_comp(w, T, mi.mv[r][s][0], c.pmv[r][s][0], g.f_code);
        put_mv_comp(w, T, mi.mv[r][s][1], c.pmv[r][s][1], g.f_code);
      }
    }
    if (pattern) put_vlc(w, T->cbp[c.cbp]);
  }
  int tex = 0;
  for (int b = 0; b < 6; b++) {
    const int16_t *l = lev + lev_at((size_t)b * stride + mbi);
    if (intra) {
      // 7.2.1: dct_dc_size + dct_dc_differential
      int d = c.dc_diff[b];
      int a = d < 0 ? -d : d;
      int size = 0;
      while (a) { size++; a >>= 1; }
      put_vlc(w, b < 4 ? T->dc_lum[size] : T->dc_chr[size]);
      if (size) bw_put(w, (uint32_t)(d >= 0 ? d : d + (1 << size) - 1), size);
      int before = w.pos * 32 + w.nacc;
      put_block_coefs(w, T, l, nzmask[(size_t)b * stride], true, g.intra_vlc_format);
      tex += w.pos * 32 + w.nacc - before - (g.intra_vlc_format ? 4 : 2);   // intra: no End of Block
    } else if (c.cbp & (32 >> b)) {
      int before = w.pos * 32 + w.nacc;
      put_block_coefs(w, T, l, nzmask[(size_t)b * stride], false, 0);
      tex += w.pos * 32 + w.nacc - before;
    }
  }
  *tex_bits = tex;
  return bw_finish(w);
}

HD inline uint32_t bswap32(uint32_t v) {
  return (v >> 24) | ((v >> 8) & 0xff00) | ((v << 8) & 0xff0000) | (v << 24);
}
