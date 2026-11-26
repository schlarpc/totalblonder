// The conversion, composition and color kernels and their device helpers. Part of the device
// image and of the host reference library (kernels/tb_kernels.cu, kernels/tb_kernels_ref.cc) and
// formerly also included by the C++ host. Their records (Taps, ConvClass, ConvJob, RectJob,
// CscPar, PreJob) are defined in crates/tb-kernel-abi (tb_kernel_abi.h). Integer arithmetic only.
#pragma once
#include <stddef.h>
#include <stdint.h>
#include <cuda_runtime.h>

#include "tb_kernel_abi.h"

__device__ inline uint32_t pp_ld4(const uint8_t *row, int o) {   // four bytes at byte offset o of a 4-byte aligned row
  const uint32_t *w = (const uint32_t *)(row + (o & ~3));
  int sh = (o & 3) * 8;
  uint32_t lo = w[0];
  return sh ? (lo >> sh) | (w[1] << (32 - sh)) : lo;
}
__device__ inline uint64_t pp_ld8(const uint8_t *row, int o) {   // eight bytes at an even byte offset
  const uint32_t *w = (const uint32_t *)(row + (o & ~3));
  uint64_t a = w[0] | ((uint64_t)w[1] << 32);
  return (o & 2) ? (a >> 16) | ((uint64_t)w[2] << 48) : a;
}
__device__ inline uint32_t pp_clamp8(int v) { return v < 0 ? 0u : v > 255 ? 255u : (uint32_t)v; }

// Horizontal pass: source rows of the lattice -> intermediate planes (8-bit; Y, then U, then V).
__global__ void k_conv_h(ConvClass c, const ConvJob *jobs, int nj, uint8_t *tmp) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wy = c.dw / 4, wc = c.dw / 8;
  size_t ny = (size_t)wy * c.th_y, nc = (size_t)wc * c.th_c, per = ny + nc;
  if (idx >= per * nj) return;
  int j = (int)(idx / per);
  size_t r = idx % per;
  ConvJob J = jobs[j];
  uint8_t *t = tmp + (size_t)j * c.tstride;
  if (r < ny) {
    int row = (int)(r / wy), x0 = 4 * (int)(r % wy);
    const uint8_t *src = J.sy + (size_t)(c.s_off + c.s_step * row) * J.sp_y;
    const int nt = c.hy.nt;
    uint32_t out = 0;
    for (int o = 0; o < 4; o++) {
      int x = x0 + o, s = c.hy.start[x];
      const int16_t *cf = c.hy.coef + (size_t)x * nt;
      int acc = 0;
      if (c.bps == 1) {
        for (int k = 0; k < nt; k += 4) {
          uint64_t cw = *(const uint64_t *)(cf + k);
          uint32_t p = pp_ld4(src, s + k);
          acc += (int)(p & 255) * (int16_t)cw + (int)((p >> 8) & 255) * (int16_t)(cw >> 16) +
                 (int)((p >> 16) & 255) * (int16_t)(cw >> 32) + (int)(p >> 24) * (int16_t)(cw >> 48);
        }
        out |= pp_clamp8((acc + 8192) >> 14) << (8 * o);
      } else {
        for (int k = 0; k < nt; k += 4) {
          uint64_t cw = *(const uint64_t *)(cf + k);
          uint64_t p = pp_ld8(src, 2 * (s + k));
          acc += (int)((p & 0xffff) >> 2) * (int16_t)cw + (int)(((p >> 16) & 0xffff) >> 2) * (int16_t)(cw >> 16) +
                 (int)(((p >> 32) & 0xffff) >> 2) * (int16_t)(cw >> 32) + (int)((p >> 48) >> 2) * (int16_t)(cw >> 48);
        }
        out |= pp_clamp8((acc + (1 << 19)) >> 20) << (8 * o);
      }
    }
    *(uint32_t *)(t + (size_t)row * c.dw + x0) = out;
  } else {
    r -= ny;
    int row = (int)(r / wc), x0 = 4 * (int)(r % wc);
    const uint8_t *src = J.suv + (size_t)(c.s_off + c.s_step * row) * J.sp_uv;
    const int nt = c.hc.nt;
    uint32_t ou = 0, ov = 0;
    for (int o = 0; o < 4; o++) {
      int x = x0 + o, s = c.hc.start[x];
      const int16_t *cf = c.hc.coef + (size_t)x * nt;
      int au = 0, av = 0;
      if (c.bps == 1) {
        for (int k = 0; k < nt; k += 4) {
          uint64_t cw = *(const uint64_t *)(cf + k);
          uint64_t p = pp_ld8(src, 2 * (s + k));
          int c0 = (int16_t)cw, c1 = (int16_t)(cw >> 16), c2 = (int16_t)(cw >> 32), c3 = (int16_t)(cw >> 48);
          au += (int)(p & 255) * c0 + (int)((p >> 16) & 255) * c1 + (int)((p >> 32) & 255) * c2 + (int)((p >> 48) & 255) * c3;
          av += (int)((p >> 8) & 255) * c0 + (int)((p >> 24) & 255) * c1 + (int)((p >> 40) & 255) * c2 + (int)(p >> 56) * c3;
        }
        ou |= pp_clamp8((au + 8192) >> 14) << (8 * o); ov |= pp_clamp8((av + 8192) >> 14) << (8 * o);
      } else {
        const uint32_t *w = (const uint32_t *)src + s;
        for (int k = 0; k < nt; k++) {
          uint32_t p = w[k]; int ck = cf[k];
          au += (int)((p & 0xffff) >> 2) * ck; av += (int)(p >> 18) * ck;
        }
        ou |= pp_clamp8((au + (1 << 19)) >> 20) << (8 * o); ov |= pp_clamp8((av + (1 << 19)) >> 20) << (8 * o);
      }
    }
    uint8_t *tu = t + (size_t)c.dw * c.th_y, *tv = tu + (size_t)(c.dw / 2) * c.th_c;
    *(uint32_t *)(tu + (size_t)row * (c.dw / 2) + x0) = ou;
    *(uint32_t *)(tv + (size_t)row * (c.dw / 2) + x0) = ov;
  }
}

