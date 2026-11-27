//! What the adapter needs of a GPU runtime, and the machinery both runtimes' bindings share.
//!
//! The adapter ([`crate::GpuDevice`]) is written once against [`sys::Api`]: about thirty
//! operations of the CUDA driver API, which HIP mirrors nearly name for name (`cuMemcpyHtoDAsync`
//! / `hipMemcpyHtoDAsync`, `cuLaunchKernel` / `hipModuleLaunchKernel`, ...). Each runtime binds
//! its own library by hand ([`crate::cuda`], [`crate::hip`]) and implements the trait with one
//! forwarding call per operation, so a monomorphized adapter makes exactly the driver calls it
//! made when it knew only CUDA.
//!
//! Statuses are plain integers in both runtimes (0 is success; HIP numbers its errors after the
//! CUDA runtime's). A status a runtime has no name for is still a value: no enum, no transmute.
//!
//! The bindings are checked against the runtime's header: each runtime's module generates a C++
//! file of static assertions from its binding lists (`cuda_api_check.cc`, `hip_api_check.cc`,
//! kept fresh by a test of this crate) that the Nix checks `kernels/cuda-api` and
//! `kernels/hip-api` compile against the pinned toolkit's header.

use std::ffi::{c_char, c_void};

use crate::KernelImage;

/// A runtime status: 0 is success; every other value is an error, named or not.
pub type Status = i32;

/// A handle of the runtime (stream, event, module, function, context): an opaque pointer in
/// both runtimes.
pub type Handle = *mut c_void;

/// A failed call while the adapter is being made: which function and its status.
#[derive(Clone, Copy, Debug)]
pub struct Call {
    /// The runtime function.
    pub call: &'static str,
    /// Its status.
    pub status: Status,
}

/// Checks a status of a call made while setting up: success or the call that failed.
pub(crate) fn call(call: &'static str, status: Status) -> Result<(), Call> {
    if status == 0 {
        Ok(())
    } else {
        Err(Call { call, status })
    }
}

/// A GPU runtime the adapter can run on: [`crate::Cuda`] or [`crate::Hip`].
///
/// Sealed: the operations are this crate's own (`sys::Api`).
pub trait Runtime: sys::Api + 'static {
    /// The runtime's name for messages ("CUDA", "HIP").
    const NAME: &'static str;
    /// The device image this crate was built with for this runtime.
    const IMAGE: KernelImage;
    /// The status that reads "not finished yet" from a stream or event query.
    const NOT_READY: Status;
    /// The status `restore` records while the context is being made again.
    const CONTEXT_IS_DESTROYED: Status;
    /// Whether after `s` the context cannot be used any more (`DeviceError::Lost`).
    fn context_lost(s: Status) -> bool;
    /// Whether `s` is a fault the runtime keeps for the whole process: no context can be made
    /// again until the process is restarted.
    fn poisons_process(s: Status) -> bool;
}

pub(crate) mod sys {
    use super::{Call, Handle, Status, c_char, c_void};

