// Device stand-in for <cuda_runtime.h> when the kernels are compiled with HIP for AMD GPUs (the
// device image `deviceImageHip` of nix/kernels.nix: hipcc --genco with -Ikernels/shim/hip).
// The kernel sources stay CUDA C++; this header supplies what HIP spells differently or lacks.
//
// - Qualifiers, blockIdx / threadIdx / blockDim / gridDim, __syncthreads, __ffsll,
//   __float_as_uint, __funnelshift_r (shift modulo 32, as in CUDA) and atomicOr are HIP's own.
// - HIP's __shfl_*_sync take a 64-bit lane mask and the warp's width by default (64 on wave64
//   parts). The kernels pass 0xffffffffu and are written for 32 lanes (lane = threadIdx & 31,
//   warp = threadIdx >> 5), so those builtins are disabled and the shuffles below pass width 32:
//   the same lanes take part as on the card, on wave32 and wave64 alike. The kernels call them
//   only where every thread of the block arrives (AMD lanes run in lockstep; there is no
//   independent thread scheduling to synchronize).
// - The SIMD-in-a-word intrinsics follow their CUDA definitions (integer: exact on any part);
//   they map to single AMD instructions where one exists.
//
// Floating point: hipcc compiles with -ffp-contract=off (no fused operation the source does not
// write), IEEE division and denormals kept, so the kernels compute the bits the card and the host
// build compute (docs/ARCHITECTURE.md §7.3).
#pragma once
#define HIP_DISABLE_WARP_SYNC_BUILTINS 1
#include <hip/hip_runtime.h>
#include <stdint.h>

// The kernels' own convention (the host build passes -D__CUDACC__ too): kernel code is compiled.
#ifndef __CUDACC__
#define __CUDACC__ 1
#endif
// 8x8 blocks per thread block of k_xf in this image (tb_kernel_abi.h; the host launches by it).
#define XF_BLOCKS XB_HIP

// Sum of absolute differences of the four bytes (v_sad_u8).
__device__ static inline unsigned __vsadu4(uint32_t a, uint32_t b) {
  return __builtin_amdgcn_sad_u8(a, b, 0u);
}
// Per byte (a + b + 1) >> 1 (v_lerp_u8 with the rounding bit of every byte set).
__device__ static inline uint32_t __vavgu4(uint32_t a, uint32_t b) {
  return __builtin_amdgcn_lerp(a, b, 0x01010101u);
}
// c + dot product of the four bytes, unsigned, without saturation (CUDA's __dp4a wraps).
__device__ static inline unsigned __dp4a(uint32_t a, uint32_t b, unsigned c) {
  for (int i = 0; i < 4; i++) c += ((a >> (8 * i)) & 255u) * ((b >> (8 * i)) & 255u);
  return c;
}

template <class T> __device__ static inline T __shfl_xor_sync(unsigned, T v, int d) {
  return __shfl_xor(v, d, 32);
}
template <class T> __device__ static inline T __shfl_up_sync(unsigned, T v, unsigned d) {
  return __shfl_up(v, d, 32);
}
