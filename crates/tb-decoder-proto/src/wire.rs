//! The control channel's messages and their binary form.
//!
//! One message is one `SOCK_SEQPACKET` datagram: a 12-byte header (magic `TBAV`, protocol
//! version, kind, payload length, all little-endian) and the payload. The datagram's length
//! must equal header plus payload exactly, and no message exceeds [`MAX_MESSAGE`] bytes, so a
//! receiver reads into a fixed buffer and treats a truncated datagram as a violation.
//!
//! Hand-rolled rather than a serialization crate: the messages are few and small, the hot
//! ones (`Packet`, `Video`, `Audio`, `Consumed`, `Release`) are a few dozen bytes, and every
//! field is range-checked as it is read. Bulk data never travels here: packet bytes are in the
//! input ring, pixels and samples in output pools; file descriptors of those travel as
//! `SCM_RIGHTS` next to `Configure` and `PoolGrant`.

use tb_media::{
    AudioFrameDesc, AudioHint, CodecId, Matrix, PixelFormat, Plane, Range, Rational, StreamIndex,
    StreamInfo, VideoFrameDesc, VideoHint,
};

/// First four bytes of every message.
pub const MAGIC: [u8; 4] = *b"TBAV";
/// Protocol version. Both sides refuse any other.
pub const VERSION: u16 = 2;
/// Bytes of the header.
pub const HEADER_LEN: usize = 12;
/// Largest message, header included.
pub const MAX_MESSAGE: usize = 64 * 1024;
/// Most streams in one `Streams` message.
pub const MAX_STREAMS: usize = 64;
/// Most bytes of a stream's extradata.
pub const MAX_EXTRADATA: usize = 32 * 1024;
/// Most slots named by one `Release` message.
pub const MAX_RELEASE: usize = 1024;
/// Most bytes of caption data with one picture (A/53 allows 31 triplets per picture; libav
/// may join those of both fields and of repeated pictures).
pub const MAX_CC: usize = 1024;
/// Most bytes of the text of a worker's `Error`.
pub const MAX_TEXT: usize = 256;
/// Most results in a `Probe` report.
pub const MAX_PROBE: usize = 32;

/// Why a message could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The datagram ended before a field.
    #[error("message truncated: {need} more bytes needed at offset {at}")]
    Short {
        /// Offset of the field.
        at: usize,
        /// Bytes it needs.
        need: usize,
    },
    /// The first four bytes are not [`MAGIC`].
    #[error("bad magic")]
    Magic,
    /// The peer speaks another protocol version.
    #[error("protocol version {0}, expected {VERSION}")]
    Version(u16),
    /// The kind is unknown for this direction.
    #[error("unknown message kind {0}")]
    Kind(u16),
    /// The header's length disagrees with the datagram.
    #[error("payload length {declared} declared, {actual} received")]
    Length {
        /// From the header.
        declared: usize,
        /// Received.
        actual: usize,
    },
    /// A message is larger than [`MAX_MESSAGE`].
    #[error("message of {0} bytes exceeds the limit")]
    TooLarge(usize),
    /// A field has a value outside its range.
    #[error("field {field}: value {value} out of range")]
    Value {
        /// The field.
        field: &'static str,
        /// The value read (or the length, for a sequence).
        value: u64,
    },
    /// Bytes after the last field.
    #[error("{0} bytes after the message")]
    Trailing(usize),
}

/// How the worker's system-call filter acts on a call outside its allow-list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeccompMode {
    /// No filter.
    Off,
    /// The call fails with `EPERM` (tests observe the denial).
    Errno,
    /// The process is killed with `SIGSYS` (production).
    Kill,
}

/// Hidden test modes of the worker. Production configurations carry `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fault {
    /// Normal operation.
    None,
    /// Raise `SIGSEGV` on itself when this packet (counted from 1) arrives.
    CrashAfter(u32),
    /// Stop reading anything (sleep for ever) at this packet.
    HangAfter(u32),
    /// At this packet, send a frame descriptor that names a slot outside its pool.
    BadDescriptorAfter(u32),
    /// At this packet, allocate memory without bound.
    AllocAfter(u32),
    /// At this packet, spin on the CPU without reading.
    SpinAfter(u32),
    /// Before and after the system-call filter, try what the sandbox must deny and report
    /// each outcome in a `Probe` message; with [`SeccompMode::Kill`], also try `socket(2)`
    /// once more after reporting (and die of it).
    Probe,
}

/// The first message to the worker: what it needs to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configure {
    /// Bytes of the input ring (its memfd is attached).
    pub input_ring_len: u64,
    /// Threads per software decoder (0: libav's choice).
    pub threads: u8,
    /// Decode on the GPU (NVDEC through the `cuda` hwaccel) where the codec allows it.
    pub gpu: bool,
    /// The system-call filter the worker installs after libav's initialization.
    pub seccomp: SeccompMode,
    /// Slots the worker asks for in a video pool.
    pub video_slots: u16,
    /// Slots the worker asks for in an audio pool.
    pub audio_slots: u16,
    /// Milliseconds the worker waits for a free slot before it drops a decoded frame.
    pub slot_wait_ms: u32,
    /// Largest slot the parent will grant.
    pub max_frame_bytes: u64,
    /// Test mode.
    pub fault: Fault,
}

/// Where a packet's bytes are in the input ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketRef {
    /// The stream.
    pub stream: StreamIndex,
    /// Presentation time (stream time base).
    pub pts: Option<i64>,
    /// Decoding time.
    pub dts: Option<i64>,
    /// Duration.
    pub duration: Option<i64>,
    /// A random access point.
    pub keyframe: bool,
    /// Absolute ring position of the first byte (see [`crate::ring`]).
    pub pos: u64,
    /// Bytes.
    pub len: u32,
}

/// An output pool given to the worker (its memfd is attached).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolGrant {
    /// The pool's identifier (chosen by the parent).
    pub pool: u32,
    /// The stream it serves.
    pub stream: StreamIndex,
    /// Bytes per slot; slot `i` starts at `i * slot_size`.
    pub slot_size: u64,
    /// Number of slots.
    pub slots: u16,
}

/// How a converted picture fills its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Aspect {
    /// Scaled to the target size whatever its shape.
    Stretch,
    /// Scaled to fit inside the target keeping its display aspect, the rest black
    /// (letterbox or pillarbox).
    Fit,
}

/// Deinterlacing of a converted picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deinterlace {
    /// None: an interlaced picture is scaled as a frame and stays flagged interlaced.
    Off,
    /// Bob: each field of a picture flagged interlaced becomes a picture of its own, scaled
    /// to the full target height (two outputs per input, the second half a frame later).
    Bob,
}

