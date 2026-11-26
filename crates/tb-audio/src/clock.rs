//! The audio sample clock (SPEC §5.4): blocks of 48 kHz samples with output times, possibly
//! late, early, missing or overlapping, become exactly 1536 samples per AC-3 frame, frame `j`
//! at `PTS = P0 + 2880·j` (90 kHz) forever (SPEC §5.2; A/52 Table 5.18: 1536 samples per
//! syncframe; 1536 / 48 kHz = 32 ms = 2880 ticks of 90 kHz exactly).
//!
//! # Time
//!
//! Output time is a sample index `s` on the program's 48 kHz output clock: sample `s` is due at
//! `P0 + s · 562.5` ticks of 27 MHz, and frame `j` holds samples `1536·j .. 1536·(j+1)`. A block
//! stamped at `t` (27 MHz, [`Ticks27`] as in `tb-timeline`) starts at sample
//! `round((t − P0) · 2 / 1125)`. All of it is integer arithmetic: the cadence is exact for as
//! long as an `i64` of samples lasts (6 million years).
//!
//! # Placement: continuity first, timestamps as a reference
//!
//! Audio must be continuous: a decoder's blocks carry timestamps with jitter (RTMP stamps are
//! whole milliseconds, ±48 samples), and placing every block at its rounded stamp would cut a
//! gap or an overlap into every block boundary. So each source keeps the position where its
//! next sample is *expected* (the end of its previous block). A block whose stamp is within
//! `tolerance` (default 20 ms) of that is placed there, contiguous, and the difference
//! `expected − stamp` is the source's **drift**: how far its sample count has run ahead of its
//! timestamps. The resampler's trim removes the drift ([`TrimController`]), so the stamps
//! keep A/V sync and the sample count keeps the waveform intact. A block further off than the
//! tolerance starts a new *segment* at its own stamp (a discontinuity, counted): forward, the
//! samples in between are silence; backward, the newer block replaces what was buffered from its
//! start on.
//!
//! Samples that land before the output position are late and dropped; beyond the buffer
//! (`capacity`, default 2.5 s past the output position) early and dropped; on top of buffered
//! samples they replace them. All three are counted per source.
//!
//! # Edges
//!
//! Each source has an envelope: it is 0 on empty samples, rises by 1/480 per sample (10 ms)
//! after a gap or at the start of a segment, and falls to reach 0 at the next gap or segment
//! start. The fall uses a look-ahead of 480 samples into the buffer, so a fade-out begins before
//! the gap is reached; if the gap fills in the meantime (a late block), the envelope just rises
//! again, continuously. So "silence when dry, with 10 ms fades at both edges" (SPEC §5.4) holds
//! with no special state for "dry".
//!
//! # Sources
//!
//! There is a buffer for [`Source::Live`] and one for [`Source::Standby`]; [`Source::Slate`] is
//! digital silence (SPEC §9.5). A change of source is a 20 ms raised-cosine crossfade starting
//! at a given time (SPEC §9.5: "Audio crosses over with a 20 ms fade at the same instant" as the
//! video cut). The two gains sum to one at every sample, so the crossfade of two signals under
//! the true-peak ceiling stays under it.

use crate::layout::OutputLayout;
use crate::resample::Trim;
use std::fmt;
use std::ops::Deref;
use tb_timeline::{Source, Ticks27, to_pts90};

/// Samples per AC-3 frame (A/52 §5.1, Table 5.18: 6 blocks of 256).
pub const FRAME: usize = 1536;
/// One AC-3 frame in 90 kHz ticks (32 ms).
pub const FRAME_90K: u64 = 2880;
/// One AC-3 frame in 27 MHz ticks.
pub const FRAME_27M: i64 = 864_000;
/// The edge fade, samples (10 ms, SPEC §5.4).
pub const FADE: usize = 480;
/// The source crossfade, samples (20 ms, SPEC §9.5).
pub const XFADE: usize = 960;
/// Most output channels (5.1).
pub const MAX_CHANNELS: usize = 6;

const EMPTY: u8 = 0;
const VALID: u8 = 1;
/// A valid sample that starts a segment: the envelope is 0 just before it.
const START: u8 = 2;

/// The sample index of output time `t` on a clock starting at `p0`, rounded to nearest.
pub fn sample_at(p0: Ticks27, t: Ticks27) -> i64 {
    // 27 MHz / 48 kHz = 562.5 = 1125 / 2 ticks per sample.
    let x = (i128::from(t.0) - i128::from(p0.0)) * 4 + 1125;
    let s = x.div_euclid(2250);
    i64::try_from(s).unwrap_or(if s < 0 { i64::MIN } else { i64::MAX })
}

