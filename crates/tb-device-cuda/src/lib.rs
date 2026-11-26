//! The CUDA adapter.
//!
//! Executes device plans through the CUDA driver API: the device's primary context with
//! blocking-sync scheduling, the kernel image that Nix builds from the kernel sources
//! (`nvcc -cubin`, sm_89; embedded in this crate at compile time), three streams, two events,
//! one block of device memory and one block of pinned host memory taken when the adapter is
//! made (the C++ `Encoder::Encoder` with `--overlap` and `--arena`).
//!
//! - `submit` validates the plan, queues every step in order and records a completion event of
//!   its own at the end of the plan's wait stream; it does not wait.
//! - Several plans may be in flight on different streams; each is waited for by its own event,
//!   so they complete in any order, and a later plan on the same stream does not delay the
//!   wait of an earlier one.
//! - `wait` is the hybrid wait of the C++ ([`WaitMode`], module `wait`).
//! - [`CudaDevice::set_timing`] brackets each plan with a pair of timing events on its wait
//!   stream; [`CudaDevice::last_device_time`] reports the device-side duration of the last plan
//!   waited for (the stage profile of the benchmarks).
//! - [`CudaDevice::restore`] makes the context's objects again after `DeviceError::Lost`,
//!   where the driver allows it (not after a fault on the device: see below).
//!
//! # Errors
//!
//! Every driver status is a value; none aborts. `CUDA_SUCCESS` is success; the statuses of
//! [`api::context_lost`] are `DeviceError::Lost` (the context is unusable: every later submit
//! returns `Lost` until [`CudaDevice::restore`]); every other status, including statuses this
//! crate has no name for, is `DeviceError::Failed` with the code as the driver gave it.
//!
//! Where a status arrives (measured on the RTX 2000 Ada, driver 595.71.05, by the tests in
//! `tests/plumbing.rs`):
//! - A launch the card cannot run (`k_me` with 1024 threads per block: more registers than a
//!   multiprocessor has) is refused by `cuLaunchKernel` itself with
//!   `CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES` (701). The adapter stops queueing that plan there,
//!   still records its completion event, and reports `Failed { step, code: 701 }` at the wait,
//!   after the stream has drained. The context stays usable: the next plan runs normally.
//! - A kernel that faults on the device (a read far outside the arena) is not seen by any
//!   queueing call; it surfaces at the wait as `CUDA_ERROR_ILLEGAL_ADDRESS` (700), reported as
//!   `Lost { code: 700 }`, and every later submit returns `Lost`. Such a fault poisons the
//!   process, as `cuda.h` says: after `cuDevicePrimaryCtxReset` the driver still refuses to
//!   retain the primary context or to create any other with status 700, so
//!   [`CudaDevice::restore`] returns [`CudaError::ProcessPoisoned`] and only a new process can
//!   use the card (test `device_fault_is_lost_and_poisons_the_process`). The kernel log gets
//!   an `Xid 31` (MMU fault) line for the faulting context; other processes on the card are
//!   not affected.
//! - Nothing in the tests produced a status that is neither of these; any other status is
//!   `Failed` at its step, or at the end of the plan if it surfaces at the wait.
//!
//! The adapter belongs to the thread that made it (the context is current there); it is not
//! `Send`.
//!
//! `unsafe` in this crate: the driver calls. Each call site states why the handles and
//! addresses it passes are valid.
#![allow(unsafe_code, reason = "FFI adapter: the CUDA driver API")]

mod api;
#[doc(hidden)]
pub mod cost;
mod wait;

use std::ffi::{CString, c_char, c_int, c_void};
use std::time::{Duration, Instant};

use api::{Api, CUcontext, CUdevice, CUdeviceptr, CUevent, CUfunction, CUmodule, CUstream};
pub use api::{
    CUDA_ERROR_CONTEXT_IS_DESTROYED, CUDA_ERROR_ILLEGAL_ADDRESS,
    CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES, CUresult, context_lost,
};
use tb_device::plan::{EventId, Op, Plan, StreamId};
use tb_device::{ArenaSizes, Completion, Device, DeviceError, Kernel, Nanos, Ticket};
pub use wait::WaitMode;
use wait::{Schedule, WaitStats};

/// The device image and the names of its kernels, as built by `nix/kernels.nix`
/// (`deviceImage`). The flake names the two files in `TB_KERNEL_IMAGE` and `TB_KERNEL_SYMBOLS`
/// for this package's build and in the dev shell.
#[derive(Clone, Copy, Debug)]
pub struct KernelImage {
    /// The cubin.
    pub image: &'static [u8],
    /// One line per kernel: its name, a space, its (mangled) symbol in the image.
    pub symbols: &'static str,
}

/// The image this crate was built with.
pub const IMAGE: KernelImage = KernelImage {
    image: include_bytes!(env!("TB_KERNEL_IMAGE")),
    symbols: include_str!(env!("TB_KERNEL_SYMBOLS")),
};

