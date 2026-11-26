//! Decoded frames as the consumer sees them (the C++ `QFrame`).

use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::ffi;

use crate::input::lock;

/// The matrix a frame's colors are coded with, as far as the conversion distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// ITU-R BT.709.
    Bt709,
    /// SMPTE 170M (NTSC; the same matrix as BT.601).
    Smpte170m,
    /// ITU-R BT.470 BG (PAL; the same matrix as BT.601).
    Bt470bg,
    /// The stream does not say.
    Unspecified,
    /// Anything else (BT.2020, RGB, ...).
    Other,
}

impl ColorSpace {
    fn from_av(c: ffi::AVColorSpace) -> Self {
        match c {
            ffi::AVColorSpace::BT709 => Self::Bt709,
            ffi::AVColorSpace::SMPTE170M => Self::Smpte170m,
            ffi::AVColorSpace::BT470BG => Self::Bt470bg,
            ffi::AVColorSpace::UNSPECIFIED => Self::Unspecified,
            _ => Self::Other,
        }
    }
}

/// Where a frame's pixels are.
#[derive(Debug)]
pub enum Surface {
    /// Device memory of the decoder's CUDA context: the luma plane at address `y` and the
    /// interleaved chroma plane (4:2:0, U then V) at `uv`, with their pitches in bytes. NV12
    /// has `bps` 1; P010 / P016 have `bps` 2 (little-endian samples, the value in the high
    /// bits). The memory is valid until the [`Frame`] is dropped.
    Cuda {
        /// Device address of the luma plane.
        y: u64,
        /// Device address of the chroma plane.
        uv: u64,
        /// Bytes per luma row.
        pitch_y: u32,
        /// Bytes per chroma row.
        pitch_uv: u32,
        /// Bytes per sample.
        bps: u32,
    },
    /// The host path: planar 8-bit 4:2:0 of [`DecOptions::host_size`](crate::DecOptions), the
    /// Y plane, then U, then V, rows without padding.
    Host(HostBuf),
    /// Nothing the pipeline can use: a software-decoded frame nobody converted (no
    /// `host_size`), or a device frame in a layout other than NV12 / P010 / P016. Counted in
    /// the statistics like any other.
    None,
}

/// A buffer of the host path. Returns to its input's pool when dropped, so a running input
/// allocates none after its first few frames.
pub struct HostBuf {
    data: Vec<u8>,
    pool: Arc<HostPool>,
}

impl Deref for HostBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for HostBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostBuf")
            .field("len", &self.data.len())
            .finish()
    }
}

impl Drop for HostBuf {
    fn drop(&mut self) {
        let mut free = lock(&self.pool.free);
        if free.len() < self.pool.keep {
            free.push(std::mem::take(&mut self.data));
        }
    }
}

/// The buffers of one input's host path that are not in use.
#[derive(Debug)]
pub(crate) struct HostPool {
    free: Mutex<Vec<Vec<u8>>>,
    keep: usize,
}

impl HostPool {
    /// A pool that keeps up to `keep` free buffers.
    pub(crate) fn new(keep: usize) -> Arc<Self> {
        Arc::new(Self {
            free: Mutex::new(Vec::with_capacity(keep)),
            keep,
        })
    }

    /// A buffer of `len` bytes (contents unspecified).
    pub(crate) fn get(self: &Arc<Self>, len: usize) -> HostBuf {
        let mut data = lock(&self.free).pop().unwrap_or_default();
        data.resize(len, 0);
        HostBuf {
            data,
            pool: Arc::clone(self),
        }
    }
}

