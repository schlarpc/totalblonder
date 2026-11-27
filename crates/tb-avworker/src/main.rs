//! `tb-avworker`: the sandboxed decoder. The only program of the workspace that links libav.
//!
//! Started by `tb-decoder-client` through `tb-sandbox`; never run by hand (it refuses to run
//! outside a sandbox). Its life:
//!
//! 1. [`tb_sandbox::child::enter`]: the namespace's init splits off; death signal, not
//!    dumpable, Landlock.
//! 2. `Hello` on the control channel (fd 3); `Configure` comes back with the input ring's memfd.
//! 3. libav is initialized: log level; with GPU decode, the hardware device is opened (CUDA,
//!    else VA-API: the driver's libraries are loaded now, while files can still be opened).
//! 4. The seccomp filter goes on ([`tb_sandbox::child::Entered::lock_down`]); the confinement
//!    report is complete; `Ready`.
//! 5. The loop: `Streams` opens a libavcodec decoder per stream (allow-listed codecs only);
//!    `Packet` names bytes in the input ring, which are copied into a padded `AVPacket`
//!    (credit returns at once with `Consumed`), decoded, and every frame is written into a
//!    free slot of the stream's output pool and announced with `Video` / `Audio`; `End`
//!    drains the decoders; `Ping` is answered with counters.
//!
//! Linked: libavcodec, libavutil, libswscale, libswresample. No demuxers, no protocols, no
//! device or filter libraries; the worker gets packets, nothing else. Software decode is the
//! default; with `Configure::gpu`, NVDEC through the `cuda` hwaccel where an NVIDIA card is,
//! else VA-API through the `vaapi` hwaccel (AMD: Mesa's radeonsi VA driver on the card's DRM
//! render node), with `av_hwframe_transfer_data` to host memory (NV12 or P010) before the copy
//! into the slot.
//!
//! # Output per stream (`StreamOpen::output`)
//!
//! - **Native** (the default): pictures as decoded, in planar 8-bit 4:2:0 (also from 4:2:2,
//!   chroma rows averaged), NV12, or P010 (also from planar 10-bit), rows aligned to 64 bytes;
//!   audio as f32 planar at the stream's own rate and channels. No scaling, no resampling.
//! - **Converted video** (`VideoSpec`): libswscale, Lanczos with accurate rounding, to planar
//!   8-bit 4:2:0 of the target size with rows unpadded (the encoder's host layout), limited
//!   range, the target's matrix (BT.709 from 720 lines up, BT.601 below; an unspecified
//!   source is taken as BT.709 from 720 lines up). `Fit` letterboxes or pillarboxes with
//!   black (Y 16, U/V 128) by the source's display aspect (its SAR) against the target's;
//!   `Stretch` fills. Bob deinterlacing of pictures flagged interlaced: each field, read as
//!   every other row, scaled to the full height (no half-line shift between the fields:
//!   simple bob), first field first; the second picture is half the packet's duration later
//!   (or half a period of the announced frame rate; without either, no timestamp).
//! - **Resampled audio** (`AudioSpec`): libswresample to 48 kHz f32 planar, stereo or 5.1 in
//!   A/52 order (L, C, R, Ls, Rs, LFE); its default matrix for up- and downmixing. Timestamps
//!   are the input's, less the resampler's delay, in the stream's time base; the resampler is
//!   flushed at `End`.
//!
//! Exit codes: 0 after `Shutdown` or when the parent closes the channel;
//! [`supervise::EXIT_OOM`] when memory runs out (the global allocator and libav's `ENOMEM`);
//! [`supervise::EXIT_PROTOCOL`] when the parent's messages are malformed;
//! [`supervise::EXIT_SETUP`] when the sandbox or initialization fails.

#[allow(
    unsafe_code,
    reason = "the libav FFI, each block with its SAFETY comment"
)]
mod av;
mod convert;

use std::collections::VecDeque;
use std::ffi::CStr;
use std::os::fd::{AsFd, OwnedFd};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use tb_decoder_proto::pool::{audio_bytes, pack_planes, plane_shapes};
use tb_decoder_proto::ring;
use tb_decoder_proto::supervise::{EXIT_OOM, EXIT_PROTOCOL, EXIT_SETUP};
use tb_decoder_proto::wire::{
    Aspect, AudioLayout, AudioOut, AudioSpec, Configure, Deinterlace, Fault, FromWorker,
    MAX_MESSAGE, OutputSpec, PacketRef, PoolGrant, ProbeOp, ProbeResult, ProbeStage,
    RESAMPLED_RATE, Refusal, SeccompMode, StreamOpen, StreamState, ToWorker, VideoOut, VideoSpec,
    WorkerStats,
};
use tb_media::{
    AudioFrameDesc, CodecId, Matrix, MediaKind, PixelFormat, Range, Rational, StreamIndex,
    StreamInfo, VideoFrameDesc,
};
use tb_sandbox::seccomp::{Action, Profile};
use tb_sandbox::shm::Mapping;

/// The allocator: the system's, but a failed allocation ends the process with
/// [`EXIT_OOM`] (instead of an abort the parent could not tell from a crash).
mod oom {
    #![allow(unsafe_code, reason = "a GlobalAlloc wrapper")]
    use std::alloc::{GlobalAlloc, Layout, System};

    pub struct ExitOnOom;

    fn out_of_memory() -> ! {
        // SAFETY: ends the process at once; runs nothing that could allocate.
        unsafe { libc::_exit(super::EXIT_OOM) }
    }

    // SAFETY: every call forwards to the system allocator with the same arguments; only a
    // null result is turned into an exit.
    unsafe impl GlobalAlloc for ExitOnOom {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            // SAFETY: forwarded as received.
            let p = unsafe { System.alloc(l) };
            if p.is_null() {
                out_of_memory();
            }
            p
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            // SAFETY: forwarded as received.
            let p = unsafe { System.alloc_zeroed(l) };
            if p.is_null() {
                out_of_memory();
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            // SAFETY: forwarded as received.
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            // SAFETY: forwarded as received.
            let q = unsafe { System.realloc(p, l, n) };
            if q.is_null() {
                out_of_memory();
            }
            q
        }
    }

    /// Dies of `SIGSEGV`, as a fault in libav would (fault injection). Rust's runtime handles
    /// `SIGSEGV` (stack overflow detection) and ignores a raised one, so the default action is
    /// restored first.
    pub fn segfault() -> ! {
        // SAFETY: signal and raise take no memory arguments.
        unsafe {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            libc::raise(libc::SIGSEGV);
            libc::_exit(1)
        }
    }
}

#[global_allocator]
static ALLOC: oom::ExitOnOom = oom::ExitOnOom;

fn exit(code: i32) -> ! {
    std::process::exit(code)
}

/// The control channel.
struct Chan {
    sock: OwnedFd,
    buf: Vec<u8>,
    out: Vec<u8>,
}

