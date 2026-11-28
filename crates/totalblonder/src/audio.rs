//! The audio thread (SPEC §5.2, §5.4, §8.3): every track of every program produces one AC-3
//! frame per 32 ms forever, on the same presentation timeline as the video: frame `j` is
//! presented at `P0 + j · 864 000` ticks (1536 samples at 48 kHz, exactly 2880 ticks of 90 kHz)
//! and is made at that time minus Δ. Samples come from `tb-audio`'s track pipeline (sample
//! clock, loudness, limiter; silence with fades when no source plays) and are encoded by
//! `tb-ac3`. Audio is cheap (about 0.3 % of a core per stereo track), so one thread serves all.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

use tb_ac3::{AudioCodingMode, BitRate, ConfigBuilder, Encoder};
use tb_audio::clock::AudioFrame;
use tb_audio::layout::{Mixer, OutputLayout};
use tb_audio::pipeline::{InputFormat, TrackConfig, TrackPipeline};
use tb_mux::AudioAu;
use tb_timeline::{Source, Ticks27};
use tb_ts::Stc;

use crate::carrier::CarrierMsg;
use crate::config::AudioPlan;
use crate::metrics::Counter;
use crate::time::Clock;

/// 27 MHz ticks per AC-3 frame.
pub const FRAME_TICKS: u64 = 864_000;

/// Decoded audio of a live input for one track: f32 planar at the input's rate, with the
/// output (presentation) time of its first sample.
pub enum LiveAudio {
    /// A source format change (or the first block).
    Format(InputFormat),
    /// A block of samples: planes, and the presentation time of the first sample.
    Block {
        /// Presentation time, process ticks.
        at: u64,
        /// One plane per input channel.
        planes: Vec<Vec<f32>>,
    },
    /// The source went away.
    Closed,
}

/// One output track.
pub struct Track {
    /// The carrier's id (for labels).
    pub carrier: u8,
    /// Program index within its carrier.
    pub program: usize,
    /// Track index within the program.
    pub track: usize,
    /// The carrier's STC offset.
    pub stc0: u64,
    /// The carrier's queue.
    pub tx: SyncSender<CarrierMsg>,
    pipeline: TrackPipeline,
    encoder: Encoder,
    /// `layout = "auto"`: the mono and stereo encoders and the downmixes that feed them. The
    /// pipeline runs at 5.1; all three encoders code every frame (so a change of mode has no
    /// gap in their overlap), and the one matching the source on air is sent.
    narrow: Option<Narrow>,
    frame: AudioFrame,
    /// Live (index 0) and standby (index 1) audio, and whether each is open in the pipeline.
    feeds: [Option<Receiver<LiveAudio>>; 2],
    open: [bool; 2],
    /// The program's source decision (`SourceStats::source`: 1 live): the track follows it,
    /// so sound and picture cut together.
    follow: Option<crate::metrics::Gauge>,
    on: Source,
    next_pts: u64,
    /// Frames made.
    pub frames: Counter,
    /// The rate being sent, bit/s (an auto track's follows its mode).
    pub rate: crate::metrics::Gauge,
    /// Frames refused by a full carrier queue.
    pub dropped: Counter,
}

/// The rates of a carrier's auto tracks, shared between the audio thread and the statmux
/// (SPEC §8.4): a track says what its source's mode needs, the statmux frees that from the
/// video pool first and only then grants it. The rates are transport bit/s.
#[derive(Debug, Default)]
pub struct CarrierFlex {
    /// The carrier's auto tracks.
    pub tracks: Vec<FlexTrack>,
}

/// One auto track's share of [`CarrierFlex`].
#[derive(Debug)]
pub struct FlexTrack {
    /// Program index within the carrier.
    pub program: usize,
    /// Track index within the program.
    pub track: usize,
    /// The rate of its widest mode: what the carrier's budget reserves for it.
    pub ceiling_ts: u64,
    /// What the mode of the source on air needs.
    pub want_ts: std::sync::atomic::AtomicU64,
    /// What the statmux has made room for: the track sends nothing that needs more.
    pub granted_ts: std::sync::atomic::AtomicU64,
}

impl FlexTrack {
    /// A track starting granted `start_ts` (its stereo rate: the slate is stereo).
    pub fn new(program: usize, track: usize, ceiling_ts: u64, start_ts: u64) -> Self {
        FlexTrack {
            program,
            track,
            ceiling_ts,
            want_ts: start_ts.into(),
            granted_ts: start_ts.into(),
        }
    }
}

