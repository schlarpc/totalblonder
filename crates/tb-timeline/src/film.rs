//! Film mode: turning the timeline's selection per output field into coded pictures that say
//! how long they are shown.
//!
//! The timeline selects one source picture for every *display unit* of a channel: every field
//! of an interlaced channel (1080i, 480i), every frame period of a progressive one (720p at
//! 59.94 Hz). When a 24 Hz source is shown on such a channel the selections repeat in the 3:2
//! pattern: three units of one source frame, two of the next. Coding those units as they come
//! means 30 (or 60) pictures per second, two fifths of the interlaced ones woven from two
//! film frames. The [`Assembler`] instead groups consecutive units that show the same instant
//! into one picture and says how long it is shown; the encoder writes that as
//! repeat_first_field (`tb_m2v_encoder::Encoder::submit_shown`) and the *decoder* does the
//! pulldown. It makes no guess and keeps no model of a cadence: it looks at what the units
//! actually are, one picture at a time, so a cadence that breaks costs nothing but the
//! picture at the break.
//!
//! # Rules
//!
//! Units arrive in display order ([`Assembler::push`]); each has a [`Key`] that says what it
//! shows. A picture is a run of units from the head of the queue:
//!
//! | Head of the queue | Interlaced channel (units are fields) | Progressive channel (units are frame periods) |
//! |---|---|---|
//! | three units of one instant | a progressive picture of three fields | a frame shown three times |
//! | two of one instant, then another | a progressive picture of two fields | a frame shown twice |
//! | one of an instant, then another | an interlaced picture of two fields, one from each (the fallback: fields of two instants are woven, exactly what the channel showed before film mode) | a frame shown once |
//! | a unit that is nobody's twin ([`Key::Unique`]: a field of interlaced video, a mosaic) | an interlaced picture of it and the next unit | a frame shown once |
//!
//! Three units need [`FilmCfg::three`]; without it (film mode off, or a sequence that may not
//! use repeat_first_field) pictures are two fields (one frame period) long as they always
//! were, and nothing waits. The assembler decides as soon as it can: a picture leaves when the
//! queue shows where its run ends, which takes one unit beyond the run, so the latency is at
//! most two frame periods on an interlaced channel and two units on a progressive one, and the
//! queue never holds more than four units. [`Assembler::pull`] gives at most one picture per
//! call (the encoder takes one frame per session and tick); with 3:2 pulldown four calls in
//! five give one, and the fifth gives none: nothing is due.
//!
//! What holds for any sequence of units whatsoever (the property tests of this module):
//!
//! - every unit is in exactly one picture, in order: display time is covered exactly once;
//! - the first field of each picture has the parity the display needs
//!   ([`Picture::top_first`]): fields alternate through pictures of two and three (H.262 7.12);
//! - a picture of three units, and a picture flagged progressive, shows one instant only
//!   (H.262 6.3.10: repeat_first_field needs progressive_frame);
//! - at the end ([`Assembler::flush`]) everything queued leaves; a single last field is shown
//!   as a two-field picture (one field of display more than was pushed, at the very end).
//!
//! # Breaks
//!
//! [`FilmStats`] counts pictures by duration and the places where film stops being a run of
//! whole frames in turn, by what is seen there ([`Break`]): fields of two instants woven, video,
//! the picture before again, a frame that is not the next one of its source, and, while the
//! pictures before were alternating three and two, the next frame at the wrong length. Each is
//! counted where it begins (a still of a hundred pictures is one break, video of an hour one).
//! The causes the caller knows better (a source cut, a held session) it can tell from the keys
//! it handed in; the assembler names what it can see.

use std::collections::VecDeque;

/// What a display unit shows, as far as sameness goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A whole picture from one instant: frame `idx` of source `src` (an input; the slate
    /// under a number of its own). Two units with the same key show the same picture.
    Instant {
        /// The source.
        src: u32,
        /// Its frame.
        idx: i64,
    },
    /// Content that no other unit shares: one field of an interlaced source frame, a
    /// composition of several inputs.
    Unique,
}

