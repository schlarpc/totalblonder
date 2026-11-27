//! GOP structure: typing frames in display order and queueing them in coded order
//! (`Session::enqueue`, `Session::flush`, `Session::emit`).
//!
//! Frames arrive in display order. A key frame (I, starting a closed GOP) is placed at
//! display index 0, on the grid every `gop` frame periods (shifted by `gop_phase` for
//! staggered sessions), when one is requested ([`GopState::want_key`]), and once after a key
//! frame was replaced by a repeat or after a hold ([`GopState::rekey`]). Such a make-up key
//! frame is `off_grid`: the next key frame stays where the grid has it. Between key frames
//! every `m`-th frame is a P picture, the frames before it B pictures, which wait in
//! [`GopState::pending`] until their backward anchor has been queued; the frame before a grid
//! key frame is always a P picture, and a key frame turns the last waiting B picture into a P
//! picture.
//!
//! # The grid is in display time
//!
//! A frame is shown for [`Frame::dur`] *display units*: fields of an interlaced sequence (two,
//! or three with repeat_first_field), frame periods of a progressive one (one; two or three
//! with repeat_first_field). [`GopState::start`] counts the units accepted so far, and the key
//! frame grid is a set of display times, `gop` frame periods apart: **the key frame is the
//! first frame that starts at or after a grid time.** While every frame is shown for one frame
//! period that is every `gop`-th frame, as before. With frames of three fields a key frame
//! starts at most one field after its grid time (two frame periods at most in a progressive
//! sequence that repeats frames), and the grid itself does not move: key frames stay `gop`
//! frame periods apart on average, whatever the mix of durations, which is the promise to the
//! mux and the statmux controller (staggered sessions stay staggered). The anchor distance `m`
//! counts pictures, and temporal_reference counts pictures (H.262 6.3.9).

use std::collections::VecDeque;

use tb_m2v_syntax::PicType;

/// One input frame on its way through the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Display index.
    pub disp: u64,
    /// Ring slot that holds (or will hold) the frame on the device.
    pub slot: u8,
    /// Presentation time, 90 kHz.
    pub pts: i64,
    /// First tick (batch step count) at which the frame is on the device.
    pub avail_step: u64,
    /// Picture type, assigned when the frame leaves [`GopState::enqueue`].
    pub pic_type: PicType,
    /// The frame starts a closed GOP (sequence header, GOP header, I picture).
    pub gop_start: bool,
    /// progressive_frame = 1 in an interlaced sequence.
    pub progressive: bool,
    /// Key frame that makes up for a missed one; the cadence stays on its grid.
    pub off_grid: bool,
    /// Display time of the frame's first field (or frame) in display units since the start of
    /// the session, assigned by [`GopState::enqueue`].
    pub start: u64,
    /// Display units the frame is shown for: 2 or 3 fields in an interlaced sequence, 1, 2 or
    /// 3 frame periods in a progressive one.
    pub dur: u8,
    /// Interlaced sequence: the first field shown is the top field.
    pub tff: bool,
    /// `cc_data` triplets for this frame (3 bytes each, at most 31).
    pub cc: Vec<u8>,
    /// The Active Format Description written in this frame's picture user data, if any
    /// ([`tb_m2v_syntax::afd`]).
    pub afd: Option<tb_m2v_syntax::afd::ActiveFormat>,
}

/// The GOP state of one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GopState {
    /// Frames accepted so far (the next display index).
    pub n_in: u64,
    /// Display index of the first frame of the current GOP.
    pub gop_first: u64,
    /// Display units accepted so far: the display time at which the next frame starts.
    pub start: u64,
    /// Display time of the next key frame on the grid, in display units: the first frame that
    /// starts at or after it is a key frame.
    pub next_key: u64,
    /// A key frame was requested (`force_keyframe`).
    pub want_key: bool,
    /// A key frame was replaced by a repeat: the next frame is a key frame off the grid.
    pub rekey: bool,
    /// B frames waiting for their backward anchor, display order.
    pub pending: VecDeque<Frame>,
    /// Typed frames in coded order, ready for the tick.
    pub coded: VecDeque<Frame>,
    /// `flush` was called.
    pub flushed: bool,
}

