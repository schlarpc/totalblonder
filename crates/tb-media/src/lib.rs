//! The plain-data contract of the ingest path (docs/SERVER.md).
//!
//! ```text
//!  network ──► receiver (pure, Rust) ──► InputEvent ──► decoder worker (libav, sandboxed
//!              RTMP, RTP/UDP MPEG-TS,                   child process) ──► decoded frames
//!              SRT, files ...                                              (shared memory)
//! ```
//!
//! A *receiver* is a sans-io protocol state machine that turns the bytes of one producer
//! connection into [`InputEvent`]s: the set of elementary streams ([`StreamInfo`]), then
//! compressed [`Packet`]s with the producer's timestamps, then an end. Containers are parsed
//! in Rust; only codecs are left to libav, which runs in a separate, sandboxed process that
//! sees nothing but these events. The worker hands back decoded frames described by
//! [`VideoFrameDesc`] / [`AudioFrameDesc`] (the pixels and samples travel in shared memory;
//! the descriptors say where and what they are).
//!
//! Everything here is plain data: no I/O, no allocation policy, no libav types.
#![forbid(unsafe_code)]

use std::fmt;

/// A positive rational number, used for time bases (seconds per tick) and frame rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    num: u32,
    den: u32,
}

/// A rational with a zero term.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rational {num}/{den} has a zero term")]
pub struct ZeroRational {
    /// Numerator given.
    pub num: u32,
    /// Denominator given.
    pub den: u32,
}

impl Rational {
    /// `num / den`, reduced. Both must be non-zero.
    pub const fn new(num: u32, den: u32) -> Result<Self, ZeroRational> {
        if num == 0 || den == 0 {
            return Err(ZeroRational { num, den });
        }
        let g = gcd(num, den);
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }
    /// Numerator (reduced).
    pub const fn num(self) -> u32 {
        self.num
    }
    /// Denominator (reduced).
    pub const fn den(self) -> u32 {
        self.den
    }
    /// 1/1000 s: RTMP and FLV timestamps.
    pub const MILLIS: Self = Self { num: 1, den: 1000 };
    /// 1/90000 s: MPEG-2 systems PTS and DTS, RTP video clocks.
    pub const MPEG_90K: Self = Self {
        num: 1,
        den: 90_000,
    };
    /// 1/27000000 s: the system clock of MPEG-2 systems.
    pub const MPEG_27M: Self = Self {
        num: 1,
        den: 27_000_000,
    };

    /// `t` ticks of this time base in ticks of `to`, rounded to nearest (ties away from zero).
    /// Exact 128-bit arithmetic; saturates at the `i64` range.
    pub fn rescale(self, t: i64, to: Rational) -> i64 {
        let n = i128::from(t) * i128::from(self.num) * i128::from(to.den);
        let d = i128::from(self.den) * i128::from(to.num);
        let q = if n >= 0 {
            (n + d / 2) / d
        } else {
            (n - d / 2) / d
        };
        i64::try_from(q).unwrap_or(if q < 0 { i64::MIN } else { i64::MAX })
    }
}

const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

/// Video or audio (or data such as timed text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Pictures.
    Video,
    /// Sound.
    Audio,
    /// Timed text, captions, metadata.
    Data,
}

/// The codecs a receiver can announce. The worker maps each to a libav decoder; a codec the
/// worker does not allow is refused there, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodecId {
    /// H.264 / AVC. Packets are length-prefixed NAL units when the stream's extradata is an
    /// `AVCDecoderConfigurationRecord` (FLV, MP4), Annex B start codes otherwise (MPEG-TS).
    H264,
    /// H.265 / HEVC, the same two framings (`HEVCDecoderConfigurationRecord` or Annex B).
    Hevc,
    /// AV1 (`AV1CodecConfigurationRecord`, low-overhead OBUs).
    Av1,
    /// VP9.
    Vp9,
    /// MPEG-2 video (H.262), elementary stream.
    Mpeg2Video,
    /// AAC. Raw access units with an `AudioSpecificConfig` as extradata (FLV, MP4), or ADTS
    /// frames with no extradata (MPEG-TS).
    Aac,
    /// AC-3 (A/52), sync frames.
    Ac3,
    /// E-AC-3 (A/52 Annex E), sync frames.
    Eac3,
    /// MPEG-1/2 audio layer II or III.
    MpegAudio,
    /// Opus.
    Opus,
    /// Linear PCM, signed 16-bit little-endian interleaved.
    PcmS16le,
    /// Timed text in UTF-8, one cue per packet (the packet's duration is the cue's).
    Text,
}

impl CodecId {
    /// What kind of media the codec carries.
    pub const fn kind(self) -> MediaKind {
        match self {
            Self::H264 | Self::Hevc | Self::Av1 | Self::Vp9 | Self::Mpeg2Video => MediaKind::Video,
            Self::Aac | Self::Ac3 | Self::Eac3 | Self::MpegAudio | Self::Opus | Self::PcmS16le => {
                MediaKind::Audio
            }
            Self::Text => MediaKind::Data,
        }
    }
}

/// Parameters a receiver knows about a video stream before decoding (may be absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoHint {
    /// Coded width, if announced.
    pub width: Option<u32>,
    /// Coded height, if announced.
    pub height: Option<u32>,
    /// Frame rate, if announced.
    pub frame_rate: Option<Rational>,
}

/// Parameters a receiver knows about an audio stream before decoding (may be absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioHint {
    /// Sampling rate in Hz, if announced.
    pub sample_rate: Option<u32>,
    /// Channel count, if announced.
    pub channels: Option<u8>,
}

