# The sandboxed decoder: log

Package of 2026-11-26 (overnight, unattended). Brief: libav must not run in the server
process; it runs in a worker process with Linux sandboxing and fault supervision, fed by the
plain-data contract of `tb-media`, frames back through shared memory, everything it says
validated. Design: `docs/ARCHITECTURE.md` §5.5. This log records what was built, decided and
measured, and what is left.

Machine: bagel (no GPU), Linux 7.2.15, Landlock in the LSM list (kernel ABI 10; we use 6),
unprivileged user namespaces allowed, yama `ptrace_scope` 1. FFmpeg libraries: nixpkgs
`ffmpeg-headless` 9.0.2 (linked by the worker); the `ffmpeg` program on PATH is 6.1.6 (fixtures
and references only).

## 1. What was built

| Crate | Kind | What |
|---|---|---|
| `tb-decoder-proto` | pure (`forbid(unsafe_code)`) | The wire protocol (versioned, length-checked, every field range-checked), the input ring's accounting, output pools and the validation of every frame descriptor (`ValidSlot` only from validation, consumed on release), per-stream output specs and their validation, the supervisor (backoff with jitter, circuit breaker), the watchdog, exit classification |
| `tb-sandbox` | adapter (`unsafe` for `clone3`, `execve`, `fork`, fd set-up, `mmap`) | `spawn(Command, ipc_fd, Policy) -> Child`: namespaces, minimal root, rlimits, fds; `child::enter()` / `Entered::lock_down()` for the sandboxed program: PID-namespace init, death signal, not dumpable, Landlock, seccomp; per-layer reports; pidfd supervision; `shm` (sealed memfds, mappings); `ipc` (SEQPACKET with `SCM_RIGHTS`); `probe` (attempts for tests) |
| `tb-avworker` | binary, the only thing that links libav | libavcodec decoders for an allow-list of codecs, parsers for Annex B / ADTS / ES input, frames into slots; optional converted video (libswscale) and resampled audio (libswresample); fault injection |
| `tb-decoder-client` | adapter (no `unsafe`, no libav) | `DecoderPool::spawn(name) -> Decoder`; `try_send(InputEvent)` never blocks; `poll(deadline) -> Option<Decoded>`; `events()`; `AsFd` for epoll; frames as `VideoFrame` / `AudioFrame` guards on their slots |

Why a separate client crate (rather than inside `tb-sandbox`): `tb-sandbox` is generic process
confinement and knows nothing of decoding; the client is the decoder's protocol driver. The
server depends on `tb-decoder-client` only; nothing that the server links links libav.

### Public API (parent)

```text
DecoderPool::new(PoolConfig) -> Result<DecoderPool>       // worker path, Policy, RestartPolicy,
DecoderPool::spawn(&self, name) -> Result<Decoder>          // WatchdogConfig, Limits, ring and
Decoder::try_send(InputEvent) -> Result<(), SendError>      // slot sizes, threads, gpu, seccomp
Decoder::try_send_streams(Vec<StreamOpen>)                  // per-stream output spec
Decoder::set_video_output(Option<VideoSpec>) / set_audio_output(Option<AudioSpec>)
Decoder::poll(Option<Instant>) -> Option<Decoded>           // frames; None at deadline or event
Decoder::events() -> impl Iterator<Item = Event>            // Started, Exited{class}, Restarting,
Decoder::stats() / confinement() / wait_ready() / shutdown()//  CircuitOpen, Healthy, Violation,
impl AsFd for Decoder                                       //  StreamRefused, Drained, Fatal
Decoded::{Video(VideoFrame), Audio(AudioFrame)}             // desc + slot guard (drop = release)
```

`SendError`: `Full` (ring or socket full: dropped, counted), `Unavailable` (no worker),
`TooLarge`, `UnknownStream`, `AwaitingKeyframe` (video after a restart), `BadStreams`.

## 2. Decisions

- **No thread inside the client.** A `Decoder` is owned and driven by the caller's thread
  (SPEC §3: one ingest-and-decode thread per input). Every call does what is due; `poll`
  waits on an epoll set (socket, pidfd, stderr pipe, a timerfd for the watchdog and the
  supervisor, an eventfd woken by frames dropped on other threads). A caller with several
  decoders can epoll their descriptors.
