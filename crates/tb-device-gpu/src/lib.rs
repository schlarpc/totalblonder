//! The GPU adapter: executes device plans on an NVIDIA card through the CUDA driver API or on an
//! AMD card through HIP, with one implementation of the protocol over both.
//!
//! [`GpuDevice<R>`] is written once against the operations of a [`Runtime`] ([`Cuda`]:
//! `libcuda.so.1`; [`Hip`]: `libamdhip64.so`), which HIP mirrors nearly name for name. Both
//! runtimes are bound by hand and loaded at run time, so one binary runs on either vendor and
//! builds and starts without either. Each embeds its device image, built by Nix from the same
//! kernel sources (`nix/kernels.nix`: `nvcc -cubin` for sm_89; `hipcc` for the AMD targets of
//! `deviceImageHip`).
//!
//! The device: its primary context with blocking-sync scheduling, the kernel image, three
//! streams, two events, one block of device memory and one block of pinned host memory taken
//! when the adapter is made (the C++ `Encoder::Encoder` with `--overlap` and `--arena`).
//!
//! - `submit` validates the plan, queues every step in order and records a completion event of
//!   its own at the end of the plan's wait stream; it does not wait.
//! - Several plans may be in flight on different streams; each is waited for by its own event,
//!   so they complete in any order, and a later plan on the same stream does not delay the
//!   wait of an earlier one.
//! - `wait` is the hybrid wait of the C++ ([`WaitMode`], module `wait`).
//! - [`GpuDevice::set_timing`] brackets each plan with a pair of timing events on its wait
//!   stream; [`GpuDevice::last_device_time`] reports the device-side duration of the last plan
//!   waited for (the stage profile of the benchmarks).
//! - [`GpuDevice::restore`] makes the context's objects again after `DeviceError::Lost`,
//!   where the runtime allows it (not after a fault on the device: see below).
//!
//! # Errors
//!
//! Every runtime status is a value; none aborts. 0 is success; the statuses of the runtime's
//! `context_lost` ([`cuda::context_lost`], [`hip::context_lost`]) are `DeviceError::Lost` (the
//! context is unusable: every later submit returns `Lost` until [`GpuDevice::restore`]); every
//! other status, including statuses this crate has no name for, is `DeviceError::Failed` with
//! the code as the runtime gave it.
//!
//! A launch the card refuses is `Failed` at its step, reported at the wait after the stream has
//! drained; the context stays usable. A kernel that faults on the device surfaces at the wait as
//! `Lost`, and where the runtime keeps the fault for the process, [`GpuDevice::restore`]
//! returns [`GpuError::ProcessPoisoned`]. Where each status arrives on each vendor's card is
//! documented in the runtime's module and tested in `tests/plumbing.rs`.
//!
//! The adapter belongs to the thread that made it (the context is current there); it is not
//! `Send`.
//!
//! `unsafe` in this crate: the runtime calls. Each call site states why the handles and
//! addresses it passes are valid.
#![allow(unsafe_code, reason = "FFI adapter: the CUDA driver API and HIP")]

#[doc(hidden)]
pub mod cost;
pub mod cuda;
pub mod hip;
mod runtime;
mod wait;

use std::ffi::{CString, c_void};
use std::time::{Duration, Instant};

pub use cuda::Cuda;
pub use hip::Hip;
pub use runtime::Runtime;
use runtime::{Handle, Status};
use tb_device::plan::{EventId, Op, Plan, StreamId};
use tb_device::{ArenaSizes, Completion, Device, DeviceError, Kernel, Nanos, Ticket};
pub use wait::WaitMode;
use wait::{Schedule, WaitStats};

/// The adapter on an NVIDIA card.
pub type CudaDevice = GpuDevice<Cuda>;
/// The adapter on an AMD card.
pub type HipDevice = GpuDevice<Hip>;

/// Success, in both runtimes.
const SUCCESS: Status = 0;

