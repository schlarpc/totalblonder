//! The CUDA runtime: the part of the CUDA driver API the adapter uses, bound by hand and loaded
//! at run time.
//!
//! Thin own bindings instead of a crate (docs/ARCHITECTURE.md §5.2): about thirty functions of
//! an interface that has been stable since CUDA 4 (the `_v2` names), loaded with `dlopen` so
//! that the program builds and starts on a machine without a driver, and returning the
//! driver's status as a plain integer, so that a code this program does not know is still a
//! value (no enum, no transmute).
//!
//! The types, constants, record and function signatures below are written with the names of
//! `cuda.h`. They are checked against `cuda.h` of the pinned toolkit (CUDA 13.2, the one the
//! device image is built with): the test `api_check_is_fresh` generates a C++ file from these
//! lists (`cuda_api_check.cc` next to `Cargo.toml`), and the Nix check `kernels/cuda-api`
//! compiles that file against the toolkit's header. It asserts for every function that the
//! header's unversioned name resolves to the symbol loaded here (`cuMemAlloc` → `cuMemAlloc_v2`)
//! and that its type is exactly the one declared here, and for every constant and every field of
//! `CUDA_MEMCPY2D` its value, offset and size.
//!
//! Where statuses arrive on the card (RTX 2000 Ada, driver 595.71.05, `tests/plumbing.rs`):
//! - A launch the card cannot run (`k_me` with 1024 threads per block: more registers than a
//!   multiprocessor has) is refused by `cuLaunchKernel` itself with
//!   `CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES` (701): `Failed` at its step; the context stays usable.
//! - A kernel that faults on the device (a read far outside the arena) surfaces at the wait as
//!   `CUDA_ERROR_ILLEGAL_ADDRESS` (700), `Lost`. Such a fault poisons the process, as `cuda.h`
//!   says: after `cuDevicePrimaryCtxReset` the driver still refuses to retain the primary context
//!   or to create any other with status 700, so `restore` returns
//!   [`crate::GpuError::ProcessPoisoned`]. The kernel log gets an `Xid 31` (MMU fault) line for
//!   the faulting context; other processes on the card are not affected.
#![allow(
    non_snake_case,
    non_camel_case_types,
    reason = "names are those of cuda.h, so that the generated check can name both sides"
)]

use std::ffi::{CStr, c_char, c_float, c_int, c_uint, c_void};

use crate::KernelImage;
use crate::runtime::{Call, Handle, Runtime, Status, bindings, call, consts, record, sys};

/// `CUresult`: 0 is success. A plain integer: every value is valid.
pub type CUresult = c_int;
/// `CUdeviceptr`.
pub type CUdeviceptr = u64;
/// `CUdevice`.
pub type CUdevice = c_int;
/// `CUcontext`.
pub type CUcontext = *mut c_void;
/// `CUmodule`.
pub type CUmodule = *mut c_void;
/// `CUfunction`.
pub type CUfunction = *mut c_void;
/// `CUstream`.
pub type CUstream = *mut c_void;
/// `CUevent`.
pub type CUevent = *mut c_void;
/// `CUarray`.
pub type CUarray = *mut c_void;
/// `CUmemorytype` (a C enum: `int`-sized; the check asserts the size).
pub type CUmemorytype = c_uint;

