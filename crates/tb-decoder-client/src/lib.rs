//! The server's side of the sandboxed decoder (`docs/ARCHITECTURE.md` §5.5).
//!
//! ```text
//! let pool = DecoderPool::new(PoolConfig::new(worker_path))?;
//! let mut dec = pool.spawn("input-1")?;          // starts a sandboxed tb-avworker
//! loop {                                          // a plain OS thread, no async runtime
//!     for ev in receiver.events() { let _ = dec.try_send(ev); }   // never blocks
//!     while let Some(frame) = dec.poll(Some(deadline)) { ... }    // zero-copy frames
//!     for e in dec.events() { ... }               // crashes, restarts, refusals
//! }
//! ```
//!
//! A [`Decoder`] is owned by one thread and driven by it: there is no thread inside. Every
//! call does the bookkeeping that is due (messages from the worker, slots given back by
//! dropped frames, heartbeats, the watchdog, the worker's exit, restarts); [`Decoder::poll`]
//! waits for whichever comes first of a frame, an event, or the caller's deadline, and the
//! decoder's descriptor ([`AsFd`]) is readable whenever there is something to do, for a
//! caller that multiplexes several decoders with `epoll`.
//!
//! - **Never blocks the caller.** [`Decoder::try_send`] copies a packet into the input ring
//!   and sends a 40-byte message with `MSG_DONTWAIT`; a full ring or socket is
//!   [`SendError::Full`], the packet is dropped and counted. Control messages (`Streams`,
//!   `End`) are queued if the socket is full and go first when it drains.
//! - **Zero copy out.** A frame is a validated descriptor plus a guard on its slot of shared
//!   memory; dropping the frame gives the slot back (from any thread).
//! - **Nothing the worker says is believed unchecked** ([`tb_decoder_proto::pool`]): a
//!   message that does not validate is a violation, the worker is killed, and the restart
//!   policy applies.
//! - **A crash is an event**, never a panic or an error of the caller's next call:
//!   [`Event::Exited`] with its [`ExitClass`], then [`Event::Restarting`] or
//!   [`Event::CircuitOpen`]. Frames the caller holds stay valid. After a restart the last
//!   `Streams` is sent again and video waits for a key frame.
//!
//! This crate does not link libav; the worker binary (`tb-avworker`) does.

mod frames;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::event::epoll;
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use tb_decoder_proto::pool::{Limits, PoolTable, Released, Streams, Violation};
use tb_decoder_proto::ring::InputRing;
use tb_decoder_proto::supervise::{
    Action, Counters, ExitClass, KillReason, Millis, Outcome, RawExit, RestartPolicy, Supervisor,
    Verdict, Watchdog, WatchdogConfig, classify,
};
use tb_decoder_proto::wire::{
    AudioSpec, Configure, Fault, FromWorker, MAX_MESSAGE, OutputSpec, PacketRef, ProbeResult,
    ProbeStage, Refusal, SeccompMode, StreamOpen, StreamState, ToWorker, VideoSpec, WorkerStats,
    codec_supported, sanitize,
};
use tb_media::{InputEvent, MediaKind, StreamIndex};
use tb_sandbox::shm::Mapping;
use tb_sandbox::{Confinement, Layer};

pub use frames::{AudioFrame, Decoded, VideoFrame};
/// The protocol crate: specs, exit classes, limits and policies used in this API.
pub use tb_decoder_proto as proto;

/// Configuration shared by the decoders of a pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// The `tb-avworker` binary.
    pub worker: PathBuf,
    /// The sandbox.
    pub policy: tb_sandbox::Policy,
    /// Restarts.
    pub restart: RestartPolicy,
    /// Heartbeats and stalls.
    pub watchdog: WatchdogConfig,
    /// What a worker may say.
    pub limits: Limits,
    /// Bytes of each worker's input ring (packets: twice the largest).
    pub input_ring_bytes: u64,
    /// Slots of a video pool.
    pub video_slots: u16,
    /// Slots of an audio pool.
    pub audio_slots: u16,
    /// Milliseconds a worker waits for a free slot before it drops a frame.
    pub slot_wait_ms: u32,
    /// Threads per software decoder (0: libav's choice).
    pub threads: u8,
    /// Decode video on the GPU (NVDEC) where possible.
    pub gpu: bool,
    /// What the system-call filter does to a call outside its list.
    pub seccomp: SeccompMode,
    /// Test modes of the worker.
    #[doc(hidden)]
    pub fault: Fault,
}

impl PoolConfig {
    /// Defaults for a worker binary at `worker`.
    pub fn new(worker: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
            policy: tb_sandbox::Policy::default(),
            restart: RestartPolicy::default(),
            watchdog: WatchdogConfig::default(),
            limits: Limits::default(),
            input_ring_bytes: 8 << 20,
            video_slots: 8,
            audio_slots: 32,
            slot_wait_ms: 20,
            threads: 1,
            gpu: false,
            seccomp: SeccompMode::Kill,
            fault: Fault::None,
        }
    }
}

