//! Thin owners of the libav objects the worker uses: decoder contexts, frames, packets,
//! parsers, a hardware decoding device (CUDA or VAAPI). Every `unsafe` block of the worker that
//! touches libav is here.

use std::ffi::{CStr, c_int};
use std::ptr;

use ffmpeg_sys_the_third as ffi;

/// A libav error code (`AVERROR(...)`, negative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvErr(pub c_int);

/// `AVERROR(EAGAIN)`.
pub const EAGAIN: c_int = -libc::EAGAIN;
/// `AVERROR(ENOMEM)`.
pub const ENOMEM: c_int = -libc::ENOMEM;
/// `AVERROR_EOF`.
pub const EOF: c_int = -(((b'E' as c_int) | ((b'O' as c_int) << 8) | ((b'F' as c_int) << 16))
    | ((b' ' as c_int) << 24));

/// Only errors from libav's own log (stderr, which the parent captures and limits).
pub fn quiet() {
    // SAFETY: sets a global integer.
    unsafe { ffi::av_log_set_level(ffi::AV_LOG_ERROR) };
}

/// `avcodec_version()`.
pub fn avcodec_version() -> u32 {
    // SAFETY: no arguments; returns a constant.
    unsafe { ffi::avcodec_version() }
}

/// Loads the first of these shared libraries that loads and keeps it loaded for the life of
/// the process, so that a later `dlopen` by name finds it without opening a file.
pub fn preload(names: &[&CStr]) -> bool {
    names.iter().any(|n| {
        // SAFETY: a NUL-terminated name; the handle is never closed (the library stays loaded
        // for the process's life, which is the point).
        !unsafe { libc::dlopen(n.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null()
    })
}

/// A decoder of this libav build.
#[derive(Debug, Clone, Copy)]
pub struct Codec(*const ffi::AVCodec);

impl Codec {
    /// The decoder of this name, if the build has it.
    pub fn find(name: &CStr) -> Option<Self> {
        // SAFETY: a NUL-terminated name; returns a static descriptor or null.
        let p = unsafe { ffi::avcodec_find_decoder_by_name(name.as_ptr()) };
        (!p.is_null()).then_some(Self(p))
    }

    fn id(self) -> ffi::AVCodecID {
        // SAFETY: a non-null static descriptor of libavcodec.
        unsafe { (*self.0).id }
    }
}

/// The hardware decoders the worker can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwKind {
    /// NVDEC through the `cuda` hwaccel (NVIDIA).
    Cuda,
    /// VA-API through the `vaapi` hwaccel (AMD and Intel: Mesa's or Intel's VA driver on a DRM
    /// render node).
    Vaapi,
}

impl HwKind {
    fn device_type(self) -> ffi::AVHWDeviceType {
        match self {
            Self::Cuda => ffi::AVHWDeviceType::CUDA,
            Self::Vaapi => ffi::AVHWDeviceType::VAAPI,
        }
    }
}

/// A hardware device context for decoding.
pub struct HwDevice {
    buf: *mut ffi::AVBufferRef,
    kind: HwKind,
}

impl HwDevice {
    /// Opens the default device of `kind`: loads the driver's libraries and, for VA-API, opens
    /// the first DRM render node and initializes its VA driver. Done before the system-call
    /// filter is in place.
    pub fn open(kind: HwKind) -> Result<Self, AvErr> {
        let mut p: *mut ffi::AVBufferRef = ptr::null_mut();
        // SAFETY: `p` receives a new reference on success; no device name, no options.
        let r = unsafe {
            ffi::av_hwdevice_ctx_create(
                &raw mut p,
                kind.device_type(),
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if r < 0 || p.is_null() {
            return Err(AvErr(r));
        }
        Ok(Self { buf: p, kind })
    }

    /// Which decoder this is.
    pub fn kind(&self) -> HwKind {
        self.kind
    }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // SAFETY: a reference this object owns; unref sets the pointer to null.
        unsafe { ffi::av_buffer_unref(&raw mut self.buf) };
    }
}

/// `get_format` with a device: the device's format (`HW`: CUDA or VAAPI) if offered, else the
/// first software format (libav then decodes in software).
unsafe extern "C" fn pick_hw<const HW: i32>(
    _: *mut ffi::AVCodecContext,
    list: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut first_sw = ffi::AVPixelFormat::NONE;
    let mut f = list;
    // SAFETY: libavcodec passes a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        while *f != ffi::AVPixelFormat::NONE {
            if *f == ffi::AVPixelFormat(HW) {
                return *f;
            }
            if first_sw == ffi::AVPixelFormat::NONE {
                first_sw = *f;
            }
            f = f.add(1);
        }
    }
    first_sw
}