- **Packets through a shared ring, not the socket.** A key frame at 30 Mbit/s can exceed a
  socket buffer (~208 KB); the ring (8 MiB default, half of it the largest packet) takes one
  copy and a 40-byte message. Credit is the worker's `Consumed`, sent as soon as it has copied
  a packet into its `AVPacket`. The ring's memfd is sealed against resizing and against new
  writable mappings (`F_SEAL_FUTURE_WRITE` after the parent mapped it): the worker can only
  map it read-only.
- **Output pools made and sealed by the parent.** The worker asks (`NeedPool`), the parent
  checks the size against limits, makes a memfd, seals `SHRINK|GROW|SEAL`, maps it read-only
  and grants it with the descriptor. A worker can therefore never shrink a pool under the
  parent (which would `SIGBUS` the server). One active pool per stream; a format change
  retires it (`RetirePool`), slots the caller still holds stay valid until dropped.
- **Slots are credit.** The worker writes only free slots; a slot is the parent's from the
  frame message until the guard is dropped. With no free slot the worker waits
  `slot_wait_ms` (20 ms default) and then drops the frame (`dropped_no_slot`), so a slow
  consumer costs frames, not input, and never blocks the worker for long.
- **Control messages queue, packets drop.** `Streams`, `End`, `Release`, `PoolGrant`, `Ping`
  are queued when the socket is full and go first; packets are refused while anything is
  queued. A queue beyond 4096 messages kills the worker (stalled).
- **Validation, and a violation kills.** Every descriptor: pool known and not retired, slot in
  range and the worker's, stream announced and of the pool, size within limits, plane count
  of the format, strides at least a row, planes inside the slot and disjoint, captions whole
  triplets, timestamps within 10 s of the range of the packets sent on the stream, audio rate
  and channels in range and fitting; converted output matching its spec exactly (size,
  unpadded planes, limited range, matrix; 48 kHz and the layout's channels). Also: messages
  before `Hello`/`Ready`, a second `Hello`, a `Pong` for no ping, `Ended` without `End`,
  credit beyond what was written, any descriptor from the worker, a truncated datagram. The
  worker is killed (`Killed(Violation)`), the event says what.
- **Shared-memory reads.** Frames are read in place (zero copy). A broken worker could keep
  writing a slot it handed over; that can only change pixel values, never sizes or anything
  validated (copied into the parent first). Documented in `tb_sandbox::shm`.
- **seccomp: kill, not EPERM, for calls outside the list** (production default
  `SeccompMode::Kill`; `Errno` for tests). A compromised or unexpected path should crash, be
  classified `SeccompViolation` and counted, not probe at leisure. Exception: `open*` and
  `creat` fail with `EPERM` in both modes (glibc opens `/proc/sys/vm/overcommit_memory` and
  `/sys/devices/system/cpu/online` on rare paths and copes with failure); `clone3` fails with
  `ENOSYS` so glibc falls back to `clone`, whose flags the filter checks (`CLONE_THREAD`
  required: threads yes, processes no). Three stacked programs; the allow-list is installed
  last because it does not allow `seccomp(2)` itself (found the hard way: installing it first
  made the next install a `SIGSYS`).
- **seccomp after libav's initialization.** The worker opens the CUDA device (GPU mode) and
  allocates its frames before `lock_down`; decoders are opened later, under the filter (their
  threads, allocations and, with swscale/swresample, conversions all ran under the kill-mode
  filter in the tests).
- **PID namespace with an init.** The program is PID 1 of its namespace after `clone3`; it
  forks at once in `child::enter` and the parent half becomes the namespace's init (death
  signal, not dumpable, Landlock with no rights, a seccomp filter of a dozen calls), which
  reaps and writes the program's exit status on the status pipe. Reasons: a PID 1 ignores
  signals it has no handler for, so a crash inside PID 1 would not be a crash; killing the
  init through the pidfd kills the whole namespace.
- **Death signal after `execve`, not before**: `execve` clears `PDEATHSIG` when it drops the
  capabilities of the user namespace. The parent's pid is also checked (`TB_SANDBOX_PPID`) to
  close the race without a PID namespace; with one, socket EOF ends the worker anyway.
- **Landlock in the program, not before `execve`.** Built with the `landlock` crate in a
  normal process (allocation is fine there) before any untrusted input; rules: read+execute
  beneath each read-only path (file-only rights on files, else the ruleset is only partially
  enforced), read+write+ioctl on device nodes, nothing else; TCP bind/connect handled (none
  allowed); scopes (abstract UNIX sockets, signals) at ABI 6.