/// Parameters of a [`SampleClock`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockConfig {
    /// Output time of sample 0 (the presentation time of frame 0).
    pub p0: Ticks27,
    /// The output layout (channel count).
    pub layout: OutputLayout,
    /// Samples buffered per source past the output position (default 120 000: 2.5 s, which is
    /// `2·J + 0.5 s` for the default jitter target `J` = 1 s, SPEC §5.3).
    pub capacity: usize,
    /// Largest distance between a block's stamp and the expected position at which it is still
    /// taken as contiguous, samples (default 960: 20 ms).
    pub tolerance: usize,
}

impl ClockConfig {
    /// The defaults for `layout`, starting at `p0`.
    pub fn new(p0: Ticks27, layout: OutputLayout) -> Self {
        Self {
            p0,
            layout,
            capacity: 120_000,
            tolerance: 960,
        }
    }
}

/// A [`ClockConfig`] value out of range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockConfigError {
    /// The capacity must hold at least two frames and the fade look-ahead, and at most 60 s.
    #[error("capacity {0} samples is outside 3552..=2880000")]
    Capacity(usize),
    /// The tolerance must be below a second.
    #[error("tolerance {0} samples is above 48000")]
    Tolerance(usize),
}

/// Why a block was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    /// The slate has no buffer: it is silence.
    #[error("the slate takes no samples")]
    Slate,
    /// Wrong number of planes.
    #[error("expected {expected} planes, got {got}")]
    Planes {
        /// Planes expected.
        expected: usize,
        /// Planes given.
        got: usize,
    },
    /// The planes differ in length.
    #[error("planes differ in length")]
    Ragged,
}

/// What happened to one source's samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceCounters {
    /// Samples received.
    pub received: u64,
    /// Samples dropped because their position had already been output.
    pub late: u64,
    /// Samples dropped because their position was beyond the buffer.
    pub early: u64,
    /// Buffered samples replaced by newer ones (overlap), or cleared by a backward
    /// discontinuity.
    pub replaced: u64,
    /// New segments started because a stamp was beyond the tolerance (after the first).
    pub discontinuities: u64,
    /// Non-finite samples taken as zero.
    pub nonfinite: u64,
}

/// What the clock produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockCounters {
    /// Frames produced.
    pub frames: u64,
    /// Output samples (per channel) with no data from the selected source, which was not the
    /// slate (fill).
    pub filled: u64,
    /// Times the selected source ran dry (a valid sample followed by an empty one).
    pub underruns: u64,
    /// Source changes completed.
    pub switches: u64,
    /// Output samples clamped to ±1.0 (nothing upstream should make any).
    pub clipped: u64,
}

#[derive(Clone, Debug)]
struct Buffer {
    /// Planar: channel `c` at `c·cap ..`.
    data: Vec<f32>,
    state: Vec<u8>,
    /// Expected position of the next contiguous sample.
    next: Option<i64>,
    /// Smoothed drift, samples.
    drift: f64,
    drift_seen: bool,
    /// The envelope at the last sample output.
    env: f32,
    /// Whether the last sample output was valid.
    was_valid: bool,
    counters: SourceCounters,
}

impl Buffer {
    fn new(channels: usize, cap: usize) -> Self {
        Self {
            data: vec![0.0; channels * cap],
            state: vec![EMPTY; cap],
            next: None,
            drift: 0.0,
            drift_seen: false,
            env: 0.0,
            was_valid: false,
            counters: SourceCounters::default(),
        }
    }
}

/// One frame of output: 1536 samples per channel, A/52 channel order.
#[derive(Clone, PartialEq)]
pub struct AudioFrame {
    channels: usize,
    data: Box<[[f32; FRAME]; MAX_CHANNELS]>,
}

impl fmt::Debug for AudioFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioFrame")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl AudioFrame {
    /// A silent frame for `layout`.
    pub fn new(layout: OutputLayout) -> Self {
        Self {
            channels: layout.count(),
            data: Box::new([[0.0; FRAME]; MAX_CHANNELS]),
        }
    }
    /// The channel count.
    pub fn channels(&self) -> usize {
        self.channels
    }
    /// Channel `c` (A/52 order).
    pub fn plane(&self, c: usize) -> &[f32; FRAME] {
        &self.data[c.min(MAX_CHANNELS - 1)]
    }
    /// The planes as the encoder takes them (`&[&[f32]]`).
    pub fn planes(&self) -> Planes<'_> {
        let mut refs: [&[f32]; MAX_CHANNELS] = [&[]; MAX_CHANNELS];
        for (r, d) in refs.iter_mut().zip(self.data.iter()) {
            *r = d;
        }
        Planes {
            refs,
            n: self.channels,
        }
    }
}