/// What a decoder is opened with.
pub struct OpenParams<'a> {
    /// Codec configuration.
    pub extradata: &'a [u8],
    /// Time base of packets (num, den).
    pub time_base: (i32, i32),
    /// Threads (0: libav's choice).
    pub threads: i32,
    /// Audio rate and channels when known (raw formats need them).
    pub audio: Option<(i32, i32)>,
    /// Video size when known.
    pub video: Option<(i32, i32)>,
    /// Decode on this device.
    pub hw: Option<&'a HwDevice>,
}

/// An open decoder.
pub struct Decoder {
    ctx: *mut ffi::AVCodecContext,
}

impl Decoder {
    /// Allocates, configures and opens a decoder.
    pub fn open(codec: Codec, p: &OpenParams<'_>) -> Result<Self, AvErr> {
        // SAFETY: a valid codec descriptor; returns a new context or null.
        let ctx = unsafe { ffi::avcodec_alloc_context3(codec.0) };
        if ctx.is_null() {
            return Err(AvErr(ENOMEM));
        }
        let d = Self { ctx };
        let pad = ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
        // SAFETY: `ctx` is a fresh context this object owns, not yet open; these are fields
        // the user sets before avcodec_open2. The extradata buffer is av_mallocz'd with the
        // padding libavcodec requires and handed to the context, which frees it.
        unsafe {
            let c = &mut *ctx;
            c.pkt_timebase = ffi::AVRational {
                num: p.time_base.0,
                den: p.time_base.1,
            };
            c.thread_count = p.threads;
            if !p.extradata.is_empty() {
                let buf = ffi::av_mallocz(p.extradata.len() + pad).cast::<u8>();
                if buf.is_null() {
                    return Err(AvErr(ENOMEM));
                }
                ptr::copy_nonoverlapping(p.extradata.as_ptr(), buf, p.extradata.len());
                c.extradata = buf;
                c.extradata_size = p.extradata.len() as c_int;
            }
            if let Some((rate, ch)) = p.audio {
                c.sample_rate = rate;
                ffi::av_channel_layout_default(&raw mut c.ch_layout, ch);
            }
            if let Some((w, h)) = p.video {
                c.width = w;
                c.height = h;
            }
            if let Some(hw) = p.hw {
                c.hw_device_ctx = ffi::av_buffer_ref(hw.buf);
                if c.hw_device_ctx.is_null() {
                    return Err(AvErr(ENOMEM));
                }
                c.get_format = Some(match hw.kind {
                    HwKind::Cuda => pick_hw::<{ ffi::AVPixelFormat::CUDA.0 }>,
                    HwKind::Vaapi => pick_hw::<{ ffi::AVPixelFormat::VAAPI.0 }>,
                });
            }
        }
        // SAFETY: the context was allocated for `codec`; no options dictionary.
        let r = unsafe { ffi::avcodec_open2(ctx, codec.0, ptr::null_mut()) };
        if r < 0 {
            return Err(AvErr(r));
        }
        Ok(d)
    }

    /// Sends a packet (`None`: end of stream). `Err(EAGAIN)`: receive frames first.
    pub fn send(&mut self, pkt: Option<&Packet>) -> Result<(), AvErr> {
        let p = pkt.map_or(ptr::null(), |p| p.0.cast_const());
        // SAFETY: an open context this object owns; a valid packet or null.
        let r = unsafe { ffi::avcodec_send_packet(self.ctx, p) };
        if r < 0 { Err(AvErr(r)) } else { Ok(()) }
    }