/// Video delivered converted (libswscale in the worker) instead of native: planar 8-bit
/// 4:2:0 of the target size, limited range, rows without padding (Y, then U, then V), the
/// `tb_m2v_encoder::FrameSource::Host` layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoSpec {
    /// Target width (even).
    pub width: u32,
    /// Target height (even).
    pub height: u32,
    /// Display aspect ratio of the target picture (16/9, 4/3).
    pub display_aspect: Rational,
    /// How the source fills the target.
    pub aspect: Aspect,
    /// Deinterlacing.
    pub deinterlace: Deinterlace,
}

impl VideoSpec {
    /// The target's color matrix: BT.709 from 720 lines up, BT.601 below (a source of the
    /// other kind is converted).
    pub fn matrix(&self) -> tb_media::Matrix {
        if self.height >= 720 {
            tb_media::Matrix::Bt709
        } else {
            tb_media::Matrix::Bt601
        }
    }
}

/// Channel layouts of resampled audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioLayout {
    /// L, R.
    Stereo,
    /// 5.1 in A/52 order: L, C, R, Ls, Rs, LFE.
    Surround51,
}

impl AudioLayout {
    /// Channels.
    pub fn channels(self) -> u8 {
        match self {
            Self::Stereo => 2,
            Self::Surround51 => 6,
        }
    }
}

/// Audio delivered resampled (libswresample in the worker): 48 kHz, f32 planar, this layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioSpec {
    /// The layout.
    pub layout: AudioLayout,
}

/// Rate of resampled audio.
pub const RESAMPLED_RATE: u32 = 48_000;

/// Largest side of a target picture.
pub const MAX_TARGET: u32 = 8192;

/// What the worker delivers for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutputSpec {
    /// Frames as the decoder makes them (the default).
    #[default]
    Native,
    /// Converted video.
    Video(VideoSpec),
    /// Resampled audio.
    Audio(AudioSpec),
}

/// One stream to open, and what to deliver for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOpen {
    /// The stream.
    pub info: StreamInfo,
    /// Native or converted frames.
    pub output: OutputSpec,
}

/// Messages from the parent to the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToWorker {
    /// Start-up parameters; the input ring's memfd is attached.
    Configure(Configure),
    /// A new set of streams (a new session or a reconfiguration), each native or converted.
    Streams(Vec<StreamOpen>),
    /// A packet in the input ring.
    Packet(PacketRef),
    /// The session ended: drain the decoders, answer `Ended`.
    End,
    /// An output pool; its memfd is attached.
    PoolGrant(PoolGrant),
    /// The pool asked for cannot be granted (too large); the stream's frames are dropped.
    PoolDenied {
        /// The stream.
        stream: StreamIndex,
    },
    /// Slots the parent is done with.
    Release {
        /// The pool.
        pool: u32,
        /// Slot indices.
        slots: Vec<u16>,
    },
    /// Heartbeat; the worker answers `Pong` with the same number.
    Ping {
        /// Sequence number.
        seq: u64,
    },
    /// Exit cleanly.
    Shutdown,
}

/// Why the worker does not decode a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The codec is not on the worker's allow-list.
    NotAllowed,
    /// libav has no decoder for it in this build.
    NoDecoder,
    /// The decoder did not open (bad extradata, unsupported profile).
    OpenFailed,
    /// Parameters the worker needs are missing (PCM without rate or channels).
    MissingParameters,
}

/// What the worker says about one announced stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// A decoder is open.
    Accepted,
    /// It is not decoded; its packets are dropped.
    Refused(Refusal),
}

/// The worker's counters, sent with every `Pong`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerStats {
    /// Packets taken from the ring.
    pub packets: u64,
    /// Frames handed out.
    pub frames: u64,
    /// Packets or frames libav reported an error for.
    pub decode_errors: u64,
    /// Decoded frames dropped because no slot was free in time.
    pub dropped_no_slot: u64,
    /// Packets of refused or unknown streams, dropped.
    pub dropped_packets: u64,
    /// Frames in a pixel or sample format the worker cannot hand out.
    pub unsupported_frames: u64,
    /// CPU time of the worker process, microseconds.
    pub cpu_us: u64,
}

/// A decoded picture in a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoOut {
    /// The pool.
    pub pool: u32,
    /// The slot.
    pub slot: u16,
    /// The picture; plane offsets are relative to the slot's start.
    pub desc: VideoFrameDesc,
}

/// Decoded audio in a slot: planes of `samples` f32 values, one per channel, contiguous from
/// the slot's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOut {
    /// The pool.
    pub pool: u32,
    /// The slot.
    pub slot: u16,
    /// The block.
    pub desc: AudioFrameDesc,
}

/// Something the worker attempted in a sandbox probe ([`Fault::Probe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeOp {
    /// `open("/etc/passwd", O_RDONLY)`.
    ReadPasswd,
    /// `open` of a new file for writing at the root, then in `/tmp`.
    WriteFile,
    /// `socket(AF_INET, SOCK_STREAM)`.
    Socket,
    /// `socket(AF_UNIX)` and `connect` to an abstract address.
    ConnectUnix,
    /// `execve` of the worker's own binary.
    Exec,
    /// `ptrace(PTRACE_ATTACH)` of the worker's parent (the sandbox's init, or the server).
    Ptrace,
    /// `kill(<parent's pid outside>, 0)`: whether a process outside is visible.
    SignalOutside,
    /// Network interfaces other than loopback (via `/sys/class/net`), or an outbound UDP send.
    Network,
}

/// When a probe was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeStage {
    /// After the sandbox's own layers, before the system-call filter.
    BeforeSeccomp,
    /// With the filter in place (in [`SeccompMode::Errno`]).
    AfterSeccomp,
}

/// One probe result: 0 if the operation succeeded, else the `errno` it failed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeResult {
    /// The operation.
    pub op: ProbeOp,
    /// 0: allowed (a sandbox failure); otherwise the error number.
    pub errno: i32,
}

