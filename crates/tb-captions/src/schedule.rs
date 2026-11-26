//! The [`Captioner`]: cues in, the `cc_data()` of every picture out.
//!
//! **Time.** The output is a sequence of pictures, each shown for one to three field periods
//! of 1001/60000 s (two for a 29.97 Hz frame, one for a 59.94 Hz frame, two or three for a
//! film picture). Field period `p` starts at time `p x 1001/60` ms after the first picture;
//! cue times are on that clock ([`crate::cue`]). Each field period carries 10 constructs: one
//! CEA-608 pair and nine DTVCC byte pairs (CTA-708-E §4.3.6 and Table 4: 20 per 29.97 Hz
//! frame, 10 per 59.94 Hz frame, 20 or 30 per film picture), which is the 9600 bit/s of the
//! caption channel: 960 for CEA-608, 8640 for DTVCC (§4.1).
//!
//! **CEA-608 slots.** Each field gets one pair per frame, so a 608 channel is a sequence of
//! slots: slot `k` of field `f` is the `k`-th field period of that field's parity, at about
//! frame `k` (field period `2k` or `2k + 1`). A cue at field period `p` is placed at slot
//! `p / 2`. The CEA-608 timing is thus within a field period of the cue's time, which is
//! below what CTA-608-E B.11.7 (p.69) calls unobjectionable (1 to 4 frames).
//!
//! **DTVCC slots.** Field period `p` has DTVCC slots `9p` to `9p + 8`. A caption channel packet
//! takes consecutive slots (no padding inside it: some decoders end a packet at padding, the
//! rule of `tb_timeline::captions`) and runs over picture boundaries when it needs to. A
//! packet acts when its last byte arrives, so a command meant for field period `p` is placed
//! in a packet that starts at slot `9p`.
//!
//! **Pop-on (CEA-608).** Each screen ([`crate::layout`]) is a *load*, `RCL ENM PAC text ...`
//! (B.8.3, p.65), sent in the frames before it is due, and an `EOC` in the slot of its start
//! ("the first EOC command ... occurs on the frame the captioner desires the caption to
//! display", B.11.2, p.68); an `EDM` in the slot of its end clears it unless the next screen
//! starts within a frame, in which case that screen's `EOC` replaces it at once (B.8.3, p.66).
//! The load of a screen goes after the `EOC` of the screen before (it writes the memory that
//! `EOC` just hid) and at most [`CaptionerCfg::max_lead`] before its own `EOC` ("no caption
//! should be pre-sent more than 15 seconds before its appear time", B.8.3, p.66). Control
//! codes go twice in consecutive slots of their field (D.2) or once ([`Redundancy`]); fixed
//! codes are placed first, the load fills the free slots around them, never splitting a
//! doubled code.
//!
//! **Pop-on (CEA-708).** Two sets of windows, used by alternate screens (§8.11.3, p.85 uses two
//! windows): the load deletes this set's windows, defines them hidden (one window per block of
//! consecutive rows, 32 columns, anchored at the bottom center of the row block's last row on
//! the 75-row grid, left justified, transparent fill: predefined window style 2, which "looks
//! quite like CEA-608 captions", p.67), sets the pen and writes the rows with SetPenLocation;
//! at the start time one packet deletes the other set's windows and displays this set
//! (`DLW`, `DSW`), at the end one packet deletes this set unless the next screen replaces it.
//! This differs from §8.11.3's ClearWindows + ToggleWindows on purpose: Toggle's result
//! depends on what the receiver already knows, and a receiver that tuned in between a load
//! and its display would show the wrong set; explicit Delete and Display make each screen
//! stand on its own, which is what §8.11 asks ("each caption should contain all commands
//! necessary to display captions properly, without depending on any previous caption data").
//!
//! **Roll-up.** For live text: each row of a cue is `RU2/3/4 CR PAC text` in CEA-608 (B.8.1,
//! p.65), and `DefineWindow` (visible, unchanged after the first row so receivers ignore it),
//! window and pen attributes, `SetPenLocation` to the bottom row, `CR`, the text in CEA-708
//! (§8.11.1, p.84). Rows go as soon as the channel allows from the cue's start; the screen is
//! cleared at the cue's end if no cue follows within a frame.
//!
//! **Late cues.** When a load cannot be sent between the screen before and its start, or a
//! roll-up row starts more than [`CaptionerCfg::roll_up_tolerance`] after its cue, the
//! [`Late`] report says so, and [`LatePolicy`] says what happens: the screen is shown when its
//! load is complete (if that is before its end), or skipped. Nothing is shifted without a
//! report.

use std::collections::BTreeMap;

use crate::ccdata;
use crate::cea608::{self, Channel, Code, Misc, Pair, Redundancy, Units};
use crate::cea708::packet::{Packer, Packet};
use crate::cea708::{
    Anchor, Char708, Command, PenAttrs, PenColor, ServiceNumber, WindowAttrs, WindowDef, WindowId,
    WindowMap,
};
use crate::cue::{Cue, FieldRate, Millis, SpanStyle};
use crate::layout::{self, LaidRow, LayoutCfg, LayoutReport, Screen};