impl GopState {
    /// Accept the next frame in display order (`frame.disp` and `frame.pic_type` are assigned
    /// here) and move every frame whose type is now decided to [`GopState::coded`].
    /// `gop`, `m`, `gop_phase` are the session's; `released_from_hold` is true for the first
    /// frame after a hold (it becomes a key frame). `per_frame`: display units per frame period
    /// (2 for an interlaced sequence, 1 for a progressive one); `frame.dur` is the frame's
    /// duration in those units (0 is taken as one frame period).
    ///
    /// `gop` and `m` must be at least 1 (the C++ divides by them unchecked); 0 is treated as 1.
    pub fn enqueue(
        &mut self,
        mut frame: Frame,
        gop: u32,
        m: u32,
        gop_phase: u32,
        released_from_hold: bool,
        per_frame: u8,
    ) {
        let per_frame = u64::from(per_frame.max(1));
        // the grid distance in display units
        let gop = u64::from(gop.max(1)) * per_frame;
        let m = u64::from(m.max(1));
        let gop_phase = u64::from(gop_phase) * per_frame;
        if frame.dur == 0 {
            frame.dur = per_frame as u8;
        }

        frame.disp = self.n_in;
        self.n_in += 1;
        frame.start = self.start;
        self.start += u64::from(frame.dur);
        if frame.disp == 0 {
            self.next_key = frame.start;
        }
        if released_from_hold {
            // a session released from hold starts again with a key frame
            self.rekey = true;
        }
        let on_grid = frame.start >= self.next_key;
        let key = on_grid || self.want_key || self.rekey;
        if key {
            // close the previous GOP: the last waiting B picture becomes a P picture
            self.close_pending();
            // A key frame that makes up for a missed one (rekey) does not move the cadence: the
            // next key frame stays where the grid has it.
            let keep_grid = self.rekey && !self.want_key && !on_grid;
            frame.off_grid = keep_grid;
            let forced = self.want_key;
            self.want_key = false;
            self.rekey = false;
            self.gop_first = frame.disp;
            if !keep_grid {
                self.next_key = if frame.disp == 0 && gop_phase > 0 {
                    frame.start + gop - gop_phase % gop
                } else if forced || frame.disp == 0 {
                    // a forced key frame restarts the cadence at its own display time
                    frame.start + gop
                } else {
                    // The grid does not follow the picture: a key frame that starts a field
                    // after its grid time (the frame before it was shown for three) leaves
                    // the next grid time where it was.
                    let late = (frame.start - self.next_key) / gop;
                    self.next_key + (late + 1) * gop
                };
            }
            self.emit(frame, PicType::I, true);
            return;
        }
        let gpos = frame.disp - self.gop_first;
        // the frame after this one starts at or after the grid time: it will be a key frame
        if gpos.is_multiple_of(m) || frame.start + u64::from(frame.dur) >= self.next_key {
            self.emit(frame, PicType::P, false);
            self.emit_pending_b();
        } else {
            self.pending.push_back(frame);
        }
    }

    /// No more input: waiting B pictures are closed with a P picture.
    pub fn flush(&mut self) {
        if self.flushed {
            return;
        }
        self.flushed = true;
        self.close_pending();
    }

    /// Nothing waiting and nothing to code.
    pub fn idle(&self) -> bool {
        self.pending.is_empty() && self.coded.is_empty()
    }

    /// The last waiting frame becomes a P picture, the ones before it B pictures.
    fn close_pending(&mut self) {
        if let Some(last) = self.pending.pop_back() {
            self.emit(last, PicType::P, false);
            self.emit_pending_b();
        }
    }

    /// Every waiting frame leaves as a B picture, in display order.
    fn emit_pending_b(&mut self) {
        while let Some(b) = self.pending.pop_front() {
            self.emit(b, PicType::B, false);
        }
    }

