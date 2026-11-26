//! One input: demux and decode in a thread of its own, decoded frames in a short queue
//! (the C++ `Input`).

use std::collections::VecDeque;
use std::ffi::{CString, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::codec::packet::Mut as _;
use ffmpeg_the_third::ffi;

use crate::frame::{Frame, HostPool, Layout, Surface, layout, pix_fmt_name};
use tb_timeline::ingest::{FrameTimes, PacketGate, newest_due, push_bounded, take_ready};

use crate::{AvError, DecDevice, DecOptions, DecodeError, clock, now};

/// Most lateness values kept between two calls of [`Input::take_lateness`].
const LATE_MAX: usize = 100_000;

/// Most caption entries of frames dropped from a full queue kept between two calls of
/// [`Input::take_with_captions`].
const CC_DROPPED_MAX: usize = 256;

/// Locks a mutex whose data stays consistent when a holder panicked (counters, queues).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counters and findings of one input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputStats {
    /// Frames the decoder returned.
    pub decoded: u64,
    /// Frames dropped from a full queue.
    pub dropped: u64,
    /// Frames dropped from a full queue whose captions were lost with them: nobody took
    /// frames for so long that more than 256 dropped frames with captions were waiting for
    /// [`Input::take_with_captions`].
    pub cc_lost: u64,
    /// Packets sent to the decoder.
    pub packets: u64,
    /// Bytes of those packets.
    pub bytes: u64,
    /// Times the input was rewound.
    pub loops: u64,
    /// Packets the decoder refused.
    pub errors: u64,
    /// Packets not sent while skipping to a key frame (resynchronization).
    pub skipped: u64,
    /// Resynchronizations: a group of pictures given up because the decoder was behind.
    pub resyncs: u64,
    /// Times the decoder was told to skip non-reference pictures.
    pub degrades: u64,
    /// CPU time of the decode thread, microseconds (updated every 16 packets and at the end).
    pub cpu_us: u64,
    /// The first frame was device memory (`AV_PIX_FMT_CUDA`). False before the first frame,
    /// for software decoding, and for a hardware open that libav turned into software decoding.
    pub on_device: bool,
    /// Pixel format of the first frame (for a device frame, of its pool), with a note when an
    /// input opened on a device is not decoded there. Empty before the first frame.
    pub pixfmt: String,
    /// A frame was flagged interlaced.
    pub interlaced_seen: bool,
    /// The decode thread has finished: end of a file that does not loop, or a failure.
    pub ended: bool,
    /// The decode thread gave up: the input could not be read again after a rewind.
    pub failed: bool,
}

#[derive(Default)]
struct Queue {
    /// Decoded frames with their times, oldest first (the form of `tb_timeline::ingest`).
    q: VecDeque<(f64, Frame)>,
    /// Captions of frames dropped from the full queue, oldest first: `(frame idx, cc bytes)`.
    /// They are older than everything in `q`.
    cc_dropped: VecDeque<(i64, Vec<u8>)>,
    ended: bool,
}

/// What the decode thread and the consumer share.
struct Shared {
    qmax: usize,
    block: bool,
    paced: bool,
    hw: bool,
    queue: Mutex<Queue>,
    cv: Condvar,
    stop: AtomicBool,
    stall: AtomicBool,
    started: AtomicBool,
    failed: AtomicBool,
    /// `f64` bits: lateness is recorded from this stream time on.
    late_from: AtomicU64,
    late: Mutex<Vec<f32>>,
    decoded: AtomicU64,
    dropped: AtomicU64,
    cc_lost: AtomicU64,
    packets: AtomicU64,
    bytes: AtomicU64,
    loops: AtomicU64,
    errors: AtomicU64,
    skipped: AtomicU64,
    resyncs: AtomicU64,
    degrades: AtomicU64,
    cpu_us: AtomicU64,
    on_device: AtomicBool,
    interlaced_seen: AtomicBool,
    pixfmt: OnceLock<String>,
    pool: Arc<HostPool>,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.stop.load(Relaxed)
    }
}

