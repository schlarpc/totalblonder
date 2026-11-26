//! Frame and row selection of one output channel, per output frame: a port of the per-channel
//! block of the tick loop of the C++ harness `m2vpipe` (docs/ARCHITECTURE.md, "History") without
//! any device, libav or encoder call.
//!
//! The caller owns the inputs (queues, decoded frames), the encoder session and the converter.
//! At every tick on which a channel fires (`(t + phase) % every == every - 1`, see
//! [`crate::Divider`]) it describes each tile's input as a [`TileIn`] and calls
//! [`Channel::select`]. The answer says what to submit: the previous frame again, the slate, a
//! host frame, or a new device frame made by a list of conversion [`Job`]s. Fit, color and
//! overlay are the caller's (they need the converter); so is the log file, for which the answer
//! carries the action character and the two source indices exactly as the C++ writes them to
//! `--ptslog`.
//!
//! Every decision, counter and state update is made in the order of the C++, so that an offline
//! run makes the same decisions frame for frame. Places where the C++ looks doubtful are ported
//! as they are and marked "Doubt" in the comments.

/// Rows of a picture a conversion reads or writes (`ROWS_ALL`, `ROWS_TOP`, `ROWS_BOTTOM` of
/// the C++ spike; the caller maps it to the converter's type).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rows {
    /// Every row.
    All,
    /// The even rows (top field).
    Top,
    /// The odd rows (bottom field).
    Bottom,
}

/// What selection needs to know about a decoded frame (the C++ reads it from the `AVFrame`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SrcFrame {
    /// The input's running frame index (`QFrame::idx`: continuous over loops).
    pub idx: i64,
    /// `AV_FRAME_FLAG_INTERLACED`.
    pub interlaced: bool,
    /// `AV_FRAME_FLAG_TOP_FIELD_FIRST`. The C++ harness's rule (`tff = flag || !src_il`) is
    /// applied here, so the caller may pass the raw flag.
    pub tff: bool,
    /// Frame width in samples (`AVFrame::width`).
    pub w: u32,
    /// Frame height in rows (`AVFrame::height`).
    pub h: u32,
}

/// One tile's input at this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileIn {
    /// The frame the input selected at this tick (`in.now`).
    pub now: Option<SrcFrame>,
    /// The frame it had selected at the tick before (`in.prev`).
    pub prev: Option<SrcFrame>,
    /// The input's frame rate (`in.dec.fps`).
    pub fps: f64,
    /// `(Tsel - in.off) - now.t > hold`, evaluated by the caller; false when
    /// there is no frame.
    pub stale: bool,
}

/// The fixed properties of a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChannelCfg {
    /// Output width (`Fmt::w`).
    pub w: u32,
    /// Output height (`Fmt::h`).
    pub h: u32,
    /// Output frame rate, numerator (`Fmt::fn`).
    pub fps_n: u32,
    /// Output frame rate, denominator (`Fmt::fd`).
    pub fps_d: u32,
    /// Interlaced output (`Fmt::il`).
    pub interlaced: bool,
    /// A grid of inputs (`Chan::mosaic`).
    pub mosaic: bool,
    /// The comparison path: frames converted on the host (`Chan::host`).
    pub host: bool,
    /// Number of tiles (`Chan::tiles.size()`): 1 for a plain channel, 0 for a slate-only one.
    pub tiles: usize,
}

/// Which of a tile's two selected frames a job reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Which {
    /// `top[k]`: the frame of the tick before when fields are selected one by one, else the
    /// same as [`Which::Bot`].
    Top,
    /// `bot[k]`: the frame selected at this tick.
    Bot,
}

/// One conversion into the output frame (an entry of `jobs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Job {
    /// Index of the tile.
    pub tile: usize,
    /// Which of the tile's frames is read.
    pub which: Which,
    /// That frame.
    pub frame: SrcFrame,
    /// Source rows.
    pub srows: Rows,
    /// Destination rows.
    pub drows: Rows,
    /// The vertical [1 2 1]/4 low-pass across output lines (sources above 40 Hz woven into an
    /// interlaced frame).
    pub weave_lp: bool,
}

/// What the caller must do for this output frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// `submit_again`: the encoder codes the previous frame again.
    Again,
    /// The slate. `copy`: `submit_dev(progressive = true)` and one copy of the slate frame into
    /// the target; else `submit_again` (the channel already shows the slate).
    Slate {
        /// The slate must be copied into a new device target.
        copy: bool,
    },
    /// Host path: `submit` of this frame's host buffer.
    Host {
        /// The frame to submit.
        frame: SrcFrame,
    },
    /// `submit_dev(progressive)`, then these conversions into the target (in this order).
    Convert {
        /// The picture's `progressive_frame` flag.
        progressive: bool,
        /// The conversions; never empty.
        jobs: Vec<Job>,
    },
}