    /// Receives a frame into `f`. `Ok(false)`: none now (EAGAIN) or the end (EOF).
    pub fn receive(&mut self, f: &mut Frame) -> Result<bool, AvErr> {
        // SAFETY: an open context and a frame this process owns.
        let r = unsafe { ffi::avcodec_receive_frame(self.ctx, f.0) };
        match r {
            0 => Ok(true),
            EAGAIN | EOF => Ok(false),
            e => Err(AvErr(e)),
        }
    }

    /// Resets the decoder (after a drain, before new input).
    pub fn flush(&mut self) {
        // SAFETY: an open context this object owns.
        unsafe { ffi::avcodec_flush_buffers(self.ctx) };
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context this object owns; free sets the pointer to null.
        unsafe { ffi::avcodec_free_context(&raw mut self.ctx) };
    }
}

/// A packet that owns a padded copy of its bytes.
pub struct Packet(*mut ffi::AVPacket);

impl Packet {
    /// A packet holding a copy of `data`.
    pub fn copy(
        data: &[u8],
        pts: Option<i64>,
        dts: Option<i64>,
        dur: Option<i64>,
        key: bool,
    ) -> Result<Self, AvErr> {
        let len = c_int::try_from(data.len()).map_err(|_| AvErr(-libc::EINVAL))?;
        // SAFETY: returns a new blank packet or null.
        let p = unsafe { ffi::av_packet_alloc() };
        if p.is_null() {
            return Err(AvErr(ENOMEM));
        }
        let pkt = Self(p);
        // SAFETY: `p` is blank; av_new_packet allocates `len` bytes plus zeroed padding, and
        // `data` is copied into the first `len`.
        unsafe {
            let r = ffi::av_new_packet(p, len);
            if r < 0 {
                return Err(AvErr(r));
            }
            ptr::copy_nonoverlapping(data.as_ptr(), (*p).data, data.len());
            (*p).pts = pts.unwrap_or(ffi::AV_NOPTS_VALUE);
            (*p).dts = dts.unwrap_or(ffi::AV_NOPTS_VALUE);
            (*p).duration = dur.unwrap_or(0);
            if key {
                (*p).flags |= ffi::AV_PKT_FLAG_KEY;
            }
        }
        Ok(pkt)
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        // SAFETY: the packet this object owns; free sets the pointer to null.
        unsafe { ffi::av_packet_free(&raw mut self.0) };
    }
}

/// A codec parser (joins MPEG-TS fragments into access units).
pub struct Parser(*mut ffi::AVCodecParserContext);

/// One access unit out of a parser.
pub struct Unit {
    /// Its bytes (a copy).
    pub packet: Packet,
}

impl Parser {
    /// A parser for the codec, if libavcodec has one.
    pub fn new(codec: Codec) -> Option<Self> {
        // SAFETY: returns a new parser context or null.
        let p = unsafe { ffi::av_parser_init(codec.id()) };
        (!p.is_null()).then_some(Self(p))
    }

