//! The video hub (ARCHITECTURE §8): one thread owns the encoder's device and its `Engine`, with
//! one session per program. At each output tick it asks every due program's frame
//! provider for the picture of that presentation time, submits, ticks, and hands the access
//! units to the carriers with their times moved onto each carrier's STC.
//!
//! Timing (SPEC §5.2): frame `k` of a program is presented at `T_k = P0 + k · period`
//! (process time) and is submitted at `T_k − Δ`. The encoder's own clock is the 90 kHz value of
//! `T_k`, so an access unit's PTS and DTS map to the carrier's STC as `stc0 + 300 · t`.

use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use tb_device::ArenaSizes;
use tb_engine::{Engine, TickDevice};
use tb_m2v_encoder::{Encoder, EncoderOptions, FrameSource, SessionConfig, SessionId};
use tb_mux::{VideoAu, VideoReconfig};
use tb_ts::Stc;

use crate::carrier::CarrierMsg;
use crate::config::{Format, ProgramPlan};
use crate::metrics::{Counter, Gauge};
use crate::overload::{Candidate, Governor, Step};
use crate::time::{Clock, HZ};

/// What a provider gave for a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picture {
    /// A new picture was written into the buffer.
    Fresh,
    /// The same picture as last time: the encoder repeats it without an upload.
    Repeat,
}

/// The source of a program's pictures (the fallback chain's output, SPEC §9.5).
pub trait FrameProvider: Send {
    /// Writes the picture to present at process time `t` into `out` (planar 4:2:0 at the
    /// program's size), or says it is unchanged.
    fn picture(&mut self, t: u64, out: &mut [u8]) -> Picture;
    /// Whether the slate is on air (the statmux pins such a program to its floor).
    fn on_slate(&self) -> bool {
        true
    }
    /// The Active Format Description of the picture handed out last (`tb_m2v_syntax::afd`;
    /// the slate fills the frame).
    fn active_format(&self) -> tb_m2v_syntax::afd::ActiveFormat {
        tb_m2v_syntax::afd::ActiveFormat::FULL
    }
    /// The program moves to another format: pictures of `w`×`h` from now on, at `cadence`.
    fn reformat(&mut self, _w: u32, _h: u32, _cadence: crate::source::Cadence) {}
    /// Film mode: what is shown at display unit time `t` (a field of an interlaced output, a
    /// frame period of a progressive one), as a whole picture of `len` bytes and its sameness
    /// key. `None`: the provider has no film mode, the program is coded frame by frame.
    fn shot(&mut self, _t: u64, _len: usize) -> Option<Shot> {
        None
    }
}

/// One display unit's content in film mode.
pub struct Shot {
    /// What it shows, for the assembler's sameness test.
    pub key: tb_timeline::film::Key,
    /// The whole picture (planar 4:2:0 at the program's size).
    pub data: std::sync::Arc<Vec<u8>>,
}

/// Rows of an interlaced picture: the top field's lines from `top`, the bottom field's from
/// `bottom` (whole pictures of the same size), into `out`.
fn weave(out: &mut [u8], w: usize, h: usize, top: &[u8], bottom: &[u8]) {
    let mut at = 0;
    for (pw, ph) in [(w, h), (w / 2, h / 2), (w / 2, h / 2)] {
        for row in 0..ph {
            let src = if row % 2 == 0 { top } else { bottom };
            let r = at + row * pw..at + (row + 1) * pw;
            if let (Some(d), Some(s)) = (out.get_mut(r.clone()), src.get(r)) {
                d.copy_from_slice(s);
            }
        }
        at += pw * ph;
    }
}

/// The statmux of the hub: one controller per carrier (SPEC §8.4).
pub struct StatmuxSetup {
    /// Settings.
    pub plan: crate::config::StatmuxPlan,
    /// Per carrier: the transport rate left for video.
    pub pools_ts: Vec<u64>,
    /// PCR interval, ms (for the transport overhead).
    pub pcr_ms: u64,
    /// Per carrier, its auto audio tracks: the pool grows by what they leave of their ceiling
    /// and shrinks before one is granted more (`crate::audio::CarrierFlex`).
    pub flex: Vec<std::sync::Arc<crate::audio::CarrierFlex>>,
}

/// A program's place in the hub.
pub struct HubSession {
    /// Index of its carrier in the plant.
    pub carrier: usize,
    /// The program.
    pub plan: ProgramPlan,
    /// Its carrier's STC offset.
    pub stc0: u64,
    /// Where its access units go.
    pub tx: SyncSender<CarrierMsg>,
    /// Its pictures.
    pub provider: Box<dyn FrameProvider>,
    /// Access units refused by a full carrier queue.
    pub dropped: Counter,
    /// Access units delivered.
    pub delivered: Counter,
    /// Captions to write into the pictures, if any.
    pub captioner: Option<tb_captions::Captioner>,
    /// The program at each rung of its ladder (`plan` is the one it is on).
    pub ladder: Vec<ProgramPlan>,
    /// Its rung now (metric).
    pub rung_gauge: Gauge,
    /// Format switches (metric).
    pub switches: Counter,
    /// Encoder sessions of earlier formats still delivering their last access units.
    pub old_ids: Vec<SessionId>,
    /// The encoder's counters for this program (metrics).
    pub encoder: EncoderStats,
    /// The program's context (service, carrier, program, channel) for its wide events.
    pub ctx: tracing::Span,
}

