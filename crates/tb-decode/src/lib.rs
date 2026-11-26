//! The decode adapter: inputs demuxed and decoded through libavformat / libavcodec, one thread
//! per input, decoded frames in a short queue.
//!
//! This is the port of the C++ decoder of the CUDA spike (docs/ARCHITECTURE.md, "History"). On the
//! card, decoding is the `cuda` hwaccel (NVDEC driven by libavcodec's own bitstream parsers) or a
//! `*_cuvid` decoder, inside the CUDA context the rest of the process uses ([`DecDevice::wrap`]):
//! frames come out as `AV_PIX_FMT_CUDA`, the luma plane and the interleaved chroma plane as device
//! addresses ([`Surface::Cuda`]), so they are plain device memory for the conversion kernels and
//! the driver's decode work is not time-sliced against them as a second context's would be
//! (measured on the card). Without a device, decoding is libavcodec's software
//! decoders; with [`DecOptions::host_size`] each frame is also converted with swscale in the decode
//! thread to planar 8-bit 4:2:0 of that size ([`Surface::Host`]).
//!
//! An [`Input`] is opened, started at a wall-clock time ([`now`] is the clock), polled with
//! [`Input::take`] ("the newest frame up to this time"), and closed. What the decode thread
//! decides (pacing, the lead, degrading and resynchronizing a late decoder, frame times over
//! loops and stalls) is the sans-io module `tb_timeline::ingest`, which has no `unsafe` and no
//! clock; this crate's thread acts on its answers.
//!
//! # Bindings: `ffmpeg-the-third`, with raw calls where its safe layer ends
//!
//! Evaluated on 2026-11-25 against the FFmpeg of the flake's nixpkgs (9.0.2, the one the spike's
//! C++ was built with):
//!
//! | Option | Finding | Verdict |
//! |---|---|---|
//! | `ffmpeg-the-third` 6.0.0+ffmpeg-9.0 | Released 2026-08-09 (5.0.0 for FFmpeg 8.1 in April 2026, 4.1.0 in March: it follows FFmpeg's releases). Its `-sys` crate runs bindgen on the installed headers, so every struct this crate reads (`AVFrame`, `AVCodecContext`, `AVHWDeviceContext`, `AVHWFramesContext`) has the layout of the library it links, and enums are newtypes over integers (a value the bindings do not name is not undefined behavior). It builds in the Nix sandbox under breakbulk with two settings on `ffmpeg-sys-the-third` (pkg-config and `rustPlatform.bindgenHook` as native inputs, `ffmpeg-headless` as input; `nix/rust.nix`) | Chosen |
//! | a C shim bound by hand | Would work, but is a second build of headers Nix already hands to bindgen, and every getter is a place for the two sides to disagree | Not needed |
//!
//! What the safe layer does here: opening the container and finding the stream, the decoder
//! context and its parameters, packets, sending and receiving, `skip_frame`, flushing, frame
//! properties (size, color space and range, sample aspect ratio, best-effort time stamp, the
//! A53 caption side data). What it has no words for, done with `ffmpeg_sys_the_third` calls in
//! this crate:
//!
//! - the CUDA device context: `av_hwdevice_ctx_alloc`, the fields of `AVCUDADeviceContext`,
//!   `av_hwdevice_ctx_init` (`device.rs`). The struct is not in the bindings, because
//!   `libavutil/hwcontext_cuda.h` needs NVIDIA's headers; it is declared in `device.rs`, and the
//!   flake check `tb-decode/cuda-layout` compiles `shim/cuda_layout.c` against the real header
//!   to hold the declaration to it;
//! - `hw_device_ctx`, `get_format`, `thread_count`, `pkt_timebase` and `hwaccel_flags` of the
//!   decoder context, and decoder private options without the safe layer's panics;
//! - a frame's device addresses and pitches, the software format of its frame pool, its flags;
//! - seeking by byte position, `av_packet_unref`, swscale into a buffer that is not a frame.
//!
//! Three functions of the safe layer panic on a string with a NUL byte or when out of memory
//! (`decoder::find_by_name`, `Dictionary::set`, `format::input`); this crate checks strings
//! first and sets dictionary entries with the raw call.
//!
//! # What runs where
//!
//! The crate builds and its tests run without a GPU (software decoding of clips the tests
//! encode themselves). [`DecDevice::own`] and [`DecDevice::wrap`] return an error on a machine
//! without the NVIDIA driver. The driver's libraries (`libcuda.so.1`, `libnvcuvid.so.1`) are
//! opened by libav by name at run time; making a [`DecDevice`] loads them first, from the
//! loader's search path or else from `/run/opengl-driver/lib` (NixOS), so that no
//! `LD_LIBRARY_PATH` is needed there.
//!
//! `unsafe` in this crate: the libav calls and field accesses listed above, each on objects this
//! crate owns, and three libc calls (two clocks, the thread's nice value).
#![allow(
    unsafe_code,
    reason = "FFI adapter: libavcodec, libavformat, libswscale"
)]

use std::ffi::c_int;
use std::path::PathBuf;

use ffmpeg_the_third as ffmpeg;