/// The answer of [`Channel::select`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    /// What to do.
    pub decision: Decision,
    /// The action as `--ptslog` names it: `N` new frame, `W` not progressive (two instants or an
    /// interlaced source), `A` again, `S` slate copied, `s` slate again, `H` host frame.
    pub act: char,
    /// Input frame behind the top field as the log has it (-1: none).
    pub src_top: i64,
    /// Input frame behind the bottom field as the log has it (-1: none).
    pub src_bot: i64,
    /// Index of this output frame since the start (`npts - 1` after the call).
    pub n: i64,
    /// Its PTS in 90 kHz units: `n * 90000 * fps_d / fps_n`, integer division.
    pub pts: i64,
}

/// The counters of a channel that leave the warm-up out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Output frames.
    pub frames: i64,
    /// Output frames made from new input frames.
    pub fresh: i64,
    /// Output frames that repeat the previous one.
    pub again: i64,
    /// Output frames that show the slate.
    pub slates: i64,
    /// Output frames with two source instants woven.
    pub weaves: i64,
    /// Input frames the frame clock dropped.
    pub drops: i64,
    /// Progressive source frames in an interlaced output frame.
    pub psf: i64,
}

/// Selection state of one output channel (the selection fields of `Chan`).
#[derive(Clone, Debug)]
pub struct Channel {
    cfg: ChannelCfg,
    last_top: i64,
    last_bot: i64,
    bob_idx: i64,
    bob_field: i32,
    last_tile: Vec<i64>,
    on_slate: bool,
    have: bool,
    npts: i64,
    /// The counters (public to read; [`Channel::reset_counters`] clears them).
    pub counters: Counters,
}

impl Channel {
    /// A channel before its first frame (`Chan`'s initial values, `last_tile` all -2).
    pub fn new(cfg: ChannelCfg) -> Self {
        Self {
            cfg,
            last_top: -2,
            last_bot: -2,
            bob_idx: -2,
            bob_field: 0,
            last_tile: vec![-2; cfg.tiles],
            on_slate: false,
            have: false,
            npts: 0,
            counters: Counters::default(),
        }
    }

    /// The configuration.
    pub fn cfg(&self) -> &ChannelCfg {
        &self.cfg
    }

    /// Output frames since the start, warm-up included (`Chan::npts`).
    pub fn npts(&self) -> i64 {
        self.npts
    }

    /// The channel shows the slate (`Chan::on_slate`; the host path never sets it).
    pub fn on_slate(&self) -> bool {
        self.on_slate
    }

    /// End of the warm-up: the counters start again, `npts` and the
    /// selection state stay.
    pub fn reset_counters(&mut self) {
        self.counters = Counters::default();
    }

    /// The frames behind the two fields of a tile.
    ///
    /// Interlaced output takes one selection per field, except from a source at the output's
    /// frame rate (within 1 %), where both fields come from the frame of this tick.
    fn pick(&self, t: &TileIn) -> (Option<SrcFrame>, Option<SrcFrame>) {
        let c = &self.cfg;
        let same_rate = (t.fps * f64::from(c.fps_d) / f64::from(c.fps_n) - 1.0).abs() < 0.01;
        let per_field = c.interlaced && !c.mosaic && t.prev.is_some() && !same_rate;
        (if per_field { t.prev } else { t.now }, t.now)
    }

    /// One output frame of this channel.
    ///
    /// `tiles` has one entry per tile, in tile order (`cfg.tiles` of them; further entries are
    /// ignored). Call it only at the channel's ticks.
    pub fn select(&mut self, tiles: &[TileIn]) -> Selected {
        let tiles = tiles.get(..self.cfg.tiles).unwrap_or(tiles);
        let n = self.npts;
        let pts = crate::sat(
            i128::from(n) * 90_000 * i128::from(self.cfg.fps_d) / i128::from(self.cfg.fps_n.max(1)),
        );
        self.npts += 1;
        self.counters.frames += 1;
        // Is there a frame at all, and has every source of a plain channel
        // had one within `hold`?
        let mut any = false;
        let mut alive = !tiles.is_empty();
        for t in tiles {
            if t.now.is_some() {
                any = true;
            }
            if (t.now.is_none() || t.stale) && !self.cfg.mosaic {
                alive = false;
            }
        }
        // The single input of a plain channel. `alive` implies it has frames; a channel
        // without them takes the slate branch as in the C++.
        let single = tiles.first().and_then(|t| match self.pick(t) {
            (Some(top), Some(bot)) => Some((top, bot)),
            _ => None,
        });
        let (decision, act, src_top, src_bot) = if self.cfg.host {
            self.host(tiles)
        } else if !alive || !any {
            self.slate()
        } else if self.cfg.mosaic {
            self.device(tiles, None)
        } else if let Some(pair) = single {
            self.device(tiles, Some(pair))
        } else {
            self.slate()
        };
        Selected {
            decision,
            act,
            src_top,
            src_bot,
            n,
            pts,
        }
    }

