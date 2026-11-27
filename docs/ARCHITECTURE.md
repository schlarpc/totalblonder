# totalblonder: architecture of the Rust workspace

State of 2026-11-25 (after package X1). This document describes the Rust workspace at the
repository root: what each crate is for, where the line between pure logic and I/O runs, how
the GPU is modeled, how the kernels are built for NVIDIA and AMD cards, and how the encoder is
checked.
`docs/SPEC.md` says what the server must do; this says how the code is laid out to do it.

The workspace began as the port of the host side of a CUDA MPEG-2 encoder written as a C++
spike; the port reached the C++'s gate and capacity and the C++
was retired (§10, "History"). The kernels are CUDA C++ (`crates/tb-kernel-abi/kernels/`). The rest of the server
(mux, tables, ingest, control, probe) will be added to the same workspace under the same rules
(§9).

Labels: **[decided]** done or fixed; **[proposed]** designed here, not built; **[unknown]**
needs a measurement or the owner. Work packages (`E4`, `K3`, …) are the steps of the port (§10).

## 1. Rules

1. **Sans-io core.** [decided] A logic crate takes inputs and returns outputs as plain data.
   It has no clock, thread, socket, file, FFI, global state or async runtime. Time is an
   argument. Every such crate has `#![forbid(unsafe_code)]` and its tests run in the Nix
   sandbox with no GPU.
2. **The device is a protocol.** [decided] The core does not call CUDA or HIP. It writes a plan for
   each phase of a tick and reads a completion record. Adapters execute plans (§4, §5).
3. **One definition of the kernel ABI.** [decided] Records shared by host and kernels are
   defined once, in Rust; the C header is generated (§3).
4. **One copy of the kernels.** [decided] The kernel sources (`crates/tb-kernel-abi/kernels/`) are compiled two ways
   from the same files, both by Nix: into the device image and for the host (the reference
   adapter) (§6).
5. **Errors are values.** [decided] A device error is a failed completion that the core turns
   into a failure of the batches the plan served. Nothing in a library crate aborts the
   process: no `unwrap` or `expect` outside tests (clippy denies both), `thiserror` enums at
   crate boundaries.
6. **Conventions.** [decided] Toolchain pinned in `rust-toolchain.toml` (1.99.1), `rustfmt`
   with `max_width = 100`, clippy with warnings denied, `missing_docs` on, `unsafe` only in the
   adapter crates with a `SAFETY` comment on every block, `tracing` in adapters only, tests
   run with nextest. `nix flake check` enforces all of it.

## 2. Crate map

```
                    tb-kernel-abi            records and constants shared with the kernels
                    /     |      \
        tb-m2v-syntax  tb-m2v-rate  tb-device ............ plans, kernel records, arenas,
                    \     |      /      |    \             recorder / replay
                    tb-m2v-encoder      |     tb-convert   conversion planning
                          |             |
   sans-io  ──────────────┼─────────────┼──────────────────────────────────────────
   adapters               |        tb-device-ref   (host-compiled kernels)
                          |        tb-device-gpu   (CUDA driver API, HIP)  tb-decode (libav)
                          |             |                                     |
                                   tb-engine  (Engine: adapter + encoder + plan)
                                        /                    \
                        tb-m2venc  (command-line driver)      tb-pipe  (pipeline harness)

   tb-timeline  (no dependencies)   tb-golden  (golden vectors: reader, record writer)
   tb-m2v-verify  (conformance verifier, from the standards; used by the gate)

   The sandboxed decoder (§5.5):
   tb-media ── tb-decoder-proto (pure) ──┬── tb-decoder-client (server side; no libav)
                                         └── tb-avworker (binary; the only libav)
                       tb-sandbox (process confinement, shm, ipc) ──┘ both
```

| Crate | Kind | Responsibility | Ported from (C++ spike, §10) |
|---|---|---|---|
| `tb-kernel-abi` | pure | `Geo`, `SessPar`, `MbInfo`, `MbRes`, `RowStat`, `PicStat`, `Tables`, `VlcCode`, `Bufs`, the conversion records `Taps`, `ConvClass`, `ConvJob`, `RectJob`, `CscPar`, `PreJob`, constants. Source of `tb_kernel_abi.h` | the C++ records of the kernels and of the conversion |
| `tb-m2v-syntax` | pure | Bit writer, sequence / GOP / picture headers, caption user data, all-skipped slices, the tables of H.262 | `HBits`, `Batch::headers`, `Batch::skip_slices`, table set-up, `vlc_tables.h` |
| `tb-m2v-rate` | pure | Buffer model and rate-change contract (`vbv`); rho-domain model (`model`); picture allocation, row quantizers, correction passes, the ladder (`control`) | `Vbv`, `RateControl`, `nz_at`, `Batch::choose`, the rate-control halves of `begin` / `quantize` / `retry` / `collect` |
| `tb-device` | pure | The device protocol: `DevBuf` / `HostBuf`, arena allocator, typed kernel records, `Plan`, `Completion`, `DeviceError`, the `Device` trait, `Recorder` and `Replay`, the plumbing self-test | `Arena`, the `LAUNCH` macros, every `cudaMemcpyAsync` call site |
| `tb-m2v-encoder` | pure | Sessions, GOP structure, batch geometry and layout, deadline policy, the tick state machine. Emits plans, consumes completions, returns access units | `Session`, `Batch`, `Encoder` |
| `tb-convert` | pure | Filter taps, color matrices, which conversion launches a tick needs (`Converter`), and their plan steps (`Lowerer`) | the host half of the C++ conversion |
| `tb-timeline` | pure | Timestamp mapper, frame clock, fallback selector (SPEC §5.3, §5.4, §9.5); which frame and rows a channel shows (`select`); film mode: the selections per output field grouped into pictures that say how long they are shown (`film`, §7.6); the decode thread's decisions (`ingest`: pacing, degrade, resync, frame times); captions through drops, repeats and pictures of different display times (`captions`); admission to the hardware decoder (`admission`) | the C++ harness's frame selection and decoder decisions; otherwise new |
| `tb-device-ref` | adapter | Executes plans with the kernels compiled for the host | none (new) |
| `tb-device-gpu` | adapter | Executes plans on a card: the CUDA driver API (NVIDIA) or HIP (AMD), one implementation of the protocol over both | the encoder's CUDA runtime calls |
| `tb-decode` | adapter | Inputs demuxed and decoded through libavcodec: NVDEC by the `cuda` hwaccel inside the CUDA adapter's context, or software with swscale; one thread per input | the C++ harness's decoder threads |
| `tb-media` | pure | The plain-data contract of ingest: codecs, streams, packets (`InputEvent`), decoded-frame descriptors | none |
| `tb-decoder-proto` | pure | The sandboxed decoder's wire protocol, input ring and output pool accounting, validation of everything the worker says, restart and watchdog policy, exit classification (§5.5) | none |
| `tb-sandbox` | adapter | Spawns a confined child (namespaces, minimal root, rlimits, Landlock, seccomp; each layer reported), supervises it by pidfd; sealed memfds; the SEQPACKET channel (§5.5) | none |
| `tb-avworker` | binary | The decoder worker: the only program that links libav (libavcodec, libavutil, libswscale, libswresample) | none |
| `tb-decoder-client` | adapter | The server's handle: `DecoderPool`, `Decoder` (`try_send`, `poll`, `events`), zero-copy frames (§5.5) | none |
| `tb-golden` | pure | Embeds and parses `crates/tb-golden/vectors/*.jsonl`; the record writer (`write`) of the encoder's golden recorder | none |
| `tb-engine` | adapter | The device-owning loop: `Engine` (an adapter, the encoder and the plan; one tick at a time), `TickDevice` (what the loop needs of an adapter), `Guarded` (the host-arena rule around any adapter), the drivers' random sequences | the driver's loop |
| `tb-ac3` | pure | AC-3 (A/52 main body) encoder at 48 kHz: MDCT with block switching, exponents, the decoder's parametric bit allocation, SNR offset search per block, mantissa quantization, syncframe packing with both CRCs; a strict frame parser and a reference decoder for tests | none (new; written from A/52:2012) |
| `tb-m2v-verify` | pure + binary | MPEG-2 video conformance verifier written from H.262, A/53, SCTE 43 and CTA-708 (`docs/notes/m2v-conformance-rules.md`); runs on every stream of the gate | none |
| `tb-m2venc` | binary | Command-line driver for the gate and the benchmarks | the driver `m2venc` |
| `tb-pipe` | binary | The pipeline harness: inputs, frame selection, conversion, sessions | the harness `m2vpipe` |

Dependency direction: pure crates depend only on pure crates. Adapters depend on `tb-device`
and `tb-kernel-abi`, never on the encoder. The encoder never depends on an adapter. Only
binaries see both sides.

Why this split and not finer [decided]: the buffer model and rate control share state and are
ported from one struct, so they are one crate with separate modules (the mux will use
`tb_m2v_rate::vbv` for the delivery contract). GOP structure, layout, shedding and the tick
are one crate because the tick owns all of their state; they are separate modules with
separate work packages. Syntax is its own crate because the mux and the probe will parse and
write the same headers.

## 3. The kernel ABI

**Decision [decided]: Rust is the single definition; the C header is generated.**
`crates/tb-kernel-abi/src/lib.rs` defines every record with `#[repr(C)]`. cbindgen reads that
one file (no Cargo run) and writes `tb_kernel_abi.h`. The kernel sources and the host build of
the kernels include that header. `common.h` and `kernels.h` do not define these records.

Alternative rejected: the C header as source with bindgen. It would have left the C++ of the
time untouched, but it puts a build script and libclang into a pure crate, hides the types from
readers of the Rust, cannot make the records `bytemuck::Pod` (bindgen keeps implicit padding;
here every record that crosses the bus has explicit padding and derives `Pod`, so uploads are
safe byte casts), and leaves the definition with the side that only reads the records. The
host fills them; the host should own them.

How the two sides are kept equal:

