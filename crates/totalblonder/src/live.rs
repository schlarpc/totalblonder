//! Live inputs end to end (SPEC §9): the input's socket thread (`ingest`) hands its
//! `tb_media` events to a decoder thread, which drives the sandboxed decoder
//! (`tb-decoder-client`; libav runs in a confined child process) and puts what comes back on the
//! program's presentation timeline:
//!
//! ```text
//!  socket ─► ingest thread ─InputEvent─► decoder thread ─┬─ pictures (program size) ─► LiveProvider
//!            (tb-rtmp, tb-tsin)          (Decoder, Mapper) └─ audio (48 kHz, A/52 order) ─► audio Track
//! ```
//!
//! One `tb_timeline::Mapper` per input maps both media, so audio and video keep the producer's
//! relative timing (SPEC §5.4). Frames are handed to the mapper as if they had arrived `Δ` later
//! than they did, so its jitter buffer `J` sits on the presentation timeline. A decoded picture
//! is a whole picture, so every one may start a mapper session.
//!
//! Audio: each of the program's tracks takes one of the input's audio streams
//! ([`crate::audio_routes`]); only the streams some track takes are opened, each resampled to
//! the layout of the first track it feeds, and its blocks go to every track it feeds.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use tb_decoder_client::proto::wire::{
    Aspect, AudioLayout, AudioSpec, Deinterlace, OutputSpec, StreamOpen, VideoSpec,
};
use tb_decoder_client::{Decoded, Decoder, DecoderPool, Event};
use tb_media::{InputEvent, MediaKind, Rational, StreamIndex};
use tb_timeline::{Mapper, MapperConfig, Ticks27};

use crate::audio::LiveAudio;
use crate::audio_routes::{InputAudio, TrackWant, assign};
use crate::config::{AudioPlan, ProgramPlan};
use crate::ingest::{EventSink, InputStats};
use crate::metrics::Counter;
use crate::source::LiveEvent;
use crate::time::Clock;

/// The ingest thread's sink: a bounded queue to the decoder thread (full: drop and count).
pub struct ChannelSink {
    tx: SyncSender<InputEvent>,
    /// Events dropped because the decoder thread was behind.
    pub dropped: Counter,
}

impl EventSink for ChannelSink {
    fn event(&mut self, ev: InputEvent) {
        if self.tx.try_send(ev).is_err() {
            self.dropped.inc();
        }
    }
}

/// Counters of a live input's decode path.
#[derive(Debug, Clone, Default)]
pub struct LiveStats {
    /// Pictures handed to the program.
    pub pictures: Counter,
    /// Audio blocks handed to the program.
    pub audio_blocks: Counter,
    /// Decoder worker exits (crashes, kills, violations).
    pub worker_exits: Counter,
    /// Mapper sessions started.
    pub sessions: Counter,
}

/// Where a live input's output goes and in what form.
pub struct LiveTarget {
    /// The program's video queue.
    pub video: SyncSender<LiveEvent>,
    /// Each audio track's queue, in track order.
    pub audio: Vec<SyncSender<LiveAudio>>,
    /// What each track asks of the input, and its layout.
    pub audio_tracks: Vec<(TrackWant, AudioLayout)>,
    /// Pictures at the program's size.
    pub video_spec: VideoSpec,
    /// Pictures at each rung of the program's ladder (`video_spec` is rung 0).
    pub specs: Vec<VideoSpec>,
    /// Whether each rung's format is interlaced (for the bar data's line numbers).
    pub interlaced: Vec<bool>,
    /// The program's rung now: the thread follows it.
    pub rung: crate::config::Rung,
    /// Δ.
    pub delay: u64,
}

/// A track's layout as the worker delivers it.
pub fn audio_layout(a: &AudioPlan) -> AudioLayout {
    match a.channels {
        6 => AudioLayout::Surround51,
        1 => AudioLayout::Mono,
        _ => AudioLayout::Stereo,
    }
}

/// What each of a program's tracks asks of a live input, and its layout.
pub fn audio_tracks(p: &ProgramPlan) -> Vec<(TrackWant, AudioLayout)> {
    p.audio
        .iter()
        .map(|a| {
            (
                TrackWant {
                    language: a.lang.bytes(),
                    input_track: a.input_track,
                },
                audio_layout(a),
            )
        })
        .collect()
}

