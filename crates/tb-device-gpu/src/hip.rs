//! The HIP runtime: the part of AMD's HIP API the adapter uses, bound by hand and loaded at run
//! time, the counterpart of [`crate::cuda`].
//!
//! HIP mirrors the CUDA driver API the adapter uses nearly name for name (`hipModuleLoadData`,
//! `hipModuleLaunchKernel`, `hipMemcpyHtoDAsync`, `hipEventRecord`, ...), and numbers its
//! statuses after the CUDA runtime's (600 not ready, 700 illegal address, 701 out of resources,
//! 709 context destroyed). Two differences shape the adapter's side:
//!
//! - Contexts: HIP's primary-context functions are deprecated. The adapter makes the device
//!   current with `hipSetDevice` (blocking-sync scheduling set before with `hipSetDeviceFlags`,
//!   refused once the device is active, which is accepted as in CUDA), and `restore` destroys the
//!   device's primary context with `hipDeviceReset`.
//! - `hipDeviceptr_t` is a pointer, not an integer: the implementation converts.
//!
//! Loaded from `libamdhip64.so` (ROCm 6.2 or later has every function bound here; the pinned
//! toolkit is ROCm 7.2). The bindings are checked against `hip/hip_runtime_api.h` of the pinned
//! toolkit as [`crate::cuda`]'s are against `cuda.h`: the test `api_check_is_fresh` keeps
//! `hip_api_check.cc` fresh and the Nix check `kernels/hip-api` compiles it.
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    reason = "names are those of hip_runtime_api.h, so that the generated check can name both sides"
)]

use std::ffi::{CStr, c_char, c_float, c_int, c_uint, c_void};

use crate::KernelImage;
use crate::runtime::{Call, Handle, Runtime, Status, bindings, call, consts, sys};

/// `hipError_t`: 0 is success. A plain integer: every value is valid (the header's enum is
/// `int`-sized; the check asserts the size).
pub type hipError_t = c_int;
/// `hipDevice_t`.
pub type hipDevice_t = c_int;
/// `hipDeviceptr_t`: a pointer in HIP.
pub type hipDeviceptr_t = *mut c_void;
/// `hipModule_t`.
pub type hipModule_t = *mut c_void;
/// `hipFunction_t`.
pub type hipFunction_t = *mut c_void;
/// `hipStream_t`.
pub type hipStream_t = *mut c_void;
/// `hipEvent_t`.
pub type hipEvent_t = *mut c_void;
/// `hipMemcpyKind` (a C enum: `int`-sized; the check asserts the size).
pub type hipMemcpyKind = c_uint;

consts! {
    /// Success.
    hipSuccess: hipError_t = 0;
    /// `hipStreamQuery` / `hipEventQuery`: the work is not finished.
    hipErrorNotReady: hipError_t = 600;
    /// An argument out of range. A launch of more work-items than a dispatch can name (2^31
    /// blocks of 1024 threads) is refused with it; the context stays usable.
    hipErrorInvalidValue: hipError_t = 1;
    /// The launch needs more resources than a compute unit has.
    hipErrorLaunchOutOfResources: hipError_t = 701;
    /// Lost-context statuses (see [`context_lost`]).
    hipErrorDeinitialized: hipError_t = 4;
    /// See [`context_lost`].
    hipErrorInvalidContext: hipError_t = 201;
    /// See [`context_lost`].
    hipErrorECCNotCorrectable: hipError_t = 214;
    /// See [`context_lost`].
    hipErrorIllegalAddress: hipError_t = 700;
    /// See [`context_lost`].
    hipErrorLaunchTimeOut: hipError_t = 702;
    /// See [`context_lost`].
    hipErrorContextIsDestroyed: hipError_t = 709;
    /// See [`context_lost`].
    hipErrorAssert: hipError_t = 710;
    /// See [`context_lost`].
    hipErrorLaunchFailure: hipError_t = 719;
    /// See [`context_lost`].
    hipErrorUnknown: hipError_t = 999;
    /// `hipDeviceScheduleBlockingSync`: a thread that waits for the device sleeps.
    hipDeviceScheduleBlockingSync: c_uint = 4;
    /// `hipStreamNonBlocking`: the stream does not synchronize with the null stream.
    hipStreamNonBlocking: c_uint = 1;
    /// `hipEventBlockingSync`: `hipEventSynchronize` on this event sleeps instead of spinning.
    hipEventBlockingSync: c_uint = 1;
    /// `hipEventDisableTiming`.
    hipEventDisableTiming: c_uint = 2;
    /// `hipHostMallocDefault`: pinned, portable to the device.
    hipHostMallocDefault: c_uint = 0;
    /// `hipMemcpyHostToDevice`.
    hipMemcpyHostToDevice: hipMemcpyKind = 1;
}

