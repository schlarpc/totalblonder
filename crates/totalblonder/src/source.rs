//! A program's picture source (SPEC §5.4, §9.5): decoded frames of its live input, already on
//! the presentation timeline (the input's mapper runs in its decoder thread, `live.rs`), one
//! chosen per output tick by the frame clock, and the fallback chain deciding between them and
//! the slate. Both state machines are `tb-timeline`'s; this is the plumbing around them and the
//! frame store.
//!
//! Time domains: input timestamps are in 27 MHz ticks of the input (unwrapped by the
//! receiver); arrival and output times are process time. A frame is handed to the mapper as if
//! it had arrived `Δ` later than it did, so the mapper's jitter buffer `J` is measured on the
//! presentation timeline: a frame that arrives at `r` is shown at about `r + Δ + J`, and the
//! hub, which asks for the picture of presentation time `t` at `t − Δ`, always finds the frames
//! that arrived at least `J` before.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use tb_timeline::film::Key;
use tb_timeline::{Fallback, FrameClock, FrameId, Selection, Source, Ticks27};

use crate::metrics::{Counter, Gauge};
use crate::slate::Slate;
use crate::video::{FrameProvider, Picture, Shot};

/// What a live input delivers to its program.
pub enum LiveEvent {
    /// A decoded picture at the program's size (planar 4:2:0), already on the
    /// presentation timeline by the input's mapper (`live.rs`).
    Frame {
        /// Presentation time, process ticks.
        at: u64,
        /// The picture.
        data: Vec<u8>,
        /// Its two fields are from different instants (an interlaced source): in film mode
        /// each field is then content of its own, never repeated as a whole frame.
        interlaced: bool,
        /// Its Active Format Description (`tb_m2v_syntax::afd`): where in the frame the
        /// source's picture is.
        afd: tb_m2v_syntax::afd::ActiveFormat,
    },
    /// The input's session ended (connection closed, decoder restarted).
    Closed,
}

/// Counters of a program's source.
#[derive(Debug, Clone, Default)]
pub struct SourceStats {
    /// 1 live, 2 standby, 3 slate (SPEC §11.3 `program_source`).
    pub source: Gauge,
    /// Source changes.
    pub switches: Counter,
    /// Frames repeated by the frame clock.
    pub repeats: Gauge,
    /// Frames dropped by the frame clock.
    pub drops: Gauge,
    /// Mapper sessions started.
    pub sessions: Counter,
    /// Frames received.
    pub frames: Counter,
}

/// The fallback timings (SPEC §9.5).
#[derive(Debug, Clone, Copy)]
pub struct FallbackTimes {
    /// Repeats of the live source tolerated before moving down the chain.
    pub hold: u64,
    /// Health a source must show before it is cut to.
    pub promote_after: u64,
}

impl Default for FallbackTimes {
    fn default() -> Self {
        FallbackTimes {
            hold: 27_000_000,
            promote_after: 3 * 27_000_000,
        }
    }
}

/// The geometry of the program's output cadence, to know where GOPs start.
#[derive(Debug, Clone, Copy)]
pub struct Cadence {
    /// Presentation time of frame 0.
    pub p0: u64,
    /// Frame period, ticks.
    pub period: u64,
    /// Key frame distance.
    pub gop: u32,
    /// The session's `gop_phase`.
    pub phase: u32,
    /// Δ.
    pub delay: u64,
    /// Selections per frame period: 2 when film mode samples an interlaced output per field,
    /// else 1.
    pub units: u32,
}

/// One stored frame.
struct Stored {
    id: FrameId,
    data: Arc<Vec<u8>>,
    interlaced: bool,
    afd: tb_m2v_syntax::afd::ActiveFormat,
}

/// One input feeding the program (live or standby): its queue, frame clock and frames.
struct Feed {
    rx: Receiver<LiveEvent>,
    clock: FrameClock,
    frames: VecDeque<Stored>,
    next_id: u64,
    frames_in: Counter,
}

