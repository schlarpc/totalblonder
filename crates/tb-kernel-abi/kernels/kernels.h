// CUDA kernels of the MPEG-2 encoder. Every kernel is batched over the sessions of one format
// that code a picture in this tick: `list[0..na)` holds their session indices, a thread derives
// its slot k in the list from its index and its session from list[k]. Buffers are laid out
// [slot][session][...] or [session][...], so sessions never move in memory.
#pragma once
#include <cuda_runtime.h>
#include "common.h"
#include "entropy.h"

// Bufs (base pointers of the picture stores of one batch) and SL_* are in tb_kernel_abi.h.

__device__ inline int iabs(int a) { return a < 0 ? -a : a; }
__device__ inline int iclamp(int a, int lo, int hi) { return a < lo ? lo : (a > hi ? hi : a); }
__device__ inline int slot_of(const SessPar &p, int sel) { return sel == SL_CUR ? p.cur : sel == SL_REC ? p.rec : 0; }

// Padding of the source frames (the upload copies the picture straight into the padded planes).
// plane: one plane of the ring, frames `stride` bytes apart; the frame of session s is in slot[s].
// k_pad_cols: columns sw .. pw-1 of rows 0 .. sh-1 repeat column sw-1.
// k_pad_rows: rows sh .. ph-1 repeat row sh-1 (run it after k_pad_cols).
__global__ void k_pad_cols(uint8_t *plane, size_t stride, int pw, int sw, int sh, int n, const int *list, const uint8_t *slot, int na) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  size_t per = (size_t)(pw - sw) * sh;
  if (idx >= per * na) return;
  int s = list[idx / per];
  int r = (int)(idx % per);
  int y = r / (pw - sw), x = sw + r % (pw - sw);
  uint8_t *p = plane + ((size_t)slot[s] * n + s) * stride + (size_t)y * pw;
  p[x] = p[sw - 1];
}
__global__ void k_pad_rows(uint8_t *plane, size_t stride, int pw, int sh, int ph, int n, const int *list, const uint8_t *slot, int na) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  size_t per = (size_t)pw * (ph - sh);
  if (idx >= per * na) return;
  int s = list[idx / per];
  int r = (int)(idx % per);
  int y = sh + r / pw, x = r % pw;
  uint8_t *p = plane + ((size_t)slot[s] * n + s) * stride;
  p[(size_t)y * pw + x] = p[(size_t)(sh - 1) * pw + x];
}

// The two pyramid levels of a luma plane in one pass, per session: 2:1 box decimation to half
// resolution and again to quarter resolution. One thread per four pixels of the quarter plane
// (16 x 4 source pixels: 16 words in, four words of the half plane and one of the quarter plane
// out). src holds planes of sw x sh (sw a multiple of 16), `sstride` bytes apart per (slot,
// session); d1 planes of sw/2 x sh/2, d2 planes of sw/4 x sh/4; ssel/dsel say which slot of the
// session's parameters addresses the source and both destinations (SL_*).
__device__ inline uint32_t down2w(uint32_t a0, uint32_t a1, uint32_t b0, uint32_t b1) {
  const uint32_t M = 0x00ff00ffu;
  // 16-bit lanes: (even + odd pixel of both rows + 2) >> 2
  uint32_t s0 = (((a0 & M) + ((a0 >> 8) & M) + (b0 & M) + ((b0 >> 8) & M) + 0x00020002u) >> 2) & M;
  uint32_t s1 = (((a1 & M) + ((a1 >> 8) & M) + (b1 & M) + ((b1 >> 8) & M) + 0x00020002u) >> 2) & M;
  return (s0 & 0xffu) | ((s0 >> 8) & 0xff00u) | ((s1 & 0xffu) << 16) | ((s1 << 8) & 0xff000000u);
}
__global__ void k_pyr(const uint8_t *src, size_t sstride, int sw, int sh, uint8_t *d1, size_t d1stride, uint8_t *d2, size_t d2stride,
                      int n, const int *list, int na, const SessPar *par, int ssel, int dsel) {
  int qw = sw >> 4, qh = sh >> 2;   // words per row and rows of the quarter plane
  unsigned idx = blockIdx.x * blockDim.x + threadIdx.x;   // 32-bit index arithmetic: 64-bit division is slow
  unsigned per = (unsigned)(qw * qh);
  if (idx >= per * na) return;
  int s = list[idx / per];
  int r = (int)(idx % per);
  int y = r / qw, xw = r % qw;
  SessPar sp = par[s];
  size_t so = (size_t)slot_of(sp, ssel) * n + s, dn = (size_t)slot_of(sp, dsel) * n + s;
  int ws = sw >> 2;
  const uint32_t *p = (const uint32_t *)(src + so * sstride + (size_t)(4 * y) * sw) + 4 * xw;
  uint32_t *o1 = (uint32_t *)(d1 + dn * d1stride) + (size_t)(2 * y) * (sw >> 3) + 2 * xw;
  uint32_t h00 = down2w(p[0], p[1], p[ws], p[ws + 1]), h01 = down2w(p[2], p[3], p[ws + 2], p[ws + 3]);
  uint32_t h10 = down2w(p[2 * ws], p[2 * ws + 1], p[3 * ws], p[3 * ws + 1]), h11 = down2w(p[2 * ws + 2], p[2 * ws + 3], p[3 * ws + 2], p[3 * ws + 3]);
  o1[0] = h00; o1[1] = h01; o1[sw >> 3] = h10; o1[(sw >> 3) + 1] = h11;
  ((uint32_t *)(d2 + dn * d2stride))[(size_t)y * qw + xw] = down2w(h00, h01, h10, h11);
}

// Does nothing: with --tickmark the encoder launches it at the start of every tick so that a
// timeline (tools/gate/nsys_tick.py) can find the ticks.
__global__ void k_tick() {}

// Set every macroblock of the listed sessions to intra (mode = MODE_INTRA) or to forward
// prediction with a zero frame vector (mode = MODE_FWD; used for the all-skipped picture).
__global__ void k_set_mode(MbInfo *info, int nmb, const int *list, int na, int mode) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * nmb) return;
  int s = list[idx / nmb];
  MbInfo o;
  for (int i = 0; i < 8; i++) o.mv[i >> 2][(i >> 1) & 1][i & 1] = 0;
  o.mode = (uint8_t)mode; o.dct_type = 0; o.field_pred = 0; o.fsel = 0; o.cost = 0;
  info[(size_t)s * nmb + idx % nmb] = o;
}

// ---- packed-pixel helpers (four pixels per 32-bit word; rows of all planes are 4-byte aligned
// because the padded width is a multiple of 16; the planes are allocated with guard bytes on
// both sides so that an aligned load may start up to 4 bytes before a row and end 8 after it).
__device__ inline unsigned sad4w(const uint32_t *a, const uint32_t *b) {
  return __vsadu4(a[0], b[0]) + __vsadu4(a[1], b[1]) + __vsadu4(a[2], b[2]) + __vsadu4(a[3], b[3]);
}
__device__ inline void avg4w(const uint32_t *a, const uint32_t *b, uint32_t *o) {
  o[0] = __vavgu4(a[0], b[0]); o[1] = __vavgu4(a[1], b[1]); o[2] = __vavgu4(a[2], b[2]); o[3] = __vavgu4(a[3], b[3]);
}
// Per macroblock, on the source picture: (a) frame or field DCT (interlaced frame pictures):
// compare how well adjacent lines match (frame) with how well lines of the same field match;
// (b) spatial activity for adaptive quantization, after MPEG-2 Test Model 5 step 3:
// 1 + the smallest variance of the four frame-organized (and, when interlaced, the four
// field-organized) 8x8 luminance blocks.
__global__ void k_mbstat(Geo g, const uint8_t *ring_y, size_t ring_stride, const SessPar *par, const int *list, int na,
                         MbInfo *info, uint16_t *act) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * g.nmb) return;
  int s = list[idx / g.nmb], mb = idx % g.nmb, mbx = mb % g.mbw, mby = mb / g.mbw;
  const uint8_t *p = ring_y + ((size_t)par[s].cur * g.n + s) * ring_stride + (size_t)(mby * 16) * g.w + mbx * 16;
  unsigned dframe = 0, dfield = 0;
  unsigned sum[8] = {0, 0, 0, 0, 0, 0, 0, 0}, sq[8] = {0, 0, 0, 0, 0, 0, 0, 0};
  uint32_t r1[4] = {0, 0, 0, 0}, r2[4] = {0, 0, 0, 0};   // rows j - 1, j - 2
#pragma unroll
  for (int j = 0; j < 16; j++) {
    const uint32_t *rp = (const uint32_t *)(p + (size_t)j * g.w);
    uint32_t r[4] = {rp[0], rp[1], rp[2], rp[3]};
    if (j >= 1) dframe += sad4w(r, r1);
    if (j >= 2) dfield += sad4w(r, r2);
#pragma unroll
    for (int hb = 0; hb < 2; hb++) {
      unsigned sm = __vsadu4(r[2 * hb], 0) + __vsadu4(r[2 * hb + 1], 0);
      unsigned qq = __dp4a(r[2 * hb], r[2 * hb], __dp4a(r[2 * hb + 1], r[2 * hb + 1], 0u));
      int fb = (j >> 3) * 2 + hb, ib = 4 + (j & 1) * 2 + hb;
      sum[fb] += sm; sq[fb] += qq; sum[ib] += sm; sq[ib] += qq;
    }
#pragma unroll
    for (int q = 0; q < 4; q++) { r2[q] = r1[q]; r1[q] = r[q]; }
  }
  unsigned best = 0xffffffffu;
  // a picture coded without field tools (SessPar::frame_only: both fields are one instant)
  // has no field DCT and no field-organized blocks to measure
  bool ft = g.interlaced && !par[s].frame_only;
  for (int k = 0; k < (ft ? 8 : 4); k++) {
    unsigned var = (sq[k] - ((sum[k] * sum[k]) >> 6)) >> 6;
    if (var < best) best = var;
  }
  size_t mbi = (size_t)s * g.nmb + mb;
  act[mbi] = (uint16_t)(best > 65534u ? 65535u : best + 1);
  // 15 line pairs against 14: scale, and ask for a clear win before leaving frame DCT
  info[mbi].dct_type = ft ? (dfield * 15 + 14 * 15 * 64 < dframe * 14) : 0;
}

// Exact four-sample half-sample average of packed bytes, (a + b + c + d + 2) >> 2 per byte (7.6.4).
__device__ inline uint32_t avg4x(uint32_t a, uint32_t b, uint32_t c, uint32_t d) {
  const uint32_t M6 = 0x3f3f3f3fu, M2 = 0x03030303u;
  uint32_t hi = ((a >> 2) & M6) + ((b >> 2) & M6) + ((c >> 2) & M6) + ((d >> 2) & M6);
  uint32_t lo = (a & M2) + (b & M2) + (c & M2) + (d & M2) + 0x02020202u;
  return hi + ((lo >> 2) & M2);
}
// Exact half-sample prediction of 16 pixels (7.6.4) from rows rowa/rowb (rowb is only read when
// lh is odd), xh in half samples.
__device__ inline void hp16x(const uint8_t *rowa, const uint8_t *rowb, int xh, int lh, uint32_t *o) {
  int x = xh >> 1;
  int a = x & ~3, o8 = (x - a) * 8;
  const uint32_t *wp = (const uint32_t *)(rowa + a);
  uint32_t W[5] = {wp[0], wp[1], wp[2], wp[3], wp[4]};   // 17 pixels from an offset of at most 3 bytes
  uint32_t A[5], A1[4];
  for (int i = 0; i < 4; i++) A[i] = __funnelshift_r(W[i], W[i + 1], o8);
  A[4] = W[4] >> o8;
  if (xh & 1) for (int i = 0; i < 4; i++) A1[i] = __funnelshift_r(A[i], A[i + 1], 8);
  if (lh & 1) {
    wp = (const uint32_t *)(rowb + a);
    uint32_t V[5] = {wp[0], wp[1], wp[2], wp[3], wp[4]}, C[5];
    for (int i = 0; i < 4; i++) C[i] = __funnelshift_r(V[i], V[i + 1], o8);
    C[4] = V[4] >> o8;
    if (xh & 1) for (int i = 0; i < 4; i++) o[i] = avg4x(A[i], A1[i], C[i], __funnelshift_r(C[i], C[i + 1], 8));
    else for (int i = 0; i < 4; i++) o[i] = __vavgu4(A[i], C[i]);
  } else if (xh & 1) {
    for (int i = 0; i < 4; i++) o[i] = __vavgu4(A[i], A1[i]);
  } else {
    for (int i = 0; i < 4; i++) o[i] = A[i];
  }
}