    /// The host path.
    ///
    /// Doubt: a host channel never returns to the slate when its input goes away (it repeats
    /// for good), and `on_slate` is not used here.
    fn host(&mut self, tiles: &[TileIn]) -> (Decision, char, i64, i64) {
        let r = tiles.first().and_then(|t| t.now);
        let (decision, act) = match r {
            Some(frame) if frame.idx != self.last_bot => {
                if self.last_bot >= 0 && frame.idx > self.last_bot + 1 {
                    self.counters.drops += frame.idx - self.last_bot - 1;
                }
                self.last_bot = frame.idx;
                self.counters.fresh += 1;
                self.have = true;
                (Decision::Host { frame }, 'H')
            }
            _ if self.have => {
                self.counters.again += 1;
                (Decision::Again, 'A')
            }
            _ => {
                // nothing yet: the slate
                self.counters.slates += 1;
                self.have = true;
                (Decision::Slate { copy: true }, 'S')
            }
        };
        (decision, act, self.last_bot, self.last_bot)
    }

    /// The fallback: the slate, copied once and then repeated.
    fn slate(&mut self) -> (Decision, char, i64, i64) {
        let (copy, act) = if self.on_slate {
            (false, 's')
        } else {
            (true, 'S')
        };
        self.on_slate = true;
        self.counters.slates += 1;
        self.have = true;
        self.last_top = -2;
        self.last_bot = -2;
        (Decision::Slate { copy }, act, -1, -1)
    }

    /// `submit_again` with its accounting.
    fn again(&mut self) -> (Decision, char, i64, i64) {
        self.counters.again += 1;
        (Decision::Again, 'A', self.last_top, self.last_bot)
    }

    /// Frames on the device. `single`: top and bottom frame of a plain
    /// channel's input (`None` for a mosaic).
    fn device(
        &mut self,
        tiles: &[TileIn],
        single: Option<(SrcFrame, SrcFrame)>,
    ) -> (Decision, char, i64, i64) {
        let c = self.cfg;
        // has anything changed since the last output frame?
        let mut changed = match single {
            None => tiles
                .iter()
                .zip(&self.last_tile)
                .any(|(t, &last)| t.now.map_or(-1, |f| f.idx) != last),
            // bob: the second field of the same frame is new content
            Some((_, bot)) if !c.interlaced && bot.interlaced && c.fps_n == 60000 => true,
            Some((top, bot)) => bot.idx != self.last_bot || top.idx != self.last_top,
        };
        if self.on_slate {
            changed = true;
        }
        self.on_slate = false;
        if !changed {
            return self.again();
        }
        let mut progressive = true;
        let mut jobs = Vec::new();
        for (k, t) in tiles.iter().enumerate() {
            if !self.tile_jobs(k, t, &mut jobs, &mut progressive) {
                jobs.clear(); // both fields shown: repeat
                break;
            }
        }
        if jobs.is_empty() {
            return self.again();
        }
        match single {
            None => {
                for (last, t) in self.last_tile.iter_mut().zip(tiles) {
                    *last = t.now.map_or(-1, |f| f.idx);
                }
            }
            Some((top, bot)) => {
                // Doubt: `top` counts as used whenever it differs from
                // `bot`, also for an interlaced source, of which only `bot` was converted.
                let nb = bot.idx;
                if self.last_bot >= 0 && nb > self.last_bot + 1 {
                    let used = i64::from(top.idx != nb && top.idx > self.last_bot);
                    self.counters.drops += nb - self.last_bot - 1 - used;
                }
                self.last_top = top.idx;
                self.last_bot = nb;
            }
        }
        self.counters.fresh += 1;
        self.have = true;
        let act = if progressive { 'N' } else { 'W' };
        (
            Decision::Convert { progressive, jobs },
            act,
            self.last_top,
            self.last_bot,
        )
    }