- The committed header (`crates/tb-kernel-abi/kernels/tb_kernel_abi.h`, next to the kernels) must equal the
  generated one: check `kernels/abi-header-fresh`. `nix run .#abi-header` rewrites it.
- Rust side: `tb-kernel-abi` pins every size (`sizes_are_pinned`) and compares every offset
  and size with a manifest printed by a C++ program compiled against the generated header
  (`crates/tb-kernel-abi/kernels/abi_layout.cc`, derivation `abiLayout`, test `layout_matches_the_c_header`).
- C++ side: `static_assert`s in `vlc_tables.h` and `kernels.h`; the launch signatures are
  checked separately (§4.3).

Constants are generated as macros so that they keep the `int` semantics the kernels were
written with. Records that hold device addresses (`Bufs`) carry raw pointers and are built by
adapters only; the core names buffers by handle.

`MbCode` and `FieldRes` stay in the C++ of the kernels: only kernels use them. The conversion
records (`ConvClass`, `ConvJob`, `RectJob`, `CscPar`, `PreJob`, `Taps`) are in this crate
[done, K2]; the conversion kernels are `crates/tb-kernel-abi/kernels/pipe_kernels.h`, part of the image and of the
host library. The tables the kernels read are built by `tb-m2v-syntax` and uploaded as the
`Tables` record (the golden `tables` record pins it); `crates/tb-kernel-abi/kernels/vlc_tables.h` and `common.h`
still hold the C++ spike's copy of the table values, which the image does not need [open:
remove them with the next change to the kernels, it shifts the image's line table].

## 4. The device protocol (`tb-device`)

### 4.1 Memory

An adapter owns two blocks of memory, taken once when it is made: the device arena and the
host arena (pinned, on the CUDA adapter). A buffer is an offset and a length in one of them:

```rust
pub struct DevBuf  { pub off: u64, pub len: u64 }   // a region of the device arena
pub struct HostBuf { pub off: u64, pub len: u64 }   // a region of the host arena
pub struct ArenaAlloc { /* first fit over (offset, length) ranges, neighbors merged */ }
```

Laying out a batch is arithmetic (`tb_m2v_encoder::layout`), so opening a session, making a
batch for a new format and freeing one never call a driver. This is the arena path of the C++
(`--arena`), which measured 0.2 ms per new batch against 5 to 86 ms from the driver
(spike notes §C.6); the driver path and the side thread are not ported [decided]. A batch that
does not fit is an error to the caller (`EncoderError::ArenaFull`), not a stall.

The core writes parameters and frames into the host arena and reads results from it through
a `&mut [u8]` that the driver loop lends it. No copy, no allocation, and still plain data.
Rule: a region named by an upload or download of a plan in flight is not touched until that
plan has completed.

### 4.2 Plans

```rust
pub struct StreamId(pub u8);   // MAIN (kernels), UPLOAD (next tick's frames), SIDE (clearing)
pub struct EventId(pub u8);    // PARAMS, FRAMES

pub enum Op {
    Fill     { dst: DevBuf, byte: u8 },
    Upload   { dst: DevBuf, src: HostBuf },
    Upload2d { dst: DevBuf, dst_pitch: u32, src: HostBuf, src_pitch: u32, width: u32, rows: u32 },
    Copy     { dst: DevBuf, src: DevBuf },                 // device to device, no overlap
    UploadRefs { dst: DevBuf, src: HostBuf, kind: RefKind }, // records with handles (below)
    Download { dst: HostBuf, src: DevBuf },
    Launch(Launch),              // kernel, grid, block
    Record(EventId),             // mark this point of the stream
    WaitFor(EventId),            // what follows on this stream runs after the mark
}
pub struct Step { pub stream: StreamId, pub op: Op }
pub struct Plan { pub steps: Vec<Step>, pub wait: StreamId }

pub struct Completion {
    pub ticket: Ticket,
    pub status: Result<(), DeviceError>,
    pub submitted: Nanos,        // adapter clock
    pub finished: Nanos,
}
pub enum DeviceError { BadPlan { .. }, Failed { step, code }, Lost { code }, Timeout, UnknownTicket }

pub trait Device {
    fn arenas(&self) -> ArenaSizes;
    fn host(&self) -> &[u8];
    fn host_mut(&mut self) -> &mut [u8];
    fn now(&self) -> Nanos;
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError>;   // queues, does not wait
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion;
}
```

A plan is one phase of a tick: everything queued before the host next has to wait. The
ordering rules of the C++ are in the plan as data, not in adapter code:

- Operations of one stream run in queue order.
- The measured rule that the frame upload must wait until the tick's parameter copies have
  gone through (optlog experiment 41) is `Record(PARAMS)` on `MAIN` after the parameter
  uploads and `WaitFor(PARAMS)` at the head of the `UPLOAD` steps.
- Frames uploaded on `UPLOAD` during tick n are read by tick n + 1. As in the C++, the host
  waits for the upload stream at the start of the tick: with the overlapped upload a tick
  begins with a plan of its own that has no steps and waits on `UPLOAD` (phase `Sync`)
  [decided in E4]. The event pair `Record(FRAMES)` / `WaitFor(FRAMES)` (one host wait fewer)
  is an encoder option (`EncoderOptions::frames_event`, off): measured in G2 next to the C++, it
  made no difference beyond the noise (§12, item 4). Because the upload of tick n may still read host memory while the caller writes the
  frame of tick n + 1, a session has two host frame regions with the overlapped upload.
- A tick has a fixed number of waits: three plans (`Begin`, `Quant`, `Finish`), issued even
  when they are empty, plus one `Quant` plan per extra quantization round, each ending with
  one wait on `MAIN`; with the overlapped upload the `Sync` plan before them. The encoder
  names the phase of the plan it wrote (`Encoder::phase`, `Encoder::wait_class`) so that an
  adapter can keep its wait statistics per phase. Outside ticks, `Encoder::settle` writes a
  plan that only waits for a stream (the C++ `Encoder::finish`); the one for `MAIN` carries
  the clearing and the tables of new batches, which otherwise go at the head of the next
  `Begin` plan.

**Plans handed out per batch [built, G2].** With `EncoderOptions::split_plans` the first
quantization plan and the finish plan come one batch at a time: `Step::Queue` tells the driver
to submit the plan without waiting and to call `Encoder::queued` for the next; the phase's
last plan is `Step::Run` as before, and its wait covers the parts queued ahead of it on the
same stream (the driver waits for their tickets afterwards and reports the first failure with
that completion). The operations and their order on each stream are unchanged; only the
device starts on batch 1 while the host does the rate control of batch 2, as the C++ does
(`for (auto &b : batches) b->quantize()`). The drivers turn it on; replays of recorded plans
leave it off, because plan boundaries are what a replay compares.

**Device memory outside the arena [built, K2 + G2].** A decoder surface is device memory the
adapter does not own. The protocol names it without holding an address:

- `tb_device::kernel::DevRef` is a place in device memory as plain data: the arena, or a
  *foreign slot* (bits 63..48: 0 = arena, s + 1 = slot s) and a byte offset.
- The only records that hold source addresses are the conversion kernels' job records, which
  are uploaded as bytes. The core writes them in handle form (`ConvJobRef`, `PreJobRef`: the
  layout of the ABI records with a `DevRef` in place of each pointer) and uploads them with
  `Op::UploadRefs`. The adapter turns handles into addresses (`RefKind::resolve`, safe code
  shared by both adapters) before it queues anything, and refuses the whole plan (`BadPlan`)
  when a slot is not bound or an offset lies outside its space. The host region is consumed by
  the upload (the CUDA adapter resolves in place); in a recording the upload appears in
  handle form, so traces stay deterministic.
- Binding is the adapter's own interface, not the protocol's: `GpuDevice::bind_foreign(slot,
  base, len)` is `unsafe` with one rule, which is the lifetime rule of the design: the memory
  stays valid until the slot is bound anew or unbound **and** every plan submitted meanwhile
  has completed. `RefDevice::bind_foreign(slot, len)` owns stand-in memory, so the whole path
  is tested in the sandbox (`tb-device-ref/tests/convert.rs`).
- The pipeline's use (`tb_pipe::dev::PipeDevice::bind_surfaces`): the planes of the frames a
  tick's conversions read are bound for that tick, slot by slot, and the frames are held until
  the tick's conversion plan has completed. That plan is the driver's own: it is submitted
  before the encoder's tick and runs ahead of the tick's kernels on `MAIN`
  (`FrameSource::Device`), and is waited for after the tick.
- Destinations and everything else a kernel argument names stay arena regions (`DevBuf`).

**Static and dynamic [decided].** Static: arena sizes, the kernel set and signatures, streams
and events, and the order of operations within a phase, which is straight-line code in the
core as it is in `Batch::launch_begin`. Dynamic: buffer placement, session lists, counts and
grid sizes. There is no graph object and no scheduling in the adapter: it executes steps in
order. The core rewrites one `Plan` in place each phase; the step vector keeps its capacity,
so the hot path allocates nothing after warm-up. A step is about 400 bytes (a launch with a
`Geo` and a `Bufs`); a tick of one batch is about 30 steps (golden case `cbr`: 869 operations
in 30 ticks, waits included).

### 4.3 Kernels

`tb_device::kernel` has one record per kernel with the fields of the C signature, pointers as
`DevBuf`:

```rust
pub struct Pyr { pub src: DevBuf, pub sstride: u64, pub sw: i32, pub sh: i32, pub d1: DevBuf,
                 pub d1stride: u64, pub d2: DevBuf, pub d2stride: u64, pub n: i32,
                 pub list: DevBuf, pub na: i32, pub par: DevBuf, pub ssel: i32, pub dsel: i32 }
pub enum Kernel { Tick(Tick), PadCols(PadCols), PadRows(PadRows), Pyr(Pyr), /* ... 24 */ }
pub struct Launch { pub kernel: Kernel, pub grid: u32, pub block: u32 }
```