// Result of the field search for one reference: per field of the macroblock (0 top, 1 bottom)
// the best vector (half samples; vertical in half field lines), the reference field and the SAD.
struct FieldRes { int mvx[2], mvy[2], sel[2]; unsigned sad[2]; };

// Half-sample refinement of one field of the macroblock (16 x 8 field lines) around a
// full-sample field vector, eight neighbors, packed pixels read straight from the planes.
// cur points at the macroblock's first line of that field; ref is the reference frame plane.
// (x0, L0): position of the block in the reference field `sel` (pixels, field lines).
__device__ inline unsigned field_refine(const uint8_t *cur, const uint8_t *ref, int w, int h, int x0, int L0, int sel,
                                        unsigned sad_full, int *ox_out, int *oy_out) {
  int hf = h >> 1;
  bool left = x0 - 1 >= 0, right = x0 + 16 <= w - 1, up = L0 - 1 >= 0, down = L0 + 8 <= hf - 1;
  unsigned acc[8] = {0, 0, 0, 0, 0, 0, 0, 0};
  uint32_t pC[4], pL[4], pR[4], cprev[4] = {0, 0, 0, 0};
  int a = (x0 - 1) & ~3, s8 = ((x0 - 1) - a) * 8;
  for (int i = -1; i <= 8; i++) {
    int rl = iclamp(L0 + i, 0, hf - 1);
    const uint32_t *wp = (const uint32_t *)(ref + (size_t)(2 * rl + sel) * w + a);
    uint32_t W[6] = {wp[0], wp[1], wp[2], wp[3], wp[4], wp[5]};
    uint32_t A[5], C[4], hL[4], hR[4], t[4];
#pragma unroll
    for (int q = 0; q < 5; q++) A[q] = __funnelshift_r(W[q], W[q + 1], s8);   // 20 pixels from x0 - 1
#pragma unroll
    for (int q = 0; q < 4; q++) { t[q] = A[q]; C[q] = __funnelshift_r(A[q], A[q + 1], 8); }
    avg4w(t, C, hL);
#pragma unroll
    for (int q = 0; q < 4; q++) t[q] = __funnelshift_r(A[q], A[q + 1], 16);
    avg4w(C, t, hR);
    uint32_t cw[4] = {0, 0, 0, 0};
    if (i >= 0 && i <= 7) {
      const uint32_t *cp = (const uint32_t *)(cur + (size_t)(2 * i) * w);
      cw[0] = cp[0]; cw[1] = cp[1]; cw[2] = cp[2]; cw[3] = cp[3];
      acc[0] += sad4w(cw, hL); acc[1] += sad4w(cw, hR);
    }
    if (i >= 0) {
      uint32_t V[4], DL[4], DR[4];
      avg4w(pC, C, V); avg4w(pL, hL, DL); avg4w(pR, hR, DR);
      if (i <= 7) { acc[2] += sad4w(cw, V); acc[4] += sad4w(cw, DL); acc[5] += sad4w(cw, DR); }
      if (i >= 1) { acc[3] += sad4w(cprev, V); acc[6] += sad4w(cprev, DL); acc[7] += sad4w(cprev, DR); }
    }
    for (int q = 0; q < 4; q++) { pC[q] = C[q]; pL[q] = hL[q]; pR[q] = hR[q]; cprev[q] = cw[q]; }
  }
  // candidate k -> (ox, oy): 0 (-1,0) 1 (1,0) 2 (0,-1) 3 (0,1) 4 (-1,-1) 5 (1,-1) 6 (-1,1) 7 (1,1)
  unsigned best = sad_full;
  int bx = 0, by = 0;
  for (int k = 0; k < 8; k++) {
    int ox = k < 2 ? 2 * k - 1 : k < 4 ? 0 : 2 * (k & 1) - 1;
    int oy = k < 2 ? 0 : k < 4 ? 2 * (k & 1) - 1 : (k < 6 ? -1 : 1);
    if ((ox < 0 && !left) || (ox > 0 && !right) || (oy < 0 && !up) || (oy > 0 && !down)) continue;
    if (acc[k] < best) { best = acc[k]; bx = ox; by = oy; }
  }
  *ox_out = bx; *oy_out = by;
  return best;
}

// Full-sample search of one field of the macroblock (16 x 8 field lines) in reference field
// `sel`, the nine positions around (x0, L0) (pixels, field lines), packed pixels. Updates the
// best SAD and position when it finds a better one; returns true in that case.
__device__ inline bool field_seed(const uint8_t *cur, const uint8_t *ref, int w, int h, int x0, int L0, int sel,
                                  unsigned *best, int *bx, int *bl) {
  int hf = h >> 1;
  x0 = iclamp(x0, 0, w - 16); L0 = iclamp(L0, 0, hf - 8);
  unsigned acc[9] = {0, 0, 0, 0, 0, 0, 0, 0, 0};
  int a = (x0 - 1) & ~3, s8 = ((x0 - 1) - a) * 8;
  uint32_t cm[4] = {0, 0, 0, 0}, cc[4] = {0, 0, 0, 0}, cn[4];   // current field lines i - 1, i, i + 1
  { const uint32_t *cp = (const uint32_t *)cur; cn[0] = cp[0]; cn[1] = cp[1]; cn[2] = cp[2]; cn[3] = cp[3]; }
#pragma unroll
  for (int i = -1; i <= 8; i++) {
    int rl = iclamp(L0 + i, 0, hf - 1);
    const uint32_t *wp = (const uint32_t *)(ref + (size_t)(2 * rl + sel) * w + a);
    uint32_t W[6] = {wp[0], wp[1], wp[2], wp[3], wp[4], wp[5]};
    uint32_t A[5];
#pragma unroll
    for (int q = 0; q < 5; q++) A[q] = __funnelshift_r(W[q], W[q + 1], s8);
#pragma unroll
    for (int dx = 0; dx < 3; dx++) {
      uint32_t t[4];
#pragma unroll
      for (int q = 0; q < 4; q++) t[q] = __funnelshift_r(A[q], A[q + 1], 8 * dx);
      // reference line i serves current line i - dy for dy = -1, 0, 1
      if (i + 1 <= 7) acc[dx] += sad4w(cn, t);
      if (i >= 0 && i <= 7) acc[3 + dx] += sad4w(cc, t);
      if (i - 1 >= 0) acc[6 + dx] += sad4w(cm, t);
    }
#pragma unroll
    for (int q = 0; q < 4; q++) { cm[q] = cc[q]; cc[q] = cn[q]; }
    if (i + 2 <= 7) {
      const uint32_t *cp = (const uint32_t *)(cur + (size_t)(2 * (i + 2)) * w);
      cn[0] = cp[0]; cn[1] = cp[1]; cn[2] = cp[2]; cn[3] = cp[3];
    }
  }
  bool better = false;
  for (int k = 0; k < 9; k++) {
    int dx = k % 3 - 1, dy = k / 3 - 1;
    if (x0 + dx < 0 || x0 + dx > w - 16 || L0 + dy < 0 || L0 + dy > hf - 8) continue;
    if (acc[k] < *best) { *best = acc[k]; *bx = x0 + dx; *bl = L0 + dy; better = true; }
  }
  return better;
}

// SAD of the macroblock against the average of two field-based predictions (bidirectional).
__device__ inline unsigned field_bi_sad(const uint8_t *cur, const uint8_t *rf, const uint8_t *rb, int w, int h, int px,
                                        int py, const FieldRes &f, const FieldRes &b) {
  unsigned sad = 0;
  int hf = h >> 1;
  for (int fld = 0; fld < 2; fld++) {
    for (int r = 0; r < 8; r++) {
      uint32_t a[4], c[4];
      int lf = 2 * (py / 2 + r) + f.mvy[fld], lb = 2 * (py / 2 + r) + b.mvy[fld];
      int la = iclamp(lf >> 1, 0, hf - 1), la2 = iclamp((lf >> 1) + (lf & 1), 0, hf - 1);
      hp16x(rf + (size_t)(2 * la + f.sel[fld]) * w, rf + (size_t)(2 * la2 + f.sel[fld]) * w, 2 * px + f.mvx[fld], lf, a);
      la = iclamp(lb >> 1, 0, hf - 1); la2 = iclamp((lb >> 1) + (lb & 1), 0, hf - 1);
      hp16x(rb + (size_t)(2 * la + b.sel[fld]) * w, rb + (size_t)(2 * la2 + b.sel[fld]) * w, 2 * px + b.mvx[fld], lb, c);
      avg4w(a, c, a);
      sad += sad4w((const uint32_t *)(cur + (size_t)(py + 2 * r + fld) * w + px), a);
    }
  }
  return sad;
}