/// Statuses after which the device's context cannot be used any more: those of
/// [`crate::cuda::context_lost`] that HIP has (HIP numbers them the same).
pub fn context_lost(s: hipError_t) -> bool {
    matches!(
        s,
        hipErrorDeinitialized
            | hipErrorInvalidContext
            | hipErrorECCNotCorrectable
            | hipErrorIllegalAddress
            | hipErrorLaunchTimeOut
            | hipErrorContextIsDestroyed
            | hipErrorAssert
            | hipErrorLaunchFailure
            | hipErrorUnknown
    )
}

/// Faults on the device after which no context can be made in this process.
pub fn poisons_process(s: hipError_t) -> bool {
    matches!(
        s,
        hipErrorIllegalAddress | hipErrorLaunchTimeOut | hipErrorLaunchFailure
    )
}

bindings! {
    status = hipError_t;
    libraries = [
        "libamdhip64.so",
        "libamdhip64.so.7",
        "libamdhip64.so.6",
        "/run/opengl-driver/lib/libamdhip64.so",
        "/run/opengl-driver/lib/libamdhip64.so.7",
        "/run/opengl-driver/lib/libamdhip64.so.6",
    ];
    fn hipInit(c_uint);
    fn hipDrvGetErrorName(hipError_t, *mut *const c_char);
    fn hipDeviceGet(*mut hipDevice_t, c_int);
    fn hipDeviceGetName(*mut c_char, c_int, hipDevice_t);
    fn hipSetDeviceFlags(c_uint);
    fn hipSetDevice(c_int);
    fn hipDeviceReset();
    fn hipModuleLoadData(*mut hipModule_t, *const c_void);
    fn hipModuleUnload(hipModule_t);
    fn hipModuleGetFunction(*mut hipFunction_t, hipModule_t, *const c_char);
    fn hipMalloc(*mut *mut c_void, usize);
    fn hipMemGetInfo(*mut usize, *mut usize);
    fn hipFree(*mut c_void);
    fn hipHostMalloc(*mut *mut c_void, usize, c_uint);
    fn hipFreeHost(*mut c_void);
    fn hipMemcpyHtoDAsync(hipDeviceptr_t, *const c_void, usize, hipStream_t);
    fn hipMemcpyDtoHAsync(*mut c_void, hipDeviceptr_t, usize, hipStream_t);
    fn hipMemcpyDtoDAsync(hipDeviceptr_t, hipDeviceptr_t, usize, hipStream_t);
    fn hipMemcpy2DAsync(
        *mut c_void, usize, *const c_void, usize, usize, usize, hipMemcpyKind, hipStream_t,
    );
    fn hipMemsetD8Async(hipDeviceptr_t, u8, usize, hipStream_t);
    fn hipModuleLaunchKernel(
        hipFunction_t, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint, hipStream_t,
        *mut *mut c_void, *mut *mut c_void,
    );
    fn hipStreamCreateWithFlags(*mut hipStream_t, c_uint);
    fn hipStreamDestroy(hipStream_t);
    fn hipStreamSynchronize(hipStream_t);
    fn hipStreamQuery(hipStream_t);
    fn hipStreamWaitEvent(hipStream_t, hipEvent_t, c_uint);
    fn hipEventCreateWithFlags(*mut hipEvent_t, c_uint);
    fn hipEventDestroy(hipEvent_t);
    fn hipEventRecord(hipEvent_t, hipStream_t);
    fn hipEventQuery(hipEvent_t);
    fn hipEventSynchronize(hipEvent_t);
    fn hipEventElapsedTime(*mut c_float, hipEvent_t, hipEvent_t);
}

/// A device address as HIP's pointer type.
#[inline]
fn dptr(a: u64) -> hipDeviceptr_t {
    std::ptr::with_exposed_provenance_mut(a as usize)
}

/// The HIP runtime: AMD's `libamdhip64.so` (ROCm).
pub struct Hip(Lib);