/// Plans that may be in flight at once. More is a bug of the caller (a ticket never waited
/// for), reported as `DeviceError::BadPlan` by `submit`.
pub const MAX_IN_FLIGHT: usize = 64;

/// Why the adapter could not be made or made again.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CudaError {
    /// The driver library could not be loaded, or lacks a function.
    #[error("CUDA driver library: {0}")]
    Driver(String),
    /// A driver call failed while the adapter was being made.
    #[error("{call} failed with CUDA status {status} ({name})")]
    Call {
        /// The driver function.
        call: &'static str,
        /// Its status.
        status: i32,
        /// The status's name as the driver gives it (`cuGetErrorName`), or "unknown".
        name: String,
    },
    /// The image has no symbol for a kernel the protocol names.
    #[error("the kernel image has no kernel named {0}")]
    MissingKernel(&'static str),
    /// The driver refuses to make any context in this process again: after a fault on the
    /// device (an illegal address, a trap, a launch failure) the driver keeps the status for
    /// the whole process, as `cuda.h` says ("the process must be terminated and relaunched").
    /// Only a new process can use the card.
    #[error("CUDA status {status} poisons this process: it must be restarted to use the card")]
    ProcessPoisoned {
        /// The status the driver keeps returning.
        status: i32,
    },
}

/// Completion and timing events of one plan in flight. Made when first needed and kept for the
/// adapter's life, so that the steady state makes no driver objects.
#[derive(Clone, Copy)]
struct Slot {
    done: CUevent,
    start: CUevent,
    end: CUevent,
    busy: bool,
}

struct InFlight {
    ticket: Ticket,
    /// The slot whose completion event follows the plan; `NO_SLOT` for a plan without steps,
    /// which is a wait for `stream` itself.
    slot: usize,
    stream: CUstream,
    class: u64,
    steps: usize,
    timed: bool,
    submitted: Nanos,
    /// An error noticed while queueing: reported at the wait, after the stream has drained.
    failed: Option<DeviceError>,
}

/// [`InFlight::slot`] of a plan that has no completion event.
const NO_SLOT: usize = usize::MAX;

/// What a wait waits for.
#[derive(Clone, Copy)]
enum Target {
    /// A plan's completion event.
    Event(CUevent),
    /// A stream: everything queued on it (a plan without steps).
    Stream(CUstream),
}

/// A CUDA device executing plans.
pub struct CudaDevice {
    api: Api,
    device: CUdevice,
    image: KernelImage,
    /// Whether the primary context is retained (released on teardown).
    retained: bool,
    /// The primary context while retained.
    ctx: CUcontext,
    module: CUmodule,
    /// `CUfunction` of each kernel of `Kernel::NAMES`.
    functions: Vec<CUfunction>,
    streams: [CUstream; StreamId::COUNT],
    /// A stream outside the protocol, made on request for another user of the context.
    aux_stream: CUstream,
    events: [CUevent; EventId::COUNT],
    slots: Vec<Slot>,
    arenas: ArenaSizes,
    dev_base: CUdeviceptr,
    host_base: *mut u8,
    in_flight: Vec<InFlight>,
    /// Foreign device regions by slot: base address and length (length 0: not bound).
    foreign: Vec<(u64, u64)>,
    /// The context is unusable since this status; cleared by a successful `restore`.
    lost: Option<i32>,
    mode: WaitMode,
    stats: WaitStats,
    timing: bool,
    last_time: Option<(Ticket, Nanos)>,
    next_ticket: u64,
    epoch: Instant,
    name: String,
}

impl std::fmt::Debug for CudaDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CudaDevice")
            .field("name", &self.name)
            .field("arenas", &self.arenas)
            .field("lost", &self.lost)
            .finish()
    }
}

/// Status from the driver: success, or the status as an error to report.
fn ok(s: CUresult) -> Result<(), CUresult> {
    if s == api::CUDA_SUCCESS {
        Ok(())
    } else {
        Err(s)
    }
}

/// A status as a protocol error at `step`.
fn device_error(step: usize, status: CUresult) -> DeviceError {
    if context_lost(status) {
        DeviceError::Lost { code: status }
    } else {
        DeviceError::Failed { step, code: status }
    }
}