/// A program's encoder counters, over every session it has had (SPEC §11.3 "Program").
#[derive(Debug, Clone, Default)]
pub struct EncoderStats {
    /// Pictures coded on the device.
    pub pictures: Counter,
    /// Repeats made on the host.
    pub repeats: Counter,
    /// Pictures given up by the deadline policy (the overload signal).
    pub shed: Counter,
    /// Pictures whose plan failed.
    pub failed: Counter,
    /// Key frame positions that carried a repeat.
    pub keys_missed: Counter,
    /// Bits of all access units.
    pub bits: Counter,
    /// The delivery rate in force, bit/s.
    pub rate: Gauge,
}

impl EncoderStats {
    /// Registers them under `labels`.
    pub fn register(&self, reg: &mut crate::control::Registry, labels: &[(&'static str, String)]) {
        use crate::control::Value::{Counter as C, Gauge as G};
        let l = || labels.to_vec();
        reg.add("encoder_pictures_total", l(), C(self.pictures.clone()));
        reg.add("encoder_repeats_total", l(), C(self.repeats.clone()));
        reg.add("encoder_shed_total", l(), C(self.shed.clone()));
        reg.add("encoder_failed_total", l(), C(self.failed.clone()));
        reg.add(
            "encoder_keys_missed_total",
            l(),
            C(self.keys_missed.clone()),
        );
        reg.add("encoder_bits_total", l(), C(self.bits.clone()));
        reg.add("encoder_rate_bps", l(), G(self.rate.clone()));
    }

    /// Adds what a session's counters gained since `last` (that session's values when last
    /// seen; zeros for a new session).
    fn update(&self, st: &tb_m2v_encoder::SessionStats, rate: u64, last: &mut [u64; 6]) {
        let now = [
            st.pictures,
            st.repeats,
            st.shed,
            st.failed,
            st.keys_missed,
            st.total_bits as u64,
        ];
        let c = [
            &self.pictures,
            &self.repeats,
            &self.shed,
            &self.failed,
            &self.keys_missed,
            &self.bits,
        ];
        for ((c, n), l) in c.iter().zip(now).zip(last.iter_mut()) {
            c.add(n.saturating_sub(*l));
            *l = n;
        }
        self.rate.set(rate);
    }
}

/// Step-down settings and what the hub needs to carry a switch out.
#[derive(Debug, Clone, Copy)]
pub struct OverloadSetup {
    /// When to step.
    pub plan: crate::config::OverloadPlan,
    /// PCR interval, ms (for the transport rate of the new format).
    pub pcr_ms: u64,
}

/// The captioner of a program with a subtitle file: its cues on CC1 and CEA-708 service 1,
/// pop-on, times from the program's start. Parse errors and cues that cannot be sent in time
/// are logged.
pub fn captioner(p: &ProgramPlan) -> Option<tb_captions::Captioner> {
    use tb_captions::schedule::{Aspect, CaptionerCfg, Presentation, Track};
    let path = p.subtitles.as_ref()?;
    let (n, d) = p.format.fps();
    let rate = tb_m2v_syntax::profile::frame_rate_code(n, d)
        .and_then(tb_captions::FieldRate::of_frame_rate_code)?;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(path = %path.display(), "subtitles: {e}");
            return None;
        }
    };
    let vtt = text.trim_start_matches('\u{feff}').starts_with("WEBVTT");
    let parsed = if vtt {
        tb_captions::vtt::parse(&text)
    } else {
        tb_captions::srt::parse(&text)
    };
    let mut track = Track::primary(Presentation::PopOn);
    if let Some(t) = track.cea708.as_mut()
        && p.format.size().0 <= 720
    {
        t.aspect = Aspect::Standard;
    }
    let mut cfg = CaptionerCfg::new(vec![track]);
    cfg.rate = rate;
    let mut c = tb_captions::Captioner::new(cfg).ok()?;
    let rep = c.add_cues(0, &parsed.cues);
    for e in &parsed.errors {
        tracing::warn!(path = %path.display(), "subtitles: {e}");
    }
    for l in &rep.late {
        tracing::warn!(path = %path.display(), "subtitle cue late: {l:?}");
    }
    tracing::info!(path = %path.display(), cues = parsed.cues.len(), "subtitles loaded");
    Some(c)
}

/// The encoder session of a program (SPEC §8.2): closed GOPs of 0.5 s, two B pictures, the
/// ceiling in the sequence header, the buffer at `D = vbv / max`, constant target.
pub fn session_config(p: &ProgramPlan, phase: u32, capacity: u32) -> SessionConfig {
    let (w, h) = p.format.size();
    let (fps_n, fps_d) = p.format.fps();
    SessionConfig {
        w,
        h,
        fps_n,
        fps_d,
        interlaced: p.format.interlaced(),
        tff: true,
        gop: p.format.gop(),
        m: 3,
        gop_phase: phase,
        max_bitrate: p.max_bps,
        vbv_bits: p.vbv_bits,
        bitrate: p.max_bps,
        capacity,
        priority: p.priority,
        ..SessionConfig::default()
    }
}

/// The encoder options of the server.
pub fn encoder_options() -> EncoderOptions {
    EncoderOptions {
        split_plans: true,
        ..EncoderOptions::default()
    }
}

/// Sessions' configurations, key frames staggered across the GOP (SPEC §8.4).
pub fn session_configs(plans: &[&ProgramPlan]) -> Vec<SessionConfig> {
    let n = plans.len().max(1) as u32;
    plans
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let same = plans.iter().filter(|q| q.format == p.format).count() as u32;
            let gop = p.format.gop();
            session_config(p, (i as u32 * gop / n) % gop, same)
        })
        .collect()
}