impl Runtime for Hip {
    const NAME: &'static str = "HIP";
    const IMAGE: KernelImage = KernelImage {
        image: include_bytes!(env!("TB_KERNEL_IMAGE_HIP")),
        symbols: include_str!(env!("TB_KERNEL_SYMBOLS_HIP")),
    };
    const NOT_READY: Status = hipErrorNotReady;
    const CONTEXT_IS_DESTROYED: Status = hipErrorContextIsDestroyed;
    fn context_lost(s: Status) -> bool {
        context_lost(s)
    }
    fn poisons_process(s: Status) -> bool {
        poisons_process(s)
    }
}

impl sys::Api for Hip {
    fn load() -> Result<Self, String> {
        Lib::load().map(Self)
    }

    fn status_name(&self, s: Status) -> Option<String> {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: the out-pointer is to a local; on success the runtime points it at a static
        // NUL-terminated string. An unknown status is refused and `p` stays null.
        unsafe {
            ((self.0.hipDrvGetErrorName)(s, &mut p) == hipSuccess && !p.is_null())
                .then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
        }
    }

    unsafe fn init(&self, ordinal: i32) -> Result<i32, Call> {
        let mut device: hipDevice_t = 0;
        // SAFETY: hipInit is the first call; the out-pointer is to a local.
        unsafe {
            call("hipInit", (self.0.hipInit)(0))?;
            call("hipDeviceGet", (self.0.hipDeviceGet)(&mut device, ordinal))?;
        }
        Ok(device)
    }

    unsafe fn device_name(&self, device: i32) -> Option<String> {
        let mut name = [0 as c_char; 128];
        // SAFETY: `name` has the length passed (less one, so that it stays NUL-terminated);
        // CStr reads up to the NUL inside it.
        unsafe {
            ((self.0.hipDeviceGetName)(name.as_mut_ptr(), name.len() as c_int - 1, device)
                == hipSuccess)
                .then(|| CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned())
        }
    }

    unsafe fn enter(&self, device: i32) -> Result<Handle, Call> {
        // SAFETY: plain runtime calls. The flags apply when the device's primary context is
        // made; on an active device the call is refused and the existing flags stay, which is
        // accepted (as in CUDA).
        unsafe {
            let _ = (self.0.hipSetDeviceFlags)(hipDeviceScheduleBlockingSync);
            call("hipSetDevice", (self.0.hipSetDevice)(device))?;
        }
        Ok(std::ptr::null_mut())
    }

    unsafe fn leave(&self, _device: i32) {
        // `hipSetDevice` takes nothing that needs giving back.
    }

    unsafe fn reset(&self, device: i32) {
        // SAFETY: plain runtime calls; the reset acts on the current device, made `device`
        // first, and destroys its primary context with every allocation in it.
        unsafe {
            let _ = (self.0.hipSetDevice)(device);
            let _ = (self.0.hipDeviceReset)();
        }
    }