/// Borrowed planes of an [`AudioFrame`]; dereferences to `[&[f32]]`.
#[derive(Clone, Copy, Debug)]
pub struct Planes<'a> {
    refs: [&'a [f32]; MAX_CHANNELS],
    n: usize,
}

impl<'a> Deref for Planes<'a> {
    type Target = [&'a [f32]];
    fn deref(&self) -> &Self::Target {
        &self.refs[..self.n]
    }
}

/// What [`SampleClock::next_frame`] produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    /// The frame index `j`.
    pub index: u64,
    /// Its presentation time, `P0 + 864000·j` ticks of 27 MHz.
    pub pts: Ticks27,
    /// The source selected at the end of the frame.
    pub source: Source,
    /// Every sample of the frame is zero.
    pub silent: bool,
}

impl FrameInfo {
    /// The PTS as written in the PES header (90 kHz, modulo 2^33).
    pub fn pts90(&self) -> u64 {
        to_pts90(self.pts)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Switch {
    from: Source,
    to: Source,
    start: i64,
}

/// The sample clock of one output track.
#[derive(Clone, Debug)]
pub struct SampleClock {
    cfg: ClockConfig,
    channels: usize,
    /// Live, standby.
    bufs: [Buffer; 2],
    /// Position of the next output sample (start of the next frame).
    read: i64,
    frame: u64,
    selected: Source,
    /// The crossfade in progress or due.
    switch: Option<Switch>,
    /// The next change, once that one is done: (source, earliest start).
    queued: Option<(Source, i64)>,
    /// Per source, the frame's samples after the envelope (scratch).
    scratch: [Vec<f32>; 2],
    counters: ClockCounters,
}

fn slot(s: Source) -> Option<usize> {
    match s {
        Source::Live => Some(0),
        Source::Standby => Some(1),
        Source::Slate => None,
    }
}

impl SampleClock {
    /// A clock with `cfg`, at frame 0, on the slate.
    pub fn new(cfg: ClockConfig) -> Result<Self, ClockConfigError> {
        if !(2 * FRAME + FADE..=2_880_000).contains(&cfg.capacity) {
            return Err(ClockConfigError::Capacity(cfg.capacity));
        }
        if cfg.tolerance > 48_000 {
            return Err(ClockConfigError::Tolerance(cfg.tolerance));
        }
        let ch = cfg.layout.count();
        Ok(Self {
            cfg,
            channels: ch,
            bufs: [Buffer::new(ch, cfg.capacity), Buffer::new(ch, cfg.capacity)],
            read: 0,
            frame: 0,
            selected: Source::Slate,
            switch: None,
            queued: None,
            scratch: [vec![0.0; ch * FRAME], vec![0.0; ch * FRAME]],
            counters: ClockCounters::default(),
        })
    }

    /// The configuration.
    pub fn config(&self) -> &ClockConfig {
        &self.cfg
    }

    /// The index of the next frame.
    pub fn next_index(&self) -> u64 {
        self.frame
    }

    /// The presentation time of frame `j`.
    pub fn pts_of(&self, j: u64) -> Ticks27 {
        let t = i128::from(self.cfg.p0.0) + i128::from(j) * i128::from(FRAME_27M);
        Ticks27(i64::try_from(t).unwrap_or(i64::MAX))
    }

    /// The output time of sample `s`, rounded down to a 27 MHz tick.
    pub fn time_of(&self, s: i64) -> Ticks27 {
        let t = i128::from(self.cfg.p0.0) + (i128::from(s) * 1125).div_euclid(2);
        Ticks27(i64::try_from(t).unwrap_or(i64::MAX))
    }

    /// The source selected now.
    pub fn selected(&self) -> Source {
        self.selected
    }

    /// The clock's counters.
    pub fn counters(&self) -> ClockCounters {
        self.counters
    }

    /// The counters of a source (zero for the slate).
    pub fn source_counters(&self, s: Source) -> SourceCounters {
        slot(s).map_or_else(SourceCounters::default, |i| self.bufs[i].counters)
    }

    /// Buffer depth of a source: how far its expected next sample is past the output position,
    /// samples (`None` before its first block or after [`SampleClock::end`]). The jitter
    /// buffer's fill as audio sees it; the mapper keeps it near `J` (SPEC §5.3).
    pub fn depth(&self, s: Source) -> Option<i64> {
        slot(s).and_then(|i| self.bufs[i].next.map(|n| n - self.read))
    }

    /// Drift of a source: by how many samples its sample count has run ahead of its timestamps
    /// (smoothed over about 50 blocks). Zero when the resampler's ratio is right; this is what
    /// the trim must remove ([`TrimController`]).
    pub fn drift(&self, s: Source) -> Option<f64> {
        slot(s).and_then(|i| self.bufs[i].drift_seen.then_some(self.bufs[i].drift))
    }

    /// The source's input has ended (a session ended, SPEC §5.3): its next block starts a new
    /// segment at its own stamp. What is buffered still plays and fades out at its end.
    pub fn end(&mut self, s: Source) {
        if let Some(i) = slot(s) {
            self.bufs[i].next = None;
            self.bufs[i].drift_seen = false;
            self.bufs[i].drift = 0.0;
        }
    }

    /// Give a block of `s`'s samples (one plane per output channel, A/52 order, 48 kHz) whose
    /// first sample is due at `at`.
    pub fn push(&mut self, s: Source, at: Ticks27, planes: &[&[f32]]) -> Result<(), PushError> {
        let i = slot(s).ok_or(PushError::Slate)?;
        if planes.len() != self.channels {
            return Err(PushError::Planes {
                expected: self.channels,
                got: planes.len(),
            });
        }
        let n = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != n) {
            return Err(PushError::Ragged);
        }
        if n == 0 {
            return Ok(());
        }
        let stamp = sample_at(self.cfg.p0, at);
        let cap = self.cfg.capacity;
        let tol = self.cfg.tolerance as i64;
        let read = self.read;
        let b = &mut self.bufs[i];
        b.counters.received += n as u64;
        let (start, segment) = match b.next {
            Some(e) if (i128::from(stamp) - i128::from(e)).abs() <= i128::from(tol) => {
                let d = (i128::from(e) - i128::from(stamp)) as f64;
                if b.drift_seen {
                    b.drift += (d - b.drift) / 50.0;
                } else {
                    b.drift = d;
                    b.drift_seen = true;
                }
                (e, false)
            }
            Some(e) => {
                b.counters.discontinuities += 1;
                // Backward: what was buffered from the new start on is replaced; clear the
                // part the new block does not cover.
                if stamp < e {
                    let from = stamp.saturating_add(n as i64).max(read);
                    let to = e.min(read + cap as i64);
                    for p in from..to {
                        let k = p.rem_euclid(cap as i64) as usize;
                        if b.state[k] != EMPTY {
                            b.state[k] = EMPTY;
                            b.counters.replaced += 1;
                        }
                    }
                }
                b.drift_seen = false;
                (stamp, true)
            }
            None => (stamp, true),
        };
        let mut first = segment;
        for k in 0..n {
            let p = start.saturating_add(k as i64);
            if p < read {
                b.counters.late += 1;
                continue;
            }
            if p >= read + cap as i64 {
                b.counters.early += (n - k) as u64;
                break;
            }
            let at = p.rem_euclid(cap as i64) as usize;
            if b.state[at] != EMPTY {
                b.counters.replaced += 1;
            }
            b.state[at] = if first { START } else { VALID };
            first = false;
            for (c, plane) in planes.iter().enumerate() {
                let v = plane[k];
                b.data[c * cap + at] = if v.is_finite() {
                    v
                } else {
                    b.counters.nonfinite += 1;
                    0.0
                };
            }
        }
        // Older samples right after the block (a restart landing inside buffered audio) are a
        // different signal: make them a segment start, so the join fades instead of jumping.
        let after = start.saturating_add(n as i64);
        if after >= read && after < read + cap as i64 {
            let k = after.rem_euclid(cap as i64) as usize;
            if b.state[k] != EMPTY {
                b.state[k] = START;
            }
        }
        b.next = Some(after);
        Ok(())
    }

    /// Change to source `to` with a 20 ms crossfade starting at output time `at` (or at the
    /// next output sample if `at` has passed). A change requested while another is due or
    /// running follows it (a later request replaces an earlier one that is still waiting).
    pub fn switch(&mut self, to: Source, at: Ticks27) {
        let start = sample_at(self.cfg.p0, at).max(self.read);
        match self.switch {
            None if to == self.selected => {}
            None => {
                self.switch = Some(Switch {
                    from: self.selected,
                    to,
                    start,
                });
            }
            Some(sw) => {
                self.queued =
                    (to != sw.to).then_some((to, start.max(sw.start.saturating_add(XFADE as i64))));
            }
        }
    }

    /// Produce the next frame into `out` (which must be for the clock's layout; its channel
    /// count is set). Exactly 1536 samples per channel; `info.pts` is `P0 + 2880·j` in 90 kHz.
    pub fn next_frame(&mut self, out: &mut AudioFrame) -> FrameInfo {
        let cap = self.cfg.capacity;
        let read = self.read;
        let ch = self.channels;
        // Each source through its envelope, into scratch.
        for (b, sc) in self.bufs.iter_mut().zip(self.scratch.iter_mut()) {
            envelope(b, sc, read, cap, ch);
        }
        // Mix the selected source, with the crossfade if one is due in this frame.
        out.channels = ch;
        let mut silent = true;
        let mut clipped = 0u64;
        for k in 0..FRAME {
            let p = read + k as i64;
            if let Some(sw) = self.switch
                && p >= sw.start.saturating_add(XFADE as i64)
            {
                self.selected = sw.to;
                self.switch = self.queued.take().map(|(to, start)| Switch {
                    from: sw.to,
                    to,
                    start: start.max(p),
                });
                self.counters.switches += 1;
            }
            let (a, bsrc, w) = match self.switch {
                Some(sw) if p >= sw.start => {
                    let x = ((p - sw.start) as f32 + 0.5) / XFADE as f32;
                    let w = 0.5 - 0.5 * (std::f32::consts::PI * x).cos();
                    (sw.from, Some(sw.to), w)
                }
                _ => (self.selected, None, 0.0),
            };
            if let (Some(i), None) = (slot(self.selected), bsrc)
                && self.bufs[i].state[p.rem_euclid(cap as i64) as usize] == EMPTY
            {
                self.counters.filled += 1;
            }
            for c in 0..ch {
                let va = slot(a).map_or(0.0, |i| self.scratch[i][c * FRAME + k]);
                let v = match bsrc {
                    Some(bs) => {
                        let vb = slot(bs).map_or(0.0, |i| self.scratch[i][c * FRAME + k]);
                        va * (1.0 - w) + vb * w
                    }
                    None => va,
                };
                let v = if v.abs() > 1.0 {
                    clipped += 1;
                    v.clamp(-1.0, 1.0)
                } else {
                    v
                };
                if v != 0.0 {
                    silent = false;
                }
                out.data[c][k] = v;
            }
        }
        // Underruns of the selected source: valid followed by empty.
        if let Some(i) = slot(self.selected) {
            let b = &self.bufs[i];
            let mut prev = b.was_valid;
            for k in 0..FRAME {
                let v = b.state[(read + k as i64).rem_euclid(cap as i64) as usize] != EMPTY;
                if prev && !v {
                    self.counters.underruns += 1;
                }
                prev = v;
            }
        }
        // Consume the frame's slots.
        for b in &mut self.bufs {
            let last = (read + FRAME as i64 - 1).rem_euclid(cap as i64) as usize;
            b.was_valid = b.state[last] != EMPTY;
            for k in 0..FRAME {
                b.state[(read + k as i64).rem_euclid(cap as i64) as usize] = EMPTY;
            }
        }
        self.counters.clipped += clipped;
        self.counters.frames += 1;
        let info = FrameInfo {
            index: self.frame,
            pts: self.pts_of(self.frame),
            source: self.selected,
            silent,
        };
        self.frame += 1;
        self.read += FRAME as i64;
        info
    }
}

/// Apply a source's envelope to the frame starting at `read` and write it to `sc` (planar).
fn envelope(b: &mut Buffer, sc: &mut [f32], read: i64, cap: usize, ch: usize) {
    let st = |p: i64| -> u8 {
        if p >= read + cap as i64 {
            EMPTY
        } else {
            b.state[p.rem_euclid(cap as i64) as usize]
        }
    };
    // dist[k]: valid samples from read + k up to the next barrier (an empty sample or a segment
    // start after it), capped at FADE; computed backwards over the frame plus the look-ahead.
    let mut dist = [0u16; FRAME];
    let mut d: usize = 0;
    let mut barrier_after = st(read + (FRAME + FADE) as i64) != VALID;
    for k in (0..FRAME + FADE).rev() {
        let s = st(read + k as i64);
        d = if s == EMPTY {
            0
        } else if barrier_after {
            1
        } else {
            (d + 1).min(FADE)
        };
        if k < FRAME {
            dist[k] = d as u16;
        }
        barrier_after = s != VALID;
    }
    let mut g = b.env;
    let up = 1.0 / FADE as f32;
    for (k, &dk) in dist.iter().enumerate() {
        let s = st(read + k as i64);
        let at = (read + k as i64).rem_euclid(cap as i64) as usize;
        g = match s {
            EMPTY => 0.0,
            START => up.min(f32::from(dk) / FADE as f32),
            _ => (g + up).min(f32::from(dk) / FADE as f32),
        };
        for c in 0..ch {
            sc[c * FRAME + k] = if s == EMPTY {
                0.0
            } else {
                b.data[c * cap + at] * g
            };
        }
    }
    b.env = g;
}

/// The resampler trim of a source (SPEC §5.4): the mapper's skew taken out, plus a proportional
/// correction of the clock's drift.
///
/// The mapper's skew (`out = offset + in · (1 + skew)`) says how long the producer's second is
/// on our clock: `n` input samples must become `n · 48000/fs · (1 + skew)` output samples, so the
/// trim is `1/(1 + skew) − 1 ≈ −skew`. Whatever the skew estimate misses shows as drift, which
/// the correction `drift / (τ · 48 kHz)` removes with time constant `τ` (default 10 s). The
/// correction is clamped to `max_correction_ppb` and the total to ±500 ppm.
///
/// \[choice\] The SPEC calls the correction "small"; with the skew fed forward it is (the
/// residual of the estimate, a few ppm). Its default clamp is nevertheless the full 500 ppm, so
/// that a source whose skew is not known (no mapper, or a mapper still converging) is held by
/// the drift loop alone: a 300 ppm error then rests at a drift of 144 samples (3 ms), far
/// inside the clock's 20 ms tolerance. The loop (an integrator, the drift, behind a ~1 s
/// smoothing, with a 10 s proportional gain) is well damped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrimController {
    /// Time constant of the drift correction, seconds.
    pub tau_s: f64,
    /// Clamp of the correction, ppb.
    pub max_correction_ppb: i64,
}

