//! A fragmented-MP4 (CMAF, ISO/IEC 14496-12) demuxer for HLS: the init segment's tracks, then
//! each fragment's samples.
//!
//! - **Init** (`moov`): per `trak`, `tkhd` (track_ID), `mdia/mdhd` (timescale, language),
//!   `mdia/hdlr` (vide / soun), `stbl/stsd`'s first sample entry: `avc1`/`avc3` (`avcC`),
//!   `hvc1`/`hev1` (`hvcC`), `av01` (`av1C`), `vp09` (`vpcC`), `mp4a` (`esds`: AAC's
//!   AudioSpecificConfig, or MP3), `ac-3`, `ec-3`, `Opus` (`dOps`, turned into the `OpusHead`
//!   a decoder takes). `mvex/trex` gives the per-track defaults. Encrypted entries (`encv`,
//!   `enca`) and anything else are reported as unsupported tracks.
//! - **Fragments** (`moof` + `mdat`): per `traf`, `tfhd` (base data offset, default sample
//!   duration/size/flags, default-base-is-moof), `tfdt` (base media decode time) and every
//!   `trun` (data offset, per-sample duration, size, flags, composition offset). Data offsets
//!   count from the `moof` unless `tfhd` gives a base; a `trun` without one continues after the
//!   previous run. A sample is a key frame unless its flags say non-sync.
//!
//! Byte layouts follow ISO/IEC 14496-12:2022 §8 (boxes) and ISO/IEC 14496-1 §7.2.6 (ES
//! descriptors); `dOps` is RFC 7845 §5.1 by way of the Opus-in-ISOBMFF mapping.

use std::collections::BTreeMap;

use tb_media::CodecId;

/// Why a box structure could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("fMP4: {0}")]
pub struct Fmp4Error(pub String);

fn err(s: impl Into<String>) -> Fmp4Error {
    Fmp4Error(s.into())
}

/// One track of the init segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// `track_ID`.
    pub id: u32,
    /// Ticks per second of its timestamps.
    pub timescale: u32,
    /// The codec; `None` for one not carried (the reason in `unsupported`).
    pub codec: Option<CodecId>,
    /// What was not carried.
    pub unsupported: Option<String>,
    /// The decoder configuration (avcC, hvcC, av1C, vpcC, AudioSpecificConfig, OpusHead).
    pub extradata: Vec<u8>,
    /// Width and height of a video sample entry.
    pub size: Option<(u32, u32)>,
    /// Channels and sampling rate of an audio sample entry (the configuration's when it has
    /// them).
    pub audio: Option<(u8, u32)>,
    /// `mdhd` language, unless `und`.
    pub language: Option<[u8; 3]>,
    defaults: Defaults,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Defaults {
    duration: u32,
    size: u32,
    flags: u32,
}

/// One sample of a fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// `track_ID`.
    pub track: u32,
    /// Decoding time, in the track's timescale.
    pub dts: i64,
    /// Presentation time.
    pub pts: i64,
    /// A sync sample.
    pub key: bool,
    /// The bytes (length-prefixed NAL units for AVC/HEVC, as in the init's configuration).
    pub data: Vec<u8>,
}

/// The demuxer of one rendition: tracks from its init segment, then samples.
#[derive(Debug, Clone, Default)]
pub struct Demuxer {
    tracks: BTreeMap<u32, Track>,
}

/// A box: its type, its payload, and its offset in the buffer.
type Atom<'a> = ([u8; 4], &'a [u8], usize);

/// The boxes of `b`, in order.
fn boxes(mut b: &[u8]) -> Result<Vec<Atom<'_>>, Fmp4Error> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while !b.is_empty() {
        if b.len() < 8 {
            return Err(err("box header truncated"));
        }
        let size32 = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        let kind = [b[4], b[5], b[6], b[7]];
        let (size, head) = match size32 {
            0 => (b.len(), 8),
            1 => {
                let s = b.get(8..16).ok_or(err("largesize truncated"))?;
                let s = u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                (usize::try_from(s).map_err(|_| err("box too large"))?, 16)
            }
            n => (n as usize, 8),
        };
        if size < head || size > b.len() {
            return Err(err(format!(
                "box {} of {size} bytes in {} left",
                String::from_utf8_lossy(&kind),
                b.len()
            )));
        }
        out.push((kind, &b[head..size], at));
        at += size;
        b = &b[size..];
    }
    Ok(out)
}

fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Result<Option<&'a [u8]>, Fmp4Error> {
    Ok(boxes(b)?
        .into_iter()
        .find(|(k, _, _)| k == kind)
        .map(|(_, p, _)| p))
}

fn be16(b: &[u8], at: usize) -> Result<u16, Fmp4Error> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| err("field truncated"))
}

fn be32(b: &[u8], at: usize) -> Result<u32, Fmp4Error> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| err("field truncated"))
}

fn be64(b: &[u8], at: usize) -> Result<u64, Fmp4Error> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| err("field truncated"))
}

/// The payload of an ES descriptor's DecoderSpecificInfo, and its objectTypeIndication.
fn esds(b: &[u8]) -> Option<(u8, Vec<u8>)> {
    // full box header, then descriptors: tag, size (7 bits a byte, top bit = more)
    fn desc(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, mut r) = b.split_first()?;
        let mut len = 0usize;
        for _ in 0..4 {
            let (&x, rr) = r.split_first()?;
            r = rr;
            len = (len << 7) | usize::from(x & 0x7F);
            if x & 0x80 == 0 {
                break;
            }
        }
        Some((tag, r.get(..len)?, r.get(len..)?))
    }
    let (tag, es, _) = desc(b.get(4..)?)?;
    if tag != 0x03 {
        return None;
    }
    let flags = *es.get(2)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2; // dependsOn_ES_ID
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?); // URL
    }
    if flags & 0x20 != 0 {
        at += 2; // OCR_ES_Id
    }
    let (tag, dc, _) = desc(es.get(at..)?)?;
    if tag != 0x04 {
        return None;
    }
    let oti = *dc.first()?;
    let dsi = desc(dc.get(13..)?)
        .filter(|(t, _, _)| *t == 0x05)
        .map(|(_, p, _)| p.to_vec())
        .unwrap_or_default();
    Some((oti, dsi))
}

/// `dOps` (big-endian) as an `OpusHead` (little-endian), RFC 7845 §5.1.
fn opus_head(d: &[u8]) -> Option<Vec<u8>> {
    let ch = *d.get(1)?;
    let pre_skip = be16(d, 2).ok()?;
    let rate = be32(d, 4).ok()?;
    let gain = be16(d, 8).ok()?;
    let family = *d.get(10)?;
    let mut h = b"OpusHead".to_vec();
    h.push(1);
    h.push(ch);
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&gain.to_le_bytes());
    h.push(family);
    if family != 0 {
        h.extend_from_slice(d.get(11..13 + usize::from(ch))?);
    }
    Some(h)
}

fn sample_entry(entry_kind: [u8; 4], e: &[u8], t: &mut Track) -> Result<(), Fmp4Error> {
    let video = |t: &mut Track, cfg: &[u8; 4], codec| -> Result<(), Fmp4Error> {
        t.size = Some((u32::from(be16(e, 24)?), u32::from(be16(e, 26)?)));
        match child(
            e.get(78..).ok_or(err("visual sample entry truncated"))?,
            cfg,
        )? {
            Some(c) => {
                t.codec = Some(codec);
                t.extradata = c.to_vec();
            }
            None => {
                t.unsupported = Some(format!(
                    "{} without {}",
                    String::from_utf8_lossy(&entry_kind),
                    String::from_utf8_lossy(cfg)
                ))
            }
        }
        Ok(())
    };
    let audio_children = || e.get(28..).ok_or(err("audio sample entry truncated"));
    let audio_hint =
        || -> Result<(u8, u32), Fmp4Error> { Ok((be16(e, 16)? as u8, be32(e, 24)? >> 16)) };
    match &entry_kind {
        b"avc1" | b"avc3" => video(t, b"avcC", CodecId::H264)?,
        b"hvc1" | b"hev1" => video(t, b"hvcC", CodecId::Hevc)?,
        b"av01" => video(t, b"av1C", CodecId::Av1)?,
        b"vp09" => video(t, b"vpcC", CodecId::Vp9)?,
        b"mp4a" => {
            t.audio = Some(audio_hint()?);
            match child(audio_children()?, b"esds")?.and_then(esds) {
                Some((0x40 | 0x66 | 0x67 | 0x68, asc)) => {
                    t.codec = Some(CodecId::Aac);
                    t.extradata = asc;
                }
                Some((0x69 | 0x6B, _)) => t.codec = Some(CodecId::MpegAudio),
                Some((oti, _)) => t.unsupported = Some(format!("mp4a objectType 0x{oti:02x}")),
                None => t.unsupported = Some("mp4a without esds".into()),
            }
        }
        b"ac-3" => {
            t.audio = Some(audio_hint()?);
            t.codec = Some(CodecId::Ac3);
        }
        b"ec-3" => {
            t.audio = Some(audio_hint()?);
            t.codec = Some(CodecId::Eac3);
        }
        b"Opus" => {
            t.audio = Some(audio_hint()?);
            match child(audio_children()?, b"dOps")?.and_then(opus_head) {
                Some(h) => {
                    t.codec = Some(CodecId::Opus);
                    t.extradata = h;
                }
                None => t.unsupported = Some("Opus without dOps".into()),
            }
        }
        other => t.unsupported = Some(format!("sample entry {}", String::from_utf8_lossy(other))),
    }
    Ok(())
}