// Vertical pass: intermediate planes -> the destination rectangle (rows d_off + d_step * k).
__global__ void k_conv_v(ConvClass c, const ConvJob *jobs, int nj, const uint8_t *tmp) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wy = c.dw / 4, wc = c.dw / 8;
  size_t ny = (size_t)wy * c.vy.n, nc = (size_t)wc * c.vc.n, per = ny + 2 * nc;
  if (idx >= per * nj) return;
  int j = (int)(idx / per);
  size_t r = idx % per;
  ConvJob J = jobs[j];
  const uint8_t *t = tmp + (size_t)j * c.tstride;
  const uint8_t *sp; uint8_t *dp; int pitch, k, x0, w;
  Taps T;
  if (r < ny) { k = (int)(r / wy); x0 = 4 * (int)(r % wy); sp = t; dp = J.dy; pitch = J.dp; w = c.dw; T = c.vy; }
  else {
    r -= ny;
    int pl = r >= nc; if (pl) r -= nc;
    k = (int)(r / wc); x0 = 4 * (int)(r % wc); w = c.dw / 2; pitch = J.dp / 2; T = c.vc;
    sp = t + (size_t)c.dw * c.th_y + (pl ? (size_t)w * c.th_c : 0);
    dp = pl ? J.dv : J.du;
  }
  int s = T.start[k];
  const int16_t *cf = T.coef + (size_t)k * T.nt;
  const uint32_t *p = (const uint32_t *)(sp + (size_t)s * w + x0);
  int a0 = 8192, a1 = 8192, a2 = 8192, a3 = 8192;
  for (int i = 0; i < T.nt; i++) {
    uint32_t v = *(const uint32_t *)((const uint8_t *)p + (size_t)i * w);
    int ck = cf[i];
    a0 += (int)(v & 255) * ck; a1 += (int)((v >> 8) & 255) * ck; a2 += (int)((v >> 16) & 255) * ck; a3 += (int)(v >> 24) * ck;
  }
  uint32_t out = pp_clamp8(a0 >> 14) | (pp_clamp8(a1 >> 14) << 8) | (pp_clamp8(a2 >> 14) << 16) | (pp_clamp8(a3 >> 14) << 24);
  *(uint32_t *)(dp + (size_t)(c.d_off + c.d_step * k) * pitch + x0) = out;
}