/// The streams to open for the worker, with their outputs, and which tracks each audio stream
/// feeds. Audio streams no track takes are left out (their packets are not sent).
fn opens(
    list: &[tb_media::StreamInfo],
    video: VideoSpec,
    tracks: &[(TrackWant, AudioLayout)],
) -> (Vec<StreamOpen>, HashMap<StreamIndex, Vec<usize>>) {
    let audio: Vec<InputAudio> = list
        .iter()
        .filter(|i| i.codec.kind() == MediaKind::Audio)
        .map(|i| InputAudio {
            index: i.index,
            language: i.language,
        })
        .collect();
    let wants: Vec<TrackWant> = tracks.iter().map(|t| t.0).collect();
    let mut routes: HashMap<StreamIndex, Vec<usize>> = HashMap::new();
    for (k, s) in assign(&audio, &wants).into_iter().enumerate() {
        if let Some(s) = s {
            routes.entry(s).or_default().push(k);
        }
    }
    let opens = list
        .iter()
        .filter_map(|info| {
            let output = match info.codec.kind() {
                MediaKind::Video => OutputSpec::Video(video),
                MediaKind::Audio => {
                    let k = *routes.get(&info.index)?.first()?;
                    OutputSpec::Audio(AudioSpec {
                        layout: tracks[k].1,
                    })
                }
                MediaKind::Data => OutputSpec::Native,
            };
            Some(StreamOpen {
                info: info.clone(),
                output,
            })
        })
        .collect();
    (opens, routes)
}

/// The worker's video output spec for a program.
pub fn specs(p: &ProgramPlan) -> VideoSpec {
    let (w, h) = p.format.size();
    let (an, ad) = p.format.display_aspect();
    VideoSpec {
        width: w,
        height: h,
        // the coded frame's aspect: the source is fitted into what the TV will show
        display_aspect: Rational::new(an, ad).unwrap_or(Rational::MILLIS),
        aspect: Aspect::Fit,
        // Interlaced outputs take frames as they come; the encoder codes them as frames.
        deinterlace: Deinterlace::Off,
    }
}

/// The Active Format Description and bar data of a converted picture: the rectangle its source
/// fills in a frame of `spec`, interlaced or not (the whole frame when the worker does not
/// say). The worker centers the rectangle on even coordinates (`tb-avworker`, its fit:
/// `x = (w − aw) / 4 · 2`, likewise `y`).
pub fn active_format(
    spec: &VideoSpec,
    interlaced: bool,
    active: Option<(u32, u32)>,
) -> tb_m2v_syntax::afd::ActiveFormat {
    use tb_m2v_syntax::afd::{ActiveFormat, Raster, describe};
    let (w, h) = (spec.width, spec.height);
    let (aw, ah) = active.unwrap_or((w, h));
    let Some(raster) = Raster::of(w, h, interlaced) else {
        return ActiveFormat::FULL;
    };
    let wide = spec.display_aspect.num() * 3 > spec.display_aspect.den() * 4;
    let (x, y) = (
        (w.saturating_sub(aw)) / 4 * 2,
        (h.saturating_sub(ah)) / 4 * 2,
    );
    describe(raster, wide, w, h, x, y, aw, ah)
}

/// The queue an ingest thread feeds and the decoder thread drains.
pub fn input_queue() -> (ChannelSink, Receiver<InputEvent>) {
    let (tx, rx) = sync_channel(4096);
    (
        ChannelSink {
            tx,
            dropped: Counter::default(),
        },
        rx,
    )
}

/// Snaps a source's picture times onto its nominal frame grid. Containers with millisecond
/// timestamps (FLV/RTMP) put a 23.976 Hz source's frames up to 0.5 ms off their true times;
/// the frame clock, sampling per field in film mode, then sees every other frame within a
/// millisecond of a field boundary and the 3:2 cadence wobbles. A time within a quarter period
/// of the grid is moved onto it; one further off re-anchors the grid (a cut, a jump).
#[derive(Debug, Clone, Copy)]
struct GridSnap {
    /// The period, 27 MHz ticks, as numerator / denominator (27e6 · den / num).
    p_num: i128,
    p_den: i128,
    anchor: Option<i64>,
}

impl GridSnap {
    fn new(rate: Rational) -> Self {
        GridSnap {
            p_num: 27_000_000 * i128::from(rate.den()),
            p_den: i128::from(rate.num()),
            anchor: None,
        }
    }