fn track(trak: &[u8]) -> Result<Option<Track>, Fmp4Error> {
    let tkhd = child(trak, b"tkhd")?.ok_or(err("trak without tkhd"))?;
    let id = if tkhd.first() == Some(&1) {
        be32(tkhd, 20)?
    } else {
        be32(tkhd, 12)?
    };
    let mdia = child(trak, b"mdia")?.ok_or(err("trak without mdia"))?;
    let mdhd = child(mdia, b"mdhd")?.ok_or(err("mdia without mdhd"))?;
    let (timescale, lang_at) = if mdhd.first() == Some(&1) {
        (be32(mdhd, 20)?, 32)
    } else {
        (be32(mdhd, 12)?, 20)
    };
    let l = be16(mdhd, lang_at)?;
    let language = [
        ((l >> 10) & 31) as u8 + 0x60,
        ((l >> 5) & 31) as u8 + 0x60,
        (l & 31) as u8 + 0x60,
    ];
    let language =
        (language.iter().all(u8::is_ascii_lowercase) && &language != b"und").then_some(language);
    let hdlr = child(mdia, b"hdlr")?.ok_or(err("mdia without hdlr"))?;
    let handler = hdlr.get(8..12).ok_or(err("hdlr truncated"))?;
    if handler != b"vide" && handler != b"soun" {
        return Ok(None);
    }
    if timescale == 0 {
        return Err(err(format!("track {id}: timescale 0")));
    }
    let stsd = child(mdia, b"minf")?
        .and_then(|m| child(m, b"stbl").ok().flatten())
        .and_then(|s| child(s, b"stsd").ok().flatten())
        .ok_or(err(format!("track {id}: no stsd")))?;
    let entries = boxes(stsd.get(8..).ok_or(err("stsd truncated"))?)?;
    let (kind, e, _) = entries.first().ok_or(err("stsd without entries"))?;
    let mut t = Track {
        id,
        timescale,
        codec: None,
        unsupported: None,
        extradata: Vec::new(),
        size: None,
        audio: None,
        language,
        defaults: Defaults::default(),
    };
    sample_entry(*kind, e, &mut t)?;
    if t.codec == Some(CodecId::Aac)
        && let Some((rate, ch)) = asc_rate_channels(&t.extradata)
    {
        t.audio = Some((ch, rate));
    }
    Ok(Some(t))
}

/// Sampling rate and channel configuration of an AudioSpecificConfig (ISO/IEC 14496-3
/// §1.6.2.1), for the hint.
fn asc_rate_channels(asc: &[u8]) -> Option<(u32, u8)> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let v = u32::from(*asc.first()?) << 8 | u32::from(*asc.get(1)?);
    let aot = v >> 11;
    if aot == 31 {
        return None;
    }
    let fi = ((v >> 7) & 15) as usize;
    let ch = ((v >> 3) & 15) as u8;
    Some((*RATES.get(fi)?, ch))
}