impl CudaDevice {
    /// Make an adapter on CUDA device `ordinal` with arenas of these sizes and the kernels of
    /// `image`. The device arena is cleared.
    pub fn new(ordinal: i32, arenas: ArenaSizes, image: &KernelImage) -> Result<Self, CudaError> {
        let api = Api::load().map_err(CudaError::Driver)?;
        let null = std::ptr::null_mut();
        let mut this = Self {
            api,
            device: 0,
            image: *image,
            retained: false,
            ctx: null,
            module: null,
            functions: Vec::with_capacity(Kernel::NAMES.len()),
            streams: [null; StreamId::COUNT],
            aux_stream: null,
            events: [null; EventId::COUNT],
            slots: Vec::with_capacity(MAX_IN_FLIGHT),
            arenas,
            dev_base: 0,
            host_base: std::ptr::null_mut(),
            in_flight: Vec::with_capacity(MAX_IN_FLIGHT),
            foreign: Vec::new(),
            lost: None,
            mode: WaitMode::default(),
            stats: WaitStats::default(),
            timing: false,
            last_time: None,
            next_ticket: 0,
            epoch: Instant::now(),
            name: String::new(),
        };
        let mut device: CUdevice = 0;
        // SAFETY: cuInit is the first call; the out-pointer is to a local.
        unsafe {
            this.ck("cuInit", (this.api.cuInit)(0))?;
            this.ck("cuDeviceGet", (this.api.cuDeviceGet)(&mut device, ordinal))?;
        }
        this.device = device;
        let mut name = [0 as c_char; 128];
        // SAFETY: `name` has the length passed (less one, so that it stays NUL-terminated);
        // CStr reads up to the NUL inside it.
        unsafe {
            if (this.api.cuDeviceGetName)(name.as_mut_ptr(), name.len() as c_int - 1, device)
                == api::CUDA_SUCCESS
            {
                this.name = std::ffi::CStr::from_ptr(name.as_ptr())
                    .to_string_lossy()
                    .into_owned();
            }
        }
        // On an error `this` is dropped, which frees what `setup` made so far.
        this.setup()?;
        tracing::info!(
            device = %this.name, device_bytes = arenas.device, host_bytes = arenas.host,
            kernels = this.functions.len(), "CUDA device ready"
        );
        Ok(this)
    }

    /// The device's name as the driver reports it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How `wait` waits (default [`WaitMode::Hybrid`]).
    pub fn set_wait_mode(&mut self, mode: WaitMode) {
        self.mode = mode;
    }

    /// Bracket every plan submitted from now on with timing events, for
    /// [`CudaDevice::last_device_time`]. Off by default (the events cost a little).
    pub fn set_timing(&mut self, on: bool) {
        self.timing = on;
    }

    /// The device-side duration of the last plan waited for that was submitted with timing on:
    /// from the start of its wait stream's first operation of the plan to the end of its last.
    pub fn last_device_time(&self) -> Option<(Ticket, Nanos)> {
        self.last_time
    }

    /// The CUDA context the adapter works in (the device's primary context), as the driver's
    /// `CUcontext` handle, for code that must put its own work into the same context (the
    /// decoders: two contexts time-slice even inside one process). Null after a failed restore.
    pub fn context(&self) -> *mut c_void {
        self.ctx.cast()
    }

    /// Free and total memory of the card in bytes, as the driver reports them for this
    /// context (`cuMemGetInfo`): everything every process holds counts as used.
    pub fn mem_info(&self) -> Option<(u64, u64)> {
        let (mut free, mut total) = (0usize, 0usize);
        // SAFETY: out-pointers to locals; the context is current on this thread.
        let s = unsafe { (self.api.cuMemGetInfo_v2)(&mut free, &mut total) };
        (s == api::CUDA_SUCCESS).then_some((free as u64, total as u64))
    }

    /// A stream of the adapter's context that no plan uses, for another user of the context
    /// (libavcodec's copies of decoder surfaces): created on the first call with the
    /// non-blocking flag, so that work on it neither waits for nor holds up the default stream,
    /// and destroyed with the adapter. The handle is the driver's `CUstream`; it is invalid
    /// after [`CudaDevice::restore`] (ask again).
    pub fn aux_stream(&mut self) -> Result<*mut c_void, CudaError> {
        if self.aux_stream.is_null() {
            let mut s = std::ptr::null_mut();
            // SAFETY: the out-pointer is to a local; the context made in `setup` is current on
            // this thread (the type is not `Send`).
            let status = unsafe { (self.api.cuStreamCreate)(&mut s, api::CU_STREAM_NON_BLOCKING) };
            self.ck("cuStreamCreate", status)?;
            self.aux_stream = s;
        }
        Ok(self.aux_stream.cast())
    }

    /// Bind device memory the adapter does not own (a decoder surface) to foreign slot `slot`,
    /// so that handle records of a plan can name it (`tb_device::kernel::DevRef::foreign`).
    /// A slot that is bound already is bound anew.
    ///
    /// # Safety
    /// `base .. base + len` must be device memory of this adapter's context, readable by
    /// kernels, and must stay so until the slot is bound to something else or unbound **and**
    /// every plan submitted while it was bound has completed.
    pub unsafe fn bind_foreign(&mut self, slot: u16, base: u64, len: u64) {
        let i = usize::from(slot);
        if self.foreign.len() <= i {
            self.foreign.resize(i + 1, (0, 0));
        }
        self.foreign[i] = (base, len);
    }

    /// Unbind every foreign slot. Plans already submitted keep the addresses they were given.
    pub fn unbind_foreign(&mut self) {
        self.foreign.clear();
    }

    /// Whether the context was lost and [`CudaDevice::restore`] is needed.
    pub fn is_lost(&self) -> bool {
        self.lost.is_some()
    }