consts! {
    /// Success.
    CUDA_SUCCESS: CUresult = 0;
    /// `cuStreamQuery` / `cuEventQuery`: the work is not finished.
    CUDA_ERROR_NOT_READY: CUresult = 600;
    /// The launch needs more registers, threads or shared memory than a multiprocessor has.
    /// Reported by `cuLaunchKernel` itself; the context stays usable.
    CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES: CUresult = 701;
    /// Lost-context statuses (see [`context_lost`]).
    CUDA_ERROR_DEINITIALIZED: CUresult = 4;
    /// See [`context_lost`].
    CUDA_ERROR_INVALID_CONTEXT: CUresult = 201;
    /// See [`context_lost`].
    CUDA_ERROR_ECC_UNCORRECTABLE: CUresult = 214;
    /// See [`context_lost`].
    CUDA_ERROR_NVLINK_UNCORRECTABLE: CUresult = 220;
    /// See [`context_lost`].
    CUDA_ERROR_CONTAINED: CUresult = 226;
    /// See [`context_lost`].
    CUDA_ERROR_ILLEGAL_ADDRESS: CUresult = 700;
    /// See [`context_lost`].
    CUDA_ERROR_LAUNCH_TIMEOUT: CUresult = 702;
    /// See [`context_lost`].
    CUDA_ERROR_CONTEXT_IS_DESTROYED: CUresult = 709;
    /// See [`context_lost`].
    CUDA_ERROR_ASSERT: CUresult = 710;
    /// See [`context_lost`].
    CUDA_ERROR_HARDWARE_STACK_ERROR: CUresult = 714;
    /// See [`context_lost`].
    CUDA_ERROR_ILLEGAL_INSTRUCTION: CUresult = 715;
    /// See [`context_lost`].
    CUDA_ERROR_MISALIGNED_ADDRESS: CUresult = 716;
    /// See [`context_lost`].
    CUDA_ERROR_INVALID_ADDRESS_SPACE: CUresult = 717;
    /// See [`context_lost`].
    CUDA_ERROR_INVALID_PC: CUresult = 718;
    /// See [`context_lost`].
    CUDA_ERROR_LAUNCH_FAILED: CUresult = 719;
    /// See [`context_lost`].
    CUDA_ERROR_TENSOR_MEMORY_LEAK: CUresult = 721;
    /// See [`context_lost`].
    CUDA_ERROR_MPS_CLIENT_TERMINATED: CUresult = 810;
    /// See [`context_lost`].
    CUDA_ERROR_EXTERNAL_DEVICE: CUresult = 911;
    /// See [`context_lost`].
    CUDA_ERROR_UNKNOWN: CUresult = 999;
    /// `CU_CTX_SCHED_BLOCKING_SYNC`: a thread that waits for the device sleeps.
    CU_CTX_SCHED_BLOCKING_SYNC: c_uint = 4;
    /// `CU_STREAM_NON_BLOCKING`: the stream does not synchronize with the default stream.
    CU_STREAM_NON_BLOCKING: c_uint = 1;
    /// `CU_EVENT_BLOCKING_SYNC`: `cuEventSynchronize` on this event sleeps instead of spinning.
    CU_EVENT_BLOCKING_SYNC: c_uint = 1;
    /// `CU_EVENT_DISABLE_TIMING`.
    CU_EVENT_DISABLE_TIMING: c_uint = 2;
    /// `CU_MEMORYTYPE_HOST`.
    CU_MEMORYTYPE_HOST: CUmemorytype = 1;
    /// `CU_MEMORYTYPE_DEVICE`.
    CU_MEMORYTYPE_DEVICE: CUmemorytype = 2;
}

/// Statuses after which the context cannot be used any more: `cuda.h` says for each of them
/// that the context "must be destroyed" or that "any further CUDA work will return the same
/// error", or that it is gone already. For most of the second kind the header adds that "the
/// process must be terminated and relaunched"; the adapter still tries to make the context
/// again ([`crate::GpuDevice::restore`]) and reports it if the driver refuses. Every other
/// status, including any this list does not know, is an error of one operation.
pub fn context_lost(s: CUresult) -> bool {
    matches!(
        s,
        CUDA_ERROR_DEINITIALIZED
            | CUDA_ERROR_INVALID_CONTEXT
            | CUDA_ERROR_ECC_UNCORRECTABLE
            | CUDA_ERROR_NVLINK_UNCORRECTABLE
            | CUDA_ERROR_CONTAINED
            | CUDA_ERROR_ILLEGAL_ADDRESS
            | CUDA_ERROR_LAUNCH_TIMEOUT
            | CUDA_ERROR_CONTEXT_IS_DESTROYED
            | CUDA_ERROR_ASSERT
            | CUDA_ERROR_HARDWARE_STACK_ERROR
            | CUDA_ERROR_ILLEGAL_INSTRUCTION
            | CUDA_ERROR_MISALIGNED_ADDRESS
            | CUDA_ERROR_INVALID_ADDRESS_SPACE
            | CUDA_ERROR_INVALID_PC
            | CUDA_ERROR_LAUNCH_FAILED
            | CUDA_ERROR_TENSOR_MEMORY_LEAK
            | CUDA_ERROR_MPS_CLIENT_TERMINATED
            | CUDA_ERROR_EXTERNAL_DEVICE
            | CUDA_ERROR_UNKNOWN
    )
}