/// How captions are presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presentation {
    /// Pop-on: each screen appears whole at its start and goes at its end (subtitles).
    PopOn,
    /// Roll-up with 2, 3 or 4 rows: rows appear as they are sent and scroll up (live text).
    RollUp(RollUpRows),
}

/// Rows of a roll-up caption (RU2, RU3, RU4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RollUpRows {
    /// Two rows.
    Two = 2,
    /// Three rows.
    Three = 3,
    /// Four rows.
    Four = 4,
}

/// The display a CEA-708 service is authored for: the width of the window grid (§8.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aspect {
    /// 16:9, 210 grid columns.
    Wide,
    /// 4:3, 160 grid columns.
    Standard,
}

impl Aspect {
    /// The grid column of the center of the safe title area.
    fn center(self) -> u8 {
        match self {
            Self::Wide => 104,
            Self::Standard => 79,
        }
    }
}

/// A track's CEA-608 channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cea608Track {
    /// The channel (one track per field).
    pub channel: Channel,
    /// Pop-on or roll-up.
    pub presentation: Presentation,
    /// Control codes once or twice.
    pub redundancy: Redundancy,
}

/// A track's CEA-708 service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cea708Track {
    /// The service number.
    pub service: ServiceNumber,
    /// Pop-on or roll-up.
    pub presentation: Presentation,
    /// The display the windows are placed for.
    pub aspect: Aspect,
}

/// One caption track (a language): the same text in a CEA-608 channel and a CEA-708 service.
/// CTA-708-E's compatibility pairs are service 1 with CC1 and service 2 with CC3
/// ([`Track::primary`], [`Track::secondary`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Track {
    /// The CEA-608 channel, if any.
    pub cea608: Option<Cea608Track>,
    /// The CEA-708 service, if any.
    pub cea708: Option<Cea708Track>,
    /// Rows and placement.
    pub layout: LayoutCfg,
}

impl Track {
    /// CC1 and service 1, `presentation`, for a 16:9 picture.
    pub fn primary(presentation: Presentation) -> Self {
        Self {
            cea608: Some(Cea608Track {
                channel: Channel::Cc1,
                presentation,
                redundancy: Redundancy::Doubled,
            }),
            cea708: Some(Cea708Track {
                service: ServiceNumber::PRIMARY,
                presentation,
                aspect: Aspect::Wide,
            }),
            layout: LayoutCfg::default(),
        }
    }

    /// CC3 and service 2 (a second language), `presentation`, for a 16:9 picture. CC3's
    /// control codes go once (CTA-608-E D.2, B.14).
    pub fn secondary(presentation: Presentation) -> Self {
        Self {
            cea608: Some(Cea608Track {
                channel: Channel::Cc3,
                presentation,
                redundancy: Redundancy::Single,
            }),
            cea708: Some(Cea708Track {
                service: ServiceNumber::SECONDARY,
                presentation,
                aspect: Aspect::Wide,
            }),
            layout: LayoutCfg::default(),
        }
    }
}

/// What happens to a screen whose load cannot be sent before its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatePolicy {
    /// Shown as soon as its load is complete, if that is before its end; else skipped.
    Delay,
    /// Skipped.
    Skip,
}

/// Configuration of a [`Captioner`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptionerCfg {
    /// The field rate of the output.
    pub rate: FieldRate,
    /// The tracks.
    pub tracks: Vec<Track>,
    /// Most time a pop-on load is sent before its screen (15 s: CTA-608-E B.8.3).
    pub max_lead: Millis,
    /// What happens to late screens.
    pub late: LatePolicy,
    /// A roll-up row that starts later than this after its cue is reported.
    pub roll_up_tolerance: Millis,
}

impl CaptionerCfg {
    /// One primary pop-on track (CC1 and service 1) at 29.97 / 59.94 Hz.
    pub fn new(tracks: Vec<Track>) -> Self {
        Self {
            rate: FieldRate::NTSC,
            tracks,
            max_lead: Millis(15_000),
            late: LatePolicy::Delay,
            roll_up_tolerance: Millis(1000),
        }
    }
}

/// Why a configuration is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Two tracks use the same field (CC1 and CC2, or CC3 and CC4): a field has one pair per
    /// frame, and this crate gives it to one track.
    #[error("tracks {0} and {1} both use CEA-608 field {2}")]
    SharedField(usize, usize, u8),
    /// Two tracks use the same CEA-708 service.
    #[error("tracks {0} and {1} both use CEA-708 service {2}")]
    SharedService(usize, usize, u8),
    /// A roll-up base row above the number of rows (CTA-608-E B.5: "Never send a PAC with a
    /// base row less than the number of roll-up rows").
    #[error("track {0}: roll-up of {1} rows on base row {2}")]
    BaseRow(usize, u8, u8),
    /// More than four rows on the screen.
    #[error("track {0}: max_rows must be 1 to 4")]
    MaxRows(usize),
}