    /// Feeds `data` (empty: flush) and calls `out` with every complete unit. Units carry
    /// `dur`, the input's duration (one access unit per packet is the usual case).
    pub fn parse(
        &mut self,
        dec: &mut Decoder,
        mut data: &[u8],
        (pts, dts): (Option<i64>, Option<i64>),
        dur: Option<i64>,
        mut out: impl FnMut(Unit),
    ) -> Result<(), AvErr> {
        let flush = data.is_empty();
        let (mut pts, mut dts) = (
            pts.unwrap_or(ffi::AV_NOPTS_VALUE),
            dts.unwrap_or(ffi::AV_NOPTS_VALUE),
        );
        loop {
            let mut ob: *mut u8 = ptr::null_mut();
            let mut on: c_int = 0;
            let len = c_int::try_from(data.len()).map_err(|_| AvErr(-libc::EINVAL))?;
            // SAFETY: parser and decoder contexts this process owns; `data` is readable for
            // `len` bytes. The output buffer is the parser's, valid until the next call; it is
            // copied before then.
            let used = unsafe {
                ffi::av_parser_parse2(
                    self.0,
                    dec.ctx,
                    &raw mut ob,
                    &raw mut on,
                    data.as_ptr(),
                    len,
                    pts,
                    dts,
                    0,
                )
            };
            if used < 0 {
                return Err(AvErr(used));
            }
            // timestamps belong to the first unit that starts in this input only
            pts = ffi::AV_NOPTS_VALUE;
            dts = ffi::AV_NOPTS_VALUE;
            if on > 0 && !ob.is_null() {
                // SAFETY: the parser returned `on` readable bytes at `ob`.
                let unit = unsafe { std::slice::from_raw_parts(ob, on as usize) };
                // SAFETY: the parser context's fields after a call that output a unit.
                let (upts, udts, key) =
                    unsafe { ((*self.0).pts, (*self.0).dts, (*self.0).key_frame == 1) };
                let opt = |t: i64| (t != ffi::AV_NOPTS_VALUE).then_some(t);
                out(Unit {
                    packet: Packet::copy(unit, opt(upts), opt(udts), dur, key)?,
                });
            }
            let used = used as usize;
            data = data.get(used..).unwrap_or_default();
            if flush {
                if on == 0 {
                    return Ok(());
                }
            } else if data.is_empty() {
                return Ok(());
            } else if used == 0 && on == 0 {
                // no progress: should not happen; stop rather than spin
                return Ok(());
            }
        }
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        // SAFETY: the parser this object owns, closed once.
        unsafe { ffi::av_parser_close(self.0) };
    }
}

/// A frame (video or audio), reused across receives.
pub struct Frame(*mut ffi::AVFrame);

/// Pixel layouts the worker distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pix {
    /// 8-bit planar 4:2:0 (limited or full range).
    Yuv420p,
    /// 8-bit planar 4:2:2.
    Yuv422p,
    /// 10-bit planar 4:2:0, 16-bit little-endian samples, value in the low bits.
    Yuv420p10,
    /// 8-bit 4:2:0, interleaved chroma.
    Nv12,
    /// 10-bit 4:2:0, interleaved chroma, value in the high bits.
    P010,
    /// A device frame (CUDA or VAAPI): transferred to host memory before use.
    Device,
    /// Anything else.
    Other(i32),
}

/// Sample layouts the worker distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Smp {
    /// f32 planar.
    Fltp,
    /// f32 interleaved.
    Flt,
    /// s16 planar or interleaved.
    S16(bool),
    /// s32 planar or interleaved.
    S32(bool),
    /// f64 planar or interleaved.
    Dbl(bool),
    /// Anything else.
    Other(i32),
}

impl Frame {
    /// A blank frame.
    pub fn new() -> Result<Self, AvErr> {
        // SAFETY: returns a new blank frame or null.
        let p = unsafe { ffi::av_frame_alloc() };
        if p.is_null() {
            return Err(AvErr(ENOMEM));
        }
        Ok(Self(p))
    }

    fn f(&self) -> &ffi::AVFrame {
        // SAFETY: a valid frame this object owns.
        unsafe { &*self.0 }
    }

    /// Drops the frame's buffers.
    pub fn unref(&mut self) {
        // SAFETY: a valid frame this object owns.
        unsafe { ffi::av_frame_unref(self.0) };
    }

    /// Width, height.
    pub fn size(&self) -> (u32, u32) {
        let f = self.f();
        (f.width.max(0) as u32, f.height.max(0) as u32)
    }