    /// Destroy the context's objects and make them again: after `DeviceError::Lost`, or at any
    /// time. The primary context is reset (every allocation of it is freed by the driver, the
    /// pinned arena included), then made again with the same arena sizes and image. Plans in
    /// flight are forgotten (their tickets become unknown), the host arena is zeroed and may
    /// move, the device arena is cleared. If it fails, the adapter stays lost (no host arena,
    /// every submit `Lost`) and `restore` may be tried again.
    ///
    /// After a fault on the device this cannot succeed: driver 595 keeps the fault's status for
    /// the process (measured: `cuDevicePrimaryCtxRetain` and `cuCtxCreate` return 700 after
    /// `cuDevicePrimaryCtxReset`), and `restore` returns [`CudaError::ProcessPoisoned`]. The
    /// process must then be restarted; a restore succeeds after losses that do not poison the
    /// process (a context destroyed or reset by another part of the process) and on a healthy
    /// context.
    pub fn restore(&mut self) -> Result<(), CudaError> {
        tracing::warn!(lost = ?self.lost, "making the CUDA context again");
        self.teardown(true);
        self.lost = Some(api::CUDA_ERROR_CONTEXT_IS_DESTROYED);
        self.setup()?;
        self.lost = None;
        Ok(())
    }

    fn status_name(&self, s: CUresult) -> String {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: the out-pointer is to a local; on success the driver points it at a static
        // NUL-terminated string. Any status may be passed: an unknown one is refused with
        // CUDA_ERROR_INVALID_VALUE and `p` stays null.
        unsafe {
            if (self.api.cuGetErrorName)(s, &mut p) == api::CUDA_SUCCESS && !p.is_null() {
                return std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            }
        }
        "unknown".to_owned()
    }

    fn ck(&self, call: &'static str, status: CUresult) -> Result<(), CudaError> {
        ok(status).map_err(|status| {
            if api::poisons_process(status) {
                CudaError::ProcessPoisoned { status }
            } else {
                CudaError::Call {
                    call,
                    status,
                    name: self.status_name(status),
                }
            }
        })
    }

    /// Retain and enter the primary context, load the image, make streams, events and arenas.
    /// Every object made is recorded in `self` as soon as it exists, so that `teardown` frees
    /// exactly what was made, also after a failure half-way.
    fn setup(&mut self) -> Result<(), CudaError> {
        let null = std::ptr::null_mut();
        let mut ctx: CUcontext = null;
        // SAFETY: plain driver calls with out-pointers to locals and fields. The primary
        // context's flags are set before it is retained (they apply when the context is made;
        // if another part of the process already holds it, the call fails with
        // PRIMARY_CONTEXT_ACTIVE and the existing flags stay, which is accepted).
        unsafe {
            let _ = (self.api.cuDevicePrimaryCtxSetFlags_v2)(
                self.device,
                api::CU_CTX_SCHED_BLOCKING_SYNC,
            );
            self.ck(
                "cuDevicePrimaryCtxRetain",
                (self.api.cuDevicePrimaryCtxRetain)(&mut ctx, self.device),
            )?;
            self.retained = true;
            self.ctx = ctx;
            self.ck("cuCtxSetCurrent", (self.api.cuCtxSetCurrent)(ctx))?;
            let mut module = null;
            self.ck(
                "cuModuleLoadData",
                // The image is an ELF cubin whose length the driver reads from its headers.
                (self.api.cuModuleLoadData)(&mut module, self.image.image.as_ptr().cast()),
            )?;
            self.module = module;
        }
        self.functions.clear();
        for kernel in Kernel::NAMES {
            let symbol = self
                .image
                .symbols
                .lines()
                .filter_map(|l| l.split_once(' '))
                .find_map(|(n, s)| (n == *kernel).then_some(s.trim()))
                .ok_or(CudaError::MissingKernel(kernel))?;
            let c = CString::new(symbol).map_err(|_| CudaError::MissingKernel(kernel))?;
            let mut f = null;
            // SAFETY: `module` is the module loaded above; `c` is NUL-terminated and lives
            // across the call.
            let s = unsafe { (self.api.cuModuleGetFunction)(&mut f, self.module, c.as_ptr()) };
            if s != api::CUDA_SUCCESS {
                return Err(CudaError::MissingKernel(kernel));
            }
            self.functions.push(f);
        }
        let mut host: *mut c_void = null;
        // SAFETY: out-pointers to fields and locals; sizes are the arena sizes (at least one
        // byte each, so that the handles are never null); the host arena has `arenas.host`
        // bytes when it is zeroed; stream 0 exists before the fill queued on it.
        unsafe {
            for i in 0..StreamId::COUNT {
                let mut s = null;
                self.ck("cuStreamCreate", (self.api.cuStreamCreate)(&mut s, 0))?;
                self.streams[i] = s;
            }
            for i in 0..EventId::COUNT {
                let mut e = null;
                self.ck(
                    "cuEventCreate",
                    (self.api.cuEventCreate)(&mut e, api::CU_EVENT_DISABLE_TIMING),
                )?;
                self.events[i] = e;
            }
            let dev_len = self.arenas.device.max(1) as usize;
            let mut base = 0;
            self.ck("cuMemAlloc", (self.api.cuMemAlloc_v2)(&mut base, dev_len))?;
            self.dev_base = base;
            self.ck(
                "cuMemHostAlloc",
                (self.api.cuMemHostAlloc)(&mut host, self.arenas.host.max(1) as usize, 0),
            )?;
            self.host_base = host.cast();
            std::ptr::write_bytes(self.host_base, 0, self.arenas.host as usize);
            self.ck(
                "cuMemsetD8Async",
                (self.api.cuMemsetD8Async)(self.dev_base, 0, dev_len, self.streams[0]),
            )?;
            self.ck(
                "cuStreamSynchronize",
                (self.api.cuStreamSynchronize)(self.streams[0]),
            )?;
        }
        Ok(())
    }