/// The statuses of [`context_lost`] for which `cuda.h` adds that "the process must be
/// terminated and relaunched": faults on the device that the driver keeps for the process.
pub fn poisons_process(s: CUresult) -> bool {
    matches!(
        s,
        CUDA_ERROR_CONTAINED
            | CUDA_ERROR_ILLEGAL_ADDRESS
            | CUDA_ERROR_LAUNCH_TIMEOUT
            | CUDA_ERROR_HARDWARE_STACK_ERROR
            | CUDA_ERROR_ILLEGAL_INSTRUCTION
            | CUDA_ERROR_MISALIGNED_ADDRESS
            | CUDA_ERROR_INVALID_ADDRESS_SPACE
            | CUDA_ERROR_INVALID_PC
            | CUDA_ERROR_LAUNCH_FAILED
            | CUDA_ERROR_TENSOR_MEMORY_LEAK
            | CUDA_ERROR_MPS_CLIENT_TERMINATED
            | CUDA_ERROR_EXTERNAL_DEVICE
    )
}

record! {
    /// `CUDA_MEMCPY2D`.
    #[allow(missing_docs, reason = "the fields of CUDA_MEMCPY2D")]
    pub struct Memcpy2d as "CUDA_MEMCPY2D" {
        srcXInBytes: usize,
        srcY: usize,
        srcMemoryType: CUmemorytype,
        srcHost: *const c_void,
        srcDevice: CUdeviceptr,
        srcArray: CUarray,
        srcPitch: usize,
        dstXInBytes: usize,
        dstY: usize,
        dstMemoryType: CUmemorytype,
        dstHost: *mut c_void,
        dstDevice: CUdeviceptr,
        dstArray: CUarray,
        dstPitch: usize,
        WidthInBytes: usize,
        Height: usize,
    }
}

bindings! {
    status = CUresult;
    libraries = ["libcuda.so.1", "/run/opengl-driver/lib/libcuda.so.1"];
    fn cuInit(c_uint);
    fn cuGetErrorName(CUresult, *mut *const c_char);
    fn cuDeviceGet(*mut CUdevice, c_int);
    fn cuDeviceGetName(*mut c_char, c_int, CUdevice);
    fn cuDevicePrimaryCtxSetFlags_v2(CUdevice, c_uint);
    fn cuDevicePrimaryCtxRetain(*mut CUcontext, CUdevice);
    fn cuDevicePrimaryCtxRelease_v2(CUdevice);
    fn cuDevicePrimaryCtxReset_v2(CUdevice);
    fn cuCtxSetCurrent(CUcontext);
    fn cuModuleLoadData(*mut CUmodule, *const c_void);
    fn cuModuleUnload(CUmodule);
    fn cuModuleGetFunction(*mut CUfunction, CUmodule, *const c_char);
    fn cuMemAlloc_v2(*mut CUdeviceptr, usize);
    fn cuMemGetInfo_v2(*mut usize, *mut usize);
    fn cuMemFree_v2(CUdeviceptr);
    fn cuMemHostAlloc(*mut *mut c_void, usize, c_uint);
    fn cuMemFreeHost(*mut c_void);
    fn cuMemcpyHtoDAsync_v2(CUdeviceptr, *const c_void, usize, CUstream);
    fn cuMemcpyDtoHAsync_v2(*mut c_void, CUdeviceptr, usize, CUstream);
    fn cuMemcpyDtoDAsync_v2(CUdeviceptr, CUdeviceptr, usize, CUstream);
    fn cuMemcpy2DAsync_v2(*const Memcpy2d, CUstream);
    fn cuMemsetD8Async(CUdeviceptr, u8, usize, CUstream);
    fn cuLaunchKernel(
        CUfunction, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint, CUstream,
        *mut *mut c_void, *mut *mut c_void,
    );
    fn cuStreamCreate(*mut CUstream, c_uint);
    fn cuStreamDestroy_v2(CUstream);
    fn cuStreamSynchronize(CUstream);
    fn cuStreamQuery(CUstream);
    fn cuStreamWaitEvent(CUstream, CUevent, c_uint);
    fn cuEventCreate(*mut CUevent, c_uint);
    fn cuEventDestroy_v2(CUevent);
    fn cuEventRecord(CUevent, CUstream);
    fn cuEventQuery(CUevent);
    fn cuEventSynchronize(CUevent);
    fn cuEventElapsedTime_v2(*mut c_float, CUevent, CUevent);
}

/// The CUDA runtime: NVIDIA's driver library `libcuda.so.1`.
pub struct Cuda(Lib);

impl Runtime for Cuda {
    const NAME: &'static str = "CUDA";
    const IMAGE: KernelImage = KernelImage {
        image: include_bytes!(env!("TB_KERNEL_IMAGE_CUDA")),
        symbols: include_str!(env!("TB_KERNEL_SYMBOLS_CUDA")),
    };
    const XF_BLOCKS: u32 = tb_kernel_abi::XB as u32;
    const NOT_READY: Status = CUDA_ERROR_NOT_READY;
    const CONTEXT_IS_DESTROYED: Status = CUDA_ERROR_CONTEXT_IS_DESTROYED;
    fn context_lost(s: Status) -> bool {
        context_lost(s)
    }
    fn poisons_process(s: Status) -> bool {
        poisons_process(s)
    }
}