    /// The pixel layout.
    pub fn pix(&self) -> Pix {
        let v = self.f().format;
        let p = ffi::AVPixelFormat(v);
        match p {
            ffi::AVPixelFormat::YUV420P | ffi::AVPixelFormat::YUVJ420P => Pix::Yuv420p,
            ffi::AVPixelFormat::YUV422P | ffi::AVPixelFormat::YUVJ422P => Pix::Yuv422p,
            ffi::AVPixelFormat::YUV420P10LE => Pix::Yuv420p10,
            ffi::AVPixelFormat::NV12 => Pix::Nv12,
            ffi::AVPixelFormat::P010LE => Pix::P010,
            ffi::AVPixelFormat::CUDA | ffi::AVPixelFormat::VAAPI => Pix::Device,
            _ => Pix::Other(v),
        }
    }

    /// The sample layout.
    pub fn smp(&self) -> Smp {
        let v = self.f().format;
        match ffi::AVSampleFormat(v) {
            ffi::AVSampleFormat::FLTP => Smp::Fltp,
            ffi::AVSampleFormat::FLT => Smp::Flt,
            ffi::AVSampleFormat::S16 => Smp::S16(false),
            ffi::AVSampleFormat::S16P => Smp::S16(true),
            ffi::AVSampleFormat::S32 => Smp::S32(false),
            ffi::AVSampleFormat::S32P => Smp::S32(true),
            ffi::AVSampleFormat::DBL => Smp::Dbl(false),
            ffi::AVSampleFormat::DBLP => Smp::Dbl(true),
            _ => Smp::Other(v),
        }
    }

    /// Whether the pixel format's range is full (the `J` formats, or the flag).
    pub fn full_range(&self) -> Option<bool> {
        let f = self.f();
        match ffi::AVPixelFormat(f.format) {
            ffi::AVPixelFormat::YUVJ420P | ffi::AVPixelFormat::YUVJ422P => Some(true),
            _ => match f.color_range {
                ffi::AVColorRange::JPEG => Some(true),
                ffi::AVColorRange::MPEG => Some(false),
                _ => None,
            },
        }
    }

    /// The color matrix: 1 BT.709, 2 BT.601, 3 BT.2020, 0 unknown.
    pub fn matrix(&self) -> u8 {
        match self.f().colorspace {
            ffi::AVColorSpace::BT709 => 1,
            ffi::AVColorSpace::SMPTE170M | ffi::AVColorSpace::BT470BG => 2,
            ffi::AVColorSpace::BT2020_NCL => 3,
            _ => 0,
        }
    }

    /// The best-effort presentation time.
    pub fn pts(&self) -> Option<i64> {
        let t = self.f().best_effort_timestamp;
        (t != ffi::AV_NOPTS_VALUE).then_some(t)
    }

    /// Interlaced, top field first.
    pub fn interlace(&self) -> (bool, bool) {
        let fl = self.f().flags;
        (
            fl & ffi::AV_FRAME_FLAG_INTERLACED != 0,
            fl & ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0,
        )
    }

    /// Damaged.
    pub fn corrupt(&self) -> bool {
        let f = self.f();
        f.flags & ffi::AV_FRAME_FLAG_CORRUPT != 0 || f.decode_error_flags != 0
    }

    /// Sample aspect ratio if signaled.
    pub fn sar(&self) -> Option<(u32, u32)> {
        let r = self.f().sample_aspect_ratio;
        (r.num > 0 && r.den > 0).then_some((r.num as u32, r.den as u32))
    }

    /// The A/53 caption side data.
    pub fn a53_cc(&self) -> Vec<u8> {
        // SAFETY: a valid frame; returns its side data entry of that type or null.
        let sd = unsafe { ffi::av_frame_get_side_data(self.0, ffi::AVFrameSideDataType::A53_CC) };
        if sd.is_null() {
            return Vec::new();
        }
        // SAFETY: a side data entry of the frame: `size` readable bytes at `data`.
        unsafe {
            let s = &*sd;
            if s.data.is_null() {
                return Vec::new();
            }
            std::slice::from_raw_parts(s.data, s.size).to_vec()
        }
    }