/// Messages from the worker to the parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromWorker {
    /// First message: the worker is up (its confinement report travels separately).
    Hello {
        /// The worker's pid in its own namespace.
        pid: u32,
        /// `avcodec_version()`.
        avcodec_version: u32,
    },
    /// `Configure` was applied, libav is initialized, the system-call filter is in place.
    Ready,
    /// The outcome of opening one announced stream.
    StreamStatus {
        /// The stream.
        stream: StreamIndex,
        /// Accepted or refused.
        state: StreamState,
    },
    /// The worker needs a pool for a stream (none yet, or the frames no longer fit).
    NeedPool {
        /// The stream.
        stream: StreamIndex,
        /// Bytes per slot.
        slot_size: u64,
        /// Slots.
        slots: u16,
    },
    /// The worker no longer writes to a pool (a new one replaces it); slots the parent holds
    /// stay valid until released.
    RetirePool {
        /// The pool.
        pool: u32,
    },
    /// A decoded picture.
    Video(VideoOut),
    /// Decoded audio.
    Audio(AudioOut),
    /// The worker has read the ring up to this absolute position; the space before it is free.
    Consumed {
        /// Position.
        upto: u64,
    },
    /// Answer to `Ping`.
    Pong {
        /// The ping's number.
        seq: u64,
        /// Counters.
        stats: WorkerStats,
    },
    /// The session's `End` was processed: every frame has been sent.
    Ended,
    /// A non-fatal error worth logging.
    Error {
        /// A number for the kind of error.
        code: u16,
        /// Text (at most [`MAX_TEXT`] bytes; the parent sanitizes it).
        text: Vec<u8>,
    },
    /// Results of a sandbox probe.
    Probe {
        /// When.
        stage: ProbeStage,
        /// What was tried and what happened.
        results: Vec<ProbeResult>,
    },
}

// ---------------------------------------------------------------------------------------------
// Primitive reading and writing

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(n).ok_or(WireError::Short {
            at: self.at,
            need: n,
        })?;
        let s = self.b.get(self.at..end).ok_or(WireError::Short {
            at: self.at,
            need: end - self.b.len().min(end),
        })?;
        self.at = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    fn i64(&mut self) -> Result<i64, WireError> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    fn bool(&mut self, field: &'static str) -> Result<bool, WireError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(WireError::Value {
                field,
                value: v.into(),
            }),
        }
    }
    fn opt_i64(&mut self, field: &'static str) -> Result<Option<i64>, WireError> {
        let present = self.bool(field)?;
        let v = self.i64()?;
        Ok(present.then_some(v))
    }
    /// A count or length, refused above `max`.
    fn len16(&mut self, field: &'static str, max: usize) -> Result<usize, WireError> {
        let n = usize::from(self.u16()?);
        if n > max {
            return Err(WireError::Value {
                field,
                value: n as u64,
            });
        }
        Ok(n)
    }
    fn len32(&mut self, field: &'static str, max: usize) -> Result<usize, WireError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(WireError::Value {
                field,
                value: n as u64,
            });
        }
        Ok(n)
    }
    fn rational(&mut self, field: &'static str) -> Result<Rational, WireError> {
        let (num, den) = (self.u32()?, self.u32()?);
        Rational::new(num, den).map_err(|_| WireError::Value {
            field,
            value: (u64::from(num) << 32) | u64::from(den),
        })
    }
    fn opt_rational(&mut self, field: &'static str) -> Result<Option<Rational>, WireError> {
        let present = self.bool(field)?;
        let (num, den) = (self.u32()?, self.u32()?);
        if !present {
            return Ok(None);
        }
        Rational::new(num, den)
            .map(Some)
            .map_err(|_| WireError::Value {
                field,
                value: (u64::from(num) << 32) | u64::from(den),
            })
    }
    fn opt_u32(&mut self, field: &'static str) -> Result<Option<u32>, WireError> {
        let present = self.bool(field)?;
        let v = self.u32()?;
        Ok(present.then_some(v))
    }
    fn finish(self) -> Result<(), WireError> {
        match self.b.len() - self.at {
            0 => Ok(()),
            n => Err(WireError::Trailing(n)),
        }
    }
}

fn bad(field: &'static str, value: impl Into<u64>) -> WireError {
    WireError::Value {
        field,
        value: value.into(),
    }
}

struct Writer<'a> {
    b: &'a mut Vec<u8>,
}

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.b.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }
    fn opt_i64(&mut self, v: Option<i64>) {
        self.bool(v.is_some());
        self.i64(v.unwrap_or(0));
    }
    fn opt_u32(&mut self, v: Option<u32>) {
        self.bool(v.is_some());
        self.u32(v.unwrap_or(0));
    }
    fn rational(&mut self, r: Rational) {
        self.u32(r.num());
        self.u32(r.den());
    }
    fn opt_rational(&mut self, r: Option<Rational>) {
        self.bool(r.is_some());
        self.u32(r.map_or(0, Rational::num));
        self.u32(r.map_or(0, Rational::den));
    }
    /// A length-prefixed (u16) byte string, refused above `max`.
    fn bytes16(&mut self, field: &'static str, v: &[u8], max: usize) -> Result<(), WireError> {
        if v.len() > max {
            return Err(bad(field, v.len() as u64));
        }
        self.u16(v.len() as u16);
        self.b.extend_from_slice(v);
        Ok(())
    }
    fn bytes32(&mut self, field: &'static str, v: &[u8], max: usize) -> Result<(), WireError> {
        if v.len() > max {
            return Err(bad(field, v.len() as u64));
        }
        self.u32(v.len() as u32);
        self.b.extend_from_slice(v);
        Ok(())
    }
}