// Hierarchical motion search for one reference: 4x4 at 1/4 resolution over +-R2 (= +-4*R2
// full-pel), +-1 at 1/2 resolution, +-1 at full resolution, then the 8 half-pel neighbors.
// Leaves the half-pel frame vector in mv and returns the frame SAD.
// When fr is not null it also returns the best field-based prediction: the nine full-pel frame
// candidates are scored separately on the even and the odd lines of the macroblock, which gives
// nine candidates per field for free (a frame candidate with vertical offset dy predicts the top
// field from the reference field of parity dy & 1), then each field gets a half-pel step.
// Everything works on packed pixels (four per 32-bit word, __vsadu4 / __vavgu4) read straight
// from the planes; there are no per-thread pixel arrays. Rows that would lie outside the picture
// are clamped for the load and their candidates masked; a load may run a few bytes over the end
// of a row (into the next row or the guard bytes), which the candidate masks make harmless.
__device__ inline unsigned me_search(const Geo &g, const uint8_t *y0, const uint8_t *y1, const uint8_t *y2,
                                     const uint8_t *cury, const uint8_t *cur1, const uint8_t *cur2, int mbx, int mby,
                                     int *mv, FieldRes *fr, int lam, int tdist2, int tff, int tp, int tpx, int tpy) {
  int w = g.w, h = g.h, w1 = w >> 1, h1 = h >> 1, w2 = w >> 2, h2 = h >> 2;
  // The level-2 loads below reach 4 * ((R2 + 3) / 4) bytes before a row and that plus 7 past
  // it. The host refuses a range above ME_MAX_RANGE2 (the vectors of a larger one have no legal
  // f_code); the clamp keeps the loads inside the guard bytes of the buffer whatever arrives.
  int R2 = g.range2 > ME_MAX_RANGE2 ? ME_MAX_RANGE2 : g.range2 < 0 ? 0 : g.range2, lambda = g.lambda;
  // level 2: px is a multiple of 4, so candidate dx = 4 * wx + sub is word wx shifted by sub bytes
  int px = mbx * 4, py = mby * 4;
  int bx = 0, by = 0;
  unsigned best = 0xffffffffu;
  {
    const uint32_t *cp = (const uint32_t *)(cur2 + (size_t)py * w2 + px);
    int ws = w2 >> 2, nwx = (R2 + 3) >> 2;
    uint32_t c0 = cp[0], c1 = cp[ws], c2 = cp[2 * ws], c3 = cp[3 * ws];
    for (int dy = -R2; dy <= R2; dy++) {
      int yy = py + dy;
      if (yy < 0 || yy > h2 - 4) continue;
      const uint32_t *rp = (const uint32_t *)(y2 + (size_t)yy * w2 + px);
      uint32_t l0 = rp[-nwx], l1 = rp[ws - nwx], l2 = rp[2 * ws - nwx], l3 = rp[3 * ws - nwx];
      for (int wx = -nwx; wx <= nwx; wx++) {
        uint32_t h0 = rp[wx + 1], h1w = rp[ws + wx + 1], h2w = rp[2 * ws + wx + 1], h3 = rp[3 * ws + wx + 1];
#pragma unroll
        for (int sub = 0; sub < 4; sub++) {
          int dx = 4 * wx + sub, xx = px + dx;
          unsigned sad = __vsadu4(c0, __funnelshift_r(l0, h0, 8 * sub)) + __vsadu4(c1, __funnelshift_r(l1, h1w, 8 * sub)) +
                         __vsadu4(c2, __funnelshift_r(l2, h2w, 8 * sub)) + __vsadu4(c3, __funnelshift_r(l3, h3, 8 * sub));
          unsigned cost = sad * 4 + lambda * (iabs(dx) + iabs(dy));
          if (dx >= -R2 && dx <= R2 && xx >= 0 && xx <= w2 - 4 && cost < best) { best = cost; bx = dx; by = dy; }
        }
        l0 = h0; l1 = h1w; l2 = h2w; l3 = h3;
      }
    }
  }
  // level 1: nine candidates around (2 bx, 2 by) at half resolution, 8x8 pixels
  px = mbx * 8; py = mby * 8;
  int cx = bx * 2, cy = by * 2;
  {
    uint32_t cw[8][2];
#pragma unroll
    for (int j = 0; j < 8; j++) {
      const uint32_t *cp = (const uint32_t *)(cur1 + (size_t)(py + j) * w1 + px);
      cw[j][0] = cp[0]; cw[j][1] = cp[1];
    }
    unsigned acc[9] = {0, 0, 0, 0, 0, 0, 0, 0, 0};
    int x0 = px + cx - 1, a = x0 & ~3, s8 = (x0 - a) * 8;
#pragma unroll
    for (int i = -1; i <= 8; i++) {
      int ry = iclamp(py + cy + i, 0, h1 - 1);
      const uint32_t *wp = (const uint32_t *)(y1 + (size_t)ry * w1 + a);
      uint32_t W0 = wp[0], W1 = wp[1], W2 = wp[2], W3 = wp[3];
      uint32_t A0 = __funnelshift_r(W0, W1, s8), A1 = __funnelshift_r(W1, W2, s8), A2 = __funnelshift_r(W2, W3, s8);
#pragma unroll
      for (int dx = 0; dx < 3; dx++) {
        uint32_t t0 = __funnelshift_r(A0, A1, 8 * dx), t1 = __funnelshift_r(A1, A2, 8 * dx);
#pragma unroll
        for (int dy = -1; dy <= 1; dy++) {
          int ci = i - dy;
          if (ci >= 0 && ci <= 7) acc[(dy + 1) * 3 + dx] += __vsadu4(cw[ci][0], t0) + __vsadu4(cw[ci][1], t1);
        }
      }
    }
    bx = cx; by = cy; best = 0xffffffffu;
#pragma unroll
    for (int k = 0; k < 9; k++) {
      int dx = cx + k % 3 - 1, dy = cy + k / 3 - 1, yy = py + dy, xx = px + dx;
      if (yy >= 0 && yy <= h1 - 8 && xx >= 0 && xx <= w1 - 8 && acc[k] < best) { best = acc[k]; bx = dx; by = dy; }
    }
  }
  // level 0: nine full-pel candidates around (2 bx, 2 by), each scored on the even and odd lines
  px = mbx * 16; py = mby * 16;
  cx = iclamp(bx * 2, -px, w - 16 - px);
  cy = iclamp(by * 2, -py, h - 16 - py);
  const uint8_t *curp = cury + (size_t)py * w + px;
  int ix = cx, iy = cy;
  best = 0xffffffffu;
  unsigned fbest[2] = {0xffffffffu, 0xffffffffu};
  int ffx[2] = {cx, cx}, ffy[2] = {cy, cy};
  {
    unsigned ae[9] = {0, 0, 0, 0, 0, 0, 0, 0, 0}, ao[9] = {0, 0, 0, 0, 0, 0, 0, 0, 0};
    int x0 = px + cx - 1, a = x0 & ~3, s8 = (x0 - a) * 8;
    uint32_t cm[4] = {0, 0, 0, 0}, cc[4] = {0, 0, 0, 0}, cn[4];
    { const uint32_t *cp = (const uint32_t *)curp; cn[0] = cp[0]; cn[1] = cp[1]; cn[2] = cp[2]; cn[3] = cp[3]; }
#pragma unroll
    for (int i = -1; i <= 16; i++) {
      // cm, cc, cn: current rows i - 1, i, i + 1
      int ry = iclamp(py + cy + i, 0, h - 1);
      const uint32_t *wp = (const uint32_t *)(y0 + (size_t)ry * w + a);
      uint32_t W[6] = {wp[0], wp[1], wp[2], wp[3], wp[4], wp[5]};
      uint32_t A[5];
#pragma unroll
      for (int q = 0; q < 5; q++) A[q] = __funnelshift_r(W[q], W[q + 1], s8);
#pragma unroll
      for (int dx = 0; dx < 3; dx++) {
        uint32_t t[4];
#pragma unroll
        for (int q = 0; q < 4; q++) t[q] = __funnelshift_r(A[q], A[q + 1], 8 * dx);
        // reference row i serves current row i - dy
        if (i + 1 <= 15) { if ((i + 1) & 1) ao[dx] += sad4w(cn, t); else ae[dx] += sad4w(cn, t); }                    // dy = -1
        if (i >= 0 && i <= 15) { if (i & 1) ao[3 + dx] += sad4w(cc, t); else ae[3 + dx] += sad4w(cc, t); }             // dy = 0
        if (i - 1 >= 0) { if ((i - 1) & 1) ao[6 + dx] += sad4w(cm, t); else ae[6 + dx] += sad4w(cm, t); }              // dy = 1
      }
#pragma unroll
      for (int q = 0; q < 4; q++) { cm[q] = cc[q]; cc[q] = cn[q]; }
      if (i + 2 <= 15) {
        const uint32_t *cp = (const uint32_t *)(curp + (size_t)(i + 2) * w);
        cn[0] = cp[0]; cn[1] = cp[1]; cn[2] = cp[2]; cn[3] = cp[3];
      }
    }
#pragma unroll
    for (int k = 0; k < 9; k++) {
      int dx = k % 3 - 1, dy = k / 3 - 1, yy = py + cy + dy, xx = px + cx + dx;
      if (yy < 0 || yy > h - 16 || xx < 0 || xx > w - 16) continue;
      unsigned se = ae[k], so = ao[k];
      if (se + so < best) { best = se + so; ix = cx + dx; iy = cy + dy; }
      if (se < fbest[0]) { fbest[0] = se; ffx[0] = cx + dx; ffy[0] = cy + dy; }
      if (so < fbest[1]) { fbest[1] = so; ffx[1] = cx + dx; ffy[1] = cy + dy; }
    }
  }
  if (tp) {
    // Temporal candidate: the full-pel position the co-located macroblock of the previous coded
    // picture predicts for this reference (tpx, tpy), kept inside the picture and the vector range.
    int lim = 4 * R2 + 3;
    int qx = iclamp(iclamp(tpx, -lim, lim), -px, w - 16 - px), qy = iclamp(iclamp(tpy, -lim, lim), -py, h - 16 - py);
    unsigned se = 0, so = 0;
    int x0 = px + qx, a = x0 & ~3, s8 = (x0 - a) * 8;
    for (int j = 0; j < 16; j++) {   // not unrolled: the kernel is short of registers
      const uint32_t *wp = (const uint32_t *)(y0 + (size_t)(py + qy + j) * w + a);
      uint32_t W[5] = {wp[0], wp[1], wp[2], wp[3], wp[4]}, t[4];
#pragma unroll
      for (int q = 0; q < 4; q++) t[q] = __funnelshift_r(W[q], W[q + 1], s8);
      unsigned sd = sad4w((const uint32_t *)(curp + (size_t)j * w), t);
      if (j & 1) so += sd; else se += sd;
    }
    if (se + so < best) { best = se + so; ix = qx; iy = qy; }
    if (se < fbest[0]) { fbest[0] = se; ffx[0] = qx; ffy[0] = qy; }
    if (so < fbest[1]) { fbest[1] = so; ffx[1] = qx; ffy[1] = qy; }
  }
  if (fr) {
    for (int f = 0; f < 2; f++) {
      // Current field line k of the macroblock is frame row py + 2k + f; the frame candidate
      // maps it to reference frame row py + 2k + f + ffy: field (f + ffy) & 1, field line
      // (py + 2k + f + ffy - sel) / 2.
      int sel = (f + ffy[f]) & 1;
      int mvy_full = (f + ffy[f] - sel) / 2;   // exact: the numerator is even
      int bx = px + ffx[f], bl = py / 2 + mvy_full;
      const uint8_t *cf = cury + (size_t)(py + f) * w + px;
      // Second seed: the reference field of the other parity is one field period nearer or
      // farther than the frame distance, so for steady motion its vector is the frame vector
      // scaled by the ratio of the field distances (the frame candidates above only cover that
      // field at the unscaled displacement).
      if (tdist2 != 0) {
        int s2 = 1 - f;
        int dp = tff ? (f - s2) : (s2 - f);
        float sc = (float)(tdist2 + dp) / (float)tdist2;
        int dxs = (int)floorf(fmaf((float)ix, sc, 0.5f));
        int ls = (int)floorf(fmaf(fmaf((float)iy, sc, (float)(f - s2)), 0.5f, 0.5f));
        if (field_seed(cf, y0, w, h, px + dxs, py / 2 + ls, s2, &fbest[f], &bx, &bl)) sel = s2;
      }
      int ox = 0, oy = 0;
      fr->sad[f] = field_refine(cf, y0, w, h, bx, bl, sel, fbest[f], &ox, &oy);
      fr->sel[f] = sel; fr->mvx[f] = 2 * (bx - px) + ox; fr->mvy[f] = 2 * (bl - py / 2) + oy;
    }
  }
  // half-pel ring around (ix, iy): acc k -> (ox, oy): 0 (-1,0) 1 (1,0) 2 (0,-1) 3 (0,1) 4 (-1,-1) 5 (1,-1) 6 (-1,1) 7 (1,1)
  int hx = 0, hy = 0;
  {
    unsigned acc[8] = {0, 0, 0, 0, 0, 0, 0, 0};
    int x0 = px + ix - 1, a = x0 & ~3, s8 = (x0 - a) * 8;
    uint32_t pL[4], pC[4], pR[4], cprev[4] = {0, 0, 0, 0};
#pragma unroll
    for (int i = -1; i <= 16; i++) {
      int ry = iclamp(py + iy + i, 0, h - 1);
      const uint32_t *wp = (const uint32_t *)(y0 + (size_t)ry * w + a);
      uint32_t W[6] = {wp[0], wp[1], wp[2], wp[3], wp[4], wp[5]};
      uint32_t A[5], L[4], C[4], R[4], hL[4], hR[4];
#pragma unroll
      for (int q = 0; q < 5; q++) A[q] = __funnelshift_r(W[q], W[q + 1], s8);
#pragma unroll
      for (int q = 0; q < 4; q++) { L[q] = A[q]; C[q] = __funnelshift_r(A[q], A[q + 1], 8); R[q] = __funnelshift_r(A[q], A[q + 1], 16); }
      avg4w(L, C, hL); avg4w(C, R, hR);
      uint32_t cw[4] = {0, 0, 0, 0};
      if (i >= 0 && i <= 15) {
        const uint32_t *cp = (const uint32_t *)(curp + (size_t)i * w);
        cw[0] = cp[0]; cw[1] = cp[1]; cw[2] = cp[2]; cw[3] = cp[3];
        acc[0] += sad4w(cw, hL); acc[1] += sad4w(cw, hR);
      }
      if (i >= 0) {
        uint32_t V[4], DL[4], DR[4];
        avg4w(pC, C, V);
#pragma unroll
        for (int q = 0; q < 4; q++) { DL[q] = avg4x(pL[q], pC[q], L[q], C[q]); DR[q] = avg4x(pC[q], pR[q], C[q], R[q]); }
        if (i <= 15) { acc[2] += sad4w(cw, V); acc[4] += sad4w(cw, DL); acc[5] += sad4w(cw, DR); }
        if (i >= 1) { acc[3] += sad4w(cprev, V); acc[6] += sad4w(cprev, DL); acc[7] += sad4w(cprev, DR); }
      }
#pragma unroll
      for (int q = 0; q < 4; q++) { pL[q] = L[q]; pC[q] = C[q]; pR[q] = R[q]; cprev[q] = cw[q]; }
    }
    bool up = py + iy - 1 >= 0, down = py + iy + 16 <= h - 1, left = px + ix - 1 >= 0, right = px + ix + 16 <= w - 1;
    // same order as a raster scan of the ring, so that ties resolve as before
    if (up && left && acc[4] < best) { best = acc[4]; hx = -1; hy = -1; }
    if (up && acc[2] < best) { best = acc[2]; hx = 0; hy = -1; }
    if (up && right && acc[5] < best) { best = acc[5]; hx = 1; hy = -1; }
    if (left && acc[0] < best) { best = acc[0]; hx = -1; hy = 0; }
    if (right && acc[1] < best) { best = acc[1]; hx = 1; hy = 0; }
    if (down && left && acc[6] < best) { best = acc[6]; hx = -1; hy = 1; }
    if (down && acc[3] < best) { best = acc[3]; hx = 0; hy = 1; }
    if (down && right && acc[7] < best) { best = acc[7]; hx = 1; hy = 1; }
  }
  // Prefer the zero vector when it is nearly as good (cheaper to code, enables skipping).
  int mvx = 2 * ix + hx, mvy = 2 * iy + hy;
  if (mvx != 0 || mvy != 0) {
    unsigned sad0 = 0;
    for (int j = 0; j < 16; j++)
      sad0 += sad4w((const uint32_t *)(curp + (size_t)j * w), (const uint32_t *)(y0 + (size_t)(py + j) * w + px));
    if (sad0 <= best + 16 + 6 * lam) { best = sad0; mvx = mvy = 0; }   // a zero vector saves about 6 bits (and lets P macroblocks be skipped)
  }
  mv[0] = mvx; mv[1] = mvy;
  return best;
}