`Kernel::args()` lists the arguments in order; `Args::resolve(base)` turns handles into
addresses and yields the array of pointers to argument values that both launch interfaces
take (`cuLaunchKernel` and the host library's `tbk_launch`). Both adapters therefore share
the marshaling, in safe code. The seven conversion kernels (`ConvH`, `ConvV`, `ConvCopy`,
`Half`, `FillRect`, `Overlay`, `Csc`) take their records by value like `Geo` and `Bufs`:
`ConvClassRef` (tap tables as arena offsets), `RectRef`, `CscPar`. `Kernel::index()` is the
variant's position, without a search.

Signature check: the host library reports the size of each parameter of each kernel
(`tbk_kernel_params`), and `tb-device-ref` compares them with what `resolve` produces for all
24 kernels (`kernel_signatures_agree`). A parameter added on one side only fails
`nix flake check`. Parameter *order* among equal-sized parameters is not checked by this;
the per-kernel tests of K1 are what catch a swap.

### 4.4 Recording and replay

`sim::Recorder<D>` wraps any device and keeps a `Trace`: each plan, the bytes its uploads
read, its completion, and the bytes its downloads wrote. `sim::Replay` is a device made of a
trace. It rejects a plan that differs from the recorded one or uploads different bytes, hands
back the recorded results, and takes faults:

```rust
pub enum Fault {
    Fail  { submit: usize, error: DeviceError },   // this plan fails; no results are written
    Delay { submit: usize, extra: Nanos },         // this plan takes longer (contention)
}
```

This is what makes the deadline policy and the error isolation testable without hardware:
record a run once, then replay it with a phase that fails or completes late, on a clock the
test controls. A trace can also be built from golden records (`sim::golden`), so the recorded
runs replay through the core; and the host-arena rule is checked around any adapter
(`sim::HostGuard`, `tb_engine::Guarded`).

### 4.5 Errors [decided, built in E4]

How `tb_m2v_encoder::tick` handles the status of a completion (its module documentation is
the contract; tests `tick_golden.rs`, `tick_model.rs`):

- `BadPlan` (a bug in the core, or a plan the adapter refused at `submit`, which the driver
  reports as a failed completion), `Failed` (an operation was refused, for example out of
  resources at launch), `UnknownTicket`: the tick ends. Every picture the plan was coding
  becomes a host-made repeat (a valid picture, made without the device, as the deadline policy
  makes them), and its session re-keys: the next frame submitted is a key frame, off the grid,
  and the cadence stays on its grid. The first picture of a stream cannot be a repeat; it
  stays queued and is tried again. Frames whose upload was part of the failed plan are
  remembered as missing and their pictures become repeats. A failed wait for the upload
  stream (`Sync`) only marks the frames it should have confirmed. A failed set-up of a new
  batch is queued again. The device stays in use; there is no automatic retry of a plan.
- `Lost`: as above for the tick; from then on every session behaves as on hold (one repeat
  per due picture, buffer model, GOP grid and timestamps advancing; no plans). Either the
  driver puts a new adapter with the same arena sizes in place and calls
  `Encoder::device_restored` (batches are cleared and get their tables again; each session
  repeats until its next key frame, which the next submitted frame is), or, since the CUDA
  driver may refuse a new context in a process that saw a fault (D1), a fresh process opens
  the sessions again from `Encoder::snapshot` with `Encoder::restore`.
  [measured, D1] A fault on the device (illegal address, launch failure, trap) poisons the
  whole process on driver 595: after `cuDevicePrimaryCtxReset` the driver refuses any new
  context with the same status, so `GpuDevice::restore` returns
  `GpuError::ProcessPoisoned` and only a new process can use the card. The AMD card (ROCm 7.2)
  behaves the same, provided the process writes no core files (§5.2). In-process restore
  works only for losses that do not poison. So the tick thread and the adapter belong in a
  child process that a supervisor restarts, or the server restarts as a whole [open].
  What the driver does today [built in G1, `tb-m2venc`]: after `Lost` it takes no more input,
  lets every queued frame leave as a host-made repeat, ends each stream with
  sequence_end_code and exits with status 3 (4 when plans merely failed, 5 when a plan did
  not complete within `--device-timeout` and the streams end at the last whole picture).
  What moving the device-owning loop into a child process takes [proposed]. The cut exists:
  `tb_engine::Engine` owns adapter, encoder and plan, and everything above it (the
  schedule, files, statistics) only issues encoder commands between ticks and takes access
  units. So:
  1. the child is `Engine` plus a message loop; the parent keeps configuration, inputs, the
     mux and the per-session bookkeeping it needs to continue (below);
  2. commands (`open`, `close`, `submit`, `set_rate`, `force_keyframe`, `hold`, `tick` with
     its deadline) and results (access units, tick statistics, the error of a failed plan)
     cross a socket pair as plain records; all are plain data already. Access-unit bytes go
     through a shared ring or as message payload (15 Mbit/s per 1080i session: small);
  3. frames must not be copied through a socket: the host arena becomes shared memory
     (`memfd`) that the child registers with the driver (`cuMemHostRegister` in place of
     `cuMemHostAlloc`, one more binding) and that decoders in the parent write frame regions
     of (`Encoder::reserve_host`, `FrameSource::HostAt`); arena offsets mean the same on
     both sides;
  4. the child sends `Encoder::snapshot` of every session with each tick's results (a few
     hundred bytes per session; today the call clones the rate history, to be made
     allocation-free). When the child dies or reports `Lost`, the parent makes repeats itself
     for the ticks until a new child is up (the host-repeat path needs no device; it has to
     be callable without an `Encoder`, or the parent keeps a deviceless `Encoder` in the lost
     state fed with the same commands), then `Encoder::restore`s every session in the new
     child; each continues with a key frame;
  5. start-up of a child costs context creation and the module load (about 0.3 s measured in
     D1's tests) plus the clearing of the arena: some ten ticks of repeats.
- `Timeout`: not a failure. `Encoder::resume` returns `Step::Wait`; the driver waits again
  with the same ticket. The deadline policy acts before a tick is queued, not on a late plan.

The C++ exits on any CUDA error; the mapping above is new behavior.

## 5. Adapters

### 5.1 `tb-device-ref` [decided, built]

Two byte vectors, synchronous execution, and the kernels compiled for the host from the same
source (`crates/tb-kernel-abi/kernels/tb_kernels_ref.cc` includes `kernels.h` through a stand-in for
`<cuda_runtime.h>`). The shim defines the qualifiers away, provides `blockIdx` / `threadIdx`
as thread-locals, and implements the intrinsics the kernels use (`__vsadu4`, `__vavgu4`,
`__funnelshift_r`, `__dp4a`, `__ffsll`, `__float_as_uint`, `atomicOr`) from their
definitions. Kernels with barriers (`k_xf`, `k_rowstat`, `k_ent_rowsum`) run the threads of
a block as `ucontext` fibers in lockstep: every thread runs to its next `__syncthreads` or
`__shfl_*_sync`, then all continue; a shuffle reads the partner's value of the same step. All
17 kernels compile for the host today; `k_pyr` is tested; K1 tests the rest.

Integer kernels are bit-exact by construction; the four that use `float` are bit-exact because
both builds compute the same fused operations (§7.3).

### 5.2 `tb-device-gpu` [decided, built; HIP added after the port]

One implementation of the protocol, `GpuDevice<R: Runtime>`, over two runtimes: `Cuda`
(`libcuda.so.1`, NVIDIA) and `Hip` (`libamdhip64.so`, AMD, ROCm 6.2 or later). The adapter is
written against the thirty-odd operations it needs (`runtime::sys::Api`: module load, launch,
copies, memset, streams, events, pinned memory, context enter / leave / reset), which HIP
mirrors nearly name for name and numbers its statuses after the CUDA runtime's (600 not ready,
700 illegal address, 709 context destroyed); each runtime implements them with one forwarding
call each, so the monomorphized adapter makes the driver calls it made when it knew only
CUDA. `CudaDevice` and `HipDevice` are the two instances; one binary embeds both images and runs
on either vendor's card, chosen at run time (`--device cuda|hip`, `video.device`).

Differences the runtimes keep to themselves: HIP's primary-context functions are deprecated, so
`Hip` enters with `hipSetDeviceFlags(hipDeviceScheduleBlockingSync)` and `hipSetDevice`, and
resets with `hipDeviceReset`; `hipDeviceptr_t` is a pointer, converted from the arena's integer
addresses. `GpuDevice::context()` (the `CUcontext` for libav) exists on `CudaDevice` only.

