//! FLV tag bodies as RTMP audio and video messages carry them.
//!
//! - Legacy video (FLV v10.1 §E.4.3.1, `VIDEODATA` / `AVCVIDEOPACKET`): `FrameType` (4 bits),
//!   `CodecID` (4 bits); for AVC (7) an `AVCPacketType` (0 sequence header, 1 NALUs, 2 end of
//!   sequence) and a signed 24-bit `CompositionTime` in ms. CodecID 12 (HEVC, the widespread
//!   pre-standard extension of Chinese CDNs and of ffmpeg before 6.1) is read with the same
//!   layout.
//! - Enhanced RTMP (veovera "Enhanced RTMP" v1 2023, v2 2024): `IsExHeader` (bit 7),
//!   `VideoFrameType` (3 bits), `VideoPacketType` (4 bits), then a FourCC (`hvc1`, `av01`,
//!   `vp09`, and in v2 `avc1`). Packet types: SequenceStart 0, CodedFrames 1 (with a
//!   `CompositionTime` SI24 for `hvc1`/`avc1` only), SequenceEnd 2, CodedFramesX 3 (composition
//!   time 0), Metadata 4, MPEG2TSSequenceStart 5, Multitrack 6, ModEx 7 (v2: a prefix, skipped;
//!   its nanosecond timestamp offset is below our millisecond time base).
//! - Legacy audio (FLV §E.4.2.1, `AUDIODATA` / `AACAUDIODATA`): `SoundFormat` (4 bits), rate
//!   (2), size (1), type (1); AAC (10) has an `AACPacketType` (0 AudioSpecificConfig, 1 raw).
//!   MP3 (2, and 14 "MP3 8 kHz") and 16-bit little-endian PCM (3) are carried too.
//! - Enhanced audio (Enhanced RTMP v2): `SoundFormat` 9 = ExHeader, `AudioPacketType` (low
//!   4 bits: SequenceStart 0, CodedFrames 1, SequenceEnd 2, MultichannelConfig 4,
//!   Multitrack 5, ModEx 7), FourCC `mp4a`, `Opus`, `.mp3`, `ac-3`, `ec-3`.
//! - Enhanced audio **multitrack** (v2, [`parse_audio_tracks`]): `AvMultitrackType` (4 bits:
//!   OneTrack 0, ManyTracks 1, ManyTracksManyCodecs 2) and the tracks' `AudioPacketType` (4
//!   bits, never Multitrack), then the FourCC unless each track has its own; per track (its
//!   FourCC for ManyTracksManyCodecs), `audioTrackId` (UI8) and, unless OneTrack, the track's
//!   size (UI24); the message holds tracks until it is used up. Track 0 is the default track.
//!   The tracks carry no language: they are told apart by id only.
//!
//! Everything else (Sorenson, VP6, Speex, Nellymoser, G.711, multitrack video) is reported as
//! [`VideoTag::Unsupported`] / [`AudioTag::Unsupported`] so the session can ignore the track.

use tb_media::CodecId;

use crate::chunk::be24;

/// A parsed video message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoTag {
    /// Decoder configuration (an `AVCDecoderConfigurationRecord`, `hvcC`, `av1C` or `vpcC`).
    SequenceStart {
        /// The codec.
        codec: CodecId,
        /// The configuration record.
        config: Vec<u8>,
    },
    /// One coded picture.
    Frame {
        /// The codec.
        codec: CodecId,
        /// A key frame (FrameType 1).
        key: bool,
        /// Composition time offset in ms: pts = dts + cts.
        cts: i32,
        /// The coded picture in the codec's FLV framing.
        data: Vec<u8>,
    },
    /// End of sequence.
    SequenceEnd {
        /// The codec.
        codec: CodecId,
    },
    /// Something with no picture: a command/info frame, metadata, an MPEG-2 TS sequence start.
    Info,
    /// A codec or packet layout this crate does not carry.
    Unsupported {
        /// What it was.
        what: String,
    },
}

/// A parsed audio message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioTag {
    /// Decoder configuration (AudioSpecificConfig, OpusHead).
    SequenceStart {
        /// The codec.
        codec: CodecId,
        /// The configuration record.
        config: Vec<u8>,
    },
    /// One or more coded audio frames.
    Frame {
        /// The codec.
        codec: CodecId,
        /// Rate and channels the tag header gives (meaningful for PCM and MP3 only).
        header: AudioHeader,
        /// The data.
        data: Vec<u8>,
    },
    /// End of sequence, or nothing to carry (an empty tag).
    Info,
    /// A codec this crate does not carry.
    Unsupported {
        /// What it was.
        what: String,
    },
}