mod device;
mod frame;
mod input;
pub use device::DecDevice;
pub use frame::{ColorSpace, Frame, HostBuf, Surface};
pub use input::{Input, InputStats};

/// An error code of libav with its text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} (libav error {code})")]
pub struct AvError {
    /// The `AVERROR` value (negative).
    pub code: i32,
    /// libav's description of it.
    pub message: String,
}

impl AvError {
    pub(crate) fn from_code(code: c_int) -> Self {
        Self::from(ffmpeg::Error::from(code))
    }
}

impl From<ffmpeg::Error> for AvError {
    fn from(e: ffmpeg::Error) -> Self {
        Self {
            code: e.into(),
            message: e.to_string(),
        }
    }
}

/// Why a device context or an input could not be made.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// [`DecDevice::wrap`] was given a null context.
    #[error("no CUDA context to wrap (null)")]
    NullContext,
    /// libav could not make its CUDA device context: no driver, no such device, or a context
    /// it cannot use.
    #[error("libav CUDA device context: {what}: {source}")]
    Device {
        /// The libav call that failed.
        what: &'static str,
        /// libav's error.
        #[source]
        source: AvError,
    },
    /// libav returned no memory for an object.
    #[error("libav could not allocate {0}")]
    Alloc(&'static str),
    /// An option cannot be used as given.
    #[error("option: {0}")]
    BadOption(String),
    /// The file could not be opened or its streams could not be read.
    #[error("open {path}: {source}")]
    Open {
        /// The input.
        path: PathBuf,
        /// libav's error.
        #[source]
        source: AvError,
    },
    /// The file has no video stream.
    #[error("{0}: no video stream")]
    NoVideo(PathBuf),
    /// libav has no decoder of that name or for that codec.
    #[error("no decoder {0}")]
    NoDecoder(String),
    /// The decoder could not be set up or opened.
    #[error("decoder {codec}: {what}: {source}")]
    Decoder {
        /// The decoder's name.
        codec: String,
        /// The libav call that failed.
        what: &'static str,
        /// libav's error.
        #[source]
        source: AvError,
    },
}

/// Options of one input (the C++ `DecOptions`, with its defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct DecOptions {
    /// `None`: the codec's native decoder (with the `cuda` hwaccel on a device); else a decoder
    /// by name, e.g. `h264_cuvid`.
    pub decoder: Option<String>,
    /// Decoder private options (`deint=adaptive`, ...).
    pub opts: Vec<(String, String)>,
    /// Software decode: frames are host memory. Also in effect when the input is opened without
    /// a device.
    pub sw: bool,
    /// Threads of a software decoder.
    pub sw_threads: u32,
    /// Hand out the decoder's own surfaces instead of copies in pool frames
    /// (`AV_HWACCEL_FLAG_UNSAFE_OUTPUT`).
    pub unsafe_output: bool,
    /// Deliver packets at the stream's own rate, like a live source.
    pub paced: bool,
    /// At the end of the file, continue at its start (frame times continue).
    pub looping: bool,
    /// Decoded frames kept; a full queue drops the oldest (counted) or, with `block`, holds the
    /// decoder back.
    pub qmax: usize,
    /// Paced: packets are sent this many seconds before their decode time.
    pub lead: f64,
    /// Frame times from the frame's index and the stream's rate. Container time stamps may be
    /// rounded to milliseconds (Matroska), which makes equal-rate sources jitter around a tick.
    pub cfr: bool,
    /// Paced: seconds behind at which the decoder stops decoding pictures nothing refers to
    /// (0: never).
    pub degrade: f64,
    /// Paced: seconds the decode thread may be behind its packets before it skips to the next
    /// key frame (0: never).
    pub resync: f64,
    /// Offline runs: a full queue holds the decoder back instead of dropping, and
    /// [`Input::take`] waits until its answer no longer depends on thread timing.
    pub block: bool,
    /// The host path: software-decoded frames are converted with swscale (bicubic) to planar
    /// 8-bit 4:2:0 of this size (width, height; both even) in the decode thread. Frames decoded
    /// on a device are not converted: they stay device memory.
    pub host_size: Option<(u32, u32)>,
}

impl Default for DecOptions {
    fn default() -> Self {
        Self {
            decoder: None,
            opts: Vec::new(),
            sw: false,
            sw_threads: 1,
            unsafe_output: false,
            paced: false,
            looping: true,
            qmax: 4,
            lead: 0.0,
            cfr: false,
            degrade: 0.0,
            resync: 0.0,
            block: false,
            host_size: None,
        }
    }
}

/// Seconds on `CLOCK_MONOTONIC`: the clock of [`Input::start`], [`Frame::done`] and the
/// lateness record (the C++ `dec_now`).
pub fn now() -> f64 {
    clock(libc::CLOCK_MONOTONIC)
}

/// Seconds of a POSIX clock (0 if the clock cannot be read, which these two cannot fail at).
pub(crate) fn clock(id: libc::clockid_t) -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid timespec for the call to write.
    let r = unsafe { libc::clock_gettime(id, &raw mut ts) };
    if r != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

/// Limits libav's own log output (stderr) to errors, as the C++ tools do. Process-wide.
pub fn log_errors_only() {
    ffmpeg::log::set_level(ffmpeg::log::Level::Error);
}
