// Prints the layout of every ABI record as C++ sees tb_kernel_abi.h: "Record = size align",
// then "Record.field offset size" per field, in the order of the header. The tests of
// crates/tb-kernel-abi compare the output with what Rust computes for the same records.
#include <stdio.h>

#include "tb_kernel_abi.h"

#define REC(T) printf(#T " = %zu %zu\n", sizeof(T), alignof(T))
#define FLD(T, f) printf(#T "." #f " %zu %zu\n", offsetof(T, f), sizeof(((T *)0)->f))

int main() {
  REC(VlcCode); FLD(VlcCode, code); FLD(VlcCode, len); FLD(VlcCode, pad);
  REC(Tables); FLD(Tables, mbai); FLD(Tables, cbp); FLD(Tables, motion); FLD(Tables, dc_lum); FLD(Tables, dc_chr);
  FLD(Tables, dct0); FLD(Tables, dct1); FLD(Tables, scan_pos); FLD(Tables, scan_ras); FLD(Tables, intra_q);
  FLD(Tables, qscale); FLD(Tables, qcode_near);
  REC(Geo); FLD(Geo, n); FLD(Geo, w); FLD(Geo, h); FLD(Geo, mbw); FLD(Geo, mbh); FLD(Geo, nmb); FLD(Geo, f_code);
  FLD(Geo, intra_vlc_format); FLD(Geo, alt_scan); FLD(Geo, interlaced); FLD(Geo, field_pred); FLD(Geo, range2);
  FLD(Geo, lambda); FLD(Geo, field_bits); FLD(Geo, aq_s); FLD(Geo, smooth); FLD(Geo, tpred);
  REC(SessPar); FLD(SessPar, pic_type); FLD(SessPar, cur); FLD(SessPar, fwd); FLD(SessPar, bwd); FLD(SessPar, rec);
  FLD(SessPar, keep); FLD(SessPar, aq); FLD(SessPar, want_pred); FLD(SessPar, avg_act); FLD(SessPar, dist_f);
  FLD(SessPar, dist_b); FLD(SessPar, tff); FLD(SessPar, qfloor); FLD(SessPar, lam); FLD(SessPar, pdist_f);
  FLD(SessPar, pdist_b); FLD(SessPar, frame_only); FLD(SessPar, zigzag);
  REC(MbInfo); FLD(MbInfo, mv); FLD(MbInfo, mode); FLD(MbInfo, dct_type); FLD(MbInfo, field_pred); FLD(MbInfo, fsel);
  FLD(MbInfo, cost);
  REC(MbRes); FLD(MbRes, dc_diff); FLD(MbRes, skipped); FLD(MbRes, cbp); FLD(MbRes, mode); FLD(MbRes, nomc);
  FLD(MbRes, first); FLD(MbRes, row); FLD(MbRes, dct_type); FLD(MbRes, field_pred); FLD(MbRes, fsel); FLD(MbRes, pad);
  REC(RowStat); FLD(RowStat, nz); FLD(RowStat, act_sum); FLD(RowStat, n_intra); FLD(RowStat, n_field); FLD(RowStat, pad);
  REC(PicStat); FLD(PicStat, bytes); FLD(PicStat, tex_bits); FLD(PicStat, big_rows);
  REC(Bufs); FLD(Bufs, ring_y); FLD(Bufs, ring_u); FLD(Bufs, ring_v); FLD(Bufs, ring_stride); FLD(Bufs, cur1);
  FLD(Bufs, cur2); FLD(Bufs, ref_y); FLD(Bufs, ref_u); FLD(Bufs, ref_v); FLD(Bufs, ref_y1); FLD(Bufs, ref_y2);
  REC(Taps); FLD(Taps, start); FLD(Taps, coef); FLD(Taps, n); FLD(Taps, nt);
  REC(ConvClass); FLD(ConvClass, bps); FLD(ConvClass, dw); FLD(ConvClass, th_y); FLD(ConvClass, th_c);
  FLD(ConvClass, s_step); FLD(ConvClass, s_off); FLD(ConvClass, d_step); FLD(ConvClass, d_off);
  FLD(ConvClass, tstride); FLD(ConvClass, copy_w); FLD(ConvClass, copy_h); FLD(ConvClass, hy); FLD(ConvClass, hc);
  FLD(ConvClass, vy); FLD(ConvClass, vc);
  REC(ConvJob); FLD(ConvJob, sy); FLD(ConvJob, suv); FLD(ConvJob, dy); FLD(ConvJob, du); FLD(ConvJob, dv);
  FLD(ConvJob, sp_y); FLD(ConvJob, sp_uv); FLD(ConvJob, dp); FLD(ConvJob, pad);
  REC(RectJob); FLD(RectJob, dy); FLD(RectJob, du); FLD(RectJob, dv); FLD(RectJob, dp); FLD(RectJob, w);
  FLD(RectJob, h); FLD(RectJob, yuv); FLD(RectJob, oy); FLD(RectJob, ou); FLD(RectJob, ov); FLD(RectJob, oa);
  FLD(RectJob, oa2);
  REC(CscPar); FLD(CscPar, m); FLD(CscPar, off); FLD(CscPar, interlaced);
  REC(PreJob); FLD(PreJob, sy); FLD(PreJob, suv); FLD(PreJob, sp_y); FLD(PreJob, sp_uv);
  return 0;
}