/// The legacy audio tag header's rate and channel bits (FLV §E.4.2.1). For AAC they are fixed
/// at 44 kHz stereo and mean nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioHeader {
    /// 5512, 11025, 22050 or 44100 Hz (8000 for "MP3 8 kHz"); 0 for enhanced audio.
    pub rate: u32,
    /// 1 or 2; 0 for enhanced audio.
    pub channels: u8,
}

/// A malformed tag body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed FLV {kind} tag: {why}")]
pub struct FlvError {
    /// "audio" or "video".
    pub kind: &'static str,
    /// What is wrong.
    pub why: &'static str,
}

const fn verr(why: &'static str) -> FlvError {
    FlvError { kind: "video", why }
}
const fn aerr(why: &'static str) -> FlvError {
    FlvError { kind: "audio", why }
}

fn si24(b: &[u8]) -> i32 {
    // Sign-extend the 24-bit value.
    ((be24(b) << 8).cast_signed()) >> 8
}

/// Parse a video message body.
pub fn parse_video(p: &[u8]) -> Result<VideoTag, FlvError> {
    let &b0 = p.first().ok_or(verr("empty"))?;
    if b0 & 0x80 != 0 {
        return parse_video_ex(p);
    }
    let frame_type = b0 >> 4;
    let codec_id = b0 & 0x0F;
    let codec = match codec_id {
        7 => CodecId::H264,
        12 => CodecId::Hevc,
        n => {
            return Ok(VideoTag::Unsupported {
                what: format!("FLV video CodecID {n}"),
            });
        }
    };
    if frame_type == 5 {
        // Video info/command frame (§E.4.3.1): no picture.
        return Ok(VideoTag::Info);
    }
    let h = p.get(1..5).ok_or(verr("AVC header truncated"))?;
    let data = &p[5..];
    match h[0] {
        0 => Ok(VideoTag::SequenceStart {
            codec,
            config: data.to_vec(),
        }),
        1 => Ok(VideoTag::Frame {
            codec,
            key: frame_type == 1,
            cts: si24(&h[1..4]),
            data: data.to_vec(),
        }),
        2 => Ok(VideoTag::SequenceEnd { codec }),
        _ => Err(verr("AVCPacketType above 2")),
    }
}

/// Enhanced RTMP packet types (v1 + v2).
mod ex {
    pub const SEQUENCE_START: u8 = 0;
    pub const CODED_FRAMES: u8 = 1;
    pub const SEQUENCE_END: u8 = 2;
    pub const CODED_FRAMES_X: u8 = 3;
    pub const METADATA: u8 = 4;
    pub const MPEG2TS_SEQUENCE_START: u8 = 5;
    pub const MULTITRACK: u8 = 6;
    pub const MOD_EX: u8 = 7;
}