/// A device image and the names of its kernels, as built by `nix/kernels.nix` (`deviceImage`,
/// `deviceImageHip`). The flake names the files in `TB_KERNEL_IMAGE_CUDA` /
/// `TB_KERNEL_SYMBOLS_CUDA` and `TB_KERNEL_IMAGE_HIP` / `TB_KERNEL_SYMBOLS_HIP` for this
/// package's build and in the dev shell; each runtime embeds its own ([`Runtime::IMAGE`]).
#[derive(Clone, Copy, Debug)]
pub struct KernelImage {
    /// The cubin (CUDA) or the code object or offload bundle (HIP).
    pub image: &'static [u8],
    /// One line per kernel: its name, a space, its (mangled) symbol in the image.
    pub symbols: &'static str,
}

/// Plans that may be in flight at once. More is a bug of the caller (a ticket never waited
/// for), reported as `DeviceError::BadPlan` by `submit`.
pub const MAX_IN_FLIGHT: usize = 64;

/// Why the adapter could not be made or made again.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GpuError {
    /// The runtime's library could not be loaded, or lacks a function.
    #[error("{runtime} runtime library: {detail}")]
    Driver {
        /// "CUDA" or "HIP".
        runtime: &'static str,
        /// What the loader said.
        detail: String,
    },
    /// A runtime call failed while the adapter was being made.
    #[error("{call} failed with status {status} ({name})")]
    Call {
        /// The runtime function.
        call: &'static str,
        /// Its status.
        status: i32,
        /// The status's name as the runtime gives it, or "unknown".
        name: String,
    },
    /// The image has no symbol for a kernel the protocol names.
    #[error("the kernel image has no kernel named {0}")]
    MissingKernel(&'static str),
    /// The runtime refuses to make any context in this process again: after a fault on the
    /// device (an illegal address, a trap, a launch failure) the driver keeps the status for
    /// the whole process ("the process must be terminated and relaunched", as `cuda.h` says).
    /// Only a new process can use the card.
    #[error("status {status} poisons this process: it must be restarted to use the card")]
    ProcessPoisoned {
        /// The status the runtime keeps returning.
        status: i32,
    },
}

/// Completion and timing events of one plan in flight. Made when first needed and kept for the
/// adapter's life, so that the steady state makes no driver objects.
#[derive(Clone, Copy)]
struct Slot {
    done: Handle,
    start: Handle,
    end: Handle,
    busy: bool,
}

struct InFlight {
    ticket: Ticket,
    /// The slot whose completion event follows the plan; `NO_SLOT` for a plan without steps,
    /// which is a wait for `stream` itself.
    slot: usize,
    stream: Handle,
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
    Event(Handle),
    /// A stream: everything queued on it (a plan without steps).
    Stream(Handle),
}

/// A GPU executing plans through runtime `R` ([`Cuda`] or [`Hip`]).
pub struct GpuDevice<R: Runtime> {
    api: R,
    device: i32,
    image: KernelImage,
    /// Whether the primary context is retained (released on teardown).
    retained: bool,
    /// The primary context while retained (null where the runtime hands none out).
    ctx: Handle,
    module: Handle,
    /// The function of each kernel of `Kernel::NAMES`.
    functions: Vec<Handle>,
    streams: [Handle; StreamId::COUNT],
    /// A stream outside the protocol, made on request for another user of the context.
    aux_stream: Handle,
    events: [Handle; EventId::COUNT],
    slots: Vec<Slot>,
    arenas: ArenaSizes,
    dev_base: u64,
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

impl<R: Runtime> std::fmt::Debug for GpuDevice<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuDevice")
            .field("runtime", &R::NAME)
            .field("name", &self.name)
            .field("arenas", &self.arenas)
            .field("lost", &self.lost)
            .finish()
    }
}

/// Status from the runtime: success, or the status as an error to report.
fn ok(s: Status) -> Result<(), Status> {
    if s == SUCCESS { Ok(()) } else { Err(s) }
}

/// A status as a protocol error at `step`.
fn device_error<R: Runtime>(step: usize, status: Status) -> DeviceError {
    if R::context_lost(status) {
        DeviceError::Lost { code: status }
    } else {
        DeviceError::Failed { step, code: status }
    }
}