/// The arenas a set of sessions needs, measured by opening them in a probe encoder, with a
/// quarter more for batches made at run time.
pub fn arena_sizes(cfgs: &[SessionConfig]) -> Result<ArenaSizes, tb_m2v_encoder::EncoderError> {
    const HUGE: u64 = 1 << 56;
    let mut e = Encoder::new(
        encoder_options(),
        ArenaSizes {
            device: HUGE,
            host: HUGE,
        },
    );
    for c in cfgs {
        e.open(c)?;
    }
    let (d, h) = e.free_bytes();
    let pad = |x: u64| x + x / 4 + (64 << 20);
    Ok(ArenaSizes {
        device: pad(HUGE - d),
        host: pad(HUGE - h),
    })
}

/// The hub's timing.
#[derive(Debug, Clone, Copy)]
pub struct HubTiming {
    /// Process time of frame 0 of every program.
    pub p0: u64,
    /// Δ: frames are submitted this long before they are presented.
    pub delay: u64,
    /// The epoch, for deadlines in real time (`None` offline: no deadline policy).
    pub epoch: Option<Instant>,
    /// Presentation time at which the programs end (offline runs): no frame is submitted
    /// for it or later, every session is closed so its last GOP is coded, and the streams end
    /// with `sequence_end_code`.
    pub end: Option<u64>,
}

/// H.262 §6.2.2: `sequence_end_code`.
const SEQUENCE_END: [u8; 4] = [0, 0, 1, 0xB7];

fn finish<D: TickDevice>(
    eng: &mut Engine<D>,
    sessions: &mut [HubSession],
    ids: &[SessionId],
) -> Result<(), String> {
    for &sid in ids {
        eng.enc_mut().close(sid).map_err(|e| e.to_string())?;
    }
    for _ in 0..64 {
        let done = ids
            .iter()
            .all(|&sid| eng.enc().is_closed(sid).unwrap_or(true));
        eng.tick(None).map_err(|e| e.to_string())?;
        for (i, s) in sessions.iter_mut().enumerate() {
            while let Some(au) = eng.enc_mut().take_output(ids[i]) {
                let mut data = au.data;
                if au.last {
                    data.extend_from_slice(&SEQUENCE_END);
                }
                let v = VideoAu {
                    data,
                    pts: Stc(s.stc0 + (au.pts.max(0) as u64) * 300),
                    dts: Stc(s.stc0 + (au.dts.max(0) as u64) * 300),
                    keyframe: au.keyframe,
                    reconfig: None,
                };
                match s.tx.try_send(CarrierMsg::Video(s.plan.index, v)) {
                    Ok(()) => s.delivered.inc(),
                    Err(_) => s.dropped.inc(),
                }
            }
        }
        if done {
            return Ok(());
        }
    }
    Err("sessions did not close".into())
}

struct Statmux {
    ctl: Vec<Option<tb_statmux::Controller>>,
    /// Per carrier: the flexible audio budget and its bookkeeping.
    flex: Vec<FlexState>,
    /// Session index of (carrier, program).
    sessions: Vec<Vec<usize>>,
    pcr_ms: u64,
    next_tick: u64,
}

fn run_commands(
    cmds: Vec<tb_statmux::Command>,
    carrier: usize,
    sm: &mut Statmux,
    sessions: &[HubSession],
    ids: &[SessionId],
    enc: &mut Encoder,
) {
    for c in cmds {
        match c {
            tb_statmux::Command::Encoder { program, bps } => {
                let Some(&si) = sm.sessions[carrier].get(program) else {
                    continue;
                };
                match enc.set_rate(ids[si], bps, false) {
                    Ok(t) => {
                        if let Some(ctl) = sm.ctl[carrier].as_mut() {
                            ctl.ticket(program, t);
                        }
                    }
                    Err(e) => tracing::warn!("statmux set_rate: {e}"),
                }
            }
            tb_statmux::Command::Mux { program, bps } => {
                let Some(&si) = sm.sessions[carrier].get(program) else {
                    continue;
                };
                let s = &sessions[si];
                let ts = s.plan.ts_bps_for(bps, sm.pcr_ms);
                if s.tx.try_send(CarrierMsg::VideoRate(program, ts)).is_err() {
                    tracing::error!("statmux: rate change not delivered (queue full)");
                }
            }
        }
    }
}

/// A carrier's auto audio tracks as the statmux sees them.
struct FlexState {
    flex: std::sync::Arc<crate::audio::CarrierFlex>,
    /// The video pool with every auto track at its ceiling (the configuration's).
    base_ts: u64,
    /// The programs' fixed transport overhead (for the elementary pool).
    overhead_ts: u64,
    /// Per track: since when it has wanted less than it was granted.
    down_since: Vec<Option<u64>>,
    /// The elementary pool asked of the controller last.
    requested: u64,
}

/// A track that dropped to a cheaper mode gives its rate back after this long (its frames at
/// the old rate have left the mux by then).
const FLEX_RELEASE: u64 = HZ;

/// The elementary pool of a transport pool.
fn pool_es(pool_ts: u64, overhead_ts: u64) -> u64 {
    pool_ts.saturating_sub(overhead_ts) * 184 / 188
}