    /// Row `r` of plane `i`, `bytes` long; `None` if the plane does not exist, its pitch is
    /// negative or shorter than `bytes`.
    pub fn row(&self, i: usize, r: usize, bytes: usize) -> Option<&[u8]> {
        let f = self.f();
        let base = *f.data.get(i)?;
        let pitch = *f.linesize.get(i)?;
        if base.is_null() || pitch < 0 || (pitch as usize) < bytes {
            return None;
        }
        // SAFETY: libavcodec returned this frame with plane `i` of at least `rows` rows of
        // `pitch` bytes; callers ask for rows below the plane's height (computed from the
        // frame's own size and format) and for at most `pitch` bytes of each.
        Some(unsafe { std::slice::from_raw_parts(base.add(r * pitch as usize), bytes) })
    }

    /// Samples per channel, rate, channels.
    pub fn audio(&self) -> (u32, u32, u32) {
        let f = self.f();
        (
            f.nb_samples.max(0) as u32,
            f.sample_rate.max(0) as u32,
            f.ch_layout.nb_channels.max(0) as u32,
        )
    }

    /// Plane `i` of an audio frame as bytes, `bytes` long (planar: one channel; packed: all).
    pub fn audio_plane(&self, i: usize, bytes: usize) -> Option<&[u8]> {
        let f = self.f();
        // audio planes beyond 8 live in extended_data; this worker takes at most 8 channels
        // planar (more are refused by the caller)
        let base = *f.data.get(i)?;
        if base.is_null() || (f.linesize[0] as usize) < bytes {
            return None;
        }
        // SAFETY: for audio, every plane has linesize[0] bytes; `bytes` is at most that.
        Some(unsafe { std::slice::from_raw_parts(base, bytes) })
    }

    /// Copies a device frame to host memory (NV12 or P010) into `dst`.
    pub fn transfer_to(&self, dst: &mut Frame) -> Result<(), AvErr> {
        dst.unref();
        // SAFETY: two valid frames; `self` is a hardware frame with a frames context.
        let r = unsafe { ffi::av_hwframe_transfer_data(dst.0, self.0, 0) };
        if r < 0 {
            return Err(AvErr(r));
        }
        // SAFETY: as above; copies timestamps, flags and side data.
        let r = unsafe { ffi::av_frame_copy_props(dst.0, self.0) };
        if r < 0 { Err(AvErr(r)) } else { Ok(()) }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: the frame this object owns; free sets the pointer to null.
        unsafe { ffi::av_frame_free(&raw mut self.0) };
    }
}

impl Frame {
    /// The raw pixel format (`AVPixelFormat` value).
    pub fn raw_format(&self) -> c_int {
        self.f().format
    }

    /// The duration libav carried with the frame (stream time base), if known.
    pub fn duration(&self) -> Option<i64> {
        let d = self.f().duration;
        (d > 0).then_some(d)
    }
}

/// `SWS_CS_*` of a matrix code of [`Frame::matrix`] (1 BT.709, 2 BT.601, 3 BT.2020).
pub fn sws_cs(matrix: u8) -> c_int {
    match matrix {
        1 => ffi::SWS_CS_ITU709,
        3 => ffi::SWS_CS_BT2020,
        _ => ffi::SWS_CS_ITU601,
    }
}

/// A libswscale context from one source geometry to planar 8-bit 4:2:0 of one size, Lanczos,
/// with the color matrix and range conversion set.
pub struct Scaler {
    ctx: *mut ffi::SwsContext,
    /// What it was made for: source width, height, format, matrix, full range; destination
    /// width, height, matrix.
    pub key: (c_int, c_int, c_int, c_int, bool, c_int, c_int, c_int),
}

/// Destination planes of [`Scaler::scale`]: Y, U, V with their strides.
pub struct Dst<'a> {
    /// The planes.
    pub planes: [&'a mut [u8]; 3],
    /// Their strides.
    pub strides: [usize; 3],
}