/// One input file being decoded.
pub struct Input {
    shared: Arc<Shared>,
    /// The demuxer and decoder until [`Input::start`] hands them to the thread.
    worker: Mutex<Option<Worker>>,
    thread: Option<JoinHandle<()>>,
    fps: f64,
    size: (u32, u32),
    codec: String,
}

/// `get_format` of a decoder on a device: the CUDA format if the decoder offers it, else none
/// (libav then falls back to software decoding or fails, depending on the decoder).
unsafe extern "C" fn pick_cuda(
    _: *mut ffi::AVCodecContext,
    mut f: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    // SAFETY: libavcodec passes a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        while *f != ffi::AVPixelFormat::NONE {
            if *f == ffi::AVPixelFormat::CUDA {
                return *f;
            }
            f = f.add(1);
        }
    }
    ffi::AVPixelFormat::NONE
}

fn c_string(what: &str, s: &str) -> Result<CString, DecodeError> {
    CString::new(s).map_err(|_| DecodeError::BadOption(format!("{what} contains a NUL byte")))
}

impl Input {
    /// Opens `path`, finds its video stream and opens a decoder for it. With `dev` the decoder
    /// runs on that device (unless [`DecOptions::sw`]); without, in software.
    pub fn open(
        path: &Path,
        opts: &DecOptions,
        dev: Option<&DecDevice>,
    ) -> Result<Self, DecodeError> {
        let dev = if opts.sw { None } else { dev };
        if opts.qmax == 0 {
            return Err(DecodeError::BadOption("qmax must be at least 1".into()));
        }
        if let Some((w, h)) = opts.host_size
            && (w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || w > 1 << 15 || h > 1 << 15)
        {
            return Err(DecodeError::BadOption(format!(
                "host_size {w}x{h}: both must be even and in 2..=32768"
            )));
        }
        if path.as_os_str().as_bytes().contains(&0) {
            return Err(DecodeError::BadOption("path contains a NUL byte".into()));
        }
        let fmt = ffmpeg::format::input(path).map_err(|e| DecodeError::Open {
            path: path.to_owned(),
            source: e.into(),
        })?;
        let st = fmt
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| DecodeError::NoVideo(path.to_owned()))?;
        let vs = st.index();
        let par = st.parameters();
        let id = par.id();
        let size = (par.width(), par.height());
        let codec = match &opts.decoder {
            Some(name) => {
                c_string("decoder name", name)?;
                ffmpeg::decoder::find_by_name(name)
            }
            // AV1: the default decoder is libdav1d, which has no hwaccel; the native one
            // drives NVDEC
            None if dev.is_some() && id == ffmpeg::codec::Id::AV1 => {
                ffmpeg::decoder::find_by_name("av1")
            }
            None => ffmpeg::decoder::find(id),
        }
        .ok_or_else(|| {
            DecodeError::NoDecoder(
                opts.decoder
                    .clone()
                    .unwrap_or_else(|| format!("for {id:?}")),
            )
        })?;
        let name = codec.name().to_owned();
        let fail = |what: &'static str, source: AvError| DecodeError::Decoder {
            codec: name.clone(),
            what,
            source,
        };

        let mut ctx = ffmpeg::codec::Context::new_with_codec(codec);
        // SAFETY: only compared with null.
        if unsafe { ctx.as_ptr() }.is_null() {
            return Err(DecodeError::Alloc("a decoder context"));
        }
        ctx.set_parameters(par)
            .map_err(|e| fail("avcodec_parameters_to_context", e.into()))?;
        let hw_ref = match dev {
            Some(d) => {
                let r = d.new_ref();
                if r.is_null() {
                    return Err(DecodeError::Alloc("a device context reference"));
                }
                r
            }
            None => ptr::null_mut(),
        };
        // SAFETY: `ctx` owns a context that was allocated above and is not open yet; these are
        // fields the user sets before `avcodec_open2`. The context takes over `hw_ref` (it
        // releases `hw_device_ctx` when it is freed).
        unsafe {
            let c = ctx.as_mut_ptr();
            (*c).pkt_timebase = st.time_base().into();
            if dev.is_some() {
                (*c).hw_device_ctx = hw_ref;
                (*c).get_format = Some(pick_cuda);
                (*c).thread_count = 1;
                if opts.unsafe_output {
                    (*c).hwaccel_flags |= ffi::AV_HWACCEL_FLAG_UNSAFE_OUTPUT;
                }
            } else {
                (*c).thread_count = c_int::try_from(opts.sw_threads).unwrap_or(c_int::MAX);
            }
        }

        let mut dict: *mut ffi::AVDictionary = ptr::null_mut();
        let mut set_all = || -> Result<(), DecodeError> {
            for (k, v) in &opts.opts {
                let (ck, cv) = (c_string("option name", k)?, c_string("option value", v)?);
                // SAFETY: `dict` is null or a dictionary this function owns; the strings are
                // NUL-terminated and copied by the call.
                let e = unsafe { ffi::av_dict_set(&raw mut dict, ck.as_ptr(), cv.as_ptr(), 0) };
                if e < 0 {
                    return Err(DecodeError::Alloc("a dictionary entry"));
                }
            }
            Ok(())
        };
        let set = set_all();
        // SAFETY: `dict` is null or a dictionary nothing else refers to; the wrapper frees it.
        let dict = unsafe { ffmpeg::Dictionary::from_raw(dict) };
        set?;
        let (dec, left) = {
            let mut dict = dict;
            let r = ctx.decoder().open_as_with(codec, dict.as_mut());
            (r.map_err(|e| fail("avcodec_open2", e.into()))?, dict)
        };
        for (k, v) in left.iter() {
            tracing::warn!(codec = %name, option = k, value = v, "decoder does not know the option");
        }

        let rate = match st.avg_frame_rate() {
            r if r.numerator() != 0 => r,
            _ => st.rate(),
        };
        let mut fps = if rate.denominator() != 0 {
            f64::from(rate.numerator()) / f64::from(rate.denominator())
        } else {
            30.0
        };
        // a cuvid decoder that deinterlaces returns one frame per field unless told to drop
        // the second
        let deint = opts.opts.iter().any(|(k, v)| k == "deint" && v != "weave");
        let drop_second = opts
            .opts
            .iter()
            .any(|(k, v)| k == "drop_second_field" && v != "0");
        if deint && !drop_second {
            fps *= 2.0;
        }
        if !(fps.is_finite() && fps > 0.0) {
            fps = 30.0;
        }
        let tb = f64::from(st.time_base());
        let start_time = st.start_time();
        tracing::debug!(
            path = %path.display(), codec = %name, width = size.0, height = size.1, fps,
            device = dev.is_some(), "input open"
        );

        let shared = Arc::new(Shared {
            qmax: opts.qmax,
            block: opts.block,
            paced: opts.paced,
            hw: dev.is_some(),
            queue: Mutex::default(),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            stall: AtomicBool::new(false),
            started: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            late_from: AtomicU64::new(0f64.to_bits()),
            late: Mutex::default(),
            decoded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            cc_lost: AtomicU64::new(0),
            packets: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            loops: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            resyncs: AtomicU64::new(0),
            degrades: AtomicU64::new(0),
            cpu_us: AtomicU64::new(0),
            on_device: AtomicBool::new(false),
            interlaced_seen: AtomicBool::new(false),
            pixfmt: OnceLock::new(),
            // as many as can be queued, plus the ones a consumer holds (the C++ ring)
            pool: HostPool::new(opts.qmax + 6),
        });
        let worker = Worker {
            path: path.display().to_string(),
            fmt,
            dec,
            vs,
            start_time,
            fps,
            tb,
            o: opts.clone(),
            scratch: ffmpeg::frame::Video::empty(),
            sws: None,
            warned_layout: false,
        };
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
            thread: None,
            fps,
            size,
            codec: name,
        })
    }

    /// Frames per second of the stream (doubled for a cuvid decoder that returns fields).
    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// Width and height of the stream as the container states them.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Name of the decoder in use.
    pub fn codec(&self) -> &str {
        &self.codec
    }

    /// Starts the decode thread (at nice 10: decode threads yield to the tick thread). `t0` is
    /// the time on the clock of [`now`](crate::now) at which stream time 0 is due; it matters
    /// to a paced input and to the lateness record. A second call does nothing.
    pub fn start(&mut self, t0: f64) {
        let Some(worker) = lock(&self.worker).take() else {
            return;
        };
        self.shared.started.store(true, Relaxed);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("tb-decode".into())
            .spawn(move || worker.run(&shared, t0));
        match spawned {
            Ok(h) => self.thread = Some(h),
            Err(e) => {
                tracing::error!(error = %e, "decode thread could not be started");
                self.shared.failed.store(true, Relaxed);
                lock(&self.shared.queue).ended = true;
                self.shared.cv.notify_all();
            }
        }
    }

    /// The newest queued frame with a time up to `limit`, and the number of older ones that
    /// were released for it. `None` when there is none.
    ///
    /// With [`DecOptions::block`] the call first waits until the queue shows a frame beyond
    /// the limit, is full, or the input has ended, so that the answer does not depend on
    /// thread timing; it then waits again if that emptied the queue. (Before [`Input::start`]
    /// there is nothing to wait for and the call returns at once.)
    pub fn take(&self, limit: f64) -> (Option<Frame>, u32) {
        // the captions of the frames released go with them
        self.take_with_captions(limit, &mut Vec::new())
    }

    /// [`Input::take`], and the captions of the frames that are not returned: appends to
    /// `skipped_cc`, in decode order, `(frame idx, cc bytes)` of every frame with captions
    /// that was dropped from the full queue since the last call or is released by this one.
    /// All of them are older than the frame returned, so pushing `skipped_cc` and then the
    /// returned frame's [`Frame::cc`] into a caption carrier gives it every decoded frame's
    /// captions once and in order. Frames without captions add nothing.
    ///
    /// At most 256 dropped frames' captions are kept between calls: beyond that the oldest
    /// are discarded and counted in [`InputStats::cc_lost`].
    pub fn take_with_captions(
        &self,
        limit: f64,
        skipped_cc: &mut Vec<(i64, Vec<u8>)>,
    ) -> (Option<Frame>, u32) {
        let sh = &*self.shared;
        let block = sh.block && sh.started.load(Relaxed);
        let (mut got, mut skipped): (Option<Frame>, u32) = (None, 0);
        let mut g = lock(&sh.queue);
        loop {
            if block {
                g = sh
                    .cv
                    .wait_while(g, |s| {
                        !(take_ready(&s.q, limit, sh.qmax, s.ended) || sh.stopped())
                    })
                    .unwrap_or_else(PoisonError::into_inner);
            }
            let due = g.q.iter().take_while(|e| e.0 <= limit).count();
            // Decode order: the frame of an earlier round that this round replaces, the frames
            // dropped from the full queue, the queued frames this round releases.
            if due > 0
                && let Some(f) = got.as_mut()
                && !f.cc.is_empty()
            {
                skipped_cc.push((f.idx, std::mem::take(&mut f.cc)));
            }
            if due > 0 || got.is_none() {
                // (otherwise they are newer than the frame returned: the next call's)
                skipped_cc.extend(g.cc_dropped.drain(..));
            }
            for (_, f) in g.q.iter_mut().take(due.saturating_sub(1)) {
                if !f.cc.is_empty() {
                    skipped_cc.push((f.idx, std::mem::take(&mut f.cc)));
                }
            }
            let (newest, released) = newest_due(&mut g.q, limit);
            if let Some((_, f)) = newest {
                // a frame returned by an earlier round is released for this one
                skipped += u32::from(got.is_some());
                got = Some(f);
            }
            skipped += u32::try_from(released).unwrap_or(u32::MAX);
            sh.cv.notify_all();
            if !block || !g.q.is_empty() || g.ended || sh.stopped() {
                break;
            }
        }
        (got, skipped)
    }

    /// Test aid: while set, the source delivers nothing; afterwards it continues, later by the
    /// time it stood still.
    pub fn set_stall(&self, on: bool) {
        self.shared.stall.store(on, Relaxed);
    }

    /// Lateness is recorded for frames from this stream time on (start-up left out).
    pub fn set_late_from(&self, t: f64) {
        self.shared.late_from.store(t.to_bits(), Relaxed);
    }

    /// The counters and findings so far.
    pub fn stats(&self) -> InputStats {
        let sh = &*self.shared;
        InputStats {
            decoded: sh.decoded.load(Relaxed),
            dropped: sh.dropped.load(Relaxed),
            cc_lost: sh.cc_lost.load(Relaxed),
            packets: sh.packets.load(Relaxed),
            bytes: sh.bytes.load(Relaxed),
            loops: sh.loops.load(Relaxed),
            errors: sh.errors.load(Relaxed),
            skipped: sh.skipped.load(Relaxed),
            resyncs: sh.resyncs.load(Relaxed),
            degrades: sh.degrades.load(Relaxed),
            cpu_us: sh.cpu_us.load(Relaxed),
            on_device: sh.on_device.load(Relaxed),
            pixfmt: sh.pixfmt.get().cloned().unwrap_or_default(),
            interlaced_seen: sh.interlaced_seen.load(Relaxed),
            ended: lock(&sh.queue).ended,
            failed: sh.failed.load(Relaxed),
        }
    }

    /// Whether the decode thread has finished (end of a file that does not loop, or a
    /// failure). Frames may still be queued.
    pub fn ended(&self) -> bool {
        lock(&self.shared.queue).ended
    }

    /// Paced inputs: for each frame since the last call (from the time of
    /// [`Input::set_late_from`] on), the seconds between its nominal time (`t0 + t`) and its
    /// arrival from the decoder. At most 100000 values are kept between calls.
    pub fn take_lateness(&self) -> Vec<f32> {
        std::mem::take(&mut *lock(&self.shared.late))
    }

    /// Stops the decode thread, waits for it, and releases the queued frames, the decoder and
    /// the file. Dropping the input does the same. Frames already taken stay valid.
    pub fn close(self) {}

    fn shut_down(&mut self) {
        // under the lock, so that a thread about to wait cannot miss it
        {
            let _g = lock(&self.shared.queue);
            self.shared.stop.store(true, Relaxed);
        }
        self.shared.cv.notify_all();
        if let Some(h) = self.thread.take()
            && h.join().is_err()
        {
            tracing::error!("decode thread panicked");
        }
        let drained = std::mem::take(&mut lock(&self.shared.queue).q);
        drop(drained);
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// A swscale context for one source geometry.
struct Scaler {
    ctx: *mut ffi::SwsContext,
    src: (c_int, c_int, c_int),
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: `ctx` came from `sws_getContext`, is not null and is freed once.
        unsafe { ffi::sws_freeContext(self.ctx) };
    }
}