impl FlexState {
    /// The transport pool when the auto tracks hold `rates`.
    fn pool_ts(&self, rates: impl Iterator<Item = u64>) -> u64 {
        let spare: u64 = self
            .flex
            .tracks
            .iter()
            .zip(rates)
            .map(|(t, r)| t.ceiling_ts.saturating_sub(r))
            .sum();
        self.base_ts + spare
    }

    /// One pass (SPEC §8.4): a track wanting less is granted less after [`FLEX_RELEASE`] and
    /// the pool grows; a track wanting more makes the pool shrink and is granted when the
    /// controller has reached the smaller pool.
    fn step(&mut self, ctl: &mut tb_statmux::Controller, now: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        if self.flex.tracks.is_empty() {
            return;
        }
        let mut hold = Vec::with_capacity(self.flex.tracks.len());
        let mut ups = Vec::new();
        for (k, t) in self.flex.tracks.iter().enumerate() {
            let (w, mut g) = (t.want_ts.load(Relaxed), t.granted_ts.load(Relaxed));
            if w < g {
                let since = *self.down_since[k].get_or_insert(now);
                if now >= since + FLEX_RELEASE {
                    tracing::info!(
                        program = t.program + 1,
                        track = t.track + 1,
                        "audio rate given back to video: {} -> {} bit/s (transport)",
                        g,
                        w
                    );
                    t.granted_ts.store(w, Relaxed);
                    g = w;
                    self.down_since[k] = None;
                }
            } else {
                self.down_since[k] = None;
            }
            if w > g {
                ups.push(k);
            }
            hold.push(w.max(g));
        }
        let target = pool_es(self.pool_ts(hold.into_iter()), self.overhead_ts);
        if target != self.requested {
            match ctl.request_pool(target) {
                Ok(()) => self.requested = target,
                Err(e) => {
                    tracing::warn!("statmux: an audio track's wider mode does not fit: {e}");
                    return;
                }
            }
        }
        if !ups.is_empty() && ctl.pool() <= self.requested {
            for k in ups {
                let t = &self.flex.tracks[k];
                let w = t.want_ts.load(Relaxed);
                tracing::info!(
                    program = t.program + 1,
                    track = t.track + 1,
                    "audio rate granted from the video pool: {} -> {} bit/s (transport)",
                    t.granted_ts.load(Relaxed),
                    w
                );
                t.granted_ts.store(w, Relaxed);
            }
        }
    }
}

fn statmux_setup(setup: &StatmuxSetup, sessions: &[HubSession]) -> Result<Statmux, String> {
    let n = setup.pools_ts.len();
    let mut sm = Statmux {
        ctl: Vec::new(),
        flex: Vec::new(),
        sessions: vec![Vec::new(); n],
        pcr_ms: setup.pcr_ms,
        next_tick: 0,
    };
    for (si, s) in sessions.iter().enumerate() {
        if let Some(v) = sm.sessions.get_mut(s.carrier) {
            if v.len() <= s.plan.index {
                v.resize(s.plan.index + 1, usize::MAX);
            }
            v[s.plan.index] = si;
        }
    }
    for (c, pool_ts) in setup.pools_ts.iter().enumerate() {
        let progs: Vec<&HubSession> = sm.sessions[c]
            .iter()
            .filter_map(|&i| sessions.get(i))
            .collect();
        if progs.is_empty() {
            sm.ctl.push(None);
            sm.flex.push(FlexState {
                flex: Default::default(),
                base_ts: *pool_ts,
                overhead_ts: 0,
                down_since: Vec::new(),
                requested: 0,
            });
            continue;
        }
        // Elementary pool: the transport pool less each program's fixed overhead, at 184/188.
        let overhead: u64 = progs
            .iter()
            .map(|s| s.plan.ts_bps_for(0, setup.pcr_ms) + 1)
            .sum();
        let mut flex = FlexState {
            flex: setup.flex.get(c).cloned().unwrap_or_default(),
            base_ts: *pool_ts,
            overhead_ts: overhead,
            down_since: Vec::new(),
            requested: 0,
        };
        flex.down_since = vec![None; flex.flex.tracks.len()];
        // The pool starts with what the auto tracks leave of their ceilings.
        let start_ts = flex.pool_ts(
            flex.flex
                .tracks
                .iter()
                .map(|t| t.granted_ts.load(std::sync::atomic::Ordering::Relaxed)),
        );
        let pool_es = pool_es(start_ts, overhead);
        flex.requested = pool_es;
        let params: Vec<tb_statmux::ProgramParams> = progs
            .iter()
            .map(|s| tb_statmux::ProgramParams {
                min_bps: s.plan.min_bps,
                max_bps: s.plan.max_bps,
                weight: s.plan.weight,
            })
            .collect();
        let cfg = tb_statmux::Config {
            pool_bps: pool_es,
            alpha: setup.plan.alpha,
            max_step: setup.plan.max_step,
            hysteresis: setup.plan.hysteresis,
            smoothing: 0.3,
            delay: HZ / 2,
        };
        let (ctl, _) = tb_statmux::Controller::new(cfg, &params)
            .map_err(|e| format!("statmux carrier {c}: {e}"))?;
        sm.ctl.push(Some(ctl));
        sm.flex.push(flex);
    }
    Ok(sm)
}

/// Gives a session the Active Format Description `af` for the frames submitted from now on,
/// if it is not what it has.
fn set_afd<D: TickDevice>(
    eng: &mut Engine<D>,
    sid: SessionId,
    af: tb_m2v_syntax::afd::ActiveFormat,
    cur: &mut Option<tb_m2v_syntax::afd::ActiveFormat>,
) {
    if *cur == Some(af) {
        return;
    }
    match eng.enc_mut().set_active_format(sid, Some(af)) {
        Ok(()) => *cur = Some(af),
        Err(e) => tracing::warn!("active format: {e}"),
    }
}