// Same size, no filter: copy luma, split the interleaved chroma (and round 16-bit samples to 8).
__global__ void k_conv_copy(ConvClass c, const ConvJob *jobs, int nj) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wy = c.copy_w / 4, wc = c.copy_w / 8;
  size_t ny = (size_t)wy * c.copy_h, nc = (size_t)wc * (c.copy_h / 2), per = ny + nc;
  if (idx >= per * nj) return;
  int j = (int)(idx / per);
  size_t r = idx % per;
  ConvJob J = jobs[j];
  if (r < ny) {
    int row = (int)(r / wy), x0 = 4 * (int)(r % wy);
    uint32_t out;
    if (c.bps == 1) out = *(const uint32_t *)(J.sy + (size_t)row * J.sp_y + x0);
    else {
      const uint32_t *w = (const uint32_t *)(J.sy + (size_t)row * J.sp_y + 2 * x0);
      uint32_t a = w[0], b = w[1];
      out = pp_clamp8(((a & 0xffff) + 128) >> 8) | (pp_clamp8(((a >> 16) + 128) >> 8) << 8) |
            (pp_clamp8(((b & 0xffff) + 128) >> 8) << 16) | (pp_clamp8(((b >> 16) + 128) >> 8) << 24);
    }
    *(uint32_t *)(J.dy + (size_t)row * J.dp + x0) = out;
  } else {
    r -= ny;
    int row = (int)(r / wc), x0 = 4 * (int)(r % wc);
    uint32_t ou, ov;
    if (c.bps == 1) {
      const uint32_t *w = (const uint32_t *)(J.suv + (size_t)row * J.sp_uv + 2 * x0);
      uint32_t a = w[0], b = w[1];
      ou = (a & 255) | ((a >> 8) & 0xff00) | ((b & 255) << 16) | ((b << 8) & 0xff000000u);
      ov = ((a >> 8) & 255) | ((a >> 16) & 0xff00) | ((b << 8) & 0xff0000) | (b & 0xff000000u);
    } else {
      const uint32_t *w = (const uint32_t *)(J.suv + (size_t)row * J.sp_uv + 4 * x0);
      ou = ov = 0;
      for (int o = 0; o < 4; o++) {
        uint32_t p = w[o];
        ou |= pp_clamp8(((p & 0xffff) + 128) >> 8) << (8 * o); ov |= pp_clamp8(((p >> 16) + 128) >> 8) << (8 * o);
      }
    }
    *(uint32_t *)(J.du + (size_t)row * (J.dp / 2) + x0) = ou;
    *(uint32_t *)(J.dv + (size_t)row * (J.dp / 2) + x0) = ov;
  }
}

