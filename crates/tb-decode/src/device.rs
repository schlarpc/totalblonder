//! The libav hardware device context (the C++ `DecDevice`).

use std::ffi::{CStr, CString, c_void};
use std::ptr;
use std::sync::Once;

use ffmpeg_the_third::ffi;

use crate::{AvError, DecodeError};

/// `AVCUDADeviceContext` of `libavutil/hwcontext_cuda.h` (FFmpeg 9.0): what
/// `AVHWDeviceContext.hwctx` points to for `AV_HWDEVICE_TYPE_CUDA`. `CUcontext` and `CUstream`
/// are pointers to opaque structs. Not in the generated bindings (the header needs NVIDIA's);
/// `shim/cuda_layout.c` holds this declaration to the header (flake check
/// `tb-decode/cuda-layout`).
#[repr(C)]
struct AvCudaDeviceContext {
    cuda_ctx: *mut c_void,
    stream: *mut c_void,
    internal: *mut c_void,
}

/// Loads the driver's libraries that libav opens by name (`libcuda.so.1` for the device context,
/// `libnvcuvid.so.1` for NVDEC), so that libav finds them: by the loader's search path first,
/// else from where NixOS puts them (as `tb-device-cuda` does for `libcuda.so.1`). A library
/// loaded here answers libav's later `dlopen` of the same soname. Missing libraries are left
/// for libav to report.
fn preload_driver() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for names in [
            [c"libcuda.so.1", c"/run/opengl-driver/lib/libcuda.so.1"],
            [
                c"libnvcuvid.so.1",
                c"/run/opengl-driver/lib/libnvcuvid.so.1",
            ],
        ] {
            let found = names.iter().any(|name: &&CStr| {
                // SAFETY: a NUL-terminated name; loading runs the initializers of NVIDIA's
                // driver library, which have no preconditions. The handle is kept for the
                // life of the process on purpose (libav takes handles of its own).
                !unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }
                    .is_null()
            });
            tracing::debug!(library = ?names[0], found, "driver library");
        }
    });
}

/// A libav hardware device context for decoding on a CUDA device. Shared by the inputs opened
/// on it (each holds a reference of its own).
#[derive(Debug)]
pub struct DecDevice {
    hw: *mut ffi::AVBufferRef,
    shared: bool,
}

// SAFETY: the buffer reference is only read (to take further references, which libav counts
// atomically) and released once, in `drop`; libav's device context has no thread affinity.
unsafe impl Send for DecDevice {}
// SAFETY: as above: `&DecDevice` only allows `av_buffer_ref` on a reference nobody changes.
unsafe impl Sync for DecDevice {}

impl DecDevice {
    /// Wraps an existing CUDA context (a raw `CUcontext`) and a `CUstream` of that context, on
    /// which libav runs its own copies (decoder surface to pool frame). The stream may be null (the
    /// context's default stream); the C++ decoder gave libav a non-blocking stream of its own so
    /// that its copies neither wait for nor hold up the other streams.
    ///
    /// libav pushes the context around its own calls and never destroys it.
    ///
    /// # Safety
    ///
    /// `cuda_ctx` is a valid context and `stream` null or a valid stream of it, and both
    /// outlive this `DecDevice`, every [`Input`](crate::Input) opened on it and every
    /// [`Frame`](crate::Frame) those inputs returned.
    pub unsafe fn wrap(cuda_ctx: *mut c_void, stream: *mut c_void) -> Result<Self, DecodeError> {
        if cuda_ctx.is_null() {
            return Err(DecodeError::NullContext);
        }
        preload_driver();
        // SAFETY: no preconditions; returns null when out of memory.
        let mut hw = unsafe { ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType::CUDA) };
        if hw.is_null() {
            return Err(DecodeError::Alloc("a CUDA device context"));
        }
        // SAFETY: `hw` is the buffer `av_hwdevice_ctx_alloc` returned: its data is an
        // `AVHWDeviceContext`, and for the CUDA type libav allocated `hwctx` as an
        // `AVCUDADeviceContext` (zeroed), which this code is the only user of until init.
        unsafe {
            let dev = (*hw).data.cast::<ffi::AVHWDeviceContext>();
            let cuda = (*dev).hwctx.cast::<AvCudaDeviceContext>();
            (*cuda).cuda_ctx = cuda_ctx;
            (*cuda).stream = stream;
        }
        // SAFETY: `hw` is an allocated, not yet initialized device context.
        let e = unsafe { ffi::av_hwdevice_ctx_init(hw) };
        if e < 0 {
            // SAFETY: `hw` is the reference this function owns; nothing else holds one.
            unsafe { ffi::av_buffer_unref(&raw mut hw) };
            return Err(DecodeError::Device {
                what: "av_hwdevice_ctx_init",
                source: AvError::from_code(e),
            });
        }
        tracing::debug!("libav device context on the caller's CUDA context");
        Ok(Self { hw, shared: true })
    }

    /// A CUDA context of libav's own on device `ordinal` (the comparison case `--dec-ctx own`).
    pub fn own(ordinal: u32) -> Result<Self, DecodeError> {
        let name = CString::new(ordinal.to_string())
            .map_err(|_| DecodeError::BadOption("device ordinal".into()))?;
        let mut hw = ptr::null_mut();
        preload_driver();
        // SAFETY: `hw` is a valid place for the result, `name` a NUL-terminated string that
        // lives over the call, no options.
        let e = unsafe {
            ffi::av_hwdevice_ctx_create(
                &raw mut hw,
                ffi::AVHWDeviceType::CUDA,
                name.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        if e < 0 || hw.is_null() {
            return Err(DecodeError::Device {
                what: "av_hwdevice_ctx_create",
                source: AvError::from_code(e),
            });
        }
        tracing::debug!(
            ordinal,
            "libav device context with a CUDA context of its own"
        );
        Ok(Self { hw, shared: false })
    }

    /// Whether this wraps the caller's context ([`DecDevice::wrap`]) rather than one of libav's.
    pub fn is_shared(&self) -> bool {
        self.shared
    }

    /// A new reference to the device context, for a decoder context to own (null: no memory).
    pub(crate) fn new_ref(&self) -> *mut ffi::AVBufferRef {
        // SAFETY: `self.hw` is a live reference for as long as `self` is.
        unsafe { ffi::av_buffer_ref(self.hw) }
    }
}

impl Drop for DecDevice {
    fn drop(&mut self) {
        // SAFETY: `self.hw` is the reference this object owns and is not used afterwards.
        // Decoders and frames hold references of their own.
        unsafe { ffi::av_buffer_unref(&raw mut self.hw) };
    }
}