    /// Free everything `setup` made; with `reset`, also reset the primary context (after a
    /// lost context, so that its broken state goes with it). Statuses are ignored: on a lost
    /// context the calls fail, and the reset frees what they could not.
    fn teardown(&mut self, reset: bool) {
        let null = std::ptr::null_mut();
        let api = &self.api;
        // SAFETY: every handle is either null (never made: skipped) or one `setup` or
        // `slot_for` made and nothing has freed. Streams are synchronized before the memory
        // they may still be using is freed. Each handle is set to null after it is freed.
        unsafe {
            if self.lost.is_none() {
                for s in self.streams {
                    if !s.is_null() {
                        let _ = (api.cuStreamSynchronize)(s);
                    }
                }
            }
            if !self.host_base.is_null() {
                let _ = (api.cuMemFreeHost)(self.host_base.cast());
                self.host_base = std::ptr::null_mut();
            }
            if self.dev_base != 0 {
                let _ = (api.cuMemFree_v2)(self.dev_base);
                self.dev_base = 0;
            }
            for slot in &self.slots {
                for e in [slot.done, slot.start, slot.end] {
                    if !e.is_null() {
                        let _ = (api.cuEventDestroy_v2)(e);
                    }
                }
            }
            for e in &mut self.events {
                if !e.is_null() {
                    let _ = (api.cuEventDestroy_v2)(*e);
                    *e = null;
                }
            }
            for s in self.streams.iter_mut().chain([&mut self.aux_stream]) {
                if !s.is_null() {
                    let _ = (api.cuStreamDestroy_v2)(*s);
                    *s = null;
                }
            }
            if !self.module.is_null() {
                let _ = (api.cuModuleUnload)(self.module);
                self.module = null;
            }
            if self.retained {
                let _ = (api.cuDevicePrimaryCtxRelease_v2)(self.device);
                self.retained = false;
                self.ctx = null;
            }
            if reset {
                let _ = (api.cuDevicePrimaryCtxReset_v2)(self.device);
            }
        }
        self.slots.clear();
        self.functions.clear();
        self.in_flight.clear();
        self.foreign.clear();
    }

    /// A free slot with the events a plan needs, made if there is none yet.
    fn slot_for(&mut self, timed: bool) -> Result<usize, CUresult> {
        let i = match self.slots.iter().position(|s| !s.busy) {
            Some(i) => i,
            None => {
                let null = std::ptr::null_mut();
                let mut done = null;
                // SAFETY: out-pointer to a local. The completion event sleeps in
                // cuEventSynchronize (the context's blocking-sync flag covers stream waits,
                // this flag the event's) and keeps no time stamp.
                ok(unsafe {
                    (self.api.cuEventCreate)(
                        &mut done,
                        api::CU_EVENT_BLOCKING_SYNC | api::CU_EVENT_DISABLE_TIMING,
                    )
                })?;
                self.slots.push(Slot {
                    done,
                    start: null,
                    end: null,
                    busy: false,
                });
                self.slots.len() - 1
            }
        };
        if timed && self.slots[i].start.is_null() {
            let (mut start, mut end) = (std::ptr::null_mut(), std::ptr::null_mut());
            // SAFETY: out-pointers to locals; flags 0: timing events. Each is stored as soon
            // as it exists, so that teardown destroys it.
            unsafe {
                ok((self.api.cuEventCreate)(&mut start, 0))?;
                self.slots[i].start = start;
                ok((self.api.cuEventCreate)(&mut end, 0))?;
                self.slots[i].end = end;
            }
        }
        self.slots[i].busy = true;
        Ok(i)
    }

