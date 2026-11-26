//! The audio thread (SPEC §5.2, §5.4, §8.3): every track of every program produces one AC-3
//! frame per 32 ms forever, on the same presentation timeline as the video: frame `j` is
//! presented at `P0 + j · 864 000` ticks (1536 samples at 48 kHz, exactly 2880 ticks of 90 kHz)
//! and is made at that time minus Δ. Samples come from `tb-audio`'s track pipeline (sample
//! clock, loudness, limiter; silence with fades when no source plays) and are encoded by
//! `tb-ac3`. Audio is cheap (about 0.3 % of a core per stereo track), so one thread serves all.

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

use tb_ac3::{AudioCodingMode, BitRate, ConfigBuilder, Encoder};
use tb_audio::clock::AudioFrame;
use tb_audio::layout::OutputLayout;
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
    /// Frames refused by a full carrier queue.
    pub dropped: Counter,
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
    ) -> Result<Track, TrackError> {
        let (layout, mode, lfe) = if plan.channels == 6 {
            (OutputLayout::FivePointOne, AudioCodingMode::THREE_TWO, true)
        } else {
            (OutputLayout::Stereo, AudioCodingMode::STEREO, false)
        };
        let rate = BitRate::from_kbps(plan.kbps).ok_or(TrackError::Rate(plan.kbps))?;
        let cfg = ConfigBuilder::new(mode, lfe, rate).build()?;
        let pipeline = TrackPipeline::new(TrackConfig::new(Ticks27(p0 as i64), layout))?;
        Ok(Track {
            carrier,
            program,
            track,
            stc0,
            tx,
            pipeline,
            encoder: Encoder::new(cfg),
            frame: AudioFrame::new(layout),
            feeds,
            open: [false; 2],
            follow,
            on: Source::Slate,
            next_pts: p0,
            frames: Counter::default(),
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
        match self.encoder.encode_frame(&self.frame.planes()) {
            Ok(bytes) => {
                let au = AudioAu {
                    data: bytes.to_vec(),
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