// Motion estimation and macroblock mode decision. One thread per macroblock of every listed
// session; the session's picture type gives the number of references (P: forward; B: both).
__global__ void k_me(Geo g, Bufs B, const SessPar *par, const int *list, int na, const MbInfo *prev, MbInfo *out) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * g.nmb) return;
  int s = list[idx / g.nmb], mb = idx % g.nmb, mbx = mb % g.mbw, mby = mb / g.mbw;
  SessPar sp = par[s];
  int nref = sp.pic_type == PIC_B ? 2 : 1;
  int w = g.w, h = g.h;
  size_t ysz = (size_t)w * h;
  const uint8_t *cury = B.ring_y + ((size_t)sp.cur * g.n + s) * B.ring_stride;
  const uint8_t *cur1 = B.cur1 + (size_t)s * (ysz / 4), *cur2 = B.cur2 + (size_t)s * (ysz / 16);
  const uint8_t *curp = cury + (size_t)(mby * 16) * w + mbx * 16;
  unsigned sum = 0, act = 0;
  for (int j = 0; j < 16; j++) {
    const uint32_t *cp = (const uint32_t *)(curp + (size_t)j * w);
    sum += __vsadu4(cp[0], 0) + __vsadu4(cp[1], 0) + __vsadu4(cp[2], 0) + __vsadu4(cp[3], 0);
  }
  uint32_t m4 = ((sum + 128) >> 8) * 0x01010101u;
  for (int j = 0; j < 16; j++) {
    const uint32_t *cp = (const uint32_t *)(curp + (size_t)j * w);
    act += __vsadu4(cp[0], m4) + __vsadu4(cp[1], m4) + __vsadu4(cp[2], m4) + __vsadu4(cp[3], m4);
  }

  MbInfo o;
  for (int i = 0; i < 8; i++) o.mv[i >> 2][(i >> 1) & 1][i & 1] = 0;
  o.dct_type = 0; o.field_pred = 0; o.fsel = 0;
  bool fld = g.field_pred != 0 && !sp.frame_only;   // no field-based prediction in a frame-only picture
  FieldRes fres[2];
  int mvs[2][2] = {{0, 0}, {0, 0}};
  unsigned sads[2] = {0, 0};
  int lam = sp.lam;
  unsigned fpen = (unsigned)(g.field_bits * lam);
  const uint8_t *refy[2];
  // Motion of the co-located macroblock of the session's previous coded picture, in half
  // samples per frame of display time (a field vector's vertical component counts double).
  float vx = 0, vy = 0;
  int tp = 0;
  if (g.tpred) {
    MbInfo pm = prev[(size_t)s * g.nmb + mb];
    int d = (pm.mode & MODE_FWD) ? sp.pdist_f : sp.pdist_b, dir = (pm.mode & MODE_FWD) ? 0 : 1;
    if (pm.mode != MODE_INTRA && d != 0) {
      tp = 1;
      vx = (float)pm.mv[0][dir][0] / (float)d;
      vy = (float)(pm.field_pred ? 2 * pm.mv[0][dir][1] : pm.mv[0][dir][1]) / (float)d;
    }
  }
  for (int r = 0; r < nref; r++) {
    size_t of = (size_t)(r ? sp.bwd : sp.fwd) * g.n + s;
    refy[r] = B.ref_y + of * ysz;
    int dist = r ? sp.dist_b : sp.dist_f;
    sads[r] = me_search(g, refy[r], B.ref_y1 + of * (ysz / 4), B.ref_y2 + of * (ysz / 16), cury, cur1, cur2, mbx, mby, mvs[r],
                        fld ? &fres[r] : nullptr, lam, 2 * dist, sp.tff, tp, (int)floorf(fmaf(vx * dist, 0.5f, 0.5f)),
                        (int)floorf(fmaf(vy * dist, 0.5f, 0.5f)));
  }
  unsigned bestsad = sads[0];
  int mode = MODE_FWD, field = 0;
  if (fld) {
    unsigned t = fres[0].sad[0] + fres[0].sad[1] + fpen;
    if (t < bestsad) { bestsad = t; field = 1; }
  }
  if (nref == 2) {
    unsigned sb = sads[1];
    unsigned sbi = 0;
    for (int j = 0; j < 16; j++) {
      uint32_t a[4], b[4];
      int yf = 2 * (mby * 16 + j) + mvs[0][1], yb = 2 * (mby * 16 + j) + mvs[1][1];
      const uint8_t *ra = refy[0] + (size_t)(yf >> 1) * w, *rb = refy[1] + (size_t)(yb >> 1) * w;
      hp16x(ra, ra + w, 2 * mbx * 16 + mvs[0][0], yf, a);
      hp16x(rb, rb + w, 2 * mbx * 16 + mvs[1][0], yb, b);
      avg4w(a, b, a);
      sbi += sad4w((const uint32_t *)(curp + (size_t)j * w), a);
    }
    if (sb < bestsad) { bestsad = sb; mode = MODE_BWD; field = 0; }
    unsigned bipen = (unsigned)(8 + 6 * lam);   // the second vector
    if (sbi + bipen < bestsad) { bestsad = sbi + bipen; mode = MODE_BI; field = 0; }
    if (fld) {
      unsigned t = fres[1].sad[0] + fres[1].sad[1] + fpen;
      if (t < bestsad) { bestsad = t; mode = MODE_BWD; field = 1; }
      t = field_bi_sad(cury, refy[0], refy[1], w, h, mbx * 16, mby * 16, fres[0], fres[1]) + bipen + 2 * fpen;
      if (t < bestsad) { bestsad = t; mode = MODE_BI; field = 1; }
    }
  }
  // an intra macroblock costs its six DC differences and a longer type code: about 40 bits more
  if (act + 512 + 40 * (unsigned)lam < bestsad) { mode = MODE_INTRA; bestsad = act; field = 0; }
  if (mode & MODE_FWD) {
    if (field) for (int r = 0; r < 2; r++) { o.mv[r][0][0] = (int16_t)fres[0].mvx[r]; o.mv[r][0][1] = (int16_t)fres[0].mvy[r]; o.fsel |= fres[0].sel[r] << r; }
    else { o.mv[0][0][0] = (int16_t)mvs[0][0]; o.mv[0][0][1] = (int16_t)mvs[0][1]; }
  }
  if (mode & MODE_BWD) {
    if (field) for (int r = 0; r < 2; r++) { o.mv[r][1][0] = (int16_t)fres[1].mvx[r]; o.mv[r][1][1] = (int16_t)fres[1].mvy[r]; o.fsel |= fres[1].sel[r] << (2 + r); }
    else { o.mv[0][1][0] = (int16_t)mvs[1][0]; o.mv[0][1][1] = (int16_t)mvs[1][1]; }
  }
  o.mode = (uint8_t)mode;
  o.field_pred = (uint8_t)field;
  o.cost = bestsad;
  out[(size_t)s * g.nmb + mb] = o;
}

// SAD of the macroblock at (px, py) against a frame-based prediction with the vectors of `m`
// (mode FWD, BWD or BI). Returns 0xffffffff when a vector points outside the picture.
__device__ inline unsigned frame_pred_sad(const uint8_t *cur, const uint8_t *rf, const uint8_t *rb, int w, int h, int px,
                                          int py, const MbInfo &m) {
  for (int s = 0; s < 2; s++) {
    if (!(m.mode & (s ? MODE_BWD : MODE_FWD))) continue;
    int x = 2 * px + m.mv[0][s][0], y = 2 * py + m.mv[0][s][1];
    if (x < 0 || y < 0 || ((x + 1) >> 1) + 16 > w || ((y + 1) >> 1) + 16 > h) return 0xffffffffu;
  }
  unsigned sad = 0;
  for (int j = 0; j < 16; j++) {
    uint32_t a[4], b[4];
    bool have = false;
    for (int s = 0; s < 2; s++) {
      if (!(m.mode & (s ? MODE_BWD : MODE_FWD))) continue;
      const uint8_t *ref = s ? rb : rf;
      int yh = 2 * (py + j) + m.mv[0][s][1];
      const uint8_t *ra = ref + (size_t)(yh >> 1) * w;
      hp16x(ra, ra + w, 2 * px + m.mv[0][s][0], yh, have ? b : a);
      if (have) avg4w(a, b, a);
      have = true;
    }
    sad += sad4w((const uint32_t *)(cur + (size_t)(py + j) * w + px), a);
  }
  return sad;
}