/// One display unit: what it shows and whatever the caller needs to make it (`data`: the
/// source frame and rows, a handle that keeps the decoded frame alive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit<T> {
    /// What it shows.
    pub key: Key,
    /// The caller's.
    pub data: T,
}

/// How an [`Assembler`] is set up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FilmCfg {
    /// Units are fields (two per frame period); else frame periods.
    pub interlaced: bool,
    /// Interlaced: the channel's first field is the top field.
    pub top_first: bool,
    /// Pictures may be shown for other than one frame period (film mode on, and the sequence
    /// may use repeat_first_field: H.262 Table 8-7). Off: two fields (one frame period) each.
    pub three: bool,
}

/// What was seen at a cadence break ([`FilmStats::breaks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Break {
    /// Fields of two instants in one picture, after a picture of one instant: a film frame
    /// was shown for one field only (a cut or an edit in the middle of the cadence, a frame the
    /// clock dropped), or progressive video faster than the channel's frame rate begins.
    Woven,
    /// Units that are nobody's twin, after a picture of one instant: interlaced video, a
    /// mosaic.
    Video,
    /// The picture before again, after one that was new: the source did not advance (a repeat
    /// by the frame clock, a stalled or held source, a still, the slate).
    Still,
    /// Not the next frame of the same source: another source, the slate, frames the clock
    /// dropped (counted also when the durations go on alternating: the pictures are whole,
    /// but the film is not continuous).
    Jump,
    /// The next frame of the same source at the wrong length, while the pictures before were
    /// alternating three and two (two after two, three after three): the phase of the cadence
    /// moved, as it does at an edit of the film itself and, regularly, when the source's rate
    /// is not exactly 4/5 of the channel's. Also the first sign of a source that stalls.
    Shift,
}

impl Break {
    /// Every kind, in the order of [`FilmStats::breaks`].
    pub const ALL: [Self; 5] = [
        Self::Woven,
        Self::Video,
        Self::Still,
        Self::Jump,
        Self::Shift,
    ];

    /// A short name for reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Woven => "woven",
            Self::Video => "video",
            Self::Still => "still",
            Self::Jump => "jump",
            Self::Shift => "shift",
        }
    }
}

/// Counters of an [`Assembler`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilmStats {
    /// Units pushed.
    pub units_in: u64,
    /// Units in the pictures given out (one more than pushed when a flush padded a field).
    pub units_out: u64,
    /// Pictures by duration in units (index 1, 2, 3).
    pub pictures: [u64; 4],
    /// Pictures flagged progressive (one instant).
    pub progressive: u64,
    /// Pictures that show the instant of the picture before (nothing to convert).
    pub again: u64,
    /// Breaks by [`Break`], in the order of [`Break::ALL`].
    pub breaks: [u64; 5],
    /// Calls of [`Assembler::pull`] that had no picture to give.
    pub idle: u64,
}

/// One coded picture: which units it shows and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture<T> {
    /// Display units before it since the start.
    pub start: u64,
    /// Units it is shown for: 2 or 3 fields, or 1, 2 or 3 frame periods.
    pub units: u8,
    /// Interlaced channel: its first field is the top field.
    pub top_first: bool,
    /// All its units show one instant.
    pub progressive: bool,
    /// It shows the instant of the picture before it: the encoder can code the previous
    /// frame again.
    pub again: bool,
    /// The cadence break it is, if the pictures before were alternating three and two.
    pub brk: Option<Break>,
    /// A flush showed its single field twice.
    pub padded: bool,
    first: Unit<T>,
    second: Option<Unit<T>>,
}

impl<T> Picture<T> {
    /// The unit shown first.
    pub fn first(&self) -> &Unit<T> {
        &self.first
    }

    /// The other source of an interlaced picture woven from two units (`None` when every
    /// field shows [`Picture::first`]).
    pub fn second(&self) -> Option<&Unit<T>> {
        self.second.as_ref()
    }

    /// Interlaced channel: the units behind the top rows and behind the bottom rows.
    pub fn top_bottom(&self) -> (&Unit<T>, &Unit<T>) {
        let second = self.second.as_ref().unwrap_or(&self.first);
        if self.top_first {
            (&self.first, second)
        } else {
            (second, &self.first)
        }
    }
}