/// Writes header and payload into `out` (cleared first).
fn frame(
    out: &mut Vec<u8>,
    kind: u16,
    body: impl FnOnce(&mut Writer<'_>) -> Result<(), WireError>,
) -> Result<(), WireError> {
    out.clear();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    body(&mut Writer { b: out })?;
    let len = out.len();
    if len > MAX_MESSAGE {
        return Err(WireError::TooLarge(len));
    }
    let payload = (len - HEADER_LEN) as u32;
    out[8..12].copy_from_slice(&payload.to_le_bytes());
    Ok(())
}

/// Checks the header of a received datagram; returns its kind and a reader of the payload.
fn open(b: &[u8]) -> Result<(u16, Reader<'_>), WireError> {
    if b.len() > MAX_MESSAGE {
        return Err(WireError::TooLarge(b.len()));
    }
    let mut r = Reader { b, at: 0 };
    if r.arr::<4>()? != MAGIC {
        return Err(WireError::Magic);
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(WireError::Version(version));
    }
    let kind = r.u16()?;
    let declared = r.u32()? as usize;
    let actual = b.len() - HEADER_LEN;
    if declared != actual {
        return Err(WireError::Length { declared, actual });
    }
    Ok((kind, r))
}

// ---------------------------------------------------------------------------------------------
// Enumerations

fn codec_code(c: CodecId) -> u8 {
    match c {
        CodecId::H264 => 1,
        CodecId::Hevc => 2,
        CodecId::Av1 => 3,
        CodecId::Vp9 => 4,
        CodecId::Mpeg2Video => 5,
        CodecId::Aac => 16,
        CodecId::Ac3 => 17,
        CodecId::Eac3 => 18,
        CodecId::MpegAudio => 19,
        CodecId::Opus => 20,
        CodecId::PcmS16le => 21,
        CodecId::Text => 32,
        // tb_media::CodecId is non_exhaustive; a codec this protocol does not know is sent as
        // 0, which the worker refuses (it decodes to an error there, not to a guess)
        _ => 0,
    }
}

fn codec_from(v: u8) -> Result<CodecId, WireError> {
    Ok(match v {
        1 => CodecId::H264,
        2 => CodecId::Hevc,
        3 => CodecId::Av1,
        4 => CodecId::Vp9,
        5 => CodecId::Mpeg2Video,
        16 => CodecId::Aac,
        17 => CodecId::Ac3,
        18 => CodecId::Eac3,
        19 => CodecId::MpegAudio,
        20 => CodecId::Opus,
        21 => CodecId::PcmS16le,
        32 => CodecId::Text,
        _ => return Err(bad("codec", v)),
    })
}

/// Whether the wire protocol can name a codec (a later `tb_media::CodecId` may not be).
pub fn codec_supported(c: CodecId) -> bool {
    codec_code(c) != 0
}

fn pixfmt_code(p: PixelFormat) -> u8 {
    match p {
        PixelFormat::Yuv420p => 1,
        PixelFormat::Nv12 => 2,
        PixelFormat::P010 => 3,
    }
}

fn pixfmt_from(v: u8) -> Result<PixelFormat, WireError> {
    Ok(match v {
        1 => PixelFormat::Yuv420p,
        2 => PixelFormat::Nv12,
        3 => PixelFormat::P010,
        _ => return Err(bad("pixel format", v)),
    })
}

fn matrix_code(m: Matrix) -> u8 {
    match m {
        Matrix::Unspecified => 0,
        Matrix::Bt709 => 1,
        Matrix::Bt601 => 2,
        Matrix::Bt2020 => 3,
    }
}

fn matrix_from(v: u8) -> Result<Matrix, WireError> {
    Ok(match v {
        0 => Matrix::Unspecified,
        1 => Matrix::Bt709,
        2 => Matrix::Bt601,
        3 => Matrix::Bt2020,
        _ => return Err(bad("matrix", v)),
    })
}

fn range_code(r: Range) -> u8 {
    match r {
        Range::Unspecified => 0,
        Range::Limited => 1,
        Range::Full => 2,
    }
}

fn range_from(v: u8) -> Result<Range, WireError> {
    Ok(match v {
        0 => Range::Unspecified,
        1 => Range::Limited,
        2 => Range::Full,
        _ => return Err(bad("range", v)),
    })
}

fn seccomp_code(m: SeccompMode) -> u8 {
    match m {
        SeccompMode::Off => 0,
        SeccompMode::Errno => 1,
        SeccompMode::Kill => 2,
    }
}

fn seccomp_from(v: u8) -> Result<SeccompMode, WireError> {
    Ok(match v {
        0 => SeccompMode::Off,
        1 => SeccompMode::Errno,
        2 => SeccompMode::Kill,
        _ => return Err(bad("seccomp mode", v)),
    })
}

fn fault_code(f: Fault) -> (u8, u32) {
    match f {
        Fault::None => (0, 0),
        Fault::CrashAfter(n) => (1, n),
        Fault::HangAfter(n) => (2, n),
        Fault::BadDescriptorAfter(n) => (3, n),
        Fault::AllocAfter(n) => (4, n),
        Fault::SpinAfter(n) => (5, n),
        Fault::Probe => (6, 0),
    }
}

fn fault_from(k: u8, n: u32) -> Result<Fault, WireError> {
    Ok(match k {
        0 => Fault::None,
        1 => Fault::CrashAfter(n),
        2 => Fault::HangAfter(n),
        3 => Fault::BadDescriptorAfter(n),
        4 => Fault::AllocAfter(n),
        5 => Fault::SpinAfter(n),
        6 => Fault::Probe,
        _ => return Err(bad("fault", k)),
    })
}

fn refusal_code(r: Refusal) -> u8 {
    match r {
        Refusal::NotAllowed => 1,
        Refusal::NoDecoder => 2,
        Refusal::OpenFailed => 3,
        Refusal::MissingParameters => 4,
    }
}

fn state_from(v: u8) -> Result<StreamState, WireError> {
    Ok(match v {
        0 => StreamState::Accepted,
        1 => StreamState::Refused(Refusal::NotAllowed),
        2 => StreamState::Refused(Refusal::NoDecoder),
        3 => StreamState::Refused(Refusal::OpenFailed),
        4 => StreamState::Refused(Refusal::MissingParameters),
        _ => return Err(bad("stream state", v)),
    })
}

/// The wire numbers of the probe operations, in order.
const PROBE_OPS: [ProbeOp; 8] = [
    ProbeOp::ReadPasswd,
    ProbeOp::WriteFile,
    ProbeOp::Socket,
    ProbeOp::ConnectUnix,
    ProbeOp::Exec,
    ProbeOp::Ptrace,
    ProbeOp::SignalOutside,
    ProbeOp::Network,
];

fn probe_code(op: ProbeOp) -> u8 {
    PROBE_OPS.iter().position(|o| *o == op).unwrap_or(0) as u8
}

// ---------------------------------------------------------------------------------------------
// Composite fields

fn put_stream(w: &mut Writer<'_>, s: &StreamInfo) -> Result<(), WireError> {
    w.u16(s.index.0);
    let c = codec_code(s.codec);
    if c == 0 {
        return Err(bad("codec", 0u8));
    }
    w.u8(c);
    w.rational(s.time_base);
    w.bytes32("extradata", &s.extradata, MAX_EXTRADATA)?;
    w.opt_u32(s.video.width);
    w.opt_u32(s.video.height);
    w.opt_rational(s.video.frame_rate);
    w.opt_u32(s.audio.sample_rate);
    w.bool(s.audio.channels.is_some());
    w.u8(s.audio.channels.unwrap_or(0));
    w.bool(s.language.is_some());
    w.b.extend_from_slice(&s.language.unwrap_or([0; 3]));
    Ok(())
}

fn get_stream(r: &mut Reader<'_>) -> Result<StreamInfo, WireError> {
    let index = StreamIndex(r.u16()?);
    let codec = codec_from(r.u8()?)?;
    let time_base = r.rational("time base")?;
    let n = r.len32("extradata", MAX_EXTRADATA)?;
    let extradata = r.take(n)?.to_vec();
    let video = VideoHint {
        width: r.opt_u32("width")?,
        height: r.opt_u32("height")?,
        frame_rate: r.opt_rational("frame rate")?,
    };
    let sample_rate = r.opt_u32("sample rate")?;
    let has_ch = r.bool("channels")?;
    let ch = r.u8()?;
    let has_lang = r.bool("language")?;
    let lang = r.arr::<3>()?;
    Ok(StreamInfo {
        index,
        codec,
        time_base,
        extradata,
        video,
        audio: AudioHint {
            sample_rate,
            channels: has_ch.then_some(ch),
        },
        language: has_lang.then_some(lang),
    })
}

fn put_output(w: &mut Writer<'_>, o: &OutputSpec) -> Result<(), WireError> {
    match o {
        OutputSpec::Native => w.u8(0),
        OutputSpec::Video(v) => {
            w.u8(1);
            w.u32(v.width);
            w.u32(v.height);
            w.rational(v.display_aspect);
            w.u8(match v.aspect {
                Aspect::Stretch => 0,
                Aspect::Fit => 1,
            });
            w.u8(match v.deinterlace {
                Deinterlace::Off => 0,
                Deinterlace::Bob => 1,
            });
        }
        OutputSpec::Audio(a) => {
            w.u8(2);
            w.u8(match a.layout {
                AudioLayout::Stereo => 0,
                AudioLayout::Surround51 => 1,
            });
        }
    }
    Ok(())
}

fn get_output(r: &mut Reader<'_>) -> Result<OutputSpec, WireError> {
    Ok(match r.u8()? {
        0 => OutputSpec::Native,
        1 => {
            let width = r.u32()?;
            let height = r.u32()?;
            for (f, v) in [("target width", width), ("target height", height)] {
                if v == 0 || v % 2 != 0 || v > MAX_TARGET {
                    return Err(bad(f, v));
                }
            }
            let display_aspect = r.rational("display aspect")?;
            let aspect = match r.u8()? {
                0 => Aspect::Stretch,
                1 => Aspect::Fit,
                v => return Err(bad("aspect", v)),
            };
            let deinterlace = match r.u8()? {
                0 => Deinterlace::Off,
                1 => Deinterlace::Bob,
                v => return Err(bad("deinterlace", v)),
            };
            OutputSpec::Video(VideoSpec {
                width,
                height,
                display_aspect,
                aspect,
                deinterlace,
            })
        }
        2 => OutputSpec::Audio(AudioSpec {
            layout: match r.u8()? {
                0 => AudioLayout::Stereo,
                1 => AudioLayout::Surround51,
                v => return Err(bad("layout", v)),
            },
        }),
        v => return Err(bad("output", v)),
    })
}

fn put_video(w: &mut Writer<'_>, d: &VideoFrameDesc) -> Result<(), WireError> {
    w.u16(d.stream.0);
    w.opt_i64(d.pts);
    w.u32(d.width);
    w.u32(d.height);
    w.u8(pixfmt_code(d.format));
    if d.planes.len() > 4 {
        return Err(bad("planes", d.planes.len() as u64));
    }
    w.u8(d.planes.len() as u8);
    for p in &d.planes {
        w.u32(p.offset);
        w.u32(p.stride);
    }
    w.bool(d.interlaced);
    w.bool(d.top_field_first);
    w.u8(matrix_code(d.matrix));
    w.u8(range_code(d.range));
    w.opt_rational(d.sample_aspect);
    w.bytes16("cc_data", &d.cc_data, MAX_CC)?;
    w.bool(d.corrupt);
    w.bool(d.active.is_some());
    let (aw, ah) = d.active.unwrap_or((0, 0));
    w.u32(aw);
    w.u32(ah);
    Ok(())
}

fn get_video(r: &mut Reader<'_>) -> Result<VideoFrameDesc, WireError> {
    let stream = StreamIndex(r.u16()?);
    let pts = r.opt_i64("pts")?;
    let width = r.u32()?;
    let height = r.u32()?;
    let format = pixfmt_from(r.u8()?)?;
    let np = usize::from(r.u8()?);
    if np > 4 {
        return Err(bad("planes", np as u64));
    }
    let mut planes = Vec::with_capacity(np);
    for _ in 0..np {
        planes.push(Plane {
            offset: r.u32()?,
            stride: r.u32()?,
        });
    }
    let interlaced = r.bool("interlaced")?;
    let top_field_first = r.bool("top field first")?;
    let matrix = matrix_from(r.u8()?)?;
    let range = range_from(r.u8()?)?;
    let sample_aspect = r.opt_rational("sample aspect")?;
    let n = r.len16("cc_data", MAX_CC)?;
    let cc_data = r.take(n)?.to_vec();
    let corrupt = r.bool("corrupt")?;
    let has_active = r.bool("active")?;
    let (aw, ah) = (r.u32()?, r.u32()?);
    let active = has_active.then_some((aw, ah));
    Ok(VideoFrameDesc {
        stream,
        pts,
        width,
        height,
        format,
        planes,
        interlaced,
        top_field_first,
        matrix,
        range,
        sample_aspect,
        cc_data,
        corrupt,
        active,
    })
}

// ---------------------------------------------------------------------------------------------
// Messages to the worker

mod kind {
    pub const CONFIGURE: u16 = 1;
    pub const STREAMS: u16 = 2;
    pub const PACKET: u16 = 3;
    pub const END: u16 = 4;
    pub const POOL_GRANT: u16 = 5;
    pub const POOL_DENIED: u16 = 6;
    pub const RELEASE: u16 = 7;
    pub const PING: u16 = 8;
    pub const SHUTDOWN: u16 = 9;

    pub const HELLO: u16 = 64;
    pub const READY: u16 = 65;
    pub const STREAM_STATUS: u16 = 66;
    pub const NEED_POOL: u16 = 67;
    pub const RETIRE_POOL: u16 = 68;
    pub const VIDEO: u16 = 69;
    pub const AUDIO: u16 = 70;
    pub const CONSUMED: u16 = 71;
    pub const PONG: u16 = 72;
    pub const ENDED: u16 = 73;
    pub const ERROR: u16 = 74;
    pub const PROBE: u16 = 75;
}

impl ToWorker {
    /// Encodes the message into `out` (which is cleared first).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            Self::Configure(c) => frame(out, kind::CONFIGURE, |w| {
                w.u64(c.input_ring_len);
                w.u8(c.threads);
                w.bool(c.gpu);
                w.u8(seccomp_code(c.seccomp));
                w.u16(c.video_slots);
                w.u16(c.audio_slots);
                w.u32(c.slot_wait_ms);
                w.u64(c.max_frame_bytes);
                let (k, n) = fault_code(c.fault);
                w.u8(k);
                w.u32(n);
                Ok(())
            }),
            Self::Streams(list) => frame(out, kind::STREAMS, |w| {
                if list.len() > MAX_STREAMS {
                    return Err(bad("streams", list.len() as u64));
                }
                w.u16(list.len() as u16);
                list.iter().try_for_each(|s| {
                    put_stream(w, &s.info)?;
                    put_output(w, &s.output)
                })
            }),
            Self::Packet(p) => frame(out, kind::PACKET, |w| {
                w.u16(p.stream.0);
                w.opt_i64(p.pts);
                w.opt_i64(p.dts);
                w.opt_i64(p.duration);
                w.bool(p.keyframe);
                w.u64(p.pos);
                w.u32(p.len);
                Ok(())
            }),
            Self::End => frame(out, kind::END, |_| Ok(())),
            Self::PoolGrant(g) => frame(out, kind::POOL_GRANT, |w| {
                w.u32(g.pool);
                w.u16(g.stream.0);
                w.u64(g.slot_size);
                w.u16(g.slots);
                Ok(())
            }),
            Self::PoolDenied { stream } => frame(out, kind::POOL_DENIED, |w| {
                w.u16(stream.0);
                Ok(())
            }),
            Self::Release { pool, slots } => frame(out, kind::RELEASE, |w| {
                if slots.len() > MAX_RELEASE {
                    return Err(bad("release", slots.len() as u64));
                }
                w.u32(*pool);
                w.u16(slots.len() as u16);
                slots.iter().for_each(|s| w.u16(*s));
                Ok(())
            }),
            Self::Ping { seq } => frame(out, kind::PING, |w| {
                w.u64(*seq);
                Ok(())
            }),
            Self::Shutdown => frame(out, kind::SHUTDOWN, |_| Ok(())),
        }
    }

    /// Decodes one datagram.
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let (k, mut r) = open(b)?;
        let m = match k {
            kind::CONFIGURE => Self::Configure(Configure {
                input_ring_len: r.u64()?,
                threads: r.u8()?,
                gpu: r.bool("gpu")?,
                seccomp: seccomp_from(r.u8()?)?,
                video_slots: r.u16()?,
                audio_slots: r.u16()?,
                slot_wait_ms: r.u32()?,
                max_frame_bytes: r.u64()?,
                fault: {
                    let k = r.u8()?;
                    fault_from(k, r.u32()?)?
                },
            }),
            kind::STREAMS => {
                let n = r.len16("streams", MAX_STREAMS)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    let info = get_stream(&mut r)?;
                    let output = get_output(&mut r)?;
                    v.push(StreamOpen { info, output });
                }
                Self::Streams(v)
            }
            kind::PACKET => Self::Packet(PacketRef {
                stream: StreamIndex(r.u16()?),
                pts: r.opt_i64("pts")?,
                dts: r.opt_i64("dts")?,
                duration: r.opt_i64("duration")?,
                keyframe: r.bool("keyframe")?,
                pos: r.u64()?,
                len: r.u32()?,
            }),
            kind::END => Self::End,
            kind::POOL_GRANT => Self::PoolGrant(PoolGrant {
                pool: r.u32()?,
                stream: StreamIndex(r.u16()?),
                slot_size: r.u64()?,
                slots: r.u16()?,
            }),
            kind::POOL_DENIED => Self::PoolDenied {
                stream: StreamIndex(r.u16()?),
            },
            kind::RELEASE => {
                let pool = r.u32()?;
                let n = r.len16("release", MAX_RELEASE)?;
                let mut slots = Vec::with_capacity(n);
                for _ in 0..n {
                    slots.push(r.u16()?);
                }
                Self::Release { pool, slots }
            }
            kind::PING => Self::Ping { seq: r.u64()? },
            kind::SHUTDOWN => Self::Shutdown,
            k => return Err(WireError::Kind(k)),
        };
        r.finish()?;
        Ok(m)
    }
}