/// Why a decoder could not be made.
#[derive(Debug, thiserror::Error)]
pub enum DecoderError {
    /// The configuration cannot work.
    #[error("decoder configuration: {0}")]
    Config(String),
    /// A system call of this process failed.
    #[error("decoder: {what}: {errno}")]
    Sys {
        /// The call.
        what: &'static str,
        /// The error.
        errno: Errno,
    },
}

fn sys(what: &'static str) -> impl FnOnce(Errno) -> DecoderError {
    move |errno| DecoderError::Sys { what, errno }
}

/// Makes decoders that share a configuration.
#[derive(Debug, Clone)]
pub struct DecoderPool {
    cfg: Arc<PoolConfig>,
}

impl DecoderPool {
    /// A pool; checks that the worker binary exists.
    pub fn new(cfg: PoolConfig) -> Result<Self, DecoderError> {
        if !cfg.worker.is_absolute() || !cfg.worker.is_file() {
            return Err(DecoderError::Config(format!(
                "worker {} is not an absolute path to a file",
                cfg.worker.display()
            )));
        }
        if cfg.input_ring_bytes < 64 << 10 || cfg.video_slots == 0 || cfg.audio_slots == 0 {
            return Err(DecoderError::Config("ring or slot counts too small".into()));
        }
        Ok(Self { cfg: Arc::new(cfg) })
    }

    /// A decoder; its first worker starts now (it is ready a few milliseconds later; packets
    /// sent meanwhile wait in the ring).
    pub fn spawn(&self, name: &str) -> Result<Decoder, DecoderError> {
        Decoder::new(Arc::clone(&self.cfg), name)
    }
}

/// Why a packet or event was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// The ring or the socket is full: the worker is behind. Dropped and counted.
    #[error("input full")]
    Full,
    /// No worker is running (restarting, circuit open, stopped). Dropped and counted.
    #[error("no worker")]
    Unavailable,
    /// The packet is larger than half the input ring. Dropped and counted.
    #[error("packet too large")]
    TooLarge,
    /// The packet's stream was not announced. Dropped and counted.
    #[error("unknown stream")]
    UnknownStream,
    /// Video after a (re)start waits for a key frame. Dropped and counted.
    #[error("waiting for a key frame")]
    AwaitingKeyframe,
    /// The stream set cannot be sent (a codec the protocol cannot name, too many streams,
    /// extradata too large).
    #[error("stream set refused")]
    BadStreams,
}

/// What happened to the decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A worker is ready, with this confinement.
    Started {
        /// Number of the worker (from 1).
        generation: u64,
        /// What took effect.
        confinement: Confinement,
    },
    /// A worker ended.
    Exited {
        /// Its number.
        generation: u64,
        /// How.
        class: ExitClass,
    },
    /// The next worker starts after this delay.
    Restarting {
        /// The delay.
        after: Duration,
    },
    /// Too many failures: no worker for this long (the program goes to standby or slate).
    CircuitOpen {
        /// How long.
        for_: Duration,
    },
    /// The worker has been healthy long enough: backoff reset, circuit closed.
    Healthy,
    /// The worker said something that does not validate (it is being killed).
    Violation {
        /// What.
        what: String,
    },
    /// The worker does not decode a stream.
    StreamRefused {
        /// The stream.
        stream: StreamIndex,
        /// Why.
        reason: Refusal,
    },
    /// The session's `End` was processed: every frame of it has been delivered.
    Drained,
    /// No more workers will be started: the configuration cannot be satisfied (a required
    /// confinement layer is missing) or the decoder was shut down.
    Fatal {
        /// Why.
        reason: String,
    },
    /// Results of a sandbox probe (test mode).
    #[doc(hidden)]
    Probe {
        /// When.
        stage: ProbeStage,
        /// Results.
        results: Vec<ProbeResult>,
    },
}

/// Counters of a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Packets sent to a worker.
    pub packets: u64,
    /// Their bytes.
    pub bytes: u64,
    /// Dropped: ring or socket full.
    pub dropped_full: u64,
    /// Dropped: no worker.
    pub dropped_unavailable: u64,
    /// Dropped: too large.
    pub dropped_too_large: u64,
    /// Dropped: unknown stream.
    pub dropped_unknown: u64,
    /// Dropped: waiting for a key frame.
    pub dropped_awaiting_key: u64,
    /// Pictures delivered.
    pub video_frames: u64,
    /// Audio blocks delivered.
    pub audio_frames: u64,
    /// Protocol violations by workers.
    pub violations: u64,
    /// Pool requests denied.
    pub pools_denied: u64,
    /// The latest counters of the current worker.
    pub worker: WorkerStats,
    /// The supervisor's counters.
    pub supervisor: Counters,
}

const KEY_SOCK: u64 = 1;
const KEY_PIDFD: u64 = 2;
const KEY_STDERR: u64 = 3;
const KEY_TIMER: u64 = 4;
const KEY_WAKE: u64 = 5;

/// Messages read per call at most (a flood cannot hold the caller).
const BATCH: usize = 256;
/// Control messages queued while the socket is full, at most; beyond, the worker is stalled.
const MAX_PENDING: usize = 4096;