/// Groups display units into pictures; see the module documentation.
#[derive(Clone, Debug)]
pub struct Assembler<T> {
    cfg: FilmCfg,
    queue: VecDeque<Unit<T>>,
    start: u64,
    next_top: bool,
    /// The instant of the last picture, if it showed one.
    last: Option<Key>,
    /// Durations of the last two progressive pictures in a row (0: none).
    cadence: [u8; 2],
    /// The last picture showed the instant of the one before it.
    last_again: bool,
    /// The counters.
    pub stats: FilmStats,
}

impl<T> Assembler<T> {
    /// An assembler before its first unit.
    pub fn new(cfg: FilmCfg) -> Self {
        Self {
            cfg,
            queue: VecDeque::with_capacity(6),
            start: 0,
            next_top: cfg.top_first,
            last: None,
            cadence: [0; 2],
            last_again: false,
            stats: FilmStats::default(),
        }
    }

    /// The configuration.
    pub fn cfg(&self) -> &FilmCfg {
        &self.cfg
    }

    /// Turn pictures of other than one frame period on or off from the next picture on. The
    /// field parity carries on either way: after an odd number of three-field pictures the
    /// channel's pictures begin with the other field until another one evens it out.
    pub fn set_three(&mut self, three: bool) {
        self.cfg.three = three;
    }

    /// Units waiting.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Display units given out in pictures so far.
    pub fn display_units(&self) -> u64 {
        self.start
    }

    /// Interlaced: the parity the next picture begins with.
    pub fn next_top_first(&self) -> bool {
        self.next_top
    }

    /// The next display unit.
    pub fn push(&mut self, unit: Unit<T>) {
        self.stats.units_in += 1;
        self.queue.push_back(unit);
    }

    /// Units of the head's instant at the head of the queue (1 for a unique head), 3 at most.
    fn run(&self) -> usize {
        let Some(head) = self.queue.front() else {
            return 0;
        };
        if head.key == Key::Unique {
            return 1;
        }
        self.queue
            .iter()
            .take(3)
            .take_while(|u| u.key == head.key)
            .count()
    }

    /// The next picture, if the queue decides one. Call it once per output frame period, after
    /// the pushes of that period.
    pub fn pull(&mut self) -> Option<Picture<T>> {
        let n = self.queue.len();
        let run = self.run();
        let unique = self.queue.front().is_some_and(|u| u.key == Key::Unique);
        // How many units the picture takes; `None`: not known yet.
        let take = if self.cfg.interlaced {
            if n < 2 {
                None
            } else if !self.cfg.three {
                // two fields, at once
                Some(2)
            } else if run == 3 {
                Some(3)
            } else if run == 2 {
                // two of one instant: the third unit decides between two and three
                (n >= 3).then_some(2)
            } else {
                // one field of an instant and one of another, or video: woven
                Some(2)
            }
        } else if n == 0 {
            None
        } else if !self.cfg.three || unique {
            Some(1)
        } else if run == 3 {
            Some(3)
        } else {
            // the run has ended if a unit of another instant is behind it
            (run < n).then_some(run)
        };
        let picture = take.and_then(|k| self.emit(k, run, false));
        if picture.is_none() {
            self.stats.idle += 1;
        }
        picture
    }

    /// No more units: the next picture of what is queued (call until `None`). Runs are closed
    /// where they stand; a single last field of an interlaced channel is shown twice.
    pub fn flush(&mut self) -> Option<Picture<T>> {
        let n = self.queue.len();
        if n == 0 {
            return None;
        }
        let run = self.run();
        if !self.cfg.interlaced {
            let k = if self.cfg.three { run } else { 1 };
            return self.emit(k, run, false);
        }
        if n == 1 {
            return self.emit(1, 1, true);
        }
        let k = if run == 3 && self.cfg.three { 3 } else { 2 };
        self.emit(k, run, false)
    }

