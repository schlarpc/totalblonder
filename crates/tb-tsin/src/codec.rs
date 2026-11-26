//! From PMT entries to codecs, and the codec-level details a TS receiver needs: key frames
//! found in the bitstream, ISO 639 languages, and Opus's TS framing.
//!
//! stream_type (H.222.0 Table 2-34, and the registered private values):
//!
//! | stream_type | descriptors | codec |
//! |---|---|---|
//! | 0x01, 0x02 | | MPEG-1/2 video (one decoder) |
//! | 0x03, 0x04 | | MPEG-1/2 audio |
//! | 0x0F | | AAC in ADTS |
//! | 0x11 | | AAC in LATM/LOAS: **refused** (`CodecId` has no LATM framing) |
//! | 0x1B | | H.264 (Annex B) |
//! | 0x24 | | HEVC (Annex B) |
//! | 0x81 | | AC-3 (ATSC A/52 Annex A; registration "AC-3") |
//! | 0x87 | | E-AC-3 (ATSC A/52 Annex G) |
//! | 0x06 | AC-3 descriptor 0x6A, or registration "AC-3" | AC-3 (DVB, ETSI EN 300 468 Annex D) |
//! | 0x06 | enhanced AC-3 descriptor 0x7A, or registration "EAC3" | E-AC-3 |
//! | 0x06 | registration "Opus" | Opus, in the TS framing of `opus_control_header` (as ffmpeg) |
//! | 0x06 | registration "HEVC" | HEVC |
//!
//! Everything else (subtitles, teletext, SCTE-35, metadata, ...) is left out of `Streams`.

use tb_media::CodecId;

use crate::psi::{Descriptor, PmtStream};

/// Registration descriptor (H.222.0 §2.6.8).
const REGISTRATION: u8 = 0x05;
/// ISO 639 language descriptor (§2.6.18).
const ISO_639: u8 = 0x0A;
/// DVB AC-3 descriptor (ETSI EN 300 468 Annex D).
const DVB_AC3: u8 = 0x6A;
/// DVB enhanced AC-3 descriptor.
const DVB_EAC3: u8 = 0x7A;
/// DVB extension descriptor (EN 300 468); extension tag 0x80 is the Opus audio descriptor.
const DVB_EXTENSION: u8 = 0x7F;

/// What a PMT entry carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamKind {
    /// A codec we pass on.
    Codec(CodecId),
    /// Something we do not pass on; the text says what.
    Skipped(&'static str),
}

fn registration(d: &[Descriptor]) -> Option<&[u8]> {
    d.iter()
        .find(|d| d.tag == REGISTRATION && d.data.len() >= 4)
        .map(|d| &d.data[..4])
}

/// The codec of a PMT entry; `program` are the program-level descriptors (ATSC puts the
/// "GA94"/"AC-3" registration there).
pub fn classify(s: &PmtStream, program: &[Descriptor]) -> StreamKind {
    let d = &s.descriptors;
    let has = |tag: u8| d.iter().any(|x| x.tag == tag);
    match s.stream_type {
        0x01 | 0x02 => StreamKind::Codec(CodecId::Mpeg2Video),
        0x03 | 0x04 => StreamKind::Codec(CodecId::MpegAudio),
        0x0F => StreamKind::Codec(CodecId::Aac),
        0x11 => StreamKind::Skipped("AAC in LATM (stream_type 0x11) is not supported"),
        0x1B => StreamKind::Codec(CodecId::H264),
        0x24 => StreamKind::Codec(CodecId::Hevc),
        0x81 => StreamKind::Codec(CodecId::Ac3),
        0x87 => StreamKind::Codec(CodecId::Eac3),
        0x06 => {
            let reg = registration(d).or_else(|| registration(program));
            if has(DVB_EAC3) || reg == Some(b"EAC3") {
                StreamKind::Codec(CodecId::Eac3)
            } else if has(DVB_AC3) || reg == Some(b"AC-3") {
                StreamKind::Codec(CodecId::Ac3)
            } else if reg == Some(b"Opus") {
                StreamKind::Codec(CodecId::Opus)
            } else if reg == Some(b"HEVC") {
                StreamKind::Codec(CodecId::Hevc)
            } else {
                StreamKind::Skipped("private data (stream_type 0x06) of no known registration")
            }
        }
        _ => StreamKind::Skipped("stream_type not carried"),
    }
}