    /// The operations of a runtime, as the adapter uses them. Every method is one call into the
    /// runtime with the arguments as given; device addresses are integers in both runtimes
    /// (HIP's `hipDeviceptr_t` is a pointer: the implementation converts).
    ///
    /// # Safety (all methods)
    ///
    /// The caller passes handles made by this runtime and still alive, device addresses and
    /// lengths inside allocations of the current context, host pointers valid for the lengths
    /// given (pinned where the runtime requires it for asynchronous copies), out-pointers to
    /// writable places, and makes the calls on the thread whose context is current.
    #[allow(
        clippy::missing_safety_doc,
        reason = "the trait's documentation states it"
    )]
    #[allow(
        missing_docs,
        reason = "each method is the runtime function its name says"
    )]
    pub trait Api: Sized {
        /// Load the runtime's library and look up every function.
        fn load() -> Result<Self, String>;
        /// The status's name as the runtime gives it, if it has one.
        fn status_name(&self, s: Status) -> Option<String>;
        /// Initialize the runtime and find device `ordinal`; returns the device's handle.
        unsafe fn init(&self, ordinal: i32) -> Result<i32, Call>;
        /// The device's name.
        unsafe fn device_name(&self, device: i32) -> Option<String>;
        /// Make the device's primary context current on this thread, with blocking-sync
        /// scheduling where it is not yet active. Returns the context's handle, or null where
        /// the runtime does not hand one out.
        unsafe fn enter(&self, device: i32) -> Result<Handle, Call>;
        /// Let go of what `enter` took. Statuses are ignored (the context may be lost).
        unsafe fn leave(&self, device: i32);
        /// Destroy the device's primary context and everything in it (after `leave`, or when
        /// `enter` never succeeded). Statuses are ignored.
        unsafe fn reset(&self, device: i32);
        unsafe fn module_load(&self, module: &mut Handle, image: *const c_void) -> Status;
        unsafe fn module_unload(&self, module: Handle) -> Status;
        unsafe fn module_function(
            &self,
            function: &mut Handle,
            module: Handle,
            name: *const c_char,
        ) -> Status;
        unsafe fn mem_alloc(&self, ptr: &mut u64, len: usize) -> Status;
        unsafe fn mem_free(&self, ptr: u64) -> Status;
        /// Pinned host memory.
        unsafe fn host_alloc(&self, ptr: &mut *mut c_void, len: usize) -> Status;
        unsafe fn host_free(&self, ptr: *mut c_void) -> Status;
        unsafe fn mem_info(&self, free: &mut usize, total: &mut usize) -> Status;
        unsafe fn memset_d8(&self, dst: u64, byte: u8, len: usize, stream: Handle) -> Status;
        unsafe fn htod(&self, dst: u64, src: *const c_void, len: usize, stream: Handle) -> Status;
        unsafe fn dtoh(&self, dst: *mut c_void, src: u64, len: usize, stream: Handle) -> Status;
        unsafe fn dtod(&self, dst: u64, src: u64, len: usize, stream: Handle) -> Status;
        /// Host to device, `rows` rows of `width` bytes at the pitches given.
        #[allow(clippy::too_many_arguments, reason = "the copy's own parameters")]
        unsafe fn htod_2d(
            &self,
            dst: u64,
            dst_pitch: usize,
            src: *const c_void,
            src_pitch: usize,
            width: usize,
            rows: usize,
            stream: Handle,
        ) -> Status;
        /// A one-dimensional launch: `grid` blocks of `block` threads, no dynamic shared memory,
        /// `args` the kernel's argument pointers.
        unsafe fn launch(
            &self,
            function: Handle,
            grid: u32,
            block: u32,
            stream: Handle,
            args: *mut *mut c_void,
        ) -> Status;
        unsafe fn stream_create(&self, stream: &mut Handle, non_blocking: bool) -> Status;
        unsafe fn stream_destroy(&self, stream: Handle) -> Status;
        unsafe fn stream_sync(&self, stream: Handle) -> Status;
        unsafe fn stream_query(&self, stream: Handle) -> Status;
        unsafe fn stream_wait_event(&self, stream: Handle, event: Handle) -> Status;
        /// `blocking`: a synchronize on the event sleeps; `timing`: it keeps a time stamp.
        unsafe fn event_create(&self, event: &mut Handle, blocking: bool, timing: bool) -> Status;
        unsafe fn event_destroy(&self, event: Handle) -> Status;
        unsafe fn event_record(&self, event: Handle, stream: Handle) -> Status;
        unsafe fn event_query(&self, event: Handle) -> Status;
        unsafe fn event_sync(&self, event: Handle) -> Status;
        unsafe fn event_elapsed(&self, ms: &mut f32, start: Handle, end: Handle) -> Status;
    }
}