    /// Take `k` units off the queue as one picture. `run`: units of the head's instant.
    fn emit(&mut self, k: usize, run: usize, pad: bool) -> Option<Picture<T>> {
        let first = self.queue.pop_front()?;
        let mut second = None;
        for i in 1..k {
            let u = self.queue.pop_front();
            if i == 1 {
                second = u;
            }
        }
        let units = if pad { 2 } else { k as u8 };
        let one_instant = first.key != Key::Unique && run >= k;
        let progressive = one_instant || !self.cfg.interlaced;
        let key = one_instant.then_some(first.key);
        let again = key.is_some() && key == self.last;
        let top_first = self.cfg.interlaced && self.next_top;
        if self.cfg.interlaced && units == 3 {
            self.next_top = !self.next_top;
        }
        // ---- the cadence, and what breaks it
        let locked = matches!(self.cadence, [3, 2] | [2, 3]);
        let expected = self.cadence[0];
        let film = one_instant && !again && (self.cfg.interlaced || units >= 2);
        // the next frame of the same source
        let next_frame = matches!(
            (self.last, first.key),
            (Some(Key::Instant { src: a, idx: i }), Key::Instant { src: b, idx: j })
                if a == b && j == i + 1
        );
        // What is seen at this picture. A picture after one that showed a single instant can be
        // a break of film; a picture after woven fields or video is not one again (a run of
        // them counts once, where it begins).
        let after_instant = self.last.is_some();
        let brk = if !one_instant {
            let video =
                first.key == Key::Unique && second.as_ref().is_none_or(|s| s.key == Key::Unique);
            after_instant.then_some(if video { Break::Video } else { Break::Woven })
        } else if again {
            // the first repeat of a run
            (!self.last_again).then_some(Break::Still)
        } else if after_instant && !next_frame {
            Some(Break::Jump)
        } else if locked && next_frame && units != expected {
            Some(Break::Shift)
        } else {
            None
        };
        self.last_again = again;
        if let Some(b) = brk {
            self.stats.breaks[b as usize] += 1;
        }
        self.cadence = if film {
            [self.cadence[1], units]
        } else {
            [0, 0]
        };
        self.last = key;
        let second = if one_instant { None } else { second };
        let p = Picture {
            start: self.start,
            units,
            top_first,
            progressive,
            again,
            brk,
            padded: pad,
            first,
            second,
        };
        self.start += u64::from(units);
        self.stats.units_out += u64::from(units);
        self.stats.pictures[usize::from(units.min(3))] += 1;
        self.stats.progressive += u64::from(progressive);
        self.stats.again += u64::from(again);
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64, as in the crate's property tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    const fn cfg(interlaced: bool, three: bool) -> FilmCfg {
        FilmCfg {
            interlaced,
            top_first: true,
            three,
        }
    }

    fn inst(idx: i64) -> Key {
        Key::Instant { src: 0, idx }
    }

    /// Push `keys` as a channel does (two units per call of `pull` when interlaced, else
    /// one), then flush. The unit's data is its position in the input.
    fn run(c: FilmCfg, keys: &[Key]) -> (Vec<Picture<usize>>, FilmStats, usize) {
        let mut a = Assembler::new(c);
        let mut out = Vec::new();
        let mut most = 0;
        let per = if c.interlaced { 2 } else { 1 };
        for (i, chunk) in keys.chunks(per).enumerate() {
            for (j, k) in chunk.iter().enumerate() {
                a.push(Unit {
                    key: *k,
                    data: i * per + j,
                });
            }
            most = most.max(a.queued());
            let before = a.display_units();
            if let Some(p) = a.pull() {
                // the latency: a picture leaves at most two frame periods after its first unit
                // was pushed
                assert!(
                    (i * per + per) as u64 - before <= if c.interlaced { 4 } else { 3 },
                    "picture at {before} left at {}",
                    i * per + per
                );
                out.push(p);
            }
        }
        while let Some(p) = a.flush() {
            out.push(p);
        }
        assert_eq!(a.queued(), 0);
        (out, a.stats, most)
    }