    /// Queue the steps. Returns the first error with its step; earlier steps stay queued.
    fn queue(&mut self, plan: &Plan) -> Result<(), DeviceError> {
        let api = &self.api;
        for (i, step) in plan.steps.iter().enumerate() {
            let stream = self.streams[usize::from(step.stream.0)];
            // SAFETY: (all arms) the plan was validated against the arena sizes, so every
            // device address `dev_base + off .. + len` lies in the device allocation and every
            // host address in the pinned allocation, both owned by `self` and alive until
            // teardown (after all streams are synchronized). Streams, events and functions are
            // the handles made in `setup`. The driver copies kernel arguments and the 2-D copy
            // descriptor during the call, so pointers into `args` and `c` need to live only
            // across it.
            let status = unsafe {
                match step.op {
                    Op::Fill { dst, byte } => (api.cuMemsetD8Async)(
                        self.dev_base + dst.off,
                        byte,
                        dst.len as usize,
                        stream,
                    ),
                    // The handles were resolved in place by `submit_keyed` before anything was
                    // queued: from here it is a copy like any other.
                    Op::Upload { dst, src } | Op::UploadRefs { dst, src, .. } => (api
                        .cuMemcpyHtoDAsync_v2)(
                        self.dev_base + dst.off,
                        self.host_base.add(src.off as usize).cast(),
                        src.len as usize,
                        stream,
                    ),
                    Op::Copy { dst, src } => (api.cuMemcpyDtoDAsync_v2)(
                        self.dev_base + dst.off,
                        self.dev_base + src.off,
                        src.len as usize,
                        stream,
                    ),
                    Op::Download { dst, src } => (api.cuMemcpyDtoHAsync_v2)(
                        self.host_base.add(dst.off as usize).cast(),
                        self.dev_base + src.off,
                        src.len as usize,
                        stream,
                    ),
                    Op::Upload2d {
                        dst,
                        dst_pitch,
                        src,
                        src_pitch,
                        width,
                        rows,
                    } => {
                        let c = api::Memcpy2d {
                            srcXInBytes: 0,
                            srcY: 0,
                            srcMemoryType: api::CU_MEMORYTYPE_HOST,
                            srcHost: self.host_base.add(src.off as usize).cast(),
                            srcDevice: 0,
                            srcArray: std::ptr::null_mut(),
                            srcPitch: src_pitch as usize,
                            dstXInBytes: 0,
                            dstY: 0,
                            dstMemoryType: api::CU_MEMORYTYPE_DEVICE,
                            dstHost: std::ptr::null_mut(),
                            dstDevice: self.dev_base + dst.off,
                            dstArray: std::ptr::null_mut(),
                            dstPitch: dst_pitch as usize,
                            WidthInBytes: width as usize,
                            Height: rows as usize,
                        };
                        (api.cuMemcpy2DAsync_v2)(&c, stream)
                    }
                    Op::Launch(l) => {
                        let mut args = l.kernel.resolve(self.dev_base);
                        let mut ptrs = args.pointers();
                        (api.cuLaunchKernel)(
                            self.functions[l.kernel.index()],
                            l.grid,
                            1,
                            1,
                            l.block,
                            1,
                            1,
                            0,
                            stream,
                            ptrs.as_mut_ptr(),
                            std::ptr::null_mut(),
                        )
                    }
                    Op::Record(e) => (api.cuEventRecord)(self.events[usize::from(e.0)], stream),
                    Op::WaitFor(e) => {
                        (api.cuStreamWaitEvent)(stream, self.events[usize::from(e.0)], 0)
                    }
                }
            };
            if status != api::CUDA_SUCCESS {
                return Err(device_error(i, status));
            }
        }
        Ok(())
    }

    /// Queue a plan whose waits are accounted under `class` (0 to 255, the caller's numbering
    /// of its phases, as the C++ numbers its three waits) for the hybrid wait. `submit` keys on
    /// the plan's wait stream and length instead.
    pub fn submit_class(&mut self, plan: &Plan, class: u8) -> Result<Ticket, DeviceError> {
        self.submit_keyed(plan, wait::explicit_class(class))
    }