/// One running worker.
struct Running {
    generation: u64,
    child: tb_sandbox::Child,
    sock: OwnedFd,
    ring: InputRing,
    ring_map: Mapping,
    pools: PoolTable,
    maps: BTreeMap<u32, Arc<Mapping>>,
    streams: Streams,
    wd: Watchdog,
    hello: bool,
    ready: bool,
    killed: Option<KillReason>,
    shutdown: bool,
    pending: VecDeque<(Vec<u8>, Option<OwnedFd>)>,
    wait_key: BTreeSet<StreamIndex>,
    end_pending: bool,
    sock_closed: bool,
    stderr_buf: Vec<u8>,
}

/// A decoder: a supervised sandboxed worker and the state to feed it.
pub struct Decoder {
    cfg: Arc<PoolConfig>,
    name: String,
    epoch: Instant,
    sup: Supervisor,
    run: Option<Running>,
    generation: u64,
    session: Option<Vec<StreamOpen>>,
    video_spec: Option<VideoSpec>,
    audio_spec: Option<AudioSpec>,
    out: VecDeque<Decoded>,
    events: VecDeque<Event>,
    releases: Arc<frames::ReleaseQueue>,
    epoll: OwnedFd,
    timer: OwnedFd,
    stats: Stats,
    stopped: bool,
    buf: Vec<u8>,
    msg: Vec<u8>,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("name", &self.name)
            .field("generation", &self.generation)
            .field("phase", &self.sup.phase())
            .finish_non_exhaustive()
    }
}

impl AsFd for Decoder {
    /// Readable when a call has something to do (an `epoll` descriptor).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.epoll.as_fd()
    }
}

impl Decoder {
    fn new(cfg: Arc<PoolConfig>, name: &str) -> Result<Self, DecoderError> {
        let ep = epoll::create(epoll::CreateFlags::CLOEXEC).map_err(sys("epoll_create"))?;
        let timer = rustix::time::timerfd_create(
            rustix::time::TimerfdClockId::Monotonic,
            rustix::time::TimerfdFlags::CLOEXEC | rustix::time::TimerfdFlags::NONBLOCK,
        )
        .map_err(sys("timerfd_create"))?;
        let wake = rustix::event::eventfd(
            0,
            rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
        )
        .map_err(sys("eventfd"))?;
        let releases = frames::ReleaseQueue::new(wake);
        epoll::add(
            &ep,
            &timer,
            epoll::EventData::new_u64(KEY_TIMER),
            epoll::EventFlags::IN,
        )
        .map_err(sys("epoll_ctl"))?;
        epoll::add(
            &ep,
            releases.wake_fd(),
            epoll::EventData::new_u64(KEY_WAKE),
            epoll::EventFlags::IN,
        )
        .map_err(sys("epoll_ctl"))?;
        let seed = std::process::id() as u64 ^ (name.len() as u64) << 32 ^ 0x9e37_79b9;
        let mut d = Self {
            sup: Supervisor::new(cfg.restart.clone(), seed),
            cfg,
            name: name.to_owned(),
            epoch: Instant::now(),
            run: None,
            generation: 0,
            session: None,
            video_spec: None,
            audio_spec: None,
            out: VecDeque::new(),
            events: VecDeque::new(),
            releases,
            epoll: ep,
            timer,
            stats: Stats::default(),
            stopped: false,
            buf: vec![0; MAX_MESSAGE],
            msg: Vec::with_capacity(MAX_MESSAGE),
        };
        d.service();
        if let Some(Event::Fatal { reason }) = d.events.back() {
            return Err(DecoderError::Config(reason.clone()));
        }
        Ok(d)
    }

    fn now(&self) -> Millis {
        self.epoch.elapsed().as_millis() as Millis
    }

    /// Converted output for every video stream of the next `Streams` (and of the current
    /// session, sent again): `None` for native frames.
    pub fn set_video_output(&mut self, spec: Option<VideoSpec>) {
        self.video_spec = spec;
    }

    /// Resampled output for every audio stream of the next `Streams`: `None` for native.
    pub fn set_audio_output(&mut self, spec: Option<AudioSpec>) {
        self.audio_spec = spec;
    }

    /// Sends one event of a receiver. Never blocks.
    pub fn try_send(&mut self, ev: InputEvent) -> Result<(), SendError> {
        self.service();
        match ev {
            InputEvent::Streams(list) => {
                let opens = list
                    .into_iter()
                    .map(|info| {
                        let output = match info.codec.kind() {
                            MediaKind::Video => self
                                .video_spec
                                .map_or(OutputSpec::Native, OutputSpec::Video),
                            MediaKind::Audio => self
                                .audio_spec
                                .map_or(OutputSpec::Native, OutputSpec::Audio),
                            MediaKind::Data => OutputSpec::Native,
                        };
                        StreamOpen { info, output }
                    })
                    .collect();
                self.try_send_streams(opens)
            }
            InputEvent::Packet(p) => self.send_packet(p),
            InputEvent::End(_) => {
                self.session = None;
                if let Some(run) = self.run.as_mut() {
                    run.end_pending = true;
                    let now = self.epoch.elapsed().as_millis() as Millis;
                    run.wd.set_pending(true, now);
                    self.ctrl(&ToWorker::End, None);
                }
                Ok(())
            }
        }
    }