    /// The properties of the module documentation, for any input.
    fn check(c: FilmCfg, keys: &[Key], pics: &[Picture<usize>], most: usize) {
        assert!(most <= 4, "queue of {most}");
        let mut at = 0usize;
        let mut top = c.top_first;
        for p in pics {
            assert_eq!(p.start as usize, at, "display time has no gap");
            let n = usize::from(p.units) - usize::from(p.padded);
            // its units are the next ones of the input, in order
            assert_eq!(p.first().data, at);
            if let Some(s) = p.second() {
                assert_eq!(s.data, at + 1);
                assert!(!p.progressive || !c.interlaced);
            }
            let ks = &keys[at..at + n];
            let same = ks[0] != Key::Unique && ks.iter().all(|k| *k == ks[0]);
            if c.interlaced {
                assert!(matches!(p.units, 2 | 3));
                assert_eq!(p.top_first, top, "fields alternate");
                if p.units == 3 {
                    top = !top;
                }
                assert_eq!(p.progressive, same, "progressive is one instant");
                if p.units == 3 {
                    assert!(same && c.three, "three fields of one instant only");
                }
                let (t, b) = p.top_bottom();
                if p.second().is_some() {
                    assert_eq!(
                        (t.data, b.data),
                        if p.top_first {
                            (at, at + 1)
                        } else {
                            (at + 1, at)
                        }
                    );
                } else {
                    assert_eq!((t.data, b.data), (at, at));
                }
            } else {
                assert!(matches!(p.units, 1..=3) && p.progressive && p.second().is_none());
                assert!(same || n == 1);
                assert!(c.three || p.units == 1);
            }
            at += n;
        }
        assert_eq!(at, keys.len(), "every unit exactly once");
    }

    /// The keys of a source of `num / den` frames per unit, sampled as the timeline does
    /// (the newest frame due at each unit).
    fn sampled(units: usize, num: u64, den: u64) -> Vec<Key> {
        (0..units as u64)
            .map(|u| inst((u * num / den) as i64))
            .collect()
    }

    #[test]
    fn three_two_pulldown_becomes_pictures_of_three_and_two_fields() {
        // 24 frames per 60 fields: A A A B B C C C D D
        let keys = sampled(120, 2, 5);
        let (pics, stats, most) = run(cfg(true, true), &keys);
        check(cfg(true, true), &keys, &pics, most);
        assert_eq!(pics.len(), 48);
        assert!(
            pics.iter()
                .all(|p| p.progressive && p.brk.is_none() && !p.again)
        );
        let shape: Vec<(u8, bool)> = pics[..4].iter().map(|p| (p.units, p.top_first)).collect();
        assert_eq!(shape, [(3, true), (2, false), (3, false), (2, true)]);
        assert_eq!(stats.pictures, [0, 0, 24, 24]);
        assert_eq!(stats.breaks, [0; 5]);
        // four pictures in five frame periods: one call in five has nothing due
        assert_eq!(stats.idle, 60 - 48 + 1);
        // off: the same units as 60 pictures, 24 of them woven from two film frames
        let (pics, stats, most) = run(cfg(true, false), &keys);
        check(cfg(true, false), &keys, &pics, most);
        assert_eq!(stats.pictures, [0, 0, 60, 0]);
        assert_eq!(pics.iter().filter(|p| !p.progressive).count(), 24);
        assert_eq!(stats.idle, 0);
    }

    #[test]
    fn frame_repeats_on_a_progressive_channel() {
        let keys = sampled(120, 2, 5);
        let (pics, stats, most) = run(cfg(false, true), &keys);
        check(cfg(false, true), &keys, &pics, most);
        assert_eq!(stats.pictures, [0, 0, 24, 24]);
        assert_eq!(stats.breaks, [0; 5]);
        // 59.94 Hz video: one frame period each, one unit of latency
        let video: Vec<Key> = (0..50).map(inst).collect();
        let (pics, stats, most) = run(cfg(false, true), &video);
        check(cfg(false, true), &video, &pics, most);
        assert_eq!(stats.pictures, [0, 50, 0, 0]);
        // 29.97 Hz progressive on a 59.94 Hz channel: every frame twice
        let half = sampled(60, 1, 2);
        let (pics, stats, _) = run(cfg(false, true), &half);
        assert_eq!(stats.pictures, [0, 0, 30, 0]);
        assert!(pics.iter().all(|p| p.brk.is_none()));
    }