// Second pass of the motion decision: a macroblock adopts the mode and vectors of its left
// neighbor (the one its vectors are predicted from, 7.6.3) when that costs little SAD. The
// independent per-macroblock searches leave the vector field noisy; MPEG-2 pays for every
// vector difference and can skip a B macroblock only if it repeats its neighbor (7.6.6.4).
// in: the search result; out: the final decision (out may not alias in).
__global__ void k_mvsmooth(Geo g, Bufs B, const SessPar *par, const int *list, int na, const MbInfo *in, MbInfo *out) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * g.nmb) return;
  int s = list[idx / g.nmb], mb = idx % g.nmb, mbx = mb % g.mbw, mby = mb / g.mbw;
  size_t mbi = (size_t)s * g.nmb + mb;
  MbInfo o = in[mbi];
  if (g.smooth && mbx > 0 && o.mode != MODE_INTRA) {
    MbInfo l = in[mbi - 1];
    bool same = l.mode == o.mode && l.field_pred == o.field_pred && l.fsel == o.fsel;
    for (int i = 0; i < 8 && same; i++) same = l.mv[i >> 2][(i >> 1) & 1][i & 1] == o.mv[i >> 2][(i >> 1) & 1][i & 1];
    if (!same && l.mode != MODE_INTRA && !l.field_pred) {
      SessPar sp = par[s];
      size_t ysz = (size_t)g.w * g.h;
      const uint8_t *cury = B.ring_y + ((size_t)sp.cur * g.n + s) * B.ring_stride;
      const uint8_t *rf = B.ref_y + ((size_t)sp.fwd * g.n + s) * ysz, *rb = B.ref_y + ((size_t)sp.bwd * g.n + s) * ysz;
      unsigned sl = frame_pred_sad(cury, rf, rb, g.w, g.h, mbx * 16, mby * 16, l);
      // bits saved: the vector differences vanish, and a B macroblock without coefficients is skipped
      unsigned save = (unsigned)((sp.pic_type == PIC_B ? 12 : 6) * sp.lam);
      if (sl != 0xffffffffu && sl <= o.cost + save) {
        o.mode = l.mode; o.field_pred = 0; o.fsel = 0;
        for (int i = 0; i < 8; i++) o.mv[i >> 2][(i >> 1) & 1][i & 1] = l.mv[i >> 2][(i >> 1) & 1][i & 1];
        o.cost = sl;
      }
    }
  }
  out[mbi] = o;
}

// ---- transform ---------------------------------------------------------------------------
// 8-point DCT-II / DCT-III with the H.262 Annex A normalization, even/odd decomposition,
// everything in registers. Checked against Rust models (tb-device-ref transform.rs).
//
// Fused multiply-add is explicit in the float kernels (fdct8, idct8, n_act, mb_qcode, the
// roundings in k_xf and the temporal seeds of k_me): every fmaf below stands exactly where nvcc
// with its default -fmad=true contracted a multiply and an add in the code these kernels had
// before, and nowhere else. Every build compiles them without implicit contraction (nvcc
// -fmad=false, host -ffp-contract=off), so the device image is the same machine code as before
// and the host build computes the same bits (docs/ARCHITECTURE.md section 7.3).
//
// fmaf_nc(a, b, c) is fmaf(a, b, -c), as nvcc contracts a*b - c: one fused operation with the
// addend negated. Written as fmaf(a, b, -c), the compiler folds the negation into the multiply
// that produced c (c * k becomes c * -k) and needs a second multiply where c is also used
// unnegated (a2, a3 in idct8); the PTX neg keeps the negation on the fused operation, where
// ptxas makes it an operand modifier. The value is the same either way.
HD inline float fmaf_nc(float a, float b, float c) {
#ifdef __CUDA_ARCH__
  float r;
  asm("{\n\t.reg .f32 n;\n\tneg.f32 n, %3;\n\tfma.rn.f32 %0, %1, %2, n;\n\t}" : "=f"(r) : "f"(a), "f"(b), "f"(c));
  return r;
#else
  return fmaf(a, b, -c);
#endif
}
#define DC1 0.98078528040323044913f
#define DC2 0.92387953251128675613f
#define DC3 0.83146961230254523708f
#define DC5 0.55557023301960222474f
#define DC6 0.38268343236508977173f
#define DC7 0.19509032201612826785f
#define DC4H 0.35355339059327376220f

HD inline void fdct8(const float *in, float *out) {
  float s07 = in[0] + in[7], d07 = in[0] - in[7], s16 = in[1] + in[6], d16 = in[1] - in[6];
  float s25 = in[2] + in[5], d25 = in[2] - in[5], s34 = in[3] + in[4], d34 = in[3] - in[4];
  float e0 = s07 + s34, e1 = s16 + s25, e2 = s16 - s25, e3 = s07 - s34;
  out[0] = (e0 + e1) * DC4H;
  out[4] = (e0 - e1) * DC4H;
  out[2] = 0.5f * fmaf(DC6, e2, DC2 * e3);                                   // DC2*e3 + DC6*e2
  out[6] = 0.5f * fmaf_nc(DC6, e3, DC2 * e2);                                // DC6*e3 - DC2*e2
  out[1] = 0.5f * fmaf(DC7, d34, fmaf(DC5, d25, fmaf(DC1, d07, DC3 * d16))); // DC1*d07 + DC3*d16 + DC5*d25 + DC7*d34
  out[3] = 0.5f * fmaf(-DC5, d34, fmaf(-DC1, d25, fmaf_nc(DC3, d07, DC7 * d16)));   // DC3*d07 - DC7*d16 - DC1*d25 - DC5*d34
  out[5] = 0.5f * fmaf(DC3, d34, fmaf(DC7, d25, fmaf_nc(DC5, d07, DC1 * d16)));     // DC5*d07 - DC1*d16 + DC7*d25 + DC3*d34
  out[7] = 0.5f * fmaf(-DC1, d34, fmaf(DC3, d25, fmaf_nc(DC7, d07, DC5 * d16)));    // DC7*d07 - DC5*d16 + DC3*d25 - DC1*d34
}

HD inline void idct8(const float *F, float *out) {
  // a0 = (F0 + F4) * DC4H, a1 = (F0 - F4) * DC4H; e0 = a0 + a3, e1 = a1 + a2, e2 = a1 - a2, e3 = a0 - a3
  float s04 = F[0] + F[4], d04 = F[0] - F[4];
  float a2 = 0.5f * fmaf_nc(DC6, F[2], DC2 * F[6]);   // 0.5 (DC6*F2 - DC2*F6)
  float a3 = 0.5f * fmaf(DC6, F[6], DC2 * F[2]);      // 0.5 (DC2*F2 + DC6*F6)
  float e0 = fmaf(s04, DC4H, a3), e1 = fmaf(d04, DC4H, a2), e2 = fmaf_nc(d04, DC4H, a2), e3 = fmaf_nc(s04, DC4H, a3);
  // o0..o3 = 0.5 p0..p3
  float p0 = fmaf(DC7, F[7], fmaf(DC5, F[5], fmaf(DC1, F[1], DC3 * F[3])));        // DC1*F1 + DC3*F3 + DC5*F5 + DC7*F7
  float p1 = fmaf(-DC5, F[7], fmaf(-DC1, F[5], fmaf_nc(DC3, F[1], DC7 * F[3])));   // DC3*F1 - DC7*F3 - DC1*F5 - DC5*F7
  float p2 = fmaf(DC3, F[7], fmaf(DC7, F[5], fmaf_nc(DC5, F[1], DC1 * F[3])));     // DC5*F1 - DC1*F3 + DC7*F5 + DC3*F7
  float p3 = fmaf(-DC1, F[7], fmaf(DC3, F[5], fmaf_nc(DC7, F[1], DC5 * F[3])));    // DC7*F1 - DC5*F3 + DC3*F5 - DC1*F7
  out[0] = fmaf(0.5f, p0, e0); out[7] = fmaf(-0.5f, p0, e0); out[1] = fmaf(0.5f, p1, e1); out[6] = fmaf(-0.5f, p1, e1);
  out[2] = fmaf(0.5f, p2, e2); out[5] = fmaf(-0.5f, p2, e2); out[3] = fmaf(0.5f, p3, e3); out[4] = fmaf(-0.5f, p3, e3);
}

// Normalized macroblock activity (Test Model 5 step 3 with s = 2): 1/s .. s, 1 for an average macroblock.
HD inline float n_act(int act, int avg, float s) { return fmaf(s, (float)act, (float)avg) / fmaf(s, (float)avg, (float)act); }

#ifdef __CUDACC__
// Block indexing used by the transform kernels and by the coefficient/level/nz/pred buffers:
//   mbi = session * nmb + macroblock          (physical macroblock index, nmbtot = n * nmb of them)
//   blk = b * nmbtot + mbi                    (b = 0..5: four luma blocks, Cb, Cr)
//   coef, lev[lev_at(blk) + LEV_IDX(k)]       int16: the k-th stored value of the block in coded
//                                             order (which positions: cmask, nzmask); common.h
//   pred[(blk * 8 + row) * 8 + column]        uint8
//   hist[blk * 2 + i]                         uint64, eight 8-bit counters each (NZ_BINS = 16 bins)
// The transform kernels use 8 threads per 8x8 block and 32 blocks per CUDA block; the two
// passes of the separable transform exchange data through shared memory. Threads enumerate
// the blocks of the listed sessions only: t = b * (na * nmb) + k * nmb + macroblock.
// XB (8x8 blocks per CUDA block) is in tb_kernel_abi.h.

struct BlkGeo { int b, mbi, s, pw, ph, x0, y0; size_t blk; bool valid; };

// Picture line of row j of a block. Field DCT (luma only, 6.1.3 / Figure 6-13): blocks 0,1 hold
// the top-field lines of the macroblock and blocks 2,3 the bottom-field lines.
__device__ inline int blk_line(const BlkGeo &bg, int dct_type, int j) {
  if (bg.b < 4 && dct_type) return bg.y0 - (bg.b >> 1) * 8 + 2 * j + (bg.b >> 1);
  return bg.y0 + j;
}
// pairs: the luma blocks are enumerated two by two (blocks 0 and 1 of a macroblock, then of the
// next; after all of those, blocks 2 and 3), so that neighbors in t are neighbors in the picture
// row: the kernel with eight threads per block then reads 32 contiguous pixels per row and warp
// instead of four pieces 16 pixels apart.
__device__ inline BlkGeo blk_geo(const Geo &g, unsigned t, const int *list, int na, bool pairs = false) {
  BlkGeo r;
  unsigned nact = (unsigned)(na * g.nmb);   // 32-bit index arithmetic: 64-bit division is slow
  r.valid = t < nact * 6;
  if (!r.valid) { r.b = r.mbi = r.s = r.pw = r.ph = r.x0 = r.y0 = 0; r.blk = 0; return r; }
  int rem;
  if (pairs && t < nact * 4) {
    unsigned idx = t % (nact * 2);
    r.b = (int)(t / (nact * 2)) * 2 + (int)(idx & 1);
    rem = (int)(idx >> 1);
  } else {
    r.b = (int)(t / nact);
    rem = (int)(t % nact);
  }
  r.s = list[rem / g.nmb];
  int mb = rem % g.nmb, mbx = mb % g.mbw, mby = mb / g.mbw;
  r.mbi = r.s * g.nmb + mb;
  r.blk = (size_t)r.b * g.n * g.nmb + r.mbi;
  if (r.b < 4) { r.pw = g.w; r.ph = g.h; r.x0 = mbx * 16 + (r.b & 1) * 8; r.y0 = mby * 16 + (r.b >> 1) * 8; }
  else { r.pw = g.w >> 1; r.ph = g.h >> 1; r.x0 = mbx * 8; r.y0 = mby * 8; }
  return r;
}

// One row (8 pixels, two packed words) of a half-sample motion-compensated prediction (H.262
// 7.6.4), exact. ra, rb: the two source rows (rb is read only when fy is set), x: first pixel,
// fx, fy: half-sample flags. The vectors of the search never leave the picture, so x needs no clamp.
__device__ inline void mc_row8(const uint8_t *ra, const uint8_t *rb, int x, int fx, int fy, uint32_t *o) {
  int a = x & ~3, s8 = (x - a) * 8;
  const uint32_t *wp = (const uint32_t *)(ra + a);
  uint32_t W0 = wp[0], W1 = wp[1], W2 = wp[2];
  uint32_t A0 = __funnelshift_r(W0, W1, s8), A1 = __funnelshift_r(W1, W2, s8), A2 = W2 >> s8;   // pixels x .. x+7, x+8
  if (!fx && !fy) { o[0] = A0; o[1] = A1; return; }
  uint32_t B0 = __funnelshift_r(A0, A1, 8), B1 = __funnelshift_r(A1, A2, 8);                      // pixels x+1 .. x+8
  if (!fy) { o[0] = __vavgu4(A0, B0); o[1] = __vavgu4(A1, B1); return; }
  wp = (const uint32_t *)(rb + a);
  uint32_t V0 = wp[0], V1 = wp[1], V2 = wp[2];
  uint32_t C0 = __funnelshift_r(V0, V1, s8), C1 = __funnelshift_r(V1, V2, s8), C2 = V2 >> s8;
  if (!fx) { o[0] = __vavgu4(A0, C0); o[1] = __vavgu4(A1, C1); return; }
  o[0] = avg4x(A0, B0, C0, __funnelshift_r(C0, C1, 8));
  o[1] = avg4x(A1, B1, C1, __funnelshift_r(C1, C2, 8));
}