impl Scaler {
    /// A context; `None` if libswscale cannot convert this format.
    pub fn new(key: (c_int, c_int, c_int, c_int, bool, c_int, c_int, c_int)) -> Option<Self> {
        let (sw, sh, sf, scs, sfull, dw, dh, dcs) = key;
        if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
            return None;
        }
        let flags = (ffi::SwsFlags::LANCZOS.0 | ffi::SwsFlags::ACCURATE_RND.0) as c_int;
        // SAFETY: plain arguments; no filters, default parameters; null when it cannot.
        let ctx = unsafe {
            ffi::sws_getContext(
                sw,
                sh,
                ffi::AVPixelFormat(sf),
                dw,
                dh,
                ffi::AVPixelFormat::YUV420P,
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if ctx.is_null() {
            return None;
        }
        let s = Self { ctx, key };
        // SAFETY: a context made above; the coefficient tables are static tables of
        // libswscale. Brightness 0, contrast and saturation 1.0 in 16.16.
        unsafe {
            ffi::sws_setColorspaceDetails(
                ctx,
                ffi::sws_getCoefficients(scs),
                c_int::from(sfull),
                ffi::sws_getCoefficients(dcs),
                0,
                0,
                1 << 16,
                1 << 16,
            );
        }
        Some(s)
    }

    /// Scales frame `f` (or one field of it: `Some(0)` top, `Some(1)` bottom) into `dst` of
    /// the context's destination size. False if a destination plane is too short.
    pub fn scale(&mut self, f: &Frame, field: Option<usize>, dst: Dst<'_>) -> bool {
        let (_, sh, _, _, _, dw, dh, _) = self.key;
        let (dw, dh) = (dw as usize, dh as usize);
        let (cw, ch) = (dw.div_ceil(2), dh.div_ceil(2));
        let need = [
            (dh - 1) * dst.strides[0] + dw,
            (ch - 1) * dst.strides[1] + cw,
            (ch - 1) * dst.strides[2] + cw,
        ];
        if dst.planes.iter().zip(need).any(|(p, n)| p.len() < n)
            || dst.strides.iter().any(|s| *s > c_int::MAX as usize)
        {
            return false;
        }
        let fr = f.f();
        let mut src = [ptr::null::<u8>(); 4];
        let mut stride = [0 as c_int; 4];
        for i in 0..4 {
            let p = fr.data[i];
            let l = fr.linesize[i];
            if p.is_null() {
                continue;
            }
            match field {
                None => {
                    src[i] = p;
                    stride[i] = l;
                }
                Some(k) => {
                    // SAFETY: row `k` (0 or 1) of the plane exists (the caller asks for
                    // fields only of pictures at least four rows high).
                    src[i] = unsafe { p.offset(k as isize * l as isize) };
                    stride[i] = l * 2;
                }
            }
        }
        let [y, u, v] = dst.planes;
        let dptr = [
            y.as_mut_ptr(),
            u.as_mut_ptr(),
            v.as_mut_ptr(),
            ptr::null_mut(),
        ];
        let dstride = [
            dst.strides[0] as c_int,
            dst.strides[1] as c_int,
            dst.strides[2] as c_int,
            0,
        ];
        // SAFETY: the context was made for this source size and format (the caller checks
        // the key against the frame) and the destination size. The source planes are the
        // frame's, read for `sh` rows of the given strides: the whole picture, or every
        // other row from row 0 or 1 of a picture of 2 * sh rows. The destination planes
        // hold at least the rows the size needs (checked above).
        let r = unsafe {
            ffi::sws_scale(
                self.ctx,
                src.as_ptr(),
                stride.as_ptr(),
                0,
                sh,
                dptr.as_ptr(),
                dstride.as_ptr(),
            )
        };
        r > 0
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: the context this object owns, freed once.
        unsafe { ffi::sws_freeContext(self.ctx) };
    }
}

/// A libswresample context: any input to 48 kHz f32 planar of a fixed layout.
pub struct Resampler {
    swr: *mut ffi::SwrContext,
    /// Input rate, format, channels it was made for.
    pub key: (c_int, c_int, c_int),
    out: Vec<Vec<f32>>,
}

impl Resampler {
    /// A context for frames like `f`, to `mask` (`AV_CH_*` bits, native order) at `rate`.
    pub fn new(f: &Frame, mask: u64, rate: c_int) -> Option<Self> {
        let fr = f.f();
        let key = (fr.sample_rate, fr.format, fr.ch_layout.nb_channels);
        if key.0 <= 0 || key.2 <= 0 {
            return None;
        }
        // SAFETY: zeroed AVChannelLayouts are valid "unspecified" layouts for the setters,
        // which fill them; both are uninitialized again below.
        let mut out_l: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        let mut in_l: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
        let mut swr: *mut ffi::SwrContext = ptr::null_mut();
        // SAFETY: the layouts are local and valid; the frame's layout is read only; `swr`
        // receives a new context or stays null.
        let r = unsafe {
            if ffi::av_channel_layout_from_mask(&raw mut out_l, mask) < 0 {
                return None;
            }
            if fr.ch_layout.order == ffi::AVChannelOrder(1) {
                ffi::av_channel_layout_copy(&raw mut in_l, &raw const fr.ch_layout);
            } else {
                ffi::av_channel_layout_default(&raw mut in_l, fr.ch_layout.nb_channels);
            }
            let r = ffi::swr_alloc_set_opts2(
                &raw mut swr,
                &raw const out_l,
                ffi::AVSampleFormat::FLTP,
                rate,
                &raw const in_l,
                ffi::AVSampleFormat(fr.format),
                fr.sample_rate,
                0,
                ptr::null_mut(),
            );
            let r = if r >= 0 && !swr.is_null() {
                ffi::swr_init(swr)
            } else {
                -1
            };
            ffi::av_channel_layout_uninit(&raw mut out_l);
            ffi::av_channel_layout_uninit(&raw mut in_l);
            r
        };
        let channels = mask.count_ones() as usize;
        let s = Self {
            swr,
            key,
            out: vec![Vec::new(); channels],
        };
        (r >= 0).then_some(s)
    }