    #[test]
    fn video_and_progressive_video_on_an_interlaced_channel() {
        // interlaced video: every field its own
        let keys = vec![Key::Unique; 40];
        let (pics, stats, most) = run(cfg(true, true), &keys);
        check(cfg(true, true), &keys, &pics, most);
        assert_eq!(stats.pictures, [0, 0, 20, 0]);
        assert!(pics.iter().all(|p| !p.progressive && p.top_first));
        assert_eq!(stats.idle, 0, "video never waits");
        // 29.97 Hz progressive: two fields each, flagged progressive
        let keys = sampled(40, 1, 2);
        let (pics, stats, most) = run(cfg(true, true), &keys);
        check(cfg(true, true), &keys, &pics, most);
        assert_eq!(stats.pictures, [0, 0, 20, 0]);
        assert!(pics.iter().all(|p| p.progressive));
    }

    #[test]
    fn a_cut_in_the_cadence_weaves_until_the_fields_line_up() {
        // A A A B B C C C | cut after one field of D | E E E F F G G G
        let ks = [
            0, 0, 0, 1, 1, 2, 2, 2, 3, 10, 10, 10, 11, 11, 12, 12, 12, 13, 13,
        ];
        let keys: Vec<Key> = ks.iter().map(|k| inst(*k)).collect();
        let (pics, stats, most) = run(cfg(true, true), &keys);
        check(cfg(true, true), &keys, &pics, most);
        let shape: Vec<(u8, bool, Option<Break>)> = pics
            .iter()
            .map(|p| (p.units, p.progressive, p.brk))
            .collect();
        assert_eq!(
            shape,
            [
                (3, true, None),
                (2, true, None),
                (3, true, None),
                // the lone field of D with the first of E: one woven picture
                (2, false, Some(Break::Woven)),
                // E has two fields left
                (2, true, None),
                (2, true, None),
                (3, true, None),
                (2, true, None),
            ]
        );
        assert_eq!(stats.breaks[Break::Woven as usize], 1);
    }

    #[test]
    fn stills_and_the_slate_are_shown_three_units_at_a_time() {
        let slate = Key::Instant { src: 9, idx: 0 };
        let mut keys = sampled(20, 2, 5);
        keys.extend([slate; 30]);
        for il in [true, false] {
            let (pics, stats, most) = run(cfg(il, true), &keys);
            check(cfg(il, true), &keys, &pics, most);
            let still: Vec<&Picture<usize>> =
                pics.iter().filter(|p| p.first().key == slate).collect();
            assert_eq!(still.len(), 10);
            assert!(still.iter().all(|p| p.units == 3));
            assert!(!still[0].again && still[1..].iter().all(|p| p.again));
            assert_eq!(stats.again, 9);
            assert_eq!(stats.breaks[Break::Jump as usize], 1, "to the slate");
        }
    }

    #[test]
    fn breaks_are_named() {
        let c = cfg(true, true);
        let of = |ks: &[i64]| {
            let keys: Vec<Key> = ks.iter().map(|k| inst(*k)).collect();
            let (pics, _, most) = run(c, &keys);
            check(c, &keys, &pics, most);
            pics.iter().filter_map(|p| p.brk).collect::<Vec<_>>()
        };
        // the clock repeats a frame: three fields, then two more of the same
        assert_eq!(of(&[0, 0, 0, 1, 1, 2, 2, 2, 2, 2, 3, 3]), [Break::Still]);
        // the clock drops a frame
        assert_eq!(of(&[0, 0, 0, 1, 1, 2, 2, 2, 4, 4, 5, 5, 5]), [Break::Jump]);
        // the phase moves: two after two
        assert_eq!(
            of(&[0, 0, 0, 1, 1, 2, 2, 2, 3, 3, 4, 4, 5, 5, 5]),
            [Break::Shift]
        );
        // video after film
        let mut keys = sampled(10, 2, 5);
        keys.extend([Key::Unique; 6]);
        let (pics, stats, most) = run(c, &keys);
        check(c, &keys, &pics, most);
        assert_eq!(stats.breaks[Break::Video as usize], 1);
    }