    fn snap(&mut self, t: i64) -> i64 {
        let Some(a) = self.anchor else {
            self.anchor = Some(t);
            return t;
        };
        let d = i128::from(t) - i128::from(a);
        // k = round(d / p) with p = p_num / p_den.
        let k = (2 * d * self.p_den + self.p_num).div_euclid(2 * self.p_num);
        let g = i128::from(a) + (k * self.p_num).div_euclid(self.p_den);
        if (i128::from(t) - g).abs() * 4 * self.p_den <= self.p_num {
            g as i64
        } else {
            self.anchor = Some(t);
            t
        }
    }
}

fn to_ticks(pts: i64, tb: Rational) -> i64 {
    tb.rescale(pts, Rational::MPEG_27M)
}

/// The decoder thread of one live input, until the clock stops.
pub fn run_decoder(
    pool: &DecoderPool,
    name: &str,
    rx: Receiver<InputEvent>,
    target: LiveTarget,
    stats: LiveStats,
    clock: &dyn Clock,
) {
    let mut dec: Decoder = match pool.spawn(name) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(input = name, "decoder: {e}");
            return;
        }
    };
    // The worker running now: one `decoder.worker` wide event per worker, from ready to exit,
    // with what it decoded.
    let mut worker: Option<(tb_telemetry::Wide, u64, u64)> = None;
    let mut rung = target.rung.get();
    let mut spec = target.specs.get(rung).copied().unwrap_or(target.video_spec);
    dec.set_video_output(Some(spec));
    // the session's streams, announced again with the new spec when the program changes format
    let mut streams: Option<Vec<tb_media::StreamInfo>> = None;
    // audio stream -> the tracks it feeds; the streams the worker has open
    let mut routes: HashMap<StreamIndex, Vec<usize>> = HashMap::new();
    let mut opened: std::collections::HashSet<StreamIndex> = Default::default();
    let announce = |dec: &mut Decoder,
                    list: &[tb_media::StreamInfo],
                    spec: VideoSpec,
                    routes: &mut HashMap<StreamIndex, Vec<usize>>,
                    opened: &mut std::collections::HashSet<StreamIndex>| {
        let (o, r) = opens(list, spec, &target.audio_tracks);
        *opened = o.iter().map(|s| s.info.index).collect();
        if r != *routes {
            let mut desc: Vec<String> = r
                .iter()
                .map(|(s, ks)| {
                    let tracks: Vec<String> = ks.iter().map(|k| (k + 1).to_string()).collect();
                    format!("stream {} -> track {}", s.0, tracks.join("+"))
                })
                .collect();
            desc.sort();
            tracing::info!(input = name, "audio routes: {}", desc.join(", "));
            tb_telemetry::wide!("live.audio_routes", input = name, routes = desc.join(", ")).emit();
        }
        *routes = r;
        let _ = dec.try_send_streams(o);
    };
    let mut mapper = Mapper::new(MapperConfig::default());
    let mut bases: HashMap<StreamIndex, (Rational, MediaKind)> = HashMap::new();
    let mut grids: HashMap<StreamIndex, GridSnap> = HashMap::new();
    let mut format_sent = vec![false; target.audio.len()];
    let delay = target.delay;
    while !clock.stopping() {
        // Everything the socket delivered goes to the worker (never blocking).
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    match &ev {
                        InputEvent::Streams(s) => {
                            announce(&mut dec, s, spec, &mut routes, &mut opened);
                            streams = Some(s.clone());
                            bases = s
                                .iter()
                                .map(|i| (i.index, (i.time_base, i.codec.kind())))
                                .collect();
                            grids = s
                                .iter()
                                .filter_map(|i| {
                                    i.video.frame_rate.map(|r| (i.index, GridSnap::new(r)))
                                })
                                .collect();
                        }
                        InputEvent::End(_) => {
                            let _ = mapper.closed();
                            let _ = target.video.try_send(LiveEvent::Closed);
                            for a in &target.audio {
                                let _ = a.try_send(LiveAudio::Closed);
                            }
                            format_sent.iter_mut().for_each(|f| *f = false);
                        }
                        // a stream no track takes is not decoded
                        InputEvent::Packet(p) if !opened.contains(&p.stream) => continue,
                        InputEvent::Packet(_) => {}
                    }
                    if !matches!(ev, InputEvent::Streams(_)) {
                        let _ = dec.try_send(ev);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // The program stepped to another format: the worker converts to the new size from the
        // next key frame of the streams announced again.
        let r = target.rung.get();
        if r != rung
            && let Some(&s) = target.specs.get(r)
        {
            rung = r;
            spec = s;
            dec.set_video_output(Some(spec));
            if let Some(list) = &streams {
                announce(&mut dec, list, spec, &mut routes, &mut opened);
            }
        }
        // What the worker made.
        let deadline = Instant::now() + Duration::from_millis(5);
        while let Some(d) = dec.poll(Some(deadline)) {
            let now = clock.now();
            let arrival = Ticks27((now + delay) as i64);
            let Some(&(tb, _)) = bases.get(&d.stream()) else {
                continue;
            };
            match d {
                Decoded::Video(f) => {
                    let Some(pts) = f.desc.pts else { continue };
                    let mut ticks = to_ticks(pts, tb);
                    if let Some(g) = grids.get_mut(&f.desc.stream) {
                        ticks = g.snap(ticks);
                    }
                    let m = mapper.frame(Ticks27(ticks), arrival, true);
                    if matches!(m, tb_timeline::Mapped::Started { .. }) {
                        stats.sessions.inc();
                    }
                    let Some(at) = m.at() else { continue };
                    let mut data = Vec::with_capacity(f.slot_bytes().len());
                    for i in 0..3 {
                        if let Some(p) = f.plane(i) {
                            data.extend_from_slice(p);
                        }
                    }
                    // a frame converted under the old spec is not the program's size: dropped
                    if f.desc.width != spec.width || f.desc.height != spec.height {
                        continue;
                    }
                    let il = target.interlaced.get(rung).copied().unwrap_or(true);
                    let afd = active_format(&spec, il, f.desc.active);
                    if target
                        .video
                        .try_send(LiveEvent::Frame {
                            at: at.0.max(0) as u64,
                            data,
                            interlaced: f.desc.interlaced,
                            afd,
                        })
                        .is_ok()
                    {
                        stats.pictures.inc();
                        if let Some(w) = worker.as_mut() {
                            w.1 += 1;
                        }
                    }
                }
                Decoded::Audio(a) => {
                    let Some(ks) = routes.get(&a.desc.stream) else {
                        continue;
                    };
                    let Some(pts) = a.desc.pts else { continue };
                    // Audio follows the video's session; before the first picture it waits.
                    let Some(at) = mapper.map(Ticks27(to_ticks(pts, tb))) else {
                        continue;
                    };
                    let mut fmt = None;
                    if ks.iter().any(|&k| !format_sent[k]) {
                        use tb_audio::layout::{InputLayout, Speaker};
                        // The worker resamples to the track's layout, in A/52 order for 5.1.
                        let layout = match a.desc.channels {
                            6 => InputLayout::new(&[
                                Speaker::FrontLeft,
                                Speaker::FrontCenter,
                                Speaker::FrontRight,
                                Speaker::SideLeft,
                                Speaker::SideRight,
                                Speaker::Lfe,
                            ]),
                            n => InputLayout::default_for(usize::from(n)),
                        };
                        let (Ok(layout), Ok(rate)) = (
                            layout,
                            tb_audio::resample::InputRate::new(a.desc.sample_rate),
                        ) else {
                            continue;
                        };
                        fmt = Some(tb_audio::pipeline::InputFormat { rate, layout });
                    }
                    let planes: Vec<Vec<f32>> = (0..usize::from(a.desc.channels))
                        .filter_map(|c| a.channel(c))
                        .collect();
                    for &k in ks {
                        let Some(tx) = target.audio.get(k) else {
                            continue;
                        };
                        if !format_sent[k] {
                            let Some(f) = fmt else { continue };
                            let _ = tx.try_send(LiveAudio::Format(f));
                            format_sent[k] = true;
                        }
                        if tx
                            .try_send(LiveAudio::Block {
                                at: at.0.max(0) as u64,
                                planes: planes.clone(),
                            })
                            .is_ok()
                        {
                            stats.audio_blocks.inc();
                            if let Some(w) = worker.as_mut() {
                                w.2 += 1;
                            }
                        }
                    }
                }
            }
        }
        let _ = mapper.poll(Ticks27((clock.now() + delay) as i64));
        for e in dec.events() {
            match e {
                Event::Exited { class, generation } => {
                    stats.worker_exits.inc();
                    tracing::warn!(input = name, "decoder worker exited: {class:?}");
                    let _ = target.video.try_send(LiveEvent::Closed);
                    let (ev, pictures, blocks) = worker.take().unwrap_or_else(|| {
                        (
                            tb_telemetry::wide!("decoder.worker", generation = generation),
                            0,
                            0,
                        )
                    });
                    ev.set("exit", format!("{class:?}"))
                        .set("pictures", pictures)
                        .set("audio_blocks", blocks);
                    if !matches!(class, tb_decoder_client::proto::supervise::ExitClass::Clean) {
                        ev.level(tracing::Level::WARN);
                    }
                }
                Event::Started {
                    generation,
                    confinement,
                } => {
                    tracing::info!(
                        input = name,
                        generation,
                        "decoder worker started: {confinement:?}"
                    );
                    worker = Some((
                        tb_telemetry::wide!(
                            "decoder.worker",
                            generation = generation,
                            confinement = format!("{confinement:?}"),
                        ),
                        0,
                        0,
                    ));
                }
                Event::CircuitOpen { for_ } => {
                    tracing::error!(input = name, "decoder failing; paused for {for_:?}");
                    tb_telemetry::wide!(
                        "decoder.circuit_open",
                        paused_ms = tb_telemetry::Val::ms(for_)
                    )
                    .emit();
                }
                Event::Violation { what } => {
                    tb_telemetry::wide!("decoder.violation", what = what)
                        .level(tracing::Level::ERROR);
                }
                other => tracing::debug!(input = name, "decoder: {other:?}"),
            }
        }
    }
    dec.shutdown(Duration::from_secs(1));
    if let Some((ev, pictures, blocks)) = worker.take() {
        ev.set("exit", "shutdown")
            .set("pictures", pictures)
            .set("audio_blocks", blocks);
    }
}

