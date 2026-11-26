//! The part of the CUDA driver API the adapter uses, bound by hand and loaded at run time.
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
//! lists (`api_check.cc` next to `Cargo.toml`), and the Nix check `kernels/cuda-api` compiles
//! that file against the toolkit's header. It asserts for every function that the header's
//! unversioned name resolves to the symbol loaded here (`cuMemAlloc` → `cuMemAlloc_v2`) and
//! that its type is exactly the one declared here, and for every constant and every field of
//! `CUDA_MEMCPY2D` its value, offset and size.
#![allow(
    non_snake_case,
    non_camel_case_types,
    reason = "names are those of cuda.h, so that the generated check can name both sides"
)]

use std::ffi::{c_char, c_float, c_int, c_uint, c_void};

use libloading::Library;

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

macro_rules! consts {
    ($( $(#[$m:meta])* $name:ident : $t:ty = $v:expr; )*) => {
        $( $(#[$m])* pub const $name: $t = $v; )*
        /// Every constant above, by its `cuda.h` name, for the generated check.
        #[cfg(test)]
        pub const CONSTANTS: &[(&str, i64)] = &[ $( (stringify!($name), $v as i64) ),* ];
    };
}

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
/// again ([`crate::CudaDevice::restore`]) and reports it if the driver refuses. Every other
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

macro_rules! record {
    ($(#[$m:meta])* pub struct $name:ident as $cname:literal { $( $f:ident : $t:ty, )* }) => {
        $(#[$m])*
        #[repr(C)]
        pub struct $name { $( pub $f: $t, )* }
        impl $name {
            /// `(field, offset, size)` of every field, for the generated check.
            #[cfg(test)]
            pub const FIELDS: &[(&str, usize, usize)] =
                &[ $( (stringify!($f), std::mem::offset_of!($name, $f), size_of::<$t>()) ),* ];
            /// The C name of the record.
            #[cfg(test)]
            pub const C_NAME: &str = $cname;
        }
    };
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

macro_rules! api {
    ($( fn $name:ident($($t:ty),* $(,)?); )*) => {
        /// The loaded functions. The library stays loaded as long as this value lives.
        #[allow(missing_docs, reason = "each field is the cuda.h function of the same name")]
        pub struct Api {
            $( pub $name: unsafe extern "C" fn($($t),*) -> CUresult, )*
            _lib: Library,
        }

        /// Every function: its symbol and its parameter types as written above, for the
        /// generated check.
        #[cfg(test)]
        pub const FUNCTIONS: &[(&str, &[&str])] = &[ $( (stringify!($name), &[$(stringify!($t)),*]) ),* ];

        impl Api {
            /// Load the driver library and look up every function.
            pub fn load() -> Result<Self, String> {
                // The soname first (found through the loader's search path), then where NixOS
                // puts the driver's libraries.
                let names = ["libcuda.so.1", "/run/opengl-driver/lib/libcuda.so.1"];
                let mut errors = String::new();
                for name in names {
                    // SAFETY: loading libcuda runs its initializers, which have no
                    // preconditions; the library is NVIDIA's driver library or absent.
                    match unsafe { Library::new(name) } {
                        Ok(lib) => return Self::bind(lib),
                        Err(e) => errors.push_str(&format!("{name}: {e}; ")),
                    }
                }
                Err(errors)
            }

            fn bind(lib: Library) -> Result<Self, String> {
                $(
                    // SAFETY: the symbol is a function of the driver API with exactly this
                    // signature in cuda.h 13.2 (checked by the generated `api_check.cc`); the
                    // pointer is used only while `lib` lives, which `Api` guarantees by owning
                    // it.
                    let $name = unsafe {
                        lib.get::<unsafe extern "C" fn($($t),*) -> CUresult>(
                            concat!(stringify!($name), "\0").as_bytes(),
                        )
                    }
                    .map(|s| *s)
                    .map_err(|e| format!("{}: {e}", stringify!($name)))?;
                )*
                Ok(Self { $( $name, )* _lib: lib })
            }
        }
    };
}

api! {
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

/// The C++ source that checks this module against `cuda.h` (written to `api_check.cc`).
#[cfg(test)]
pub fn check_source() -> String {
    use std::fmt::Write;
    fn c_type(t: &str) -> String {
        let t = t.trim();
        if let Some(rest) = t.strip_prefix("*mut ") {
            return format!("{}*", c_type(rest));
        }
        if let Some(rest) = t.strip_prefix("*const ") {
            return format!("const {}*", c_type(rest));
        }
        match t {
            "c_uint" => "unsigned int",
            "c_int" => "int",
            "c_char" => "char",
            "c_void" => "void",
            "c_float" => "float",
            "usize" => "size_t",
            "u8" => "unsigned char",
            "Memcpy2d" => "CUDA_MEMCPY2D",
            other => other,
        }
        .to_owned()
    }
    let mut s = String::new();
    let w = &mut s;
    let _ = writeln!(
        w,
        "// Generated by crates/tb-device-cuda/src/api.rs (test api_check_is_fresh); do not edit.\n\
         // Compiled against cuda.h of the pinned toolkit by the Nix check kernels/cuda-api: it\n\
         // fails when a binding of the adapter differs from the header.\n\
         #include <cuda.h>\n#include <cstddef>\n#include <cstring>\n#include <type_traits>\n\n\
         #define TB_STR_(x) #x\n#define TB_STR(x) TB_STR_(x)\n\
         constexpr bool tb_eq(const char *a, const char *b) {{\n  \
         return *a == *b && (*a == 0 || tb_eq(a + 1, b + 1));\n}}\n"
    );
    let _ = writeln!(
        w,
        "// Functions: the unversioned name resolves to the symbol loaded, with this type."
    );
    for (name, params) in FUNCTIONS {
        let base = name
            .rsplit_once("_v")
            .filter(|(_, v)| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .map_or(*name, |(b, _)| b);
        let ps: Vec<String> = params.iter().map(|p| c_type(p)).collect();
        let _ = writeln!(
            w,
            "static_assert(tb_eq(TB_STR({base}), \"{name}\"), \"{base} is not {name} in cuda.h\");\n\
             static_assert(std::is_same<decltype(&{base}), CUresult (*)({})>::value, \"type of {name}\");",
            ps.join(", ")
        );
    }
    let _ = writeln!(w, "\n// Constants.");
    for (name, v) in CONSTANTS {
        let _ = writeln!(w, "static_assert((long long){name} == {v}LL, \"{name}\");");
    }
    let _ = writeln!(
        w,
        "static_assert(sizeof(CUmemorytype) == {}, \"CUmemorytype\");",
        size_of::<CUmemorytype>()
    );
    let _ = writeln!(
        w,
        "static_assert(sizeof(CUresult) == {}, \"CUresult\");",
        size_of::<CUresult>()
    );
    let _ = writeln!(w, "\n// Records.");
    let c = Memcpy2d::C_NAME;
    let _ = writeln!(
        w,
        "static_assert(sizeof({c}) == {}, \"{c}\");",
        size_of::<Memcpy2d>()
    );
    for (f, off, size) in Memcpy2d::FIELDS {
        let _ = writeln!(
            w,
            "static_assert(offsetof({c}, {f}) == {off} && sizeof((({c} *)0)->{f}) == {size}, \"{c}.{f}\");"
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed `api_check.cc` is what this module generates. With `TB_WRITE_API_CHECK=1`
    /// the test rewrites it instead (run from the crate's directory, as cargo does).
    #[test]
    fn api_check_is_fresh() {
        let want = check_source();
        if std::env::var_os("TB_WRITE_API_CHECK").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/api_check.cc");
            std::fs::write(path, &want).unwrap();
            return;
        }
        let have = include_str!("../api_check.cc");
        assert!(
            have == want,
            "crates/tb-device-cuda/api_check.cc is stale: run \
             TB_WRITE_API_CHECK=1 cargo test -p tb-device-cuda api_check_is_fresh"
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