// ---------------------------------------------------------------------------------------------
// Messages from the worker

impl FromWorker {
    /// Encodes the message into `out` (which is cleared first).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError> {
        match self {
            Self::Hello {
                pid,
                avcodec_version,
            } => frame(out, kind::HELLO, |w| {
                w.u32(*pid);
                w.u32(*avcodec_version);
                Ok(())
            }),
            Self::Ready => frame(out, kind::READY, |_| Ok(())),
            Self::StreamStatus { stream, state } => frame(out, kind::STREAM_STATUS, |w| {
                w.u16(stream.0);
                w.u8(match state {
                    StreamState::Accepted => 0,
                    StreamState::Refused(r) => refusal_code(*r),
                });
                Ok(())
            }),
            Self::NeedPool {
                stream,
                slot_size,
                slots,
            } => frame(out, kind::NEED_POOL, |w| {
                w.u16(stream.0);
                w.u64(*slot_size);
                w.u16(*slots);
                Ok(())
            }),
            Self::RetirePool { pool } => frame(out, kind::RETIRE_POOL, |w| {
                w.u32(*pool);
                Ok(())
            }),
            Self::Video(v) => frame(out, kind::VIDEO, |w| {
                w.u32(v.pool);
                w.u16(v.slot);
                put_video(w, &v.desc)
            }),
            Self::Audio(a) => frame(out, kind::AUDIO, |w| {
                w.u32(a.pool);
                w.u16(a.slot);
                w.u16(a.desc.stream.0);
                w.opt_i64(a.desc.pts);
                w.u32(a.desc.sample_rate);
                w.u8(a.desc.channels);
                w.u32(a.desc.samples);
                Ok(())
            }),
            Self::Consumed { upto } => frame(out, kind::CONSUMED, |w| {
                w.u64(*upto);
                Ok(())
            }),
            Self::Pong { seq, stats } => frame(out, kind::PONG, |w| {
                w.u64(*seq);
                for v in [
                    stats.packets,
                    stats.frames,
                    stats.decode_errors,
                    stats.dropped_no_slot,
                    stats.dropped_packets,
                    stats.unsupported_frames,
                    stats.cpu_us,
                ] {
                    w.u64(v);
                }
                Ok(())
            }),
            Self::Ended => frame(out, kind::ENDED, |_| Ok(())),
            Self::Error { code, text } => frame(out, kind::ERROR, |w| {
                w.u16(*code);
                w.bytes16("text", text, MAX_TEXT)
            }),
            Self::Probe { stage, results } => frame(out, kind::PROBE, |w| {
                w.u8(match stage {
                    ProbeStage::BeforeSeccomp => 0,
                    ProbeStage::AfterSeccomp => 1,
                });
                if results.len() > MAX_PROBE {
                    return Err(bad("probe results", results.len() as u64));
                }
                w.u8(results.len() as u8);
                for p in results {
                    w.u8(probe_code(p.op));
                    w.i32(p.errno);
                }
                Ok(())
            }),
        }
    }

    /// Decodes one datagram. Every field is range-checked; semantic checks against the
    /// parent's state are [`crate::pool`]'s.
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let (k, mut r) = open(b)?;
        let m = match k {
            kind::HELLO => Self::Hello {
                pid: r.u32()?,
                avcodec_version: r.u32()?,
            },
            kind::READY => Self::Ready,
            kind::STREAM_STATUS => Self::StreamStatus {
                stream: StreamIndex(r.u16()?),
                state: state_from(r.u8()?)?,
            },
            kind::NEED_POOL => Self::NeedPool {
                stream: StreamIndex(r.u16()?),
                slot_size: r.u64()?,
                slots: r.u16()?,
            },
            kind::RETIRE_POOL => Self::RetirePool { pool: r.u32()? },
            kind::VIDEO => Self::Video(VideoOut {
                pool: r.u32()?,
                slot: r.u16()?,
                desc: get_video(&mut r)?,
            }),
            kind::AUDIO => Self::Audio(AudioOut {
                pool: r.u32()?,
                slot: r.u16()?,
                desc: AudioFrameDesc {
                    stream: StreamIndex(r.u16()?),
                    pts: r.opt_i64("pts")?,
                    sample_rate: r.u32()?,
                    channels: r.u8()?,
                    samples: r.u32()?,
                },
            }),
            kind::CONSUMED => Self::Consumed { upto: r.u64()? },
            kind::PONG => Self::Pong {
                seq: r.u64()?,
                stats: WorkerStats {
                    packets: r.u64()?,
                    frames: r.u64()?,
                    decode_errors: r.u64()?,
                    dropped_no_slot: r.u64()?,
                    dropped_packets: r.u64()?,
                    unsupported_frames: r.u64()?,
                    cpu_us: r.u64()?,
                },
            },
            kind::ENDED => Self::Ended,
            kind::ERROR => {
                let code = r.u16()?;
                let n = r.len16("text", MAX_TEXT)?;
                Self::Error {
                    code,
                    text: r.take(n)?.to_vec(),
                }
            }
            kind::PROBE => {
                let stage = match r.u8()? {
                    0 => ProbeStage::BeforeSeccomp,
                    1 => ProbeStage::AfterSeccomp,
                    v => return Err(bad("probe stage", v)),
                };
                let n = usize::from(r.u8()?);
                if n > MAX_PROBE {
                    return Err(bad("probe results", n as u64));
                }
                let mut results = Vec::with_capacity(n);
                for _ in 0..n {
                    let c = usize::from(r.u8()?);
                    let op = *PROBE_OPS.get(c).ok_or(bad("probe op", c as u64))?;
                    results.push(ProbeResult {
                        op,
                        errno: r.i32()?,
                    });
                }
                Self::Probe { stage, results }
            }
            k => return Err(WireError::Kind(k)),
        };
        r.finish()?;
        Ok(m)
    }

    /// A short name of the message's kind, for logs and violations.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "Hello",
            Self::Ready => "Ready",
            Self::StreamStatus { .. } => "StreamStatus",
            Self::NeedPool { .. } => "NeedPool",
            Self::RetirePool { .. } => "RetirePool",
            Self::Video(_) => "Video",
            Self::Audio(_) => "Audio",
            Self::Consumed { .. } => "Consumed",
            Self::Pong { .. } => "Pong",
            Self::Ended => "Ended",
            Self::Error { .. } => "Error",
            Self::Probe { .. } => "Probe",
        }
    }
}

