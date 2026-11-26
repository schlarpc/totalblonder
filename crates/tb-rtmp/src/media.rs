//! From FLV tags to [`InputEvent`]s: which streams a publisher has, and its packets.
//!
//! An RTMP publisher has at most one video and one audio track. Their stream indices are fixed:
//! video is [`VIDEO`] (0) and audio is [`AUDIO`] (1), whether or not the other exists, so an
//! index never changes meaning within a connection.
//!
//! **When `Streams` is emitted.** A track is *known* once its configuration is: the sequence
//! header for AVC, HEVC, AV1, VP9, AAC and Opus; the first frame for MP3, PCM, AC-3 and E-AC-3,
//! which have none. Packets of a known track are held (in order) until `Streams` goes out;
//! packets of a track that is not known are dropped and counted. `Streams` is emitted when at
//! least one track is known and every other track is known or *absent*. A track is absent when
//! - `onMetaData` names the other track's codec (`videocodecid` / `audiocodecid`) and not this
//!   one's: ffmpeg's FLV muxer, which the producer uses, writes exactly the ids of the streams it
//!   has, and so does OBS; metadata that names neither says nothing;
//! - its codec is one this crate does not carry;
//! - or the wait is over: the held packets span [`MediaConfig::absent_after_ms`] of media time
//!   (1 s by default), or [`MediaConfig::absent_after`] of wall time has passed since the
//!   first media message (2 s), or the held packets reach [`MediaConfig::max_pending_bytes`].
//!
//! A video-only producer that sends `onMetaData` (as the first producer does) therefore gets `Streams` at its
//! sequence header, with no wait; one without metadata waits one second of its own media.
//!
//! **Later changes.** A sequence header whose codec or bytes differ from the current one, or a
//! track that becomes known after `Streams`, emits `Streams` again with every known track (a
//! reconfiguration for the decoder). An identical repeated sequence header is ignored.
//!
//! **Timestamps.** RTMP timestamps are milliseconds modulo 2^32; one [`MsUnwrapper`] per
//! connection (shared by both tracks, so they stay on one clock) makes them continuous. Video
//! `pts = dts + CompositionTime`. The time base is [`Rational::MILLIS`].

use std::collections::VecDeque;
use std::time::Duration;

use tb_media::{
    AudioHint, CodecId, InputEvent, Packet, Rational, StreamIndex, StreamInfo, VideoHint,
};

use crate::amf0::Amf0Value;
use crate::flv::{self, AudioTag, VideoTag};
use crate::types::RtmpTime;
use crate::unwrap::MsUnwrapper;

/// The stream index of the video track.
pub const VIDEO: StreamIndex = StreamIndex(0);
/// The stream index of the audio track.
pub const AUDIO: StreamIndex = StreamIndex(1);

/// What a held packet counts beyond its payload (its struct and queue slot, rounded up).
pub const PACKET_OVERHEAD: usize = 128;

/// When to stop waiting for a track (see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaConfig {
    /// Media time held packets may span before missing tracks are declared absent.
    pub absent_after_ms: u32,
    /// Wall time after the first media message before missing tracks are declared absent.
    pub absent_after: Duration,
    /// Most bytes held while waiting: payloads plus [`PACKET_OVERHEAD`] per packet, so that a
    /// flood of empty frames is bounded too.
    pub max_pending_bytes: usize,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            absent_after_ms: 1000,
            absent_after: Duration::from_secs(2),
            max_pending_bytes: 8 << 20,
        }
    }
}

/// Counters of what the assembler dropped or ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaStats {
    /// Frames of a track whose configuration was not yet known.
    pub dropped_before_config: u64,
    /// Tags that could not be parsed (dropped).
    pub malformed: u64,
    /// Tags of codecs or layouts not carried.
    pub unsupported: u64,
    /// Sequence headers identical to the current one.
    pub repeated_config: u64,
    /// Times `Streams` was emitted.
    pub announcements: u64,
}

/// What `onMetaData` said, as far as it matters here.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Metadata {
    /// `width` and `height`, `framerate`.
    pub video: VideoHint,
    /// `audiosamplerate`, `audiochannels` / `stereo`.
    pub audio: AudioHint,
    /// `videocodecid` present.
    pub names_video: bool,
    /// `audiocodecid` present.
    pub names_audio: bool,
}

