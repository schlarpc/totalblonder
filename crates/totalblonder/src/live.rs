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

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::time::{Duration, Instant};

use tb_decoder_client::proto::wire::{Aspect, AudioLayout, AudioSpec, Deinterlace, VideoSpec};
use tb_decoder_client::{Decoded, Decoder, DecoderPool, Event};
use tb_media::{InputEvent, MediaKind, Rational, StreamIndex};
use tb_timeline::{Mapper, MapperConfig, Ticks27};

use crate::audio::LiveAudio;
use crate::config::ProgramPlan;
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
    /// Its first audio track's queue, if it has one.
    pub audio: Option<SyncSender<LiveAudio>>,
    /// Pictures at the program's size.
    pub video_spec: VideoSpec,
    /// Audio in the track's layout.
    pub audio_spec: Option<AudioSpec>,
    /// Δ.
    pub delay: u64,
}

/// The worker's output specs for a program.
pub fn specs(p: &ProgramPlan) -> (VideoSpec, Option<AudioSpec>) {
    let (w, h) = p.format.size();
    let (an, ad) = p.format.display_aspect();
    let video = VideoSpec {
        width: w,
        height: h,
        // the coded frame's aspect: the source is fitted into what the TV will show
        display_aspect: Rational::new(an, ad).unwrap_or(Rational::MILLIS),
        aspect: Aspect::Fit,
        // Interlaced outputs take frames as they come; the encoder codes them as frames.
        deinterlace: Deinterlace::Off,
    };
    let audio = p.audio.first().map(|a| AudioSpec {
        layout: if a.channels == 6 {
            AudioLayout::Surround51
        } else {
            AudioLayout::Stereo
        },
    });
    (video, audio)
}

/// The Active Format Description of a converted picture: the rectangle its source fills in a
/// frame of `spec` (the whole frame when the worker does not say).
pub fn active_format(spec: &VideoSpec, active: Option<(u32, u32)>) -> u8 {
    let frame = f64::from(spec.display_aspect.num()) / f64::from(spec.display_aspect.den());
    let (aw, ah) = active.unwrap_or((spec.width, spec.height));
    let wide = frame > 1.5;
    tb_m2v_syntax::afd::code(
        wide,
        tb_m2v_syntax::afd::content_aspect(frame, spec.width, spec.height, aw, ah),
    )
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
    dec.set_video_output(Some(target.video_spec));
    dec.set_audio_output(target.audio_spec);
    let mut mapper = Mapper::new(MapperConfig::default());
    let mut bases: HashMap<StreamIndex, (Rational, MediaKind)> = HashMap::new();
    let mut grids: HashMap<StreamIndex, GridSnap> = HashMap::new();
    let mut audio_format_sent = false;
    let delay = target.delay;
    while !clock.stopping() {
        // Everything the socket delivered goes to the worker (never blocking).
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    match &ev {
                        InputEvent::Streams(s) => {
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
                            if let Some(a) = &target.audio {
                                let _ = a.try_send(LiveAudio::Closed);
                            }
                            audio_format_sent = false;
                        }
                        InputEvent::Packet(_) => {}
                    }
                    let _ = dec.try_send(ev);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
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
                    let afd = active_format(&target.video_spec, f.desc.active);
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
                    }
                }
                Decoded::Audio(a) => {
                    let Some(tx) = &target.audio else { continue };
                    let Some(pts) = a.desc.pts else { continue };
                    // Audio follows the video's session; before the first picture it waits.
                    let Some(at) = mapper.map(Ticks27(to_ticks(pts, tb))) else {
                        continue;
                    };
                    if !audio_format_sent {
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
                        let _ = tx.try_send(LiveAudio::Format(tb_audio::pipeline::InputFormat {
                            rate,
                            layout,
                        }));
                        audio_format_sent = true;
                    }
                    let planes: Vec<Vec<f32>> = (0..usize::from(a.desc.channels))
                        .filter_map(|c| a.channel(c))
                        .collect();
                    if tx
                        .try_send(LiveAudio::Block {
                            at: at.0.max(0) as u64,
                            planes,
                        })
                        .is_ok()
                    {
                        stats.audio_blocks.inc();
                    }
                }
            }
        }
        let _ = mapper.poll(Ticks27((clock.now() + delay) as i64));
        for e in dec.events() {
            match e {
                Event::Exited { class, .. } => {
                    stats.worker_exits.inc();
                    tracing::warn!(input = name, "decoder worker exited: {class:?}");
                    let _ = target.video.try_send(LiveEvent::Closed);
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
                }
                Event::CircuitOpen { for_ } => {
                    tracing::error!(input = name, "decoder failing; paused for {for_:?}");
                }
                other => tracing::debug!(input = name, "decoder: {other:?}"),
            }
        }
    }
    dec.shutdown(Duration::from_secs(1));
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
        use tb_m2v_syntax::afd::{FULL, LETTER_16_9, PILLAR_4_3};
        let spec = |w, h, an, ad| VideoSpec {
            width: w,
            height: h,
            display_aspect: Rational::new(an, ad).unwrap(),
            aspect: Aspect::Fit,
            deinterlace: Deinterlace::Off,
        };
        let hd = spec(1920, 1080, 16, 9);
        assert_eq!(active_format(&hd, Some((1440, 1080))), PILLAR_4_3);
        assert_eq!(active_format(&hd, Some((1920, 1080))), FULL);
        assert_eq!(active_format(&hd, None), FULL);
        let sd = spec(704, 480, 4, 3);
        assert_eq!(active_format(&sd, Some((704, 360))), LETTER_16_9);
        assert_eq!(active_format(&sd, Some((704, 480))), FULL);
    }
}