    /// Announces streams with an explicit output for each.
    pub fn try_send_streams(&mut self, opens: Vec<StreamOpen>) -> Result<(), SendError> {
        if opens.iter().any(|o| !codec_supported(o.info.codec))
            || ToWorker::Streams(opens.clone())
                .encode(&mut self.msg)
                .is_err()
        {
            return Err(SendError::BadStreams);
        }
        self.session = Some(opens.clone());
        if let Some(run) = self.run.as_mut() {
            run.streams.replace(&opens);
            run.wait_key.clear();
            self.ctrl(&ToWorker::Streams(opens), None);
        }
        Ok(())
    }

    fn send_packet(&mut self, p: tb_media::Packet) -> Result<(), SendError> {
        let stats = &mut self.stats;
        let Some(run) = self
            .run
            .as_mut()
            .filter(|r| !r.sock_closed && r.killed.is_none())
        else {
            stats.dropped_unavailable += 1;
            return Err(SendError::Unavailable);
        };
        let Some(kind) = run.streams.kind(p.stream) else {
            stats.dropped_unknown += 1;
            return Err(SendError::UnknownStream);
        };
        if kind == MediaKind::Video && run.wait_key.contains(&p.stream) {
            if !p.keyframe {
                stats.dropped_awaiting_key += 1;
                return Err(SendError::AwaitingKeyframe);
            }
            run.wait_key.remove(&p.stream);
        }
        let Ok(len) = u32::try_from(p.data.len()) else {
            stats.dropped_too_large += 1;
            return Err(SendError::TooLarge);
        };
        if u64::from(len) > run.ring.max_packet() || len == 0 {
            stats.dropped_too_large += 1;
            return Err(SendError::TooLarge);
        }
        if !run.pending.is_empty() {
            stats.dropped_full += 1;
            return Err(SendError::Full);
        }
        let Some(span) = run.ring.reserve(len) else {
            stats.dropped_full += 1;
            return Err(SendError::Full);
        };
        if !run.ring_map.copy_in(span.offset, &p.data) {
            let _ = run.ring.unreserve(span);
            stats.dropped_full += 1;
            return Err(SendError::Full);
        }
        let m = ToWorker::Packet(PacketRef {
            stream: p.stream,
            pts: p.pts,
            dts: p.dts,
            duration: p.duration,
            keyframe: p.keyframe,
            pos: span.pos,
            len,
        });
        if m.encode(&mut self.msg).is_err() {
            let _ = run.ring.unreserve(span);
            stats.dropped_too_large += 1;
            return Err(SendError::TooLarge);
        }
        match tb_sandbox::ipc::send(run.sock.as_fd(), &self.msg, None, true) {
            Ok(()) => {}
            Err(_) => {
                let _ = run.ring.unreserve(span);
                stats.dropped_full += 1;
                return Err(SendError::Full);
            }
        }
        run.streams.note_packet(p.stream, p.pts, p.dts);
        stats.packets += 1;
        stats.bytes += u64::from(len);
        let now = self.epoch.elapsed().as_millis() as Millis;
        run.wd.set_pending(true, now);
        Ok(())
    }

    /// Sends a control message, or queues it behind others while the socket is full.
    fn ctrl(&mut self, m: &ToWorker, fd: Option<OwnedFd>) {
        let Some(run) = self.run.as_mut() else { return };
        if m.encode(&mut self.msg).is_err() {
            tracing::error!(decoder = %self.name, "control message does not encode");
            return;
        }
        run.pending.push_back((self.msg.clone(), fd));
        flush(run);
        if run.pending.len() > MAX_PENDING {
            kill(run, KillReason::Stall);
        }
    }