/// Printable ASCII of untrusted text, at most [`MAX_TEXT`] characters; anything else is
/// replaced by `?`.
pub fn sanitize(text: &[u8]) -> String {
    text.iter()
        .take(MAX_TEXT)
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> StreamInfo {
        StreamInfo {
            index: StreamIndex(1),
            codec: CodecId::H264,
            time_base: Rational::MILLIS,
            extradata: vec![1, 2, 3, 4],
            video: VideoHint {
                width: Some(1920),
                height: None,
                frame_rate: Some(Rational::new(30000, 1001).unwrap()),
            },
            audio: AudioHint {
                sample_rate: None,
                channels: Some(2),
            },
            language: Some(*b"eng"),
        }
    }

    fn video() -> VideoFrameDesc {
        VideoFrameDesc {
            stream: StreamIndex(0),
            pts: Some(-3),
            width: 1920,
            height: 1080,
            format: PixelFormat::Yuv420p,
            planes: vec![
                Plane {
                    offset: 0,
                    stride: 1920,
                },
                Plane {
                    offset: 1920 * 1080,
                    stride: 960,
                },
                Plane {
                    offset: 1920 * 1080 * 5 / 4,
                    stride: 960,
                },
            ],
            interlaced: true,
            top_field_first: true,
            matrix: Matrix::Bt709,
            range: Range::Limited,
            sample_aspect: Some(Rational::new(1, 1).unwrap()),
            cc_data: vec![0xfc, 0x80, 0x80],
            corrupt: false,
            active: Some((640, 360)),
        }
    }

    fn all_to_worker() -> Vec<ToWorker> {
        vec![
            ToWorker::Configure(Configure {
                input_ring_len: 1 << 23,
                threads: 2,
                gpu: false,
                seccomp: SeccompMode::Kill,
                video_slots: 8,
                audio_slots: 16,
                slot_wait_ms: 20,
                max_frame_bytes: 1 << 26,
                fault: Fault::CrashAfter(7),
            }),
            ToWorker::Streams(vec![
                StreamOpen {
                    info: stream(),
                    output: OutputSpec::Native,
                },
                StreamOpen {
                    info: stream(),
                    output: OutputSpec::Video(VideoSpec {
                        width: 1920,
                        height: 1080,
                        display_aspect: Rational::new(16, 9).unwrap(),
                        aspect: Aspect::Fit,
                        deinterlace: Deinterlace::Bob,
                    }),
                },
                StreamOpen {
                    info: stream(),
                    output: OutputSpec::Audio(AudioSpec {
                        layout: AudioLayout::Surround51,
                    }),
                },
            ]),
            ToWorker::Packet(PacketRef {
                stream: StreamIndex(0),
                pts: Some(i64::MIN),
                dts: None,
                duration: Some(33),
                keyframe: true,
                pos: u64::MAX - 5,
                len: 99,
            }),
            ToWorker::End,
            ToWorker::PoolGrant(PoolGrant {
                pool: 3,
                stream: StreamIndex(0),
                slot_size: 3_110_400,
                slots: 8,
            }),
            ToWorker::PoolDenied {
                stream: StreamIndex(4),
            },
            ToWorker::Release {
                pool: 3,
                slots: vec![1, 2, 7],
            },
            ToWorker::Ping { seq: 42 },
            ToWorker::Shutdown,
        ]
    }

    fn all_from_worker() -> Vec<FromWorker> {
        vec![
            FromWorker::Hello {
                pid: 2,
                avcodec_version: 0x3d_0100,
            },
            FromWorker::Ready,
            FromWorker::StreamStatus {
                stream: StreamIndex(1),
                state: StreamState::Refused(Refusal::NotAllowed),
            },
            FromWorker::StreamStatus {
                stream: StreamIndex(0),
                state: StreamState::Accepted,
            },
            FromWorker::NeedPool {
                stream: StreamIndex(0),
                slot_size: 1 << 22,
                slots: 8,
            },
            FromWorker::RetirePool { pool: 1 },
            FromWorker::Video(VideoOut {
                pool: 1,
                slot: 2,
                desc: video(),
            }),
            FromWorker::Audio(AudioOut {
                pool: 2,
                slot: 0,
                desc: AudioFrameDesc {
                    stream: StreamIndex(1),
                    pts: None,
                    sample_rate: 48000,
                    channels: 2,
                    samples: 1024,
                },
            }),
            FromWorker::Consumed { upto: 12345 },
            FromWorker::Pong {
                seq: 9,
                stats: WorkerStats {
                    packets: 1,
                    frames: 2,
                    decode_errors: 3,
                    dropped_no_slot: 4,
                    dropped_packets: 5,
                    unsupported_frames: 6,
                    cpu_us: 7,
                },
            },
            FromWorker::Ended,
            FromWorker::Error {
                code: 3,
                text: b"bad \x1b[31m".to_vec(),
            },
            FromWorker::Probe {
                stage: ProbeStage::AfterSeccomp,
                results: vec![
                    ProbeResult {
                        op: ProbeOp::Socket,
                        errno: 1,
                    },
                    ProbeResult {
                        op: ProbeOp::Network,
                        errno: 0,
                    },
                ],
            },
        ]
    }

    #[test]
    fn round_trips_both_directions() {
        let mut buf = Vec::new();
        for m in all_to_worker() {
            m.encode(&mut buf).unwrap();
            assert_eq!(ToWorker::decode(&buf).unwrap(), m);
            // the other direction does not know the kind
            assert!(matches!(FromWorker::decode(&buf), Err(WireError::Kind(_))));
        }
        for m in all_from_worker() {
            m.encode(&mut buf).unwrap();
            assert_eq!(FromWorker::decode(&buf).unwrap(), m, "{}", m.name());
            assert!(matches!(ToWorker::decode(&buf), Err(WireError::Kind(_))));
        }
    }

    #[test]
    fn every_truncation_and_extension_is_refused() {
        let mut buf = Vec::new();
        for m in all_from_worker() {
            m.encode(&mut buf).unwrap();
            for cut in 0..buf.len() {
                assert!(
                    FromWorker::decode(&buf[..cut]).is_err(),
                    "{} cut {cut}",
                    m.name()
                );
            }
            let mut longer = buf.clone();
            longer.push(0);
            assert!(FromWorker::decode(&longer).is_err());
            // the header's length made consistent with the extra byte: trailing data
            let n = (longer.len() - HEADER_LEN) as u32;
            longer[8..12].copy_from_slice(&n.to_le_bytes());
            assert_eq!(FromWorker::decode(&longer), Err(WireError::Trailing(1)));
        }
    }

    #[test]
    fn header_checks() {
        let mut buf = Vec::new();
        FromWorker::Ready.encode(&mut buf).unwrap();
        let mut b = buf.clone();
        b[0] = b'X';
        assert_eq!(FromWorker::decode(&b), Err(WireError::Magic));
        let mut b = buf.clone();
        b[4] = (VERSION + 1) as u8;
        assert_eq!(FromWorker::decode(&b), Err(WireError::Version(VERSION + 1)));
        assert!(FromWorker::decode(&vec![0; MAX_MESSAGE + 1]).is_err());
    }

    #[test]
    fn field_ranges() {
        let mut buf = Vec::new();
        // a boolean that is neither 0 nor 1
        FromWorker::Video(VideoOut {
            pool: 1,
            slot: 2,
            desc: video(),
        })
        .encode(&mut buf)
        .unwrap();
        let pts_flag = HEADER_LEN + 4 + 2 + 2;
        buf[pts_flag] = 2;
        assert!(matches!(
            FromWorker::decode(&buf),
            Err(WireError::Value { field: "pts", .. })
        ));
        // a count above its bound
        let mut d = video();
        d.cc_data = vec![0; MAX_CC + 3];
        assert!(
            FromWorker::Video(VideoOut {
                pool: 0,
                slot: 0,
                desc: d
            })
            .encode(&mut buf)
            .is_err()
        );
        // a zero rational
        let mut s = stream();
        s.video.frame_rate = None;
        ToWorker::Streams(vec![StreamOpen {
            info: s,
            output: OutputSpec::Native,
        }])
        .encode(&mut buf)
        .unwrap();
        let tb = HEADER_LEN + 2 + 2 + 1;
        buf[tb..tb + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            ToWorker::decode(&buf),
            Err(WireError::Value {
                field: "time base",
                ..
            })
        ));
    }

    #[test]
    fn oversize_messages_are_refused_at_encoding() {
        let mut s = stream();
        s.extradata = vec![0; MAX_EXTRADATA];
        let mut buf = Vec::new();
        assert!(matches!(
            ToWorker::Streams(vec![
                StreamOpen {
                    info: s,
                    output: OutputSpec::Native
                };
                3
            ])
            .encode(&mut buf),
            Err(WireError::TooLarge(_))
        ));
    }

    #[test]
    fn sanitize_text() {
        assert_eq!(sanitize(b"ok \x1b[0m\n"), "ok ?[0m?");
        assert_eq!(sanitize(&[b'a'; 1000]).len(), MAX_TEXT);
    }
}