/// The narrower coding modes of an auto track.
struct Narrow {
    mono: (Mixer, Encoder, Vec<Vec<f32>>),
    stereo: (Mixer, Encoder, Vec<Vec<f32>>),
    /// The mode sent last (channels: 1, 2 or 6).
    on: usize,
    /// Each mode's rate, kbit/s: mono, stereo, 5.1.
    kbps: [u32; 3],
    /// Its share of the carrier's flexible budget (none without the statmux: every mode may
    /// be sent at any time, the budget holding the ceiling).
    flex: Option<(std::sync::Arc<CarrierFlex>, usize)>,
}

impl Narrow {
    fn kbps(&self, mode: usize) -> u32 {
        match mode {
            1 => self.kbps[0],
            2 => self.kbps[1],
            _ => self.kbps[2],
        }
    }

    /// The mode to send when the source wants `want`: it, if its rate is granted; else the
    /// mode in use (whose rate is).
    fn choose(&self, want: usize) -> usize {
        let Some((f, k)) = &self.flex else {
            return want;
        };
        let Some(t) = f.tracks.get(*k) else {
            return want;
        };
        use std::sync::atomic::Ordering::Relaxed;
        let need = crate::config::ac3_ts_bps(self.kbps(want));
        t.want_ts.store(need, Relaxed);
        if need <= t.granted_ts.load(Relaxed) {
            want
        } else {
            self.on
        }
    }
}

/// Downmixes the 5.1 frame and codes it in a narrower mode.
fn code(
    (mixer, enc, out): &mut (Mixer, Encoder, Vec<Vec<f32>>),
    planes: &[&[f32]],
) -> Result<Vec<u8>, String> {
    mixer.process(planes, out).map_err(|e| e.to_string())?;
    let refs: Vec<&[f32]> = out.iter().map(Vec::as_slice).collect();
    enc.encode_frame(&refs)
        .map(<[u8]>::to_vec)
        .map_err(|e| e.to_string())
}

/// The coding mode for a source of `channels`: 1 stays mono, 2 stereo, more 5.1.
fn mode_for(channels: usize) -> usize {
    match channels {
        1 => 1,
        2 => 2,
        _ => 6,
    }
}