- **The minimal root.** tmpfs on the first usable directory of `/tmp /mnt /run /var/tmp /opt
  /srv /home /dev` that holds no bind source (the Nix build sandbox may have no `/tmp`),
  `/nix/store` bound read-only with the source mount's locked flags kept, the program at
  `/exe`, `/proc` and devices only for GPU mode, `pivot_root`, old root detached, the root
  remounted read-only. `/nix/store` whole rather than the worker's closure: the store holds no
  secrets by Nix's design, and a closure list would have to be computed per build; the
  `ro_paths` list allows narrowing later.
- **Exit codes as classification.** 86 out of memory (a global allocator that `_exit`s on a
  failed allocation, and libav's `ENOMEM`), 87 protocol error seen by the worker, 88 set-up
  failure. `SIGSYS` seccomp, `SIGXCPU` CPU limit, other signals crash, `SIGKILL` with a reason
  of ours killed.
- **Restart policy defaults**: backoff 100 ms doubling to 10 s, jitter 20 %, healthy after
  10 s (resets backoff, closes the circuit), circuit opens after 5 failures in 60 s for 60 s,
  then one trial start (a failed trial reopens it at once). Watchdog: ready within 5 s, ping
  every 500 ms answered within 2 s, no progress for 2 s with work pending kills.
- **After a restart** the client sends the last `Streams` again and drops video packets until
  a key frame (`AwaitingKeyframe`, counted).
- **Native output**: planar 8-bit 4:2:0 (and 4:2:2 with chroma rows averaged), NV12, P010
  (and planar 10-bit packed to P010), rows aligned to 64 bytes; f32 planar audio at the native
  rate. Other pixel formats are counted unsupported. No scaling in native mode.
- **Converted output** (the lead's addition of the night): per stream, in `Streams`.
  Video: libswscale Lanczos + accurate rounding to the program's size, planar 4:2:0 unpadded
  (the encoder's `FrameSource::Host` layout), limited range, BT.709 from 720 lines and BT.601
  below (source matrix from the stream, else guessed by its height), `Fit` (letter/pillarbox,
  black Y 16 / UV 128, by the source's SAR against the target's display aspect) or `Stretch`;
  bob deinterlacing of interlaced pictures (each field read as every other row, scaled to the
  full height, no half-line compensation; the second picture half a duration later).
  Audio: libswresample to 48 kHz f32 planar, stereo or 5.1 in A/52 order (L C R Ls Rs LFE,
  reordered from libav's native FL FR FC LFE SL SR); timestamps the input's less the
  resampler's delay; flushed at `End`.
- **Codec allow-list**: H.264 (`h264`), HEVC (`hevc`), MPEG-2 (`mpeg2video`), AV1
  (`libdav1d`, `av1` for hardware), VP9 (`vp9`), AAC, AC-3, E-AC-3, MPEG audio (layer from the
  first frame header: `mp1float`/`mp2float`/`mp3float`), Opus; PCM s16le converted without
  libav; anything else (timed text, codecs the wire cannot name) refused with a
  `StreamRefused` event.
- **Linked**: libavcodec, libavutil, libswscale, libswresample (libswresample is also a
  dependency of libavcodec). Not libavformat, not libavdevice, not libavfilter (`ldd`).
- **Fault injection** (hidden, `PoolConfig::fault`): crash after N packets (`SIGSEGV`, the
  default action restored first because Rust's runtime handles it), hang, spin, a descriptor
  naming a slot out of range, unbounded allocation (address space only, so it costs no RAM),
  and a sandbox probe before and after the filter.

## 3. Confinement achieved on bagel, layer by layer

Reported by the worker's sandbox and logged by the client (`Event::Started`), with the default
policy:

| Layer | State | Evidence (probe from inside the worker, before the seccomp filter, unless noted) |
|---|---|---|
| user namespace | on | own uid/gid mapped to themselves; no capabilities after `execve` |
| mount namespace, minimal root | on | `open("/etc/passwd")` = ENOENT; creating a file at `/`, `/tmp`, `/nix/store` = EROFS |
| PID namespace | on | `kill(server_pid, 0)` = ESRCH; killing the init through the pidfd ends the program (pipe EOF) |
| network namespace | on | UDP to 192.0.2.1 = ENETUNREACH; abstract UNIX socket of a listener outside = ECONNREFUSED |
| IPC, UTS namespaces | on | (no probe; host name `tb-sandbox`) |
| no_new_privs | on | required by seccomp and Landlock, both in effect |
| close fds | on | only 0-5 survive `execve` |
| rlimits | on | NOFILE 64, CORE 0, AS 4 GiB (1 GiB in the allocation test: `OutOfMemory`) |
| death signal | on | |
| not dumpable | on | `ptrace(ATTACH, parent)` = EPERM |
| Landlock | on, ABI 6 (kernel offers 10) | without namespaces: `/etc/passwd` = EACCES, signal to the server = EPERM (scope), abstract connect = EPERM (scope) |
| seccomp | on, kill | after the filter in errno mode every probe is EPERM: `/etc/passwd`, file creation, `socket`, `connect`, `ptrace`, `kill`, UDP, `execve("/bin/sh")`; in kill mode `socket()` ends the worker with `SIGSYS` (`SeccompViolation`) |

Without user namespaces (checked under `bwrap --disable-userns`): the six namespace layers are
reported `UNAVAILABLE (errno 28)` and logged as REDUCED confinement; no_new_privs, fds,
rlimits, death signal, not dumpable, Landlock (ABI 6) and seccomp still hold; every decode,
conversion and fault test passes. A policy that requires a namespace refuses to start a worker
there (`Event::Fatal`).

In a root without `/tmp` (bwrap, nested user namespaces allowed): the new root is built on
`/dev`; every test passes. In the real Nix build sandbox (`nix build` of the check
derivations `tb-sandbox/test`, `tb-avworker/test`, `tb-decoder-proto/test`,
`tb-decoder-client`): every test passes; the ffmpeg-dependent ones skip (no `ffmpeg` there).

## 4. Test evidence

`cargo nextest run -p tb-decoder-proto -p tb-sandbox -p tb-decoder-client -p tb-avworker`:
64 tests (30 + 13 + 0 + 21), all passing on bagel; 3 ignored (`bench` twice, `gpu`: passing on nixark, §6).

- **Protocol** (`tb-decoder-proto`): round trips of every message both ways; every
  truncation and an extra byte refused; bad magic, version, booleans, counts above bounds,
  zero rationals; ring wrap, credit, rollback; slot ownership (twice, out of range, retired,
  wrong stream or kind), plane bounds, overlap, stride, plane count, dimensions, captions,
  timestamps window, audio bounds, spec matching; backoff (exponential, capped, jittered
  within bounds), circuit breaker (opens at K in the window, half-open trial, reopen, close
  when healthy), watchdog (start-up, heartbeat, stall only with work pending), exit
  classification.
- **Sandbox** (`tb-sandbox/tests/confine.rs`): layers and denials as in §3; kill mode is
  `SIGSYS`; the program's own crash (`SIGSEGV`) and exit code come through its init; kill via
  pidfd ends the namespace; a missing program is `SetupFailed(Exec, ENOENT)`; bad policies
  refused.
- **Decode** (`tb-avworker/tests/decode.rs`): 60 pictures of 320x240 H.264 and 2 s of stereo
  AAC through the sandboxed worker (seccomp kill mode): every picture bit-exact against
  `ffmpeg -f rawvideo -pix_fmt yuv420p` of the same stream, timestamps `i * 3000`, audio
  sample count equal and samples equal (maximum difference 0) to `ffmpeg -f f32le`; no drops,
  no violations. Slots: a pool of 2 slots for 60 pictures completes (slots come back); a frame
  held across the worker's shutdown stays readable and unchanged. Refused codecs reported.
- **Converted** (`tests/convert.rs`): 640x360 BT.709 to 704x480 4:3 `Fit`: 60 black rows
  above and below exactly (Y 16, UV 128), mean absolute difference to ffmpeg 6.1's
  `scale=704:360:flags=lanczos+accurate_rnd:in_color_matrix=bt709:out_color_matrix=bt601,pad`
  of **0.000** over 30 pictures; stretch to 1280x720; bob of 20 interlaced 720x480 pictures
  gives 40 with timestamps 1500 apart; stereo AAC to 5.1: tones on L and R (A/52 slots 0 and
  2), C Ls Rs LFE silent; 44.1 kHz to 48 kHz: 90112 samples in, 98082 out (98081 expected).
- **Faults** (`tests/faults.rs`), each with the parent running on:

  | Injection | Classified | Then |
  |---|---|---|
  | crash after 5 packets | `Crash(11)` | restart after ~50 ms, generation 2 ready |
  | hang | `Killed(Stall)` (watchdog, 1 s) | restart |
  | spin | `Killed(Stall)` | restart |
  | descriptor naming slot 60000 | `Violation("unknown pool 0")`, `Killed(Violation)` | restart |
  | unbounded allocation, AS 1 GiB | `OutOfMemory` (exit 86) | restart |
  | crash every 2 packets, breaker 3 | 3 × `Crash(11)` | `CircuitOpen(60 s)`; `try_send` = `Unavailable` |
  | `socket()` after lock-down, kill mode | `SeccompViolation` | restart |
  | probe, errno mode | see §3 | |

## 5. Measurements

1080p30 H.264 (libx264 medium, 8 Mbit/s, no B pictures, 600 pictures = 20 s), software
decode, one decoder thread, as fast as it goes, release build, bagel
(`cargo test -p tb-avworker --release --test bench -- --ignored --nocapture`), two runs:

| Path | frames/s | CPU per 1080p30 stream |
|---|---|---|
| in-process `tb-decode`, decode only | 290-323 | 0.092-0.102 cores |
| in-process `tb-decode`, + swscale copy to host | 288-299 | 0.099-0.103 cores |
| **sandboxed worker, native frames** | 284-285 | **0.104-0.105 cores** (worker) + **0.002** (server thread: feed, validate, poll) |
| sandboxed worker, converted to 704x480 (Lanczos) | 157-160 | 0.186-0.189 cores + 0.002 |

The sandbox costs about 2-5 % of decode CPU (one copy into the ring, one into the slot, the
messages) plus 0.002 cores in the server. Lanczos with accurate rounding to SD costs about
0.08 cores per stream; a cheaper filter, or GPU conversion (`tb-convert`), is the lever if it
matters. Worker start-up (spawn to `Ready`, 20 runs): min 7.3 ms, median 9.0 ms, max 11.0 ms.

## 6. GPU path (NVDEC), on nixark

`PoolConfig::gpu`: `/dev/nvidiactl`, `/dev/nvidia0`, `/dev/nvidia-uvm(-tools)` bound as devices
(Landlock read, write, ioctl), `/run/opengl-driver` and `/sys` read-only, a `/proc` of the PID
namespace (Landlock read), Landlock read-write-truncate on `/proc/self/task` (the worker's own
threads), no address-space limit; the worker loads `libnvcuvid.so.1` and opens the CUDA device
before the filter, and runs the `ComputeGpu` seccomp profile; device frames are transferred to
host NV12/P010 into the slot.

**Result** (nixark, RTX 2000 Ada, driver 595.71.05, run through `scripts/nixark.sh` with an 8 GB
cap): `tests/gpu.rs` passes with every layer on and seccomp in kill mode: 60 pictures of 320x240
H.264 decoded by NVDEC, delivered as NV12, luma and chroma bit-exact against `ffmpeg`'s software
decode, no violations, one worker start.

What it took (each found on the card, by bisecting layers and `strace`):

1. `cuInit` failed with `CUDA_ERROR_OPERATING_SYSTEM` before opening any device: CUDA names its
   helper thread by writing `/proc/self/task/<tid>/comm` (with `O_TRUNC`) and treats failure as
   fatal. Landlock refused it. Fix: Landlock `ReadFile | WriteFile | Truncate` beneath
   `/proc/self/task` (the `Truncate` right is separate from ABI 3 on; without it the rule was
   there and the open still failed). Also `/proc` and `/sys` readable (CPU and NUMA topology,
   `/proc/driver/nvidia/params`, `/sys/module/nvidia/initstate`).
2. `libnvcuvid.so.1` is loaded by libavcodec when a decoder opens, i.e. after the filter, when
   opening files is refused: libavcodec fell back to software. Fix: `dlopen` it before
   lock-down (a later `dlopen` by name finds it loaded).
3. The driver probes and opens files again when a decoder is created (`access`, `stat`,
   `readlink`, `openat` of `/proc/driver/nvidia/params`, `/dev/nvidia*`), tries `chmod` on
   `/dev/nvidiactl`, queries and sets scheduling (`sched_getparam`, `sched_getscheduler`,
   `getpriority`, `setpriority`), and during `cuInit` tries the MPS control socket. The GPU
   profile therefore allows `open*`, `access`, `faccessat(2)`, `stat`, `lstat`, `statfs`,
   `readlink(at)`, `getdents64`, `eventfd2`, `pipe2`, `dup(3)`, `ioctl`, the scheduler queries,
   `uname`; refuses with EPERM (the driver copes, as it does outside for a user) `chmod`,
   `chown`, `mknod` families, `socket` and `setpriority`; everything else outside the list
   still kills (`execve`, `ptrace`, `connect`, processes, `kill` ...). A census in errno mode
   with `strace -ff` after the last change: only `chmod` and `setpriority` refused.

So GPU mode is wider than software mode in two ways that matter: the worker may open files
(Landlock still limits it to the read-only paths, the device nodes and its own task
directory; nothing writable but those), and it holds the GPU's device nodes and `ioctl`, the
driver's whole attack surface. That is inherent in decoding on the card in-process; the
alternative is the CUDA-IPC design of §7, where only the worker holds the card's decoder.

Not measured on the card: NVDEC throughput through the worker (the brief's measurement is the
software path); HEVC, AV1, VP9 on NVDEC under the filter; P010 output.

## 7. CUDA-IPC instead of the host copy (design, not built)

Today a GPU-decoded picture goes device → host (`av_hwframe_transfer_data`) in the worker,
then the server uploads it again. To keep pictures on the card:

1. The worker decodes into a CUDA frames pool it owns (the `cuda` hwaccel in its own context,
   since the server's context cannot be shared across processes). For each surface pool it
   allocates (or wraps, via a custom `AVHWFramesContext` pool callback) the backing memory with
   `cuMemAlloc` and exports it once with `cuIpcGetMemHandle` (or, better, the VMM API:
   `cuMemCreate` with `CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR` and
   `cuMemExportToShareableHandle`, which gives a file descriptor that travels as
   `SCM_RIGHTS` like the memfds do and dies with its owner; legacy IPC handles are 64-byte
   blobs with no lifetime tie).
2. A new message `DevicePool { pool, handle, bytes, slots, pitch }` replaces `PoolGrant` for
   device output; the parent maps it once (`cuIpcOpenMemHandle` or
   `cuMemImportFromShareableHandle` + `cuMemMap` into the server's context) and binds it as a
   foreign slot of the device protocol (`CudaDevice::bind_foreign(slot, base, len)`,
   ARCHITECTURE §4.2 "Device memory outside the arena"); the conversion kernels read
   `DevRef { slot, offset }` exactly as they read NVDEC surfaces today.
3. Frame messages name `(pool, slot)` with plane offsets and pitches relative to the slot, as
   now; validation is the same arithmetic against the device pool's size. Ownership is the
   same credit scheme: the worker must not reuse a surface until the parent releases it, and
   the parent releases only after the conversion plan that read it has completed (the
   lifetime rule of `bind_foreign`).
4. Synchronization: the worker's decode completes on its own stream; it records a CUDA event
   exported with `cuIpcGetEventHandle` (or an external semaphore from a file descriptor), and
   the parent's conversion plan waits on it before reading. Without that, the host round trip
   of the frame message is not enough ordering on the device.
5. Confinement changes: the worker keeps `/dev/nvidia*` and `ioctl`; IPC memory handles need
   both processes in a context on the same device; a crashed worker's exported allocations
   remain valid in the parent until the parent unmaps them (VMM: refcounted by the imported
   handle), so held frames survive the worker as host slots do now. A device fault in the
   worker poisons only the worker's context (ARCHITECTURE §4.5, D1), which is the point of
   having it in a separate process.

## 8. Open, uncertain, not done

- The seccomp allow-list was derived from software H.264/AAC decode, the conversion paths and
  the fault tests (all under kill mode), and by reading; HEVC 10-bit, AV1 (dav1d's thread
  pool), VP9, AC-3, E-AC-3, MP2/MP3, Opus decode were **not** exercised under the filter here
  (no fixtures for them in the tests). A missing call would show as `SeccompViolation` and
  the circuit breaker; run them once in errno mode with `strace -f` before relying on them.
- The GPU profile is wider than the software one by necessity (§6); Landlock's `IoctlDev`
  right is granted on the device nodes only. The GPU test is `#[ignore]`d (it needs the
  card): `scripts/nixark.sh nix develop -c cargo test -p tb-avworker --test gpu -- --ignored`.
- B-pictures: the decode tests use streams without B-pictures so that test timestamps are
  simple; the protocol carries pts and dts separately and libav reorders; not separately
  tested.
- `End` drains every decoder; there is no per-stream end.
- The pts-plausibility window (10 s) assumes producers' timestamps move with real time; a
  producer jumping timestamps by more than 10 s mid-session without a new `Streams` would
  make legitimate frames look like violations (the receiver should start a new session on a
  jump, which `tb-timeline` treats as a reset anyway).
- The client logs the worker's stderr line by line (sanitized, bounded); libav's log level is
  `error`.
- In the Nix build sandbox the tests pass, but which layers were in effect there is not
  recorded (nextest hides the output of passing tests).