    /// Whether frames like `f` can go through this context.
    pub fn fits(&self, f: &Frame) -> bool {
        let fr = f.f();
        self.key == (fr.sample_rate, fr.format, fr.ch_layout.nb_channels)
    }

    /// Samples still inside, in units of `1 / base` seconds.
    pub fn delay(&self, base: i64) -> i64 {
        // SAFETY: an initialized context this object owns.
        unsafe { ffi::swr_get_delay(self.swr, base) }
    }

    /// Converts `f` (`None`: flush what is buffered). Returns the output planes, one per
    /// channel in native order, of the same length.
    pub fn convert(&mut self, f: Option<&Frame>) -> Result<&[Vec<f32>], AvErr> {
        let n_in = f.map_or(0, |f| f.f().nb_samples.max(0));
        // SAFETY: an initialized context.
        let cap = unsafe { ffi::swr_get_out_samples(self.swr, n_in) };
        if cap < 0 {
            return Err(AvErr(cap));
        }
        let cap = cap.max(0) as usize + 32;
        for p in &mut self.out {
            p.clear();
            p.resize(cap, 0.0);
        }
        let ptrs: Vec<*mut u8> = self.out.iter_mut().map(|p| p.as_mut_ptr().cast()).collect();
        let inp = f.map_or(ptr::null(), |f| {
            f.f().extended_data.cast_const().cast::<*const u8>()
        });
        // SAFETY: one output plane per output channel of `cap` f32 values; the input is the
        // frame's own planes (extended_data) of `n_in` samples in the format and layout the
        // context was made for (the caller checks `fits`), or null to flush.
        let n = unsafe { ffi::swr_convert(self.swr, ptrs.as_ptr(), cap as c_int, inp, n_in) };
        if n < 0 {
            return Err(AvErr(n));
        }
        for p in &mut self.out {
            p.truncate(n as usize);
        }
        Ok(&self.out)
    }
}

impl Drop for Resampler {
    fn drop(&mut self) {
        // SAFETY: the context this object owns; free sets the pointer to null.
        unsafe { ffi::swr_free(&raw mut self.swr) };
    }
}
