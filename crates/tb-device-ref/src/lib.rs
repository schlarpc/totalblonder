//! The CPU reference adapter.
//!
//! Executes device plans on the host: the two arenas are ordinary memory, copies are `memcpy`,
//! and a launch calls the kernel compiled from the same source as the device image
//! (`kernels/tb_kernels_ref.cc` with `kernels/shim/cuda_runtime.h`), block after block, thread
//! after thread. Everything is synchronous: `submit` runs the plan, `wait` returns its result.
//!
//! It exists so that the whole encoder runs in the Nix sandbox, without a GPU, on the kernels'
//! real arithmetic. It is slow (one host thread) and meant for small pictures.
//!
//! Streams and events need no model here: plans are executed in queue order, which satisfies
//! every ordering a plan can state (an event is recorded before anything waits for it, because
//! the core queues the record first).
//!
//! `unsafe` in this crate: the three calls into the kernel library and the raw copies inside
//! the arenas, each after the plan has been validated.
#![allow(unsafe_code, reason = "FFI adapter: the host build of the kernels")]

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

use tb_device::plan::{Op, Plan};
use tb_device::{ArenaSizes, Completion, Device, DeviceError, Kernel, Nanos, Ticket};

mod ffi {
    use super::{c_char, c_int, c_uint, c_void};
    // Declared in kernels/tb_kernels_ref.h.
    unsafe extern "C" {
        pub safe fn tbk_kernel_count() -> c_int;
        pub fn tbk_kernel_name(kernel: c_int) -> *const c_char;
        pub fn tbk_kernel_params(kernel: c_int, sizes: *mut c_uint, max: c_int) -> c_int;
        pub fn tbk_launch(
            kernel: c_int,
            grid: c_uint,
            block: c_uint,
            params: *mut *mut c_void,
        ) -> c_int;
        pub safe fn tbk_sanitized() -> c_int;
        pub safe fn tbk_test_kernel_count() -> c_int;
        pub fn tbk_test_launch(
            kernel: c_int,
            grid: c_uint,
            block: c_uint,
            out: *mut u32,
            arg: c_int,
        ) -> c_int;
    }
}

/// Why the adapter could not be made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    /// The kernel library lacks a kernel the protocol names.
    #[error("the host kernel library has no kernel named {0}")]
    MissingKernel(&'static str),
    /// A kernel's parameters in the library differ from the protocol's record.
    #[error(
        "kernel {name}: the library takes parameters of sizes {lib:?}, the protocol passes {ours:?}"
    )]
    Signature {
        /// The kernel.
        name: &'static str,
        /// Sizes of the C parameters.
        lib: Vec<usize>,
        /// Sizes of the values the protocol passes.
        ours: Vec<usize>,
    },
    /// An arena size does not fit this machine's address space.
    #[error("arena too large")]
    TooLarge,
}

/// Names of the kernels in the library, in its order.
pub fn library_kernels() -> Vec<String> {
    (0..ffi::tbk_kernel_count())
        .filter_map(|i| {
            // SAFETY: `i` is below tbk_kernel_count(), for which the library returns a pointer
            // to a NUL-terminated string literal that lives as long as the program.
            let p = unsafe { ffi::tbk_kernel_name(i) };
            if p.is_null() {
                return None;
            }
            // SAFETY: as above: non-null, NUL-terminated, static.
            Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        })
        .collect()
}

/// Sizes in bytes of the parameters of library kernel `index`.
pub fn library_param_sizes(index: usize) -> Vec<usize> {
    let mut sizes = [0 as c_uint; tb_device::kernel::MAX_ARGS];
    let Ok(k) = c_int::try_from(index) else {
        return Vec::new();
    };
    // SAFETY: `sizes` has MAX_ARGS elements and that is the `max` passed; the library writes
    // at most `max` entries.
    let n = unsafe { ffi::tbk_kernel_params(k, sizes.as_mut_ptr(), sizes.len() as c_int) };
    let n = usize::try_from(n).unwrap_or(0).min(sizes.len());
    sizes[..n].iter().map(|s| *s as usize).collect()
}