    #[test]
    fn an_odd_end_is_padded_by_one_field() {
        let keys = sampled(8, 2, 5); // A A A B B C C C: 8 fields, C's run of three ends the input
        let (pics, stats, _) = run(cfg(true, true), &keys);
        assert_eq!(stats.units_out, 8);
        assert!(pics.iter().all(|p| !p.padded));
        let keys = sampled(5, 2, 5); // A A A B B
        let mut odd = keys.clone();
        odd.push(inst(7)); // one field of another frame at the very end
        let (pics, stats, _) = run(cfg(true, true), &odd[..]);
        assert_eq!((stats.units_in, stats.units_out), (6, 7));
        assert_eq!(pics.len(), 3);
        assert!(pics[2].padded && pics[2].progressive);
        // three fields, then a lone field at the end: shown twice
        let a = [inst(0), inst(0), inst(0), inst(1)];
        let (pics, stats, _) = run(cfg(true, true), &a);
        assert_eq!((stats.units_in, stats.units_out), (4, 5));
        assert!(pics[1].padded && pics[1].units == 2 && !pics[1].top_first);
    }

    #[test]
    fn switching_film_mode_off_keeps_the_parity() {
        let c = cfg(true, true);
        let mut a = Assembler::new(c);
        let mut pics = Vec::new();
        let keys = sampled(60, 2, 5);
        for (i, pair) in keys.chunks(2).enumerate() {
            if i == 4 {
                a.set_three(false);
            }
            for k in pair {
                a.push(Unit { key: *k, data: () });
            }
            pics.extend(a.pull());
        }
        while let Some(p) = a.flush() {
            pics.push(p);
        }
        let mut top = true;
        for p in &pics {
            assert_eq!(p.top_first, top);
            if p.units == 3 {
                top = !top;
            }
        }
        assert!(pics[5..].iter().all(|p| p.units == 2));
        assert!(pics.iter().any(|p| p.units == 3));
        let total: u64 = pics.iter().map(|p| u64::from(p.units)).sum();
        assert!(total == 60 || total == 61);
    }

    /// Anything a timeline can produce: film at several rates, video, stills, cuts, sources
    /// that drop and repeat, single fields of anything.
    #[test]
    fn random_selections_are_covered_exactly_once() {
        for seed in 0..400u64 {
            let mut r = Rng(seed);
            let c = FilmCfg {
                interlaced: seed % 2 == 0,
                top_first: seed % 4 < 2,
                three: seed % 8 != 7,
            };
            let mut keys = Vec::new();
            let mut idx = 0i64;
            while keys.len() < 400 {
                let len = 1 + r.below(40) as usize;
                match r.below(7) {
                    // film: 2/5 of a frame per unit, from a random phase
                    0 | 1 => {
                        let ph = r.below(5);
                        for u in 0..len as u64 {
                            keys.push(inst(idx + ((u * 2 + ph) / 5) as i64));
                        }
                        idx += len as i64;
                    }
                    // a source a little off 24 Hz
                    2 => {
                        for u in 0..len as u64 {
                            keys.push(inst(idx + (u * 401 / 1000) as i64));
                        }
                        idx += len as i64;
                    }
                    3 => keys.extend(std::iter::repeat_n(Key::Unique, len)),
                    4 => keys.extend(std::iter::repeat_n(Key::Instant { src: 9, idx: 0 }, len)),
                    // every unit another frame, with drops and repeats
                    5 => {
                        for _ in 0..len {
                            idx += [0, 1, 1, 1, 2, 5][r.below(6) as usize];
                            keys.push(inst(idx));
                        }
                    }
                    // half rate
                    _ => {
                        for u in 0..len {
                            keys.push(inst(idx + (u / 2) as i64));
                        }
                        idx += len as i64;
                    }
                }
                idx += 3;
            }
            if c.interlaced && keys.len() % 2 == 1 {
                keys.pop();
            }
            let (pics, stats, most) = run(c, &keys);
            check(c, &keys, &pics, most);
            assert_eq!(stats.units_in as usize, keys.len(), "seed {seed}");
            assert!(stats.units_out - stats.units_in <= 1, "seed {seed}");
            let n: u64 = stats.pictures.iter().sum();
            assert_eq!(n as usize, pics.len());
        }
    }
}