// The quantiser_scale_code of one macroblock: the row's code, modulated by the activity factor.
__device__ inline int mb_qcode(const Geo &g, const Tables *T, const SessPar &sp, int rowcode, int act) {
  if (!sp.aq || rowcode >= 31) return rowcode;   // at the top of the scale modulation could only make it finer
  int q = (int)fmaf((float)T->qscale[rowcode], n_act(act, sp.avg_act, g.aq_s), 0.5f);
  return T->qcode_near[iclamp(q, 1, 112)];
}

// Prediction + residual + forward DCT. Stores the coefficients (x4, int16, coded order; the
// DC of an intra block is stored as its 8-bit level) that are not zero at the picture's floor
// quantizer (SessPar::qfloor) and a mask of their positions (cmask, one bit per coded position),
// the prediction when the picture will be reconstructed, and per block a histogram of the quantiser_scale at which each coefficient
// becomes zero (normalized by the macroblock's activity factor when adaptive quantization is on),
// which is what the rate control needs to choose quantizers before any entropy coding.
__global__ void k_xf(Geo g, const Tables *T, Bufs B, const SessPar *par, const int *list, int na,
                     const MbInfo *info, const uint16_t *act, int16_t *coef, uint8_t *predbuf, uint64_t *hist, uint64_t *cmask) {
  __shared__ float sm[XB][8][8];
  int lb = threadIdx.x >> 3, j = threadIdx.x & 7;
  BlkGeo bg = blk_geo(g, blockIdx.x * XB + lb, list, na, true);
  int mode = MODE_INTRA;
  SessPar sp;
  if (bg.valid) {
    sp = par[bg.s];
    MbInfo mi = info[bg.mbi];
    mode = mi.mode;
    int line = blk_line(bg, mi.dct_type, j);
    size_t psz = (size_t)bg.pw * bg.ph;
    const uint8_t *cp = (bg.b < 4 ? B.ring_y : bg.b == 4 ? B.ring_u : B.ring_v) + ((size_t)sp.cur * g.n + bg.s) * B.ring_stride;
    uint32_t pr[2] = {0, 0};
    if (mode != MODE_INTRA) {
      bool have = false;
      for (int dir = 0; dir < 2; dir++) {
        if (!(mode & (dir ? MODE_BWD : MODE_FWD))) continue;
        const uint8_t *ref = (bg.b < 4 ? B.ref_y : bg.b == 4 ? B.ref_u : B.ref_v) +
                             ((size_t)(dir ? sp.bwd : sp.fwd) * g.n + bg.s) * psz;
        int r = mi.field_pred ? (line & 1) : 0;   // the field this line belongs to selects the vector
        int mvx = mi.mv[r][dir][0], mvy = mi.mv[r][dir][1];
        if (bg.b >= 4) { mvx /= 2; mvy /= 2; }    // 7.6.3.7: truncation toward zero, frame and field vectors alike
        int ya, yb;
        if (mi.field_pred) {
          // 7.6.4 with field-based prediction in a frame picture: the line is predicted from the
          // reference field chosen by motion_vertical_field_select, vertical vector in field lines.
          int sel = (mi.fsel >> (2 * dir + r)) & 1, hf = bg.ph >> 1;
          int fl = (line >> 1) + (mvy >> 1);
          ya = 2 * iclamp(fl, 0, hf - 1) + sel;
          yb = 2 * iclamp(fl + (mvy & 1), 0, hf - 1) + sel;
        } else {
          ya = iclamp(line + (mvy >> 1), 0, bg.ph - 1);
          yb = iclamp(line + (mvy >> 1) + (mvy & 1), 0, bg.ph - 1);
        }
        uint32_t pa[2];
        mc_row8(ref + (size_t)ya * bg.pw, ref + (size_t)yb * bg.pw, bg.x0 + (mvx >> 1), mvx & 1, mvy & 1, pa);
        if (have) { pr[0] = __vavgu4(pr[0], pa[0]); pr[1] = __vavgu4(pr[1], pa[1]); }
        else { pr[0] = pa[0]; pr[1] = pa[1]; }
        have = true;
      }
    }
    const uint32_t *c = (const uint32_t *)(cp + (size_t)line * bg.pw + bg.x0);
    uint32_t cw[2] = {c[0], c[1]};
    float f[8], o[8];
#pragma unroll
    for (int i = 0; i < 8; i++) f[i] = (float)((int)((cw[i >> 2] >> (8 * (i & 3))) & 255) - (int)((pr[i >> 2] >> (8 * (i & 3))) & 255));
    if (sp.want_pred) {
      uint32_t *pd = (uint32_t *)(predbuf + (bg.blk * 8 + j) * 8);
      pd[0] = pr[0]; pd[1] = pr[1];
    }
    fdct8(f, o);
    for (int u = 0; u < 8; u++) sm[lb][j][u] = o[u];
  }
  __syncthreads();
  int lv[8] = {0, 0, 0, 0, 0, 0, 0, 0}, kb = 0;   // this thread's eight coefficients; bit v: coefficient v is stored
  uint32_t ppos[2] = {0, 0};                      // their coded positions, eight bits each
  uint32_t red[6] = {0, 0, 0, 0, 0, 0};   // to be combined over the block: mask (low, high), histogram (2 x low, high)
  const uint8_t *spos = T->scan_pos[bg.valid ? pic_alt_scan(g, sp) : 0];
  if (bg.valid) {
    int u = j;
    float t[8], F[8];
    for (int v = 0; v < 8; v++) t[v] = sm[lb][v][u];
    fdct8(t, F);
    int a_mb = act[bg.mbi];
    float inv_n = sp.aq ? 1.0f / n_act(a_mb, sp.avg_act, g.aq_s) : 1.0f;
    // the coarsest quantizer at which a coefficient must still be kept: the floor of the picture, for this macroblock
    float qf = (float)T->qscale[mb_qcode(g, T, sp, sp.qfloor, a_mb)];
    float invf = 0.25f / qf;
    // non-intra: (int)(|l| * invf) != 0 exactly when |l| >= thr
    int thr = 4 * (int)qf;
    if ((int)((float)thr * invf) == 0) thr++;
    uint64_t h0 = 0, h1 = 0;
    bool intra = mode == MODE_INTRA;
    for (int v = 0; v < 8; v++) {
      int r = v * 8 + u, l, keepit;
      float m;
      if (intra && r == 0) {
        l = iclamp((int)floorf(fmaf(F[0], 0.125f, 0.5f)), 0, 255);  // intra_dc_precision 8 bit
        m = 0;
        keepit = 1;
      } else {
        l = iclamp((int)floorf(fmaf(F[v], 4.0f, 0.5f)), -32768, 32767);
        // the level is non-zero while quantiser_scale <= m (intra: |F|*16/(W*q) + 0.375 >= 1)
        m = intra ? fabsf(F[v]) * (16.0f / 0.625f) / (float)T->intra_q[r] * inv_n : fabsf(F[v]) * inv_n;
        // the same expressions as in k_quant, at the floor quantizer: zero here means zero there
        keepit = intra ? (int)((float)iabs(l) * 4.0f / ((float)T->intra_q[r] * qf) + 0.375f) != 0 : iabs(l) >= thr;
      }
      lv[v] = l;
      if (keepit) {
        int p = spos[r];
        kb |= 1 << v;
        ppos[v >> 2] |= (uint32_t)p << (8 * (v & 3));
        if (p < 32) red[0] |= 1u << p; else red[1] |= 1u << (p - 32);
      }
      if (m >= 1.0f) {
        int e = (int)(__float_as_uint(m) >> 22) - 254;   // half-octave bins: 1, 1.5, 2, 3, 4, 6, ...
        if (e > NZ_BINS - 1) e = NZ_BINS - 1;
        if (e < 8) h0 += 1ull << (8 * e); else h1 += 1ull << (8 * (e - 8));
      }
    }
    red[2] = (uint32_t)h0; red[3] = (uint32_t)(h0 >> 32); red[4] = (uint32_t)h1; red[5] = (uint32_t)(h1 >> 32);
  }
  // Combine over the eight threads of the block, which are eight neighboring lanes of one warp
  // (the 8-bit histogram counters cannot carry: at most 64 coefficients per block).
#pragma unroll
  for (int d = 1; d < 8; d <<= 1) {
    red[0] |= __shfl_xor_sync(0xffffffffu, red[0], d); red[1] |= __shfl_xor_sync(0xffffffffu, red[1], d);
#pragma unroll
    for (int i = 2; i < 6; i++) red[i] += __shfl_xor_sync(0xffffffffu, red[i], d);
  }
  if (bg.valid) {
    // dense: a coefficient goes to its rank among the block's stored positions (red[0], red[1]:
    // the block's mask, known to all eight threads after the reduction)
    int16_t *lp = coef + lev_at(bg.blk);
    int nlo = __popc(red[0]);
    for (int v = 0; v < 8; v++)
      if ((kb >> v) & 1) {
        int p = (int)((ppos[v >> 2] >> (8 * (v & 3))) & 255);
        int rk = p < 32 ? __popc(red[0] & ((1u << p) - 1)) : nlo + __popc(red[1] & ((1u << (p - 32)) - 1));
        lp[LEV_IDX(rk)] = (int16_t)lv[v];
      }
    if (j == 0) {
      cmask[bg.blk] = ((uint64_t)red[1] << 32) | red[0];
      hist[bg.blk * 2] = ((uint64_t)red[3] << 32) | red[2]; hist[bg.blk * 2 + 1] = ((uint64_t)red[5] << 32) | red[4];
    }
  }
}

// Row statistics for the rate control. One CUDA block per (listed session, macroblock row), one
// thread per macroblock (block size: the row length rounded up to whole warps, at most 256):
// each thread adds up the histograms of its six blocks, the warps reduce with shuffles and the
// first thread adds the warps. (One thread per row walking 120 macroblocks took as long for 4
// sessions as for 16, and every batch of a mixed line-up paid it.)
#define ROW_VALS (NZ_BINS + 3)
__global__ void k_rowstat(Geo g, const int *list, int na, const uint64_t *hist, const uint16_t *act, const MbInfo *info,
                          RowStat *out) {
  __shared__ uint32_t sh[8][ROW_VALS];
  int row = blockIdx.x % g.mbh, s = list[blockIdx.x / g.mbh], x = threadIdx.x;
  uint32_t v[ROW_VALS];
#pragma unroll
  for (int i = 0; i < ROW_VALS; i++) v[i] = 0;
  if (x < g.mbw) {
    size_t nmbtot = (size_t)g.n * g.nmb, mbi = (size_t)s * g.nmb + (size_t)row * g.mbw + x;
#pragma unroll
    for (int b = 0; b < 6; b++) {
      uint64_t h0 = hist[((size_t)b * nmbtot + mbi) * 2], h1 = hist[((size_t)b * nmbtot + mbi) * 2 + 1];
#pragma unroll
      for (int i = 0; i < 8; i++) { v[i] += (uint32_t)(h0 >> (8 * i)) & 255; v[8 + i] += (uint32_t)(h1 >> (8 * i)) & 255; }
    }
    MbInfo mi = info[mbi];
    v[NZ_BINS] = act[mbi]; v[NZ_BINS + 1] = mi.mode == MODE_INTRA; v[NZ_BINS + 2] = mi.field_pred;
  }
#pragma unroll
  for (int i = 0; i < ROW_VALS; i++) {
    uint32_t t = v[i];
#pragma unroll
    for (int d = 16; d >= 1; d >>= 1) t += __shfl_xor_sync(0xffffffffu, t, d);
    v[i] = t;
  }
  if ((x & 31) == 0)
#pragma unroll
    for (int i = 0; i < ROW_VALS; i++) sh[x >> 5][i] = v[i];
  __syncthreads();
  if (x == 0) {
    int nw = blockDim.x >> 5;
    RowStat r;
#pragma unroll
    for (int i = 0; i < NZ_BINS; i++) { uint32_t a = 0; for (int w = 0; w < nw; w++) a += sh[w][i]; r.nz[i] = a; }
    uint32_t as = 0, ni = 0, nf = 0;
    for (int w = 0; w < nw; w++) { as += sh[w][NZ_BINS]; ni += sh[w][NZ_BINS + 1]; nf += sh[w][NZ_BINS + 2]; }
    r.act_sum = as; r.n_intra = ni; r.n_field = nf; r.pad = 0;
    out[(size_t)s * g.mbh + row] = r;
  }
}