    /// The next decoded frame, waiting until `deadline` (`None`: do not wait). Returns `None`
    /// at the deadline, or early when an event was queued ([`Decoder::events`]).
    pub fn poll(&mut self, deadline: Option<Instant>) -> Option<Decoded> {
        let events_before = self.events.len();
        loop {
            self.service();
            if let Some(f) = self.out.pop_front() {
                return Some(f);
            }
            if self.events.len() > events_before {
                return None;
            }
            let left = deadline?.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let ts = rustix::time::Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: i64::from(left.subsec_nanos()),
            };
            let mut evs = Vec::with_capacity(8);
            let _ = epoll::wait(
                &self.epoll,
                rustix::buffer::spare_capacity(&mut evs),
                Some(&ts),
            );
        }
    }

    /// Events since the last call.
    pub fn events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.events.drain(..)
    }

    /// The counters.
    pub fn stats(&self) -> Stats {
        Stats {
            supervisor: self.sup.counters(),
            ..self.stats
        }
    }

    /// The confinement of the current worker (as reported so far).
    pub fn confinement(&self) -> Option<&Confinement> {
        self.run.as_ref().map(|r| r.child.confinement())
    }

    /// Whether a worker is ready.
    pub fn is_ready(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.ready)
    }

    /// Waits until a worker is ready, at most `timeout`. Frames that arrive meanwhile stay
    /// queued for [`Decoder::poll`].
    pub fn wait_ready(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            self.service();
            if self.is_ready() {
                return true;
            }
            if self.stopped || Instant::now() >= end {
                return false;
            }
            let mut evs = Vec::with_capacity(8);
            let ts = rustix::time::Timespec {
                tv_sec: 0,
                tv_nsec: 5_000_000,
            };
            let _ = epoll::wait(
                &self.epoll,
                rustix::buffer::spare_capacity(&mut evs),
                Some(&ts),
            );
        }
    }

    /// Stops: asks the worker to exit, waits at most `timeout`, then kills it. No restarts.
    pub fn shutdown(mut self, timeout: Duration) {
        self.sup.stop();
        self.stopped = true;
        let Some(mut run) = self.run.take() else {
            return;
        };
        run.shutdown = true;
        if ToWorker::Shutdown.encode(&mut self.msg).is_ok() {
            run.pending.push_back((self.msg.clone(), None));
            flush(&mut run);
        }
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Ok(Some(_)) = run.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = run.child.kill();
        let _ = run.child.wait();
    }

    // -----------------------------------------------------------------------------------------

    /// Everything that is due now.
    fn service(&mut self) {
        let now = self.now();
        // slots given back by dropped frames
        let released = self.releases.drain();
        let mut by_pool: BTreeMap<u32, Vec<u16>> = BTreeMap::new();
        if let Some(run) = self.run.as_mut() {
            for (g, slot) in released {
                if g != run.generation {
                    continue;
                }
                match run.pools.release(slot) {
                    Released::ToWorker { pool, slot } => {
                        by_pool.entry(pool).or_default().push(slot)
                    }
                    Released::PoolGone(p) => {
                        run.maps.remove(&p);
                    }
                    Released::Nothing => {}
                }
            }
        }
        for (pool, slots) in by_pool {
            self.ctrl(&ToWorker::Release { pool, slots }, None);
        }
        self.read_messages(now);
        self.drain_stderr();
        if let Some(run) = self.run.as_mut() {
            flush(run);
            if run.ready {
                let pending = run.ring.pending() > 0 || run.end_pending;
                run.wd.set_pending(pending, now);
            }
            match run.wd.check(now) {
                Verdict::Ok => {}
                Verdict::Ping(seq) => self.ctrl(&ToWorker::Ping { seq }, None),
                Verdict::Kill(r) => {
                    tracing::warn!(decoder = %self.name, reason = ?r, "killing the worker");
                    if let Some(run) = self.run.as_mut() {
                        kill(run, r);
                    }
                }
            }
        }
        self.check_exit(now);
        match self.sup.poll(now) {
            Some(Action::Spawn) if !self.stopped => self.start(now),
            Some(Action::Healthy) => self.events.push_back(Event::Healthy),
            _ => {}
        }
        self.arm_timer(now);
    }

    fn arm_timer(&mut self, now: Millis) {
        let mut next = self.sup.deadline();
        if let Some(run) = &self.run {
            let d = run.wd.deadline();
            next = Some(next.map_or(d, |n| n.min(d)));
        }
        let delay = next.map(|t| t.saturating_sub(now).max(1));
        let spec = rustix::time::Itimerspec {
            it_interval: rustix::time::Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: match delay {
                Some(ms) => rustix::time::Timespec {
                    tv_sec: (ms / 1000) as i64,
                    tv_nsec: ((ms % 1000) * 1_000_000) as i64,
                },
                None => rustix::time::Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
            },
        };
        let mut b = [0u8; 8];
        let _ = rustix::io::read(&self.timer, &mut b);
        let _ = rustix::time::timerfd_settime(
            &self.timer,
            rustix::time::TimerfdTimerFlags::empty(),
            &spec,
        );
    }

    fn start(&mut self, now: Millis) {
        match self.try_start(now) {
            Ok(run) => {
                self.sup.started(now);
                self.run = Some(run);
                if let Some(opens) = self.session.clone()
                    && let Some(run) = self.run.as_mut()
                {
                    // the session goes on: video resumes at a key frame
                    run.streams.replace(&opens);
                    run.wait_key = opens
                        .iter()
                        .filter(|o| o.info.codec.kind() == MediaKind::Video)
                        .map(|o| o.info.index)
                        .collect();
                    self.ctrl(&ToWorker::Streams(opens), None);
                }
            }
            Err(e) => {
                tracing::error!(decoder = %self.name, error = %e, "worker could not be started");
                self.sup.started(now);
                if matches!(e, StartError::Fatal(_)) {
                    self.sup.stop();
                    self.stopped = true;
                    self.events.push_back(Event::Fatal {
                        reason: e.to_string(),
                    });
                }
                let o = self.sup.exited(now, ExitClass::SetupFailed);
                self.outcome(now, o);
            }
        }
    }

    fn try_start(&mut self, now: Millis) -> Result<Running, StartError> {
        let cfg = &*self.cfg;
        let (ours, theirs) = tb_sandbox::ipc::pair().map_err(StartError::Sys)?;
        let ring_fd = tb_sandbox::shm::create(c"tb-input-ring", cfg.input_ring_bytes)
            .map_err(StartError::Sys)?;
        let len =
            usize::try_from(cfg.input_ring_bytes).map_err(|_| StartError::Sys(Errno::INVAL))?;
        let ring_map = Mapping::map(&ring_fd, len, true).map_err(StartError::Sys)?;
        tb_sandbox::shm::seal(ring_fd.as_fd(), true).map_err(StartError::Sys)?;
        let mut policy = cfg.policy.clone();
        if cfg.gpu {
            // NVIDIA's device nodes (NVDEC), and the DRM render nodes (VA-API: AMD, Intel); the
            // worker uses CUDA where it opens, else VA-API
            let render = (128..136).map(|n| format!("/dev/dri/renderD{n}"));
            for d in [
                "/dev/nvidiactl",
                "/dev/nvidia0",
                "/dev/nvidia-uvm",
                "/dev/nvidia-uvm-tools",
            ]
            .map(String::from)
            .into_iter()
            .chain(render)
            {
                let p = PathBuf::from(d);
                if p.exists() && !policy.devices.contains(&p) {
                    policy.devices.push(p);
                }
            }
            // the drivers' libraries (CUDA's, Mesa's VA driver); CUDA's initialization also reads
            // /sys (CPU and NUMA topology) and /proc, Mesa /sys (the device's PCI identity):
            // GPU mode is wider by necessity
            for p in ["/run/opengl-driver", "/sys"] {
                let p = PathBuf::from(p);
                if p.exists() && !policy.ro_paths.contains(&p) {
                    policy.ro_paths.push(p);
                }
            }
            policy.rlimits.address_space = None;
            policy.proc = true;
            // CUDA names its threads through /proc/self/task/<tid>/comm and fails to
            // initialize (CUDA_ERROR_OPERATING_SYSTEM) if it cannot: the worker's own tasks only
            let task = PathBuf::from("/proc/self/task");
            if !policy.landlock_rw.contains(&task) {
                policy.landlock_rw.push(task);
            }
        }
        let mut cmd = tb_sandbox::Command::new(&cfg.worker);
        cmd.arg(&self.name);
        let child = tb_sandbox::spawn(&cmd, theirs, &policy).map_err(|e| match e {
            tb_sandbox::SpawnError::BadPolicy(_) | tb_sandbox::SpawnError::Unavailable { .. } => {
                StartError::Fatal(e.to_string())
            }
            e => StartError::Spawn(e.to_string()),
        })?;
        tb_sandbox::ipc::set_nonblocking(&ours).map_err(StartError::Sys)?;
        self.generation += 1;
        let mut run = Running {
            generation: self.generation,
            child,
            sock: ours,
            ring: InputRing::new(cfg.input_ring_bytes)
                .map_err(|_| StartError::Sys(Errno::INVAL))?,
            ring_map,
            pools: PoolTable::default(),
            maps: BTreeMap::new(),
            streams: Streams::default(),
            wd: Watchdog::new(cfg.watchdog.clone(), now),
            hello: false,
            ready: false,
            killed: None,
            shutdown: false,
            pending: VecDeque::new(),
            wait_key: BTreeSet::new(),
            end_pending: false,
            sock_closed: false,
            stderr_buf: Vec::new(),
        };
        let configure = ToWorker::Configure(Configure {
            input_ring_len: cfg.input_ring_bytes,
            threads: cfg.threads,
            gpu: cfg.gpu,
            seccomp: cfg.seccomp,
            video_slots: cfg.video_slots,
            audio_slots: cfg.audio_slots,
            slot_wait_ms: cfg.slot_wait_ms,
            max_frame_bytes: cfg.limits.max_frame_bytes,
            fault: cfg.fault,
        });
        configure
            .encode(&mut self.msg)
            .map_err(|_| StartError::Sys(Errno::INVAL))?;
        run.pending.push_back((self.msg.clone(), Some(ring_fd)));
        flush(&mut run);
        for (key, fd) in [
            (KEY_SOCK, Some(run.sock.as_fd())),
            (KEY_PIDFD, Some(run.child.pidfd())),
            (KEY_STDERR, run.child.stderr()),
        ] {
            if let Some(fd) = fd {
                epoll::add(
                    &self.epoll,
                    fd,
                    epoll::EventData::new_u64(key),
                    epoll::EventFlags::IN,
                )
                .map_err(StartError::Sys)?;
            }
        }
        Ok(run)
    }

    fn read_messages(&mut self, now: Millis) {
        for _ in 0..BATCH {
            let Some(run) = self.run.as_mut() else { return };
            if run.sock_closed {
                return;
            }
            let r = tb_sandbox::ipc::recv(run.sock.as_fd(), &mut self.buf, true);
            let m = match r {
                Ok(Some(m)) => m,
                Ok(None) | Err(Errno::CONNRESET) => {
                    run.sock_closed = true;
                    return;
                }
                Err(_) => return,
            };
            let res = if m.truncated || m.fd.is_some() || m.extra_fds {
                Err(Violation::Unexpected("oversized message or descriptor"))
            } else {
                FromWorker::decode(self.buf.get(..m.len).unwrap_or_default())
                    .map_err(Violation::from)
                    .and_then(|msg| self.handle(msg, now))
            };
            if let Err(v) = res {
                self.violation(v);
                return;
            }
        }
    }

    fn violation(&mut self, v: Violation) {
        tracing::error!(decoder = %self.name, violation = %v, "worker protocol violation");
        self.stats.violations += 1;
        self.events.push_back(Event::Violation {
            what: v.to_string(),
        });
        if let Some(run) = self.run.as_mut() {
            kill(run, KillReason::Violation);
        }
    }

    fn handle(&mut self, msg: FromWorker, now: Millis) -> Result<(), Violation> {
        let Some(run) = self.run.as_mut() else {
            return Ok(());
        };
        if run.killed.is_some() {
            return Ok(());
        }
        if !run.hello && !matches!(msg, FromWorker::Hello { .. }) {
            return Err(Violation::Unexpected("message before Hello"));
        }
        if run.hello && !run.ready && !matches!(msg, FromWorker::Ready | FromWorker::Probe { .. }) {
            return Err(Violation::Unexpected("message before Ready"));
        }
        let limits = &self.cfg.limits;
        match msg {
            FromWorker::Hello { .. } => {
                if run.hello {
                    return Err(Violation::Unexpected("second Hello"));
                }
                run.hello = true;
            }
            FromWorker::Ready => {
                if run.ready {
                    return Err(Violation::Unexpected("second Ready"));
                }
                run.ready = true;
                run.child.finish_reports(Duration::from_millis(200));
                let conf = run.child.confinement().clone();
                let missing = conf.missing(self.cfg.policy.required());
                if !missing.is_empty() {
                    let reason =
                        format!("required confinement missing: {missing:?} (have: {conf})");
                    tracing::error!(decoder = %self.name, "{reason}");
                    self.sup.stop();
                    self.stopped = true;
                    kill(run, KillReason::Startup);
                    self.events.push_back(Event::Fatal { reason });
                    return Ok(());
                }
                let weak = Layer::ALL.iter().any(|l| !conf.in_effect(*l));
                if weak {
                    tracing::warn!(decoder = %self.name, generation = run.generation, confinement = %conf, "worker ready with REDUCED confinement");
                } else {
                    tracing::info!(decoder = %self.name, generation = run.generation, confinement = %conf, "worker ready");
                }
                run.wd.ready(now);
                self.sup.ready(now);
                self.events.push_back(Event::Started {
                    generation: run.generation,
                    confinement: conf,
                });
            }
            FromWorker::StreamStatus { stream, state } => {
                run.streams.acknowledged();
                if let StreamState::Refused(reason) = state {
                    self.events
                        .push_back(Event::StreamRefused { stream, reason });
                }
            }
            FromWorker::NeedPool {
                stream,
                slot_size,
                slots,
            } => match run
                .pools
                .request(&run.streams, stream, slot_size, slots, limits)?
            {
                Ok(g) => {
                    let bytes = g.slot_size * u64::from(g.slots);
                    let made = (|| -> Result<(OwnedFd, Mapping), Errno> {
                        let fd = tb_sandbox::shm::create(c"tb-output-pool", bytes)?;
                        tb_sandbox::shm::seal(fd.as_fd(), false)?;
                        let len = usize::try_from(bytes).map_err(|_| Errno::INVAL)?;
                        let map = Mapping::map(&fd, len, false)?;
                        Ok((fd, map))
                    })();
                    match made {
                        Ok((fd, map)) => {
                            run.maps.insert(g.pool, Arc::new(map));
                            self.ctrl(&ToWorker::PoolGrant(g), Some(fd));
                        }
                        Err(e) => {
                            tracing::error!(decoder = %self.name, error = %e, "output pool");
                            let _ = run.pools.retire(g.pool);
                            self.stats.pools_denied += 1;
                            self.ctrl(&ToWorker::PoolDenied { stream }, None);
                        }
                    }
                }
                Err(deny) => {
                    tracing::warn!(decoder = %self.name, stream = stream.0, %deny, "output pool denied");
                    self.stats.pools_denied += 1;
                    self.ctrl(&ToWorker::PoolDenied { stream }, None);
                }
            },
            FromWorker::RetirePool { pool } => {
                if run.pools.retire(pool)? {
                    run.maps.remove(&pool);
                }
            }
            FromWorker::Video(v) => {
                let slot = run.pools.accept_video(&v, &run.streams, limits)?;
                let map = run
                    .maps
                    .get(&v.pool)
                    .cloned()
                    .ok_or(Violation::UnknownPool(v.pool))?;
                run.wd.progress(now);
                self.stats.video_frames += 1;
                self.out.push_back(frames::video(
                    v.desc,
                    map,
                    slot,
                    run.generation,
                    Arc::clone(&self.releases),
                ));
            }
            FromWorker::Audio(a) => {
                let slot = run.pools.accept_audio(&a, &run.streams, limits)?;
                let map = run
                    .maps
                    .get(&a.pool)
                    .cloned()
                    .ok_or(Violation::UnknownPool(a.pool))?;
                run.wd.progress(now);
                self.stats.audio_frames += 1;
                self.out.push_back(frames::audio(
                    a.desc,
                    map,
                    slot,
                    run.generation,
                    Arc::clone(&self.releases),
                ));
            }
            FromWorker::Consumed { upto } => {
                run.ring.consumed(upto)?;
                run.wd.progress(now);
            }
            FromWorker::Pong { seq, stats } => {
                run.wd.pong(seq)?;
                self.stats.worker = stats;
            }
            FromWorker::Ended => {
                if !run.end_pending {
                    return Err(Violation::Unexpected("Ended without End"));
                }
                run.end_pending = false;
                run.wd.progress(now);
                self.events.push_back(Event::Drained);
            }
            FromWorker::Error { code, text } => {
                tracing::warn!(decoder = %self.name, code, text = %sanitize(&text), "worker");
            }
            FromWorker::Probe { stage, results } => {
                self.events.push_back(Event::Probe { stage, results });
            }
        }
        Ok(())
    }

    fn drain_stderr(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        let Some(fd) = run.child.stderr() else { return };
        let mut b = [0u8; 4096];
        for _ in 0..4 {
            match rustix::io::read(fd, &mut b) {
                Ok(0) | Err(_) => break,
                Ok(n) => run
                    .stderr_buf
                    .extend_from_slice(b.get(..n).unwrap_or_default()),
            }
        }
        while let Some(i) = run.stderr_buf.iter().position(|c| *c == b'\n') {
            let line: Vec<u8> = run.stderr_buf.drain(..=i).collect();
            tracing::warn!(decoder = %self.name, generation = run.generation, "worker stderr: {}", sanitize(&line));
        }
        if run.stderr_buf.len() > 1024 {
            tracing::warn!(decoder = %self.name, "worker stderr: {}", sanitize(&run.stderr_buf));
            run.stderr_buf.clear();
        }
    }

    fn check_exit(&mut self, now: Millis) {
        let Some(run) = self.run.as_mut() else { return };
        let exit = match run.child.try_wait() {
            Ok(Some(e)) => e,
            Ok(None) => return,
            Err(e) => {
                tracing::error!(decoder = %self.name, error = %e, "waitid");
                return;
            }
        };
        let raw = match exit.status {
            tb_sandbox::ExitStatus::Exited(c) => RawExit::Code(c),
            tb_sandbox::ExitStatus::Signaled(s) => RawExit::Signal(s),
        };
        let class = if let Some((stage, errno)) = exit.setup_failed {
            tracing::error!(decoder = %self.name, ?stage, errno, "sandbox set-up failed");
            ExitClass::SetupFailed
        } else {
            classify(raw, run.killed, run.shutdown)
        };
        let generation = run.generation;
        // unregister and drop the worker's descriptors and mappings (frames the caller holds
        // keep their own mappings alive)
        for fd in [
            Some(run.sock.as_fd()),
            Some(run.child.pidfd()),
            run.child.stderr(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = epoll::delete(&self.epoll, fd);
        }
        self.drain_stderr();
        self.run = None;
        match class {
            ExitClass::Clean => tracing::info!(decoder = %self.name, generation, "worker exited"),
            c => tracing::warn!(decoder = %self.name, generation, class = ?c, "worker ended"),
        }
        self.events.push_back(Event::Exited { generation, class });
        let o = self.sup.exited(now, class);
        self.outcome(now, o);
    }

    fn outcome(&mut self, now: Millis, o: Outcome) {
        match o {
            Outcome::RestartAt(t) => self.events.push_back(Event::Restarting {
                after: Duration::from_millis(t.saturating_sub(now)),
            }),
            Outcome::CircuitOpen(t) => {
                tracing::error!(decoder = %self.name, "circuit open: no decoding for {} ms", t.saturating_sub(now));
                self.events.push_back(Event::CircuitOpen {
                    for_: Duration::from_millis(t.saturating_sub(now)),
                });
            }
            Outcome::Stopped => {}
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // the Child's own drop kills and reaps the worker
        self.run = None;
    }
}

/// Why a worker could not be started.
#[derive(Debug, thiserror::Error)]
enum StartError {
    #[error("{0}")]
    Sys(Errno),
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("{0}")]
    Fatal(String),
}

/// Sends queued control messages until the socket is full.
fn flush(run: &mut Running) {
    while let Some((m, fd)) = run.pending.front() {
        match tb_sandbox::ipc::send(run.sock.as_fd(), m, fd.as_ref().map(AsFd::as_fd), true) {
            Ok(()) => {
                run.pending.pop_front();
            }
            Err(Errno::AGAIN) => return,
            Err(_) => {
                run.sock_closed = true;
                run.pending.clear();
                return;
            }
        }
    }
}

fn kill(run: &mut Running, reason: KillReason) {
    if run.killed.is_none() {
        run.killed = Some(reason);
        let _ = run.child.kill();
    }
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Decoder>();
    is_send::<Decoded>();
}