impl Feed {
    fn new(rx: Receiver<LiveEvent>, unit: u64, frames_in: Counter) -> Self {
        Feed {
            rx,
            clock: FrameClock::new(Ticks27(unit as i64)),
            frames: VecDeque::new(),
            next_id: 0,
            frames_in,
        }
    }

    fn take_input(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(LiveEvent::Frame {
                    at,
                    data,
                    interlaced,
                    afd,
                }) => {
                    self.frames_in.inc();
                    let id = FrameId(self.next_id);
                    self.next_id += 1;
                    self.clock.push(id, Ticks27(at as i64));
                    self.frames.push_back(Stored {
                        id,
                        data: Arc::new(data),
                        interlaced,
                        afd,
                    });
                    while self.frames.len() > STORE {
                        self.frames.pop_front();
                    }
                }
                Ok(LiveEvent::Closed) | Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    return;
                }
            }
        }
    }

    /// The frame clock's choice at `t`; frames older than it released.
    fn tick(&mut self, t: Ticks27) -> Selection {
        self.take_input();
        let sel = self.clock.tick(t);
        if let Selection::New(id) | Selection::Repeat(id) = sel {
            while self.frames.front().is_some_and(|f| f.id.0 < id.0) {
                self.frames.pop_front();
            }
        }
        sel
    }

    fn frame(&self, id: FrameId) -> Option<&Stored> {
        self.frames.iter().find(|f| f.id == id)
    }
}

/// The picture source of a program with a live input, a standby input, or both, above the
/// slate.
pub struct LiveProvider {
    /// Live (index 0) and standby (index 1).
    feeds: [Option<Feed>; 2],
    fallback: Fallback,
    slate: Slate,
    /// The slate rendered for film mode, and the second it shows.
    slate_shot: Option<(u64, Arc<Vec<u8>>)>,
    cadence: Cadence,
    /// What was written into the encoder's buffer last: the source and frame.
    last: Option<(Source, Option<FrameId>)>,
    /// The Active Format Description of what was handed out last.
    afd: tb_m2v_syntax::afd::ActiveFormat,
    stats: SourceStats,
    /// The program's context, for `source.switch` events.
    ctx: tracing::Span,
    /// When the source on air was cut to (process time).
    since: u64,
}

/// Frames kept for the frame clock (beyond this the oldest are dropped by the store).
const STORE: usize = 64;

/// The slate's source number in film keys.
const SLATE_SRC: u32 = u32::MAX;

fn feed_index(s: Source) -> Option<usize> {
    match s {
        Source::Live => Some(0),
        Source::Standby => Some(1),
        Source::Slate => None,
    }
}

impl LiveProvider {
    /// A provider fed by `live` and `standby` (either may be absent), falling back to `slate`.
    pub fn new(
        live: Option<Receiver<LiveEvent>>,
        standby: Option<Receiver<LiveEvent>>,
        slate: Slate,
        cadence: Cadence,
        times: FallbackTimes,
        stats: SourceStats,
    ) -> Self {
        let unit = cadence.period / u64::from(cadence.units.max(1));
        LiveProvider {
            feeds: [
                live.map(|rx| Feed::new(rx, unit, stats.frames.clone())),
                standby.map(|rx| Feed::new(rx, unit, stats.frames.clone())),
            ],
            fallback: Fallback::new(
                Ticks27(times.hold as i64),
                Ticks27(times.promote_after as i64),
            ),
            slate,
            slate_shot: None,
            cadence,
            last: None,
            afd: tb_m2v_syntax::afd::ActiveFormat::FULL,
            stats,
            ctx: tracing::Span::none(),
            since: 0,
        }
    }

    /// Reports its switches in `ctx` (the program's span).
    pub fn with_context(mut self, ctx: tracing::Span) -> Self {
        self.ctx = ctx;
        self
    }