/// The host launcher's own test kernels (`tbk_test_launch` in `kernels/tb_kernels_ref.cc`):
/// small kernels with barriers that check the fibers and shuffles every kernel with barriers
/// runs on, and misuses the launcher must report. Each writes one word per thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherTest {
    /// Three phases through shared memory separated by `__syncthreads`: thread `t` of block `b`
    /// writes `((b << 16) | t) + 1` only if no thread ran ahead of a barrier.
    Sync,
    /// `__shfl_xor_sync` of `(b << 16) | t` with delta `arg`.
    ShflXor,
    /// `__shfl_up_sync` of `(b << 16) | t` with delta `arg`.
    ShflUp,
    /// Inclusive prefix sum over the block of `t * arg + 1`, as `k_ent_rowsum` computes it.
    Scan,
    /// Odd threads wait at `__syncthreads` while even threads shuffle: reported as code -3.
    DivergeKind,
    /// Thread 0 passes one barrier more than the others: reported as code -3.
    DivergeCount,
    /// The lanes shuffle with different deltas: reported as code -3.
    DivergeDelta,
    /// [`LauncherTest::Sync`] run as a plain loop (without the barrier flag): reported as -5.
    SyncUnflagged,
}

impl LauncherTest {
    /// Every test kernel, in the library's order.
    pub const ALL: [LauncherTest; 8] = [
        LauncherTest::Sync,
        LauncherTest::ShflXor,
        LauncherTest::ShflUp,
        LauncherTest::Scan,
        LauncherTest::DivergeKind,
        LauncherTest::DivergeCount,
        LauncherTest::DivergeDelta,
        LauncherTest::SyncUnflagged,
    ];
}

/// Whether the linked kernel library was built with AddressSanitizer (the feature `asan`).
pub fn sanitized() -> bool {
    ffi::tbk_sanitized() != 0
}

/// Number of test kernels in the library (must equal `LauncherTest::ALL.len()`).
pub fn launcher_test_count() -> usize {
    usize::try_from(ffi::tbk_test_kernel_count()).unwrap_or(0)
}

/// Runs a launcher test kernel over `grid` blocks of `block` threads with argument `arg`.
/// Returns one word per thread (block-major), or the launcher's status code (as
/// [`DeviceError::Failed`] carries it: -2 block size, -3 barriers not reached together, -4 no
/// memory, -5 a barrier outside the fibers).
pub fn run_launcher_test(
    test: LauncherTest,
    grid: u32,
    block: u32,
    arg: i32,
) -> Result<Vec<u32>, i32> {
    let n = usize::try_from(u64::from(grid) * u64::from(block)).map_err(|_| -2)?;
    let mut out = vec![0u32; n];
    let k = test as c_int;
    // SAFETY: `k` is a valid test kernel index (the enum follows the library's order; checked
    // by the tests against tbk_test_kernel_count). Each test kernel writes only
    // out[b * block + t] for b < grid, t < block, and `out` has grid * block words; the
    // launcher rejects block sizes outside 1..=1024 before running anything.
    let r = unsafe { ffi::tbk_test_launch(k, grid, block, out.as_mut_ptr(), arg) };
    if r == 0 { Ok(out) } else { Err(r) }
}

/// A device made of host memory and host-compiled kernels.
#[derive(Debug)]
pub struct RefDevice {
    arenas: ArenaSizes,
    /// The device arena, as u64 words so that every kernel's loads are aligned.
    dev: Vec<u64>,
    /// The host arena.
    host: Vec<u64>,
    /// Foreign regions by slot (memory "on the device" outside the arena: what a decoder's
    /// surfaces are on the card), as u64 words; empty: not bound.
    foreign: Vec<Vec<u64>>,
    /// Library index of each kernel of `Kernel::NAMES`.
    lib_index: Vec<c_int>,
    /// Completions of plans not yet waited for.
    done: Vec<Completion>,
    next_ticket: u64,
    /// A deterministic clock: one tick per executed step.
    clock: u64,
}