impl<R: Runtime> GpuDevice<R> {
    /// Make an adapter on device `ordinal` of runtime `R` with arenas of these sizes and the
    /// kernels of `image` (normally `&R::IMAGE`). The device arena is cleared.
    pub fn new(ordinal: i32, arenas: ArenaSizes, image: &KernelImage) -> Result<Self, GpuError> {
        let api = R::load().map_err(|detail| GpuError::Driver {
            runtime: R::NAME,
            detail,
        })?;
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
        // SAFETY: the runtime's first calls; out-pointers are to locals inside `init`.
        let device = unsafe { this.api.init(ordinal) }.map_err(|c| this.call_error(c))?;
        this.device = device;
        // SAFETY: `device` is the handle `init` returned.
        this.name = unsafe { this.api.device_name(device) }.unwrap_or_default();
        // On an error `this` is dropped, which frees what `setup` made so far.
        this.setup()?;
        tracing::info!(
            runtime = R::NAME, device = %this.name, device_bytes = arenas.device,
            host_bytes = arenas.host, kernels = this.functions.len(), "GPU device ready"
        );
        Ok(this)
    }

    /// The device's name as the runtime reports it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The runtime's name ("CUDA", "HIP").
    pub fn runtime(&self) -> &'static str {
        R::NAME
    }

    /// How `wait` waits (default [`WaitMode::Hybrid`]).
    pub fn set_wait_mode(&mut self, mode: WaitMode) {
        self.mode = mode;
    }

    /// Bracket every plan submitted from now on with timing events, for
    /// [`GpuDevice::last_device_time`]. Off by default (the events cost a little).
    pub fn set_timing(&mut self, on: bool) {
        self.timing = on;
    }

    /// The device-side duration of the last plan waited for that was submitted with timing on:
    /// from the start of its wait stream's first operation of the plan to the end of its last.
    pub fn last_device_time(&self) -> Option<(Ticket, Nanos)> {
        self.last_time
    }

    /// Free and total memory of the card in bytes, as the runtime reports them for this
    /// context: everything every process holds counts as used.
    pub fn mem_info(&self) -> Option<(u64, u64)> {
        let (mut free, mut total) = (0usize, 0usize);
        // SAFETY: out-pointers to locals; the context is current on this thread.
        let s = unsafe { self.api.mem_info(&mut free, &mut total) };
        (s == SUCCESS).then_some((free as u64, total as u64))
    }

    /// A stream of the adapter's context that no plan uses, for another user of the context
    /// (libavcodec's copies of decoder surfaces): created on the first call with the
    /// non-blocking flag, so that work on it neither waits for nor holds up the default stream,
    /// and destroyed with the adapter. The handle is the runtime's stream; it is invalid after
    /// [`GpuDevice::restore`] (ask again).
    pub fn aux_stream(&mut self) -> Result<*mut c_void, GpuError> {
        if self.aux_stream.is_null() {
            let mut s = std::ptr::null_mut();
            // SAFETY: the out-pointer is to a local; the context made in `setup` is current on
            // this thread (the type is not `Send`).
            let status = unsafe { self.api.stream_create(&mut s, true) };
            self.ck("stream create", status)?;
            self.aux_stream = s;
        }
        Ok(self.aux_stream)
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

    /// Whether the context was lost and [`GpuDevice::restore`] is needed.
    pub fn is_lost(&self) -> bool {
        self.lost.is_some()
    }

    /// Destroy the context's objects and make them again: after `DeviceError::Lost`, or at any
    /// time. The primary context is reset (every allocation of it is freed by the runtime, the
    /// pinned arena included), then made again with the same arena sizes and image. Plans in
    /// flight are forgotten (their tickets become unknown), the host arena is zeroed and may
    /// move, the device arena is cleared. If it fails, the adapter stays lost (no host arena,
    /// every submit `Lost`) and `restore` may be tried again.
    ///
    /// After a fault on the device this cannot succeed where the runtime keeps the fault for
    /// the process (CUDA: driver 595 returns 700 from `cuDevicePrimaryCtxRetain` and
    /// `cuCtxCreate` after `cuDevicePrimaryCtxReset`), and `restore` returns
    /// [`GpuError::ProcessPoisoned`]. The process must then be restarted; a restore succeeds
    /// after losses that do not poison the process (a context destroyed or reset by another
    /// part of the process) and on a healthy context.
    pub fn restore(&mut self) -> Result<(), GpuError> {
        tracing::warn!(runtime = R::NAME, lost = ?self.lost, "making the GPU context again");
        self.teardown(true);
        self.lost = Some(R::CONTEXT_IS_DESTROYED);
        self.setup()?;
        self.lost = None;
        Ok(())
    }

    fn status_name(&self, s: Status) -> String {
        self.api
            .status_name(s)
            .unwrap_or_else(|| "unknown".to_owned())
    }

    fn call_error(&self, c: runtime::Call) -> GpuError {
        if R::poisons_process(c.status) {
            GpuError::ProcessPoisoned { status: c.status }
        } else {
            GpuError::Call {
                call: c.call,
                status: c.status,
                name: self.status_name(c.status),
            }
        }
    }

    fn ck(&self, call: &'static str, status: Status) -> Result<(), GpuError> {
        runtime::call(call, status).map_err(|c| self.call_error(c))
    }

    /// Enter the primary context, load the image, make streams, events and arenas. Every
    /// object made is recorded in `self` as soon as it exists, so that `teardown` frees
    /// exactly what was made, also after a failure half-way.
    fn setup(&mut self) -> Result<(), GpuError> {
        let null = std::ptr::null_mut();
        // SAFETY: `device` is the handle `init` returned; on success the context stays
        // entered until `teardown`.
        let ctx = unsafe { self.api.enter(self.device) }.map_err(|c| self.call_error(c))?;
        self.retained = true;
        self.ctx = ctx;
        let mut module = null;
        // SAFETY: the image is the runtime's own format (a cubin, a code object), which the
        // runtime reads up to the length its headers give; the out-pointer is to a local.
        let s = unsafe {
            self.api
                .module_load(&mut module, self.image.image.as_ptr().cast())
        };
        self.ck("module load", s)?;
        self.module = module;
        self.functions.clear();
        for kernel in Kernel::NAMES {
            let symbol = self
                .image
                .symbols
                .lines()
                .filter_map(|l| l.split_once(' '))
                .find_map(|(n, s)| (n == *kernel).then_some(s.trim()))
                .ok_or(GpuError::MissingKernel(kernel))?;
            let c = CString::new(symbol).map_err(|_| GpuError::MissingKernel(kernel))?;
            let mut f = null;
            // SAFETY: `module` is the module loaded above; `c` is NUL-terminated and lives
            // across the call.
            let s = unsafe { self.api.module_function(&mut f, self.module, c.as_ptr()) };
            if s != SUCCESS {
                return Err(GpuError::MissingKernel(kernel));
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
                self.ck("stream create", self.api.stream_create(&mut s, false))?;
                self.streams[i] = s;
            }
            for i in 0..EventId::COUNT {
                let mut e = null;
                self.ck("event create", self.api.event_create(&mut e, false, false))?;
                self.events[i] = e;
            }
            let dev_len = self.arenas.device.max(1) as usize;
            let mut base = 0;
            self.ck("device alloc", self.api.mem_alloc(&mut base, dev_len))?;
            self.dev_base = base;
            self.ck(
                "pinned host alloc",
                self.api
                    .host_alloc(&mut host, self.arenas.host.max(1) as usize),
            )?;
            self.host_base = host.cast();
            std::ptr::write_bytes(self.host_base, 0, self.arenas.host as usize);
            self.ck(
                "memset",
                self.api
                    .memset_d8(self.dev_base, 0, dev_len, self.streams[0]),
            )?;
            self.ck("stream synchronize", self.api.stream_sync(self.streams[0]))?;
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
                        let _ = api.stream_sync(s);
                    }
                }
            }
            if !self.host_base.is_null() {
                let _ = api.host_free(self.host_base.cast());
                self.host_base = std::ptr::null_mut();
            }
            if self.dev_base != 0 {
                let _ = api.mem_free(self.dev_base);
                self.dev_base = 0;
            }
            for slot in &self.slots {
                for e in [slot.done, slot.start, slot.end] {
                    if !e.is_null() {
                        let _ = api.event_destroy(e);
                    }
                }
            }
            for e in &mut self.events {
                if !e.is_null() {
                    let _ = api.event_destroy(*e);
                    *e = null;
                }
            }
            for s in self.streams.iter_mut().chain([&mut self.aux_stream]) {
                if !s.is_null() {
                    let _ = api.stream_destroy(*s);
                    *s = null;
                }
            }
            if !self.module.is_null() {
                let _ = api.module_unload(self.module);
                self.module = null;
            }
            if self.retained {
                api.leave(self.device);
                self.retained = false;
                self.ctx = null;
            }
            if reset {
                api.reset(self.device);
            }
        }
        self.slots.clear();
        self.functions.clear();
        self.in_flight.clear();
        self.foreign.clear();
    }

    /// A free slot with the events a plan needs, made if there is none yet.
    fn slot_for(&mut self, timed: bool) -> Result<usize, Status> {
        let i = match self.slots.iter().position(|s| !s.busy) {
            Some(i) => i,
            None => {
                let null = std::ptr::null_mut();
                let mut done = null;
                // SAFETY: out-pointer to a local. The completion event sleeps in a synchronize
                // (the context's blocking-sync flag covers stream waits, this flag the event's)
                // and keeps no time stamp.
                ok(unsafe { self.api.event_create(&mut done, true, false) })?;
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
            // SAFETY: out-pointers to locals; timing events. Each is stored as soon as it
            // exists, so that teardown destroys it.
            unsafe {
                ok(self.api.event_create(&mut start, false, true))?;
                self.slots[i].start = start;
                ok(self.api.event_create(&mut end, false, true))?;
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
            // the handles made in `setup`. The runtime copies kernel arguments and the 2-D copy
            // descriptor during the call, so pointers into `args` need to live only across it.
            let status = unsafe {
                match step.op {
                    Op::Fill { dst, byte } => {
                        api.memset_d8(self.dev_base + dst.off, byte, dst.len as usize, stream)
                    }
                    // The handles were resolved in place by `submit_keyed` before anything was
                    // queued: from here it is a copy like any other.
                    Op::Upload { dst, src } | Op::UploadRefs { dst, src, .. } => api.htod(
                        self.dev_base + dst.off,
                        self.host_base.add(src.off as usize).cast(),
                        src.len as usize,
                        stream,
                    ),
                    Op::Copy { dst, src } => api.dtod(
                        self.dev_base + dst.off,
                        self.dev_base + src.off,
                        src.len as usize,
                        stream,
                    ),
                    Op::Download { dst, src } => api.dtoh(
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
                    } => api.htod_2d(
                        self.dev_base + dst.off,
                        dst_pitch as usize,
                        self.host_base.add(src.off as usize).cast(),
                        src_pitch as usize,
                        width as usize,
                        rows as usize,
                        stream,
                    ),
                    Op::Launch(l) => {
                        let mut args = l.kernel.resolve(self.dev_base);
                        let mut ptrs = args.pointers();
                        api.launch(
                            self.functions[l.kernel.index()],
                            l.grid,
                            l.block,
                            stream,
                            ptrs.as_mut_ptr(),
                        )
                    }
                    Op::Record(e) => api.event_record(self.events[usize::from(e.0)], stream),
                    Op::WaitFor(e) => api.stream_wait_event(stream, self.events[usize::from(e.0)]),
                }
            };
            if status != SUCCESS {
                return Err(device_error::<R>(i, status));
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
            // costs a round trip to the device before it reads as complete (measured on CUDA:
            // about 30 us per tick, 0.5 to 1 % of a free-running tick); asking the stream
            // itself returns at once when it is idle, as the C++ `cudaStreamSynchronize` does.
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
        let slot = self.slot_for(timed).map_err(|s| device_error::<R>(0, s))?;
        let Slot {
            done, start, end, ..
        } = self.slots[slot];
        let wait_stream = self.streams[usize::from(plan.wait.0)];
        let n = plan.steps.len();
        let mut failed = if timed {
            // SAFETY: the events are the slot's, made by `slot_for` (start and end when
            // `timed`); the stream is one of `setup`'s.
            let s = unsafe { self.api.event_record(start, wait_stream) };
            ok(s).err().map(|s| device_error::<R>(0, s))
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
                    let s = self.api.event_record(end, wait_stream);
                    if s != SUCCESS {
                        s
                    } else {
                        self.api.event_record(done, wait_stream)
                    }
                } else {
                    self.api.event_record(done, wait_stream)
                }
            };
            if let Err(s) = ok(s) {
                failed = failed.or(Some(device_error::<R>(n, s)));
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
    ) -> Result<Status, ()> {
        let query = |api: &R| {
            // SAFETY: an event is a slot's completion event, made in `slot_for` and recorded
            // by `submit_keyed`; a stream is one of `setup`'s. Both live until teardown, which
            // takes `&mut self`.
            unsafe {
                match target {
                    Target::Event(done) => api.event_query(done),
                    Target::Stream(s) => api.stream_query(s),
                }
            }
        };
        let block = |api: &R| {
            // SAFETY: as for `query`.
            unsafe {
                match target {
                    Target::Event(done) => api.event_sync(done),
                    Target::Stream(s) => api.stream_sync(s),
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
                    if s != R::NOT_READY {
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
                    if s != R::NOT_READY {
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
        if status == SUCCESS {
            self.stats.update(class, secs(t0));
        }
        Ok(status)
    }
}

impl CudaDevice {
    /// The CUDA context the adapter works in (the device's primary context), as the driver's
    /// `CUcontext` handle, for code that must put its own work into the same context (the
    /// decoders: two contexts time-slice even inside one process). Null after a failed restore.
    pub fn context(&self) -> *mut c_void {
        self.ctx
    }
}

impl<R: Runtime> Device for GpuDevice<R> {
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
        // `&mut self`. The runtime writes to it only for downloads of plans in flight; the
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
            SUCCESS
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
            (None, SUCCESS) => Ok(()),
            // An asynchronous error surfaces at the wait; which step caused it is not known,
            // so it is reported at the end of the plan.
            (None, s) => Err(device_error::<R>(steps, s)),
        };
        if let Err(DeviceError::Lost { code }) = status {
            self.lost = Some(code);
        }
        if status.is_ok() && timed {
            let mut ms: f32 = 0.0;
            let Slot { start, end, .. } = self.slots[slot];
            // SAFETY: both are timing events of this slot, recorded around the plan, which has
            // completed (its completion event follows `end` on the same stream).
            if unsafe { self.api.event_elapsed(&mut ms, start, end) } == SUCCESS {
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

impl<R: Runtime> Drop for GpuDevice<R> {
    fn drop(&mut self) {
        self.teardown(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_map_to_errors() {
        for lost in [700, 709] {
            assert_eq!(
                device_error::<Cuda>(3, lost),
                DeviceError::Lost { code: lost }
            );
            assert_eq!(
                device_error::<Hip>(3, lost),
                DeviceError::Lost { code: lost }
            );
        }
        assert_eq!(
            device_error::<Cuda>(3, 701),
            DeviceError::Failed { step: 3, code: 701 }
        );
        assert_eq!(
            device_error::<Hip>(3, 701),
            DeviceError::Failed { step: 3, code: 701 }
        );
        // Codes the adapter has no name for stay values.
        for code in [-7, 123_456] {
            assert_eq!(
                device_error::<Cuda>(0, code),
                DeviceError::Failed { step: 0, code }
            );
            assert_eq!(
                device_error::<Hip>(0, code),
                DeviceError::Failed { step: 0, code }
            );
        }
    }

    fn no_driver_is_an_error_value<R: Runtime>() {
        // Where the runtime's library is missing (the sandbox), making the adapter fails
        // without a panic; where it is present, it may succeed or fail for want of a device.
        match GpuDevice::<R>::new(0, tb_device::selftest::ARENAS, &R::IMAGE) {
            Ok(_) | Err(GpuError::Driver { .. } | GpuError::Call { .. }) => {}
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn no_cuda_driver_is_an_error_value() {
        no_driver_is_an_error_value::<Cuda>();
    }

    #[test]
    fn no_hip_runtime_is_an_error_value() {
        no_driver_is_an_error_value::<Hip>();
    }
}