/// Identifies an elementary stream within one input session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamIndex(pub u16);

/// One elementary stream of an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// The stream's index within the session; packets refer to it.
    pub index: StreamIndex,
    /// The codec.
    pub codec: CodecId,
    /// Unit of the packets' timestamps.
    pub time_base: Rational,
    /// Codec configuration (for example an `AVCDecoderConfigurationRecord`), empty if none.
    pub extradata: Vec<u8>,
    /// Video parameters announced by the container.
    pub video: VideoHint,
    /// Audio parameters announced by the container.
    pub audio: AudioHint,
    /// ISO 639-2 language code if the container gives one.
    pub language: Option<[u8; 3]>,
}

/// One compressed access unit (or a fragment the codec's parser can join, for MPEG-TS input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// The stream it belongs to.
    pub stream: StreamIndex,
    /// Presentation time in the stream's time base, if known.
    pub pts: Option<i64>,
    /// Decoding time in the stream's time base, if known.
    pub dts: Option<i64>,
    /// Duration in the stream's time base, if known.
    pub duration: Option<i64>,
    /// A random access point (key frame).
    pub keyframe: bool,
    /// The payload, in the framing [`CodecId`] describes.
    pub data: Vec<u8>,
}

/// Why an input session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// The producer closed the connection or sent an end-of-stream command.
    Closed,
    /// The transport failed (reset, timeout).
    Transport(String),
    /// The producer violated the protocol; the text says how.
    Protocol(String),
    /// Our side ended it (shutdown, reconfiguration, a limit).
    Local(String),
}

/// What a receiver emits, in order. A session is `Streams`, then any number of `Packet`s
/// (and possibly further `Streams` when the producer changes codec parameters mid-session,
/// which the decoder treats as a reconfiguration), then `End`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// The session's elementary streams, complete. Replaces any earlier set.
    Streams(Vec<StreamInfo>),
    /// One packet of a stream announced by the last `Streams`.
    Packet(Packet),
    /// The session is over; the next event, if any, starts a new session.
    End(EndReason),
}

/// Pixel layouts the decoder hands back in host memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Planar Y, U, V, 8 bits, chroma halved both ways.
    Yuv420p,
    /// Y plane, then interleaved U/V plane, 8 bits, 4:2:0.
    Nv12,
    /// As NV12 with 16-bit little-endian samples, value in the high 10 bits.
    P010,
}

/// Color matrix, as the decoder reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Matrix {
    /// ITU-R BT.709.
    Bt709,
    /// BT.601 (SMPTE 170M / BT.470 BG).
    Bt601,
    /// BT.2020 non-constant luminance.
    Bt2020,
    /// The stream did not say.
    Unspecified,
}

/// Video sample range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Range {
    /// 16–235 (8 bit).
    Limited,
    /// 0–255.
    Full,
    /// The stream did not say.
    Unspecified,
}

/// One plane of a decoded picture inside a shared buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    /// Byte offset of the plane's first row from the start of the frame's buffer.
    pub offset: u32,
    /// Bytes from one row to the next.
    pub stride: u32,
}

/// A decoded picture as the worker describes it. The buffer it refers to is the transport's
/// business (a slot of a shared-memory ring).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrameDesc {
    /// The stream it was decoded from.
    pub stream: StreamIndex,
    /// Presentation time in the stream's time base (best effort, as libav estimates it).
    pub pts: Option<i64>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Layout.
    pub format: PixelFormat,
    /// Planes in order (Y, U, V or Y, UV).
    pub planes: Vec<Plane>,
    /// The picture is interlaced.
    pub interlaced: bool,
    /// Top field first, if interlaced.
    pub top_field_first: bool,
    /// Color matrix.
    pub matrix: Matrix,
    /// Sample range.
    pub range: Range,
    /// Sample aspect ratio, if signaled.
    pub sample_aspect: Option<Rational>,
    /// A/53 `cc_data` triplets carried with the picture (SEI or user data), 3 bytes each.
    pub cc_data: Vec<u8>,
    /// The frame was decoded from a damaged or incomplete picture.
    pub corrupt: bool,
}

/// A block of decoded audio as the worker describes it: 32-bit float, planar, one plane of
/// `samples` values per channel, contiguous in the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrameDesc {
    /// The stream it was decoded from.
    pub stream: StreamIndex,
    /// Presentation time of the first sample in the stream's time base.
    pub pts: Option<i64>,
    /// Sampling rate in Hz.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u8,
    /// Samples per channel.
    pub samples: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_reduces_and_rescales() {
        let r = Rational::new(2, 2000).unwrap();
        assert_eq!((r.num(), r.den()), (1, 1000));
        assert_eq!(Rational::MILLIS.rescale(1000, Rational::MPEG_90K), 90_000);
        assert_eq!(
            Rational::MPEG_90K.rescale(3003, Rational::MPEG_27M),
            3003 * 300
        );
        // Rounding to nearest, ties away from zero, both signs.
        assert_eq!(Rational::MPEG_90K.rescale(45, Rational::MILLIS), 1);
        assert_eq!(Rational::MPEG_90K.rescale(-45, Rational::MILLIS), -1);
        assert_eq!(Rational::MPEG_90K.rescale(44, Rational::MILLIS), 0);
        assert!(Rational::new(0, 1).is_err());
    }

    #[test]
    fn kinds() {
        assert_eq!(CodecId::H264.kind(), MediaKind::Video);
        assert_eq!(CodecId::Aac.kind(), MediaKind::Audio);
        assert_eq!(CodecId::Text.kind(), MediaKind::Data);
    }
}
