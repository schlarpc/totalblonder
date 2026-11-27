//! One session of the encoder: its state between ticks, and the pictures made on the host
//! (the C++ `Session`, `Batch::headers`, `Batch::host_repeat`).

use std::collections::VecDeque;

use tb_kernel_abi::Geo;
use tb_m2v_rate::RateControl;
use tb_m2v_rate::control::RUNG_SKIP;
use tb_m2v_syntax::PicType;
use tb_m2v_syntax::headers::{PicInfo, SeqInfo, write_headers_afd, write_skip_slices};

use crate::au::AccessUnit;
use crate::config::SessionConfig;
use crate::golden::{self, AuState, FilmRec, Gold, RcObs};
use crate::gop::{Frame, GopState};

/// Where the frame submitted since the last tick comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The session's host frame region; `alt`: the second region (overlapped upload).
    Host { alt: bool },
    /// A region of the host arena that the caller owns, at this offset (`w * h * 3 / 2` bytes).
    HostAt { off: u64 },
    /// Written on the device by the caller.
    Dev,
}

/// The frame submitted since the last tick (`up_ptr` / `up_dev`, `up_slot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub slot: u8,
    pub src: Source,
}

#[derive(Debug)]
pub(crate) struct Session {
    pub id: u32,
    pub cfg: SessionConfig,
    /// Id of the batch.
    pub batch: u32,
    /// Slot in the batch.
    pub slot: usize,
    pub fps: f64,
    pub seq: SeqInfo,
    // input and GOP structure
    pub gop: GopState,
    pub n_coded: u64,
    pub tref_base: u64,
    pub pts0: i64,
    /// Display units per frame period: 2 (fields) in an interlaced sequence, 1 in a
    /// progressive one.
    pub upf: u8,
    /// The session's place on the caller's time line: display unit of the first frame, such
    /// that `pts0 = floor(unit0 * unit period) + pts_off` (see [`Session::set_origin`]).
    pub unit0: i64,
    /// What `pts0` has beyond the grid of display units from PTS 0 (0 for a caller that counts
    /// its frames from PTS 0).
    pub pts_off: i64,
    /// Interlaced sequence: the next frame's first field must be the top field (H.262 7.12:
    /// output fields alternate).
    pub next_top: bool,
    /// The decoding clock: display units from the first picture's decoding time to the next
    /// picture's (H.262 Annex C.9, C.11).
    pub dec_units: u64,
    /// Display units of the last coded I or P picture (one frame period before the first).
    pub prev_anchor: u8,
    /// Pictures and display units coded since the last key frame: when the next key frame is
    /// planned, the mean display time of a picture of the GOP before it (for the picture
    /// count rate control plans a GOP with).
    pub gop_acc: (u32, u32),
    /// Access units by display duration in units (index 1, 2, 3).
    pub shown: [u64; 4],
    /// Frames whose PTS as submitted was not the display time the session counts.
    pub pts_mismatch: u64,
    /// Reference slot of the most recent anchor.
    pub newer: u8,
    /// Display index of the picture in each reference slot.
    pub ref_disp: [i64; 2],
    pub have_ref: bool,
    pub last_dist_f: i8,
    pub last_dist_b: i8,
    pub up: Option<Pending>,
    /// Frames that occupy a ring slot.
    pub n_real: u64,
    /// Frames submitted from host memory.
    pub n_host: u64,
    pub last_slot: u8,
    pub last_avail: u64,
    /// Ring slot whose upload (or padding) is in the `Begin` plan in flight, main stream.
    pub fly_main: Option<u8>,
    /// Ring slot whose upload on the upload stream has not been waited for yet.
    pub fly_up: Option<u8>,
    /// With the frames event: the ring slot whose upload was queued in the tick before the
    /// last (the plan that waits for its event confirms it).
    pub fly_up_prev: Option<u8>,
    /// Ring slots whose content never reached the device (bit per slot).
    pub bad_slots: u8,
    /// Nothing but a key frame is coded on the device (after the device was lost, or after a
    /// restart): the reference pictures on the device are not the decoder's.
    pub need_key: bool,
    // dynamic sessions and the deadline-miss policy
    pub closing: bool,
    pub closed: bool,
    pub hold: bool,
    pub hold_prev: bool,
    /// The Active Format Description given to the frames submitted from now on.
    pub afd: Option<u8>,
    pub shed_now: bool,
    /// The newest anchor is a repeat of the one before: B pictures in between repeat too.
    pub dup: bool,
    pub repeats: u64,
    pub keys_missed: u64,
    pub shed_count: u64,
    pub failed: u64,
    pub pictures: u64,
    // rate
    pub rc: RateControl,
    pub rate: u64,
    pub pending_rate: u64,
    pub pending_ticket: u32,
    pub next_ticket: u32,
    pub pending_at_gop: bool,
    pub out: VecDeque<AccessUnit>,
    pub total_bits: f64,
}

