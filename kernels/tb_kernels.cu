// Device image of the encoder kernels: nvcc -cubin compiles this file for sm_89 with the flags
// of nix/kernels.nix (nvflags: those the C++ spike built its kernels with, the same machine code).
// nix/kernels.nix checks that every kernel of tb_kernel_list.h is in the image.
#include "kernels.h"
#include "pipe_kernels.h"