impl RefDevice {
    /// A device with arenas of these sizes, zeroed. Fails when the library and the protocol
    /// disagree about the kernels.
    pub fn new(arenas: ArenaSizes) -> Result<Self, RefError> {
        let words = |bytes: u64| usize::try_from(bytes.div_ceil(8)).map_err(|_| RefError::TooLarge);
        let lib = library_kernels();
        let mut lib_index = Vec::with_capacity(Kernel::NAMES.len());
        for name in Kernel::NAMES {
            let i = lib
                .iter()
                .position(|n| n == name)
                .ok_or(RefError::MissingKernel(name))?;
            lib_index.push(c_int::try_from(i).map_err(|_| RefError::TooLarge)?);
        }
        let d = Self {
            arenas,
            dev: vec![0; words(arenas.device)?],
            host: vec![0; words(arenas.host)?],
            foreign: Vec::new(),
            lib_index,
            done: Vec::new(),
            next_ticket: 0,
            clock: 0,
        };
        tracing::debug!(
            device = arenas.device,
            host = arenas.host,
            "reference device ready"
        );
        Ok(d)
    }

    /// Bind foreign slot `slot` to a region of `len` zeroed bytes owned by the adapter and
    /// return it for filling: the stand-in for a decoder surface, which handle records of a
    /// plan name with `tb_device::kernel::DevRef::foreign`. A slot bound already is replaced.
    pub fn bind_foreign(&mut self, slot: u16, len: usize) -> &mut [u8] {
        let i = usize::from(slot);
        if self.foreign.len() <= i {
            self.foreign.resize(i + 1, Vec::new());
        }
        // Eight bytes of slack: the kernels load whole 32-bit words up to the row's end.
        self.foreign[i] = vec![0; len.div_ceil(8) + 1];
        &mut bytemuck::cast_slice_mut(&mut self.foreign[i])[..len]
    }

    /// The bytes of a bound foreign slot.
    pub fn foreign(&self, slot: u16) -> Option<&[u8]> {
        let v = self
            .foreign
            .get(usize::from(slot))
            .filter(|v| !v.is_empty())?;
        Some(bytemuck::cast_slice(v))
    }

    /// Unbind every foreign slot.
    pub fn unbind_foreign(&mut self) {
        self.foreign.clear();
    }

    fn dev_bytes(&mut self) -> &mut [u8] {
        let n = self.arenas.device as usize;
        &mut bytemuck::cast_slice_mut(&mut self.dev)[..n]
    }