/// Runs the hub until the clock stops. Returns why it ended.
pub fn run<D: TickDevice>(
    mut eng: Engine<D>,
    mut sessions: Vec<HubSession>,
    timing: HubTiming,
    statmux: Option<StatmuxSetup>,
    overload: Option<OverloadSetup>,
    clock: &dyn Clock,
) -> Result<(), String> {
    let plans: Vec<&ProgramPlan> = sessions.iter().map(|s| &s.plan).collect();
    let cfgs = session_configs(&plans);
    let mut ids: Vec<SessionId> = Vec::new();
    for c in &cfgs {
        ids.push(
            eng.enc_mut()
                .open(c)
                .map_err(|e| format!("open session: {e}"))?,
        );
    }
    // The tick is the shortest frame period; slower programs are due every `every` ticks.
    let period = |f: Format| {
        let (n, d) = f.fps();
        HZ * u64::from(d) / u64::from(n)
    };
    // (over every format a program may step to: the tick must divide them all)
    let tick = sessions
        .iter()
        .flat_map(|s| s.ladder.iter().map(|p| p.format).chain([s.plan.format]))
        .map(period)
        .min()
        .unwrap_or(HZ / 30);
    let mut every: Vec<u64> = sessions
        .iter()
        .map(|s| period(s.plan.format) / tick)
        .collect();
    // Step-down: GOP phases kept across switches, batch capacities per format (every program
    // that may use it), the governor, and the switches in progress.
    let phases: Vec<u32> = cfgs.iter().map(|c| c.gop_phase).collect();
    let capacity_of = {
        let mut v: Vec<(Format, u32)> = Vec::new();
        for s in &sessions {
            for p in &s.ladder {
                match v.iter_mut().find(|(f, _)| *f == p.format) {
                    Some((_, n)) => *n += 1,
                    None => v.push((p.format, 1)),
                }
            }
        }
        move |f: Format| v.iter().find(|(g, _)| *g == f).map_or(1, |(_, n)| *n)
    };
    let mut gov = overload
        .as_ref()
        .filter(|o| o.plan.enabled && sessions.iter().any(|s| s.ladder.len() > 1))
        .map(|o| Governor::new(o.plan, sessions.len(), timing.p0));
    let mut next_sample = timing.p0 + HZ;
    // the encoder counters: each session's values when last copied out
    let mut next_stats = timing.p0 + HZ;
    let mut enc_last: Vec<[u64; 6]> = vec![[0; 6]; sessions.len()];
    // old sessions still delivering their last access units
    let mut closing: Vec<SessionId> = Vec::new();
    // per program: the tick index at which its new session starts, while switching
    let mut resume: Vec<Option<u64>> = vec![None; sessions.len()];
    // per program: the mux parameters to send with the new session's first access unit
    let mut reconfig: Vec<Option<VideoReconfig>> = vec![None; sessions.len()];
    // The statmux: controllers made, their starting rates set before the first picture.
    let mut sm = match &statmux {
        Some(setup) => Some(statmux_setup(setup, &sessions)?),
        None => None,
    };
    if let Some(sm) = sm.as_mut() {
        for c in 0..sm.ctl.len() {
            let Some(ctl) = sm.ctl[c].as_ref() else {
                continue;
            };
            let n = sm.sessions[c].len();
            let mut cmds = Vec::new();
            for i in 0..n {
                if let Some((e, m)) = ctl.rates(i) {
                    cmds.push(tb_statmux::Command::Mux { program: i, bps: m });
                    cmds.push(tb_statmux::Command::Encoder { program: i, bps: e });
                }
            }
            run_commands(cmds, c, sm, &sessions, &ids, eng.enc_mut());
        }
    }
    let mut first = vec![true; sessions.len()];
    // The Active Format Description each session writes, as last given to the encoder.
    let mut afd: Vec<Option<tb_m2v_syntax::afd::ActiveFormat>> = vec![None; sessions.len()];
    let mut cc: Vec<u8> = Vec::with_capacity(96);
    // Film mode: per session the assembler of display units (fields of an interlaced output,
    // frame periods of a progressive one) into pictures with their display durations.
    let mut film: Vec<Option<tb_timeline::film::Assembler<std::sync::Arc<Vec<u8>>>>> = sessions
        .iter()
        .map(|s| {
            s.plan.film.then(|| {
                tb_timeline::film::Assembler::new(tb_timeline::film::FilmCfg {
                    interlaced: s.plan.format.interlaced(),
                    top_first: s.plan.format.interlaced(),
                    three: true,
                })
            })
        })
        .collect();
    let mut k: u64 = 0;
    loop {
        let t = timing.p0 + k * tick;
        let wake = t.saturating_sub(timing.delay);
        if !clock.sleep_until(wake) || timing.end.is_some_and(|e| t >= e) {
            return finish(&mut eng, &mut sessions, &ids);
        }
        let mut any = !closing.is_empty();
        for (i, s) in sessions.iter_mut().enumerate() {
            if let Some(r) = resume[i] {
                if k < r {
                    continue;
                }
                resume[i] = None;
                first[i] = true;
            }
            if !k.is_multiple_of(every[i]) {
                continue;
            }
            any = true;
            let sid = ids[i];
            if let Some(asm) = film[i].as_mut() {
                // Film mode: this frame period's units to the assembler; a picture when one
                // is decided (none in one tick of five with 3:2: nothing is due).
                let (w, h) = s.plan.format.size();
                let len = (w * h * 3 / 2) as usize;
                let per = period(s.plan.format);
                let upf: u64 = if s.plan.format.interlaced() { 2 } else { 1 };
                for u in 0..upf {
                    if let Some(shot) = s.provider.shot(t + u * per / upf, len) {
                        asm.push(tb_timeline::film::Unit {
                            key: shot.key,
                            data: shot.data,
                        });
                    }
                }
                let Some(pic) = asm.pull() else { continue };
                set_afd(&mut eng, sid, s.provider.active_format(), &mut afd[i]);
                let shown = tb_m2v_encoder::Shown {
                    units: pic.units,
                    top_first: pic.top_first,
                    progressive: pic.progressive,
                };
                let src = if pic.again && !first[i] {
                    FrameSource::Again
                } else {
                    let buf = eng.enc().frame_buffer(sid).map_err(|e| e.to_string())?;
                    let out = buf
                        .of_mut(eng.host_mut())
                        .ok_or("frame buffer outside the host arena")?;
                    match pic.second() {
                        Some(_) if s.plan.format.interlaced() => {
                            let (top, bottom) = pic.top_bottom();
                            weave(out, w as usize, h as usize, &top.data, &bottom.data);
                        }
                        _ => {
                            let d = &pic.first().data;
                            if d.len() == out.len() {
                                out.copy_from_slice(d);
                            }
                        }
                    }
                    FrameSource::Host
                };
                first[i] = false;
                let (fn_, fd) = s.plan.format.fps();
                let pts90 = (timing.p0 / 300) as i64
                    + (i128::from(pic.start) * 90_000 * i128::from(fd)
                        / (i128::from(fn_) * i128::from(upf as u8))) as i64;
                cc.clear();
                if let Some(capt) = s.captioner.as_mut() {
                    let frc = tb_m2v_syntax::profile::frame_rate_code(fn_, fd).unwrap_or(4);
                    let (count, _) =
                        tb_m2v_syntax::captions::shown_counts(frc, pic.units, upf as u8);
                    let tf = s.plan.format.interlaced().then_some(pic.top_first);
                    capt.picture((count / 10) as u8, tf, &mut cc);
                }
                if let Err(e) = eng.enc_mut().submit_shown(sid, src, pts90, &cc, shown) {
                    tracing::warn!(program = s.plan.service, "film submit: {e}");
                }
                continue;
            }
            let buf = eng.enc().frame_buffer(sid).map_err(|e| e.to_string())?;
            let pic = match buf.of_mut(eng.host_mut()) {
                Some(out) => s.provider.picture(t, out),
                None => return Err("frame buffer outside the host arena".into()),
            };
            set_afd(&mut eng, sid, s.provider.active_format(), &mut afd[i]);
            let src = if pic == Picture::Fresh || first[i] {
                FrameSource::Host
            } else {
                FrameSource::Again
            };
            first[i] = false;
            let pts90 = (t / 300) as i64;
            cc.clear();
            if let Some(capt) = s.captioner.as_mut()
                && let Ok(sh) = eng.enc().ordinary(sid)
            {
                let (n, d) = s.plan.format.fps();
                let frc = tb_m2v_syntax::profile::frame_rate_code(n, d).unwrap_or(4);
                let (count, _) = tb_m2v_syntax::captions::shown_counts(frc, sh.units, sh.units);
                let tf = s.plan.format.interlaced().then_some(sh.top_first);
                capt.picture((count / 10) as u8, tf, &mut cc);
            }
            if let Err(e) = eng.enc_mut().submit(sid, src, pts90, &cc) {
                tracing::warn!(program = s.plan.service, "submit: {e}");
            }
        }
        if any {
            // The deadline: 85 % of a tick after the wake (SPEC §14.x "typical" budget).
            let deadline = timing.epoch.map(|e| {
                e + Duration::from_nanos(
                    ((u128::from(wake) + u128::from(tick) * 85 / 100) * 1000 / 27) as u64,
                )
            });
            if let Err(e) = eng.tick(deadline) {
                tracing::error!("video tick: {e}");
            }
            for (i, s) in sessions.iter_mut().enumerate() {
                while let Some(au) = eng.enc_mut().take_output(ids[i]) {
                    if let Some(ctl) = sm
                        .as_mut()
                        .and_then(|m| m.ctl.get_mut(s.carrier))
                        .and_then(Option::as_mut)
                    {
                        let unit = if s.plan.format.interlaced() {
                            period(s.plan.format) / 2
                        } else {
                            period(s.plan.format)
                        };
                        ctl.access_unit(
                            s.plan.index,
                            au.bits,
                            au.avg_q,
                            u64::from(au.units) * unit,
                            (au.dts.max(0) as u64) * 300,
                            au.rate_ticket,
                        );
                        ctl.set_slate(s.plan.index, s.provider.on_slate());
                    }
                    let v = VideoAu {
                        data: au.data,
                        pts: Stc(s.stc0 + (au.pts.max(0) as u64) * 300),
                        dts: Stc(s.stc0 + (au.dts.max(0) as u64) * 300),
                        keyframe: au.keyframe,
                        reconfig: reconfig[i].take(),
                    };
                    match s.tx.try_send(CarrierMsg::Video(s.plan.index, v)) {
                        Ok(()) => s.delivered.inc(),
                        Err(_) => s.dropped.inc(),
                    }
                }
            }
            // The old sessions of programs that switched: their last access units, the last
            // with sequence_end_code, then they go.
            let mut still = Vec::new();
            for sid in std::mem::take(&mut closing) {
                let owner = sessions.iter().position(|s| s.old_ids.contains(&sid));
                while let Some(au) = eng.enc_mut().take_output(sid) {
                    let Some(s) = owner.and_then(|o| sessions.get(o)) else {
                        continue;
                    };
                    let mut data = au.data;
                    if au.last {
                        data.extend_from_slice(&SEQUENCE_END);
                    }
                    let v = VideoAu {
                        data,
                        pts: Stc(s.stc0 + (au.pts.max(0) as u64) * 300),
                        dts: Stc(s.stc0 + (au.dts.max(0) as u64) * 300),
                        keyframe: au.keyframe,
                        reconfig: None,
                    };
                    match s.tx.try_send(CarrierMsg::Video(s.plan.index, v)) {
                        Ok(()) => s.delivered.inc(),
                        Err(_) => s.dropped.inc(),
                    }
                }
                if eng.enc().is_closed(sid).unwrap_or(true) {
                    let _ = eng.enc_mut().release(sid);
                    if let Some(s) = owner.and_then(|o| sessions.get_mut(o)) {
                        s.old_ids.retain(|&x| x != sid);
                    }
                } else {
                    still.push(sid);
                }
            }
            closing = still;
        }
        // The statmux: lowerings complete as the mux reaches them; allocation once per 0.5 s.
        if let Some(m) = sm.as_mut() {
            let tick_now = wake >= m.next_tick;
            if tick_now {
                m.next_tick = wake + HZ / 2;
            }
            for c in 0..m.ctl.len() {
                let cmds = match m.ctl[c].as_mut() {
                    Some(ctl) if tick_now => ctl.tick(wake),
                    Some(ctl) => ctl.poll(wake),
                    None => continue,
                };
                if !cmds.is_empty() {
                    run_commands(cmds, c, m, &sessions, &ids, eng.enc_mut());
                }
                // After the commands: a grant is made only once the mux has been lowered.
                if let (Some(ctl), Some(f)) = (m.ctl[c].as_mut(), m.flex.get_mut(c)) {
                    f.step(ctl, wake);
                }
            }
        }
        // The encoder's counters, once a second.
        if wake >= next_stats {
            next_stats = wake + HZ;
            for (i, s) in sessions.iter().enumerate() {
                if let Ok(sn) = eng.enc().snapshot(ids[i]) {
                    s.encoder.update(&sn.stats, sn.rate, &mut enc_last[i]);
                }
            }
        }
        // Step-down: once a second, the counters to the governor, and its decision carried out.
        if let (Some(g), Some(setup)) = (gov.as_mut(), overload.as_ref())
            && wake >= next_sample
        {
            next_sample = wake + HZ;
            for (i, sid) in ids.iter().enumerate() {
                if resume[i].is_none()
                    && let Ok(sn) = eng.enc().snapshot(*sid)
                {
                    g.sample(wake, i, sn.stats.shed, sn.stats.pictures + sn.stats.repeats);
                }
            }
            let cands: Vec<Candidate> = sessions
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let (w, h) = s.plan.format.size();
                    let (n, d) = s.plan.format.fps();
                    Candidate {
                        priority: s.plan.priority,
                        rung: s.plan.rung.get(),
                        rungs: s.ladder.len(),
                        mbs: u64::from(w.div_ceil(16) * h.div_ceil(16)) * u64::from(n)
                            / u64::from(d),
                        switching: resume[i].is_some(),
                    }
                })
                .collect();
            if let Some(step) = g.decide(wake, &cands) {
                let (i, to) = match step {
                    Step::Down(i) => (i, sessions[i].plan.rung.get() + 1),
                    Step::Up(i) => (i, sessions[i].plan.rung.get().saturating_sub(1)),
                };
                // the new session first: if it cannot be opened, nothing changes
                let new_plan = sessions[i].ladder[to].clone();
                let cfg = session_config(&new_plan, phases[i], capacity_of(new_plan.format));
                let down = matches!(step, Step::Down(_));
                let step_ev = tb_telemetry::Wide::child_of(&sessions[i].ctx, "video.step")
                    .with("direction", if down { "down" } else { "up" })
                    .with("from", sessions[i].plan.format.name())
                    .with("to", new_plan.format.name())
                    .with("rung", to);
                step_ev.level(if down {
                    tracing::Level::WARN
                } else {
                    tracing::Level::INFO
                });
                match eng.enc_mut().open(&cfg) {
                    Err(e) => {
                        step_ev.error(format!("the new session was refused: {e}"));
                    }
                    Ok(new_sid) => {
                        let s = &mut sessions[i];
                        enc_last[i] = [0; 6];
                        let old = std::mem::replace(&mut ids[i], new_sid);
                        if let Err(e) = eng.enc_mut().close(old) {
                            tracing::warn!("closing the old session: {e}");
                        }
                        s.old_ids.push(old);
                        closing.push(old);
                        // the rate the new stream starts at, and its mux parameters
                        let es = match sm
                            .as_mut()
                            .and_then(|m| m.ctl.get_mut(s.carrier))
                            .and_then(Option::as_mut)
                        {
                            Some(ctl) => ctl
                                .restart(
                                    s.plan.index,
                                    tb_statmux::ProgramParams {
                                        min_bps: new_plan.min_bps,
                                        max_bps: new_plan.max_bps,
                                        weight: new_plan.weight,
                                    },
                                )
                                .unwrap_or(new_plan.min_bps),
                            None => new_plan.max_bps,
                        };
                        if es != new_plan.max_bps
                            && let Err(e) = eng.enc_mut().set_rate(new_sid, es, false)
                        {
                            tracing::warn!("step: set_rate: {e}");
                        }
                        let d_new = new_plan.vbv_bits * HZ / new_plan.max_bps;
                        reconfig[i] = Some(VideoReconfig {
                            rx_bps: new_plan.format.rx_bps(),
                            buffer_delay: d_new,
                            rate_bps: new_plan.ts_bps_for(es, setup.pcr_ms),
                        });
                        // The gap: the new stream's first picture enters the decoder's buffer
                        // no earlier than its DTS − D (the mux's rule), which must not come
                        // before the old stream's last DTS (at most the time just submitted):
                        // D, two frame periods between the first PTS and DTS, and one more.
                        let per = period(new_plan.format);
                        let ev = (per / tick).max(1);
                        let after = (d_new + 3 * per).div_ceil(tick);
                        resume[i] = Some((k + after + 1).div_ceil(ev) * ev);
                        every[i] = ev;
                        afd[i] = None;
                        film[i] = new_plan.film.then(|| {
                            tb_timeline::film::Assembler::new(tb_timeline::film::FilmCfg {
                                interlaced: new_plan.format.interlaced(),
                                top_first: new_plan.format.interlaced(),
                                three: true,
                            })
                        });
                        let (w, h) = new_plan.format.size();
                        let (fn_, fd) = new_plan.format.fps();
                        s.provider.reformat(
                            w,
                            h,
                            crate::source::Cadence {
                                p0: timing.p0,
                                period: HZ * u64::from(fd) / u64::from(fn_),
                                gop: new_plan.format.gop(),
                                phase: phases[i],
                                delay: timing.delay,
                                units: if new_plan.film && new_plan.format.interlaced() {
                                    2
                                } else {
                                    1
                                },
                            },
                        );
                        step_ev.set("start_bps", es);
                        s.plan = new_plan;
                        s.plan.rung.set(to);
                        s.rung_gauge.set(to as u64);
                        s.switches.inc();
                        g.reset(i);
                    }
                }
                step_ev.emit();
            }
        }
        k += 1;
    }
}

