//! One output audio track: per source, resample → map → normalize → limit; then the sample
//! clock, which chooses between the sources and makes the AC-3 frames (SPEC §5.4, §8.3, §9.5).
//!
//! ```text
//!  live    ──► resample ─► map ─► normalize ─► limit ──┐
//!                                                       ├─► sample clock ─► 1536-sample frames
//!  standby ──► resample ─► map ─► normalize ─► limit ──┘        (slate: silence)
//! ```
//!
//! Resampling and channel mapping are both linear and time-invariant per channel, so they
//! commute; a source with more input channels than the output is mapped first (fewer channels to
//! resample), one with fewer is resampled first. The result is the same up to `f32` rounding.
//!
//! Timing: the caller stamps each input block with the output time of its first sample (from
//! the mapper of `tb-timeline`, SPEC §5.3). The pipeline carries the stamp through: the first
//! resampled sample of a call is at an exactly known input position, whose time is the block's
//! stamp plus the position's offset at the input rate; the limiter's delay is subtracted. The
//! clock then places the samples (contiguously while the stamps agree with the count) and
//! measures the drift, which the [`TrimController`] turns into the resampler's trim together
//! with the mapper's skew.

use crate::clock::{
    AudioFrame, ClockConfig, ClockConfigError, ClockCounters, FrameInfo, PushError, SampleClock,
    SourceCounters, TrimController,
};
use crate::layout::{
    DownmixScaling, InputLayout, MAX_INPUT_CHANNELS, MixError, Mixer, OutputLayout,
};
use crate::limiter::{Limiter, LimiterConfig, LimiterConfigError, LimiterError};
use crate::loudness::MeterError;
use crate::normalize::{Normalizer, NormalizerConfig, NormalizerConfigError};
use crate::resample::{InputRate, ResampleError, Resampler, Trim};
use tb_timeline::{Source, Ticks27};

/// The configuration of a track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackConfig {
    /// The clock: start time, layout, buffer and tolerance.
    pub clock: ClockConfig,
    /// Downmix scaling for sources with more channels than the output.
    pub downmix: DownmixScaling,
    /// The loudness normalizer of every source.
    pub normalizer: NormalizerConfig,
    /// The true-peak limiter of every source.
    pub limiter: LimiterConfig,
    /// The resampler trim control.
    pub trim: TrimController,
}

impl TrackConfig {
    /// The defaults for a track in `layout` whose frame 0 is presented at `p0`.
    pub fn new(p0: Ticks27, layout: OutputLayout) -> Self {
        Self {
            clock: ClockConfig::new(p0, layout),
            downmix: DownmixScaling::default(),
            normalizer: NormalizerConfig::default(),
            limiter: LimiterConfig::default(),
            trim: TrimController::default(),
        }
    }

    /// The output layout.
    pub fn layout(&self) -> OutputLayout {
        self.clock.layout
    }
}