// Quantization (the inverse of 7.4.2.3) of the stored coefficients into levels, one thread per
// block. rowq[session * mbh + row] is the row's base quantiser_scale_code; the session's `keep`
// limits how many coefficients of a block survive (the last steps of the buffer ladder).
// Only the coefficients that k_xf stored are read (cmask, in sequence): the others are zero at any
// quantizer the rate control may use for this picture. Only the non-zero levels are written, dense;
// nzmask has one bit per coded position that holds one (bit 0 of an intra block is always
// set: its DC level), nz says whether the block is coded.
__global__ void k_quant(Geo g, const Tables *T, const SessPar *par, const int *list, int na, const uint8_t *rowq,
                        const MbInfo *info, const uint16_t *act, const int16_t *coef, const uint64_t *cmask, int16_t *lev,
                        uint64_t *nzmask, uint8_t *nz, uint8_t *mbq) {
  unsigned t = blockIdx.x * blockDim.x + threadIdx.x;
  unsigned nact = (unsigned)(na * g.nmb);
  if (t >= nact * 6) return;
  int b = (int)(t / nact), rem = (int)(t % nact);
  int s = list[rem / g.nmb], mb = rem % g.nmb;
  size_t nmbtot = (size_t)g.n * g.nmb, mbi = (size_t)s * g.nmb + mb;
  SessPar sp = par[s];
  int code = mb_qcode(g, T, sp, rowq[(size_t)s * g.mbh + mb / g.mbw], act[mbi]);
  float q = (float)T->qscale[code];
  bool intra = info[mbi].mode == MODE_INTRA;
  const int16_t *cp = coef + lev_at((size_t)b * nmbtot + mbi);
  int16_t *lp = lev + lev_at((size_t)b * nmbtot + mbi);
  const uint8_t *ras = T->scan_ras[pic_alt_scan(g, sp)];
  uint64_t mask = 0, cm = cmask[(size_t)b * nmbtot + mbi];
  int keep = sp.keep, kc = 0, kl = 0;   // values read from cp, levels written to lp
  if (intra) {
    lp[0] = cp[0]; kc = kl = 1;
    mask = 1;
    cm &= ~1ull;
    while (cm) {
      int i = __ffsll((long long)cm) - 1;
      cm &= cm - 1;
      if (i >= keep) break;
      int c = cp[LEV_IDX(kc)]; kc++;
      int a = (int)((float)iabs(c) * 4.0f / ((float)T->intra_q[ras[i]] * q) + 0.375f);
      if (a > 2047) a = 2047;
      if (a) { lp[LEV_IDX(kl)] = (int16_t)(c < 0 ? -a : a); kl++; mask |= 1ull << i; }
    }
  } else {
    float inv = 0.25f / q;  // W = 16 (default non-intra matrix)
    while (cm) {
      int i = __ffsll((long long)cm) - 1;
      cm &= cm - 1;
      if (i >= keep) break;
      int c = cp[LEV_IDX(kc)]; kc++;
      int a = (int)((float)iabs(c) * inv);
      if (a > 2047) a = 2047;
      if (a) { lp[LEV_IDX(kl)] = (int16_t)(c < 0 ? -a : a); kl++; mask |= 1ull << i; }
    }
  }
  nzmask[(size_t)b * nmbtot + mbi] = mask;
  nz[(size_t)b * nmbtot + mbi] = mask != 0;
  if (b == 0) mbq[mbi] = (uint8_t)code;
}

// Inverse quantization (7.4.2.3), saturation (7.4.3), mismatch control (7.4.4), IDCT, add
// prediction, write the reconstructed block into out_* [slots][n][plane] at the slot selected
// by osel (SL_REC: the session's new reference; SL_ZERO: a one-slot dump buffer). One thread
// per block: only the non-zero levels are visited, the prediction is read and the result
// written as packed rows (two words per row), rows are transformed before columns.
__global__ void k_recon(Geo g, const Tables *T, const SessPar *par, const int *list, int na, const int16_t *lev,
                        const uint64_t *nzmask, const uint8_t *nz, const uint8_t *predbuf, const MbInfo *info, const uint8_t *mbq,
                        uint8_t *out_y, uint8_t *out_u, uint8_t *out_v, int osel) {
  BlkGeo bg = blk_geo(g, blockIdx.x * blockDim.x + threadIdx.x, list, na);
  if (!bg.valid) return;
  MbInfo mi = info[bg.mbi];
  bool intra = mi.mode == MODE_INTRA;
  bool coded = intra || nz[bg.blk];
  const uint32_t *pd = (const uint32_t *)(predbuf + bg.blk * 64);
  uint8_t *op = (bg.b < 4 ? out_y : bg.b == 4 ? out_u : out_v) +
                ((size_t)slot_of(par[bg.s], osel) * g.n + bg.s) * bg.pw * bg.ph + bg.x0;
  uint32_t rw[8][2];
#pragma unroll
  for (int y = 0; y < 8; y++) { rw[y][0] = pd[2 * y]; rw[y][1] = pd[2 * y + 1]; }
  if (coded) {
    float G[64];
    for (int i = 0; i < 64; i++) G[i] = 0.0f;
    int q = T->qscale[mbq[bg.mbi]];
    const uint8_t *ras = T->scan_ras[pic_alt_scan(g, par[bg.s])];
    const int16_t *lp = lev + lev_at(bg.blk);
    uint64_t mask = nzmask[bg.blk];
    int sum = 0, last = 0, k = 0;
    while (mask) {
      int p = __ffsll((long long)mask) - 1;
      mask &= mask - 1;
      int r = ras[p], l = lp[LEV_IDX(k)], val;
      k++;
      if (intra) val = r == 0 ? 8 * l : (2 * l * T->intra_q[r] * q) / 32;
      else val = l == 0 ? 0 : ((2 * l + (l > 0 ? 1 : -1)) * 16 * q) / 32;
      val = iclamp(val, -2048, 2047);
      sum += val;
      if (r == 63) last = val;
      G[r] = (float)val;
    }
    if ((sum & 1) == 0) G[63] = (float)(last ^ 1);   // mismatch control (7.4.4): toggle the LSB of F[7][7]
    for (int v = 0; v < 8; v++) {
      float f[8], o[8];
#pragma unroll
      for (int u = 0; u < 8; u++) f[u] = G[v * 8 + u];
      idct8(f, o);
#pragma unroll
      for (int u = 0; u < 8; u++) G[v * 8 + u] = o[u];
    }
#pragma unroll
    for (int x = 0; x < 8; x++) {
      float t[8], o[8];
#pragma unroll
      for (int v = 0; v < 8; v++) t[v] = G[v * 8 + x];
      idct8(t, o);
#pragma unroll
      for (int y = 0; y < 8; y++) {
        int r = iclamp((int)floorf(o[y] + 0.5f), -256, 255);
        int sh = 8 * (x & 3);
        uint32_t w = rw[y][x >> 2];
        uint32_t pix = (uint32_t)iclamp(r + (int)((w >> sh) & 255), 0, 255);
        rw[y][x >> 2] = (w & ~(255u << sh)) | (pix << sh);
      }
    }
  }
#pragma unroll
  for (int y = 0; y < 8; y++) {
    uint32_t *o = (uint32_t *)(op + (size_t)blk_line(bg, mi.dct_type, y) * bg.pw);
    o[0] = rw[y][0]; o[1] = rw[y][1];
  }
}

// --- entropy coding on the GPU ---------------------------------------------------------

// E1: one thread per macroblock. Resolves what the slice syntax makes depend on the macroblocks
// to the left (7.2.1, 7.6.6) without a serial pass over the row:
//   - DC predictors depend on the left neighbor only (its DC levels if it is intra, else the reset value);
//   - a P macroblock is skipped on its own data (7.6.6.2);
//   - a B macroblock is skipped when it repeats the left neighbor's mode and its vectors equal
//     the predictors PMV[0][s] (7.6.6.4). PMV[0][s] is the vector of the nearest macroblock to the
//     left that uses direction s, or zero if an intra macroblock or the slice start comes first:
//     a skipped macroblock leaves the predictors alone, but its vector equals PMV[0][s] anyway.
// The address increment, the quantizer to signal and the predictors of the coded vectors need
// the skipped flags of the neighbors and are resolved by k_ent_pack.
__global__ void k_ent_resolve(Geo g, const SessPar *par, const int *list, int na, const MbInfo *info,
                              const uint8_t *nz, const int16_t *lev, MbRes *code) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * g.nmb) return;
  int s = list[idx / g.nmb], mb = idx % g.nmb, x = mb % g.mbw, row = mb / g.mbw;
  size_t stride = (size_t)g.n * g.nmb, mbi = (size_t)s * g.nmb + mb;
  int pic_type = par[s].pic_type;
  MbInfo mi = info[mbi];
  MbRes c;
  c.mode = mi.mode; c.nomc = 0; c.skipped = 0; c.first = (x == 0);
  c.row = (uint8_t)row; c.dct_type = mi.dct_type;
  c.field_pred = mi.field_pred; c.fsel = mi.fsel; c.pad[0] = c.pad[1] = c.pad[2] = 0;
  for (int b = 0; b < 6; b++) c.dc_diff[b] = 0;
  bool intra = mi.mode == MODE_INTRA;
  int cbp = 0;
  if (intra) cbp = 63;
  else for (int b = 0; b < 6; b++) if (nz[(size_t)b * stride + mbi]) cbp |= 32 >> b;
  c.cbp = (uint8_t)cbp;
  bool edge = (x == 0 || x == g.mbw - 1);
  bool skip = false;
  if (!intra && cbp == 0 && !edge && !mi.field_pred) {
    if (pic_type == PIC_P) {
      skip = (mi.mv[0][0][0] == 0 && mi.mv[0][0][1] == 0);
    } else if (pic_type == PIC_B && info[mbi - 1].mode == mi.mode) {
      skip = true;
      int need = mi.mode;
      for (int y = x - 1; y >= 0 && need && skip; y--) {
        MbInfo l = info[mbi - (size_t)(x - y)];
        if (l.mode == MODE_INTRA) {   // predictors were reset to zero
          for (int d = 0; d < 2; d++) if ((need >> d) & 1) skip = skip && mi.mv[0][d][0] == 0 && mi.mv[0][d][1] == 0;
          need = 0;
          break;
        }
        for (int d = 0; d < 2; d++) {
          if (!((need >> d) & 1) || !((l.mode >> d) & 1)) continue;
          need &= ~(1 << d);
          int py = l.field_pred ? l.mv[0][d][1] * 2 : l.mv[0][d][1];   // 7.6.3.1
          skip = skip && mi.mv[0][d][0] == l.mv[0][d][0] && mi.mv[0][d][1] == py;
        }
      }
      if (need)   // reached the slice start: zero predictors
        for (int d = 0; d < 2; d++) if ((need >> d) & 1) skip = skip && mi.mv[0][d][0] == 0 && mi.mv[0][d][1] == 0;
    }
  }
  if (skip) {
    c.skipped = 1;
  } else if (intra) {
    // 7.2.1: the predictors are reset by any macroblock that is not intra (skipped ones included)
    int dcpred[3] = {128, 128, 128};
    if (x > 0 && info[mbi - 1].mode == MODE_INTRA)
      for (int cc = 0; cc < 3; cc++) dcpred[cc] = lev[lev_at((size_t)(3 + cc) * stride + mbi - 1)];
    for (int b = 0; b < 6; b++) {
      int cc = b < 4 ? 0 : b - 3;
      int dc = lev[lev_at((size_t)b * stride + mbi)];
      c.dc_diff[b] = (int16_t)(dc - dcpred[cc]);
      dcpred[cc] = dc;
    }
  } else if (pic_type == PIC_P && !mi.field_pred && mi.mv[0][0][0] == 0 && mi.mv[0][0][1] == 0 && cbp != 0) {
    c.nomc = 1;                                                         // "No MC, Coded"
  }
  code[mbi] = c;
}