/// The first language of an ISO 639 language descriptor (§2.6.18), if any.
pub fn language(d: &[Descriptor]) -> Option<[u8; 3]> {
    let l = d.iter().find(|d| d.tag == ISO_639 && d.data.len() >= 3)?;
    let code = [l.data[0], l.data[1], l.data[2]];
    code.iter().all(u8::is_ascii_alphabetic).then_some(code)
}

/// An `OpusHead` (RFC 7845 §5.1) for the DVB Opus descriptor's channel_config_code, for mono
/// and stereo (mapping family 0); `None` otherwise (the decoder then assumes stereo).
pub fn opus_head(d: &[Descriptor]) -> Option<Vec<u8>> {
    let e = d
        .iter()
        .find(|d| d.tag == DVB_EXTENSION && d.data.first() == Some(&0x80))?;
    let channels = match e.data.get(1)? {
        // 0: dual mono, coded as two channels.
        0 | 2 => 2u8,
        1 => 1,
        _ => return None,
    };
    let mut h = b"OpusHead".to_vec();
    h.push(1);
    h.push(channels);
    h.extend_from_slice(&0u16.to_le_bytes()); // pre-skip: not signaled in TS
    h.extend_from_slice(&48_000u32.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes());
    h.push(0); // mapping family 0
    Some(h)
}

/// Iterates over the bytes after each 3-byte start code 00 00 01.
fn after_start_codes(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 3 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                i += 3;
                return Some(&data[i..]);
            }
            i += 1;
        }
        None
    })
}

/// Whether an access unit is a random access point, from the bitstream: an IDR slice (H.264
/// nal_unit_type 5), an IRAP picture (HEVC nal_unit_type 16–23), or a sequence header (MPEG-2
/// 0xB3). Audio is always a key frame.
pub fn is_key(codec: CodecId, data: &[u8]) -> bool {
    match codec {
        CodecId::H264 => after_start_codes(data).any(|n| n.first().is_some_and(|b| b & 0x1F == 5)),
        CodecId::Hevc => after_start_codes(data).any(|n| {
            n.first()
                .is_some_and(|b| (16..=23).contains(&((b >> 1) & 0x3F)))
        }),
        CodecId::Mpeg2Video => after_start_codes(data).any(|n| n.first() == Some(&0xB3)),
        c => c.kind() == tb_media::MediaKind::Audio,
    }
}

/// One Opus access unit from a PES payload in TS framing (as ffmpeg reads and writes it): an
/// opus_control_header (11-bit prefix 0x3FF, start/end trim and extension flags, the AU size
/// as a run of 0xFF bytes and a final byte, optional trims and extension) and the packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusAu<'a> {
    /// The Opus packet.
    pub data: &'a [u8],
    /// Its duration in 90 kHz ticks, from its TOC (RFC 6716 §3.1), if readable.
    pub duration: Option<i64>,
}

/// Split a PES payload into Opus access units; stops at the first malformed header.
pub fn opus_units(mut p: &[u8]) -> Vec<OpusAu<'_>> {
    let mut v = Vec::new();
    while p.len() >= 2 {
        if p[0] != 0x7F || p[1] & 0xE0 != 0xE0 {
            break;
        }
        let start_trim = p[1] & 0x10 != 0;
        let end_trim = p[1] & 0x08 != 0;
        let ext = p[1] & 0x04 != 0;
        let mut i = 2;
        let mut size = 0usize;
        loop {
            let Some(&b) = p.get(i) else { return v };
            i += 1;
            size += usize::from(b);
            if b != 0xFF {
                break;
            }
        }
        i += 2 * usize::from(start_trim) + 2 * usize::from(end_trim);
        if ext {
            let Some(&n) = p.get(i) else { return v };
            i += 1 + usize::from(n);
        }
        let Some(data) = p.get(i..i + size) else {
            return v;
        };
        v.push(OpusAu {
            data,
            duration: opus_duration(data),
        });
        p = &p[i + size..];
    }
    v
}

