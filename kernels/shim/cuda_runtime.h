// Host stand-in for <cuda_runtime.h>: lets kernels.h compile as ordinary C++ so that the CPU
// reference adapter (crates/tb-device-ref) runs the same kernel source as the GPU.
//
// Model: tbk_launch() runs the blocks of a launch one after the other on the calling thread.
// Inside a block, a kernel without barriers runs its threads as a loop. A kernel with barriers
// (__syncthreads, __shfl_*_sync) runs its threads as fibers in lockstep: every thread runs to
// its next barrier, then all continue. A shuffle is a barrier at which each lane publishes its
// value and reads its partner's value of the same step.
//
// Only what kernels.h and entropy.h use is defined. Arithmetic follows the CUDA definitions of
// the intrinsics. Floating point is compiled without contraction (-ffp-contract=off), as the
// device image is (-fmad=false); the kernels write their fused multiply-adds as fmaf, which is
// libm's correctly rounded fused operation here and fma.rn.f32 on the card, so both builds
// compute the same bits (docs/ARCHITECTURE.md §7.3).
#pragma once
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#define __global__
#define __device__
#define __host__
#define __shared__ static thread_local

struct tbk_dim3 { unsigned x, y, z; };
extern thread_local tbk_dim3 threadIdx, blockIdx, blockDim, gridDim;

// ---- integer intrinsics (CUDA "integer intrinsics" and SIMD-in-a-word video instructions)
static inline unsigned __vsadu4(uint32_t a, uint32_t b) {   // sum of absolute differences of the four bytes
  unsigned s = 0;
  for (int i = 0; i < 4; i++) {
    int x = (int)((a >> (8 * i)) & 255) - (int)((b >> (8 * i)) & 255);
    s += (unsigned)(x < 0 ? -x : x);
  }
  return s;
}
static inline uint32_t __vavgu4(uint32_t a, uint32_t b) {   // per byte (a + b + 1) >> 1
  return (a | b) - (((a ^ b) & 0xfefefefeu) >> 1);
}
static inline uint32_t __funnelshift_r(uint32_t lo, uint32_t hi, uint32_t shift) {   // low word of (hi:lo) >> (shift & 31)
  shift &= 31;
  return shift ? (lo >> shift) | (hi << (32 - shift)) : lo;
}
static inline unsigned __dp4a(uint32_t a, uint32_t b, unsigned c) {   // c + dot product of the four bytes (unsigned)
  for (int i = 0; i < 4; i++) c += ((a >> (8 * i)) & 255) * ((b >> (8 * i)) & 255);
  return c;
}
static inline int __ffsll(long long v) { return __builtin_ffsll(v); }
static inline uint32_t __float_as_uint(float f) { uint32_t u; memcpy(&u, &f, 4); return u; }
// One block runs on one host thread, so the atomics are plain operations.
static inline uint32_t atomicOr(uint32_t *p, uint32_t v) { uint32_t o = *p; *p = o | v; return o; }

// ---- barriers (implemented by the launcher in tb_kernels_ref.cc)
void tbk_barrier();
uint64_t tbk_shuffle(uint64_t value, int partner_delta_kind, int d);
static inline void __syncthreads() { tbk_barrier(); }
template <class T> static inline T __shfl_xor_sync(unsigned, T v, int d) {
  uint64_t in = 0, out;
  memcpy(&in, &v, sizeof(T));
  out = tbk_shuffle(in, 0, d);
  T r;
  memcpy(&r, &out, sizeof(T));
  return r;
}
template <class T> static inline T __shfl_up_sync(unsigned, T v, int d) {
  uint64_t in = 0, out;
  memcpy(&in, &v, sizeof(T));
  out = tbk_shuffle(in, 1, d);
  T r;
  memcpy(&r, &out, sizeof(T));
  return r;
}