    /// `Session::emit`: type the frame and queue it in coded order.
    fn emit(&mut self, mut frame: Frame, pic_type: PicType, gop_start: bool) {
        frame.pic_type = pic_type;
        frame.gop_start = gop_start;
        self.coded.push_back(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts: i64) -> Frame {
        Frame {
            disp: u64::MAX,
            slot: 0,
            pts,
            avail_step: 0,
            pic_type: PicType::B,
            gop_start: false,
            progressive: false,
            off_grid: false,
            start: 0,
            dur: 0,
            tff: false,
            cc: Vec::new(),
            afd: None,
        }
    }

    /// The frames queued so far as (disp, type, gop_start, off_grid), emptying the queue.
    fn take(g: &mut GopState) -> Vec<(u64, PicType, bool, bool)> {
        g.coded
            .drain(..)
            .map(|f| (f.disp, f.pic_type, f.gop_start, f.off_grid))
            .collect()
    }

    fn feed(g: &mut GopState, n: usize, gop: u32, m: u32, phase: u32) {
        for _ in 0..n {
            g.enqueue(frame(0), gop, m, phase, false, 1);
        }
    }

    use PicType::{B, I, P};

    #[test]
    fn m1_has_no_b_pictures() {
        let mut g = GopState::default();
        feed(&mut g, 7, 3, 1, 0);
        assert_eq!(
            take(&mut g),
            vec![
                (0, I, true, false),
                (1, P, false, false),
                (2, P, false, false),
                (3, I, true, false),
                (4, P, false, false),
                (5, P, false, false),
                (6, I, true, false),
            ]
        );
        assert!(g.pending.is_empty());
    }

    #[test]
    fn gop1_is_all_key_frames() {
        let mut g = GopState::default();
        feed(&mut g, 4, 1, 3, 0);
        let got = take(&mut g);
        assert_eq!(got.len(), 4);
        for (i, f) in got.iter().enumerate() {
            assert_eq!(*f, (i as u64, I, true, false));
        }
        assert_eq!(g.next_key, 4);
    }

    #[test]
    fn coded_order_with_b_pictures_and_flush() {
        let mut g = GopState::default();
        feed(&mut g, 9, 6, 3, 0);
        assert_eq!(
            take(&mut g),
            vec![
                (0, I, true, false),
                (3, P, false, false),
                (1, B, false, false),
                (2, B, false, false),
                // the frame before the grid key frame is a P picture
                (5, P, false, false),
                (4, B, false, false),
                (6, I, true, false),
            ]
        );
        assert_eq!(g.pending.len(), 2);
        g.flush();
        assert_eq!(
            take(&mut g),
            vec![(8, P, false, false), (7, B, false, false)]
        );
        assert!(g.idle() && g.flushed);
        g.flush();
        assert!(g.idle());
    }

    #[test]
    fn staggered_phase_moves_the_first_grid_key_frame() {
        let mut g = GopState::default();
        g.enqueue(frame(0), 6, 3, 3, false, 1);
        assert_eq!(g.next_key, 3);
        feed(&mut g, 4, 6, 3, 3);
        assert_eq!(
            take(&mut g),
            vec![
                (0, I, true, false),
                (2, P, false, false),
                (1, B, false, false),
                (3, I, true, false),
            ]
        );
        assert_eq!(g.pending.len(), 1);
        assert_eq!(g.next_key, 9);
    }

    #[test]
    fn forced_key_frame_on_the_frame_before_a_grid_key_frame() {
        let mut g = GopState::default();
        feed(&mut g, 5, 6, 3, 0);
        take(&mut g);
        assert_eq!(g.pending.len(), 1); // disp 4
        g.want_key = true;
        g.enqueue(frame(0), 6, 3, 0, false, 1);
        assert_eq!(
            take(&mut g),
            vec![(4, P, false, false), (5, I, true, false)]
        );
        // a forced key frame restarts the cadence
        assert_eq!((g.gop_first, g.next_key), (5, 11));
        assert!(!g.want_key);
        feed(&mut g, 1, 6, 3, 0);
        assert!(take(&mut g).is_empty(), "disp 6 is no longer a key frame");
    }

    #[test]
    fn two_missed_key_frames_in_a_row_make_one_attempt() {
        let mut g = GopState::default();
        feed(&mut g, 7, 6, 3, 0);
        let got = take(&mut g);
        assert_eq!(got.last(), Some(&(6, I, true, false)));
        // The tick replaces the key frame at 6 by a repeat; it was on the grid, so it asks for
        // one make-up attempt.
        g.rekey = true;
        g.enqueue(frame(0), 6, 3, 0, false, 1);
        assert_eq!(take(&mut g), vec![(7, I, true, true)]);
        assert_eq!((g.gop_first, g.next_key), (7, 12));
        assert!(!g.rekey);
        // The make-up key frame is replaced too: it is off the grid, so the tick does not set
        // rekey again and prediction continues up to the grid key frame.
        feed(&mut g, 6, 6, 3, 0);
        assert_eq!(
            take(&mut g),
            vec![
                (10, P, false, false),
                (8, B, false, false),
                (9, B, false, false),
                (11, P, false, false),
                (12, I, true, false),
            ]
        );
        assert_eq!(g.next_key, 18);
    }

    #[test]
    fn release_from_hold_is_an_off_grid_key_frame() {
        let mut g = GopState::default();
        feed(&mut g, 3, 6, 3, 0);
        take(&mut g);
        g.enqueue(frame(0), 6, 3, 0, true, 1);
        assert_eq!(
            take(&mut g),
            vec![
                (2, P, false, false),
                (1, B, false, false),
                (3, I, true, true)
            ]
        );
        assert_eq!(g.next_key, 6);
    }

    #[test]
    fn rekey_on_the_grid_position_is_not_off_grid() {
        let mut g = GopState::default();
        feed(&mut g, 6, 6, 3, 0);
        take(&mut g);
        g.rekey = true;
        g.enqueue(frame(0), 6, 3, 0, false, 1);
        assert_eq!(take(&mut g), vec![(6, I, true, false)]);
        assert_eq!(g.next_key, 12);
    }
}
