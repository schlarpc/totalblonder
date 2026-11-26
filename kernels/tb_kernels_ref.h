// C interface of the host build of the kernels (libtbkernels_ref.a), used by crates/tb-device-ref.
#pragma once
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

// Number of kernels in the library.
int tbk_kernel_count(void);
// Name of kernel `kernel` (as in tb_kernel_list.h), or null.
const char *tbk_kernel_name(int kernel);
// Writes the size in bytes of each parameter of the kernel to sizes[0..max) and returns the
// number of parameters, or -1 for an unknown kernel.
int tbk_kernel_params(int kernel, unsigned *sizes, int max);
// Runs kernel `kernel` over `grid` blocks of `block` threads. params[i] points to the value of
// parameter i, exactly as for cuLaunchKernel. Returns 0, or: -1 unknown kernel, -2 block size
// out of range (1..1024), -3 the threads of a block did not reach their barriers together
// (in some step not all threads stopped at the same kind of barrier, or some ended while others
// waited), -4 no memory for the fiber stacks, -5 a kernel that tb_kernel_list.h lists without
// barriers reached one.
int tbk_launch(int kernel, unsigned grid, unsigned block, void **params);

// 1 when the library was built with AddressSanitizer (nix/kernels.nix: refLibAsan), else 0.
int tbk_sanitized(void);

// The launcher's own test kernels (not part of the image), for the tests of
// crates/tb-device-ref: their number, and a launch of test kernel `kernel` with an output array
// of grid * block words and one integer argument. Returns as tbk_launch. The kernels, in order:
// 0 t_sync, 1 t_shfl_xor (arg: delta), 2 t_shfl_up (arg: delta), 3 t_scan (arg: factor),
// 4 t_diverge_kind, 5 t_diverge_count, 6 t_diverge_delta, 7 t_sync run without the barrier flag.
int tbk_test_kernel_count(void);
int tbk_test_launch(int kernel, unsigned grid, unsigned block, uint32_t *out, int arg);

#ifdef __cplusplus
}
#endif