    fn submit_keyed(&mut self, plan: &Plan, class: u64) -> Result<Ticket, DeviceError> {
        if let Some(code) = self.lost {
            return Err(DeviceError::Lost { code });
        }
        plan.validate(self.arenas)?;
        if self.in_flight.len() >= MAX_IN_FLIGHT {
            return Err(DeviceError::BadPlan {
                step: 0,
                what: "too many plans in flight (tickets not waited for)",
            });
        }
        // Handle records become addresses, in the host arena, before anything is queued: a
        // handle that cannot be resolved rejects the plan whole.
        for (i, step) in plan.steps.iter().enumerate() {
            if let Op::UploadRefs { src, kind, .. } = step.op {
                let foreign = &self.foreign;
                let lookup = |slot: u16| {
                    foreign
                        .get(usize::from(slot))
                        .copied()
                        .filter(|&(_, len)| len > 0)
                };
                // SAFETY: the plan was validated, so `src` lies inside the pinned host arena,
                // which `self` owns; no other reference to the arena exists while `&mut self`
                // is held, and the protocol's rule keeps the device from reading this region
                // (it is named by no plan in flight).
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(
                        self.host_base.add(src.off as usize),
                        src.len as usize,
                    )
                };
                kind.resolve(bytes, (self.dev_base, self.arenas.device), &lookup)
                    .map_err(|what| DeviceError::BadPlan { step: i, what })?;
            }
        }
        let timed = self.timing;
        if plan.steps.is_empty() && !timed {
            // A plan without steps is a wait for its stream (the host's wait for the upload
            // stream at the start of a tick, a settle). An event recorded on an idle stream
            // costs a round trip to the device before it reads as complete (measured: about
            // 30 us per tick, 0.5 to 1 % of a free-running tick); asking the stream itself
            // returns at once when it is idle, as the C++ `cudaStreamSynchronize` does.
            let ticket = Ticket(self.next_ticket);
            self.next_ticket += 1;
            self.in_flight.push(InFlight {
                ticket,
                slot: NO_SLOT,
                stream: self.streams[usize::from(plan.wait.0)],
                class,
                steps: 0,
                timed: false,
                submitted: self.now(),
                failed: None,
            });
            return Ok(ticket);
        }
        let slot = self.slot_for(timed).map_err(|s| device_error(0, s))?;
        let Slot {
            done, start, end, ..
        } = self.slots[slot];
        let wait_stream = self.streams[usize::from(plan.wait.0)];
        let n = plan.steps.len();
        let mut failed = if timed {
            // SAFETY: the events are the slot's, made by `slot_for` (start and end when
            // `timed`); the stream is one of `setup`'s.
            let s = unsafe { (self.api.cuEventRecord)(start, wait_stream) };
            ok(s).err().map(|s| device_error(0, s))
        } else {
            None
        };
        if failed.is_none() {
            failed = self.queue(plan).err();
        }
        // The completion event goes behind whatever was queued, also after a failure, so that
        // the wait drains the stream before it reports. Not on a lost context: nothing runs.
        if !matches!(failed, Some(DeviceError::Lost { .. })) {
            // SAFETY: as above.
            let s = unsafe {
                if timed {
                    let s = (self.api.cuEventRecord)(end, wait_stream);
                    if s != api::CUDA_SUCCESS {
                        s
                    } else {
                        (self.api.cuEventRecord)(done, wait_stream)
                    }
                } else {
                    (self.api.cuEventRecord)(done, wait_stream)
                }
            };
            if let Err(s) = ok(s) {
                failed = failed.or(Some(device_error(n, s)));
            }
        }
        if let Some(e) = &failed {
            if let DeviceError::Lost { code } = e {
                self.lost = Some(*code);
            }
            if let DeviceError::Failed { code, .. } | DeviceError::Lost { code } = e {
                tracing::warn!(error = %e, status = %self.status_name(*code), "queueing a plan failed");
            }
        }
        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        self.in_flight.push(InFlight {
            ticket,
            slot,
            stream: wait_stream,
            class,
            steps: n,
            timed,
            submitted: self.now(),
            failed,
        });
        Ok(ticket)
    }

    /// Wait for a completion event or a stream in the current mode. `Err(())`: the timeout
    /// passed first.
    fn wait_event(
        &mut self,
        target: Target,
        class: u64,
        timeout: Option<Nanos>,
    ) -> Result<CUresult, ()> {
        let query = |api: &Api| {
            // SAFETY: an event is a slot's completion event, made in `slot_for` and recorded
            // by `submit_keyed`; a stream is one of `setup`'s. Both live until teardown, which
            // takes `&mut self`.
            unsafe {
                match target {
                    Target::Event(done) => (api.cuEventQuery)(done),
                    Target::Stream(s) => (api.cuStreamQuery)(s),
                }
            }
        };
        let block = |api: &Api| {
            // SAFETY: as for `query`.
            unsafe {
                match target {
                    Target::Event(done) => (api.cuEventSynchronize)(done),
                    Target::Stream(s) => (api.cuStreamSynchronize)(s),
                }
            }
        };
        let limit = timeout.map(|t| t.0 as f64 * 1e-9);
        let t0 = Instant::now();
        let secs = |t: Instant| t.elapsed().as_secs_f64();
        // Never panics: a negative or NaN length sleeps for nothing.
        let sleep = |d: f64| std::thread::sleep(Duration::try_from_secs_f64(d).unwrap_or_default());
        let pause = |left: f64| sleep(left.clamp(0.0, 50e-6));
        let status = match self.mode {
            WaitMode::Blocking => match limit {
                None => block(&self.api),
                Some(l) => loop {
                    let s = query(&self.api);
                    if s != api::CUDA_ERROR_NOT_READY {
                        break s;
                    }
                    if secs(t0) >= l {
                        return Err(());
                    }
                    pause(l - secs(t0));
                },
            },
            WaitMode::Hybrid => {
                let mut plan = Schedule::of(self.stats.ema(class));
                if matches!(target, Target::Stream(_)) {
                    // What is left of a stream's work at an arbitrary moment (the frame
                    // upload at the start of the next tick) varies from call to call, unlike
                    // the length of a phase: a nap sized by the average overshoots. Poll.
                    plan.nap = 0.0;
                }
                if plan.nap > 0.0 {
                    sleep(limit.map_or(plan.nap, |l| plan.nap.min(l)));
                }
                loop {
                    let s = query(&self.api);
                    if s != api::CUDA_ERROR_NOT_READY {
                        break s;
                    }
                    let el = secs(t0);
                    if limit.is_some_and(|l| el >= l) {
                        return Err(());
                    }
                    if el > plan.block_after {
                        // Not as expected: block (with a limit, sleep in short steps instead).
                        match limit {
                            None => break block(&self.api),
                            Some(l) => pause(l - el),
                        }
                    }
                }
            }
        };
        if status == api::CUDA_SUCCESS {
            self.stats.update(class, secs(t0));
        }
        Ok(status)
    }
}

