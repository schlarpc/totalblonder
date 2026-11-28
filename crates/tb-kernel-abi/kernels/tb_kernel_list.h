// The kernels of the image. X(name, barriers): barriers = 1 when the kernel calls
// __syncthreads or a __shfl_*_sync (the host launcher then runs the threads of a block as
// fibers in lockstep; the others run as a plain loop).
// Adding a kernel: add it here, add its argument record to crates/tb-device/src/kernel.rs.
#pragma once
#define TB_KERNELS(X) \
  X(k_tick, 0) \
  X(k_pad_cols, 0) \
  X(k_pad_rows, 0) \
  X(k_pyr, 0) \
  X(k_set_mode, 0) \
  X(k_mbstat, 0) \
  X(k_me, 0) \
  X(k_mvsmooth, 0) \
  X(k_xf, 1) \
  X(k_xf16, 1) \
  X(k_xf32, 1) \
  X(k_xf64, 1) \
  X(k_rowstat, 1) \
  X(k_quant, 0) \
  X(k_recon, 0) \
  X(k_ent_resolve, 0) \
  X(k_ent_pack, 0) \
  X(k_ent_rowsum, 1) \
  X(k_ent_picsum, 0) \
  X(k_ent_scatter, 0) \
  X(k_conv_h, 0) \
  X(k_conv_v, 0) \
  X(k_conv_copy, 0) \
  X(k_half, 0) \
  X(k_fill, 0) \
  X(k_overlay, 0) \
  X(k_csc, 0)