impl Chan {
    fn send(&mut self, m: &FromWorker) {
        if m.encode(&mut self.out).is_err() {
            // a message this worker built does not encode: a bug here
            exit(EXIT_PROTOCOL);
        }
        if tb_sandbox::ipc::send(self.sock.as_fd(), &self.out, None, false).is_err() {
            // the parent is gone
            exit(0);
        }
    }

    /// The next message, waiting at most `timeout` (`None`: for ever). `Ok(None)`: timeout.
    fn recv(&mut self, timeout: Option<Duration>) -> Option<(ToWorker, Option<OwnedFd>)> {
        if let Some(t) = timeout {
            let mut p = [rustix::event::PollFd::new(
                &self.sock,
                rustix::event::PollFlags::IN,
            )];
            let ts = rustix::time::Timespec {
                tv_sec: t.as_secs() as i64,
                tv_nsec: i64::from(t.subsec_nanos()),
            };
            match rustix::event::poll(&mut p, Some(&ts)) {
                Ok(0) => return None,
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(_) => exit(0),
            }
        }
        self.buf.resize(MAX_MESSAGE, 0);
        let r = match tb_sandbox::ipc::recv(self.sock.as_fd(), &mut self.buf, timeout.is_some()) {
            Ok(Some(r)) => r,
            Ok(None) => exit(0),
            Err(rustix::io::Errno::AGAIN) => return None,
            Err(_) => exit(0),
        };
        if r.truncated || r.extra_fds {
            exit(EXIT_PROTOCOL);
        }
        let Some(bytes) = self.buf.get(..r.len) else {
            exit(EXIT_PROTOCOL)
        };
        match ToWorker::decode(bytes) {
            Ok(m) => Some((m, r.fd)),
            Err(_) => exit(EXIT_PROTOCOL),
        }
    }
}

/// An output pool as the worker sees it.
struct Pool {
    id: u32,
    map: Mapping,
    slot_size: u64,
    free: Vec<bool>,
}

/// One announced stream.
struct Stream {
    info: StreamInfo,
    /// Native or converted frames.
    output: OutputSpec,
    state: StreamState,
    dec: Option<av::Decoder>,
    parser: Option<av::Parser>,
    /// MPEG audio: the decoder is chosen from the first frame header.
    pending_mpa: bool,
    /// The scaler of converted video.
    scaler: Option<av::Scaler>,
    /// The resampler of resampled audio.
    resampler: Option<av::Resampler>,
    /// Resampled audio: position of the next output sample, in 1/48000 s.
    next48: Option<i64>,
    /// Duration of the last packet (for what a parser still holds at the end).
    last_dur: Option<i64>,
    pool: Option<Pool>,
    denied: bool,
}

struct Worker {
    chan: Chan,
    cfg: Configure,
    ring: Mapping,
    ring_len: u64,
    streams: Vec<Stream>,
    deferred: VecDeque<(ToWorker, Option<OwnedFd>)>,
    frame: av::Frame,
    sw: av::Frame,
    hw: Option<av::HwDevice>,
    stats: WorkerStats,
    packets_seen: u32,
    warned: u32,
}

/// libav's decoder for a codec (the allow-list), if this build has it.
fn decoder_for(codec: CodecId, first: &[u8], gpu: bool) -> Result<av::Codec, Refusal> {
    let names: &[&CStr] = match codec {
        CodecId::H264 => &[c"h264"],
        CodecId::Hevc => &[c"hevc"],
        CodecId::Mpeg2Video => &[c"mpeg2video"],
        // the native AV1 decoder only drives hardware; software is dav1d
        CodecId::Av1 if gpu => &[c"av1", c"libdav1d"],
        CodecId::Av1 => &[c"libdav1d", c"av1"],
        CodecId::Vp9 => &[c"vp9"],
        CodecId::Aac => &[c"aac"],
        CodecId::Ac3 => &[c"ac3"],
        CodecId::Eac3 => &[c"eac3"],
        CodecId::MpegAudio => match mpeg_audio_layer(first) {
            Some(1) => &[c"mp1float", c"mp1"],
            Some(2) => &[c"mp2float", c"mp2"],
            _ => &[c"mp3float", c"mp3"],
        },
        CodecId::Opus => &[c"opus"],
        _ => return Err(Refusal::NotAllowed),
    };
    names
        .iter()
        .find_map(|n| av::Codec::find(n))
        .ok_or(Refusal::NoDecoder)
}

/// The layer of the first MPEG audio frame header in `b` (1, 2 or 3).
fn mpeg_audio_layer(b: &[u8]) -> Option<u8> {
    b.windows(2).find_map(|w| {
        if w[0] == 0xff && w[1] & 0xe0 == 0xe0 {
            match (w[1] >> 1) & 3 {
                3 => Some(1),
                2 => Some(2),
                1 => Some(3),
                _ => None,
            }
        } else {
            None
        }
    })
}

/// Whether a codec's packets need a parser to become access units.
fn needs_parser(s: &StreamInfo) -> bool {
    let annex_b = s.extradata.is_empty()
        || s.extradata.starts_with(&[0, 0, 1])
        || s.extradata.starts_with(&[0, 0, 0, 1]);
    match s.codec {
        CodecId::H264 | CodecId::Hevc => annex_b,
        CodecId::Mpeg2Video | CodecId::Ac3 | CodecId::Eac3 | CodecId::MpegAudio => true,
        CodecId::Aac => s.extradata.is_empty(),
        _ => false,
    }
}

fn tb(r: Rational) -> (i32, i32) {
    (
        i32::try_from(r.num()).unwrap_or(1),
        i32::try_from(r.den()).unwrap_or(1),
    )
}

impl Worker {
    fn send(&mut self, m: &FromWorker) {
        self.chan.send(m);
    }

    fn error(&mut self, code: u16, text: &str) {
        // at most a few dozen notes per worker: the parent logs them
        if self.warned < 32 {
            self.warned += 1;
            let text = text.as_bytes().iter().take(200).copied().collect();
            self.send(&FromWorker::Error { code, text });
        }
    }

    fn stats(&self) -> WorkerStats {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
        WorkerStats {
            cpu_us: t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1_000,
            ..self.stats
        }
    }

    /// The next message: deferred ones first.
    fn next(&mut self) -> (ToWorker, Option<OwnedFd>) {
        if let Some(m) = self.deferred.pop_front() {
            return m;
        }
        loop {
            if let Some(m) = self.chan.recv(None) {
                return m;
            }
        }
    }