/// A frame without content, for moving a frame out of its place.
pub(crate) fn blank_frame() -> Frame {
    Frame {
        disp: 0,
        slot: 0,
        pts: 0,
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

/// `PIC_*` to the syntax type (anything else counts as B; the callers pass 1, 2 or 3).
pub(crate) fn pic_type_of(code: u8) -> PicType {
    match code {
        tb_kernel_abi::PIC_I => PicType::I,
        tb_kernel_abi::PIC_P => PicType::P,
        _ => PicType::B,
    }
}

impl Session {
    /// Nothing waiting and nothing to code.
    pub fn idle(&self) -> bool {
        self.gop.idle()
    }

    /// Fix the session's time base from the PTS of its first frame. The caller's PTS are taken
    /// to be `floor(k * unit period)` for display unit k counted from PTS 0 (what a frame
    /// counter gives, and what H.222.0 2.4.3.7 defines a PTS as: system clock times
    /// presentation time, "DIV 300", truncated); `unit0` is the first frame's k and `pts_off`
    /// whatever `pts` has beyond it, kept as a constant offset. With it every PTS and DTS of
    /// the session is exact: no rounding accumulates and PTS = DTS for a B picture.
    pub fn set_origin(&mut self, pts: i64) {
        let (num, den) = self.unit_ratio();
        let unit0 = (i128::from(pts) * den).div_euclid(num)
            + i128::from((i128::from(pts) * den).rem_euclid(num) != 0);
        self.unit0 = i64::try_from(unit0).unwrap_or(0);
        self.pts0 = pts;
        self.pts_off = pts.wrapping_sub(self.stamp(0));
    }

    /// The display unit period in 90 kHz ticks as a fraction: `90000 * fps_d / (fps_n * upf)`.
    fn unit_ratio(&self) -> (i128, i128) {
        (
            90_000 * i128::from(self.cfg.fps_d),
            // fps_n is checked to be positive when the session is opened
            i128::from(self.cfg.fps_n.max(1)) * i128::from(self.upf.max(1)),
        )
    }

    /// The 90 kHz time stamp of display unit `unit0 + units` (signed; not reduced modulo 2^33:
    /// [`AccessUnit::pts33`]). Integer arithmetic on the exact rational; the division
    /// truncates (toward zero for the one negative value, the first DTS of a session that
    /// starts at PTS 0, as before).
    fn stamp(&self, units: i64) -> i64 {
        let (num, den) = self.unit_ratio();
        let t = (i128::from(self.unit0) + i128::from(units)) * num / den;
        i64::try_from(t)
            .unwrap_or(i64::MAX)
            .wrapping_add(self.pts_off)
    }

    /// Presentation time of the frame that starts at display unit `start`.
    pub fn pts_of(&self, start: u64) -> i64 {
        self.stamp(i64::try_from(start).unwrap_or(i64::MAX))
    }

    /// Decoding time of the next coded picture. The first picture is decoded one frame period
    /// before it is presented (the reordering delay); from then on the decoding clock
    /// advances by the intervals of H.262 Annex C.9 and C.11 ([`Session::coded`]).
    pub fn dts(&self) -> i64 {
        self.stamp(i64::try_from(self.dec_units).unwrap_or(i64::MAX) - i64::from(self.upf))
    }

    /// Time of the next coded picture in the buffer model: seconds from the first picture's
    /// decoding time (`n_coded / fps` while every picture is shown for one frame period).
    pub fn t(&self) -> f64 {
        self.dec_units as f64 / f64::from(self.upf.max(1)) / self.fps
    }

    /// A picture of a frame has been coded as an access unit: the coded index and the
    /// decoding clock advance. The clock follows H.262 Annex C.11 (p.141; C.9, p.140, for a
    /// progressive sequence): the next picture is examined after the display time of the
    /// picture that is output at this one's decoding time, which is the picture itself if it
    /// is a B picture, and otherwise the I or P picture coded before it ("if the previous
    /// coded P-Frame or coded I-Frame has repeat_first_field equal to 1, then t(n+1) - t(n)
    /// is equal to 3*T"). `dur`, `gop_start`: of the frame; `b`: the picture was coded as a B
    /// picture.
    pub fn coded(&mut self, dur: u8, gop_start: bool, b: bool) {
        let dur = dur.max(1);
        if b {
            self.dec_units += u64::from(dur);
        } else {
            self.dec_units += u64::from(self.prev_anchor);
            self.prev_anchor = dur;
        }
        self.n_coded += 1;
        self.shown[usize::from(dur.min(3))] += 1;
        if gop_start {
            self.gop_acc = (0, 0);
        }
        self.gop_acc.0 = self.gop_acc.0.saturating_add(1);
        self.gop_acc.1 = self.gop_acc.1.saturating_add(u32::from(dur));
    }

    /// Pictures from frame `f` to the next key frame on the grid, for the GOP budget: exact
    /// while every picture is shown for one frame period; otherwise the display time left
    /// over the mean display time of the pictures coded since the last key frame (of this
    /// picture when there is none).
    pub fn to_next_key(&self, f: &Frame) -> u32 {
        let left = self.gop.next_key.saturating_sub(f.start);
        let (pics, units) = match self.gop_acc {
            (p, u) if p > 0 && u > 0 => (p, u),
            _ => (1, u32::from(f.dur.max(1))),
        };
        u32::try_from((left * u64::from(pics)).div_ceil(u64::from(units))).unwrap_or(u32::MAX)
    }

    /// What the golden records add for frame `f`: `None` for a frame shown for one frame period
    /// in the session's field order at a display time equal to its index.
    pub fn film(&self, f: &Frame) -> Option<FilmRec> {
        let time = f.start / u64::from(self.upf.max(1));
        let top = self.cfg.interlaced && self.cfg.tff;
        if f.dur == self.upf && f.tff == top && time == f.disp {
            return None;
        }
        let (rff, tff) = self.rff_tff(f);
        Some(FilmRec {
            time,
            tff,
            rff,
            units: f.dur,
            top_first: f.tff,
        })
    }

    /// repeat_first_field and top_field_first of frame `f` (H.262 6.3.10, p.49-50).
    /// Interlaced sequence: three fields, and the parity of the first. Progressive sequence:
    /// the frame is output twice (1, 0) or three times (1, 1).
    pub fn rff_tff(&self, f: &Frame) -> (bool, bool) {
        if self.seq.interlaced {
            (f.dur == 3, f.tff)
        } else {
            (f.dur >= 2, f.dur == 3)
        }
    }

    /// The picture of frame `f` is coded without field tools although the batch has them: both
    /// its fields are one instant (progressive_frame = 1 in an interlaced sequence), and then
    /// frame_pred_frame_dct shall be 1 (A/53 Part 4 6.1.6, p.11; SCTE 43 7.1.6, p.4): frame
    /// prediction and frame DCT only, no frame_motion_type or dct_type in the macroblocks.
    pub fn frame_only(&self, geo: &Geo, f: &Frame) -> bool {
        geo.interlaced != 0 && f.progressive
    }

    /// Such a picture is scanned in the zigzag order when the session left the scan to the
    /// format (the alternate scan is the choice for interlaced content, which it is not).
    pub fn zigzag(&self, geo: &Geo, f: &Frame) -> bool {
        self.frame_only(geo, f) && self.cfg.alt_scan.is_none()
    }

    /// Sequence, GOP and picture headers of one access unit into `out`, which is cleared first
    /// (the C++ `Batch::headers`).
    pub fn headers(
        &self,
        geo: &Geo,
        f: &Frame,
        pic_type: PicType,
        gop_hdr: bool,
        seq_hdr: bool,
        out: &mut Vec<u8>,
    ) {
        out.clear();
        let (rff, tff) = self.rff_tff(f);
        let pic = PicInfo {
            pic_type,
            disp: f.disp,
            // the time code counts frame periods of display time (H.262 6.3.8)
            time: f.start / u64::from(self.upf.max(1)),
            tref_base: self.tref_base,
            f_code: u8::try_from(geo.f_code).unwrap_or(15),
            tff,
            rff,
            frame_pred_frame_dct: geo.interlaced == 0 || self.frame_only(geo, f),
            intra_vlc_format: geo.intra_vlc_format != 0,
            alt_scan: geo.alt_scan != 0 && !self.zigzag(geo, f),
            progressive_frame: f.progressive,
        };
        write_headers_afd(out, &self.seq, &pic, seq_hdr, gop_hdr, &f.cc, f.afd);
    }

    /// A repeated picture for frame `f`, made here and now: no kernel runs for it. An I
    /// position becomes a P picture without GOP header (flagged `key_missed`) and asks for a key
    /// frame at the next submitted frame, unless it was itself a make-up key frame. The buffer
    /// model takes the picture like any other (the C++ `Batch::host_repeat`).
    ///
    /// `ticket`: a rate change that took effect with this picture (only when a device failure
    /// turns a picture that was already begun into a repeat). `scratch` is working memory.
    pub fn host_repeat(
        &mut self,
        geo: &Geo,
        f: &Frame,
        ticket: u32,
        scratch: &mut Vec<u8>,
        g: &mut Gold,
    ) {
        let pic_type = if f.pic_type == PicType::B {
            PicType::B
        } else {
            PicType::P
        };
        let missed = f.pic_type == PicType::I;
        // the buffer advances first, as in the C++ (the records come in that order)
        let t = self.t();
        self.rc.repeat_advance(t, &mut RcObs { g, s: self.id });
        self.headers(geo, f, pic_type, false, false, scratch);
        golden::hdr(
            g,
            self.id,
            f,
            pic_type.code(),
            false,
            false,
            self.tref_base,
            self.rc.cbr,
            scratch,
            self.film(f),
        );
        write_skip_slices(
            scratch,
            geo.mbw.unsigned_abs(),
            geo.mbh.unsigned_abs(),
            pic_type,
            geo.interlaced == 0 || self.frame_only(geo, f),
        );
        let bits = 8.0 * scratch.len() as f64;
        let res = self
            .rc
            .repeat_add(bits, f.pic_type.code(), &mut RcObs { g, s: self.id });
        let au = AccessUnit {
            // the one allocation of the picture
            data: scratch.as_slice().to_vec(),
            pts: f.pts,
            dts: self.dts(),
            disp: f.disp,
            coded: self.n_coded,
            pic_type: pic_type.code(),
            keyframe: false,
            avg_q: 0.0,
            complexity: 0.0,
            bits: bits as u64,
            rate: self.rate,
            rate_ticket: ticket,
            queue_bits: res.queue_bits,
            room_bits: res.room_bits,
            cap_bits: res.cap_bits,
            rung: RUNG_SKIP,
            tries: 0,
            intra_mbs: 0,
            field_mbs: 0,
            repeat: true,
            key_missed: missed,
            off_grid: false,
            last: false,
            units: f.dur,
            top_first: f.tff,
            recon: Vec::new(),
        };
        if pic_type == PicType::P {
            // the decoder's new anchor equals the old one
            self.ref_disp[usize::from(self.newer & 1)] = i64::try_from(f.disp).unwrap_or(i64::MAX);
            self.dup = true;
        }
        // One attempt to make a missed key frame up, at the next frame submitted; if that one is
        // given up as well the session waits for the next key frame on the grid.
        if missed {
            if !f.off_grid {
                self.gop.rekey = true;
            }
            self.keys_missed += 1;
        }
        self.repeats += 1;
        self.total_bits += bits;
        if g.on() {
            let st = self.au_state(self.film(f));
            golden::au(g, &st, &au, f.pic_type.code(), (0, 0), None);
        }
        self.coded(f.dur, f.gop_start, pic_type == PicType::B);
        self.out.push_back(au);
    }
}

impl Session {
    /// The session's state for an `au` record.
    pub fn au_state(&self, film: Option<FilmRec>) -> AuState<'_> {
        AuState {
            film,
            t: self.t(),
            s: self.id,
            rc: &self.rc,
            newer: self.newer,
            ref_disp: self.ref_disp,
            dup: self.dup,
            have_ref: self.have_ref,
            rekey: self.gop.rekey,
            tref_base: self.tref_base,
        }
    }
}

/// Index of the session with this id (the sessions are kept in ascending order of id).
pub(crate) fn find(sessions: &[Session], id: u32) -> Option<usize> {
    sessions.binary_search_by_key(&id, |s| s.id).ok()
}