    unsafe fn module_load(&self, module: &mut Handle, image: *const c_void) -> Status {
        // SAFETY: the caller's contract (`sys::Api`). The image is an ELF code object (or an
        // offload bundle) whose length the runtime reads from its headers.
        unsafe { (self.0.hipModuleLoadData)(module, image) }
    }
    unsafe fn module_unload(&self, module: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipModuleUnload)(module) }
    }
    unsafe fn module_function(
        &self,
        f: &mut Handle,
        module: Handle,
        name: *const c_char,
    ) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipModuleGetFunction)(f, module, name) }
    }
    unsafe fn mem_alloc(&self, ptr: &mut u64, len: usize) -> Status {
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: the caller's contract (`sys::Api`); the out-pointer is to a local.
        let s = unsafe { (self.0.hipMalloc)(&mut p, len) };
        *ptr = p.expose_provenance() as u64;
        s
    }
    unsafe fn mem_free(&self, ptr: u64) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipFree)(dptr(ptr)) }
    }
    unsafe fn host_alloc(&self, ptr: &mut *mut c_void, len: usize) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipHostMalloc)(ptr, len, hipHostMallocDefault) }
    }
    unsafe fn host_free(&self, ptr: *mut c_void) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipFreeHost)(ptr) }
    }
    unsafe fn mem_info(&self, free: &mut usize, total: &mut usize) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipMemGetInfo)(free, total) }
    }
    #[inline]
    unsafe fn memset_d8(&self, dst: u64, byte: u8, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipMemsetD8Async)(dptr(dst), byte, len, stream) }
    }
    #[inline]
    unsafe fn htod(&self, dst: u64, src: *const c_void, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipMemcpyHtoDAsync)(dptr(dst), src, len, stream) }
    }
    #[inline]
    unsafe fn dtoh(&self, dst: *mut c_void, src: u64, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipMemcpyDtoHAsync)(dst, dptr(src), len, stream) }
    }
    #[inline]
    unsafe fn dtod(&self, dst: u64, src: u64, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipMemcpyDtoDAsync)(dptr(dst), dptr(src), len, stream) }
    }
    #[inline]
    unsafe fn htod_2d(
        &self,
        dst: u64,
        dst_pitch: usize,
        src: *const c_void,
        src_pitch: usize,
        width: usize,
        rows: usize,
        stream: Handle,
    ) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe {
            (self.0.hipMemcpy2DAsync)(
                dptr(dst),
                dst_pitch,
                src,
                src_pitch,
                width,
                rows,
                hipMemcpyHostToDevice,
                stream,
            )
        }
    }
    #[inline]
    unsafe fn launch(
        &self,
        f: Handle,
        grid: u32,
        block: u32,
        stream: Handle,
        args: *mut *mut c_void,
    ) -> Status {
        // SAFETY: the caller's contract (`sys::Api`). The runtime copies the arguments during
        // the call.
        unsafe {
            (self.0.hipModuleLaunchKernel)(
                f,
                grid,
                1,
                1,
                block,
                1,
                1,
                0,
                stream,
                args,
                std::ptr::null_mut(),
            )
        }
    }
    unsafe fn stream_create(&self, stream: &mut Handle, non_blocking: bool) -> Status {
        let flags = if non_blocking {
            hipStreamNonBlocking
        } else {
            0
        };
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipStreamCreateWithFlags)(stream, flags) }
    }
    unsafe fn stream_destroy(&self, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipStreamDestroy)(stream) }
    }
    #[inline]
    unsafe fn stream_sync(&self, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipStreamSynchronize)(stream) }
    }
    #[inline]
    unsafe fn stream_query(&self, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipStreamQuery)(stream) }
    }
    #[inline]
    unsafe fn stream_wait_event(&self, stream: Handle, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipStreamWaitEvent)(stream, event, 0) }
    }
    unsafe fn event_create(&self, event: &mut Handle, blocking: bool, timing: bool) -> Status {
        let flags = if blocking { hipEventBlockingSync } else { 0 }
            | if timing { 0 } else { hipEventDisableTiming };
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipEventCreateWithFlags)(event, flags) }
    }
    unsafe fn event_destroy(&self, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipEventDestroy)(event) }
    }
    #[inline]
    unsafe fn event_record(&self, event: Handle, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipEventRecord)(event, stream) }
    }
    #[inline]
    unsafe fn event_query(&self, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipEventQuery)(event) }
    }
    #[inline]
    unsafe fn event_sync(&self, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipEventSynchronize)(event) }
    }
    unsafe fn event_elapsed(&self, ms: &mut f32, start: Handle, end: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.hipEventElapsedTime)(ms, start, end) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{CheckSpec, assert_fresh, check_source};

    /// The C++ source that checks this module against `hip_runtime_api.h`
    /// (`hip_api_check.cc`).
    fn source() -> String {
        check_source(&CheckSpec {
            module: "crates/tb-device-gpu/src/hip.rs",
            test: "api_check_is_fresh",
            nix_check: "kernels/hip-api",
            prelude: "#define __HIP_PLATFORM_AMD__ 1\n",
            header: "hip/hip_runtime_api.h",
            status: "hipError_t",
            functions: FUNCTIONS,
            constants: CONSTANTS,
            sizes: &[
                ("hipMemcpyKind", size_of::<hipMemcpyKind>()),
                ("hipError_t", size_of::<hipError_t>()),
                ("hipDeviceptr_t", size_of::<hipDeviceptr_t>()),
            ],
            records: &[],
            types: &[],
        })
    }

    /// The committed `hip_api_check.cc` is what this module generates. With
    /// `TB_WRITE_API_CHECK=1` the test rewrites it instead.
    #[test]
    fn api_check_is_fresh() {
        assert_fresh(
            "hip_api_check.cc",
            include_str!("../hip_api_check.cc"),
            &source(),
        );
    }

    #[test]
    fn every_status_is_a_value() {
        for s in [-1, i32::MIN, i32::MAX, 1, 2, 600, 701, 1052, 12_345] {
            assert!(!context_lost(s), "{s}");
        }
        for s in [4, 201, 700, 709, 719, 999] {
            assert!(context_lost(s), "{s}");
        }
        for s in 0..2000 {
            assert!(!poisons_process(s) || context_lost(s), "{s}");
        }
    }
}