impl Demuxer {
    /// An empty demuxer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads an init segment (`ftyp` + `moov`); the tracks it has, audio and video only.
    pub fn init(&mut self, bytes: &[u8]) -> Result<Vec<Track>, Fmp4Error> {
        let top = boxes(bytes)?;
        let moov = top
            .iter()
            .find(|(k, _, _)| k == b"moov")
            .map(|(_, p, _)| *p)
            .ok_or(err("init segment without moov"))?;
        let mut tracks = BTreeMap::new();
        for (k, p, _) in boxes(moov)? {
            if &k == b"trak"
                && let Some(t) = track(p)?
            {
                tracks.insert(t.id, t);
            }
        }
        if let Some(mvex) = child(moov, b"mvex")? {
            for (k, p, _) in boxes(mvex)? {
                if &k == b"trex"
                    && let Some(t) = tracks.get_mut(&be32(p, 4)?)
                {
                    t.defaults = Defaults {
                        duration: be32(p, 12)?,
                        size: be32(p, 16)?,
                        flags: be32(p, 20)?,
                    };
                }
            }
        }
        self.tracks = tracks;
        Ok(self.tracks.values().cloned().collect())
    }

    /// The tracks of the last init segment.
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.values()
    }

    /// The samples of every `moof` + `mdat` in `bytes` (a segment or a part), in order.
    /// Samples of tracks the init segment did not have are skipped.
    pub fn fragment(&self, bytes: &[u8]) -> Result<Vec<Sample>, Fmp4Error> {
        let mut out = Vec::new();
        for (k, moof, at) in boxes(bytes)? {
            if &k != b"moof" {
                continue;
            }
            for (k, traf, _) in boxes(moof)? {
                if &k != b"traf" {
                    continue;
                }
                self.traf(bytes, at, traf, &mut out)?;
            }
        }
        Ok(out)
    }

    fn traf(
        &self,
        bytes: &[u8],
        moof_at: usize,
        traf: &[u8],
        out: &mut Vec<Sample>,
    ) -> Result<(), Fmp4Error> {
        let tfhd = child(traf, b"tfhd")?.ok_or(err("traf without tfhd"))?;
        let flags = be32(tfhd, 0)? & 0x00FF_FFFF;
        let id = be32(tfhd, 4)?;
        let Some(t) = self.tracks.get(&id) else {
            return Ok(());
        };
        let mut at = 8;
        let mut base = moof_at as u64;
        if flags & 0x01 != 0 {
            base = be64(tfhd, at)?;
            at += 8;
        }
        if flags & 0x02 != 0 {
            at += 4;
        }
        let mut d = t.defaults;
        if flags & 0x08 != 0 {
            d.duration = be32(tfhd, at)?;
            at += 4;
        }
        if flags & 0x10 != 0 {
            d.size = be32(tfhd, at)?;
            at += 4;
        }
        if flags & 0x20 != 0 {
            d.flags = be32(tfhd, at)?;
        }
        let mut dts = match child(traf, b"tfdt")? {
            Some(b) if b.first() == Some(&1) => be64(b, 4)? as i64,
            Some(b) => i64::from(be32(b, 4)?),
            None => 0,
        };
        let mut next_data = base;
        for (k, trun, _) in boxes(traf)? {
            if &k != b"trun" {
                continue;
            }
            let v = trun.first().copied().unwrap_or(0);
            let f = be32(trun, 0)? & 0x00FF_FFFF;
            let count = be32(trun, 4)? as usize;
            let mut at = 8;
            let mut pos = next_data;
            if f & 0x01 != 0 {
                pos = base.wrapping_add_signed(i64::from(be32(trun, at)? as i32));
                at += 4;
            }
            let mut first_flags = None;
            if f & 0x04 != 0 {
                first_flags = Some(be32(trun, at)?);
                at += 4;
            }
            if count > 1 << 20 {
                return Err(err("trun sample count too large"));
            }
            for i in 0..count {
                let mut field = |bit: u32| -> Result<Option<u32>, Fmp4Error> {
                    if f & bit != 0 {
                        let x = be32(trun, at)?;
                        at += 4;
                        Ok(Some(x))
                    } else {
                        Ok(None)
                    }
                };
                let dur = field(0x100)?.unwrap_or(d.duration);
                let size = field(0x200)?.unwrap_or(d.size);
                let sflags = field(0x400)?
                    .or(if i == 0 { first_flags } else { None })
                    .unwrap_or(d.flags);
                let cto = field(0x800)?.map_or(0, |c| {
                    if v == 0 {
                        i64::from(c)
                    } else {
                        i64::from(c as i32)
                    }
                });
                let start = usize::try_from(pos).map_err(|_| err("data offset"))?;
                let end = start
                    .checked_add(size as usize)
                    .ok_or_else(|| err("sample size"))?;
                let data = bytes
                    .get(start..end)
                    .ok_or_else(|| err(format!("track {id}: sample outside the data")))?;
                out.push(Sample {
                    track: id,
                    dts,
                    pts: dts + cto,
                    key: sflags & 0x0001_0000 == 0,
                    data: data.to_vec(),
                });
                pos += u64::from(size);
                dts += i64::from(dur);
            }
            next_data = pos;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A box of `kind` around `payload`.
    pub fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    fn full(version: u8, flags: u32, rest: &[u8]) -> Vec<u8> {
        let mut v = vec![version];
        v.extend_from_slice(&flags.to_be_bytes()[1..]);
        v.extend_from_slice(rest);
        v
    }

    /// An init segment: track 1 avc1 (90 kHz, 1280×720), track 2 mp4a AAC-LC 48 kHz stereo
    /// in Spanish.
    pub fn init() -> Vec<u8> {
        let tkhd = |id: u32| {
            bx(
                b"tkhd",
                &full(0, 3, &[&[0u8; 8][..], &id.to_be_bytes(), &[0; 68]].concat()),
            )
        };
        let mdhd = |ts: u32, lang: u16| {
            bx(
                b"mdhd",
                &full(
                    0,
                    0,
                    &[
                        &[0u8; 8][..],
                        &ts.to_be_bytes(),
                        &[0; 4],
                        &lang.to_be_bytes(),
                        &[0; 2],
                    ]
                    .concat(),
                ),
            )
        };
        let hdlr = |h: &[u8; 4]| bx(b"hdlr", &full(0, 0, &[&[0u8; 4][..], h, &[0; 13]].concat()));
        let stsd = |entry: Vec<u8>| {
            bx(
                b"stsd",
                &full(0, 0, &[&1u32.to_be_bytes()[..], &entry].concat()),
            )
        };
        let stbl = |entry| bx(b"stbl", &stsd(entry));
        let trak = |id, ts, lang, h: &[u8; 4], entry| {
            let mdia = bx(
                b"mdia",
                &[mdhd(ts, lang), hdlr(h), bx(b"minf", &stbl(entry))].concat(),
            );
            bx(b"trak", &[tkhd(id), mdia].concat())
        };
        let mut visual = vec![0u8; 78];
        visual[24..26].copy_from_slice(&1280u16.to_be_bytes());
        visual[26..28].copy_from_slice(&720u16.to_be_bytes());
        visual.extend(bx(b"avcC", &[1, 0x64, 0, 0x1F, 0xFF]));
        let avc1 = bx(b"avc1", &visual);
        let mut audio = vec![0u8; 28];
        audio[16..18].copy_from_slice(&2u16.to_be_bytes());
        audio[24..28].copy_from_slice(&(48000u32 << 16).to_be_bytes());
        // ES_Descriptor { ES_ID 1, DecoderConfig { OTI 0x40, DSI [0x11, 0x90] } }
        let dsi = [0x05, 2, 0x11, 0x90];
        let dc = [
            &[0x04, 13 + dsi.len() as u8, 0x40, 0x15][..],
            &[0; 11],
            &dsi,
        ]
        .concat();
        let es = [&[0x03, 3 + dc.len() as u8, 0, 1, 0][..], &dc].concat();
        audio.extend(bx(b"esds", &full(0, 0, &es)));
        let mp4a = bx(b"mp4a", &audio);
        // "spa": s=19, p=16, a=1
        let spa = (19 << 10) | (16 << 5) | 1;
        let und = (21 << 10) | (14 << 5) | 4;
        let trex = |id: u32, dur: u32| {
            bx(
                b"trex",
                &full(
                    0,
                    0,
                    &[
                        &id.to_be_bytes()[..],
                        &1u32.to_be_bytes(),
                        &dur.to_be_bytes(),
                        &[0; 8],
                    ]
                    .concat(),
                ),
            )
        };
        let moov = bx(
            b"moov",
            &[
                trak(1, 90_000, und, b"vide", avc1),
                trak(2, 48_000, spa, b"soun", mp4a),
                bx(b"mvex", &[trex(1, 3003), trex(2, 1024)].concat()),
            ]
            .concat(),
        );
        [bx(b"ftyp", b"iso6\0\0\0\0"), moov].concat()
    }

    /// A fragment of track `id`: decode time `base`, samples `(size, cto, key)` with the
    /// trex default duration, data offset from the moof.
    pub fn fragment(id: u32, base: u64, samples: &[(u32, i32, bool)], fill: u8) -> Vec<u8> {
        let mut entries = Vec::new();
        for (size, cto, key) in samples {
            entries.extend_from_slice(&size.to_be_bytes());
            let flags: u32 = if *key { 0x0200_0000 } else { 0x0101_0000 };
            entries.extend_from_slice(&flags.to_be_bytes());
            entries.extend_from_slice(&cto.to_be_bytes());
        }
        let build = |data_offset: i32| {
            let trun = bx(
                b"trun",
                &full(
                    1,
                    0x01 | 0x200 | 0x400 | 0x800,
                    &[
                        &(samples.len() as u32).to_be_bytes()[..],
                        &data_offset.to_be_bytes(),
                        &entries,
                    ]
                    .concat(),
                ),
            );
            let traf = bx(
                b"traf",
                &[
                    bx(b"tfhd", &full(0, 0x02_0000, &id.to_be_bytes())),
                    bx(b"tfdt", &full(1, 0, &base.to_be_bytes())),
                    trun,
                ]
                .concat(),
            );
            bx(
                b"moof",
                &[bx(b"mfhd", &full(0, 0, &1u32.to_be_bytes())), traf].concat(),
            )
        };
        let moof_len = build(0).len();
        let total: u32 = samples.iter().map(|s| s.0).sum();
        let data: Vec<u8> = (0..total).map(|i| fill.wrapping_add(i as u8)).collect();
        [build(moof_len as i32 + 8), bx(b"mdat", &data)].concat()
    }

    #[test]
    fn init_segment_tracks() {
        let mut d = Demuxer::new();
        let t = d.init(&init()).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(
            (t[0].id, t[0].timescale, t[0].codec),
            (1, 90_000, Some(CodecId::H264))
        );
        assert_eq!(t[0].size, Some((1280, 720)));
        assert_eq!(t[0].extradata, [1, 0x64, 0, 0x1F, 0xFF]);
        assert_eq!(t[0].language, None, "und is no language");
        assert_eq!(t[1].codec, Some(CodecId::Aac));
        assert_eq!(t[1].extradata, [0x11, 0x90]);
        assert_eq!(t[1].audio, Some((2, 48_000)));
        assert_eq!(t[1].language, Some(*b"spa"));
    }

    #[test]
    fn fragment_samples() {
        let mut d = Demuxer::new();
        d.init(&init()).unwrap();
        let f = [
            fragment(1, 900_000, &[(5, 3003, true), (3, -3003, false)], 0x10),
            fragment(2, 480_000, &[(4, 0, true)], 0x80),
        ]
        .concat();
        let s = d.fragment(&f).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            (s[0].track, s[0].dts, s[0].pts, s[0].key),
            (1, 900_000, 903_003, true)
        );
        assert_eq!(s[0].data, [0x10, 0x11, 0x12, 0x13, 0x14]);
        assert_eq!((s[1].dts, s[1].pts, s[1].key), (903_003, 900_000, false));
        assert_eq!(s[1].data, [0x15, 0x16, 0x17]);
        assert_eq!((s[2].track, s[2].dts, s[2].data.len()), (2, 480_000, 4));
        // a sample pointing past the data is an error, not a panic
        let mut bad = fragment(1, 0, &[(5, 0, true)], 0);
        bad.truncate(bad.len() - 2);
        assert!(d.fragment(&bad).is_err());
    }

    #[test]
    fn opus_head_from_dops() {
        let dops = [0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0];
        let h = opus_head(&dops).unwrap();
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!(h[9], 2);
        assert_eq!(u16::from_le_bytes([h[10], h[11]]), 312);
        assert_eq!(u32::from_le_bytes([h[12], h[13], h[14], h[15]]), 48_000);
    }
}