/// A track that cannot be set up.
#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    /// The AC-3 configuration.
    #[error("AC-3: {0}")]
    Ac3(#[from] tb_ac3::ConfigError),
    /// The pipeline configuration.
    #[error("audio pipeline: {0}")]
    Pipeline(#[from] tb_audio::pipeline::TrackConfigError),
    /// A rate the encoder does not have.
    #[error("no A/52 rate of {0} kbit/s")]
    Rate(u32),
    /// A channel layout.
    #[error("layout: {0}")]
    Layout(#[from] tb_audio::layout::LayoutError),
}

impl Track {
    /// A track presenting frame 0 at process time `p0`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        carrier: u8,
        plan: &AudioPlan,
        program: usize,
        track: usize,
        stc0: u64,
        p0: u64,
        tx: SyncSender<CarrierMsg>,
        feeds: [Option<Receiver<LiveAudio>>; 2],
        follow: Option<crate::metrics::Gauge>,
        flex: Option<(std::sync::Arc<CarrierFlex>, usize)>,
    ) -> Result<Track, TrackError> {
        let (layout, mode, lfe) = match plan.channels {
            6 => (OutputLayout::FivePointOne, AudioCodingMode::THREE_TWO, true),
            1 => (OutputLayout::Mono, AudioCodingMode::Mono, false),
            _ => (OutputLayout::Stereo, AudioCodingMode::STEREO, false),
        };
        let rate = BitRate::from_kbps(plan.kbps).ok_or(TrackError::Rate(plan.kbps))?;
        let cfg = ConfigBuilder::new(mode, lfe, rate).build()?;
        let narrow = if plan.auto {
            use tb_audio::layout::{DownmixScaling, InputLayout, Speaker::*};
            // the pipeline's 5.1 in A/52 order
            let bed =
                InputLayout::new(&[FrontLeft, FrontCenter, FrontRight, SideLeft, SideRight, Lfe])?;
            let enc = |m, kbps: u32| -> Result<Encoder, TrackError> {
                let rate = BitRate::from_kbps(kbps).ok_or(TrackError::Rate(kbps))?;
                Ok(Encoder::new(ConfigBuilder::new(m, false, rate).build()?))
            };
            Some(Narrow {
                mono: (
                    Mixer::new(bed, OutputLayout::Mono, DownmixScaling::Unscaled),
                    enc(AudioCodingMode::Mono, plan.mode_kbps[0])?,
                    vec![Vec::new()],
                ),
                stereo: (
                    Mixer::new(bed, OutputLayout::Stereo, DownmixScaling::Unscaled),
                    enc(AudioCodingMode::STEREO, plan.mode_kbps[1])?,
                    vec![Vec::new(), Vec::new()],
                ),
                on: 2,
                kbps: plan.mode_kbps,
                flex,
            })
        } else {
            None
        };
        let pipeline = TrackPipeline::new(TrackConfig::new(Ticks27(p0 as i64), layout))?;
        Ok(Track {
            carrier,
            program,
            track,
            stc0,
            tx,
            pipeline,
            encoder: Encoder::new(cfg),
            narrow,
            frame: AudioFrame::new(layout),
            feeds,
            open: [false; 2],
            follow,
            on: Source::Slate,
            next_pts: p0,
            frames: Counter::default(),
            rate: {
                let g = crate::metrics::Gauge::default();
                g.set(
                    u64::from(if plan.auto {
                        plan.mode_kbps[1]
                    } else {
                        plan.kbps
                    }) * 1000,
                );
                g
            },
            dropped: Counter::default(),
        })
    }

    fn take_live(&mut self) {
        for (i, src) in [(0, Source::Live), (1, Source::Standby)] {
            let Some(rx) = &self.feeds[i] else { continue };
            loop {
                match rx.try_recv() {
                    Ok(LiveAudio::Format(f)) => {
                        if self.pipeline.open(src, f).is_ok() {
                            self.open[i] = true;
                        }
                    }
                    Ok(LiveAudio::Block { at, planes }) => {
                        if self.open[i] {
                            let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
                            if let Err(e) = self.pipeline.push(src, Ticks27(at as i64), &refs) {
                                tracing::debug!("audio push: {e}");
                            }
                        }
                    }
                    Ok(LiveAudio::Closed) => self.pipeline.end_session(src),
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }
        }
    }

    /// Makes the next frame and queues it for the mux.
    pub fn step(&mut self) {
        self.take_live();
        if let Some(g) = &self.follow {
            let to = match g.get() {
                1 if self.open[0] => Source::Live,
                2 if self.open[1] => Source::Standby,
                _ => Source::Slate,
            };
            if to != self.on {
                self.pipeline.switch(to, Ticks27(self.next_pts as i64));
                self.on = to;
            }
        }
        self.next_pts += FRAME_TICKS;
        let info = self.pipeline.next_frame(&mut self.frame);
        let planes = self.frame.planes();
        // The source on air decides an auto track's mode; the slate is stereo.
        let source_wants = match (&self.narrow, info.source) {
            (None, _) => 0,
            (Some(_), Source::Slate) => 2,
            (Some(_), s) => self
                .pipeline
                .format(s)
                .map_or(2, |f| mode_for(f.layout.channels())),
        };
        let full = self
            .encoder
            .encode_frame(&planes)
            .map(<[u8]>::to_vec)
            .map_err(|e| e.to_string());
        let data = match self.narrow.as_mut() {
            None => full,
            Some(n) => {
                let want = n.choose(source_wants);
                if want != n.on {
                    tracing::info!(
                        carrier = self.carrier,
                        program = self.program + 1,
                        track = self.track + 1,
                        "audio coding mode: {} -> {} channels",
                        n.on,
                        want
                    );
                    n.on = want;
                    self.rate.set(u64::from(n.kbps(want)) * 1000);
                }
                // Every encoder codes every frame, so each stays continuous.
                let m = code(&mut n.mono, &planes);
                let s = code(&mut n.stereo, &planes);
                match want {
                    1 => m,
                    2 => s,
                    _ => full,
                }
            }
        };
        match data {
            Ok(data) => {
                let au = AudioAu {
                    data,
                    pts: Stc(self.stc0 + info.pts.0.max(0) as u64),
                };
                self.frames.inc();
                if self
                    .tx
                    .try_send(CarrierMsg::Audio(self.program, self.track, au))
                    .is_err()
                {
                    self.dropped.inc();
                }
            }
            Err(e) => tracing::error!("AC-3 encode: {e}"),
        }
    }

    /// Switches the track's source (the program's fallback decision) from presentation
    /// time `at`.
    pub fn switch(&mut self, to: Source, at: u64) {
        self.pipeline.switch(to, Ticks27(at as i64));
    }
}

/// Runs every track until the clock stops (or, offline, until the presentation time `end`):
/// frame `j` of all tracks is made at `p0 + j · FRAME_TICKS − delay`.
pub fn run(mut tracks: Vec<Track>, p0: u64, delay: u64, end: Option<u64>, clock: &dyn Clock) {
    let mut j: u64 = 0;
    loop {
        let pts = p0 + j * FRAME_TICKS;
        if end.is_some_and(|e| pts >= e) || !clock.sleep_until(pts.saturating_sub(delay)) {
            return;
        }
        for t in &mut tracks {
            t.step();
        }
        j += 1;
    }
}