impl HostBuf {
    /// The bytes, for the conversion to write.
    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

/// One decoded frame. Owns its pixels: dropping it releases the device frame (to the decoder's
/// pool) or the host buffer. `Send`, so that any thread may hold and drop it.
pub struct Frame {
    /// Number of the frame among those the decoder returned (from 0).
    pub idx: i64,
    /// Presentation time on the input's own clock: seconds since its start, continuous over
    /// loops, later by every stall.
    pub t: f64,
    /// Time ([`now`](crate::now)) at which the decoder returned it.
    pub done: f64,
    /// Width of the picture in pixels.
    pub width: u32,
    /// Height of the picture in pixels.
    pub height: u32,
    /// The picture is two fields.
    pub interlaced: bool,
    /// Of an interlaced picture: the top field is the first in time.
    pub top_field_first: bool,
    /// The matrix of its colors.
    pub colorspace: ColorSpace,
    /// Full-range samples (0..255 instead of 16..235).
    pub full_range: bool,
    /// Sample aspect ratio (numerator, denominator); `None` when the stream does not say.
    pub sar: Option<(i32, i32)>,
    /// A53 captions that came with the picture: `cc_data` triplets; empty if none.
    pub cc: Vec<u8>,
    /// The pixels.
    pub surface: Surface,
    /// The libav frame that owns the device memory of `Surface::Cuda`.
    av: Option<ffmpeg::frame::Video>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("idx", &self.idx)
            .field("t", &self.t)
            .field("done", &self.done)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("interlaced", &self.interlaced)
            .field("top_field_first", &self.top_field_first)
            .field("colorspace", &self.colorspace)
            .field("full_range", &self.full_range)
            .field("sar", &self.sar)
            .field("cc", &self.cc.len())
            .field("surface", &self.surface)
            .finish_non_exhaustive()
    }
}

/// What kind of memory a libav frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// `AV_PIX_FMT_CUDA` with a pool of NV12 (1) or P010 / P016 (2 bytes per sample).
    Cuda { bps: u32 },
    /// `AV_PIX_FMT_CUDA` in another software format.
    CudaOther,
    /// Host memory.
    Host,
}

/// The pixel format of the frame's samples (for a device frame: of its pool) and its layout.
pub(crate) fn layout(f: &ffi::AVFrame) -> (ffi::AVPixelFormat, Layout) {
    if f.format != ffi::AVPixelFormat::CUDA.0 {
        return (ffi::AVPixelFormat(f.format), Layout::Host);
    }
    if f.hw_frames_ctx.is_null() {
        return (ffi::AVPixelFormat::CUDA, Layout::CudaOther);
    }
    // SAFETY: a frame of a hardware format carries a reference to its frame pool's context:
    // `hw_frames_ctx` is a live `AVBufferRef` whose data is an `AVHWFramesContext`.
    let sw = unsafe { (*(*f.hw_frames_ctx).data.cast::<ffi::AVHWFramesContext>()).sw_format };
    let l = match sw {
        ffi::AVPixelFormat::NV12 => Layout::Cuda { bps: 1 },
        ffi::AVPixelFormat::P010LE | ffi::AVPixelFormat::P016LE => Layout::Cuda { bps: 2 },
        _ => Layout::CudaOther,
    };
    (sw, l)
}

/// libav's name of a pixel format.
pub(crate) fn pix_fmt_name(p: ffi::AVPixelFormat) -> String {
    // SAFETY: any value may be asked for; the result is null or a static string.
    let name = unsafe { ffi::av_get_pix_fmt_name(p) };
    if name.is_null() {
        return format!("pixel format {}", p.0);
    }
    // SAFETY: not null, so a NUL-terminated static string of libavutil.
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

impl Frame {
    /// The frame as queued: properties read from `av`, the pixels as `surface`. `keep` is the
    /// libav frame when `surface` points into it.
    pub(crate) fn new(
        av: &ffmpeg::frame::Video,
        raw: &ffi::AVFrame,
        idx: i64,
        t: f64,
        done: f64,
        surface: Surface,
    ) -> Self {
        let sar = raw.sample_aspect_ratio;
        Self {
            idx,
            t,
            done,
            width: av.width(),
            height: av.height(),
            interlaced: raw.flags & ffi::AV_FRAME_FLAG_INTERLACED != 0,
            top_field_first: raw.flags & ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0,
            colorspace: ColorSpace::from_av(raw.colorspace),
            full_range: raw.color_range == ffi::AVColorRange::JPEG,
            sar: (sar.num > 0 && sar.den > 0).then_some((sar.num, sar.den)),
            cc: av
                .side_data(ffmpeg::frame::side_data::Type::A53CC)
                .map(|s| s.data().to_vec())
                .unwrap_or_default(),
            surface,
            av: None,
        }
    }

    /// Makes the frame the owner of the libav frame its surface points into.
    pub(crate) fn keep(&mut self, av: ffmpeg::frame::Video) {
        self.av = Some(av);
    }

    /// Whether the frame holds a libav frame (device memory).
    pub fn holds_device_frame(&self) -> bool {
        self.av.is_some()
    }
}