    /// The jobs of one tile. Returns false for "both fields of this frame
    /// have been shown: repeat" (the bob at 59.94p).
    ///
    /// Doubts, ported as they are: the bob state and the `psf` / `weaves` counters change here
    /// even when the frame then becomes a repeat; an interlaced tile of a mosaic shows its top
    /// field only; an interlaced source into an interlaced output is not counted in `psf` or
    /// `weaves` and is logged as `W`.
    fn tile_jobs(&mut self, k: usize, t: &TileIn, jobs: &mut Vec<Job>, prog: &mut bool) -> bool {
        let c = self.cfg;
        let (Some(top), Some(bot)) = self.pick(t) else {
            return true;
        };
        let src_il = bot.interlaced;
        let tff = bot.tff || !src_il;
        let job = |which, frame, srows, drows, weave_lp| Job {
            tile: k,
            which,
            frame,
            srows,
            drows,
            weave_lp,
        };
        if !c.interlaced || c.mosaic {
            if src_il && !c.mosaic {
                // bob: one field per output frame; at 59.94p the two fields of a frame in turn
                let mut field = 0;
                if c.fps_n == 60000 && bot.idx == self.bob_idx {
                    field = self.bob_field + 1;
                }
                if field >= 2 {
                    return false;
                }
                self.bob_idx = bot.idx;
                self.bob_field = field;
                let want_top = (field == 0) == tff;
                let srows = if want_top { Rows::Top } else { Rows::Bottom };
                jobs.push(job(Which::Bot, bot, srows, Rows::All, false));
            } else {
                let srows = if src_il { Rows::Top } else { Rows::All };
                jobs.push(job(Which::Bot, bot, srows, Rows::All, false));
            }
        } else if top.idx == bot.idx || src_il {
            if src_il {
                *prog = false;
                if bot.w == c.w && bot.h == c.h {
                    jobs.push(job(Which::Bot, bot, Rows::All, Rows::All, false));
                } else {
                    jobs.push(job(Which::Bot, bot, Rows::Top, Rows::Top, false));
                    jobs.push(job(Which::Bot, bot, Rows::Bottom, Rows::Bottom, false));
                }
            } else {
                jobs.push(job(Which::Bot, bot, Rows::All, Rows::All, false));
                self.counters.psf += 1;
            }
        } else {
            // two instants in one frame: each field from its own source frame; sources of
            // 50/60 Hz are low-pass filtered vertically against twitter
            *prog = false;
            let lp = t.fps > 40.0;
            jobs.push(job(Which::Top, top, Rows::All, Rows::Top, lp));
            jobs.push(job(Which::Bot, bot, Rows::All, Rows::Bottom, lp));
            self.counters.weaves += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tick period of the C++ harness.
    const P: f64 = 1001.0 / 60000.0;

    fn cfg(w: u32, h: u32, fps_n: u32, interlaced: bool) -> ChannelCfg {
        ChannelCfg {
            w,
            h,
            fps_n,
            fps_d: 1001,
            interlaced,
            mosaic: false,
            host: false,
            tiles: 1,
        }
    }
    const fn i1080() -> (u32, u32, u32, bool) {
        (1920, 1080, 30000, true)
    }
    fn chan(f: (u32, u32, u32, bool)) -> Channel {
        Channel::new(cfg(f.0, f.1, f.2, f.3))
    }

    fn fr(idx: i64) -> SrcFrame {
        SrcFrame {
            idx,
            interlaced: false,
            tff: false,
            w: 1920,
            h: 1080,
        }
    }
    fn fi(idx: i64, tff: bool, w: u32, h: u32) -> SrcFrame {
        SrcFrame {
            idx,
            interlaced: true,
            tff,
            w,
            h,
        }
    }
    fn tile(now: Option<SrcFrame>, prev: Option<SrcFrame>, fps: f64) -> TileIn {
        TileIn {
            now,
            prev,
            fps,
            stale: false,
        }
    }

    /// An input as the offline harness sees it: frame k at `k / fps` (the cfr rule of the C++
    /// decoder), `advance` as `In::advance` with `Input::take`.
    struct Src {
        fps: f64,
        proto: SrcFrame,
        next: i64,
        now: Option<SrcFrame>,
        prev: Option<SrcFrame>,
    }

    impl Src {
        fn new(fps: f64, proto: SrcFrame) -> Self {
            Self {
                fps,
                proto,
                next: 0,
                now: None,
                prev: None,
            }
        }
        fn advance(&mut self, limit: f64) {
            self.prev = self.now;
            while self.next as f64 / self.fps <= limit {
                self.now = Some(SrcFrame {
                    idx: self.next,
                    ..self.proto
                });
                self.next += 1;
            }
        }
        fn tile(&self) -> TileIn {
            tile(self.now, self.prev, self.fps)
        }
    }

    /// Run `ticks` ticks of the offline loop for one channel at phase 0.
    fn run(c: &mut Channel, src: &mut Src, ticks: i64) -> Vec<Selected> {
        let every = if c.cfg().fps_n == 60000 { 1 } else { 2 };
        let mut out = Vec::new();
        for t in 0..ticks {
            src.advance(t as f64 * P + 1e-4);
            if t % every == every - 1 {
                out.push(c.select(&[src.tile()]));
            }
        }
        out
    }

    fn acts(v: &[Selected]) -> String {
        v.iter().map(|s| s.act).collect()
    }

    fn jobs(s: &Selected) -> (bool, &[Job]) {
        match &s.decision {
            Decision::Convert { progressive, jobs } => (*progressive, jobs),
            d => panic!("not a conversion: {d:?}"),
        }
    }

    #[test]
    fn same_rate_30p_into_30i_is_one_progressive_job_per_frame() {
        let mut c = chan(i1080());
        let mut s = Src::new(30000.0 / 1001.0, fr(0));
        let out = run(&mut c, &mut s, 600);
        assert_eq!(out.len(), 300);
        for (k, o) in out.iter().enumerate() {
            let (prog, j) = jobs(o);
            assert!(prog);
            assert_eq!(j.len(), 1);
            assert_eq!(
                (j[0].srows, j[0].drows, j[0].weave_lp),
                (Rows::All, Rows::All, false)
            );
            assert_eq!(j[0].which, Which::Bot);
            assert_eq!((o.src_top, o.src_bot), (k as i64, k as i64));
            assert_eq!(o.n, k as i64);
            assert_eq!(o.pts, k as i64 * 3003);
        }
        assert_eq!(c.counters.psf, 300);
        assert_eq!(c.counters.fresh, 300);
        assert_eq!(
            (c.counters.weaves, c.counters.drops, c.counters.again),
            (0, 0, 0)
        );
    }

    /// The gate case `pipe-1080p24-1080i-pulldown`: 3 s offline, 179 ticks, 89 frames, of
    /// which 53 progressive and 36 woven (notes C.3).
    #[test]
    fn film_24p_into_1080i_is_three_two_pulldown() {
        let mut c = chan(i1080());
        let mut s = Src::new(24000.0 / 1001.0, fr(0));
        let out = run(&mut c, &mut s, (3.0 / P) as i64);
        assert_eq!(out.len(), 89);
        let a = acts(&out);
        assert_eq!(a.matches('N').count(), 53);
        assert_eq!(a.matches('W').count(), 36);
        assert_eq!((c.counters.psf, c.counters.weaves), (53, 36));
        assert_eq!((c.counters.drops, c.counters.again), (0, 0));
        // four source frames A B C D fill five output frames: AA AB BC CC DD
        assert_eq!(&a[..10], "NWWNNNWWNN");
        for (k, o) in out.iter().enumerate() {
            let cyc = (k / 5) as i64 * 4;
            let (top, bot) = [(0, 0), (0, 1), (1, 2), (2, 2), (3, 3)][k % 5];
            assert_eq!((o.src_top, o.src_bot), (cyc + top, cyc + bot), "frame {k}");
            let (prog, j) = jobs(o);
            assert_eq!(prog, top == bot);
            if prog {
                assert_eq!(j.len(), 1);
            } else {
                assert_eq!(j.len(), 2);
                assert_eq!(
                    (j[0].which, j[0].drows, j[0].frame.idx),
                    (Which::Top, Rows::Top, cyc + top)
                );
                assert_eq!(
                    (j[1].which, j[1].drows, j[1].frame.idx),
                    (Which::Bot, Rows::Bottom, cyc + bot)
                );
                assert!(!j[0].weave_lp && !j[1].weave_lp, "film is not low-passed");
            }
        }
    }

    #[test]
    fn sixty_p_into_30i_weaves_every_frame_with_low_pass() {
        let mut c = chan(i1080());
        let mut s = Src::new(
            60000.0 / 1001.0,
            SrcFrame {
                w: 1280,
                h: 720,
                ..fr(0)
            },
        );
        let out = run(&mut c, &mut s, 400);
        assert_eq!(out.len(), 200);
        for (k, o) in out.iter().enumerate() {
            let (prog, j) = jobs(o);
            assert!(!prog);
            assert_eq!(o.act, 'W');
            assert_eq!(j.len(), 2);
            assert!(j.iter().all(|x| x.weave_lp && x.srows == Rows::All));
            assert_eq!((j[0].frame.idx, j[0].drows), (2 * k as i64, Rows::Top));
            assert_eq!(
                (j[1].frame.idx, j[1].drows),
                (2 * k as i64 + 1, Rows::Bottom)
            );
        }
        assert_eq!(
            (c.counters.weaves, c.counters.psf, c.counters.drops),
            (200, 0, 0)
        );
    }

    #[test]
    fn pal_25p_into_720p_repeats_between_new_frames() {
        let mut c = chan((1280, 720, 60000, false));
        let mut s = Src::new(25.0, fr(0));
        let out = run(&mut c, &mut s, 1200);
        let a = acts(&out);
        assert!(a.chars().all(|x| x == 'N' || x == 'A'));
        // 1200 ticks are 20.02 s: frames 0 to 500
        assert_eq!(c.counters.fresh, 501);
        assert_eq!(c.counters.again, 1200 - 501);
        assert_eq!(c.counters.drops, 0);
        // every new frame is followed by one or two repeats (59.94 / 25 = 2.4 ticks a frame)
        for run in a.split('N').skip(1).take(499) {
            assert!(run == "A" || run == "AA", "{run:?}");
        }
        let mut shown = -1;
        for o in &out {
            if o.act == 'N' {
                shown += 1;
                let (prog, j) = jobs(o);
                assert!(prog && j.len() == 1);
            } else {
                assert_eq!(o.decision, Decision::Again);
            }
            assert_eq!((o.src_top, o.src_bot), (shown, shown));
        }
    }

    #[test]
    fn interlaced_source_into_720p_bobs_both_fields_in_turn() {
        for tff in [true, false] {
            let mut c = chan((1280, 720, 60000, false));
            let mut s = Src::new(30000.0 / 1001.0, fi(0, tff, 1920, 1080));
            let out = run(&mut c, &mut s, 100);
            let (first, second) = if tff {
                (Rows::Top, Rows::Bottom)
            } else {
                (Rows::Bottom, Rows::Top)
            };
            for (k, o) in out.iter().enumerate() {
                let (prog, j) = jobs(o);
                assert!(prog, "a bobbed field is a progressive frame");
                assert_eq!(j.len(), 1);
                assert_eq!(j[0].frame.idx, (k / 2) as i64);
                assert_eq!(j[0].srows, if k % 2 == 0 { first } else { second });
                assert_eq!(j[0].drows, Rows::All);
            }
            assert_eq!(c.counters.fresh, 100);
            // a third tick on the same frame: both fields shown, repeat
            let o = c.select(&[s.tile()]);
            assert_eq!((o.decision, o.act), (Decision::Again, 'A'));
            assert_eq!((o.src_top, o.src_bot), (49, 49));
            let o = c.select(&[s.tile()]);
            assert_eq!(o.act, 'A');
            assert_eq!(c.counters.again, 2);
            // the next frame starts with its first field again
            let t = tile(Some(fi(50, tff, 1920, 1080)), s.now, s.fps);
            let o = c.select(&[t]);
            assert_eq!(jobs(&o).1[0].srows, first);
        }
    }

    #[test]
    fn interlaced_source_into_30p_shows_the_first_field_only() {
        let mut c = chan((1920, 1080, 30000, false));
        let mut s = Src::new(30000.0 / 1001.0, fi(0, true, 1920, 1080));
        let out = run(&mut c, &mut s, 40);
        for o in &out {
            let (prog, j) = jobs(o);
            assert!(prog && j.len() == 1);
            assert_eq!((j[0].srows, j[0].drows), (Rows::Top, Rows::All));
        }
        // the same frame again is a repeat (no bob sequence below 59.94p)
        assert_eq!(c.select(&[s.tile()]).act, 'A');
    }

    #[test]
    fn interlaced_source_into_interlaced_output_copies_or_goes_field_by_field() {
        // same size: rows copied, fields kept; not progressive
        let mut c = chan(i1080());
        let mut s = Src::new(30000.0 / 1001.0, fi(0, true, 1920, 1080));
        let out = run(&mut c, &mut s, 40);
        for o in &out {
            let (prog, j) = jobs(o);
            assert!(!prog);
            assert_eq!(o.act, 'W');
            assert_eq!(j.len(), 1);
            assert_eq!(
                (j[0].srows, j[0].drows, j[0].weave_lp),
                (Rows::All, Rows::All, false)
            );
        }
        assert_eq!(
            (c.counters.psf, c.counters.weaves, c.counters.fresh),
            (0, 0, 20)
        );

        // other size: field to field
        let mut c = chan(i1080());
        let mut s = Src::new(30000.0 / 1001.0, fi(0, true, 704, 480));
        let out = run(&mut c, &mut s, 40);
        for o in &out {
            let (prog, j) = jobs(o);
            assert!(!prog);
            assert_eq!(j.len(), 2);
            assert_eq!((j[0].srows, j[0].drows), (Rows::Top, Rows::Top));
            assert_eq!((j[1].srows, j[1].drows), (Rows::Bottom, Rows::Bottom));
            assert!(j.iter().all(|x| x.which == Which::Bot && !x.weave_lp));
        }

        // an interlaced source at another rate (25i): the selection is per field, but only the
        // frame of this tick is converted; the log names both
        let mut c = chan(i1080());
        let t = tile(
            Some(fi(5, true, 1920, 1080)),
            Some(fi(4, true, 1920, 1080)),
            25.0,
        );
        let o = c.select(&[t]);
        let (prog, j) = jobs(&o);
        assert!(!prog);
        assert_eq!(j.len(), 1);
        assert_eq!(j[0].frame.idx, 5);
        assert_eq!((o.src_top, o.src_bot), (4, 5));
    }

    #[test]
    fn mosaic_is_rendered_again_when_any_tile_changes() {
        let mut c = Channel::new(ChannelCfg {
            mosaic: true,
            tiles: 3,
            ..cfg(1920, 1080, 30000, true)
        });
        let t = |a: Option<i64>| tile(a.map(fr), None, 59.94);
        // tile 2 has no frame yet: two jobs, progressive
        let o = c.select(&[t(Some(0)), t(Some(7)), t(None)]);
        let (prog, j) = jobs(&o);
        assert!(prog);
        assert_eq!(j.iter().map(|x| x.tile).collect::<Vec<_>>(), [0, 1]);
        assert!(
            j.iter()
                .all(|x| x.srows == Rows::All && x.drows == Rows::All && !x.weave_lp)
        );
        // a mosaic channel's log has the single-input fields untouched
        assert_eq!((o.act, o.src_top, o.src_bot), ('N', -2, -2));
        // nothing new
        assert_eq!(c.select(&[t(Some(0)), t(Some(7)), t(None)]).act, 'A');
        // one tile changes: all tiles again
        let o = c.select(&[t(Some(0)), t(Some(8)), t(None)]);
        assert_eq!(jobs(&o).1.len(), 2);
        // the third tile appears
        let o = c.select(&[t(Some(0)), t(Some(8)), t(Some(1))]);
        assert_eq!(jobs(&o).1.len(), 3);
        // `prev` is never used and an interlaced tile gives its top field
        let il = tile(
            Some(fi(3, false, 704, 480)),
            Some(fi(2, false, 704, 480)),
            29.97,
        );
        let o = c.select(&[il, t(Some(8)), t(Some(1))]);
        let (prog, j) = jobs(&o);
        assert!(prog);
        assert_eq!(
            (j[0].frame.idx, j[0].srows, j[0].drows),
            (3, Rows::Top, Rows::All)
        );
        // a stale tile does not take a mosaic to the slate; no tile at all does
        let stale = TileIn {
            stale: true,
            ..t(Some(8))
        };
        assert_eq!(c.select(&[il, stale, t(Some(1))]).act, 'A');
        assert_eq!(c.select(&[t(None), t(None), t(None)]).act, 'S');
        assert_eq!(
            (c.counters.fresh, c.counters.again, c.counters.slates),
            (4, 2, 1)
        );
        assert_eq!(c.counters.drops, 0);
    }

    #[test]
    fn slate_entry_exit_and_counters() {
        let mut c = chan((1280, 720, 60000, false));
        let none = tile(None, None, 59.94);
        // no frame yet
        let o = c.select(&[none]);
        assert_eq!(
            (o.decision.clone(), o.act),
            (Decision::Slate { copy: true }, 'S')
        );
        assert_eq!((o.src_top, o.src_bot), (-1, -1));
        assert!(c.on_slate());
        let o = c.select(&[none]);
        assert_eq!((o.decision, o.act), (Decision::Slate { copy: false }, 's'));
        // the first frame
        let o = c.select(&[tile(Some(fr(0)), None, 59.94)]);
        assert_eq!(o.act, 'N');
        assert!(!c.on_slate());
        assert_eq!(c.select(&[tile(Some(fr(0)), Some(fr(0)), 59.94)]).act, 'A');
        // stale: the slate again; the frame is still there
        let stale = TileIn {
            stale: true,
            ..tile(Some(fr(0)), Some(fr(0)), 59.94)
        };
        assert_eq!(c.select(&[stale]).act, 'S');
        assert_eq!(c.select(&[stale]).act, 's');
        // back with the very same frame: it is converted again (the target held the slate)
        let o = c.select(&[tile(Some(fr(0)), Some(fr(0)), 59.94)]);
        assert_eq!((o.act, o.src_top, o.src_bot), ('N', 0, 0));
        // back after a gap: nothing counts as dropped across the slate
        assert_eq!(c.select(&[stale]).act, 'S');
        assert_eq!(c.select(&[tile(Some(fr(40)), Some(fr(0)), 59.94)]).act, 'N');
        let k = c.counters;
        assert_eq!(
            (k.frames, k.slates, k.fresh, k.again, k.drops),
            (9, 5, 3, 1, 0)
        );
        assert_eq!(c.npts(), 9);
        // the warm-up reset keeps npts and the state
        c.reset_counters();
        assert_eq!(c.counters, Counters::default());
        let o = c.select(&[tile(Some(fr(40)), Some(fr(40)), 59.94)]);
        assert_eq!((o.act, o.n, o.pts), ('A', 9, 9 * 90000 * 1001 / 60000));
        assert_eq!((c.counters.frames, c.counters.again), (1, 1));

        // a channel without tiles shows the slate for good
        let mut c = Channel::new(ChannelCfg {
            tiles: 0,
            ..cfg(704, 480, 30000, true)
        });
        assert_eq!(acts(&[c.select(&[]), c.select(&[]), c.select(&[])]), "Sss");
        assert_eq!(c.counters.slates, 3);
    }

    #[test]
    fn slate_exit_on_a_bobbed_frame_continues_its_field_sequence() {
        // ported as it is: the bob state survives the slate
        let mut c = chan((1280, 720, 60000, false));
        let f = fi(3, true, 1920, 1080);
        let t = tile(Some(f), None, 29.97);
        assert_eq!(jobs(&c.select(&[t])).1[0].srows, Rows::Top);
        assert_eq!(c.select(&[TileIn { stale: true, ..t }]).act, 'S');
        assert_eq!(jobs(&c.select(&[t])).1[0].srows, Rows::Bottom);
        // both fields shown: a repeat of what the target holds
        assert_eq!(c.select(&[t]).act, 'A');
    }

    #[test]
    fn host_path() {
        let mut c = Channel::new(ChannelCfg {
            host: true,
            ..cfg(1920, 1080, 30000, true)
        });
        let t = |a: Option<i64>| tile(a.map(fr), None, 29.97);
        // nothing yet: the slate once, then repeats
        let o = c.select(&[t(None)]);
        assert_eq!(
            (o.decision, o.act, o.src_top, o.src_bot),
            (Decision::Slate { copy: true }, 'S', -2, -2)
        );
        assert_eq!(c.select(&[t(None)]).act, 'A');
        assert!(!c.on_slate());
        let o = c.select(&[t(Some(0))]);
        assert_eq!((o.decision, o.act), (Decision::Host { frame: fr(0) }, 'H'));
        assert_eq!((o.src_top, o.src_bot), (0, 0));
        assert_eq!(c.select(&[t(Some(0))]).decision, Decision::Again);
        assert_eq!(c.select(&[t(Some(1))]).act, 'H');
        // frames 2 to 4 never shown
        assert_eq!(c.select(&[t(Some(5))]).act, 'H');
        assert_eq!(c.counters.drops, 3);
        // stale does not matter on the host path
        let stale = TileIn {
            stale: true,
            ..t(Some(5))
        };
        assert_eq!(c.select(&[stale]).act, 'A');
        let k = c.counters;
        assert_eq!((k.frames, k.fresh, k.again, k.slates), (7, 3, 3, 1));
    }

    #[test]
    fn drops_are_counted_when_the_input_skips_frames() {
        // progressive output: every frame between two shown ones is a drop
        let mut c = chan((1280, 720, 60000, false));
        let t = |now: i64, prev: i64| tile(Some(fr(now)), Some(fr(prev)), 59.94);
        c.select(&[t(0, 0)]);
        c.select(&[t(1, 0)]);
        assert_eq!(c.counters.drops, 0);
        c.select(&[t(4, 1)]);
        assert_eq!(c.counters.drops, 2);

        // interlaced output, fields from two frames: the frame behind the top field was used
        let mut c = chan(i1080());
        let t = |now: i64, prev: i64| tile(Some(fr(now)), Some(fr(prev)), 59.94);
        c.select(&[t(1, 0)]);
        c.select(&[t(3, 2)]);
        assert_eq!(c.counters.drops, 0);
        // frames 4 and 5 lost: top from 6, bottom from 7
        c.select(&[t(7, 6)]);
        assert_eq!(c.counters.drops, 2);
        // top still the old frame: 8 and 9 lost, nothing used in between
        c.select(&[t(10, 7)]);
        assert_eq!(c.counters.drops, 4);
        // 50p into interlaced: no drops although `bot` advances by two now and then
        let mut c = chan(i1080());
        let mut s = Src::new(50.0, fr(0));
        run(&mut c, &mut s, 1000);
        assert_eq!(c.counters.drops, 0);
        assert_eq!(c.counters.fresh + c.counters.again, 500);
    }

    #[test]
    fn same_rate_rule_is_within_one_per_cent() {
        let c = chan(i1080());
        let t = |fps: f64| tile(Some(fr(1)), Some(fr(0)), fps);
        // 30.0 against 29.97: same rate, both fields from `now`
        assert_eq!(c.pick(&t(30.0)).0, Some(fr(1)));
        assert_eq!(c.pick(&t(29.97)).0, Some(fr(1)));
        assert_eq!(c.pick(&t(25.0)).0, Some(fr(0)));
        assert_eq!(c.pick(&t(59.94)).0, Some(fr(0)));
        // progressive output never takes `prev`
        let c = chan((1280, 720, 60000, false));
        assert_eq!(c.pick(&t(25.0)).0, Some(fr(1)));
        // without a previous frame the top field takes `now`
        let c = chan(i1080());
        assert_eq!(c.pick(&tile(Some(fr(1)), None, 59.94)).0, Some(fr(1)));
    }
}