// 2:1 reduction in front of the resampler for large ratios (the source frame -> an 8-bit NV12 frame of half the
// size): luma 2 x 2 box; chroma [1 4 3]/8 horizontally (keeps it co-sited with the left luma sample of the
// smaller frame) and the mean of two lines vertically. The resampler then works on a quarter of the samples.
__global__ void k_half(const PreJob *jobs, int nj, int w2, int h2, int bps, uint8_t *pre, size_t stride) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wy = w2 / 4;
  size_t ny = (size_t)wy * h2, nc = (size_t)wy * (h2 / 2), per = ny + nc;
  if (idx >= per * nj) return;
  int j = (int)(idx / per);
  size_t r = idx % per;
  PreJob J = jobs[j];
  uint8_t *d = pre + (size_t)j * stride;
  if (r < ny) {
    int row = (int)(r / wy), x0 = 4 * (int)(r % wy);
    const uint8_t *a = J.sy + (size_t)(2 * row) * J.sp_y, *b = a + J.sp_y;
    uint32_t out = 0;
    if (bps == 1) {
      const uint32_t *pa = (const uint32_t *)(a + 2 * x0), *pb = (const uint32_t *)(b + 2 * x0);
      for (int h = 0; h < 2; h++) {
        uint32_t u = pa[h], v = pb[h];
        uint32_t s0 = (u & 255) + ((u >> 8) & 255) + (v & 255) + ((v >> 8) & 255) + 2;
        uint32_t s1 = ((u >> 16) & 255) + (u >> 24) + ((v >> 16) & 255) + (v >> 24) + 2;
        out |= ((s0 >> 2) | ((s1 >> 2) << 8)) << (16 * h);
      }
    } else {
      const uint32_t *pa = (const uint32_t *)(a + 4 * x0), *pb = (const uint32_t *)(b + 4 * x0);
      for (int i = 0; i < 4; i++) {
        uint32_t u = pa[i], v = pb[i];
        uint32_t sm = (u & 0xffff) + (u >> 16) + (v & 0xffff) + (v >> 16) + 512;
        out |= pp_clamp8((int)(sm >> 10)) << (8 * i);
      }
    }
    *(uint32_t *)(d + (size_t)row * w2 + x0) = out;
  } else {
    r -= ny;
    int row = (int)(r / wy), p0 = 2 * (int)(r % wy);       // two chroma pairs of the half frame per thread
    const uint8_t *a = J.suv + (size_t)(2 * row) * J.sp_uv, *b = a + J.sp_uv;
    uint32_t out = 0;
    for (int i = 0; i < 2; i++) {
      int P = p0 + i, l = 2 * P > 0 ? 2 * P - 1 : 0, c = 2 * P, rr = 2 * P + 1;
      int su, sv;
      if (bps == 1) {
        su = a[2 * l] + 4 * a[2 * c] + 3 * a[2 * rr] + b[2 * l] + 4 * b[2 * c] + 3 * b[2 * rr];
        sv = a[2 * l + 1] + 4 * a[2 * c + 1] + 3 * a[2 * rr + 1] + b[2 * l + 1] + 4 * b[2 * c + 1] + 3 * b[2 * rr + 1];
        su = (su + 8) >> 4; sv = (sv + 8) >> 4;
      } else {
        const uint16_t *a16 = (const uint16_t *)a, *b16 = (const uint16_t *)b;
        su = a16[2 * l] + 4 * a16[2 * c] + 3 * a16[2 * rr] + b16[2 * l] + 4 * b16[2 * c] + 3 * b16[2 * rr];
        sv = a16[2 * l + 1] + 4 * a16[2 * c + 1] + 3 * a16[2 * rr + 1] + b16[2 * l + 1] + 4 * b16[2 * c + 1] + 3 * b16[2 * rr + 1];
        su = (su + 2048) >> 12; sv = (sv + 2048) >> 12;
      }
      out |= (pp_clamp8(su) | (pp_clamp8(sv) << 8)) << (16 * i);
    }
    *(uint32_t *)(d + (size_t)w2 * h2 + (size_t)row * w2 + 2 * p0) = out;
  }
}

// Solid color (background of a letterboxed or composed frame).
__global__ void k_fill(RectJob j) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wy = j.w / 4, wc = j.w / 8;
  size_t ny = (size_t)wy * j.h, nc = (size_t)wc * (j.h / 2);
  if (idx >= ny + 2 * nc) return;
  if (idx < ny) { *(uint32_t *)(j.dy + (idx / wy) * j.dp + 4 * (idx % wy)) = (j.yuv & 255) * 0x01010101u; return; }
  idx -= ny;
  int pl = idx >= nc; if (pl) idx -= nc;
  *(uint32_t *)((pl ? j.dv : j.du) + (idx / wc) * (j.dp / 2) + 4 * (idx % wc)) = ((j.yuv >> (pl ? 16 : 8)) & 255) * 0x01010101u;
}