    /// The source and frame to show at display time `t` (`None` frame: the slate).
    fn choose(&mut self, t: u64) -> (Source, Option<FrameId>) {
        let tt = Ticks27(t as i64);
        let sel: [Selection; 2] = [0, 1].map(|i| {
            self.feeds[i]
                .as_mut()
                .map_or(Selection::Nothing, |f| f.tick(tt))
        });
        if let Some(f) = &self.feeds[0] {
            self.stats.repeats.set(f.clock.repeats);
            self.stats.drops.set(f.clock.drops);
        }
        let c = self.cadence;
        let k = t.saturating_sub(c.p0) / c.period.max(1);
        // A cut is taken at the first unit of a frame period that starts an output GOP.
        let first_unit =
            t.saturating_sub(c.p0) % c.period.max(1) < c.period / u64::from(c.units.max(1));
        let gop_start =
            first_unit && (k + u64::from(c.phase)).is_multiple_of(u64::from(c.gop.max(1)));
        let ok = |s: Selection| matches!(s, Selection::New(_));
        let src = self.fallback.tick(tt, ok(sel[0]), ok(sel[1]), gop_start);
        let code = match src {
            Source::Live => 1,
            Source::Standby => 2,
            Source::Slate => 3,
        };
        let was = self.stats.source.get();
        if was != code {
            if was != 0 {
                self.stats.switches.inc();
                let name = |c: u64| match c {
                    1 => "live",
                    2 => "standby",
                    _ => "slate",
                };
                // Down the chain is a fault somewhere upstream; back up is recovery.
                let level = if code > was {
                    tracing::Level::WARN
                } else {
                    tracing::Level::INFO
                };
                let ev = tb_telemetry::Wide::child_of(&self.ctx, "source.switch")
                    .with("from", name(was))
                    .with("to", name(code))
                    .with(
                        "held_s",
                        t.saturating_sub(self.since) as f64 / crate::time::HZ as f64,
                    )
                    .with("live.ok", ok(sel[0]))
                    .with("standby.ok", ok(sel[1]));
                ev.level(level);
                ev.emit();
            }
            self.since = t;
            self.stats.source.set(code);
        }
        let frame = feed_index(src).and_then(|i| match sel[i] {
            Selection::New(id) | Selection::Repeat(id) => Some(id),
            Selection::Nothing => None,
        });
        (src, frame)
    }

    fn stored(&self, src: Source, id: FrameId) -> Option<&Stored> {
        feed_index(src)
            .and_then(|i| self.feeds[i].as_ref())
            .and_then(|f| f.frame(id))
    }
}

impl FrameProvider for LiveProvider {
    fn on_slate(&self) -> bool {
        self.stats.source.get() == 3
    }

    fn active_format(&self) -> tb_m2v_syntax::afd::ActiveFormat {
        self.afd
    }

    fn reformat(&mut self, w: u32, h: u32, cadence: Cadence) {
        // Frames of the old size are of no use; the decoder thread follows the program's rung
        // and sends frames of the new size after its worker takes the new spec.
        let unit = cadence.period / u64::from(cadence.units.max(1));
        for f in self.feeds.iter_mut().flatten() {
            f.frames.clear();
            f.clock = FrameClock::new(Ticks27(unit as i64));
        }
        self.slate = self.slate.resized(w, h);
        self.slate_shot = None;
        self.cadence = cadence;
        self.last = None;
        self.afd = tb_m2v_syntax::afd::ActiveFormat::FULL;
    }