impl sys::Api for Cuda {
    fn load() -> Result<Self, String> {
        Lib::load().map(Self)
    }

    fn status_name(&self, s: Status) -> Option<String> {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: the out-pointer is to a local; on success the driver points it at a static
        // NUL-terminated string. Any status may be passed: an unknown one is refused with
        // CUDA_ERROR_INVALID_VALUE and `p` stays null.
        unsafe {
            ((self.0.cuGetErrorName)(s, &mut p) == CUDA_SUCCESS && !p.is_null())
                .then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
        }
    }

    unsafe fn init(&self, ordinal: i32) -> Result<i32, Call> {
        let mut device: CUdevice = 0;
        // SAFETY: cuInit is the first call; the out-pointer is to a local.
        unsafe {
            call("cuInit", (self.0.cuInit)(0))?;
            call("cuDeviceGet", (self.0.cuDeviceGet)(&mut device, ordinal))?;
        }
        Ok(device)
    }

    unsafe fn device_name(&self, device: i32) -> Option<String> {
        let mut name = [0 as c_char; 128];
        // SAFETY: `name` has the length passed (less one, so that it stays NUL-terminated);
        // CStr reads up to the NUL inside it.
        unsafe {
            ((self.0.cuDeviceGetName)(name.as_mut_ptr(), name.len() as c_int - 1, device)
                == CUDA_SUCCESS)
                .then(|| CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned())
        }
    }

    unsafe fn enter(&self, device: i32) -> Result<Handle, Call> {
        let mut ctx: CUcontext = std::ptr::null_mut();
        // SAFETY: plain driver calls with out-pointers to locals. The primary context's flags
        // are set before it is retained (they apply when the context is made; if another part
        // of the process already holds it, the call fails with PRIMARY_CONTEXT_ACTIVE and the
        // existing flags stay, which is accepted).
        unsafe {
            let _ = (self.0.cuDevicePrimaryCtxSetFlags_v2)(device, CU_CTX_SCHED_BLOCKING_SYNC);
            call(
                "cuDevicePrimaryCtxRetain",
                (self.0.cuDevicePrimaryCtxRetain)(&mut ctx, device),
            )?;
            if let Err(e) = call("cuCtxSetCurrent", (self.0.cuCtxSetCurrent)(ctx)) {
                let _ = (self.0.cuDevicePrimaryCtxRelease_v2)(device);
                return Err(e);
            }
        }
        Ok(ctx)
    }

    unsafe fn leave(&self, device: i32) {
        // SAFETY: the primary context was retained by `enter`.
        let _ = unsafe { (self.0.cuDevicePrimaryCtxRelease_v2)(device) };
    }

    unsafe fn reset(&self, device: i32) {
        // SAFETY: a device handle of `init`; the reset destroys the primary context with every
        // allocation in it.
        let _ = unsafe { (self.0.cuDevicePrimaryCtxReset_v2)(device) };
    }