/// A service of a track, in reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceId {
    /// A CEA-608 channel.
    Cea608(Channel),
    /// A CEA-708 service.
    Cea708(ServiceNumber),
}

/// A screen or row that could not be sent on time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Late {
    /// Track index.
    pub track: usize,
    /// Which service.
    pub service: ServiceId,
    /// When it was due.
    pub start: Millis,
    /// When it was to end.
    pub end: Millis,
    /// When it is shown instead; `None`: skipped.
    pub shown: Option<Millis>,
    /// Its text.
    pub text: String,
}

/// What [`Captioner::add_cues`] did besides scheduling.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// What the layout changed.
    pub layout: LayoutReport,
    /// What is late or skipped.
    pub late: Vec<Late>,
}

#[derive(Clone, Debug)]
struct Prev608 {
    /// Slot after the last unit placed (load or EOC) that a new load must follow.
    after: u64,
    /// The reserved clearing EDM, cancelable.
    edm: Option<u64>,
}

#[derive(Clone, Debug)]
struct Plan608 {
    track: usize,
    cfg: Cea608Track,
    layout: LayoutCfg,
    occ: BTreeMap<u64, Pair>,
    prev: Option<Prev608>,
}

#[derive(Clone, Debug)]
struct Prev708 {
    after: u64,
    clear: Option<u64>,
}

#[derive(Clone, Debug)]
struct Plan708 {
    track: usize,
    cfg: Cea708Track,
    layout: LayoutCfg,
    parity: u8,
    prev: Option<Prev708>,
    /// The roll-up window has been defined (its definition is repeated unchanged).
    rolled: bool,
}

/// Turns cues into the `cc_data()` of each picture (see the module documentation).
#[derive(Clone, Debug)]
pub struct Captioner {
    cfg: CaptionerCfg,
    f608: [Option<Plan608>; 2],
    s708: Vec<Plan708>,
    /// DTVCC packets by first slot.
    occ708: BTreeMap<u64, Packet>,
    fslot: [u64; 2],
    dslot: u64,
    fields: u64,
    seq: u8,
}

impl Captioner {
    /// A captioner with nothing scheduled, at time 0.
    pub fn new(cfg: CaptionerCfg) -> Result<Self, ConfigError> {
        let mut f608: [Option<Plan608>; 2] = [None, None];
        let mut s708: Vec<Plan708> = Vec::new();
        for (i, t) in cfg.tracks.iter().enumerate() {
            if !(1..=4).contains(&t.layout.max_rows) {
                return Err(ConfigError::MaxRows(i));
            }
            if let Some(c) = t.cea608 {
                let f = c.channel.field().index();
                if let Some(p) = &f608[f] {
                    return Err(ConfigError::SharedField(p.track, i, f as u8 + 1));
                }
                if let Presentation::RollUp(n) = c.presentation
                    && t.layout.bottom_row.get() < n as u8
                {
                    return Err(ConfigError::BaseRow(i, n as u8, t.layout.bottom_row.get()));
                }
                f608[f] = Some(Plan608 {
                    track: i,
                    cfg: c,
                    layout: t.layout,
                    occ: BTreeMap::new(),
                    prev: None,
                });
            }
            if let Some(c) = t.cea708 {
                if let Some(p) = s708.iter().find(|p| p.cfg.service == c.service) {
                    return Err(ConfigError::SharedService(p.track, i, c.service.get()));
                }
                s708.push(Plan708 {
                    track: i,
                    cfg: c,
                    layout: t.layout,
                    parity: 0,
                    prev: None,
                    rolled: false,
                });
            }
        }
        Ok(Self {
            cfg,
            f608,
            s708,
            occ708: BTreeMap::new(),
            fslot: [0, 0],
            dslot: 0,
            fields: 0,
            seq: 0,
        })
    }

    /// The configuration.
    pub fn cfg(&self) -> &CaptionerCfg {
        &self.cfg
    }

    /// The time of the next picture.
    pub fn position(&self) -> Millis {
        Millis::of_field_period(self.fields as i64, self.cfg.rate)
    }

    fn period(&self, t: Millis) -> u64 {
        t.to_field_periods(self.cfg.rate).max(0) as u64
    }

    /// Schedules cues of track `track` (an index into [`CaptionerCfg::tracks`]). Cues may come
    /// in several calls (live text); a cue that starts before the end of the last one already
    /// scheduled ends that one at its start.
    pub fn add_cues(&mut self, track: usize, cues: &[Cue]) -> Report {
        let mut rep = Report::default();
        let Some(t) = self.cfg.tracks.get(track).copied() else {
            return rep;
        };
        let pop = |p: Option<Presentation>| p == Some(Presentation::PopOn);
        let need_screens =
            pop(t.cea608.map(|c| c.presentation)) || pop(t.cea708.map(|c| c.presentation));
        if need_screens {
            let (screens, lrep) = layout::screens(cues, &t.layout);
            rep.layout = lrep;
            for s in &screens {
                self.pop_on_screen(track, s, &mut rep);
            }
        }
        let roll = |p: Option<Presentation>| matches!(p, Some(Presentation::RollUp(_)));
        if roll(t.cea608.map(|c| c.presentation)) || roll(t.cea708.map(|c| c.presentation)) {
            let mut sorted: Vec<&Cue> = cues
                .iter()
                .filter(|c| c.end > c.start && c.lines.iter().any(|l| !l.is_blank()))
                .collect();
            sorted.sort_by_key(|c| c.start);
            for c in sorted {
                self.roll_up_cue(track, c, &mut rep);
            }
        }
        rep
    }