/// Counters of an input's socket side, re-exported for registration.
pub type SocketStats = InputStats;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millisecond_times_snap_to_the_film_grid() {
        let rate = Rational::new(24_000, 1001).unwrap();
        let mut g = GridSnap::new(rate);
        // FLV milliseconds of a 23.976 Hz source, in 27 MHz ticks.
        let mut out = Vec::new();
        for n in 0..200i64 {
            let ms = (n * 1001 + 12) / 24; // rounded to the millisecond
            out.push(g.snap(ms * 27_000));
        }
        let p = 1_126_125; // 1001/24000 s
        for w in out.windows(2) {
            assert_eq!(w[1] - w[0], p);
        }
        // A jump half a period off the grid re-anchors.
        let t = out[199] + 1000 * p + p / 2;
        assert_eq!(g.snap(t), t);
        assert_eq!(g.snap(t + p + 13_000), t + p);
    }

    #[test]
    fn active_format_of_the_fitted_rectangle() {
        use tb_m2v_syntax::afd::{ActiveFormat, Bars, LETTER_16_9, PILLAR_4_3, WIDER};
        let spec = |w, h, an, ad| VideoSpec {
            width: w,
            height: h,
            display_aspect: Rational::new(an, ad).unwrap(),
            aspect: Aspect::Fit,
            deinterlace: Deinterlace::Off,
        };
        let hd = spec(1920, 1080, 16, 9);
        assert_eq!(
            active_format(&hd, true, Some((1440, 1080))),
            ActiveFormat {
                afd: PILLAR_4_3,
                bars: Some(Bars::LeftRight {
                    left_end: 239,
                    right_start: 1680
                })
            }
        );
        assert_eq!(
            active_format(&hd, true, Some((1920, 1080))),
            ActiveFormat::FULL
        );
        assert_eq!(active_format(&hd, true, None), ActiveFormat::FULL);
        // 2.4:1 in 1080i: 800 lines, the bars of ST 2016-1 Table B.3
        assert_eq!(
            active_format(&hd, true, Some((1920, 800))),
            ActiveFormat {
                afd: WIDER,
                bars: Some(Bars::TopBottom {
                    top_end: 653,
                    bottom_start: 491
                })
            }
        );
        let sd = spec(704, 480, 4, 3);
        assert_eq!(active_format(&sd, true, Some((704, 360))).afd, LETTER_16_9);
        assert_eq!(
            active_format(&sd, true, Some((704, 480))),
            ActiveFormat::FULL
        );
    }
}