/// A [`TrackConfig`] that cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum TrackConfigError {
    /// The clock's configuration.
    #[error(transparent)]
    Clock(#[from] ClockConfigError),
    /// The normalizer's configuration.
    #[error(transparent)]
    Normalizer(#[from] NormalizerConfigError),
    /// The limiter's configuration.
    #[error(transparent)]
    Limiter(#[from] LimiterConfigError),
    /// The trim control's time constant must be positive and its clamp within ±500 ppm.
    #[error("trim control: tau {tau_s} s, clamp {max_ppb} ppb")]
    Trim {
        /// The time constant given.
        tau_s: f64,
        /// The clamp given.
        max_ppb: i64,
    },
}

/// What a source delivers: its sampling rate and channel layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputFormat {
    /// Sampling rate.
    pub rate: InputRate,
    /// Channel layout of the planes.
    pub layout: InputLayout,
}

/// Why the pipeline refused a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// The slate is silence and takes no input.
    #[error("the slate takes no input")]
    Slate,
    /// The source has not been opened.
    #[error("source {0:?} is not open")]
    NotOpen(Source),
    /// The resampler refused the planes.
    #[error(transparent)]
    Resample(#[from] ResampleError),
    /// The mixer refused the planes.
    #[error(transparent)]
    Mix(#[from] MixError),
    /// The normalizer refused the planes.
    #[error(transparent)]
    Meter(#[from] MeterError),
    /// The limiter refused the planes.
    #[error(transparent)]
    Limiter(#[from] LimiterError),
    /// The clock refused the block.
    #[error(transparent)]
    Push(#[from] PushError),
}

/// The processing of one source up to the clock.
#[derive(Clone, Debug)]
struct Front {
    format: InputFormat,
    resampler: Resampler,
    mixer: Mixer,
    mix_first: bool,
    normalizer: Normalizer,
    limiter: Limiter,
    a: Vec<Vec<f32>>,
    b: Vec<Vec<f32>>,
    skew_ppb: i64,
}

impl Front {
    /// Run a block; returns the output-time offset of the first sample produced (27 MHz ticks
    /// relative to the block's first input sample) and leaves the samples in `self.b`.
    fn run(&mut self, planes: &[&[f32]]) -> Result<i64, PipelineError> {
        let first_in = self.resampler.input_consumed();
        let (idx, frac) = self.resampler.next_output_position();
        if self.mix_first {
            self.mixer.process(planes, &mut self.a)?;
            for p in &mut self.b {
                p.clear();
            }
            let mut buf = [&[][..]; MAX_INPUT_CHANNELS];
            self.resampler
                .process(refs(&self.a, &mut buf), &mut self.b)?;
        } else {
            for p in &mut self.a {
                p.clear();
            }
            self.resampler.process(planes, &mut self.a)?;
            let mut buf = [&[][..]; MAX_INPUT_CHANNELS];
            self.mixer.process(refs(&self.a, &mut buf), &mut self.b)?;
        }
        self.normalizer.process(&mut self.b)?;
        self.limiter.process(&mut self.b)?;
        // Input samples from the block's first to the first output's position, exactly:
        // (idx − first_in) + frac / 2^64, in 27 MHz ticks at the input rate.
        let fs = i128::from(self.format.rate.hz());
        let num = ((i128::from(idx) - i128::from(first_in)) << 64) + i128::from(frac);
        let offset = (num * 27_000_000 / fs) >> 64;
        // The limiter's delay at 48 kHz: 562.5 ticks a sample.
        let delay = self.limiter.latency() as i128 * 1125 / 2;
        Ok(i64::try_from(offset - delay).unwrap_or(0))
    }
}

/// Borrow up to 16 vectors as the planes `&[&[f32]]`, without allocating.
fn refs<'a, 'b>(
    v: &'a [Vec<f32>],
    buf: &'b mut [&'a [f32]; MAX_INPUT_CHANNELS],
) -> &'b [&'a [f32]] {
    for (r, p) in buf.iter_mut().zip(v) {
        *r = p;
    }
    &buf[..v.len().min(MAX_INPUT_CHANNELS)]
}

/// One output track (module documentation).
#[derive(Clone, Debug)]
pub struct TrackPipeline {
    cfg: TrackConfig,
    fronts: [Option<Front>; 2],
    clock: SampleClock,
    /// A fresh normalizer and limiter (validated once), cloned for each source opened.
    normalizer: Normalizer,
    limiter: Limiter,
}

fn slot(s: Source) -> Result<usize, PipelineError> {
    match s {
        Source::Live => Ok(0),
        Source::Standby => Ok(1),
        Source::Slate => Err(PipelineError::Slate),
    }
}

impl TrackPipeline {
    /// A track with `cfg`, no source open, on the slate (silent frames).
    pub fn new(cfg: TrackConfig) -> Result<Self, TrackConfigError> {
        let t = cfg.trim;
        if !(t.tau_s > 0.0 && t.tau_s.is_finite())
            || !(0..=i64::from(Trim::MAX_PPB)).contains(&t.max_correction_ppb)
        {
            return Err(TrackConfigError::Trim {
                tau_s: t.tau_s,
                max_ppb: t.max_correction_ppb,
            });
        }
        Ok(Self {
            clock: SampleClock::new(cfg.clock)?,
            normalizer: Normalizer::new(cfg.layout(), cfg.normalizer)?,
            limiter: Limiter::new(cfg.layout().count(), cfg.limiter)?,
            cfg,
            fronts: [None, None],
        })
    }

    /// The configuration.
    pub fn config(&self) -> &TrackConfig {
        &self.cfg
    }

    /// Open (or reopen with a new format) source `s`. A first open starts its normalizer at
    /// 0 dB; a reopen (a new session of the input, or a new channel count) keeps the gain in
    /// use and measures afresh from it ([`Normalizer::restart`]), so the input does not play at
    /// its own level until the normalizer has caught up again.
    ///
    /// [`Normalizer::restart`]: crate::normalize::Normalizer::restart
    pub fn open(&mut self, s: Source, format: InputFormat) -> Result<(), PipelineError> {
        let i = slot(s)?;
        let normalizer = match self.fronts[i].take() {
            Some(old) => {
                let mut n = old.normalizer;
                n.restart();
                n
            }
            None => self.normalizer.clone(),
        };
        let out = self.cfg.layout();
        let mix_first = format.layout.channels() > out.count();
        let rs_ch = if mix_first {
            out.count()
        } else {
            format.layout.channels()
        };
        self.fronts[i] = Some(Front {
            format,
            resampler: Resampler::new(format.rate, rs_ch),
            mixer: Mixer::new(format.layout, out, self.cfg.downmix),
            mix_first,
            normalizer,
            limiter: self.limiter.clone(),
            a: vec![Vec::new(); rs_ch],
            b: vec![Vec::new(); out.count()],
            skew_ppb: 0,
        });
        self.clock.end(s);
        Ok(())
    }

    /// Close source `s`: what it delivered still plays out (and fades); the clock goes on.
    pub fn close(&mut self, s: Source) {
        if let Ok(i) = slot(s) {
            self.fronts[i] = None;
            self.clock.end(s);
        }
    }

    /// The format of an open source.
    pub fn format(&self, s: Source) -> Option<InputFormat> {
        slot(s)
            .ok()
            .and_then(|i| self.fronts[i].as_ref().map(|f| f.format))
    }

    /// The source's session ended (SPEC §5.3): its next block is placed at its own stamp, and
    /// its normalizer measures afresh (a new session is likely a new program).
    pub fn end_session(&mut self, s: Source) {
        self.clock.end(s);
        if let Ok(i) = slot(s)
            && let Some(f) = &mut self.fronts[i]
        {
            f.normalizer.restart();
        }
    }

    /// The mapper's current skew of the source, ppb (`out = offset + in · (1 + skew)`).
    pub fn set_skew(&mut self, s: Source, skew_ppb: i64) {
        if let Ok(i) = slot(s)
            && let Some(f) = &mut self.fronts[i]
        {
            f.skew_ppb = skew_ppb;
        }
    }

    /// Give a block of decoded audio of source `s` (one plane per input channel), whose first
    /// sample is due at output time `at`.
    pub fn push(&mut self, s: Source, at: Ticks27, planes: &[&[f32]]) -> Result<(), PipelineError> {
        let i = slot(s)?;
        let f = self.fronts[i].as_mut().ok_or(PipelineError::NotOpen(s))?;
        let offset = f.run(planes)?;
        if f.b.first().is_some_and(|p| !p.is_empty()) {
            let mut buf = [&[][..]; MAX_INPUT_CHANNELS];
            self.clock.push(
                s,
                Ticks27(at.0.saturating_add(offset)),
                refs(&f.b, &mut buf),
            )?;
        }
        let trim = self.cfg.trim.trim(f.skew_ppb, self.clock.drift(s));
        f.resampler.set_trim(trim);
        Ok(())
    }

    /// Change to source `to` at output time `at` with the 20 ms crossfade (SPEC §9.5).
    pub fn switch(&mut self, to: Source, at: Ticks27) {
        self.clock.switch(to, at);
    }

    /// The next AC-3 frame's samples.
    pub fn next_frame(&mut self, out: &mut AudioFrame) -> FrameInfo {
        self.clock.next_frame(out)
    }

    /// The sample clock (depth, drift, counters).
    pub fn clock(&self) -> &SampleClock {
        &self.clock
    }

    /// The clock's counters.
    pub fn counters(&self) -> ClockCounters {
        self.clock.counters()
    }

    /// A source's counters.
    pub fn source_counters(&self, s: Source) -> SourceCounters {
        self.clock.source_counters(s)
    }

    /// The resampler trim of a source now.
    pub fn trim(&self, s: Source) -> Option<Trim> {
        slot(s)
            .ok()
            .and_then(|i| self.fronts[i].as_ref().map(|f| f.resampler.trim()))
    }

    /// The normalizer gain of a source now, dB.
    pub fn gain_db(&self, s: Source) -> Option<f64> {
        slot(s)
            .ok()
            .and_then(|i| self.fronts[i].as_ref().map(|f| f.normalizer.gain_db()))
    }
}