    unsafe fn module_load(&self, module: &mut Handle, image: *const c_void) -> Status {
        // SAFETY: the caller's contract (`sys::Api`). The image is an ELF cubin whose length the driver reads from
        // its headers.
        unsafe { (self.0.cuModuleLoadData)(module, image) }
    }
    unsafe fn module_unload(&self, module: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuModuleUnload)(module) }
    }
    unsafe fn module_function(
        &self,
        f: &mut Handle,
        module: Handle,
        name: *const c_char,
    ) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuModuleGetFunction)(f, module, name) }
    }
    unsafe fn mem_alloc(&self, ptr: &mut u64, len: usize) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemAlloc_v2)(ptr, len) }
    }
    unsafe fn mem_free(&self, ptr: u64) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemFree_v2)(ptr) }
    }
    unsafe fn host_alloc(&self, ptr: &mut *mut c_void, len: usize) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemHostAlloc)(ptr, len, 0) }
    }
    unsafe fn host_free(&self, ptr: *mut c_void) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemFreeHost)(ptr) }
    }
    unsafe fn mem_info(&self, free: &mut usize, total: &mut usize) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemGetInfo_v2)(free, total) }
    }
    #[inline]
    unsafe fn memset_d8(&self, dst: u64, byte: u8, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemsetD8Async)(dst, byte, len, stream) }
    }
    #[inline]
    unsafe fn htod(&self, dst: u64, src: *const c_void, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemcpyHtoDAsync_v2)(dst, src, len, stream) }
    }
    #[inline]
    unsafe fn dtoh(&self, dst: *mut c_void, src: u64, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemcpyDtoHAsync_v2)(dst, src, len, stream) }
    }
    #[inline]
    unsafe fn dtod(&self, dst: u64, src: u64, len: usize, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuMemcpyDtoDAsync_v2)(dst, src, len, stream) }
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
        let c = Memcpy2d {
            srcXInBytes: 0,
            srcY: 0,
            srcMemoryType: CU_MEMORYTYPE_HOST,
            srcHost: src,
            srcDevice: 0,
            srcArray: std::ptr::null_mut(),
            srcPitch: src_pitch,
            dstXInBytes: 0,
            dstY: 0,
            dstMemoryType: CU_MEMORYTYPE_DEVICE,
            dstHost: std::ptr::null_mut(),
            dstDevice: dst,
            dstArray: std::ptr::null_mut(),
            dstPitch: dst_pitch,
            WidthInBytes: width,
            Height: rows,
        };
        // SAFETY: the caller's contract (`sys::Api`). The driver reads the descriptor during the call only.
        unsafe { (self.0.cuMemcpy2DAsync_v2)(&c, stream) }
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
        // SAFETY: the caller's contract (`sys::Api`). The driver copies the arguments during the call.
        unsafe {
            (self.0.cuLaunchKernel)(
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
            CU_STREAM_NON_BLOCKING
        } else {
            0
        };
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuStreamCreate)(stream, flags) }
    }
    unsafe fn stream_destroy(&self, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuStreamDestroy_v2)(stream) }
    }
    #[inline]
    unsafe fn stream_sync(&self, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuStreamSynchronize)(stream) }
    }
    #[inline]
    unsafe fn stream_query(&self, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuStreamQuery)(stream) }
    }
    #[inline]
    unsafe fn stream_wait_event(&self, stream: Handle, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuStreamWaitEvent)(stream, event, 0) }
    }
    unsafe fn event_create(&self, event: &mut Handle, blocking: bool, timing: bool) -> Status {
        let flags = if blocking { CU_EVENT_BLOCKING_SYNC } else { 0 }
            | if timing { 0 } else { CU_EVENT_DISABLE_TIMING };
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuEventCreate)(event, flags) }
    }
    unsafe fn event_destroy(&self, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuEventDestroy_v2)(event) }
    }
    #[inline]
    unsafe fn event_record(&self, event: Handle, stream: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuEventRecord)(event, stream) }
    }
    #[inline]
    unsafe fn event_query(&self, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuEventQuery)(event) }
    }
    #[inline]
    unsafe fn event_sync(&self, event: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuEventSynchronize)(event) }
    }
    unsafe fn event_elapsed(&self, ms: &mut f32, start: Handle, end: Handle) -> Status {
        // SAFETY: the caller's contract (`sys::Api`).
        unsafe { (self.0.cuEventElapsedTime_v2)(ms, start, end) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{CheckSpec, assert_fresh, check_source};

    /// The C++ source that checks this module against `cuda.h` (`cuda_api_check.cc`).
    fn source() -> String {
        check_source(&CheckSpec {
            module: "crates/tb-device-gpu/src/cuda.rs",
            test: "api_check_is_fresh",
            nix_check: "kernels/cuda-api",
            prelude: "",
            header: "cuda.h",
            status: "CUresult",
            functions: FUNCTIONS,
            constants: CONSTANTS,
            sizes: &[
                ("CUmemorytype", size_of::<CUmemorytype>()),
                ("CUresult", size_of::<CUresult>()),
            ],
            records: &[(Memcpy2d::C_NAME, size_of::<Memcpy2d>(), Memcpy2d::FIELDS)],
            types: &[("Memcpy2d", "CUDA_MEMCPY2D")],
        })
    }

    /// The committed `cuda_api_check.cc` is what this module generates. With
    /// `TB_WRITE_API_CHECK=1` the test rewrites it instead.
    #[test]
    fn api_check_is_fresh() {
        assert_fresh(
            "cuda_api_check.cc",
            include_str!("../cuda_api_check.cc"),
            &source(),
        );
    }

    #[test]
    fn every_status_is_a_value() {
        // Statuses the adapter does not know, negative ones and the extremes are errors of one
        // operation; none is undefined.
        for s in [-1, i32::MIN, i32::MAX, 1, 2, 600, 701, 12_345] {
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
