/* Holds the declaration of AVCUDADeviceContext in crates/tb-decode/src/device.rs to the header
 * of the FFmpeg the crate links (libavutil/hwcontext_cuda.h, which needs NVIDIA's headers and
 * is therefore not in the generated bindings). Compiled, not linked or run: the flake check
 * `tb-decode/cuda-layout` (nix/rust.nix). */
#include <stddef.h>
/* CUcontext and CUstream as libavutil itself was compiled with them (nv-codec-headers); it also
 * defines CUDA_VERSION, which keeps hwcontext_cuda.h from asking for the toolkit's cuda.h. */
#include <ffnvcodec/dynlink_cuda.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_cuda.h>
#include <libavutil/pixfmt.h>

/* device.rs: #[repr(C)] struct AvCudaDeviceContext { cuda_ctx, stream, internal }: three pointers */
_Static_assert(sizeof(AVCUDADeviceContext) == 3 * sizeof(void *), "AVCUDADeviceContext: size");
_Static_assert(offsetof(AVCUDADeviceContext, cuda_ctx) == 0, "AVCUDADeviceContext.cuda_ctx");
_Static_assert(offsetof(AVCUDADeviceContext, stream) == sizeof(void *), "AVCUDADeviceContext.stream");
_Static_assert(offsetof(AVCUDADeviceContext, internal) == 2 * sizeof(void *), "AVCUDADeviceContext.internal");
_Static_assert(sizeof(((AVCUDADeviceContext *)0)->cuda_ctx) == sizeof(void *), "CUcontext is a pointer");
_Static_assert(sizeof(((AVCUDADeviceContext *)0)->stream) == sizeof(void *), "CUstream is a pointer");

/* the fields are what the crate takes them for: a context handle and a stream handle */
static void fields(AVCUDADeviceContext *c, CUcontext ctx, CUstream stream) {
  c->cuda_ctx = ctx;
  c->stream = stream;
}

int main(void) {
  (void)fields;
  return AV_HWDEVICE_TYPE_CUDA == 2 && AV_PIX_FMT_CUDA != AV_PIX_FMT_NONE ? 0 : 1;
}