// Overlay with alpha (slate text, logo, banner): dst = dst + (ov - dst) * a / 255.
__device__ inline uint32_t pp_blend(uint32_t d, uint32_t o, uint32_t a) {
  uint32_t out = 0;
  for (int i = 0; i < 4; i++) {
    int dv = (d >> (8 * i)) & 255, ov = (o >> (8 * i)) & 255, av = (a >> (8 * i)) & 255;
    out |= (uint32_t)(dv + (((ov - dv) * av * 257 + 32768) >> 16)) << (8 * i);
  }
  return out;
}
__global__ void k_overlay(RectJob j) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wy = j.w / 4, wc = j.w / 8;
  size_t ny = (size_t)wy * j.h, nc = (size_t)wc * (j.h / 2);
  if (idx >= ny + 2 * nc) return;
  if (idx < ny) {
    size_t row = idx / wy, x0 = 4 * (idx % wy);
    uint32_t a = *(const uint32_t *)(j.oa + row * j.w + x0);
    if (!a) return;
    uint32_t *d = (uint32_t *)(j.dy + row * j.dp + x0);
    *d = pp_blend(*d, *(const uint32_t *)(j.oy + row * j.w + x0), a);
    return;
  }
  idx -= ny;
  int pl = idx >= nc; if (pl) idx -= nc;
  size_t row = idx / wc, x0 = 4 * (idx % wc);
  uint32_t a = *(const uint32_t *)(j.oa2 + row * (j.w / 2) + x0);
  if (!a) return;
  uint32_t *d = (uint32_t *)((pl ? j.dv : j.du) + row * (j.dp / 2) + x0);
  *d = pp_blend(*d, *(const uint32_t *)((pl ? j.ov : j.ou) + row * (j.w / 2) + x0), a);
}

// Color matrix and range in place (BT.709 <-> BT.601 when the resolution class changes, full ->
// limited range). One thread per four chroma samples and the 8 x 2 luma samples that share them;
// the luma correction uses the nearest chroma sample (in an interlaced frame: of the same field).
__global__ void k_csc(RectJob j, CscPar m) {
  size_t idx = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  const int wc = j.w / 8;
  if (idx >= (size_t)wc * (j.h / 2)) return;
  int crow = (int)(idx / wc), x0 = 4 * (int)(idx % wc);
  uint32_t *pu = (uint32_t *)(j.du + (size_t)crow * (j.dp / 2) + x0), *pv = (uint32_t *)(j.dv + (size_t)crow * (j.dp / 2) + x0);
  uint32_t u = *pu, v = *pv, ou = 0, ov = 0;
  int ly0, ly1;   // the two luma rows that use this chroma row
  if (m.interlaced) { int p = crow & 1, f = crow >> 1; ly0 = 4 * f + p; ly1 = ly0 + 2; } else { ly0 = 2 * crow; ly1 = ly0 + 1; }
  uint32_t *py[2] = {(uint32_t *)(j.dy + (size_t)ly0 * j.dp + 2 * x0), (uint32_t *)(j.dy + (size_t)ly1 * j.dp + 2 * x0)};
  uint32_t ya[2][2] = {{py[0][0], py[0][1]}, {py[1][0], py[1][1]}}, yo[2][2] = {{0, 0}, {0, 0}};
  for (int i = 0; i < 4; i++) {
    int cb = (int)((u >> (8 * i)) & 255) - 128, cr = (int)((v >> (8 * i)) & 255) - 128;
    int ycorr = m.m[1] * cb + m.m[2] * cr + m.off[0] + 2048;
    for (int r = 0; r < 2; r++)
      for (int k = 0; k < 2; k++) {
        int px = 2 * i + k;
        int y = (ya[r][px >> 2] >> (8 * (px & 3))) & 255;
        yo[r][px >> 2] |= pp_clamp8((m.m[0] * y + ycorr) >> 12) << (8 * (px & 3));
      }
    ou |= pp_clamp8((m.m[4] * cb + m.m[5] * cr + m.off[1] + 2048) >> 12) << (8 * i);
    ov |= pp_clamp8((m.m[7] * cb + m.m[8] * cr + m.off[2] + 2048) >> 12) << (8 * i);
  }
  py[0][0] = yo[0][0]; py[0][1] = yo[0][1]; py[1][0] = yo[1][0]; py[1][1] = yo[1][1];
  *pu = ou; *pv = ov;
}