impl Default for TrimController {
    fn default() -> Self {
        Self {
            tau_s: 10.0,
            max_correction_ppb: 500_000,
        }
    }
}

impl TrimController {
    /// The trim for a mapper skew of `skew_ppb` and a drift of `drift` samples.
    pub fn trim(&self, skew_ppb: i64, drift: Option<f64>) -> Trim {
        let skew = (skew_ppb as f64 * 1e-9).clamp(-0.01, 0.01);
        let base = (1.0 / (1.0 + skew) - 1.0) * 1e9;
        let corr = drift.map_or(0.0, |d| d / (self.tau_s.max(0.1) * 48_000.0) * 1e9);
        let m = self.max_correction_ppb.max(0) as f64;
        Trim::saturating((base + corr.clamp(-m, m)).round() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> SampleClock {
        SampleClock::new(ClockConfig::new(Ticks27(1_000_000), OutputLayout::Stereo)).unwrap()
    }

    fn block(v: f32, n: usize) -> Vec<f32> {
        vec![v; n]
    }

    /// The time of sample `s`, exactly (for stamps in tests).
    fn t(c: &SampleClock, s: i64) -> Ticks27 {
        c.time_of(s)
    }

    #[test]
    fn sample_times_round_trip() {
        let p0 = Ticks27(12_345);
        let c = SampleClock::new(ClockConfig::new(p0, OutputLayout::Stereo)).unwrap();
        for s in [-5, 0, 1, 2, 3, 1535, 1536, 48_000, 1 << 40] {
            assert_eq!(sample_at(p0, c.time_of(s)), s);
        }
        assert_eq!(c.pts_of(1).0 - c.pts_of(0).0, 864_000);
        assert_eq!(FRAME_27M, 300 * FRAME_90K as i64);
        assert_eq!(sample_at(p0, Ticks27(p0.0 + 281)), 0);
        assert_eq!(sample_at(p0, Ticks27(p0.0 + 282)), 1);
    }

    #[test]
    fn config_is_validated() {
        let mut c = ClockConfig::new(Ticks27(0), OutputLayout::Stereo);
        c.capacity = 100;
        assert!(SampleClock::new(c).is_err());
        c.capacity = 120_000;
        c.tolerance = 100_000;
        assert!(SampleClock::new(c).is_err());
    }

    /// No input: silent frames with exact PTS, on the slate.
    #[test]
    fn dry_clock_is_silent_with_exact_pts() {
        let mut c = clock();
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        for j in 0..100 {
            let i = c.next_frame(&mut f);
            assert_eq!(i.index, j);
            assert_eq!(i.pts.0, 1_000_000 + 864_000 * j as i64);
            assert!(i.silent);
            assert_eq!(i.source, Source::Slate);
        }
    }

    /// Contiguous input plays exactly, with a 10 ms fade-in at its start and a 10 ms fade-out
    /// at its end, and silence after.
    #[test]
    fn edges_fade_in_and_out() {
        let mut c = clock();
        c.switch(Source::Live, Ticks27(0));
        let x = block(0.5, 10_000);
        c.push(Source::Live, t(&c, 1000), &[&x, &x]).unwrap();
        let mut out = Vec::new();
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        for _ in 0..10 {
            c.next_frame(&mut f);
            out.extend_from_slice(f.plane(0));
        }
        assert!(out[..1000].iter().all(|&v| v == 0.0));
        assert!((out[1000] - 0.5 / 480.0).abs() < 1e-6);
        assert!((out[1000 + 479] - 0.5).abs() < 1e-6);
        assert!(out[1480..10_520].iter().all(|&v| v == 0.5));
        // Fade-out: reaches 0.5/480 at the last sample.
        assert!((out[10_999] - 0.5 / 480.0).abs() < 1e-6, "{}", out[10_999]);
        assert!(out[11_000..].iter().all(|&v| v == 0.0));
        let w: Vec<f32> = out[10_515..11_000]
            .windows(2)
            .map(|w| w[0] - w[1])
            .collect();
        assert!(w.iter().all(|&d| (-1e-7..0.5 / 400.0).contains(&d)));
        assert_eq!(c.counters().underruns, 1);
        // Not counted: the first 960 samples (the crossfade from the slate).
        assert_eq!(c.counters().filled, (1000 - 960) + (15_360 - 11_000));
    }

    /// Jittered stamps within the tolerance are placed contiguously: no gaps, no fades; the
    /// drift reports how far the count is from the stamps.
    #[test]
    fn jitter_is_absorbed_and_drift_measured() {
        let mut c = clock();
        c.switch(Source::Live, Ticks27(0));
        let mut s = 0i64;
        for i in 0..40 {
            let jitter = [0, 48, -48, 20][i % 4];
            // Stamps 20 samples short of the count every block: the count runs ahead.
            let x = block(0.25, 1024);
            c.push(Source::Live, t(&c, s + jitter - 20 * i as i64), &[&x, &x])
                .unwrap();
            s += 1024;
        }
        assert_eq!(c.source_counters(Source::Live).discontinuities, 0);
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        let mut out = Vec::new();
        for _ in 0..20 {
            c.next_frame(&mut f);
            out.extend_from_slice(f.plane(1));
        }
        assert!(out[960..30_000].iter().all(|&v| v == 0.25));
        let d = c.drift(Source::Live).unwrap();
        assert!(d > 50.0, "{d}");
    }

    /// A late block is dropped where it is late; an early block beyond the buffer is dropped;
    /// an overlapping block replaces, all counted.
    #[test]
    fn late_early_and_overlap_are_counted() {
        let mut c = clock();
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        c.next_frame(&mut f);
        let x = block(0.1, 2000);
        c.push(Source::Live, t(&c, 0), &[&x, &x]).unwrap();
        assert_eq!(c.source_counters(Source::Live).late, 1536);
        c.end(Source::Live);
        c.push(Source::Live, t(&c, 1536 + 119_000), &[&x, &x])
            .unwrap();
        assert_eq!(c.source_counters(Source::Live).early, 1000);
        c.end(Source::Live);
        c.push(Source::Live, t(&c, 1536 + 118_500), &[&x, &x])
            .unwrap();
        assert_eq!(c.source_counters(Source::Live).replaced, 1000);
        assert_eq!(c.source_counters(Source::Live).early, 1500);
        assert_eq!(c.source_counters(Source::Live).discontinuities, 0);
    }

    /// A gap larger than the tolerance makes a new segment: fade out, silence, fade in.
    #[test]
    fn gap_beyond_tolerance_is_a_new_segment() {
        let mut c = clock();
        c.switch(Source::Live, Ticks27(0));
        let x = block(0.5, 4000);
        c.push(Source::Live, t(&c, 0), &[&x, &x]).unwrap();
        c.push(Source::Live, t(&c, 6000), &[&x, &x]).unwrap();
        assert_eq!(c.source_counters(Source::Live).discontinuities, 1);
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        let mut out = Vec::new();
        for _ in 0..8 {
            c.next_frame(&mut f);
            out.extend_from_slice(f.plane(0));
        }
        assert!(out[3999] > 0.0 && out[3999] < 0.01);
        assert!(out[4000..6000].iter().all(|&v| v == 0.0));
        assert!(out[6000] > 0.0 && out[6000] < 0.01);
        assert_eq!(out[6500], 0.5);
    }

    /// A backward discontinuity (a restart) replaces the buffered future with a fade between.
    #[test]
    fn backward_restart_replaces_with_fades() {
        let mut c = clock();
        c.switch(Source::Live, Ticks27(0));
        let a = block(0.5, 10_000);
        c.push(Source::Live, t(&c, 0), &[&a, &a]).unwrap();
        let b = block(-0.5, 2000);
        c.push(Source::Live, t(&c, 5000), &[&b, &b]).unwrap();
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        let mut out = Vec::new();
        for _ in 0..8 {
            c.next_frame(&mut f);
            out.extend_from_slice(f.plane(0));
        }
        assert!(out[4999] > 0.0 && out[4999] < 0.01, "{}", out[4999]);
        assert!(out[5000] < 0.0 && out[5000] > -0.01);
        assert_eq!(out[6000], -0.5);
        assert!(out[7000..].iter().all(|&v| v == 0.0));
        assert_eq!(c.source_counters(Source::Live).replaced, 5000);
    }

    /// A source change crossfades over 20 ms with gains summing to one.
    #[test]
    fn switch_crossfades_over_20_ms() {
        let mut c = clock();
        c.switch(Source::Live, Ticks27(0));
        let a = block(0.5, 20_000);
        let b = block(-0.25, 20_000);
        c.push(Source::Live, t(&c, 0), &[&a, &a]).unwrap();
        c.push(Source::Standby, t(&c, 0), &[&b, &b]).unwrap();
        c.switch(Source::Standby, t(&c, 5000));
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        let mut out = Vec::new();
        for _ in 0..8 {
            let i = c.next_frame(&mut f);
            out.extend_from_slice(f.plane(0));
            assert_ne!(i.source, Source::Slate);
        }
        assert_eq!(out[4999], 0.5);
        assert_eq!(out[5000 + 960], -0.25);
        for k in 0..960 {
            let v = out[5000 + k];
            assert!((-0.25..=0.5).contains(&v));
        }
        assert!(out[5000..5960].windows(2).all(|w| w[1] <= w[0]));
        assert_eq!(c.counters().switches, 2);
        assert_eq!(c.selected(), Source::Standby);
    }

    /// Stamps at the ends of the range neither panic nor wrap.
    #[test]
    fn extreme_stamps_are_handled() {
        let mut c = clock();
        let x = block(0.5, 1000);
        for t in [i64::MAX, i64::MIN, 0, i64::MAX - 5, i64::MIN + 5] {
            c.push(Source::Live, Ticks27(t), &[&x, &x]).unwrap();
            c.push(Source::Standby, Ticks27(t), &[&x, &x]).unwrap();
            c.switch(Source::Standby, Ticks27(t));
        }
        let mut f = AudioFrame::new(OutputLayout::Stereo);
        for _ in 0..4 {
            c.next_frame(&mut f);
        }
        let k = c.source_counters(Source::Live);
        assert_eq!(k.received, 5000);
        assert!(k.late + k.early > 0);
    }

    #[test]
    fn push_checks_shapes() {
        let mut c = clock();
        assert_eq!(
            c.push(Source::Slate, Ticks27(0), &[&[0.0], &[0.0]]),
            Err(PushError::Slate)
        );
        assert!(c.push(Source::Live, Ticks27(0), &[&[0.0]]).is_err());
        assert_eq!(
            c.push(Source::Live, Ticks27(0), &[&[0.0], &[]]),
            Err(PushError::Ragged)
        );
    }

    #[test]
    fn trim_controller_signs() {
        let tc = TrimController::default();
        // Producer clock slow by 100 ppm: its second is 1.0001 of ours (skew +100 ppm): make
        // more output per input, trim ≈ −100 ppm.
        assert!((tc.trim(100_000, None).ppb() + 99_990).abs() <= 1);
        // Count ahead of stamps by 48 samples: take input faster, +100 ppm over 10 s.
        assert_eq!(tc.trim(0, Some(48.0)).ppb(), 100_000);
        assert_eq!(tc.trim(0, Some(4800.0)).ppb(), 500_000);
        assert_eq!(tc.trim(-900_000, Some(0.0)).ppb(), 500_000);
    }
}