    /// Schedules screens laid out by the caller (pop-on services of the track only).
    pub fn add_screens(&mut self, track: usize, screens: &[Screen]) -> Report {
        let mut rep = Report::default();
        for s in screens {
            self.pop_on_screen(track, s, &mut rep);
        }
        rep
    }

    fn pop_on_screen(&mut self, track: usize, s: &Screen, rep: &mut Report) {
        let (ps, pe) = (self.period(s.start), self.period(s.end));
        let fslot = self.fslot;
        let max_lead = self.period(self.cfg.max_lead);
        let rate = self.cfg.rate;
        let policy = self.cfg.late;
        for plan in self.f608.iter_mut().flatten() {
            if plan.track == track && plan.cfg.presentation == Presentation::PopOn {
                let f = plan.cfg.channel.field().index();
                plan.pop_on(s, ps, pe, fslot[f], max_lead, policy, rate, rep);
            }
        }
        let dslot = self.dslot;
        for i in 0..self.s708.len() {
            let p = &self.s708[i];
            if p.track == track && p.cfg.presentation == Presentation::PopOn {
                self.pop_on_708(i, s, ps, pe, dslot, max_lead, rep);
            }
        }
    }

    fn roll_up_cue(&mut self, track: usize, c: &Cue, rep: &mut Report) {
        let (ps, pe) = (self.period(c.start), self.period(c.end));
        let fslot = self.fslot;
        let tol = self.period(self.cfg.roll_up_tolerance);
        let rate = self.cfg.rate;
        let policy = self.cfg.late;
        for plan in self.f608.iter_mut().flatten() {
            if plan.track == track
                && let Presentation::RollUp(n) = plan.cfg.presentation
            {
                let f = plan.cfg.channel.field().index();
                plan.roll_up(c, n, ps, pe, fslot[f], tol, policy, rate, rep);
            }
        }
        let dslot = self.dslot;
        for i in 0..self.s708.len() {
            let p = &self.s708[i];
            if p.track == track
                && let Presentation::RollUp(n) = p.cfg.presentation
            {
                self.roll_up_708(i, c, n, ps, pe, dslot, tol, rep);
            }
        }
    }

    // ---- CEA-708 planning ----

    /// The first free run of DTVCC slots at or after `c`: `(start, end)`.
    fn free_run(&self, mut c: u64) -> (u64, u64) {
        while let Some((&st, p)) = self.occ708.range(..=c).next_back() {
            let end = st + p.len_pairs() as u64;
            if end > c {
                c = end;
            } else {
                break;
            }
        }
        let b = self
            .occ708
            .range(c + 1..)
            .next()
            .map_or(u64::MAX, |(&st, _)| st);
        (c, b)
    }

    /// Places one small packet at the first run of `p.len_pairs()` free slots at or after
    /// `at`; returns its first slot.
    fn place_fixed(&mut self, at: u64, p: Packet) -> u64 {
        let n = p.len_pairs() as u64;
        let mut c = at;
        loop {
            let (a, b) = self.free_run(c);
            if b - a >= n {
                self.occ708.insert(a, p);
                return a;
            }
            c = b;
        }
    }

    /// Packs `elements` of `service` into packets in the free runs from `lo`; all must end by
    /// `hi` (when given). Returns the slot after the last packet, or `None` (nothing placed)
    /// if they do not fit.
    fn place_load(
        &mut self,
        service: ServiceNumber,
        elements: Vec<Vec<u8>>,
        lo: u64,
        hi: Option<u64>,
    ) -> Option<u64> {
        let mut packer = Packer::new(service, elements);
        let mut placed: Vec<(u64, Packet)> = Vec::new();
        let mut c = lo;
        while !packer.is_done() {
            let (a, b) = self.free_run(c);
            let room = (b - a).min(64) as usize;
            let limit = hi.map_or(usize::MAX, |h| h.saturating_sub(a) as usize);
            if limit < packer.min_pairs() {
                return None;
            }
            if room < packer.min_pairs() {
                c = b;
                continue;
            }
            let p = packer.next_packet(room.min(limit))?;
            c = a + p.len_pairs() as u64;
            placed.push((a, p));
            // the tentative packets are not in occ708 yet; keep runs from crossing them
            if c >= b {
                c = b;
            }
        }
        let end = placed.last().map_or(lo, |(a, p)| a + p.len_pairs() as u64);
        if hi.is_some_and(|h| end > h) {
            return None;
        }
        for (a, p) in placed {
            self.occ708.insert(a, p);
        }
        Some(end)
    }