/// An Opus packet's duration in 90 kHz ticks (RFC 6716 §3.1–3.2).
fn opus_duration(packet: &[u8]) -> Option<i64> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    // Frame sizes in units of 2.5 ms (225 ticks at 90 kHz).
    let units = match config {
        0..=11 => [4, 8, 16, 24][usize::from(config % 4)],
        12..=15 => [4, 8][usize::from(config % 2)],
        _ => [1, 2, 4, 8][usize::from(config % 4)],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => i64::from(*packet.get(1)? & 0x3F),
    };
    Some(units * 225 * frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Pid;

    fn entry(t: u8, d: Vec<Descriptor>) -> PmtStream {
        PmtStream {
            stream_type: t,
            pid: Pid::new(0x100).unwrap(),
            descriptors: d,
        }
    }

    fn desc(tag: u8, data: &[u8]) -> Descriptor {
        Descriptor {
            tag,
            data: data.to_vec(),
        }
    }

    #[test]
    fn stream_types() {
        let c = |t, d| classify(&entry(t, d), &[]);
        assert_eq!(c(0x1B, vec![]), StreamKind::Codec(CodecId::H264));
        assert_eq!(c(0x02, vec![]), StreamKind::Codec(CodecId::Mpeg2Video));
        assert_eq!(c(0x81, vec![]), StreamKind::Codec(CodecId::Ac3));
        assert_eq!(c(0x87, vec![]), StreamKind::Codec(CodecId::Eac3));
        assert_eq!(
            c(0x06, vec![desc(0x6A, &[0])]),
            StreamKind::Codec(CodecId::Ac3)
        );
        assert_eq!(
            c(0x06, vec![desc(0x7A, &[0])]),
            StreamKind::Codec(CodecId::Eac3)
        );
        assert_eq!(
            c(0x06, vec![desc(5, b"Opus")]),
            StreamKind::Codec(CodecId::Opus)
        );
        assert!(matches!(c(0x11, vec![]), StreamKind::Skipped(_)));
        assert!(matches!(c(0x06, vec![]), StreamKind::Skipped(_)));
        let prog = [desc(5, b"AC-3")];
        assert_eq!(
            classify(&entry(0x06, vec![]), &prog),
            StreamKind::Codec(CodecId::Ac3)
        );
        assert_eq!(language(&[desc(0x0A, b"spa\0")]), Some(*b"spa"));
        assert_eq!(language(&[desc(0x0A, b"\0\0\0\0")]), None);
    }

    #[test]
    fn keys() {
        assert!(is_key(
            CodecId::H264,
            &[0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x65, 0x88]
        ));
        assert!(!is_key(
            CodecId::H264,
            &[0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x41, 0x88]
        ));
        assert!(is_key(CodecId::Hevc, &[0, 0, 1, 0x26, 0x01, 0xAF]));
        assert!(!is_key(CodecId::Hevc, &[0, 0, 1, 0x02, 0x01, 0xAF]));
        assert!(is_key(CodecId::Mpeg2Video, &[0, 0, 1, 0xB3, 0x2D]));
        assert!(is_key(CodecId::Ac3, &[]));
    }

    #[test]
    fn opus_framing() {
        // Two AUs: 20 ms CELT fullband (config 31, code 0), sizes 3 and 256.
        let mut p = vec![0x7F, 0xE0, 3, 0xF8, 1, 2];
        p.extend_from_slice(&[0x7F, 0xE0 | 0x10, 0xFF, 1, 0, 0]);
        p.push(0xF8);
        p.extend_from_slice(&[0; 255]);
        let u = opus_units(&p);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].data, &[0xF8, 1, 2]);
        assert_eq!(u[0].duration, Some(1800));
        assert_eq!(u[1].data.len(), 256);
        assert!(opus_units(&[0x7F, 0xE0, 10, 1]).is_empty());
        assert_eq!(opus_head(&[desc(0x7F, &[0x80, 2])]).unwrap().len(), 19);
    }
}