static_assert(sizeof(MbRes) == 24, "MbRes is read as words");
// The bytes {skipped, cbp, mode, nomc} of a MbRes as one word.
__device__ inline uint32_t mbcode_flags(const MbRes *c) { return ((const uint32_t *)c)[3]; }

// E2: one thread per macroblock. Walks left over the flags k_ent_resolve left behind to find
//   - the address increment: the run of skipped macroblocks directly to the left;
//   - the quantizer to signal: the quantizer of the nearest macroblock that used one (the slice
//     header carries the first macroblock's) (7.4.2.2);
//   - the predictors of its vectors (7.6.3): per direction the vectors of the nearest macroblock
//     that is not skipped and uses that direction; zero after an intra macroblock, a skipped
//     macroblock of a P picture, or at the slice start (7.6.3.4);
// then packs the macroblock into its scratch area. The walk is one or two steps on average.
__global__ void k_ent_pack(Geo g, const Tables *T, const SessPar *par, const int *list, int na, const MbRes *code,
                           const MbInfo *info, const int16_t *lev, const uint64_t *nzmask, const uint8_t *mbq, uint32_t *scratch,
                           uint32_t *mbbits, uint16_t *mbtex) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * g.nmb) return;
  int s = list[idx / g.nmb], x = (idx % g.nmb) % g.mbw;
  size_t mbi = (size_t)s * g.nmb + idx % g.nmb;
  MbRes res = code[mbi];
  int tex = 0, bits = 0;
  if (!res.skipped) {
    MbCode c;
    for (int b = 0; b < 6; b++) c.dc_diff[b] = res.dc_diff[b];
    for (int i = 0; i < 8; i++) c.pmv[i >> 2][(i >> 1) & 1][i & 1] = 0;
    c.skipped = 0; c.cbp = res.cbp; c.mode = res.mode; c.nomc = res.nomc; c.first = res.first; c.row = res.row;
    c.dct_type = res.dct_type; c.field_pred = res.field_pred; c.fsel = res.fsel;
    int pic_type = par[s].pic_type;
    MbInfo mi = info[mbi];
    int inc = 1, prev_q = -1;
    bool run = true;
    int need = (c.mode == MODE_INTRA || c.nomc) ? 0 : c.mode;
    for (int y = x - 1; y >= 0 && (run || prev_q < 0 || need); y--) {
      size_t li = mbi - (size_t)(x - y);
      uint32_t f = mbcode_flags(code + li);
      bool sk = f & 0xff;
      int lcbp = (f >> 8) & 0xff, lmode = (f >> 16) & 0xff;
      if (run && sk) inc++; else run = false;
      if (prev_q < 0 && !sk && (lcbp != 0 || lmode == MODE_INTRA)) prev_q = mbq[li];
      if (need) {
        if (lmode == MODE_INTRA || (sk && pic_type == PIC_P)) need = 0;
        else if (!sk && (need & lmode)) {
          MbInfo l = info[li];
          for (int d = 0; d < 2; d++) {
            if (!((need & lmode) >> d & 1)) continue;
            if (mi.field_pred) {
              // 7.6.3.1: field vectors in a frame picture are predicted from PMV[r][s], vertical DIV 2
              // (a field macroblock leaves twice its vertical component in the predictor)
              for (int r = 0; r < 2; r++) {
                int lr = l.field_pred ? r : 0;
                c.pmv[r][d][0] = l.mv[lr][d][0];
                c.pmv[r][d][1] = (int16_t)(l.field_pred ? l.mv[lr][d][1] : l.mv[0][d][1] >> 1);
              }
            } else {
              c.pmv[0][d][0] = l.mv[0][d][0];
              c.pmv[0][d][1] = (int16_t)(l.field_pred ? l.mv[0][d][1] * 2 : l.mv[0][d][1]);
            }
          }
          need &= ~lmode;
        }
      }
    }
    if (prev_q < 0) prev_q = mbq[mbi - x];
    bool uses = c.mode == MODE_INTRA || c.cbp != 0;
    c.addr_inc = (uint8_t)inc;
    c.quant = uses && mbq[mbi] != prev_q;
    c.qcode = (uint8_t)(uses ? mbq[mbi] : prev_q);
    bits = encode_mb(T, g, pic_type, pic_field_tools(g, par[s]), c, mi, lev, mbi, nzmask + mbi, (size_t)g.n * g.nmb, scratch + mbi * MB_SCRATCH_WORDS, &tex);
  }
  // H.262 8.2 (Table 8-6, p.110): a 4:2:0 macroblock takes at most 4608 bits, counted from the
  // first bit of macroblock_address_increment (the slice header in front of the first macroblock
  // of a row is not part of it); at most two macroblocks of a row may take more. Bit 15 of mbtex
  // marks a macroblock above the limit (the texture bits of one macroblock stay below 2^15:
  // the scratch area holds MB_SCRATCH_WORDS * 32 bits).
  uint32_t over = (uint32_t)(bits - (res.first ? SLICE_HEADER_BITS : 0)) > (uint32_t)MB_MAX_BITS && bits > 0;
  mbbits[mbi] = (uint32_t)bits;
  mbtex[mbi] = (uint16_t)((uint32_t)tex | (over << 15));
}

// E3a: one CUDA block per slice (listed session, macroblock row), one thread per macroblock, as
// in k_rowstat. Exclusive prefix sum of the bit counts along the row (a shuffle scan inside each
// warp, the warp totals through shared memory); slice size in bytes (slices are byte aligned,
// zero stuffed); block-layer bits of the slice.
__global__ void k_ent_rowsum(Geo g, const int *list, int na, const uint32_t *mbbits, const uint16_t *mbtex,
                             uint32_t *mboff, uint32_t *slice_bytes, uint32_t *slice_tex) {
  __shared__ uint32_t wsum[8], wtex[8];
  int row = blockIdx.x % g.mbh, s = list[blockIdx.x / g.mbh], x = threadIdx.x, lane = x & 31, warp = x >> 5;
  size_t m0 = (size_t)s * g.nmb + (size_t)row * g.mbw;
  uint32_t v = 0, tx = 0;
  // tx: the texture bits in the low 16 bits, the count of macroblocks above the Table 8-6 limit
  // (bit 15 of mbtex) in the high 16; both sums stay below 2^16 per warp (32 macroblocks of at
  // most MB_SCRATCH_WORDS * 32 bits).
  if (x < g.mbw) { v = mbbits[m0 + x]; uint32_t t = mbtex[m0 + x]; tx = (t & 0x7fffu) | ((t >> 15) << 24); }
  uint32_t inc = v;   // inclusive prefix within the warp
#pragma unroll
  for (int d = 1; d < 32; d <<= 1) {
    uint32_t t = __shfl_up_sync(0xffffffffu, inc, d);
    if (lane >= d) inc += t;
  }
#pragma unroll
  for (int d = 16; d >= 1; d >>= 1) tx += __shfl_xor_sync(0xffffffffu, tx, d);
  if (lane == 31) wsum[warp] = inc;
  if (lane == 0) wtex[warp] = tx;
  __syncthreads();
  uint32_t base = 0;
  for (int w = 0; w < warp; w++) base += wsum[w];
  if (x < g.mbw) mboff[m0 + x] = base + inc - v;
  if (x == 0) {
    uint32_t acc = 0, tex = 0, big = 0;
    int nw = blockDim.x >> 5;
    for (int w = 0; w < nw; w++) { acc += wsum[w]; tex += wtex[w] & 0xffffffu; big += wtex[w] >> 24; }
    slice_bytes[(size_t)s * g.mbh + row] = (acc + 7) >> 3;
    // bit 31: more than two macroblocks of this row are above the limit of Table 8-6, which the
    // stream may not contain (the host quantizes the row again, coarser)
    slice_tex[(size_t)s * g.mbh + row] = tex | (big > (uint32_t)MB_MAX_OVER ? 0x80000000u : 0u);
  }
}

// E3b: one thread per session. Prefix sum over slices; picture size in bytes.
__global__ void k_ent_picsum(Geo g, const int *list, int na, const uint32_t *slice_bytes, const uint32_t *slice_tex,
                             uint32_t *slice_off, PicStat *pic) {
  int k = blockIdx.x * blockDim.x + threadIdx.x;
  if (k >= na) return;
  int s = list[k];
  size_t r0 = (size_t)s * g.mbh;
  uint32_t acc = 0, tex = 0;
  uint32_t big[PIC_ROW_WORDS];
  for (int i = 0; i < PIC_ROW_WORDS; i++) big[i] = 0;
  for (int r = 0; r < g.mbh; r++) {
    uint32_t t = slice_tex[r0 + r];
    slice_off[r0 + r] = acc; acc += slice_bytes[r0 + r]; tex += t & 0x7fffffffu;
    // rows beyond the mask (no level has that many: 1088 lines are 68 rows) share its last bit
    int b = r < PIC_ROW_WORDS * 32 ? r : PIC_ROW_WORDS * 32 - 1;
    big[b >> 5] |= (t >> 31) << (b & 31);
  }
  pic[s].bytes = acc; pic[s].tex_bits = tex;
  for (int i = 0; i < PIC_ROW_WORDS; i++) pic[s].big_rows[i] = big[i];
}

// E4: one thread per macroblock. ORs the scratch bits into the output buffer. The pictures of
// all sessions are packed one after the other: outpos[2 * s] is the first word of session s and
// outpos[2 * s + 1] the number of words reserved for it. Words are byte-swapped, i.e. the bytes
// are already in stream order. Bits of neighboring macroblocks share words, hence the atomics.
// The reserved words must be zero.
__global__ void k_ent_scatter(Geo g, const int *list, int na, const MbRes *code, const uint32_t *scratch,
                              const uint32_t *mbbits, const uint32_t *mboff, const uint32_t *slice_off, uint32_t *out,
                              const uint32_t *outpos) {
  int idx = blockIdx.x * blockDim.x + threadIdx.x;
  if (idx >= na * g.nmb) return;
  int s = list[idx / g.nmb], mb = idx % g.nmb;
  size_t mbi = (size_t)s * g.nmb + mb;
  if (code[mbi].skipped) return;
  int row = mb / g.mbw;
  uint32_t nbits = mbbits[mbi];
  uint64_t off = (uint64_t)slice_off[(size_t)s * g.mbh + row] * 8 + mboff[mbi];
  const uint32_t *w = scratch + mbi * MB_SCRATCH_WORDS;
  uint32_t *o = out + outpos[2 * s];
  uint32_t wi = (uint32_t)(off >> 5);
  int sh = (int)(off & 31);
  int nw = (int)((nbits + 31) >> 5);
  if (wi + nw + 1 > outpos[2 * s + 1]) return;  // cannot happen: the host reserves the picture's size plus two words
  for (int i = 0; i < nw; i++) {
    uint32_t v = w[i];
    atomicOr(o + wi + i, bswap32(v >> sh));
    if (sh) atomicOr(o + wi + i + 1, bswap32(v << (32 - sh)));
  }
}
#endif  // __CUDACC__