#[cfg(test)]
mod flex_tests {
    use super::*;
    use crate::audio::{CarrierFlex, FlexTrack};
    use std::sync::Arc;
    use std::sync::atomic::Ordering::Relaxed;

    const STEREO: u64 = 235_000;
    const FIVE_ONE: u64 = 423_000;

    fn setup(max_bps: u64) -> (FlexState, tb_statmux::Controller) {
        let flex = Arc::new(CarrierFlex {
            tracks: vec![FlexTrack::new(0, 0, FIVE_ONE, STEREO)],
        });
        let mut f = FlexState {
            flex,
            base_ts: 30_000_000,
            overhead_ts: 0,
            down_since: vec![None],
            requested: 0,
        };
        f.requested = pool_es(f.pool_ts([STEREO].into_iter()), 0);
        let cfg = tb_statmux::Config {
            pool_bps: f.requested,
            alpha: 0.85,
            max_step: 0.15,
            hysteresis: 0.03,
            smoothing: 0.3,
            delay: HZ / 2,
        };
        let p = tb_statmux::ProgramParams {
            min_bps: 2_000_000,
            max_bps,
            weight: 1.0,
        };
        let (ctl, _) = tb_statmux::Controller::new(cfg, &[p, p]).unwrap();
        (f, ctl)
    }