    fn run(&mut self, plan: &Plan) -> Result<(), DeviceError> {
        for (i, step) in plan.steps.iter().enumerate() {
            self.clock += 1;
            // The plan was validated: every region lies inside its arena, copy lengths agree.
            match step.op {
                Op::Fill { dst, byte } => {
                    self.dev_bytes()[dst.off as usize..dst.end() as usize].fill(byte);
                }
                Op::Upload { dst, src } => {
                    let (h, d) = (&self.host, &mut self.dev);
                    let h: &[u8] = bytemuck::cast_slice(h);
                    let d: &mut [u8] = bytemuck::cast_slice_mut(d);
                    d[dst.off as usize..dst.end() as usize]
                        .copy_from_slice(&h[src.off as usize..src.end() as usize]);
                }
                Op::Copy { dst, src } => {
                    // validated: equal lengths, inside the arena, not overlapping
                    self.dev_bytes()
                        .copy_within(src.off as usize..src.end() as usize, dst.off as usize);
                }
                Op::UploadRefs { dst, src, kind } => {
                    let base = self.dev.as_mut_ptr() as u64;
                    let foreign = &self.foreign;
                    let lookup = |slot: u16| {
                        foreign
                            .get(usize::from(slot))
                            .filter(|v| !v.is_empty())
                            .map(|v| (v.as_ptr() as u64, (v.len() as u64 - 1) * 8))
                    };
                    let h: &[u8] = bytemuck::cast_slice(&self.host);
                    let mut recs = h[src.off as usize..src.end() as usize].to_vec();
                    kind.resolve(&mut recs, (base, self.arenas.device), &lookup)
                        .map_err(|what| DeviceError::BadPlan { step: i, what })?;
                    self.dev_bytes()[dst.off as usize..dst.end() as usize].copy_from_slice(&recs);
                }
                Op::Upload2d {
                    dst,
                    dst_pitch,
                    src,
                    src_pitch,
                    width,
                    rows,
                } => {
                    let (h, d) = (&self.host, &mut self.dev);
                    let h: &[u8] = bytemuck::cast_slice(h);
                    let d: &mut [u8] = bytemuck::cast_slice_mut(d);
                    let w = width as usize;
                    for r in 0..rows as usize {
                        let s = src.off as usize + r * src_pitch as usize;
                        let t = dst.off as usize + r * dst_pitch as usize;
                        d[t..t + w].copy_from_slice(&h[s..s + w]);
                    }
                }
                Op::Download { dst, src } => {
                    let (h, d) = (&mut self.host, &self.dev);
                    let h: &mut [u8] = bytemuck::cast_slice_mut(h);
                    let d: &[u8] = bytemuck::cast_slice(d);
                    h[dst.off as usize..dst.end() as usize]
                        .copy_from_slice(&d[src.off as usize..src.end() as usize]);
                }
                Op::Launch(l) => {
                    let base = self.dev.as_mut_ptr() as u64;
                    let mut args = l.kernel.resolve(base);
                    let mut ptrs = args.pointers();
                    let k = self.lib_index[l.kernel.index()];
                    // SAFETY: `k` is a library index found by name in `new`. `ptrs` holds one
                    // pointer per parameter of that kernel, each to a live value of the
                    // parameter's size (`args` outlives the call; sizes are compared with the
                    // library's in the tests of this crate). Every address among the values
                    // points into `self.dev`, which is not otherwise borrowed during the call
                    // and whose regions `Plan::validate` has checked against the arena size.
                    // Job records on the device (the conversion kernels) hold addresses
                    // resolved by `UploadRefs` into `self.dev` or a bound foreign region, each
                    // checked against its space; they stay valid while the region stays bound,
                    // which is the caller's rule on the card as well.
                    // What a kernel reads and writes through those addresses is determined by
                    // its integer arguments, which the core derives from the same layout; a
                    // wrong plan can corrupt the arena's contents or read outside it, as it
                    // would on the card.
                    let r = unsafe { ffi::tbk_launch(k, l.grid, l.block, ptrs.as_mut_ptr()) };
                    if r != 0 {
                        return Err(DeviceError::Failed { step: i, code: r });
                    }
                }
                Op::Record(_) | Op::WaitFor(_) => {}
            }
        }
        Ok(())
    }
}

impl Device for RefDevice {
    fn arenas(&self) -> ArenaSizes {
        self.arenas
    }
    fn host(&self) -> &[u8] {
        &bytemuck::cast_slice(&self.host)[..self.arenas.host as usize]
    }
    fn host_mut(&mut self) -> &mut [u8] {
        let n = self.arenas.host as usize;
        &mut bytemuck::cast_slice_mut(&mut self.host)[..n]
    }
    fn now(&self) -> Nanos {
        Nanos(self.clock * 1_000)
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        plan.validate(self.arenas)?;
        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        let submitted = self.now();
        let status = self.run(plan);
        if let Err(e) = &status {
            tracing::warn!(error = %e, "plan failed on the reference device");
        }
        self.done.push(Completion {
            ticket,
            status,
            submitted,
            finished: self.now(),
        });
        Ok(ticket)
    }
    fn wait(&mut self, ticket: Ticket, _timeout: Option<Nanos>) -> Completion {
        match self.done.iter().position(|c| c.ticket == ticket) {
            Some(i) => self.done.swap_remove(i),
            None => Completion {
                ticket,
                status: Err(DeviceError::UnknownTicket),
                submitted: self.now(),
                finished: self.now(),
            },
        }
    }
}