    /// While waiting for something (a pool grant, a free slot): answers pings, applies
    /// releases, exits on shutdown, defers the rest. Returns a grant or denial when one comes,
    /// `None` at the deadline.
    fn wait_aside(&mut self, until: Instant) -> Option<(ToWorker, Option<OwnedFd>)> {
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let (m, fd) = self.chan.recv(Some(left))?;
            match m {
                ToWorker::Ping { seq } => {
                    let stats = self.stats();
                    self.send(&FromWorker::Pong { seq, stats });
                }
                ToWorker::Release { pool, slots } => {
                    self.release(pool, &slots);
                    return Some((
                        ToWorker::Release {
                            pool,
                            slots: vec![],
                        },
                        None,
                    ));
                }
                ToWorker::Shutdown => exit(0),
                m @ (ToWorker::PoolGrant(_) | ToWorker::PoolDenied { .. }) => return Some((m, fd)),
                m => self.deferred.push_back((m, fd)),
            }
        }
    }

    fn release(&mut self, pool: u32, slots: &[u16]) {
        let Some(p) = self
            .streams
            .iter_mut()
            .filter_map(|s| s.pool.as_mut())
            .find(|p| p.id == pool)
        else {
            // a pool retired since: nothing to do
            return;
        };
        for s in slots {
            match p.free.get_mut(usize::from(*s)) {
                Some(f) => *f = true,
                None => exit(EXIT_PROTOCOL),
            }
        }
    }

    fn handle_streams(&mut self, list: Vec<StreamOpen>) {
        for s in std::mem::take(&mut self.streams) {
            if let Some(p) = s.pool {
                self.send(&FromWorker::RetirePool { pool: p.id });
            }
        }
        for StreamOpen { info, output } in list {
            let mut s = Stream {
                output,
                state: StreamState::Accepted,
                dec: None,
                parser: None,
                pending_mpa: false,
                scaler: None,
                resampler: None,
                next48: None,
                last_dur: None,
                pool: None,
                denied: false,
                info,
            };
            match s.info.codec {
                CodecId::PcmS16le => {
                    let ok = matches!(s.info.audio.sample_rate, Some(1000..=384_000))
                        && matches!(s.info.audio.channels, Some(1..=16));
                    if !ok {
                        s.state = StreamState::Refused(Refusal::MissingParameters);
                    }
                }
                CodecId::MpegAudio => {
                    // the layer is in the first packet: open then
                    if decoder_for(CodecId::MpegAudio, &[], false).is_err() {
                        s.state = StreamState::Refused(Refusal::NoDecoder);
                    } else {
                        s.pending_mpa = true;
                    }
                }
                _ => {
                    if let Err(r) = self.open(&mut s, &[]) {
                        s.state = StreamState::Refused(r);
                    }
                }
            }
            self.send(&FromWorker::StreamStatus {
                stream: s.info.index,
                state: s.state,
            });
            self.streams.push(s);
        }
    }

    fn open(&mut self, s: &mut Stream, first: &[u8]) -> Result<(), Refusal> {
        let gpu = self.hw.is_some() && s.info.codec.kind() == MediaKind::Video;
        let codec = decoder_for(s.info.codec, first, gpu)?;
        let p = av::OpenParams {
            extradata: &s.info.extradata,
            time_base: tb(s.info.time_base),
            threads: i32::from(self.cfg.threads),
            audio: match (s.info.audio.sample_rate, s.info.audio.channels) {
                (Some(r), Some(c)) => Some((i32::try_from(r).unwrap_or(0), i32::from(c))),
                _ => None,
            },
            video: match (s.info.video.width, s.info.video.height) {
                (Some(w), Some(h)) => {
                    Some((i32::try_from(w).unwrap_or(0), i32::try_from(h).unwrap_or(0)))
                }
                _ => None,
            },
            hw: if gpu { self.hw.as_ref() } else { None },
        };
        let dec = av::Decoder::open(codec, &p).map_err(|e| {
            if e.0 == av::ENOMEM {
                exit(EXIT_OOM);
            }
            Refusal::OpenFailed
        })?;
        s.parser = if needs_parser(&s.info) {
            av::Parser::new(codec)
        } else {
            None
        };
        s.dec = Some(dec);
        Ok(())
    }

    fn fault(&mut self) {
        self.packets_seen = self.packets_seen.saturating_add(1);
        let n = self.packets_seen;
        match self.cfg.fault {
            Fault::CrashAfter(k) if n == k => oom::segfault(),
            Fault::HangAfter(k) if n == k => loop {
                std::thread::sleep(Duration::from_secs(3600));
            },
            Fault::SpinAfter(k) if n == k => loop {
                std::hint::spin_loop();
            },
            Fault::AllocAfter(k) if n == k => {
                // address space only (never touched): the AS limit ends it without using RAM
                let mut keep: Vec<Vec<u8>> = Vec::new();
                loop {
                    keep.push(Vec::with_capacity(64 << 20));
                    std::hint::black_box(&keep);
                }
            }
            Fault::BadDescriptorAfter(k) if n == k => {
                let pool = self
                    .streams
                    .iter()
                    .find_map(|s| s.pool.as_ref().map(|p| p.id))
                    .unwrap_or(0);
                let stream = self
                    .streams
                    .first()
                    .map_or(StreamIndex(0), |s| s.info.index);
                let (planes, _) = pack_planes(PixelFormat::Yuv420p, 64, 64, 64);
                self.send(&FromWorker::Video(VideoOut {
                    pool,
                    slot: 60_000,
                    desc: VideoFrameDesc {
                        stream,
                        pts: None,
                        width: 64,
                        height: 64,
                        format: PixelFormat::Yuv420p,
                        planes,
                        interlaced: false,
                        top_field_first: false,
                        matrix: Matrix::Unspecified,
                        range: Range::Unspecified,
                        sample_aspect: None,
                        cc_data: vec![],
                        corrupt: false,
                        active: None,
                    },
                }));
            }
            _ => {}
        }
    }

    fn handle_packet(&mut self, p: PacketRef) {
        self.fault();
        let range = match ring::locate(self.ring_len, p.pos, p.len) {
            Ok(r) => r,
            Err(_) => exit(EXIT_PROTOCOL),
        };
        let Some(data) = self.ring.slice(range.start, range.len()) else {
            exit(EXIT_PROTOCOL)
        };
        let si = self.streams.iter().position(|s| s.info.index == p.stream);
        let accepted = si.is_some_and(|i| self.streams[i].state == StreamState::Accepted);
        // an owned copy: the ring's space goes back at once
        let pkt_bytes = if accepted { Some(data.to_vec()) } else { None };
        self.stats.packets += 1;
        self.send(&FromWorker::Consumed {
            upto: p.pos + u64::from(p.len),
        });
        let (Some(i), Some(bytes)) = (si, pkt_bytes) else {
            self.stats.dropped_packets += 1;
            return;
        };
        if self.streams[i].info.codec == CodecId::PcmS16le {
            self.pcm(i, &bytes, p.pts);
            return;
        }
        if self.streams[i].pending_mpa {
            let mut s = std::mem::replace(&mut self.streams[i], placeholder());
            s.pending_mpa = false;
            if let Err(r) = self.open(&mut s, &bytes) {
                s.state = StreamState::Refused(r);
                self.send(&FromWorker::StreamStatus {
                    stream: s.info.index,
                    state: s.state,
                });
            }
            self.streams[i] = s;
            if self.streams[i].state != StreamState::Accepted {
                return;
            }
        }
        self.decode(i, &bytes, p);
    }

    fn decode(&mut self, i: usize, bytes: &[u8], p: PacketRef) {
        let mut units = Vec::new();
        {
            let s = &mut self.streams[i];
            s.last_dur = p.duration;
            let Some(dec) = s.dec.as_mut() else { return };
            match s.parser.as_mut() {
                Some(parser) => {
                    if parser
                        .parse(dec, bytes, (p.pts, p.dts), p.duration, |u| {
                            units.push(u.packet)
                        })
                        .is_err()
                    {
                        self.stats.decode_errors += 1;
                    }
                }
                None => match av::Packet::copy(bytes, p.pts, p.dts, p.duration, p.keyframe) {
                    Ok(pk) => units.push(pk),
                    Err(e) if e.0 == av::ENOMEM => exit(EXIT_OOM),
                    Err(_) => self.stats.decode_errors += 1,
                },
            }
        }
        for u in units {
            self.send_unit(i, Some(&u));
        }
    }

    /// Sends one unit (or the end, `None`) and hands out every frame that comes.
    fn send_unit(&mut self, i: usize, u: Option<&av::Packet>) {
        loop {
            let r = match self.streams[i].dec.as_mut() {
                Some(d) => d.send(u),
                None => return,
            };
            match r {
                Ok(()) => {
                    self.receive_all(i);
                    return;
                }
                Err(e) if e.0 == av::EAGAIN => {
                    // the decoder's output is full: drain it, then send again
                    self.receive_all(i);
                }
                Err(e) if e.0 == av::ENOMEM => exit(EXIT_OOM),
                Err(e) if e.0 == av::EOF => return,
                Err(_) => {
                    self.stats.decode_errors += 1;
                    self.receive_all(i);
                    return;
                }
            }
        }
    }

    fn receive_all(&mut self, i: usize) {
        loop {
            let r = match self.streams[i].dec.as_mut() {
                Some(d) => d.receive(&mut self.frame),
                None => return,
            };
            match r {
                Ok(true) => {
                    match self.streams[i].info.codec.kind() {
                        MediaKind::Video => self.emit_video(i),
                        _ => self.emit_audio(i),
                    }
                    self.frame.unref();
                }
                Ok(false) => return,
                Err(e) if e.0 == av::ENOMEM => exit(EXIT_OOM),
                Err(_) => {
                    self.stats.decode_errors += 1;
                    return;
                }
            }
        }
    }

    /// A pool for stream `i` with slots of at least `need` bytes. Waits for the grant.
    fn ensure_pool(&mut self, i: usize, need: u64, ask: u64, slots: u16) -> bool {
        if self.streams[i].denied {
            return false;
        }
        if self.streams[i]
            .pool
            .as_ref()
            .is_some_and(|p| p.slot_size >= need)
        {
            return true;
        }
        if let Some(old) = self.streams[i].pool.take() {
            self.send(&FromWorker::RetirePool { pool: old.id });
        }
        let stream = self.streams[i].info.index;
        self.send(&FromWorker::NeedPool {
            stream,
            slot_size: ask,
            slots,
        });
        // the parent answers at once; a parent that does not is the parent's watchdog's
        // business, not this one's
        let until = Instant::now() + Duration::from_secs(3600);
        loop {
            match self.wait_aside(until) {
                Some((ToWorker::PoolGrant(g), fd)) => return self.take_grant(i, g, fd),
                Some((ToWorker::PoolDenied { stream: s }, _)) if s == stream => {
                    self.streams[i].denied = true;
                    self.error(2, "output pool denied");
                    return false;
                }
                Some((ToWorker::Release { .. }, _)) => {}
                Some(_) | None => exit(EXIT_PROTOCOL),
            }
        }
    }

    fn take_grant(&mut self, i: usize, g: PoolGrant, fd: Option<OwnedFd>) -> bool {
        let bytes = g.slot_size.checked_mul(u64::from(g.slots));
        let (Some(fd), Some(bytes)) = (fd, bytes) else {
            exit(EXIT_PROTOCOL)
        };
        if g.stream != self.streams[i].info.index || g.slots == 0 {
            exit(EXIT_PROTOCOL);
        }
        if tb_sandbox::shm::check(fd.as_fd(), bytes).is_err() {
            exit(EXIT_PROTOCOL);
        }
        let Ok(len) = usize::try_from(bytes) else {
            exit(EXIT_PROTOCOL)
        };
        let map = match Mapping::map(&fd, len, true) {
            Ok(m) => m,
            Err(rustix::io::Errno::NOMEM) => exit(EXIT_OOM),
            Err(_) => exit(EXIT_PROTOCOL),
        };
        self.streams[i].pool = Some(Pool {
            id: g.pool,
            map,
            slot_size: g.slot_size,
            free: vec![true; usize::from(g.slots)],
        });
        true
    }

    /// A free slot of stream `i`'s pool, waiting up to `slot_wait_ms` for a release.
    fn acquire(&mut self, i: usize) -> Option<u16> {
        let find = |s: &Stream| s.pool.as_ref().and_then(|p| p.free.iter().position(|f| *f));
        if let Some(k) = find(&self.streams[i]) {
            return u16::try_from(k).ok();
        }
        let until = Instant::now() + Duration::from_millis(u64::from(self.cfg.slot_wait_ms));
        loop {
            match self.wait_aside(until) {
                Some((ToWorker::Release { .. }, _)) => {
                    if let Some(k) = find(&self.streams[i]) {
                        return u16::try_from(k).ok();
                    }
                }
                None => return None,
                Some(_) => exit(EXIT_PROTOCOL),
            }
        }
    }

    fn emit_video(&mut self, i: usize) {
        if let OutputSpec::Video(spec) = self.streams[i].output {
            self.emit_converted(i, spec);
            return;
        }
        let on_device = self.frame.pix() == av::Pix::Device;
        if on_device && self.frame.transfer_to(&mut self.sw).is_err() {
            self.stats.decode_errors += 1;
            return;
        }
        let f = if on_device { &self.sw } else { &self.frame };
        let pix = f.pix();
        let (w, h) = f.size();
        let format = match pix {
            av::Pix::Yuv420p | av::Pix::Yuv422p => PixelFormat::Yuv420p,
            av::Pix::Nv12 => PixelFormat::Nv12,
            av::Pix::Yuv420p10 | av::Pix::P010 => PixelFormat::P010,
            av::Pix::Device | av::Pix::Other(_) => {
                self.stats.unsupported_frames += 1;
                let t = format!("unsupported pixel format {pix:?}");
                self.error(1, &t);
                return;
            }
        };
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            self.stats.unsupported_frames += 1;
            return;
        }
        let (planes, need) = pack_planes(format, w, h, 64);
        let slots = self.cfg.video_slots;
        if need > self.cfg.max_frame_bytes || !self.ensure_pool(i, need, need, slots) {
            self.stats.dropped_no_slot += 1;
            return;
        }
        let Some(slot) = self.acquire(i) else {
            self.stats.dropped_no_slot += 1;
            return;
        };
        // borrow the frame again (ensure_pool and acquire took `self` mutably)
        let f = if on_device { &self.sw } else { &self.frame };
        let s = &mut self.streams[i];
        let Some(pool) = s.pool.as_mut() else { return };
        let off = usize::from(slot) * pool.slot_size as usize;
        let Some(dst) = pool.map.slice_mut(off, need as usize) else {
            return;
        };
        let shapes = plane_shapes(format, w, h);
        let ok = write_picture(dst, f, pix, &planes, &shapes, h);
        if !ok {
            self.stats.unsupported_frames += 1;
            return;
        }
        let (interlaced, tff) = f.interlace();
        let desc = VideoFrameDesc {
            stream: s.info.index,
            pts: f.pts(),
            width: w,
            height: h,
            format,
            planes,
            interlaced,
            top_field_first: interlaced && tff,
            matrix: match f.matrix() {
                1 => Matrix::Bt709,
                2 => Matrix::Bt601,
                3 => Matrix::Bt2020,
                _ => Matrix::Unspecified,
            },
            range: match f.full_range() {
                Some(true) => Range::Full,
                Some(false) => Range::Limited,
                None => Range::Unspecified,
            },
            sample_aspect: f.sar().and_then(|(n, d)| Rational::new(n, d).ok()),
            cc_data: {
                let mut cc = f.a53_cc();
                cc.truncate(tb_decoder_proto::wire::MAX_CC / 3 * 3);
                cc.truncate(cc.len() / 3 * 3);
                cc
            },
            corrupt: f.corrupt(),
            active: None,
        };
        let pool_id = pool.id;
        if let Some(fr) = pool.free.get_mut(usize::from(slot)) {
            *fr = false;
        }
        self.stats.frames += 1;
        self.send(&FromWorker::Video(VideoOut {
            pool: pool_id,
            slot,
            desc,
        }));
    }

    fn emit_audio(&mut self, i: usize) {
        if let OutputSpec::Audio(spec) = self.streams[i].output {
            self.emit_resampled(i, spec, false);
            return;
        }
        let (n, rate, ch) = self.frame.audio();
        let kind = match self.frame.smp() {
            av::Smp::Fltp => Some((convert::Sample::F32, false)),
            av::Smp::Flt => Some((convert::Sample::F32, true)),
            av::Smp::S16(p) => Some((convert::Sample::S16, !p)),
            av::Smp::S32(p) => Some((convert::Sample::S32, !p)),
            av::Smp::Dbl(p) => Some((convert::Sample::F64, !p)),
            av::Smp::Other(_) => None,
        };
        let (Some((kind, packed)), Ok(ch8)) = (kind, u8::try_from(ch)) else {
            self.stats.unsupported_frames += 1;
            return;
        };
        if n == 0 || ch == 0 || ch > 8 || !(1000..=384_000).contains(&rate) {
            self.stats.unsupported_frames += 1;
            return;
        }
        let pts = self.frame.pts();
        let need = audio_bytes(ch8, n);
        let ask = audio_bytes(ch8, n.max(2048));
        let slots = self.cfg.audio_slots;
        if !self.ensure_pool(i, need, ask, slots) {
            self.stats.dropped_no_slot += 1;
            return;
        }
        let Some(slot) = self.acquire(i) else {
            self.stats.dropped_no_slot += 1;
            return;
        };
        let f = &self.frame;
        let s = &mut self.streams[i];
        let Some(pool) = s.pool.as_mut() else { return };
        let off = usize::from(slot) * pool.slot_size as usize;
        let Some(dst) = pool.map.slice_mut(off, need as usize) else {
            return;
        };
        let (chu, nu) = (ch as usize, n as usize);
        let plane_bytes = if packed {
            nu * chu * kind.size()
        } else {
            nu * kind.size()
        };
        let ok = convert::audio_to_f32_planar(dst, chu, nu, kind, packed, |c| {
            f.audio_plane(c, plane_bytes)
        });
        if !ok {
            self.stats.unsupported_frames += 1;
            return;
        }
        let pool_id = pool.id;
        if let Some(fr) = pool.free.get_mut(usize::from(slot)) {
            *fr = false;
        }
        let stream = s.info.index;
        self.stats.frames += 1;
        self.send(&FromWorker::Audio(AudioOut {
            pool: pool_id,
            slot,
            desc: AudioFrameDesc {
                stream,
                pts,
                sample_rate: rate,
                channels: ch8,
                samples: n,
            },
        }));
    }

    /// Linear PCM, signed 16-bit little-endian interleaved: no libav.
    fn pcm(&mut self, i: usize, bytes: &[u8], pts: Option<i64>) {
        let (Some(rate), Some(ch)) = (
            self.streams[i].info.audio.sample_rate,
            self.streams[i].info.audio.channels,
        ) else {
            return;
        };
        let n = (bytes.len() / (2 * usize::from(ch.max(1)))) as u32;
        if n == 0 {
            return;
        }
        let need = audio_bytes(ch, n);
        if !self.ensure_pool(i, need, audio_bytes(ch, n.max(2048)), self.cfg.audio_slots) {
            self.stats.dropped_no_slot += 1;
            return;
        }
        let Some(slot) = self.acquire(i) else {
            self.stats.dropped_no_slot += 1;
            return;
        };
        let s = &mut self.streams[i];
        let Some(pool) = s.pool.as_mut() else { return };
        let off = usize::from(slot) * pool.slot_size as usize;
        let Some(dst) = pool.map.slice_mut(off, need as usize) else {
            return;
        };
        if !convert::audio_to_f32_planar(
            dst,
            usize::from(ch),
            n as usize,
            convert::Sample::S16,
            true,
            |_| Some(bytes),
        ) {
            return;
        }
        let pool_id = pool.id;
        if let Some(fr) = pool.free.get_mut(usize::from(slot)) {
            *fr = false;
        }
        let stream = s.info.index;
        self.stats.frames += 1;
        self.send(&FromWorker::Audio(AudioOut {
            pool: pool_id,
            slot,
            desc: AudioFrameDesc {
                stream,
                pts,
                sample_rate: rate,
                channels: ch,
                samples: n,
            },
        }));
    }

    /// Converted video: libswscale (Lanczos) to planar 4:2:0 of the target size, rows without
    /// padding, BT.709 from 720 lines up and BT.601 below, limited range; letterboxed or
    /// pillarboxed with black, or stretched; bob deinterlacing (each field to the full height,
    /// two pictures) of pictures flagged interlaced when asked.
    fn emit_converted(&mut self, i: usize, spec: VideoSpec) {
        let on_device = self.frame.pix() == av::Pix::Device;
        if on_device && self.frame.transfer_to(&mut self.sw).is_err() {
            self.stats.decode_errors += 1;
            return;
        }
        // the picture is taken out of `self` for the loop below, which needs `self` mutably
        let blank = match av::Frame::new() {
            Ok(f) => f,
            Err(_) => exit(EXIT_OOM),
        };
        let f = std::mem::replace(
            if on_device {
                &mut self.sw
            } else {
                &mut self.frame
            },
            blank,
        );
        self.convert_picture(i, spec, &f);
        *(if on_device {
            &mut self.sw
        } else {
            &mut self.frame
        }) = f;
    }

    fn convert_picture(&mut self, i: usize, spec: VideoSpec, f: &av::Frame) {
        let (w, h) = f.size();
        if w < 2 || h < 2 || w > 16384 || h > 16384 {
            self.stats.unsupported_frames += 1;
            return;
        }
        let (interlaced, tff) = f.interlace();
        let bob = spec.deinterlace == Deinterlace::Bob && interlaced && h % 4 == 0;
        let fields: &[Option<usize>] = match (bob, tff) {
            (false, _) => &[None],
            (true, true) => &[Some(0), Some(1)],
            (true, false) => &[Some(1), Some(0)],
        };
        let src_h = if bob { h / 2 } else { h };
        let (tw, th) = (spec.width, spec.height);
        // the source's display aspect, and the rectangle it fills
        let (sn, sd) = f.sar().unwrap_or((1, 1));
        let src_dar = f64::from(w) * f64::from(sn) / (f64::from(h) * f64::from(sd));
        let tgt_dar = f64::from(spec.display_aspect.num()) / f64::from(spec.display_aspect.den());
        let even = |x: f64, max: u32| ((x / 2.0).round() as u32 * 2).clamp(2, max);
        let (rw, rh) = match spec.aspect {
            Aspect::Stretch => (tw, th),
            Aspect::Fit if src_dar > tgt_dar => (tw, even(f64::from(th) * tgt_dar / src_dar, th)),
            Aspect::Fit => (even(f64::from(tw) * src_dar / tgt_dar, tw), th),
        };
        let (x0, y0) = (((tw - rw) / 4 * 2) as usize, ((th - rh) / 4 * 2) as usize);
        let src_m = match f.matrix() {
            0 if h >= 720 => 1,
            0 => 2,
            m => m,
        };
        let dst_m = if th >= 720 { 1 } else { 2 };
        let key = (
            w as i32,
            src_h as i32,
            f.raw_format(),
            av::sws_cs(src_m),
            f.full_range() == Some(true),
            rw as i32,
            rh as i32,
            av::sws_cs(dst_m),
        );
        if self.streams[i].scaler.as_ref().is_none_or(|s| s.key != key) {
            self.streams[i].scaler = av::Scaler::new(key);
        }
        if self.streams[i].scaler.is_none() {
            self.stats.unsupported_frames += 1;
            self.error(1, "libswscale cannot convert this pixel format");
            return;
        }
        let (tw_u, th_u) = (tw as usize, th as usize);
        let need = (tw_u * th_u * 3 / 2) as u64;
        if !self.ensure_pool(i, need, need, self.cfg.video_slots) {
            self.stats.dropped_no_slot += 1;
            return;
        }
        let pts0 = f.pts();
        // the second field half a picture later: from the frame's duration, else the rate
        let half = f.duration().map(|d| d / 2).or_else(|| {
            let r = self.streams[i].info.video.frame_rate?;
            let tb = self.streams[i].info.time_base;
            Some(Rational::new(r.den(), r.num()).ok()?.rescale(1, tb) / 2)
        });
        let cc = {
            let mut cc = f.a53_cc();
            cc.truncate(tb_decoder_proto::wire::MAX_CC / 3 * 3);
            cc.truncate(cc.len() / 3 * 3);
            cc
        };
        let corrupt = f.corrupt();
        let (planes, _) = pack_planes(PixelFormat::Yuv420p, tw, th, 1);
        let sar = Rational::new(
            spec.display_aspect.num() * th,
            spec.display_aspect.den() * tw,
        )
        .ok();
        for (k, field) in fields.iter().enumerate() {
            let Some(slot) = self.acquire(i) else {
                self.stats.dropped_no_slot += 1;
                continue;
            };
            let s = &mut self.streams[i];
            let (Some(pool), Some(scaler)) = (s.pool.as_mut(), s.scaler.as_mut()) else {
                return;
            };
            let off = usize::from(slot) * pool.slot_size as usize;
            let Some(dst) = pool.map.slice_mut(off, need as usize) else {
                return;
            };
            let (y, c) = dst.split_at_mut(tw_u * th_u);
            let (u, v) = c.split_at_mut(tw_u * th_u / 4);
            if (rw, rh) != (tw, th) {
                y.fill(16);
                u.fill(128);
                v.fill(128);
            }
            let cs = tw_u / 2;
            let yo = y0 * tw_u + x0;
            let co = (y0 / 2) * cs + x0 / 2;
            let ok = match (y.get_mut(yo..), u.get_mut(co..), v.get_mut(co..)) {
                (Some(y), Some(u), Some(v)) => scaler.scale(
                    f,
                    *field,
                    av::Dst {
                        planes: [y, u, v],
                        strides: [tw_u, cs, cs],
                    },
                ),
                _ => false,
            };
            if !ok {
                self.stats.unsupported_frames += 1;
                return;
            }
            let pool_id = pool.id;
            if let Some(fr) = pool.free.get_mut(usize::from(slot)) {
                *fr = false;
            }
            let desc = VideoFrameDesc {
                stream: s.info.index,
                pts: if k == 0 {
                    pts0
                } else {
                    pts0.zip(half).map(|(p, d)| p + d)
                },
                width: tw,
                height: th,
                format: PixelFormat::Yuv420p,
                planes: planes.clone(),
                interlaced: interlaced && !bob,
                top_field_first: interlaced && !bob && tff,
                matrix: spec.matrix(),
                range: Range::Limited,
                sample_aspect: sar,
                cc_data: if k == 0 { cc.clone() } else { Vec::new() },
                corrupt,
                active: Some((rw, rh)),
            };
            self.stats.frames += 1;
            self.send(&FromWorker::Video(VideoOut {
                pool: pool_id,
                slot,
                desc,
            }));
        }
    }

    /// Resampled audio: libswresample to 48 kHz f32 planar, stereo or 5.1 in A/52 order
    /// (L, C, R, Ls, Rs, LFE). `flush`: what the resampler still holds (at the session's end).
    fn emit_resampled(&mut self, i: usize, spec: AudioSpec, flush: bool) {
        let (mask, order): (u64, &[usize]) = match spec.layout {
            // libav's native order of the mask's bits: FL FR
            AudioLayout::Stereo => (0x3, &[0, 1]),
            // FL FR FC LFE SL SR in native order; A/52 wants L C R Ls Rs LFE
            AudioLayout::Surround51 => (0x60f, &[0, 2, 1, 4, 5, 3]),
        };
        let tb = self.streams[i].info.time_base;
        let r48 = match Rational::new(1, RESAMPLED_RATE) {
            Ok(r) => r,
            Err(_) => return,
        };
        let s = &mut self.streams[i];
        if !flush {
            if s.resampler.as_ref().is_none_or(|r| !r.fits(&self.frame)) {
                s.resampler = av::Resampler::new(&self.frame, mask, RESAMPLED_RATE as i32);
            }
            if let Some(p) = self.frame.pts()
                && let Some(r) = s.resampler.as_ref()
            {
                s.next48 = Some(tb.rescale(p, r48) - r.delay(i64::from(RESAMPLED_RATE)));
            }
        }
        let Some(r) = s.resampler.as_mut() else {
            if !flush {
                self.stats.unsupported_frames += 1;
            }
            return;
        };
        let planes: Vec<Vec<f32>> = match r.convert(if flush { None } else { Some(&self.frame) }) {
            Ok(p) => order.iter().filter_map(|&c| p.get(c).cloned()).collect(),
            Err(e) if e.0 == av::ENOMEM => exit(EXIT_OOM),
            Err(_) => {
                self.stats.decode_errors += 1;
                return;
            }
        };
        let n = planes.first().map_or(0, Vec::len);
        if n == 0 || planes.len() != order.len() {
            return;
        }
        let pts48 = s.next48;
        s.next48 = pts48.map(|p| p + n as i64);
        let pts = pts48.map(|p| r48.rescale(p, tb));
        let ch = spec.layout.channels();
        let n32 = n as u32;
        let need = audio_bytes(ch, n32);
        if !self.ensure_pool(
            i,
            need,
            audio_bytes(ch, n32.max(2048)),
            self.cfg.audio_slots,
        ) {
            self.stats.dropped_no_slot += 1;
            return;
        }
        let Some(slot) = self.acquire(i) else {
            self.stats.dropped_no_slot += 1;
            return;
        };
        let s = &mut self.streams[i];
        let Some(pool) = s.pool.as_mut() else { return };
        let off = usize::from(slot) * pool.slot_size as usize;
        let Some(dst) = pool.map.slice_mut(off, need as usize) else {
            return;
        };
        for (c, plane) in planes.iter().enumerate() {
            for (j, v) in plane.iter().enumerate() {
                let o = (c * n + j) * 4;
                if let Some(d) = dst.get_mut(o..o + 4) {
                    d.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        let pool_id = pool.id;
        if let Some(fr) = pool.free.get_mut(usize::from(slot)) {
            *fr = false;
        }
        let stream = s.info.index;
        self.stats.frames += 1;
        self.send(&FromWorker::Audio(AudioOut {
            pool: pool_id,
            slot,
            desc: AudioFrameDesc {
                stream,
                pts,
                sample_rate: RESAMPLED_RATE,
                channels: ch,
                samples: n32,
            },
        }));
    }

    fn handle_end(&mut self) {
        for i in 0..self.streams.len() {
            if self.streams[i].dec.is_none() {
                continue;
            }
            // what the parser still holds, then the decoder's own delay
            let mut units = Vec::new();
            {
                let s = &mut self.streams[i];
                if let (Some(parser), Some(dec)) = (s.parser.as_mut(), s.dec.as_mut()) {
                    let _ =
                        parser.parse(dec, &[], (None, None), s.last_dur, |u| units.push(u.packet));
                }
            }
            for u in units {
                self.send_unit(i, Some(&u));
            }
            self.send_unit(i, None);
            if let OutputSpec::Audio(spec) = self.streams[i].output {
                self.emit_resampled(i, spec, true);
            }
            let s = &mut self.streams[i];
            s.resampler = None;
            s.next48 = None;
            if let Some(d) = s.dec.as_mut() {
                d.flush();
            }
            // a fresh parser for the next session
            if s.parser.is_some()
                && let Ok(c) = decoder_for(s.info.codec, &[], false)
            {
                s.parser = av::Parser::new(c);
            }
        }
        self.send(&FromWorker::Ended);
    }
}

/// Copies or converts the picture in `f` into `dst` laid out as `planes`.
fn write_picture(
    dst: &mut [u8],
    f: &av::Frame,
    pix: av::Pix,
    planes: &[tb_media::Plane],
    shapes: &[(u64, u64)],
    h: u32,
) -> bool {
    let plane = |k: usize| {
        let p = planes.get(k)?;
        let (row, rows) = *shapes.get(k)?;
        Some((
            p.offset as usize,
            p.stride as usize,
            row as usize,
            rows as usize,
        ))
    };
    let mut ok = true;
    match pix {
        av::Pix::Yuv420p | av::Pix::Nv12 | av::Pix::P010 => {
            for k in 0..planes.len() {
                let Some((off, stride, row, rows)) = plane(k) else {
                    return false;
                };
                let Some(d) = dst.get_mut(off..) else {
                    return false;
                };
                ok &= convert::copy_plane(d, stride, rows, row, |r| f.row(k, r, row));
            }
        }
        av::Pix::Yuv422p => {
            let Some((off, stride, row, rows)) = plane(0) else {
                return false;
            };
            let Some(d) = dst.get_mut(off..) else {
                return false;
            };
            ok &= convert::copy_plane(d, stride, rows, row, |r| f.row(0, r, row));
            for k in 1..3 {
                let Some((off, stride, row, rows)) = plane(k) else {
                    return false;
                };
                let Some(d) = dst.get_mut(off..) else {
                    return false;
                };
                ok &= convert::chroma_422_to_420(d, stride, rows, h as usize, row, |r| {
                    f.row(k, r, row)
                });
            }
        }
        av::Pix::Yuv420p10 => {
            let Some((off, stride, row, rows)) = plane(0) else {
                return false;
            };
            let Some(d) = dst.get_mut(off..) else {
                return false;
            };
            let n = row / 2;
            ok &= convert::luma_10_to_p010(d, stride, rows, n, |r| f.row(0, r, 2 * n));
            let Some((off, stride, row, rows)) = plane(1) else {
                return false;
            };
            let Some(d) = dst.get_mut(off..) else {
                return false;
            };
            let n = row / 4;
            ok &= convert::chroma_10_to_p010(
                d,
                stride,
                rows,
                n,
                |r| f.row(1, r, 2 * n),
                |r| f.row(2, r, 2 * n),
            );
        }
        av::Pix::Device | av::Pix::Other(_) => return false,
    }
    ok
}

fn placeholder() -> Stream {
    Stream {
        output: OutputSpec::Native,
        info: StreamInfo {
            index: StreamIndex(u16::MAX),
            codec: CodecId::Text,
            time_base: Rational::MILLIS,
            extradata: vec![],
            video: Default::default(),
            audio: Default::default(),
            language: None,
        },
        state: StreamState::Refused(Refusal::NotAllowed),
        dec: None,
        parser: None,
        pending_mpa: false,
        scaler: None,
        resampler: None,
        next48: None,
        last_dur: None,
        pool: None,
        denied: true,
    }
}

fn probe_ops(ops: &[tb_sandbox::probe::Op]) -> Vec<ProbeResult> {
    use tb_sandbox::probe::Op;
    ops.iter()
        .map(|op| ProbeResult {
            op: match op {
                Op::ReadPasswd => ProbeOp::ReadPasswd,
                Op::WriteFile => ProbeOp::WriteFile,
                Op::Socket => ProbeOp::Socket,
                Op::ConnectUnix => ProbeOp::ConnectUnix,
                Op::Exec => ProbeOp::Exec,
                Op::Ptrace => ProbeOp::Ptrace,
                Op::SignalOutside => ProbeOp::SignalOutside,
                Op::Network => ProbeOp::Network,
            },
            errno: tb_sandbox::probe::attempt(*op),
        })
        .collect()
}

fn main() -> ExitCode {
    let mut sb = match tb_sandbox::child::enter() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tb-avworker: {e}");
            return ExitCode::from(EXIT_SETUP as u8);
        }
    };
    let Some(sock) = sb.take_ipc() else {
        return ExitCode::from(EXIT_SETUP as u8);
    };
    let mut chan = Chan {
        sock,
        buf: vec![0; MAX_MESSAGE],
        out: Vec::with_capacity(MAX_MESSAGE),
    };
    chan.send(&FromWorker::Hello {
        pid: rustix::process::getpid().as_raw_nonzero().get() as u32,
        avcodec_version: av::avcodec_version(),
    });
    let Some((ToWorker::Configure(cfg), Some(ring_fd))) = chan.recv(None) else {
        return ExitCode::from(EXIT_PROTOCOL as u8);
    };
    let Ok(ring_len) = usize::try_from(cfg.input_ring_len) else {
        return ExitCode::from(EXIT_PROTOCOL as u8);
    };
    if tb_sandbox::shm::check(ring_fd.as_fd(), cfg.input_ring_len).is_err() {
        return ExitCode::from(EXIT_PROTOCOL as u8);
    }
    let Ok(ring) = Mapping::map(&ring_fd, ring_len, false) else {
        return ExitCode::from(EXIT_SETUP as u8);
    };
    drop(ring_fd);

    // libav: everything that may need files happens now
    av::quiet();
    let hw = if cfg.gpu {
        // NVDEC's library is loaded by libavcodec when a decoder opens, which is after the
        // system-call filter (no more opening of files): load it now (where there is none,
        // nothing is loaded; VA-API's driver is loaded when its device opens, below)
        av::preload(&[
            c"/run/opengl-driver/lib/libnvcuvid.so.1",
            c"libnvcuvid.so.1",
        ]);
        // CUDA first (an NVIDIA machine may have a VA-API driver too), then VA-API
        match av::HwDevice::open(av::HwKind::Cuda)
            .or_else(|_| av::HwDevice::open(av::HwKind::Vaapi))
        {
            Ok(d) => {
                eprintln!("tb-avworker: hardware decoder {:?}", d.kind());
                Some(d)
            }
            Err(e) => {
                eprintln!(
                    "tb-avworker: no hardware decoder (CUDA nor VA-API; {}): software decode",
                    e.0
                );
                None
            }
        }
    } else {
        None
    };
    let (Ok(frame), Ok(sw)) = (av::Frame::new(), av::Frame::new()) else {
        return ExitCode::from(EXIT_OOM as u8);
    };

    let probe = cfg.fault == Fault::Probe;
    if probe {
        chan.send(&FromWorker::Probe {
            stage: ProbeStage::BeforeSeccomp,
            results: probe_ops(&tb_sandbox::probe::Op::SAFE),
        });
    }
    let action = match cfg.seccomp {
        SeccompMode::Kill => Action::Kill,
        SeccompMode::Errno | SeccompMode::Off => Action::Errno,
    };
    let profile = if hw.is_some() {
        Profile::ComputeGpu
    } else {
        Profile::Compute
    };
    // with the sandbox's seccomp turned off, this reports it off and installs nothing
    if let Err(e) = sb.lock_down(profile, action) {
        eprintln!("tb-avworker: {e}");
        return ExitCode::from(EXIT_SETUP as u8);
    }
    if probe && cfg.seccomp == SeccompMode::Errno {
        let mut ops = tb_sandbox::probe::Op::SAFE.to_vec();
        ops.push(tb_sandbox::probe::Op::Exec);
        chan.send(&FromWorker::Probe {
            stage: ProbeStage::AfterSeccomp,
            results: probe_ops(&ops),
        });
    }
    chan.send(&FromWorker::Ready);
    if probe && cfg.seccomp == SeccompMode::Kill {
        // outside the allow-list: the filter kills the process here
        let _ = tb_sandbox::probe::attempt(tb_sandbox::probe::Op::Socket);
    }

    let mut w = Worker {
        chan,
        cfg,
        ring,
        ring_len: ring_len as u64,
        streams: Vec::new(),
        deferred: VecDeque::new(),
        frame,
        sw,
        hw,
        stats: WorkerStats::default(),
        packets_seen: 0,
        warned: 0,
    };
    loop {
        let (m, fd) = w.next();
        if fd.is_some() {
            // descriptors come only with Configure and PoolGrant
            exit(EXIT_PROTOCOL);
        }
        match m {
            ToWorker::Packet(p) => w.handle_packet(p),
            ToWorker::Streams(list) => w.handle_streams(list),
            ToWorker::End => w.handle_end(),
            ToWorker::Release { pool, slots } => w.release(pool, &slots),
            ToWorker::Ping { seq } => {
                let stats = w.stats();
                w.send(&FromWorker::Pong { seq, stats });
            }
            ToWorker::Shutdown => return ExitCode::SUCCESS,
            ToWorker::Configure(_) | ToWorker::PoolGrant(_) | ToWorker::PoolDenied { .. } => {
                return ExitCode::from(EXIT_PROTOCOL as u8);
            }
        }
    }
}