/// The decode thread's own state: the demuxer, the decoder, and what `run` needs of the options.
struct Worker {
    path: String,
    fmt: ffmpeg::format::context::Input,
    dec: ffmpeg::decoder::Opened,
    vs: usize,
    start_time: i64,
    fps: f64,
    tb: f64,
    o: DecOptions,
    /// The frame `receive_frame` writes to; moved into the queue when it is device memory.
    scratch: ffmpeg::frame::Video,
    sws: Option<Scaler>,
    warned_layout: bool,
}

// SAFETY: the worker is used by one thread at a time (built by `open`, then moved into the
// decode thread). Its libav objects have no thread affinity; the crate's wrappers of them are
// `Send`, and the swscale context is the only raw pointer here.
unsafe impl Send for Worker {}

impl Worker {
    fn run(mut self, sh: &Shared, t0: f64) {
        // decode threads yield to the tick thread: on a busy host a late tick costs every
        // channel a picture, a late frame costs one channel a repeat
        // SAFETY: plain system calls on the calling thread, no memory involved.
        unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::setpriority(libc::PRIO_PROCESS, tid, 10);
        }
        let mut pkt = ffmpeg::Packet::empty();
        let mut times = FrameTimes::new(self.fps, self.tb, self.o.cfr);
        let mut gate = PacketGate::new(self.o.degrade, self.o.resync);
        // packets of the video stream since the last rewind
        let mut lap_packets = 0u64;
        while !sh.stopped() {
            if sh.stall.load(Relaxed) {
                let s0 = now();
                while sh.stall.load(Relaxed) && !sh.stopped() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                times.add_shift(now() - s0);
            }
            if pkt.read(&mut self.fmt).is_err() {
                // end of the file (or an error, which ends it the same way)
                let _ = self.dec.send_eof();
                self.receive(sh, &mut times, t0);
                if !self.o.looping {
                    break;
                }
                if lap_packets == 0 {
                    // the C++ decoder would rewind for ever
                    tracing::error!(input = %self.path, "no video packet after a rewind: giving up");
                    sh.failed.store(true, Relaxed);
                    break;
                }
                self.dec.flush();
                self.rewind();
                times.restart();
                lap_packets = 0;
                sh.loops.fetch_add(1, Relaxed);
                continue;
            }
            if pkt.stream() != self.vs {
                unref(&mut pkt);
                continue;
            }
            lap_packets += 1;
            times.packet_pts(pkt.pts());
            let due = times.due(pkt.dts().or(pkt.pts()), t0, self.o.lead);
            let behind = if self.o.paced {
                due.map(|d| now() - d)
            } else {
                None
            };
            let act = gate.packet(behind, pkt.is_key());
            sh.degrades.store(gate.degrades, Relaxed);
            sh.resyncs.store(gate.resyncs, Relaxed);
            sh.skipped.store(gate.skipped, Relaxed);
            // First measure when the decoder cannot keep up: stop decoding the pictures
            // nothing refers to (the B pictures of most streams).
            if let Some(on) = act.discard_nonref {
                self.dec.skip_frame(if on {
                    ffmpeg::Discard::NonReference
                } else {
                    ffmpeg::Discard::Default
                });
            }
            if act.sleep > 0.0 {
                sleep(sh, act.sleep);
            }
            // The decoder is shared and nothing is ever dropped inside it: once this thread
            // is too far behind its packets it gives up the rest of the group of pictures and
            // continues at the next key frame, so the input is late for a moment, not for good.
            if act.drop {
                unref(&mut pkt);
                continue;
            }
            if act.flush {
                self.dec.flush();
            }
            let n = sh.packets.fetch_add(1, Relaxed) + 1;
            sh.bytes.fetch_add(pkt.size() as u64, Relaxed);
            if n & 15 == 0 {
                sh.cpu_us.store(thread_cpu_us(), Relaxed);
            }
            if let Err(e) = self.dec.send_packet(&pkt)
                && c_int::from(e) != ffi::AVERROR(libc::EAGAIN)
            {
                sh.errors.fetch_add(1, Relaxed);
            }
            unref(&mut pkt);
            self.receive(sh, &mut times, t0);
        }
        sh.cpu_us.store(thread_cpu_us(), Relaxed);
        // the decoder and the file are released here, before the consumer hears of the end
        drop(self);
        lock(&sh.queue).ended = true;
        sh.cv.notify_all();
    }

    /// Back to the first packet: by byte position where the format allows it (a transport
    /// stream sought by time may resume after its first key frame), else by time.
    fn rewind(&mut self) {
        let ts = self.fmt.format().name().contains("mpegts");
        // SAFETY: `self.fmt` is an open input context that only this thread uses.
        let by_byte = ts
            && unsafe { ffi::av_seek_frame(self.fmt.as_mut_ptr(), -1, 0, ffi::AVSEEK_FLAG_BYTE) }
                >= 0;
        if by_byte {
            return;
        }
        let start = if self.start_time == ffi::AV_NOPTS_VALUE {
            0
        } else {
            self.start_time
        };
        let vs = c_int::try_from(self.vs).unwrap_or(-1);
        // SAFETY: as above; `vs` is the index of a stream of this context.
        let e = unsafe {
            ffi::av_seek_frame(self.fmt.as_mut_ptr(), vs, start, ffi::AVSEEK_FLAG_BACKWARD)
        };
        if e < 0 {
            tracing::warn!(input = %self.path, error = %AvError::from_code(e), "rewind failed");
        }
    }

    /// Queues every frame the decoder has ready.
    fn receive(&mut self, sh: &Shared, times: &mut FrameTimes, t0: f64) {
        while self.dec.receive_frame(&mut self.scratch).is_ok() {
            let t = times.frame(self.scratch.timestamp());
            self.push(sh, t, t0);
        }
    }

    /// Queues the frame in `self.scratch` with presentation time `t` (the C++ `Input::push`).
    fn push(&mut self, sh: &Shared, t: f64, t0: f64) {
        let done = now();
        let idx = sh.decoded.load(Relaxed);
        // SAFETY: `scratch` owns an AVFrame that `receive_frame` just filled; the reference
        // is not used after `scratch` is moved or written to again.
        let Some(raw) = (unsafe { self.scratch.as_ref() }) else {
            return;
        };
        if raw.flags & ffi::AV_FRAME_FLAG_INTERLACED != 0 {
            sh.interlaced_seen.store(true, Relaxed);
        }
        let (pix, lay) = layout(raw);
        let on_device = lay != Layout::Host;
        sh.pixfmt.get_or_init(|| {
            sh.on_device.store(on_device, Relaxed);
            let mut s = pix_fmt_name(pix);
            if sh.hw && !on_device {
                s += " (NOT on the device: software fallback)";
                tracing::warn!(input = %self.path, pixfmt = %s, "decoder opened on the device decodes in software");
            }
            s
        });
        let surface = match lay {
            Layout::Cuda { bps } => Surface::Cuda {
                y: raw.data[0] as u64,
                uv: raw.data[1] as u64,
                pitch_y: u32::try_from(raw.linesize[0]).unwrap_or(0),
                pitch_uv: u32::try_from(raw.linesize[1]).unwrap_or(0),
                bps,
            },
            Layout::CudaOther => {
                if !self.warned_layout {
                    self.warned_layout = true;
                    tracing::warn!(
                        input = %self.path, pixfmt = %pix_fmt_name(pix),
                        "device frames in a layout the pipeline does not take"
                    );
                }
                Surface::None
            }
            // host path: convert here, in the decode thread
            Layout::Host => match self.o.host_size {
                Some(size) => convert(&mut self.sws, &self.path, raw, size, &sh.pool),
                None => Surface::None,
            },
        };
        let mut frame = Frame::new(&self.scratch, raw, idx as i64, t, done, surface);
        if on_device {
            // the queue owns the device frame; the next one is received into a new AVFrame
            frame.keep(std::mem::replace(
                &mut self.scratch,
                ffmpeg::frame::Video::empty(),
            ));
        }
        if sh.paced && t >= f64::from_bits(sh.late_from.load(Relaxed)) {
            let mut late = lock(&sh.late);
            if late.len() < LATE_MAX {
                late.push((done - (t0 + t)) as f32);
            }
        }
        sh.decoded.fetch_add(1, Relaxed);
        let mut g = lock(&sh.queue);
        if sh.block {
            g = sh
                .cv
                .wait_while(g, |s| s.q.len() >= sh.qmax && !sh.stopped())
                .unwrap_or_else(PoisonError::into_inner);
        }
        // The frames a full queue loses keep their captions for the consumer.
        let over = (g.q.len() + 1).saturating_sub(sh.qmax);
        let Queue { q, cc_dropped, .. } = &mut *g;
        for (_, f) in q.iter_mut().take(over) {
            if f.cc.is_empty() {
                continue;
            }
            if cc_dropped.len() >= CC_DROPPED_MAX {
                cc_dropped.pop_front();
                sh.cc_lost.fetch_add(1, Relaxed);
            }
            cc_dropped.push_back((f.idx, std::mem::take(&mut f.cc)));
        }
        let dropped = push_bounded(q, sh.qmax, t, frame);
        sh.dropped.fetch_add(dropped as u64, Relaxed);
        drop(g);
        sh.cv.notify_all();
    }
}