    fn single_packet(service: ServiceNumber, cmds: &[Command]) -> Option<Packet> {
        let mut p = Packer::new(service, cmds.iter().map(Command::bytes).collect());
        p.next_packet(64)
    }

    /// Cancels the clearing packet of the screen before when a screen starting at
    /// `at_or_before` replaces it (within a field period); returns its packet and slot.
    fn cancel_708(&mut self, i: usize, at_or_before: u64) -> Option<(u64, Packet)> {
        let dslot = self.dslot;
        let prev = self.s708[i].prev.as_mut()?;
        let c = prev.clear?;
        if c >= dslot && at_or_before <= c + 9 {
            prev.clear = None;
            return self.occ708.remove(&c).map(|p| (c, p));
        }
        None
    }

    /// Puts back a canceled clearing packet (the screen that was to replace it is skipped).
    fn restore_708(&mut self, i: usize, canceled: Option<(u64, Packet)>) {
        if let Some((c, p)) = canceled {
            let at = self.place_fixed(c, p);
            if let Some(prev) = self.s708[i].prev.as_mut() {
                prev.clear = Some(at);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pop_on_708(
        &mut self,
        i: usize,
        s: &Screen,
        ps: u64,
        pe: u64,
        dslot: u64,
        max_lead: u64,
        rep: &mut Report,
    ) {
        let plan = self.s708[i].clone();
        let svc = plan.cfg.service;
        let par = plan.parity;
        let show_at = 9 * ps;
        let end_at = 9 * pe.max(ps + 1);
        let canceled = self.cancel_708(i, show_at);
        let this_all = WindowMap(0x55 << par);
        let other_all = WindowMap(0x55 << (1 - par));
        let (elements, this_used) = load_708(s, par, &plan);
        let after = plan.prev.as_ref().map_or(0, |p| p.after);
        let lo = dslot.max(after).max(show_at.saturating_sub(9 * max_lead));
        let rate = self.cfg.rate;
        let late = |shown: Option<u64>| Late {
            track: plan.track,
            service: ServiceId::Cea708(svc),
            start: s.start,
            end: s.end,
            shown: shown.map(|j| Millis::of_field_period((j / 9) as i64, rate)),
            text: s.text(),
        };
        let show_cmds = [
            Command::DeleteWindows(other_all),
            Command::DisplayWindows(this_used),
        ];
        let Some(show) = Self::single_packet(svc, &show_cmds) else {
            return;
        };
        let shown_at;
        if let Some(end) = self.place_load(svc, elements.clone(), lo, Some(show_at)) {
            shown_at = self.place_fixed(show_at.max(end), show);
            if shown_at >= show_at + 9 {
                rep.late.push(late(Some(shown_at)));
            }
        } else {
            match self.cfg.late {
                LatePolicy::Skip => {
                    rep.late.push(late(None));
                    self.restore_708(i, canceled);
                    return;
                }
                LatePolicy::Delay => {
                    // where would it end without a deadline?
                    let mut probe = self.clone();
                    let Some(end) = probe.place_load(svc, elements.clone(), lo, None) else {
                        return;
                    };
                    if end + 3 > end_at {
                        rep.late.push(late(None));
                        self.restore_708(i, canceled);
                        return;
                    }
                    if self.place_load(svc, elements, lo, None).is_none() {
                        return;
                    }
                    shown_at = self.place_fixed(end, show);
                    rep.late.push(late(Some(shown_at)));
                }
            }
        }
        let clear = Self::single_packet(svc, &[Command::DeleteWindows(this_all)]);
        let clear_at = clear.map(|c| self.place_fixed(end_at.max(shown_at + 3), c));
        let p = &mut self.s708[i];
        p.parity = 1 - par;
        p.prev = Some(Prev708 {
            after: shown_at + 3,
            clear: clear_at,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn roll_up_708(
        &mut self,
        i: usize,
        c: &Cue,
        n: RollUpRows,
        ps: u64,
        pe: u64,
        dslot: u64,
        tol: u64,
        rep: &mut Report,
    ) {
        let plan = self.s708[i].clone();
        let svc = plan.cfg.service;
        let start_at = 9 * ps;
        let canceled = self.cancel_708(i, start_at);
        let after = plan.prev.as_ref().map_or(0, |p| p.after);
        // the window and pen commands before the text take about two field periods
        let mut lo = dslot.max(after).max(start_at.saturating_sub(18));
        let rows = layout::roll_up_rows(c);
        let n = n as u8;
        let base = plan.layout.bottom_row.get();
        let def = WindowDef {
            id: WindowId::ZERO,
            priority: 0,
            anchor: Anchor::BottomCenter,
            relative: false,
            av: (5 * base - 1).min(74),
            ah: plan.cfg.aspect.center(),
            rows: n,
            cols: 32,
            row_lock: true,
            col_lock: true,
            visible: true,
            window_style: 2,
            pen_style: 1,
        };
        let first = lo;
        if lo > start_at + 9 * tol {
            rep.late.push(Late {
                track: plan.track,
                service: ServiceId::Cea708(svc),
                start: c.start,
                end: c.end,
                shown: (self.cfg.late == LatePolicy::Delay)
                    .then(|| Millis::of_field_period((first / 9) as i64, self.cfg.rate)),
                text: c.text(),
            });
            if self.cfg.late == LatePolicy::Skip {
                self.restore_708(i, canceled);
                return;
            }
        }
        for r in &rows {
            let mut e: Vec<Vec<u8>> = vec![
                Command::DefineWindow(def).bytes(),
                Command::SetWindowAttributes(WindowAttrs::POP_ON).bytes(),
                Command::SetPenColor(PenColor::NTSC).bytes(),
                Command::SetPenLocation { row: n - 1, col: 0 }.bytes(),
                Command::Cr.bytes(),
                Command::SetPenLocation {
                    row: n - 1,
                    col: r.col,
                }
                .bytes(),
            ];
            push_text_708(&r.cells, &mut e);
            e.push(Command::Etx.bytes());
            if let Some(end) = self.place_load(svc, e, lo, None) {
                lo = end;
            }
        }
        let end_at = (9 * pe).max(lo);
        let clear = Self::single_packet(svc, &[Command::DeleteWindows(WindowMap(1))]);
        let clear_at = clear.map(|k| self.place_fixed(end_at, k));
        let p = &mut self.s708[i];
        p.rolled = true;
        p.prev = Some(Prev708 {
            after: lo,
            clear: clear_at,
        });
    }

    // ---- output ----

    /// The `cc_data()` of the next picture, shown for `fields` field periods (1 to 3):
    /// `out` is cleared and filled with `10 x fields` constructs (`3 x` that many bytes): one
    /// CEA-608 construct per field period in display order, then DTVCC constructs and
    /// padding. `top_first`: `Some(parity of the picture's first field)` on an interlaced
    /// output, where the pair of field 2 (`cc_type` 01) goes with a top field
    /// (`tb_timeline::captions::CaptionCfg::top_first`, CTA-708-E §4.4.2 and Annex C); `None`
    /// on a progressive output, where the fields take turns, one per field period. This is the
    /// form `tb_m2v_encoder::Encoder::submit_shown` checks with
    /// `tb_m2v_syntax::captions::validate_shown`.
    pub fn picture(&mut self, fields: u8, top_first: Option<bool>, out: &mut Vec<u8>) {
        out.clear();
        let fields = fields.clamp(1, 3);
        for k in 0..u64::from(fields) {
            let f = ccdata::field_of(self.fields + k, k, top_first);
            let slot = self.fslot[f.index()];
            self.fslot[f.index()] += 1;
            let c = match &mut self.f608[f.index()] {
                Some(plan) => {
                    // stale entries (never expected) are dropped with the slot
                    while let Some((&k0, _)) = plan.occ.first_key_value() {
                        if k0 < slot {
                            plan.occ.remove(&k0);
                        } else {
                            break;
                        }
                    }
                    let p = plan.occ.remove(&slot).unwrap_or(Pair::NULL);
                    ccdata::construct_608(f, Some(p))
                }
                None => ccdata::construct_608(f, None),
            };
            out.extend_from_slice(&c);
        }
        self.fields += u64::from(fields);
        let n = 9 * u64::from(fields);
        let mut j = self.dslot;
        let stop = self.dslot + n;
        while j < stop {
            match self.occ708.range(..=j).next_back() {
                Some((&st, p)) if st + p.len_pairs() as u64 > j => {
                    let i = (j - st) as usize;
                    let seq = if i == 0 {
                        let s = self.seq;
                        self.seq = (s + 1) & 3;
                        s
                    } else {
                        0
                    };
                    let pair = p.pairs(seq).nth(i).unwrap_or([0, 0]);
                    out.extend_from_slice(&ccdata::construct_dtvcc(i == 0, pair));
                }
                _ => out.extend_from_slice(&ccdata::PAD_DTVCC),
            }
            j += 1;
        }
        self.dslot = stop;
        // packets that are wholly sent
        while let Some((&st, p)) = self.occ708.first_key_value() {
            if st + p.len_pairs() as u64 <= self.dslot {
                self.occ708.remove(&st);
            } else {
                break;
            }
        }
    }

    /// Nothing scheduled remains to be sent (every reserved slot is in the past).
    pub fn is_idle(&self) -> bool {
        self.occ708.is_empty() && self.f608.iter().flatten().all(|p| p.occ.is_empty())
    }
}

/// The load of a pop-on screen in CEA-708: the elements, and the windows it defines.
fn load_708(s: &Screen, par: u8, plan: &Plan708) -> (Vec<Vec<u8>>, WindowMap) {
    let mut e: Vec<Vec<u8>> = vec![Command::DeleteWindows(WindowMap(0x55 << par)).bytes()];
    let mut used = 0u8;
    for (b, block) in row_blocks(&s.rows).iter().enumerate().take(4) {
        let id = par + 2 * b as u8;
        let Some(wid) = WindowId::new(id) else {
            continue;
        };
        used |= 1 << id;
        let (first, last) = (block[0].row.get(), block[block.len() - 1].row.get());
        let def = WindowDef {
            id: wid,
            priority: 0,
            anchor: Anchor::BottomCenter,
            relative: false,
            av: (5 * last - 1).min(74),
            ah: plan.cfg.aspect.center(),
            rows: last - first + 1,
            cols: 32,
            row_lock: true,
            col_lock: true,
            visible: false,
            window_style: 2,
            pen_style: 1,
        };
        e.push(Command::DefineWindow(def).bytes());
        e.push(Command::SetWindowAttributes(WindowAttrs::POP_ON).bytes());
        e.push(Command::SetPenColor(PenColor::NTSC).bytes());
        for r in block.iter() {
            e.push(
                Command::SetPenLocation {
                    row: r.row.get() - first,
                    col: r.col,
                }
                .bytes(),
            );
            push_text_708(&r.cells, &mut e);
        }
    }
    e.push(Command::Etx.bytes());
    (e, WindowMap(used))
}

/// Rows split where their numbers are not consecutive.
fn row_blocks(rows: &[LaidRow]) -> Vec<&[LaidRow]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=rows.len() {
        if i == rows.len() || rows[i].row.get() != rows[i - 1].row.get() + 1 {
            if i > start {
                out.push(&rows[start..i]);
            }
            start = i;
        }
    }
    out
}

/// A row's characters with SetPenAttributes before the first and at each change of style.
/// A character CEA-708 lacks is replaced by a near one ([`Char708::substitute`]) or sent as
/// `_` (§7.1.6: "the underline character ... should be used as a substitute for unsupported
/// graphic symbols").
fn push_text_708(cells: &[layout::Cell], e: &mut Vec<Vec<u8>>) {
    let mut cur: Option<SpanStyle> = None;
    for c in cells {
        if cur != Some(c.style) {
            cur = Some(c.style);
            e.push(
                Command::SetPenAttributes(PenAttrs::plain(c.style.italic, c.style.underline))
                    .bytes(),
            );
        }
        let ch = Char708::from_char(c.ch)
            .or_else(|| Char708::substitute(c.ch))
            .or_else(|| Char708::from_char('_'))
            .map(Command::Char);
        if let Some(ch) = ch {
            e.push(ch.bytes());
        }
    }
}

impl Plan608 {
    fn units(&self) -> Units {
        Units::new(self.cfg.channel, self.cfg.redundancy)
    }

    fn free(&self, s: u64, n: u64) -> bool {
        (s..s + n).all(|k| !self.occ.contains_key(&k))
    }

    /// Slots for `units` in order from `lo`, avoiding occupied slots, ending by `hi`.
    fn fit(&self, units: &[cea608::Unit], lo: u64, hi: Option<u64>) -> Option<Vec<(u64, Pair)>> {
        let mut c = lo;
        let mut out = Vec::new();
        for u in units {
            let n = u.slots() as u64;
            while !self.free(c, n) {
                c += 1;
            }
            for k in 0..n {
                out.push((c + k, u.pair()));
            }
            c += n;
            if hi.is_some_and(|h| c > h) {
                return None;
            }
        }
        Some(out)
    }

    fn fixed(&mut self, at: u64, code: Code) -> u64 {
        let mut u = self.units();
        u.code(code);
        let units = u.finish();
        let n = units.iter().map(|u| u.slots() as u64).sum::<u64>();
        let mut c = at;
        while !self.free(c, n) {
            c += 1;
        }
        if let Some(v) = self.fit(&units, c, None) {
            for (k, p) in v {
                self.occ.insert(k, p);
            }
        }
        c
    }

    fn remove_code(&mut self, at: u64) {
        let p = self.occ.remove(&at);
        if self.cfg.redundancy == Redundancy::Doubled
            && p.is_some()
            && self.occ.get(&(at + 1)) == p.as_ref()
        {
            self.occ.remove(&(at + 1));
        }
    }

    /// Cancels the EDM of the screen before when a screen starting at `at_or_before`
    /// replaces it (within a frame); returns its slot.
    fn cancel_edm(&mut self, at_or_before: u64, now: u64) -> Option<u64> {
        let prev = self.prev.as_mut()?;
        let e = prev.edm?;
        if e >= now && at_or_before <= e + 1 {
            prev.edm = None;
            self.remove_code(e);
            return Some(e);
        }
        None
    }

    /// Puts back a canceled EDM (the screen that was to replace it is skipped).
    fn restore_edm(&mut self, canceled: Option<u64>) {
        if let Some(e) = canceled {
            let at = self.fixed(e, Code::Misc(Misc::Edm));
            if let Some(prev) = self.prev.as_mut() {
                prev.edm = Some(at);
            }
        }
    }

    fn late(
        &self,
        start: Millis,
        end: Millis,
        shown: Option<u64>,
        text: String,
        rate: FieldRate,
    ) -> Late {
        Late {
            track: self.track,
            service: ServiceId::Cea608(self.cfg.channel),
            start,
            end,
            shown: shown.map(|k| Millis::of_field_period(2 * k as i64, rate)),
            text,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pop_on(
        &mut self,
        s: &Screen,
        ps: u64,
        pe: u64,
        now: u64,
        max_lead: u64,
        policy: LatePolicy,
        rate: FieldRate,
        rep: &mut Report,
    ) {
        let (ss, mut es) = (ps / 2, pe / 2);
        // a screen shorter than two frames is shown for two (EOC and its copy must go first)
        es = es.max(ss + 2);
        let canceled = self.cancel_edm(ss, now);
        let mut u = self.units();
        u.code(Code::Misc(Misc::Rcl));
        u.code(Code::Misc(Misc::Enm));
        for r in &s.rows {
            cea608::push_row(&mut u, r.row, r.col, &r.cea608());
        }
        let load = u.finish();
        let after = self.prev.as_ref().map_or(0, |p| p.after);
        let lo = now.max(after).max(ss.saturating_sub(max_lead / 2));
        // the EOC first, then the load before it
        let eoc_n = if self.cfg.redundancy == Redundancy::Doubled {
            2
        } else {
            1
        };
        let mut eoc = ss.max(now);
        while !self.free(eoc, eoc_n) {
            eoc += 1;
        }
        let placed = if eoc == ss {
            self.fit(&load, lo, Some(eoc))
        } else {
            None
        };
        let eoc = match placed {
            Some(v) => {
                for (k, p) in v {
                    self.occ.insert(k, p);
                }
                self.fixed(eoc, Code::Misc(Misc::Eoc))
            }
            None => {
                let text = s.text();
                if policy == LatePolicy::Skip {
                    rep.late.push(self.late(s.start, s.end, None, text, rate));
                    self.restore_edm(canceled);
                    return;
                }
                let Some(v) = self.fit(&load, lo, None) else {
                    return;
                };
                let end = v.last().map_or(lo, |x| x.0 + 1);
                let mut at = end.max(ss);
                while !self.free(at, eoc_n) {
                    at += 1;
                }
                if at + eoc_n >= es {
                    rep.late.push(self.late(s.start, s.end, None, text, rate));
                    self.restore_edm(canceled);
                    return;
                }
                for (k, p) in v {
                    self.occ.insert(k, p);
                }
                let at = self.fixed(at, Code::Misc(Misc::Eoc));
                rep.late
                    .push(self.late(s.start, s.end, Some(at), text, rate));
                at
            }
        };
        let edm = self.fixed(es.max(eoc + eoc_n), Code::Misc(Misc::Edm));
        self.prev = Some(Prev608 {
            after: eoc + eoc_n,
            edm: Some(edm),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn roll_up(
        &mut self,
        c: &Cue,
        n: RollUpRows,
        ps: u64,
        pe: u64,
        now: u64,
        tol: u64,
        policy: LatePolicy,
        rate: FieldRate,
        rep: &mut Report,
    ) {
        let ss = ps / 2;
        let canceled = self.cancel_edm(ss, now);
        let ru = match n {
            RollUpRows::Two => Misc::Ru2,
            RollUpRows::Three => Misc::Ru3,
            RollUpRows::Four => Misc::Ru4,
        };
        let base = self.layout.bottom_row;
        let after = self.prev.as_ref().map_or(0, |p| p.after);
        // the codes before the first row's text (RU, CR, PAC, tab offset, mid-row code) go
        // before the cue's time, so that its text starts on time
        let rows = layout::roll_up_rows(c);
        let lead = rows.first().map_or(0, |r| {
            let mut u = self.units();
            u.code(Code::Misc(ru));
            u.code(Code::Misc(Misc::Cr));
            cea608::push_row(&mut u, base, r.col, &r.cea608());
            u.finish()
                .iter()
                .take_while(|x| x.pair().is_control())
                .map(|x| x.slots() as u64)
                .sum::<u64>()
        });
        let mut lo = now.max(after).max(ss.saturating_sub(lead));
        if lo > ss + tol / 2 {
            rep.late.push(self.late(
                c.start,
                c.end,
                (policy == LatePolicy::Delay).then_some(lo),
                c.text(),
                rate,
            ));
            if policy == LatePolicy::Skip {
                self.restore_edm(canceled);
                return;
            }
        }
        for r in rows {
            let mut u = self.units();
            u.code(Code::Misc(ru));
            u.code(Code::Misc(Misc::Cr));
            cea608::push_row(&mut u, base, r.col, &r.cea608());
            let units = u.finish();
            if let Some(v) = self.fit(&units, lo, None) {
                lo = v.last().map_or(lo, |x| x.0 + 1);
                for (k, p) in v {
                    self.occ.insert(k, p);
                }
            }
        }
        let edm = self.fixed((pe / 2).max(lo), Code::Misc(Misc::Edm));
        self.prev = Some(Prev608 {
            after: lo,
            edm: Some(edm),
        });
    }
}