    /// Video uses all of the pool: a wider mode waits until the controller has made room.
    #[test]
    fn a_wider_mode_waits_for_the_room() {
        let (mut f, mut ctl) = setup(50_000_000);
        let flex = f.flex.clone();
        let t = &flex.tracks[0];
        let before = ctl.pool();
        t.want_ts.store(FIVE_ONE, Relaxed);
        f.step(&mut ctl, 0);
        assert_eq!(
            t.granted_ts.load(Relaxed),
            STEREO,
            "not before the mux is lowered"
        );
        assert_eq!(ctl.pool(), before);
        assert_eq!(
            f.requested,
            pool_es(30_000_000, 0),
            "the pool asked for leaves the track its ceiling"
        );
    }

    /// Video leaves room: granted in the same pass; and a narrower mode gives the rate back
    /// only after FLEX_RELEASE, growing the pool.
    #[test]
    fn room_is_granted_at_once_and_given_back_later() {
        let (mut f, mut ctl) = setup(5_000_000);
        let flex = f.flex.clone();
        let t = &flex.tracks[0];
        t.want_ts.store(FIVE_ONE, Relaxed);
        f.step(&mut ctl, 0);
        assert_eq!(t.granted_ts.load(Relaxed), FIVE_ONE);
        let small = ctl.pool();
        t.want_ts.store(STEREO, Relaxed);
        f.step(&mut ctl, HZ / 2);
        assert_eq!(
            t.granted_ts.load(Relaxed),
            FIVE_ONE,
            "held while its frames drain"
        );
        f.step(&mut ctl, HZ / 2 + FLEX_RELEASE);
        assert_eq!(t.granted_ts.load(Relaxed), STEREO);
        assert!(ctl.pool() > small);
    }
}