    fn picture(&mut self, t: u64, out: &mut [u8]) -> Picture {
        if let (src, Some(id)) = self.choose(t) {
            if self.last == Some((src, Some(id))) {
                return Picture::Repeat;
            }
            if let Some(f) = self.stored(src, id)
                && f.data.len() == out.len()
            {
                out.copy_from_slice(&f.data);
                self.afd = f.afd;
                self.last = Some((src, Some(id)));
                return Picture::Fresh;
            }
        }
        let switched = self.last.is_none_or(|(s, _)| s != Source::Slate);
        if switched {
            self.slate.invalidate();
        }
        self.afd = tb_m2v_syntax::afd::ActiveFormat::FULL;
        self.last = Some((Source::Slate, None));
        if self.slate.render(t / crate::time::HZ, out) {
            Picture::Fresh
        } else {
            Picture::Repeat
        }
    }

    fn shot(&mut self, t: u64, len: usize) -> Option<Shot> {
        if let (src, Some(id)) = self.choose(t)
            && let Some(f) = self.stored(src, id)
            && f.data.len() == len
        {
            let n = feed_index(src).unwrap_or(0) as u32;
            let afd = f.afd;
            let key = if f.interlaced {
                Key::Unique
            } else {
                Key::Instant {
                    src: n,
                    idx: id.0 as i64,
                }
            };
            let shot = Shot {
                key,
                data: f.data.clone(),
            };
            self.afd = afd;
            return Some(shot);
        }
        let sec = t / crate::time::HZ;
        self.afd = tb_m2v_syntax::afd::ActiveFormat::FULL;
        if self
            .slate_shot
            .as_ref()
            .is_none_or(|(s, d)| *s != sec || d.len() != len)
        {
            let mut buf = vec![0u8; len];
            self.slate.invalidate();
            self.slate.render(sec, &mut buf);
            self.slate_shot = Some((sec, Arc::new(buf)));
        }
        let (sec, data) = self.slate_shot.clone()?;
        Some(Shot {
            key: Key::Instant {
                src: SLATE_SRC,
                idx: sec as i64,
            },
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    const P: u64 = 900_900;

    #[test]
    fn slate_then_live_then_slate() {
        let (tx, rx) = channel();
        let (w, h) = (64u32, 32u32);
        let cad = Cadence {
            p0: 0,
            period: P,
            gop: 15,
            phase: 0,
            delay: 27_000_000,
            units: 1,
        };
        let stats = SourceStats::default();
        let mut p = LiveProvider::new(
            Some(rx),
            None,
            Slate::new(w, h, "T"),
            cad,
            FallbackTimes::default(),
            stats.clone(),
        );
        let mut out = vec![0u8; (w * h * 3 / 2) as usize];
        let frame = |n: u8| vec![n; (w * h * 3 / 2) as usize];
        let pillar = tb_m2v_syntax::afd::ActiveFormat {
            afd: tb_m2v_syntax::afd::PILLAR_4_3,
            bars: Some(tb_m2v_syntax::afd::Bars::LeftRight {
                left_end: 87,
                right_start: 624,
            }),
        };
        let mut live_seen = false;
        // 20 s: input from second 2 to second 10 at 29.97, then nothing.
        for k in 0..600u64 {
            let t = k * P;
            // The hub asks at t − Δ; frames arriving then.
            let now = t.saturating_sub(cad.delay);
            if (60..300).contains(&k) {
                // Mapped one second after arrival, on the presentation timeline.
                let at = now + cad.delay + 27_000_000;
                tx.send(LiveEvent::Frame {
                    at,
                    data: frame((k % 200) as u8 + 20),
                    interlaced: false,
                    afd: pillar,
                })
                .unwrap();
            }
            let pic = p.picture(t, &mut out);
            if stats.source.get() == 1 {
                live_seen = true;
                if pic == Picture::Fresh {
                    assert!(out[0] >= 20, "a live frame, not the slate");
                    assert_eq!(p.active_format(), pillar);
                }
            }
        }
        assert!(live_seen, "live was promoted");
        // Back on the slate after the input stopped (hold + one GOP).
        assert_eq!(stats.source.get(), 3);
        assert_eq!(p.active_format(), tb_m2v_syntax::afd::ActiveFormat::FULL);
        assert!(stats.switches.get() >= 2);
    }
}