impl Metadata {
    /// Read an `onMetaData` object (or ECMA array).
    pub fn from_amf(v: &Amf0Value) -> Self {
        let num = |k: &str| v.get(k).and_then(Amf0Value::as_f64);
        let dim = |k: &str| {
            num(k)
                .filter(|x| x.is_finite() && *x >= 1.0 && *x <= 65_535.0)
                .map(|x| x as u32)
        };
        let channels = num("audiochannels")
            .filter(|x| x.is_finite() && *x >= 1.0 && *x <= 64.0)
            .map(|x| x as u8)
            .or_else(|| match v.get("stereo") {
                Some(Amf0Value::Boolean(true)) => Some(2),
                Some(Amf0Value::Boolean(false)) => Some(1),
                _ => None,
            });
        Self {
            video: VideoHint {
                width: dim("width"),
                height: dim("height"),
                frame_rate: num("framerate").and_then(frame_rate),
            },
            audio: AudioHint {
                sample_rate: num("audiosamplerate")
                    .filter(|x| x.is_finite() && *x >= 1.0 && *x <= 1.0e6)
                    .map(|x| x as u32),
                channels,
            },
            names_video: v.get("videocodecid").is_some(),
            names_audio: v.get("audiocodecid").is_some(),
        }
    }
}

/// A frame rate in frames per second as a rational: integers exactly, the NTSC family
/// (`n * 1000 / 1001`) exactly, anything else to a thousandth.
fn frame_rate(f: f64) -> Option<Rational> {
    if !f.is_finite() || !(0.5..=1000.0).contains(&f) {
        return None;
    }
    let r = f.round();
    if (f - r).abs() < 1e-3 {
        return Rational::new(r as u32, 1).ok();
    }
    let n = (f * 1.001).round();
    if (f - n * 1000.0 / 1001.0).abs() < 1e-3 {
        return Rational::new(n as u32 * 1000, 1001).ok();
    }
    Rational::new((f * 1000.0).round() as u32, 1000).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackConfig {
    codec: CodecId,
    extradata: Vec<u8>,
    audio: AudioHint,
}

#[derive(Debug, Clone, Default)]
struct Track {
    config: Option<TrackConfig>,
    unsupported: bool,
}

/// Assembles one connection's media into events.
#[derive(Debug, Clone)]
pub struct Assembler {
    config: MediaConfig,
    video: Track,
    audio: Track,
    meta: Metadata,
    announced: bool,
    gave_up_waiting: bool,
    pending: VecDeque<Packet>,
    pending_bytes: usize,
    first_dts: Option<i64>,
    first_wall: Option<Duration>,
    unwrap: MsUnwrapper,
    stats: MediaStats,
}

impl Assembler {
    /// A fresh assembler.
    pub fn new(config: MediaConfig) -> Self {
        Self {
            config,
            video: Track::default(),
            audio: Track::default(),
            meta: Metadata::default(),
            announced: false,
            gave_up_waiting: false,
            pending: VecDeque::new(),
            pending_bytes: 0,
            first_dts: None,
            first_wall: None,
            unwrap: MsUnwrapper::new(),
            stats: MediaStats::default(),
        }
    }

    /// Counters.
    pub fn stats(&self) -> MediaStats {
        self.stats
    }

    /// `Streams` has been emitted at least once.
    pub fn announced(&self) -> bool {
        self.announced
    }

    /// `onMetaData` arrived.
    pub fn on_metadata(&mut self, meta: Metadata, now: Duration, out: &mut Vec<InputEvent>) {
        self.meta = meta;
        self.maybe_announce(now, out);
    }

    /// A video message.
    pub fn on_video(
        &mut self,
        now: Duration,
        ts: RtmpTime,
        payload: &[u8],
        out: &mut Vec<InputEvent>,
    ) {
        let dts = self.unwrap.unwrap(ts);
        self.first_wall.get_or_insert(now);
        match flv::parse_video(payload) {
            Err(_) => self.stats.malformed += 1,
            Ok(VideoTag::Unsupported { .. }) => {
                self.stats.unsupported += 1;
                if self.video.config.is_none() {
                    self.video.unsupported = true;
                }
            }
            Ok(VideoTag::Info | VideoTag::SequenceEnd { .. }) => {}
            Ok(VideoTag::SequenceStart { codec, config }) => {
                let c = TrackConfig {
                    codec,
                    extradata: config,
                    audio: AudioHint::default(),
                };
                self.set_config(VIDEO, c, out);
            }
            Ok(VideoTag::Frame {
                codec,
                key,
                cts,
                data,
            }) => {
                if self.video.config.as_ref().map(|c| c.codec) != Some(codec) {
                    self.stats.dropped_before_config += 1;
                } else {
                    let p = Packet {
                        stream: VIDEO,
                        pts: Some(dts.saturating_add(i64::from(cts))),
                        dts: Some(dts),
                        duration: None,
                        keyframe: key,
                        data,
                    };
                    self.push(p, dts, out);
                }
            }
        }
        self.maybe_announce(now, out);
    }

    /// An audio message.
    pub fn on_audio(
        &mut self,
        now: Duration,
        ts: RtmpTime,
        payload: &[u8],
        out: &mut Vec<InputEvent>,
    ) {
        let dts = self.unwrap.unwrap(ts);
        self.first_wall.get_or_insert(now);
        match flv::parse_audio(payload) {
            Err(_) => self.stats.malformed += 1,
            Ok(AudioTag::Unsupported { .. }) => {
                self.stats.unsupported += 1;
                if self.audio.config.is_none() {
                    self.audio.unsupported = true;
                }
            }
            Ok(AudioTag::Info) => {}
            Ok(AudioTag::SequenceStart { codec, config }) => {
                let (rate, ch) = match codec {
                    CodecId::Aac => flv::audio_specific_config(&config),
                    CodecId::Opus => flv::opus_head(&config),
                    _ => None,
                }
                .unwrap_or_default();
                let c = TrackConfig {
                    codec,
                    extradata: config,
                    audio: AudioHint {
                        sample_rate: (rate > 0).then_some(rate),
                        channels: (ch > 0).then_some(ch),
                    },
                };
                self.set_config(AUDIO, c, out);
            }
            Ok(AudioTag::Frame {
                codec,
                header,
                data,
            }) => {
                let needs_config = matches!(codec, CodecId::Aac | CodecId::Opus);
                let current = self.audio.config.as_ref().map(|c| c.codec);
                if current != Some(codec) && !needs_config {
                    // Codecs with no sequence header are configured by their first frame.
                    let hint = if header.rate > 0 {
                        AudioHint {
                            sample_rate: Some(header.rate),
                            channels: Some(header.channels),
                        }
                    } else {
                        AudioHint::default()
                    };
                    let c = TrackConfig {
                        codec,
                        extradata: Vec::new(),
                        audio: hint,
                    };
                    self.set_config(AUDIO, c, out);
                }
                if self.audio.config.as_ref().map(|c| c.codec) != Some(codec) {
                    self.stats.dropped_before_config += 1;
                } else {
                    let p = Packet {
                        stream: AUDIO,
                        pts: Some(dts),
                        dts: Some(dts),
                        duration: None,
                        keyframe: true,
                        data,
                    };
                    self.push(p, dts, out);
                }
            }
        }
        self.maybe_announce(now, out);
    }

    /// Time passes (wall-clock wait for a missing track).
    pub fn on_tick(&mut self, now: Duration, out: &mut Vec<InputEvent>) {
        self.maybe_announce(now, out);
    }

    /// The session is ending: announce whatever is known and release held packets, so that
    /// nothing received is lost before `End`.
    pub fn finish(&mut self, out: &mut Vec<InputEvent>) {
        if !self.announced && (self.video.config.is_some() || self.audio.config.is_some()) {
            self.announce(out);
        }
    }

    fn track(&mut self, index: StreamIndex) -> &mut Track {
        if index == VIDEO {
            &mut self.video
        } else {
            &mut self.audio
        }
    }

    fn set_config(&mut self, index: StreamIndex, c: TrackConfig, out: &mut Vec<InputEvent>) {
        let t = self.track(index);
        if t.config.as_ref() == Some(&c) {
            self.stats.repeated_config += 1;
            return;
        }
        let had = t.config.replace(c).is_some();
        t.unsupported = false;
        if self.announced {
            self.announce(out);
        } else if had {
            // Held packets of the old configuration would be misdescribed: drop them.
            let before = self.pending.len();
            self.pending.retain(|p| p.stream != index);
            self.pending_bytes = self
                .pending
                .iter()
                .map(|p| p.data.len() + PACKET_OVERHEAD)
                .sum();
            self.stats.dropped_before_config += (before - self.pending.len()) as u64;
        }
    }

    fn push(&mut self, p: Packet, dts: i64, out: &mut Vec<InputEvent>) {
        if self.announced {
            out.push(InputEvent::Packet(p));
            return;
        }
        let first = *self.first_dts.get_or_insert(dts);
        if dts.saturating_sub(first) >= i64::from(self.config.absent_after_ms) {
            self.gave_up_waiting = true;
        }
        self.pending_bytes += p.data.len() + PACKET_OVERHEAD;
        self.pending.push_back(p);
        if self.pending_bytes >= self.config.max_pending_bytes {
            self.gave_up_waiting = true;
        }
    }

    fn absent(&self, t: &Track, named_other: bool, named_self: bool) -> bool {
        t.unsupported || self.gave_up_waiting || (named_other && !named_self)
    }

    fn maybe_announce(&mut self, now: Duration, out: &mut Vec<InputEvent>) {
        if self.announced {
            return;
        }
        if let Some(t0) = self.first_wall
            && now.saturating_sub(t0) >= self.config.absent_after
        {
            self.gave_up_waiting = true;
        }
        let v = &self.video;
        let a = &self.audio;
        if v.config.is_none() && a.config.is_none() {
            return;
        }
        let (nv, na) = (self.meta.names_video, self.meta.names_audio);
        let v_ok = v.config.is_some() || self.absent(v, na, nv);
        let a_ok = a.config.is_some() || self.absent(a, nv, na);
        if v_ok && a_ok {
            self.announce(out);
        }
    }

    fn announce(&mut self, out: &mut Vec<InputEvent>) {
        let mut streams = Vec::new();
        if let Some(c) = &self.video.config {
            streams.push(StreamInfo {
                index: VIDEO,
                codec: c.codec,
                time_base: Rational::MILLIS,
                extradata: c.extradata.clone(),
                video: self.meta.video,
                audio: AudioHint::default(),
                language: None,
            });
        }
        if let Some(c) = &self.audio.config {
            let audio = AudioHint {
                sample_rate: c.audio.sample_rate.or(self.meta.audio.sample_rate),
                channels: c.audio.channels.or(self.meta.audio.channels),
            };
            streams.push(StreamInfo {
                index: AUDIO,
                codec: c.codec,
                time_base: Rational::MILLIS,
                extradata: c.extradata.clone(),
                video: VideoHint::default(),
                audio,
                language: None,
            });
        }
        out.push(InputEvent::Streams(streams));
        self.stats.announcements += 1;
        self.announced = true;
        out.extend(self.pending.drain(..).map(InputEvent::Packet));
        self.pending_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVC_SEQ: &[u8] = &[0x17, 0, 0, 0, 0, 1, 0x64, 0, 0x1F];
    const AAC_SEQ: &[u8] = &[0xAF, 0, 0x12, 0x10];

    fn key(cts: u8) -> Vec<u8> {
        vec![0x17, 1, 0, 0, cts, 0, 0, 0, 1, 0x65]
    }

    fn streams(ev: &[InputEvent]) -> Vec<Vec<CodecId>> {
        ev.iter()
            .filter_map(|e| match e {
                InputEvent::Streams(s) => Some(s.iter().map(|s| s.codec).collect()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn video_only_with_metadata_is_immediate() {
        let mut a = Assembler::new(MediaConfig::default());
        let mut ev = Vec::new();
        let meta = Amf0Value::EcmaArray(vec![
            ("width".into(), Amf0Value::Number(1920.0)),
            ("height".into(), Amf0Value::Number(1080.0)),
            ("framerate".into(), Amf0Value::Number(29.97)),
            ("videocodecid".into(), Amf0Value::Number(7.0)),
        ]);
        let t = Duration::ZERO;
        a.on_metadata(Metadata::from_amf(&meta), t, &mut ev);
        a.on_video(t, RtmpTime(0), AVC_SEQ, &mut ev);
        assert_eq!(streams(&ev), [vec![CodecId::H264]]);
        let InputEvent::Streams(s) = &ev[0] else {
            panic!()
        };
        assert_eq!(s[0].video.width, Some(1920));
        assert_eq!(
            s[0].video.frame_rate,
            Some(Rational::new(30000, 1001).unwrap())
        );
        assert_eq!(s[0].extradata, &AVC_SEQ[5..]);
        a.on_video(t, RtmpTime(0), &key(66), &mut ev);
        let InputEvent::Packet(p) = &ev[1] else {
            panic!()
        };
        assert_eq!((p.dts, p.pts, p.keyframe), (Some(0), Some(66), true));
    }

    #[test]
    fn audio_and_video_wait_for_both() {
        let mut a = Assembler::new(MediaConfig::default());
        let mut ev = Vec::new();
        let t = Duration::ZERO;
        a.on_video(t, RtmpTime(0), AVC_SEQ, &mut ev);
        a.on_video(t, RtmpTime(0), &key(0), &mut ev);
        assert!(ev.is_empty());
        a.on_audio(t, RtmpTime(0), AAC_SEQ, &mut ev);
        assert_eq!(streams(&ev), [vec![CodecId::H264, CodecId::Aac]]);
        let InputEvent::Streams(s) = &ev[0] else {
            panic!()
        };
        assert_eq!(s[1].audio.sample_rate, Some(44100));
        assert_eq!(ev.len(), 2, "the held key frame follows");
    }

    #[test]
    fn missing_audio_times_out_by_media_time_and_wall_time() {
        let mut a = Assembler::new(MediaConfig::default());
        let mut ev = Vec::new();
        let t = Duration::ZERO;
        a.on_video(t, RtmpTime(0), AVC_SEQ, &mut ev);
        for i in 0..30u32 {
            a.on_video(t, RtmpTime(i * 33), &key(0), &mut ev);
        }
        assert!(ev.is_empty());
        a.on_video(t, RtmpTime(1000), &key(0), &mut ev);
        assert_eq!(streams(&ev), [vec![CodecId::H264]]);
        assert_eq!(ev.len(), 32);
        // A late audio track reconfigures.
        a.on_audio(t, RtmpTime(1000), AAC_SEQ, &mut ev);
        assert_eq!(
            streams(&ev),
            [vec![CodecId::H264], vec![CodecId::H264, CodecId::Aac]]
        );

        let mut a = Assembler::new(MediaConfig::default());
        let mut ev = Vec::new();
        a.on_video(Duration::from_millis(10), RtmpTime(0), AVC_SEQ, &mut ev);
        a.on_tick(Duration::from_millis(2000), &mut ev);
        assert!(ev.is_empty());
        a.on_tick(Duration::from_millis(2010), &mut ev);
        assert_eq!(streams(&ev), [vec![CodecId::H264]]);
    }

    #[test]
    fn changed_sequence_header_reannounces_and_repeats_do_not() {
        let mut a = Assembler::new(MediaConfig::default());
        let mut ev = Vec::new();
        let t = Duration::ZERO;
        let meta = Amf0Value::object([("videocodecid", Amf0Value::Number(7.0))]);
        a.on_metadata(Metadata::from_amf(&meta), t, &mut ev);
        a.on_video(t, RtmpTime(0), AVC_SEQ, &mut ev);
        a.on_video(t, RtmpTime(0), AVC_SEQ, &mut ev);
        assert_eq!(streams(&ev).len(), 1);
        let mut other = AVC_SEQ.to_vec();
        other.push(9);
        a.on_video(t, RtmpTime(0), &other, &mut ev);
        assert_eq!(streams(&ev).len(), 2);
        assert_eq!(a.stats().repeated_config, 1);
    }

    #[test]
    fn frames_before_config_are_dropped_and_mp3_configures_itself() {
        let mut a = Assembler::new(MediaConfig::default());
        let mut ev = Vec::new();
        let t = Duration::ZERO;
        a.on_audio(t, RtmpTime(0), &[0xAF, 1, 1, 2], &mut ev);
        assert_eq!(a.stats().dropped_before_config, 1);
        let meta = Amf0Value::object([("audiocodecid", Amf0Value::Number(2.0))]);
        a.on_metadata(Metadata::from_amf(&meta), t, &mut ev);
        a.on_audio(t, RtmpTime(0), &[0x2F, 0xFF, 0xFB], &mut ev);
        assert_eq!(streams(&ev), [vec![CodecId::MpegAudio]]);
        let InputEvent::Streams(s) = &ev[0] else {
            panic!()
        };
        assert_eq!(s[0].index, AUDIO);
        assert_eq!(s[0].audio.channels, Some(2));
    }

    #[test]
    fn frame_rates() {
        assert_eq!(frame_rate(30.0), Rational::new(30, 1).ok());
        assert_eq!(frame_rate(59.94), Rational::new(60000, 1001).ok());
        assert_eq!(frame_rate(23.976), Rational::new(24000, 1001).ok());
        assert_eq!(frame_rate(12.5), Rational::new(25, 2).ok());
        assert_eq!(frame_rate(f64::NAN), None);
    }
}