/// Skip ModEx prefixes (Enhanced RTMP v2): returns the real packet type and the rest. At most
/// a few rounds: each consumes at least two bytes.
fn skip_mod_ex(
    mut packet_type: u8,
    mut rest: &[u8],
    err: fn(&'static str) -> FlvError,
) -> Result<(u8, &[u8]), FlvError> {
    let mut rounds = 0;
    while packet_type == ex::MOD_EX {
        rounds += 1;
        if rounds > 8 {
            return Err(err("too many ModEx prefixes"));
        }
        let (&n, r) = rest.split_first().ok_or(err("ModEx truncated"))?;
        let (size, r) = if n == 0xFF {
            let s = r.get(..2).ok_or(err("ModEx truncated"))?;
            (usize::from(u16::from_be_bytes([s[0], s[1]])) + 1, &r[2..])
        } else {
            (usize::from(n) + 1, r)
        };
        let r = r.get(size..).ok_or(err("ModEx data truncated"))?;
        let (&t, r) = r.split_first().ok_or(err("ModEx truncated"))?;
        packet_type = t & 0x0F;
        rest = r;
    }
    Ok((packet_type, rest))
}

fn parse_video_ex(p: &[u8]) -> Result<VideoTag, FlvError> {
    let b0 = p[0];
    let frame_type = (b0 >> 4) & 0x07;
    let (packet_type, rest) = skip_mod_ex(b0 & 0x0F, &p[1..], verr)?;
    if packet_type == ex::MULTITRACK {
        return Ok(VideoTag::Unsupported {
            what: "Enhanced RTMP multitrack video".into(),
        });
    }
    if frame_type == 5 && packet_type != ex::METADATA {
        // A command frame: one UI8 VideoCommand, no FourCC.
        return Ok(VideoTag::Info);
    }
    let fourcc = rest.get(..4).ok_or(verr("FourCC truncated"))?;
    let rest = &rest[4..];
    let codec = match fourcc {
        b"avc1" => CodecId::H264,
        b"hvc1" => CodecId::Hevc,
        b"av01" => CodecId::Av1,
        b"vp09" => CodecId::Vp9,
        other => {
            return Ok(VideoTag::Unsupported {
                what: format!(
                    "Enhanced RTMP video FourCC {}",
                    String::from_utf8_lossy(other)
                ),
            });
        }
    };
    let key = frame_type == 1;
    match packet_type {
        ex::SEQUENCE_START => Ok(VideoTag::SequenceStart {
            codec,
            config: rest.to_vec(),
        }),
        ex::CODED_FRAMES => {
            let (cts, data) = if matches!(codec, CodecId::Hevc | CodecId::H264) {
                let c = rest.get(..3).ok_or(verr("CompositionTime truncated"))?;
                (si24(c), &rest[3..])
            } else {
                (0, rest)
            };
            Ok(VideoTag::Frame {
                codec,
                key,
                cts,
                data: data.to_vec(),
            })
        }
        ex::CODED_FRAMES_X => Ok(VideoTag::Frame {
            codec,
            key,
            cts: 0,
            data: rest.to_vec(),
        }),
        ex::SEQUENCE_END => Ok(VideoTag::SequenceEnd { codec }),
        ex::METADATA | ex::MPEG2TS_SEQUENCE_START => Ok(VideoTag::Info),
        _ => Err(verr("unknown VideoPacketType")),
    }
}

/// Parse an audio message body.
pub fn parse_audio(p: &[u8]) -> Result<AudioTag, FlvError> {
    let Some(&b0) = p.first() else {
        return Ok(AudioTag::Info);
    };
    let format = b0 >> 4;
    let header = AudioHeader {
        rate: [5512, 11025, 22050, 44100][usize::from((b0 >> 2) & 3)],
        channels: 1 + (b0 & 1),
    };
    let data = &p[1..];
    match format {
        10 => {
            let (&t, raw) = data.split_first().ok_or(aerr("AACPacketType missing"))?;
            match t {
                0 => Ok(AudioTag::SequenceStart {
                    codec: CodecId::Aac,
                    config: raw.to_vec(),
                }),
                1 => Ok(AudioTag::Frame {
                    codec: CodecId::Aac,
                    header,
                    data: raw.to_vec(),
                }),
                _ => Err(aerr("AACPacketType above 1")),
            }
        }
        2 | 14 => Ok(AudioTag::Frame {
            codec: CodecId::MpegAudio,
            header: if format == 14 {
                AudioHeader {
                    rate: 8000,
                    ..header
                }
            } else {
                header
            },
            data: data.to_vec(),
        }),
        3 if b0 & 2 != 0 => Ok(AudioTag::Frame {
            codec: CodecId::PcmS16le,
            header,
            data: data.to_vec(),
        }),
        9 => parse_audio_ex(p),
        n => Ok(AudioTag::Unsupported {
            what: format!("FLV SoundFormat {n} (size bit {})", (b0 >> 1) & 1),
        }),
    }
}

/// Parse an audio message body into its tracks: `(audioTrackId, tag)`. Everything that is not
/// Enhanced RTMP multitrack is one tag of track 0.
pub fn parse_audio_tracks(p: &[u8]) -> Result<Vec<(u8, AudioTag)>, FlvError> {
    if p.first().is_some_and(|b| b >> 4 == 9) {
        let (packet_type, rest) = skip_mod_ex(p[0] & 0x0F, &p[1..], aerr)?;
        if packet_type == 5 {
            return parse_audio_multitrack(rest);
        }
    }
    Ok(vec![(0, parse_audio(p)?)])
}

/// The body of an audio Multitrack message (after its packet type).
fn parse_audio_multitrack(rest: &[u8]) -> Result<Vec<(u8, AudioTag)>, FlvError> {
    let (&b, mut rest) = rest
        .split_first()
        .ok_or(aerr("Multitrack header truncated"))?;
    let (kind, packet_type) = (b >> 4, b & 0x0F);
    if packet_type == 5 {
        return Err(aerr("Multitrack inside Multitrack"));
    }
    if kind > 2 {
        return Err(aerr("unknown AvMultitrackType"));
    }
    let mut shared = None;
    if kind != 2 {
        shared = Some(rest.get(..4).ok_or(aerr("FourCC truncated"))?);
        rest = &rest[4..];
    }
    let mut out = Vec::new();
    while !rest.is_empty() {
        let fourcc = match shared {
            Some(f) => f,
            None => {
                let f = rest.get(..4).ok_or(aerr("FourCC truncated"))?;
                rest = &rest[4..];
                f
            }
        };
        let (&id, r) = rest.split_first().ok_or(aerr("audioTrackId truncated"))?;
        rest = r;
        let body = if kind == 0 {
            std::mem::take(&mut rest)
        } else {
            let n = rest.get(..3).ok_or(aerr("track size truncated"))?;
            let n = usize::try_from(be24(n)).unwrap_or(usize::MAX);
            let body = rest.get(3..3 + n).ok_or(aerr("track truncated"))?;
            rest = &rest[3 + n..];
            body
        };
        out.push((id, audio_ex_body(packet_type, fourcc, body)?));
        if kind == 0 {
            break;
        }
    }
    Ok(out)
}

fn parse_audio_ex(p: &[u8]) -> Result<AudioTag, FlvError> {
    let (packet_type, rest) = skip_mod_ex(p[0] & 0x0F, &p[1..], aerr)?;
    // Audio's Multitrack is 5 and its ModEx 7 (Enhanced RTMP v2 AudioPacketType).
    if packet_type == 5 {
        return Ok(AudioTag::Unsupported {
            what: "Enhanced RTMP multitrack audio (use parse_audio_tracks)".into(),
        });
    }
    let fourcc = rest.get(..4).ok_or(aerr("FourCC truncated"))?;
    audio_ex_body(packet_type, fourcc, &rest[4..])
}

/// One enhanced audio packet of `packet_type` for `fourcc`.
fn audio_ex_body(packet_type: u8, fourcc: &[u8], rest: &[u8]) -> Result<AudioTag, FlvError> {
    let codec = match fourcc {
        b"mp4a" => CodecId::Aac,
        b"Opus" => CodecId::Opus,
        b".mp3" => CodecId::MpegAudio,
        b"ac-3" => CodecId::Ac3,
        b"ec-3" => CodecId::Eac3,
        other => {
            return Ok(AudioTag::Unsupported {
                what: format!(
                    "Enhanced RTMP audio FourCC {}",
                    String::from_utf8_lossy(other)
                ),
            });
        }
    };
    match packet_type {
        0 => Ok(AudioTag::SequenceStart {
            codec,
            config: rest.to_vec(),
        }),
        1 => Ok(AudioTag::Frame {
            codec,
            header: AudioHeader::default(),
            data: rest.to_vec(),
        }),
        // SequenceEnd, MultichannelConfig.
        2 | 4 => Ok(AudioTag::Info),
        _ => Err(aerr("unknown AudioPacketType")),
    }
}

/// Sampling rate and channels from an MPEG-4 AudioSpecificConfig (ISO/IEC 14496-3 §1.6.2.1):
/// audioObjectType (5 bits, escape 31 → 6 more), samplingFrequencyIndex (4 bits, escape 15 →
/// 24-bit rate), channelConfiguration (4 bits). `None` if it is too short.
pub fn audio_specific_config(asc: &[u8]) -> Option<(u32, u8)> {
    let mut r = BitReader { data: asc, pos: 0 };
    let aot = r.bits(5)?;
    if aot == 31 {
        r.bits(6)?;
    }
    let idx = r.bits(4)?;
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let rate = if idx == 15 {
        r.bits(24)?
    } else {
        *RATES.get(usize::try_from(idx).ok()?)?
    };
    let ch = r.bits(4)?;
    // channelConfiguration 7 is 7.1 (8 channels); 0 is "in the PCE", unknown here.
    let channels = match ch {
        1..=6 => ch as u8,
        7 => 8,
        _ => 0,
    };
    Some((rate, channels))
}

/// Rate and channels from an OpusHead (RFC 7845 §5.1): channel count at byte 9, input sample
/// rate at 12..16 (little-endian). Opus always decodes at 48 kHz.
pub fn opus_head(head: &[u8]) -> Option<(u32, u8)> {
    if head.get(..8)? != b"OpusHead" {
        return None;
    }
    Some((48_000, *head.get(9)?))
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            v = (v << 1) | u32::from(bit);
            self.pos += 1;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_avc() {
        assert_eq!(
            parse_video(&[0x17, 0, 0, 0, 0, 1, 2, 3]).unwrap(),
            VideoTag::SequenceStart {
                codec: CodecId::H264,
                config: vec![1, 2, 3]
            }
        );
        assert_eq!(
            parse_video(&[0x27, 1, 0xFF, 0xFF, 0xFE, 9]).unwrap(),
            VideoTag::Frame {
                codec: CodecId::H264,
                key: false,
                cts: -2,
                data: vec![9]
            }
        );
        assert_eq!(
            parse_video(&[0x17, 1, 0, 0, 66]).unwrap(),
            VideoTag::Frame {
                codec: CodecId::H264,
                key: true,
                cts: 66,
                data: vec![]
            }
        );
        assert_eq!(
            parse_video(&[0x17, 2, 0, 0, 0]).unwrap(),
            VideoTag::SequenceEnd {
                codec: CodecId::H264
            }
        );
        assert!(parse_video(&[0x17, 1, 0]).is_err());
        assert!(parse_video(&[]).is_err());
        assert!(matches!(
            parse_video(&[0x12, 0]).unwrap(),
            VideoTag::Unsupported { .. }
        ));
    }

    #[test]
    fn enhanced_video() {
        // KeyFrame | SequenceStart, hvc1.
        let mut b = vec![0x80 | 0x10, b'h', b'v', b'c', b'1', 1, 2];
        assert_eq!(
            parse_video(&b).unwrap(),
            VideoTag::SequenceStart {
                codec: CodecId::Hevc,
                config: vec![1, 2]
            }
        );
        // InterFrame | CodedFrames, hvc1 with cts 33.
        b = vec![0x80 | 0x20 | 1, b'h', b'v', b'c', b'1', 0, 0, 33, 7];
        assert_eq!(
            parse_video(&b).unwrap(),
            VideoTag::Frame {
                codec: CodecId::Hevc,
                key: false,
                cts: 33,
                data: vec![7]
            }
        );
        // CodedFramesX for av01: no cts.
        b = vec![0x80 | 0x10 | 3, b'a', b'v', b'0', b'1', 5];
        assert_eq!(
            parse_video(&b).unwrap(),
            VideoTag::Frame {
                codec: CodecId::Av1,
                key: true,
                cts: 0,
                data: vec![5]
            }
        );
        // CodedFrames for vp09: no cts either.
        b = vec![0x80 | 0x10 | 1, b'v', b'p', b'0', b'9', 5, 6];
        assert!(matches!(
            parse_video(&b).unwrap(),
            VideoTag::Frame { cts: 0, ref data, .. } if data == &[5, 6]
        ));
        // ModEx prefix (1 byte of data, type TimestampOffsetNano | CodedFramesX).
        b = vec![0x80 | 0x10 | 7, 0, 0xAA, 0x03, b'a', b'v', b'0', b'1', 5];
        assert!(matches!(
            parse_video(&b).unwrap(),
            VideoTag::Frame {
                codec: CodecId::Av1,
                ..
            }
        ));
        // Command frame.
        assert_eq!(parse_video(&[0x80 | 0x50 | 1, 0]).unwrap(), VideoTag::Info);
    }

    #[test]
    fn audio() {
        assert_eq!(
            parse_audio(&[0xAF, 0, 0x12, 0x10]).unwrap(),
            AudioTag::SequenceStart {
                codec: CodecId::Aac,
                config: vec![0x12, 0x10]
            }
        );
        assert!(matches!(
            parse_audio(&[0xAF, 1, 1, 2]).unwrap(),
            AudioTag::Frame {
                codec: CodecId::Aac,
                ..
            }
        ));
        assert!(matches!(
            parse_audio(&[0x3F, 0, 0]).unwrap(),
            AudioTag::Frame {
                codec: CodecId::PcmS16le,
                header: AudioHeader {
                    rate: 44100,
                    channels: 2
                },
                ..
            }
        ));
        assert!(matches!(
            parse_audio(&[0x39, 0]).unwrap(),
            AudioTag::Unsupported { .. }
        ));
        assert!(matches!(
            parse_audio(&[0x90, b'O', b'p', b'u', b's', 1]).unwrap(),
            AudioTag::SequenceStart {
                codec: CodecId::Opus,
                ..
            }
        ));
        assert!(parse_audio(&[0xAF]).is_err());
    }

    #[test]
    fn asc() {
        // AAC LC, 44.1 kHz, stereo: 0x12 0x10.
        assert_eq!(audio_specific_config(&[0x12, 0x10]), Some((44100, 2)));
        // AAC LC, 48 kHz, mono: 0x11 0x88.
        assert_eq!(audio_specific_config(&[0x11, 0x88]), Some((48000, 1)));
        assert_eq!(audio_specific_config(&[0x12]), None);
    }
}