/// Constants written with the runtime header's names: `pub const`s, and for the generated check
/// the list of all of them.
macro_rules! consts {
    ($( $(#[$m:meta])* $name:ident : $t:ty = $v:expr; )*) => {
        $( $(#[$m])* pub const $name: $t = $v; )*
        /// Every constant above, by its header name, for the generated check.
        #[cfg(test)]
        pub const CONSTANTS: &[(&str, i64)] = &[ $( (stringify!($name), $v as i64) ),* ];
    };
}
pub(crate) use consts;

/// The functions of a runtime library: a struct of function pointers that owns the library,
/// `load` (the names tried in order) and, for the generated check, the list of functions with
/// their parameter types as written.
macro_rules! bindings {
    (
        status = $status:ty;
        libraries = [$($lib:expr),* $(,)?];
        $( fn $name:ident($($t:ty),* $(,)?); )*
    ) => {
        /// The loaded functions. The library stays loaded as long as this value lives.
        #[allow(missing_docs, reason = "each field is the header's function of the same name")]
        pub struct Lib {
            $( pub $name: unsafe extern "C" fn($($t),*) -> $status, )*
            _lib: libloading::Library,
        }

        /// Every function: its symbol and its parameter types as written above, for the
        /// generated check.
        #[cfg(test)]
        pub const FUNCTIONS: &[(&str, &[&str])] =
            &[ $( (stringify!($name), &[$(stringify!($t)),*]) ),* ];

        impl Lib {
            /// Load the library (the soname first, through the loader's search path, then where
            /// NixOS puts the driver's libraries) and look up every function.
            pub fn load() -> Result<Self, String> {
                let mut errors = String::new();
                for name in [$($lib),*] {
                    // SAFETY: loading the runtime runs its initializers, which have no
                    // preconditions; the library is the vendor's runtime library or absent.
                    match unsafe { libloading::Library::new(name) } {
                        Ok(lib) => return Self::bind(lib),
                        Err(e) => errors.push_str(&format!("{name}: {e}; ")),
                    }
                }
                Err(errors)
            }

            fn bind(lib: libloading::Library) -> Result<Self, String> {
                $(
                    // SAFETY: the symbol is a function of the runtime with exactly this
                    // signature in the pinned header (checked by the generated file); the
                    // pointer is used only while `lib` lives, which `Lib` guarantees by owning
                    // it.
                    let $name = unsafe {
                        lib.get::<unsafe extern "C" fn($($t),*) -> $status>(
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
pub(crate) use bindings;

/// A `#[repr(C)]` record of a runtime header with, for the generated check, the offset and size
/// of every field.
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
pub(crate) use record;

/// The fields of a record: (name, offset, size).
#[cfg(test)]
pub type Fields<'a> = &'a [(&'a str, usize, usize)];

/// What a generated header check needs to know of one runtime's bindings.
#[cfg(test)]
pub struct CheckSpec<'a> {
    /// Path of the bindings' module, for the file's first line.
    pub module: &'a str,
    /// The test that regenerates the file and the Nix check that compiles it.
    pub test: &'a str,
    pub nix_check: &'a str,
    /// Preprocessor lines before the header (`#define`s the header needs).
    pub prelude: &'a str,
    pub header: &'a str,
    /// The C name of the status type.
    pub status: &'a str,
    pub functions: &'a [(&'a str, &'a [&'a str])],
    pub constants: &'a [(&'a str, i64)],
    /// C types whose size must equal Rust's: (C name, size).
    pub sizes: &'a [(&'a str, usize)],
    /// Records: (C name, size, fields).
    pub records: &'a [(&'a str, usize, Fields<'a>)],
    /// Rust type names (as written in the bindings) with their C spelling, beyond the common
    /// ones; a name not listed is used as written (the bindings use the header's type names).
    pub types: &'a [(&'a str, &'a str)],
}

/// The C++ source of static assertions that checks bindings against their header: for every
/// function that the header's unversioned name resolves to the symbol loaded (`cuMemAlloc` →
/// `cuMemAlloc_v2`) and that it has an overload of exactly the type bound; every constant's value; the size
/// of the types listed; every record's size and every field's offset and size.
#[cfg(test)]
pub fn check_source(spec: &CheckSpec<'_>) -> String {
    use std::fmt::Write;
    let c_type = |t: &str| -> String {
        fn go(t: &str, extra: &[(&str, &str)]) -> String {
            let t = t.trim();
            if let Some(rest) = t.strip_prefix("*mut ") {
                return format!("{}*", go(rest, extra));
            }
            if let Some(rest) = t.strip_prefix("*const ") {
                return format!("const {}*", go(rest, extra));
            }
            if let Some((_, c)) = extra.iter().find(|(r, _)| *r == t) {
                return (*c).to_owned();
            }
            match t {
                "c_uint" => "unsigned int",
                "c_int" => "int",
                "c_char" => "char",
                "c_void" => "void",
                "c_float" => "float",
                "usize" => "size_t",
                "u8" => "unsigned char",
                other => other,
            }
            .to_owned()
        }
        go(t, spec.types)
    };
    let mut s = String::new();
    let w = &mut s;
    let _ = writeln!(
        w,
        "// Generated by {} (test {}); do not edit.\n\
         // Compiled against {} of the pinned toolkit by the Nix check {}: it\n\
         // fails when a binding of the adapter differs from the header.\n\
         {}#include <{}>\n#include <cstddef>\n#include <cstring>\n#include <type_traits>\n\n\
         #define TB_STR_(x) #x\n#define TB_STR(x) TB_STR_(x)\n\
         constexpr bool tb_eq(const char *a, const char *b) {{\n  \
         return *a == *b && (*a == 0 || tb_eq(a + 1, b + 1));\n}}\n",
        spec.module, spec.test, spec.header, spec.nix_check, spec.prelude, spec.header
    );
    let _ = writeln!(
        w,
        "// Functions: the unversioned name resolves to the symbol loaded, with this type."
    );
    for (name, params) in spec.functions {
        let base = name
            .rsplit_once("_v")
            .filter(|(_, v)| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .map_or(*name, |(b, _)| b);
        let ps: Vec<String> = params.iter().map(|p| c_type(p)).collect();
        // The cast picks the overload of exactly this type (HIP's C++ header adds template
        // overloads of some functions) and does not compile when there is none.
        let ty = format!("{} (*)({})", spec.status, ps.join(", "));
        let _ = writeln!(
            w,
            "static_assert(tb_eq(TB_STR({base}), \"{name}\"), \"{base} is not {name} in {}\");\n\
             static_assert(std::is_same<decltype(static_cast<{ty}>(&{base})), {ty}>::value, \"type of {name}\");",
            spec.header,
        );
    }
    let _ = writeln!(w, "\n// Constants.");
    for (name, v) in spec.constants {
        let _ = writeln!(w, "static_assert((long long){name} == {v}LL, \"{name}\");");
    }
    for (name, size) in spec.sizes {
        let _ = writeln!(w, "static_assert(sizeof({name}) == {size}, \"{name}\");");
    }
    if !spec.records.is_empty() {
        let _ = writeln!(w, "\n// Records.");
    }
    for (c, size, fields) in spec.records {
        let _ = writeln!(w, "static_assert(sizeof({c}) == {size}, \"{c}\");");
        for (f, off, size) in *fields {
            let _ = writeln!(
                w,
                "static_assert(offsetof({c}, {f}) == {off} && sizeof((({c} *)0)->{f}) == {size}, \"{c}.{f}\");"
            );
        }
    }
    s
}

/// Asserts that the committed check file equals the generated one; with `TB_WRITE_API_CHECK=1`
/// rewrites it instead (run from the crate's directory, as cargo does).
#[cfg(test)]
pub fn assert_fresh(file: &str, have: &str, want: &str) {
    if std::env::var_os("TB_WRITE_API_CHECK").is_some() {
        let path = format!("{}/{file}", env!("CARGO_MANIFEST_DIR"));
        if let Err(e) = std::fs::write(&path, want) {
            panic!("{path}: {e}");
        }
        return;
    }
    assert!(
        have == want,
        "crates/tb-device-gpu/{file} is stale: run \
         TB_WRITE_API_CHECK=1 cargo test -p tb-device-gpu api_check_is_fresh"
    );
}