/// Releases a packet's data (`av_packet_unref`); the packet can be read into again.
fn unref(pkt: &mut ffmpeg::Packet) {
    // SAFETY: the packet is a valid AVPacket owned by the wrapper; unref leaves it blank.
    unsafe { ffi::av_packet_unref(pkt.as_mut_ptr()) };
}

/// Sleeps `seconds`, looking at the stop flag every 50 ms.
fn sleep(sh: &Shared, seconds: f64) {
    let end = now() + seconds;
    while !sh.stopped() {
        let left = end - now();
        if left <= 0.0 {
            break;
        }
        std::thread::sleep(Duration::from_secs_f64(left.min(0.05)));
    }
}

/// CPU time of the calling thread in microseconds.
fn thread_cpu_us() -> u64 {
    (clock(libc::CLOCK_THREAD_CPUTIME_ID) * 1e6) as u64
}

/// The host path: `raw` (host memory) scaled to planar 8-bit 4:2:0 of `size`.
fn convert(
    slot: &mut Option<Scaler>,
    path: &str,
    raw: &ffi::AVFrame,
    (w, h): (u32, u32),
    pool: &Arc<HostPool>,
) -> Surface {
    let src = (raw.width, raw.height, raw.format);
    if slot.as_ref().is_none_or(|s| s.src != src) {
        *slot = None;
        // SAFETY: plain arguments (sizes were checked in `open`: at most 32768); no
        // filters, default parameters. Returns null when it cannot convert.
        let ctx = unsafe {
            ffi::sws_getContext(
                raw.width,
                raw.height,
                ffi::AVPixelFormat(raw.format),
                w as c_int,
                h as c_int,
                ffi::AVPixelFormat::YUV420P,
                ffi::SwsFlags::BICUBIC.0 as c_int,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ctx.is_null() {
            tracing::error!(
                input = %path, pixfmt = %pix_fmt_name(ffi::AVPixelFormat(raw.format)),
                "swscale cannot convert the input's frames"
            );
            return Surface::None;
        }
        *slot = Some(Scaler { ctx, src });
    }
    let Some(sws) = slot else {
        return Surface::None;
    };
    let (w, h) = (w as usize, h as usize);
    let mut buf = pool.get(w * h * 3 / 2);
    let (y, c) = buf.bytes_mut().split_at_mut(w * h);
    let (u, v) = c.split_at_mut(w * h / 4);
    let dst = [
        y.as_mut_ptr(),
        u.as_mut_ptr(),
        v.as_mut_ptr(),
        ptr::null_mut(),
    ];
    let pitch = [w as c_int, (w / 2) as c_int, (w / 2) as c_int, 0];
    // SAFETY: the context was made for this source geometry and this destination. The
    // source planes are the frame's own (host memory, as its format says) with its
    // pitches; the destination planes are disjoint parts of `buf` of w*h, (w/2)*(h/2) and
    // (w/2)*(h/2) bytes, which is what h rows of w and h/2 rows of w/2 bytes need (w and h
    // are even).
    let rows = unsafe {
        ffi::sws_scale(
            sws.ctx,
            raw.data.as_ptr().cast(),
            raw.linesize.as_ptr(),
            0,
            raw.height,
            dst.as_ptr(),
            pitch.as_ptr(),
        )
    };
    if rows < 0 {
        return Surface::None;
    }
    Surface::Host(buf)
}