Primary context with blocking-sync scheduling, the kernel image loaded with
`cuModuleLoadData` / `hipModuleLoadData`, three streams, two events, one device allocation and
one pinned host allocation.
`submit` validates the plan, queues every step and records a completion event of the plan's
own behind it on the wait stream; it returns without waiting. Up to 64 plans may be in flight;
each is waited for by its own event, so they complete in any order. A plan without steps (the
tick's wait for the upload stream, a settle) is waited for on its stream directly
(stream query / synchronize; an event recorded on an idle stream needs a round
trip to the device before it reads as complete) and without the hybrid wait's nap: what is
left of a stream's work at an arbitrary moment has no stable length, and the nap sized by the
average was the 1 % by which the port's free-running throughput trailed the C++ (§8.1).
`wait` is the hybrid wait
of the C++ (`Encoder::wait`, optlog experiment 19): sleep for 85 % of the class's moving
average less 0.1 ms, poll, block after twice the average plus 1 ms; the class is a number the
caller gives (`submit_class`) or the plan's wait stream and length. An error while queueing is
reported at the wait, after the stream has drained, so the caller's loop has one shape.
Optional timing events around each plan give its device-side duration (`set_timing`,
`last_device_time`). `restore` tears the context's objects down and makes them again. Which
statuses arrive where is documented in the crate and tested on both cards
(`tests/plumbing.rs`, every test instantiated as `cuda::…` and `hip::…`; what differs is the
test's `Card` trait):

| | RTX 2000 Ada, driver 595.71.05 | RX 7900 XTX, ROCm 7.2 |
|---|---|---|
| A launch the card refuses | `k_me` with 1024 threads per block: `cuLaunchKernel` returns 701 (out of resources); `Failed` at its step, the context stays usable | `k_me` runs with 1024 threads (192 VGPRs fit a workgroup processor); a grid of 2^31 such blocks is refused with 1 (`hipErrorInvalidValue`); `Failed`, the context stays usable |
| A kernel reading far outside the arena | 700 at the wait, `Lost`; the process is poisoned (`restore` returns `ProcessPoisoned`) | 2^36 bytes past the arena is still mapped (the read returns whatever is there); 2^37 below it faults: 700 (`hipErrorIllegalAddress`) at the wait, `Lost`, the process poisoned, as on CUDA |

The second row holds on AMD only because the HIP runtime turns core files off
(`hip::no_core_dumps`, soft `RLIMIT_CORE` 0, with a warning in the log). HIP's own handler of
GPU faults (`BackendErrorCallBackHandler` in ROCm's `rocclr/device/rocm/rocdevice.cpp`) records
the fault so that the wait returns 700, unless the process would write a core file
(`Os::DumpCoreFile`: `RLIMIT_CORE` nonzero); then it leaves the fault to ROCr's default handler
(`Runtime::VMFaultHandler`), which prints "Memory access fault by GPU node-1 … Page not present"
and calls `abort()` so that the core holds the fault. Measured both ways on the RX 7900 XTX: with
core files on, SIGABRT; off, `Lost { code: 700 }` and the process lives. A server that wants
core files of its own crashes gives this up on AMD.

**Bindings [decided]: thin own bindings to `libcuda.so.1` and `libamdhip64.so`**, about thirty
functions each in `src/cuda.rs` and `src/hip.rs` (macros of `src/runtime.rs`), loaded with
`libloading`. The CUDA choice, evaluated on 2026-11-25: Evaluated on 2026-11-25:

| Option | Finding | Verdict |
|---|---|---|
| `cust` (Rust-CUDA) | Last crates.io release 0.3.2, February 2022 | Rejected: not maintained as a published crate |
| `cudarc` 0.19.10 | Maintained (September 2026), has CUDA 13.2 bindings and run-time loading | Rejected for now: a missing library or symbol panics instead of returning an error; `CUresult` is a Rust enum, so a status the bindings do not list is undefined behavior; its safe layer owns one allocation per slice and has no arenas, pinned memory or events in the form the plans need, so only its raw layer would be used |
| own bindings | The driver API's `_v2` names have been stable since CUDA 4; status is an `i32`; loading returns a `Result`; the program builds and starts without a driver | Chosen |

The same reasons hold for HIP (crates for it are fewer and younger still). The bindings are
checked against the pinned toolkits' headers [decided, D1]: a test of the crate generates
`crates/tb-device-gpu/cuda_api_check.cc` and `hip_api_check.cc` from the binding lists (and
fails when a committed copy is stale), and the checks `kernels/cuda-api` and `kernels/hip-api`
compile them against `cuda.h` and `hip/hip_runtime_api.h`. It
asserts that each unversioned name resolves to the symbol the adapter loads (`cuMemAlloc` →
`cuMemAlloc_v2`; in 13.x `cuEventElapsedTime` is `cuEventElapsedTime_v2`, which a binding by
the plain name would have missed), the
exact function types (through a cast to the bound type: HIP's C++ header overloads `hipMalloc`
and `hipHostMalloc` with templates), the constants and the layout of `CUDA_MEMCPY2D`. The cost is one line
per function when a function is added.

For other users of the context [G2]: `CudaDevice::context()` gives the `CUcontext`,
`aux_stream()` a non-blocking stream that no plan uses (libav's copies of decoder surfaces run
there, so that they neither wait for nor hold up the adapter's streams), `bind_foreign` /
`unbind_foreign` the foreign slots of §4.2, `mem_info()` the card's memory in use.

### 5.3 The image and the GPU's presence

The crate embeds both images and their symbol tables at compile time
(`include_bytes!(env!("TB_KERNEL_IMAGE_CUDA"))`, `…_HIP`), so the binary is self-contained and
Nix rebuilds only this crate when a kernel changes. The crate compiles on a machine without a
GPU; making a `GpuDevice` there returns `GpuError::Driver`.

The HIP image is the same kernel sources through `kernels/shim/hip/cuda_runtime.h`, the device
counterpart of the host shim: HIP's runtime header; HIP's own `__shfl_*_sync` disabled (they
take a 64-bit mask and the wave's width) and replaced by shuffles 32 lanes wide, so the warp
code (`lane = threadIdx & 31`) means on AMD what it means on the card, on wave32 and wave64
parts alike; `__vsadu4` and `__vavgu4` as `v_sad_u8` and `v_lerp_u8`, `__dp4a` from its
definition. `fmaf_nc`'s inline PTX has had a portable branch from the start. No kernel source
changed. Built for the RDNA targets (gfx1030, gfx1100-1102, gfx1150/1151, gfx1200/1201, all
wave32) into one offload bundle; `k_me` spills (1872 bytes of scratch per thread on gfx1100),
a cost not yet measured against capacity [unknown].

### 5.4 `tb-decode` [decided, built in G2]

libavcodec with the `cuda` hwaccel, the route the spike chose (notes §C.2), in the adapter's
own context: `DecDevice::wrap(cuda_ctx, stream)` allocates an `AVHWDeviceContext` of type
CUDA, sets its `cuda_ctx` and `stream` and initializes it (decoders in a context of their own
cost 8 to 10 % of every tick). Frames are `AV_PIX_FMT_CUDA`; `Surface::Cuda` carries the two
device addresses and pitches (NV12, or P010 / P016 with `bps` 2) and the frame owns its
`AVFrame` until dropped. One thread per input (nice 10) runs the port of the C++ harness's
decoder threads: pacing,
looping, constant-rate frame times, `--degrade` (non-reference pictures skipped while behind)
and `--resync` (to the next key frame), a bounded queue; the decisions are
`tb_timeline::ingest`. Software decoding is the same thread without a device; with
`host_size` it scales with swscale into pooled host buffers (the comparison path and the
fallback of admission control).

**Bindings [decided]: `ffmpeg-the-third`** 6.0.0+ffmpeg-9.0 (released 2026-08-09; it follows
FFmpeg releases, 5.0.0 for FFmpeg 8.1 in April 2026). Its `-sys` crate runs bindgen on the
headers of the linked FFmpeg (nixpkgs `ffmpeg-headless` 9.0.2, built with nv-codec-headers:
`--enable-nvdec`), so struct layouts are the library's own; it built under breakbulk with
`pkg-config` and `rustPlatform.bindgenHook` as inputs of `ffmpeg-sys-the-third`
(`nix/rust.nix`). The safe layer covers containers, decoder contexts, packets and frames; raw
`ffmpeg_sys_the_third` calls cover the hardware device context, `get_format`, frame flags and
color, device pointers, byte seeks and swscale into a plain buffer. One layout is declared by
hand, `AVCUDADeviceContext` (three pointers; `hwcontext_cuda.h` needs NVIDIA's headers, which
bindgen does not see), and the check `tb-decode/cuda-layout` asserts it against the real
header. The driver's libraries (`libcuda.so.1`, `libnvcuvid.so.1`) are opened by name, then
from `/run/opengl-driver/lib`.

### 5.5 The sandboxed decoder [decided, built in the decoder-sandbox package]

The owner does not trust libav, so the server does not link it. Receivers (pure Rust) produce
`tb_media::InputEvent`s; `tb-decoder-client` feeds them to a `tb-avworker` process started
by `tb-sandbox` and takes decoded frames back from shared memory.

```text
 server thread ─ Decoder ── SOCK_SEQPACKET (versioned messages, fds by SCM_RIGHTS) ── tb-avworker
                   │  ▲                                                        (sandboxed,
                   │  └── output pools: sealed memfds of fixed slots ◄──────── libavcodec)
                   └───►  input ring: sealed memfd of packet bytes, credit by Consumed ──►
```

- **Confinement** (`tb-sandbox`): user, mount (read-only tmpfs root with `/nix/store` and the
  program), PID (with an init), network, IPC and UTS namespaces; no_new_privs; only the IPC
  descriptors; rlimits; death signal; not dumpable; Landlock (read/execute of the root's paths
  only, no TCP, scoped signals and abstract sockets); seccomp-bpf allow-list after libav's
  initialization, kill on anything else. Each layer reports whether it took effect; the
  client logs it and refuses a worker missing a required layer. Without user namespaces the
  namespace layers are off and loudly reported; the rest holds.
- **Protocol** (`tb-decoder-proto`, pure): every message length- and range-checked; every
  frame descriptor validated against the slot it names (planes in bounds and disjoint,
  strides, sizes, captions, timestamps near the packets sent, converted output matching its
  spec). A violation kills the worker. Packets travel in a ring (one copy, never blocking the
  server: full is a drop); frames in fixed slots handed over by credit (a frame is a guard
  whose drop returns the slot; frames outlive the worker).
- **Supervision**: heartbeats, a stall watchdog, exit classification (clean, code, out of
  memory, CPU limit, seccomp violation, crash, killed for a stall / heartbeat / start-up /
  violation, set-up failure), restart with exponential backoff and jitter, a circuit breaker.
  A crash is an `Event` for the fallback chain (SPEC §9.5), never an error of the server.
- **Outputs per stream**: native frames (planar 4:2:0, NV12, P010; f32 planar audio), or
  converted in the worker: video by libswscale to the program's size in the encoder's host
  layout (Lanczos, fit or stretch, bob, BT.601/BT.709), audio by libswresample to 48 kHz
  stereo or 5.1 in A/52 order.
- **GPU**: NVDEC through the `cuda` hwaccel in the worker where an NVIDIA card is, else VA-API
  through the `vaapi` hwaccel (AMD: Mesa's radeonsi VA driver on the DRM render node), frames
  copied to host (`av_hwframe_transfer_data`); a wider seccomp profile and Landlock rules for
  the driver (NVIDIA's device nodes, the render nodes); passing on nixark (NVDEC) and bagel
  (VA-API, the same profile, seccomp in kill mode). Keeping pictures on the card across processes (CUDA IPC into foreign
  slots, §4.2) is designed in the log, not built.
- **Cost** (bagel, 1080p30 H.264, software): 0.105 cores per stream in the worker against
  0.099-0.103 in-process (`tb-decode`), plus 0.002 in the server; start-up 9 ms.

`tb-pipe` on an AMD card (`--device hip`) decodes in software: NVDEC's surfaces in the adapter's
own context have no counterpart there yet (VA-API surfaces would have to be exported as DMA-BUFs
and imported into HIP) [open].

`tb-decode` (§5.4) stays as the in-process path of the test harnesses (`tb-pipe`) and of the
device-resident pipeline until the CUDA-IPC variant exists.

## 6. Nix and breakbulk

### 6.1 Layout [decided]

```
flake.nix          nixpkgs, breakbulk, rust-overlay; unfree by license
nix/kernels.nix    abiHeader, abiLayout, deviceImage, refLib(Asan), and their checks
nix/rust.nix       the breakbulk module: toolchain, clippy, fmt, nextest, package settings
nix/gate.nix       the gate's sandbox checks and apps, gpucomp (tools/gate/README.md)
nix/verify.nix     the conformance verifier's checks on our streams and on independent encoders
crates/tb-kernel-abi/kernels/           the kernel sources, translation units, kernel list, host shim, layout printer
tools/gate/              the gate's tools (Python and shell)
crates/tb-golden/vectors/            the golden vectors and their generator
```

- **breakbulk** (`github:schlarpc/breakbulk`) builds the workspace, one derivation per Cargo
  unit. `mkRepository` gets `nix/rust.nix` and the kernel checks.
- **Kernel derivations** are ordinary derivations outside Cargo:
  - `deviceImage`: `nvcc -O3 -std=c++17 -arch=sm_89 -lineinfo -fmad=false -cubin` (the C++ spike's flags) on
    `crates/tb-kernel-abi/kernels/tb_kernels.cu`, CUDA 13.2 pinned as in the spike; plus `tb_kernels.sym`, the
    mangled name of each kernel read from the image, and a check that every kernel of
    `crates/tb-kernel-abi/kernels/tb_kernel_list.h` is present.
  - `deviceImageHip`: `hipcc -x hip -O3 -std=c++17 -ffp-contract=off --genco` for the RDNA
    targets of `hipArchs` (ROCm 7.2 of the pinned nixpkgs, from the binary cache; about three
    minutes), with `-Ikernels/shim/hip`: `tb_kernels.hsaco`, an offload bundle, plus
    `tb_kernels.sym` read from the first target's code object, and checks that every kernel
    of the list is present and compiled for 32-lane waves.
  - `refLib`: `libtbkernels_ref.a` from `crates/tb-kernel-abi/kernels/tb_kernels_ref.cc` with
    `-fno-strict-aliasing -ffp-contract=off` (`fmaf` is libm's, correctly rounded), no C++
    runtime needed.
  - `abiHeader`, `abiLayout`: §3.
- **Plumbing into Rust units**: breakbulk package settings put store paths into the
  environment of the one package that reads each:

  ```nix
  packages.tb-kernel-abi.env.TB_ABI_LAYOUT = kernels.abiLayout;
  packages.tb-device-ref.env.TB_KERNELS_REF = kernels.refLib;        # build.rs links it
  packages.tb-device-gpu.env = { TB_KERNEL_IMAGE_CUDA = "…/tb_kernels.cubin"; TB_KERNEL_SYMBOLS_CUDA = "…";
                                 TB_KERNEL_IMAGE_HIP = "…/tb_kernels.hsaco"; TB_KERNEL_SYMBOLS_HIP = "…"; };
  packages.tb-golden.extraSources = [ "vectors" ];
  ```

  A kernel edit rebuilds the images, `tb-device-gpu` and what links it; nothing else.
- **Unfree**: `config.allowUnfreePredicate` in the flake's own `import nixpkgs` admits packages
  whose license is the CUDA EULA. No global setting on either machine.
- **Evaluation** needs import-from-derivation (breakbulk reads Cargo's plan at evaluation time
  until Nix 2.36). Both machines allow it by default.

### 6.2 Outputs [decided]

| Command | What it does |
|---|---|
| `nix flake check` | Every product, every test binary's run under nextest, doctests, clippy `-D warnings`, `cargo fmt --check`, the kernel builds, the header freshness check, the golden vectors' freshness (`golden/fresh`), the small gates on the reference adapter (`gate/ref-small`, `gate/pipe-ref-small`), the verifier's checks. 256 checks; no GPU |
| `nix develop` | Shell where plain `cargo build`, `cargo clippy`, `cargo nextest run` work: the toolchain of `rust-toolchain.toml`, the `TB_*` variables, nextest, cbindgen |
| `nix run .#gpu-plumbing -- cuda::\|hip::` | The GPU tests of `tb-device-gpu` for the machine's card, outside the sandbox (breakbulk's test app with `--include-ignored`) |
| `nix run .#gpu-bench [-- --runtime cuda\|hip]` | The protocol-cost bench of `tb-device-gpu` on the card (release build; §8.1) |
| `nix run .#tb-m2venc -- plumb ref\|cuda\|hip` | The plumbing proof as a program |
| `nix run .#tb-m2venc -- --device ref\|cuda\|hip --input …` | The encoder (`crates/tb-m2venc/src/opts.rs`; `--golden FILE` writes the golden records) |
| `nix run .#gate-check -- [--quick] [--out NAME]` | The correctness gate (`tools/gate/check.py`) on the card: corpus at broadcast sizes, torture cases, features; strict decode in two decoders, both buffer models, reconstruction against decode, quality against `tools/gate/baselines/`, the conformance verifier on every stream |
| `nix run .#gate-film -- [--only REGEX] [--measure]` | The film-mode gate on the card (`tools/gate/film_gate.py`, §7.6): `tb-m2venc --film` and `tb-pipe film=auto` at broadcast sizes, the cadence on both decoders and the displayed fields, both buffer models, the verifier; `--measure` compares rate and quality with the coded pulldown. Needs `gate-prep film` once |
| `nix run .#gate-pipe -- [--only REGEX]` | The pipeline's gate (`tools/gate/pipe_check.py`: device input, repeats, run-time sessions, conversions against CPU references and analytic patterns), verifier on every stream |
| `nix run .#gate-small -- --device ref\|cuda\|hip --golden` | The twelve golden cases through the binary, each stream on its own merits, and the committed digests. With `ref` it is the check `gate/ref-small` |
| `nix run .#gate-golden -- [--device cuda\|hip] DIR` | The golden cases on the card: every record (but the measured times) and every stream equal to the committed ones (`crates/tb-golden/vectors/README.md`) |
| `nix run .#gate-bench -- --mode free\|paced\|both --label NAME` | Capacity (`tools/gate/bench_capacity.py`): free-running macroblocks per second with spread, on-time fraction and cap99 / cap999 around given points; watches `nvidia-smi pmon` itself and repeats disturbed runs; more builds compared interleaved with `--bin NAME="PATH …"` |
| `nix run .#gate-lineup -- …`, `.#gate-contend -- …`, `.#gate-prep -- media\|inputs` | The pipeline's paced line-ups, the encoder next to other users of the card (with `gpucomp`), the corpus (`tools/gate/README.md`) |
| `nix run .#gate-tb-pipe -- --in … --ch …` | The pipeline harness itself |
| `nix run .#verify-streams -- …` | The conformance verifier on any streams |
| `nix build .#gate-tools` | The gate programs in one store path, to `nix copy` to the GPU host and run there |
| `nix run .#abi-header` | Rewrite the committed ABI header (`crates/tb-kernel-abi/kernels/tb_kernel_abi.h`) |
| `nix build .#kernels-device` / `.#kernels-device-hip` / `.#kernels-ref` / `.#gpucomp` / `.#nsys` / `.#ncu` | The pieces; Nsight Systems and Compute of the pinned toolkit |

GPU tests are `#[ignore]`d tests in adapter crates: `nix flake check` and a bare `cargo test`
build them and run nothing; the app runs them where the card is.

### 6.3 Machines

Both evaluate and build the flake [measured]. bagel has no NVIDIA GPU but an AMD one (RX 7900
XTX, gfx1100, ROCm 7.2 in `/run/opengl-driver/lib`): it compiles both images, runs everything
but the CUDA tests, and the HIP side of the GPU tests (`gpu-plumbing -- hip::`, the golden
cases with `--device hip`, the worker's VA-API test). nixark has the card (driver 595, libraries in
`/run/opengl-driver/lib`, which the adapter tries after the loader's own search path).

On nixark, memory-hungry runs go through `tools/gate/capped.sh` (transient systemd scope,
`MemoryMax=10G`, no swap). The media corpus of
the gate lives in `~/totalblonder-spike/media` on nixark (the gate's default when the tree has
no `media/`; `gate-prep` rebuilds it anywhere).

### 6.4 Copying instead of building

Building on bagel and copying is cheaper than building on nixark:
`nix copy --no-check-sigs --to ssh://nixark <paths>` of the outputs of this flake's own
derivations (2.4 GB closure) took seconds. breakbulk's own tools are needed at evaluation
time and were built on nixark the first time (reported as schlarpc/breakbulk#232).

## 7. How the encoder is checked

### 7.1 Golden vectors [decided, built]

`tb-m2venc --golden FILE` makes the encoder write one JSON line for every host-side decision
and every value that crosses the host/device boundary: GOP typing, buffer-model operations,
`SessPar` blocks and session lists, row statistics as returned, rate-control targets and row
quantizers, every correction pass and its outcome, headers, access units with their bytes, and
the device operations of each plan with the host's waits (`Encoder::record_golden`; the
recorder is `tb_m2v_encoder::golden`, rate control reports through
`tb_m2v_rate::control::Observer`). `crates/tb-golden/vectors/README.md` is the format contract. Twelve small cases
(about 6 MB) are committed, recorded on the reference adapter by `crates/tb-golden/vectors/generate.sh`; the
check `golden/fresh` fails when a committed file is not what the tree writes, and
`nix run .#gate-golden` shows that the card writes the same records. `tb-golden` embeds them for
tests. The format and the first five cases are the C++ spike's; the Rust recorder was shown to
write them identically before the C++ was deleted (`crates/tb-golden/vectors/README.md`, "Provenance").

The recorder is off unless asked for: its hooks are untaken branches, and the gates' streams
are byte-identical with it in the code (measured in X1). Doubles are printed as `%.17g` and
compared with `==`.

They are regression snapshots: each pure crate is tested against the records it owns, the tick
replays against the recorded plans, and the assembled encoder against the streams. A change of
behavior regenerates them; the diff of the vectors is part of its review.

### 7.2 The levels

Cheapest first:

1. **Replay in the sandbox** [built, E4]: a `Replay` device built from the golden records of a
   case (plans from `op`, uploads from `par` / `lists` / `rowq`, downloads from `rowstat` /
   `pass` / `au.data`). The tick must issue the recorded operations and produce the recorded
   access units (`tb-m2v-encoder/tests/tick_golden.rs`; faults and delays injected into the
   same traces). No kernel runs.
2. **Reference adapter in the sandbox** [built, G1]: the whole encoder (the driver's run loop,
   the core, the kernels compiled for the host) on the golden cases' command lines: streams
   and access units equal to the records (`crates/tb-m2venc/tests/golden_ref.rs`,
   `golden/fresh`), and every stream on its own merits through the gate's machinery
   (`gate/ref-small`, `gate/pipe-ref-small`).
3. **The gate on the card** [built]: `gate-check` (54 cases of the corpus at broadcast sizes,
   torture cases and features; `--quick` 15), `gate-pipe` (46 cases: device input, shedding,
   holds, sessions at run time, conversions), `gate-small` and `gate-golden`. Every case on its
   own merits: strict decode in ffmpeg and libmpeg2, both buffer models, reconstruction against
   decode, rate, quality against the stored baseline, and the conformance verifier
   (`tb-m2v-verify`) on every stream, which fails a case on any S1 or S2 finding and on an S3
   finding not allowed for that stream by `crates/tb-m2v-verify/tools/gate-allow.txt` (each
   allowance tied to an item of `docs/notes/post-port-worklist.md`; `docs/notes/m2v-conformance-findings.md`
   §8).
4. **Capacity on the card** [built]: `gate-bench` (free-running throughput and paced on-time
   fractions at the line-ups of §8.1), `gate-lineup` (the pipeline at real time), `gate-contend`
   (next to other users of the card).

How the Rust encoder was shown equal to the C++ one before the C++ was deleted (the same
streams on every case of the gates, byte for byte; capacity within noise) is history (§10).

**Design notes that came out of the comparison with the C++** (each was a deliberate
difference, measured and kept):

- Sessions open in the tick they are asked for: a batch is laid out in the arena at once
  (§4.1); there is no side thread that prepares batches.
- `tb-pipe` refuses inputs other than libav's decoders (`@sw`, the hwaccel path); the cuvid
  decoders were measured worse and not ported.
- `tb-pipe` makes the tap tables of the conversions its opened inputs will most likely need
  before the first tick in paced runs; offline runs make them when first used. The tables'
  contents are the same either way.
- `tb-pipe` passes captions (A53 side data of the inputs) to the encoder when an input has any
  (`--no-cc` turns it off).
- Software inputs (`@sw`): the scaled frame is copied from the decode thread's buffer into the
  session's frame region on the tick thread: one more copy of the frame on the host per channel.
- `tb-m2venc --input-mode dev` holds the clip on the device in the padded layout and copies
  whole padded planes.
- The first quantization plan and the finish plan are submitted per batch (`split_plans`), one
  plan per phase in replays.
- Frame times of a pass of a looping input count from the pass's first packet, not from the
  first frame the decoder returns (`tb_timeline::ingest::FrameTimes::packet_pts`): the other
  rule left an input late for good after a resynchronization across the end of the file.

### 7.3 Floating point: both builds compute the same bits [decided, measured]

`k_xf`, `k_quant`, `k_recon` and the temporal candidate and second field seed of `k_me` use
`float`. nvcc contracts a multiply and an add into one fused operation by default
(`-fmad=true`); the host build does not (`-ffp-contract=off`), and the C++ encoder built with
`-fmad=false` coded different streams (480i: 1,536,285 against 1,536,211 bytes). So the
reference adapter differed from the card in the last bit (K3, before: 82 output buffers of
three gate clips differ between the card and the unfused host build).

**Decision [decided, K3]: the fused operations are written in the source.** Every site where
nvcc's default contraction fused a multiply and an add is an `fmaf` in `kernels.h` (or
`fmaf_nc(a, b, c)` = `fmaf(a, b, -c)`, a PTX `neg` + `fma.rn.f32` on the card so that the
negation stays an operand modifier as nvcc's own contraction makes it), and every build
compiles without implicit contraction: the image with `-fmad=false`, the host
with `-ffp-contract=off`, where `fmaf` is libm's correctly rounded fused operation. The sites:
`fdct8` 14, `idct8` 26 (including the 8 outputs `e ± p/2`, where fusing is exact either way), `n_act` 2, `mb_qcode` 1, the two level roundings of
`k_xf`, the scaled field seed (2) and the temporal candidate (2) of `k_me`. They were found
from the SASS of the image (floating-point instructions per kernel with and without
`-fmad=false`: `k_xf` 42, `k_recon` 416, `k_me` 5, `k_quant` 3 fused operations; NVVM makes
part of them in the PTX, `ptxas` the rest) and checked the same way:

- SASS of the `-fmad=false` image from the new source against the `-fmad=true` image from
  the old: 15 of 17 kernels byte-identical (`k_quant`, `k_recon` among them); `k_xf`
  identical up to register numbers and one swapped pair of independent instructions; `k_me`
  the same multiset of floating-point instructions with their constants and modifiers, its
  PTX identical except that the temporal candidate's two multiply / `fma` / `floor`
  sequences sit in a later block and two `fma` constants go through a register.
- The C++ encoder from the new source (at the time): `check-full` 54 / 54, `check-pipe`
  46 / 46, and all 302 streams of the two gates byte-identical to the previous build's.
  `make bench` within noise (free-run macroblocks/s, old / new / old: 1080i 11.84 / 11.83 /
  11.82 M, 720p 13.16 / 13.13 / 13.13 M, 480i 10.93 / 10.86 / 10.87 M, mixed 11.68 / 11.66 /
  11.66 M; spreads 0.03 to 0.8 %).
- Card against reference adapter (`tb-device-gpu` test `float_parity`, in
  `nix run .#gpu-plumbing`; real content with `TB_PARITY_MEDIA`): one batch coding an I, a P
  and a B picture of crowd_run 480i, mobcal (bottom field first) 480i and park_joy 720p, two
  ticks (the second with the temporal candidate fed by the first), four sets of row
  quantizers through `k_quant` and `k_recon`: all 80 output buffers per clip identical. The
  image of the old source against the new host build: identical too. The new image against
  the old (unfused) host build: 82 buffers differ (`k_xf`, `k_quant`, `k_recon`), so the test
  sees the difference it guards against.
- On the host, `tb-device-ref`'s f32 port uses `mul_add` exactly at those sites and is equal
  to the kernels; the same port with every fused operation split differs from them
  (`host_fmaf_is_fused`).

Other sources of difference, checked: division is IEEE on both (`div.rn.f32`, nvcc's default
`-prec-div=true`; SSE `divss`); `floorf` is exact on both; no `sqrt`, `rsqrt` or other libm
call in the kernels (the `MUFU` instructions in the SASS are the integer-division and
division-slow-path sequences, identical in both images); no value reaches a denormal (all
operands are integers or products of integers with the DCT constants), and nvcc without
`-use_fast_math` keeps denormals anyway; float-to-int conversions are of values far inside
`int` range, with one exception: `n_act` is 0/0 = NaN when a macroblock's activity and the
average are both 0, where the card's conversion gives 0 and x86's `INT_MIN`; both are clamped
to the same quantizer index 1 by `mb_qcode`, and a NaN `m` fails `m >= 1` on both; the
SIMD-in-a-word shims (`__vsadu4`, `__vavgu4`, `__funnelshift_r`, `__dp4a`) follow the CUDA
definitions and `k_me`, `k_xf` (half-sample prediction) match on the card test above.

The AMD image keeps it too [measured, 2026-11-26, RX 7900 XTX, ROCm 7.2]: hipcc with
`-ffp-contract=off` contracts nothing the source does not write, `fmaf` is `v_fma_f32`
(correctly rounded), division is IEEE and denormals are kept by default, and the conversion of
NaN to `int` gives 0 as on the card. `float_parity` (`gpu-plumbing -- hip::`): every output
buffer of every clip identical to the reference adapter (synthetic content; the gate's clips
not yet run there). The golden cases through the binary (`generate.sh --device hip --check`):
every record and every stream equal to the committed ones (all but `deadline`, compared on no
card).

A change to the float arithmetic of the kernels must keep this: write any new multiply-add as
the card should compute it (`fmaf`, or a separate multiply and add), and the GPU test and the
f32 port of `tb-device-ref` show whether both builds still agree.

### 7.4 What is C++

The kernels (`crates/tb-kernel-abi/kernels/`), for good: CUDA C++ compiled by Nix into the device image and, through
the shim, for the host. Nothing of the host side.

### 7.5 Conformance: what cannot be configured and what is never emitted [decided, built]

The encoder cannot be configured into an illegal stream, and emits nothing illegal whatever
the content. Where each rule lives (clauses are in the code; the decisions and their reasons
in the comments next to them):

| Rule | Where | What happens |
|---|---|---|
| Sequence parameters: size, frame rate (H.262 Table 6-4), level, aspect, bit rate, buffer, buffer delay, color, the format table of the profile | `tb_m2v_syntax::profile::check`, called by `Encoder::open` | the session is refused with the clause; `SeqInfo` holds only checked values |
| Profiles | `SessionConfig::profile`: `h262` (tests), `a53`, `scte43-64qam`, `scte43-256qam` (default) | the broadcast profiles admit A/53 Part 4 Table 6.2 and differ in the rate ceiling |
| Vector range (Table 8-8) | `layout::f_code_for`, `ME_MAX_RANGE2` | a range that needs a vertical f_code above 5 is refused; the kernel clamps the range |
| Macroblock size (Table 8-6) | `k_ent_pack` to `PicStat::big_rows`, `RateControl::after_pass` | the rows named are quantized again, coarser; then the ladder |
| Buffer (Annex C, the operating model) | `RateControl`, for fixed quantizer too; `first_min` | a picture that does not fit goes down the ladder to the all-skipped picture; a session whose first picture could not fit is refused |
| No cut picture | `Batch::collect` | a picture larger than the output buffer is never accepted; if one appeared it would be given up, not written in part |
| progressive_frame implies frame_pred_frame_dct (A/53 Part 4 6.1.6) | `SessPar::frame_only`, `Session::frame_only` | such a picture is coded without field tools and says so |
| Caption user data (CTA-708-E 4.3; no start code emulation, H.262 6.3.4.1) | `tb_m2v_syntax::captions::validate`, called by `Encoder::submit` | the frame is refused with the clause |
| Sequence headers | only before I pictures | a P picture that replaces an I picture, and a repeat, carry none |
| Display of a picture: repeat_first_field only where H.262 Table 8-7 has it and only with progressive_frame 1 (6.3.10); output fields alternate (7.12); cc_count of the picture's display time (CTA-708-E 4.3.6) | `Shown::check` and `captions::validate_shown`, called by `Encoder::submit_shown`; `write_headers` | the frame is refused with the clause (§7.6); the three flags written always agree |

The proof is the strict verifier (`tb-m2v-verify`, written from the standards and not from the
encoder): in the sandbox on the reference adapter (`verify/ref-streams`: the groups clean,
broadcast and refused of `tools/ref_cases.sh`; the end-to-end tests
`crates/tb-m2venc/tests/conformance.rs`), and on every stream of every gate case on the card
(`tools/gate/conform.py`). No finding is allowed anywhere: `tools/gate-allow.txt` is empty.

### 7.6 Film mode: flagged pulldown [decided, built]

A 23.976 or 24 Hz source on a 29.97 Hz interlaced channel (1080i, 480i) or a 59.94 Hz
progressive one (720p) used to be converted before coding: fields woven in the 3:2 pattern,
frames repeated. In film mode the encoder codes the film frames only, each as a progressive
frame picture that says how long it is shown (repeat_first_field with top_field_first, H.262
6.3.10), and the decoder does the pulldown. The mechanism is per picture, with no change of
sequence parameters, so a channel goes from film to video to a still picture by picture.
In brief:

- **Who decides** [decided]: the timeline, not the encoder. `tb_timeline::film::Assembler`
  takes the selection per display unit (a field of an interlaced channel, a frame period of a
  progressive one) and gives pictures: three units of one instant, two, or the fallback (two
  fields of two instants woven into an ordinary interlaced picture; one frame period). It has
  no cadence model: each picture is decided from the units at the head of its queue, so a cut,
  a dropped or repeated frame, the slate or video cost only the picture at the break. Latency:
  two frame periods at most.
- **The encoder** takes the display with each frame (`Encoder::submit_shown`), refuses what
  the stream could not legally say (§7.5), and guarantees that display time is covered exactly
  once whatever is submitted: a picture that is given up (deadline policy, hold, failed plan,
  lost device) becomes a repeat with the display of its frame.
- **A tick without a picture** is not a miss: a film session has no frame in one tick of five
  (three of five at 720p), and a session without a queued frame is no candidate of the
  deadline policy and costs nothing.
- **Time** is counted in display units, as integers: the display clock gives PTS, the key
  frame grid and the GOP time code; the decoding clock (the intervals of H.262 Annex C.9 and
  C.11) gives DTS and the time of the buffer model. Stamps are functions of a unit count, not
  sums, so nothing drifts; a B picture has PTS = DTS.
- **Rate control** budgets a GOP by its display time; the statmux controller gets each access
  unit's display time (`AccessUnit::units`) and must count complexity and bits per unit of
  time, not per picture.
- **Per channel** (`tb-pipe --ch ...,film=auto|off`; the product's channel configuration will
  carry the same switch): `auto` flags what the timeline shows as film, `off` codes one
  picture per frame period as before. Statistics: pictures by display time, cadence breaks by
  what was seen at them (`FilmStats`), per session `SessionStats::shown`.
- **720p59.94** uses the same flags in a progressive sequence (frames shown two and three
  times; legal by H.262 Table 8-7 at 59.94 and 60 Hz, and A/53 Part 4 and SCTE 43 add no
  restriction). It is built and gated, and off unless a channel asks for it: receivers have
  seen it far less than 3:2 on 1080i and 480i (the log, "720p").
- **What it buys** [measured on the card, the log has the tables]: against the coded pulldown
  on the same 23.976 Hz inputs, PSNR-Y of what is displayed rises by 1.8 to 2.7 dB at 1080i,
  1.0 to 2.1 dB at 480i and 0.7 to 1.3 dB at 720p at equal rate, or the rate falls by 32 to
  42 %, 22 to 33 % and 13 to 18 % at equal quality; a film channel costs about a quarter less
  device time at 1080i and 480i and about half at 720p. Capacity of sessions without film
  pictures is unchanged, and their streams are byte for byte what they were.
- **What proves it**: the strict verifier, whose rules of this family were first confirmed on
  other encoders' soft-telecined streams and by mutations (`docs/notes/m2v-conformance-findings.md`
  §6); property tests of the assembler, of the encoder on the model device, and of both
  together; the reference adapter end to end (`tb-m2venc/tests/film.rs`, golden cases `film`
  and `film60`, the small gates); `gate-film` on the card, which checks the cadence on two
  decoders and the displayed fields sample for sample (`tools/gate/film_check.py`).

## 8. Threading and runtime at the edges [proposed]

The core has no threads. The edges follow SPEC §3 and the scheduler shape measured in the
spike (§C.7):

- **One tick thread** owns the `GpuDevice` (the context is current there; the type is not
  `Send`) and the `Encoder`. At the field rate it runs: frame clock per channel, conversion
  steps, `begin_tick` → (`submit`, `wait`, `resume`)* → access units to the mux through a
  bounded channel that never blocks the tick (a full queue drops and counts).
- **Time** enters as arguments: `now` and `deadline` to `begin_tick`, completion times in
  `Completion`. The adapter owns the clock and the waiting strategy.
- **Decode threads**, one per input, hand frames to the tick thread. Decode on the card
  (NVDEC through libav in the same context) is a second adapter-side concern; its surfaces
  enter plans as device regions outside the arena (an `Op` for a foreign source pointer is
  the one protocol extension C1 / P1 must design).
- **No async runtime** below the control plane. `tokio` serves the control API, the device
  supervisor and the probe (SPEC §3); it never drives a tick.

### 8.1 Budget

The Rust host must not cost measurable capacity. What the C++ host costs [measured, spike
notes §0.4, profile mode]:

| Line-up | Rate control between phases | Other host work per tick | Tick | Idle in the tick |
|---|---|---|---|---|
| 16 × 1080i | 0.04 ms | 0.09 ms | 6.88 ms | 0.37 ms |
| 96 × 480i | 0.14 ms | 0.18 ms | 7.08 ms | |
| 8 × mixed (48 sessions) | 0.11 ms | 0.07 ms | 4.98 ms | 0.23 ms |

That is about 0.03 ms per tick plus 3 µs per session. Budget for the Rust host [proposed]:
host work between phases at most 10 % above these figures (0.15 ms at 16 × 1080i, 0.35 ms at
96 × 480i); building and validating a plan at most 20 µs per batch; no allocation in
`begin_tick` / `resume` after warm-up (checked in E4 with a counting allocator in a test);
host CPU at capacity within the C++'s 0.14 to 0.19 core. Acceptance in G2 is by capacity
(`cap99` per format, old / new / old back to back, as spike notes §2 requires), not by these
figures.

Known costs of the protocol against the C++: one more copy of each launch's arguments (into
the plan, then into the driver; about 400 bytes per launch, about 20 launches per batch and
tick), and plan validation per submit (bounds checks over the same steps).

Measured [D1, 2026-11-25, RTX 2000 Ada, driver 595.71.05, card idle by `nvidia-smi pmon`
before and after; `nix run .#gpu-bench`, release build; medians of 2000 repetitions of a
plan, three runs agreeing within 10 ns]: host time of `CudaDevice::submit` per step against
the same driver calls made directly with the addresses worked out beforehand, as the C++ makes
them.

| Plan (32 steps; the mix 30) | `validate` | `submit` | direct calls | Protocol overhead |
|---|---|---|---|---|
| upload 4 KiB | 3 ns | 1.98–2.00 µs | 1.89–1.91 µs | 84–93 ns |
| launch, 14 scalar arguments (`k_pyr`) | 34 ns | 2.12–2.15 µs | 1.95–1.97 µs | 170–182 ns |
| launch, `Geo` + `Bufs` by value (`k_me`) | 43–45 ns | 2.14–2.17 µs | 1.97–1.98 µs | 163–191 ns |
| record + wait for event | 2 ns | 95 ns | 88 ns | 7 ns |
| one batch's begin phase (fill, 4 + 8 uploads, fence, 10 launches, 4 downloads) | 19 ns | 1.87–1.89 µs | 1.71–1.72 µs | 148–169 ns |

So the protocol adds about 0.1 µs per upload and 0.18 µs per launch to driver calls that cost
about 2 µs each: 5 to 9 %, at most about 0.19 µs per step, or about 5 µs per batch and tick
for the roughly 30 steps a batch queues per tick (golden case `cbr`: 869 operations in 30
ticks); per-session frame uploads add about 0.09 µs each (26 µs at 300 × 480i). Against the budget above (20 µs per batch for building and validating a plan; host work
between phases at most 10 % above the C++'s 0.15 to 0.35 ms) the adapter's share fits; the
core's plan building is E4's to measure. Hybrid against blocking wait on a 1.7 ms plan (the
same bench): 1.713 ms against 1.776–1.786 ms from submit to return (the wake-up after the
device finishes falls from about 75 µs to about 10 µs), at 0.32 ms of host CPU per wait
against 0.04 ms: the trade of optlog experiment 19, reproduced.

On the AMD card [measured, 2026-11-26, RX 7900 XTX, ROCm 7.2, `gpu-bench -- --runtime hip`]:
driver calls are cheaper (0.8 to 1.6 µs per step) and the protocol adds 0.1 to 0.28 µs per step,
as on CUDA. HIP's blocking wait spins before it sleeps, by design (ROCm's source): 100 µs of
active wait in HIP (`WaitForSignal`, `rocclr/device/rocm/rocvirtual.hpp`), then up to 200 µs of
`mwaitx` in ROCr (`InterruptSignal::WaitRelaxed`), then a sleep in the kernel until the card's
interrupt (`hsaKmtWaitOnEvent`). So a wait shorter than about 0.3 ms costs its whole length in
CPU, and a longer one about 0.3 ms plus the wake-up: on a 16 ms plan, 1.1 ms of host CPU per
wait (submit included) with the blocking wait, 2.6 ms with the hybrid one, whose polling after
the nap is the trade of optlog experiment 19 as on CUDA. (`ROC_ACTIVE_WAIT_TIMEOUT=0` changes
nothing: it governs another path.) Not a core per tick thread; how it compares with CUDA at
capacity is to be measured with the encoder [open].

Measured [G2, 2026-11-25, the encoder alone from host memory, next to Plex transcodes; spike
notes §R.2 has the tables]:

| Line-up | Host between phases (budget) | Host queueing plans | Tick (free run) |
|---|---|---|---|
| 16 × 1080i | 0.13 ms (0.15) | 0.20 ms | 6.7 ms |
| 16 × 720p | 0.07 ms | 0.11 ms | 2.6 ms |
| 96 × 480i | 0.32 ms (0.35) | 0.28 ms | 7.2 ms |
| 8 mixed units (48 sessions, 3 batches) | 0.16 ms (0.20) | 0.29 ms | 4.1 ms |

- Capacity by the acceptance of this section: at the C++'s capacity points (67 × 1080i, 75 ×
  1080p, 90 × 720p, 400 × 480i, 24 mixed units) and the two counts above each, both binaries
  keep at least 99 % of ticks on time in paced 20 s runs; mean ticks differ by −0.5 to
  +0.4 %; host CPU 0.16 to 0.21 core for both.
- Free-running throughput, final binaries, card otherwise idle: 0.995 to 1.006 of the C++ by
  round over the five line-ups (spreads 0.2 to 2.5 %). It was about 1 % behind in every
  format until the cause was found: not plan building or marshaling, but the wait of the
  plan without steps (§5.2), where the hybrid wait's nap overshot. Two more differences were found by profiling and
  removed: phases are now submitted per batch (§4.2), and a launch's arguments are resolved
  without an intermediate list (`Kernel::resolve`; `memmove` was 6.4 % of the driver's CPU
  samples against 3.9 % in the C++).
- What did not matter at these sizes: pooling the access units' bytes (`Encoder::recycle`),
  the event dependency in place of the host's wait for the upload stream (`frames_event`).

## 9. The rest of the server [built 2026-11-26]

The same rule: logic in pure crates, I/O in adapters, binaries at the top. What was built, with
the SPEC section each implements; `docs/SERVER.md` describes how they run together.

| SPEC | Pure crate | Adapter / binary |
|---|---|---|
| §5.1, §6 mux: slot clock, PCR, PES / TS packetizer, T-STD admission, slot scheduler | `tb-ts`, `tb-mux` | `tb-output` (framing, sinks, pacer) |
| §7 PSI / PSIP: sections, descriptors, carousel, guide windows, versions | `tb-psip` (carousel in `tb-mux`) | none |
| §8.4, §8.5 statmux controller, lower-before-raise | `tb-statmux` | in the video hub |
| §5.3, §5.4, §9.5 timeline, frame clock, fallback | `tb-timeline` | `totalblonder::source`, `totalblonder::live` |
| §8.3 audio | `tb-audio` (sample clock, loudness), `tb-ac3` (encoder) | `totalblonder::audio` |
| §9 ingest | `tb-media` (contract), `tb-rtmp`, `tb-tsin` | `totalblonder::ingest` (sockets) |
| §9.3 decode | `tb-decoder-proto` | `tb-sandbox`, `tb-avworker` (the only binary linking libav), `tb-decoder-client` |
| §7.7 captions | `tb-captions` | in the video hub |
| §10 modulator client and supervisor | `tb-eqam` | `totalblonder::device` (HTTP/1.0) |
| §11 probe and self-checks | `tb-probe`, `tb-ts-verify` (independent of `tb-ts`/`tb-psip`/`tb-mux`) | CLI `tb-ts-verify`; the probe's adapter waits for the HDHomeRun |
| §13 control API, configuration, persistence | config types | `totalblonder` binary (control API on std, JSON version file) |

`tb-m2v-encoder` is driven by the video hub (`totalblonder::video`) through `tb-engine`, one
session per program. libav never runs in the server process: the decoder is a sandboxed child
(§5.5), and the encoders (video on the GPU, AC-3 in Rust) do not use it.

## 10. History

The encoder began as a C++ spike on nixark: a host program (`m2venc`, its encoder `enc.h`, the pipeline
harness `m2vpipe` with its decoder threads `dec.h` and conversion `pipe.h`) and the kernels.
Its host side was ported to this workspace in work packages:

```
in parallel (CPU):        S1  R1  E1  E2  E3  D2  K1  C1  T1
after R1 (CPU):           R2
after S1 R2 E1 E2 E3 D2:  E4   (CPU: replay, then reference adapter)
GPU, serial:              D1 → K2 → K3 → G1 → G2 → X1
```

| Package | What |
|---|---|
| S1, R1, R2, E1, E2, E3, E4 | Syntax, buffer model, rate control, GOP structure, layout, deadline policy, the tick |
| D1, D2, K1, K2, K3 | CUDA adapter, device simulator, reference adapter, conversion kernels, floating-point parity |
| C1, T1 | Conversion planning, timeline |
| G1 | Driver and gate: `check-full` 54 / 54, every coded stream byte-identical to the C++ encoder's |
| G2 | Capacity within noise of the C++ (0.995 to 1.006 free-running, the C++'s paced capacity points held), the pipeline harness, `check-pipe` 46 / 46 with 212 of 228 streams identical (the 16 others the C++'s run-time sessions opened from its side thread, §7.2) |
| X1 | The C++ retired: the verifier in the gate, the golden recorder in Rust (the C++'s records reproduced before the switch), kernels to `crates/tb-kernel-abi/kernels/`, the gate to `tools/gate/`, `tb-engine`, the C++ deleted; the gates' 308 streams byte-identical before and after, the device image identical but for its recorded source path |

Comments in the crates name the C++ functions a part was ported from (`Batch::begin`,
`Session::enqueue`, `Encoder::step`, …). The C++ itself and the measurements of the spike are
not part of this repository.

## 11. Risks and open decisions

1. **Floating-point parity of the reference adapter** (§7.3): resolved in K3 without a change
   to the device code; new float arithmetic in the kernels must follow the same rule.
2. **No second implementation to compare with.** Since X1 the encoder is checked on its own
   merits (two decoders, two buffer models, the conformance verifier, quality baselines) and
   against its own recorded past (golden vectors, the gates' stream digests). A change that
   alters output bytes is reviewed by those, not by agreement with another encoder.
3. **Device errors as values** (§4.5) are built in E4 and tested with injected faults; the C++
   gives no behavior to copy, and no fault has been seen on the card yet. Open: whether a
   `Failed` plan should retry once before its pictures become repeats.
4. **`WaitFor(FRAMES)` instead of a host wait** on the upload stream: built as an option in
   G2 (`EncoderOptions::frames_event`), off by default; the driver's switch for it went with the
   C++ in X1. Measured on the card next to Plex transcodes: no difference beyond the noise of
   the runs (§8.1).
5. **Foreign device memory**: designed and built in K2 / G2 (§4.2): handles in job records,
   foreign slots on the adapters, one lifetime rule. A batch made at run time is cleared by
   the encoder's next plan, after a caller's own plan could already have written a
   `FrameSource::Device` frame into it: `Encoder::frame_target` refuses until the batch is
   set up, so a channel added to a running pipeline costs one `settle` (G2 notes, "Hazards").
6. **Own CUDA bindings** mean reading `cuda.h` on a toolkit change. The functions used have
   not changed signature since their `_v2` forms; a driver newer than the toolkit pin is the
   normal case and is supported by NVIDIA's compatibility rule.
7. **cbindgen** reads one file and does not evaluate expressions in array lengths
   (`VLC_DCT_CODES` is a literal with a compile-time assertion). If the ABI crate outgrows one
   file, the generator is replaced by a small emitter over a declarative record list.
8. **Evaluation cost**: breakbulk plans by import-from-derivation; the first evaluation on a
   machine builds its tools. Minutes once, seconds afterwards [measured on nixark].
9. **The reference adapter is slow** (one host thread, fibers for three kernels). It is for
   176x144 tests. If it is too slow for E4's cases, blocks can run on a thread pool: the
   launcher already keeps its state per thread.
10. **Golden coverage**: since X1 the vectors include rung 37 and I pictures replaced by P,
    the real-deadline path (on the reference adapter's step clock), arena churn,
    device-written frames and two formats. Not covered: device faults (the replays inject
    them), the driver's own copies of device-written frames, captions through the pipeline.

Decisions the owner may want to revisit: crate prefix `tb-` and names; Rust as the ABI source
(§3); own CUDA bindings (§5.2); dropping the driver-allocation and side-thread paths for
batches (§4.1); the committed generated header.