impl Device for CudaDevice {
    fn arenas(&self) -> ArenaSizes {
        self.arenas
    }
    fn host(&self) -> &[u8] {
        if self.host_base.is_null() {
            // Only after a failed `restore`: there is no host arena.
            return &[];
        }
        // SAFETY: `host_base` is the pinned allocation of `arenas.host` bytes (at least one
        // byte was allocated), initialized in `setup`, alive until teardown, which takes
        // `&mut self`. The driver writes to it only for downloads of plans in flight; the
        // protocol forbids reading those regions before the completion (tb_device::Device).
        unsafe { std::slice::from_raw_parts(self.host_base, self.arenas.host as usize) }
    }
    fn host_mut(&mut self) -> &mut [u8] {
        if self.host_base.is_null() {
            return &mut [];
        }
        // SAFETY: as in `host`; `&mut self` makes this the only reference from Rust.
        unsafe { std::slice::from_raw_parts_mut(self.host_base, self.arenas.host as usize) }
    }
    fn now(&self) -> Nanos {
        Nanos(u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        self.submit_keyed(plan, wait::implicit_class(plan.wait.0, plan.steps.len()))
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        let Some(i) = self.in_flight.iter().position(|f| f.ticket == ticket) else {
            let t = self.now();
            return Completion {
                ticket,
                status: Err(DeviceError::UnknownTicket),
                submitted: t,
                finished: t,
            };
        };
        let f = &self.in_flight[i];
        let (slot, class, submitted, steps, timed) =
            (f.slot, f.class, f.submitted, f.steps, f.timed);
        let lost_at_queue = matches!(f.failed, Some(DeviceError::Lost { .. }));
        let status = if lost_at_queue {
            api::CUDA_SUCCESS
        } else {
            let target = if slot == NO_SLOT {
                Target::Stream(f.stream)
            } else {
                Target::Event(self.slots[slot].done)
            };
            match self.wait_event(target, class, timeout) {
                Ok(s) => s,
                Err(()) => {
                    return Completion {
                        ticket,
                        status: Err(DeviceError::Timeout),
                        submitted,
                        finished: self.now(),
                    };
                }
            }
        };
        let f = self.in_flight.swap_remove(i);
        if let Some(s) = self.slots.get_mut(slot) {
            s.busy = false;
        }
        let status = match (f.failed, status) {
            (Some(e), _) => Err(e),
            (None, api::CUDA_SUCCESS) => Ok(()),
            // An asynchronous error surfaces at the wait; which step caused it is not known,
            // so it is reported at the end of the plan.
            (None, s) => Err(device_error(steps, s)),
        };
        if let Err(DeviceError::Lost { code }) = status {
            self.lost = Some(code);
        }
        if status.is_ok() && timed {
            let mut ms: f32 = 0.0;
            let Slot { start, end, .. } = self.slots[slot];
            // SAFETY: both are timing events of this slot, recorded around the plan, which has
            // completed (its completion event follows `end` on the same stream).
            if unsafe { (self.api.cuEventElapsedTime_v2)(&mut ms, start, end) } == api::CUDA_SUCCESS
            {
                self.last_time = Some((ticket, Nanos((f64::from(ms) * 1e6).round() as u64)));
            }
        }
        Completion {
            ticket,
            status,
            submitted,
            finished: self.now(),
        }
    }
}

impl Drop for CudaDevice {
    fn drop(&mut self) {
        self.teardown(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_errors() {
        assert_eq!(
            device_error(3, 701),
            DeviceError::Failed { step: 3, code: 701 }
        );
        assert_eq!(device_error(3, 700), DeviceError::Lost { code: 700 });
        // Codes the adapter has no name for stay values.
        assert_eq!(
            device_error(0, -7),
            DeviceError::Failed { step: 0, code: -7 }
        );
        assert_eq!(
            device_error(0, 123_456),
            DeviceError::Failed {
                step: 0,
                code: 123_456
            }
        );
    }

    #[test]
    fn no_driver_is_an_error_value() {
        // Where libcuda is missing (the sandbox), making the adapter fails without a panic;
        // where it is present, it may succeed or fail for want of a device.
        match CudaDevice::new(0, tb_device::selftest::ARENAS, &IMAGE) {
            Ok(_) | Err(CudaError::Driver(_) | CudaError::Call { .. }) => {}
            Err(e) => panic!("{e}"),
        }
    }
}
