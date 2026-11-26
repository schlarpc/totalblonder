//! The multiplexer of one carrier (SPEC §6).
//!
//! Each call to [`Mux::next_slot`] fills the next slot with the first of these that has something to
//! send (SPEC §6.3), a later class never delaying an earlier one and each class bounded by its
//! own model so it cannot starve the classes below:
//!
//! 1. **PCR due** for a program (`pcr_interval` after its last): on that program's next
//!    video packet if one may go now, else on an adaptation-only packet of the video PID;
//! 2. **tables**, from the carousel ([`crate::carousel`]);
//! 3. **audio**: a frame may start `lead` before its PTS, if its transport buffer (512 bytes at
//!    2 Mbit/s) and the AC-3 main buffer (2,592 bytes; A/53-5 §5.7) admit it; earliest PTS
//!    first;
//! 4. **video**: an access unit may start at `DTS − D` (never earlier: that is the encoder's
//!    buffer contract, `tb_m2v_rate::vbv` and `notes/film-mode-log.md`), delivered through a
//!    token bucket at the program's delivery rate and admitted by its transport buffer;
//!    earliest DTS first;
//! 5. **null**.
//!
//! An access unit that can no longer be delivered by its decoding time is dropped, and with it
//! every picture up to the next key frame (SPEC §6.6). Late audio frames are dropped alone.

use std::collections::VecDeque;

use tb_psip::build::TableSet;
use tb_ts::packet::Adaptation;
use tb_ts::pes::{self, Cursor, StreamId, VideoStreamNumber};
use tb_ts::{Cc, Packet, Pid, STC_HZ, Stc};

use crate::bucket::{Leaky, Tokens};
use crate::carousel::{Carousel, Periods, SttConfig, TableStat, WallClock};
use crate::clock::{SlotClock, TsRate};

const MS: u64 = STC_HZ / 1000;

/// A program's video stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfig {
    /// The PID (also the PCR PID).
    pub pid: Pid,
    /// The transport buffer's drain rate Rx, bit/s: 1.2 × Rmax of the profile and level for
    /// MPEG-2 (96 Mbit/s at MP@HL, 18 Mbit/s at MP@ML; Rec. §2.4.2.3), 1.2 × the CPB rate for
    /// H.264.
    pub rx_bps: u64,
    /// Delivery rate at transport level, bit/s: the token bucket's rate. Must cover the
    /// encoder's rate plus packetization ([`ts_rate_for`]).
    pub rate_bps: u64,
    /// The encoder's buffer delay D, ticks: an access unit enters the decoder's buffer no
    /// earlier than `DTS − D`.
    pub buffer_delay: u64,
    /// Set `elementary_stream_priority_indicator` on the first packet of a key frame
    /// (SCTE 128-2 §6.4.2.1 for H.264).
    pub es_priority_on_key: bool,
}

/// One audio stream of a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// The PID.
    pub pid: Pid,
    /// How long before its PTS a frame may start, ticks (SPEC §6.3: 40 ms).
    pub lead: u64,
    /// The decoder's main buffer, bytes: 2,592 for AC-3 (A/53-5 §5.7; A/52 Annex A §A4.4).
    pub buffer_bytes: u64,
}

impl AudioConfig {
    /// AC-3 on `pid` with SPEC's defaults.
    pub fn ac3(pid: Pid) -> Self {
        AudioConfig {
            pid,
            lead: 40 * MS,
            buffer_bytes: 2592,
        }
    }
}

/// One program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramConfig {
    /// Its program number (to find its PMT in table sets).
    pub number: u16,
    /// Video.
    pub video: VideoConfig,
    /// Audio streams.
    pub audio: Vec<AudioConfig>,
    /// PCR interval, ticks (SPEC §6.3: 30 ms; at most 100 ms).
    pub pcr_interval: u64,
}

/// The mux of one carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxConfig {
    /// The constant output rate.
    pub rate: TsRate,
    /// STC of slot 0.
    pub stc0: Stc,
    /// Programs.
    pub programs: Vec<ProgramConfig>,
    /// Table periods.
    pub periods: Periods,
    /// STT settings; `None` sends no STT.
    pub stt: Option<SttConfig>,
    /// Access units held per stream before pushes are refused.
    pub queue_limit: usize,
    /// Depth of the video token buckets, packets (SPEC §6.3: 2).
    pub token_depth: u64,
}

/// A configuration the mux refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A PID is reserved or used twice.
    #[error("PID {0} reserved or used twice")]
    Pid(Pid),
    /// The PCR interval is zero or above 100 ms (Rec. §2.7.2).
    #[error("PCR interval must be in (0, 100 ms]")]
    PcrInterval,
    /// The video delivery rates exceed the carrier rate.
    #[error("video rates {0} bit/s exceed the carrier rate {1} bit/s")]
    Overbooked(u64, u64),
    /// A rate is zero.
    #[error("zero rate")]
    ZeroRate,
}

/// The transport-level rate needed to deliver `es_bps` of video in access units of
/// `au_per_sec` (numerator/denominator): payload at 188/184, plus per access unit a PES header
/// (19 bytes) and up to one packet of stuffing, plus a PCR adaptation field (8 bytes) every
/// `pcr_ms`. A conservative bound, not an estimate.
pub fn ts_rate_for(es_bps: u64, au_num: u64, au_den: u64, pcr_ms: u64) -> u64 {
    let payload = (u128::from(es_bps) * 188).div_ceil(184);
    let per_au = u128::from(au_num) * (19 + 184) * 8 * 188 / 184;
    let per_au = per_au.div_ceil(u128::from(au_den.max(1)));
    let pcr = (8u128 * 8 * 1000).div_ceil(u128::from(pcr_ms.max(1)));
    (payload + per_au + pcr) as u64
}

/// One coded picture with times on the carrier's STC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoAu {
    /// The bytes of the access unit.
    pub data: Vec<u8>,
    /// Presentation time.
    pub pts: Stc,
    /// Decoding time (equal to `pts` for B pictures and when there is no reordering).
    pub dts: Stc,
    /// A random access point (closed-GOP I picture with sequence header; IDR).
    pub keyframe: bool,
}

/// One coded audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioAu {
    /// The bytes of the frame.
    pub data: Vec<u8>,
    /// Presentation time.
    pub pts: Stc,
}

/// Why an access unit was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    /// No such program or track.
    #[error("no such stream")]
    NoStream,
    /// The queue is full.
    #[error("queue full")]
    Full,
    /// Decoding times must increase and DTS ≤ PTS < DTS + 1 s.
    #[error("time stamps out of order")]
    Order,
    /// Empty or too long for a PES packet.
    #[error("bad access unit: {0}")]
    Pes(#[from] tb_ts::pes::PesError),
}

/// Counters of one program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgramStats {
    /// Video packets sent.
    pub video_packets: u64,
    /// Adaptation-only PCR packets sent.
    pub pcr_only_packets: u64,
    /// Audio packets sent.
    pub audio_packets: u64,
    /// Access units dropped as late (and the pictures after them up to a key frame).
    pub video_dropped: u64,
    /// Audio frames dropped as late.
    pub audio_dropped: u64,
    /// Video access units fully sent.
    pub video_aus: u64,
    /// Audio frames fully sent.
    pub audio_aus: u64,
    /// Longest PCR interval, ticks.
    pub max_pcr_gap: u64,
    /// Smallest margin between the end of an access unit's delivery and its DTS, ticks.
    pub min_dts_margin: Option<u64>,
}

/// Counters of the carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MuxStats {
    /// Slots produced.
    pub slots: u64,
    /// Slots skipped because the pacer went ahead.
    pub skipped: u64,
    /// Table packets.
    pub table_packets: u64,
    /// Null packets.
    pub null_packets: u64,
}

struct VideoState {
    cfg: VideoConfig,
    queue: VecDeque<VideoAu>,
    cur: Option<(Cursor<Vec<u8>>, Stc, bool)>,
    cc: Cc,
    tb: Leaky,
    tokens: Tokens,
    last_pcr: Option<u64>,
    dropping: bool,
    last_dts: Option<Stc>,
}

struct AudioState {
    cfg: AudioConfig,
    queue: VecDeque<AudioAu>,
    cur: Option<(Cursor<Vec<u8>>, Stc)>,
    cc: Cc,
    tb: Leaky,
    /// Frames in the decoder's buffer model: (removal time, bytes).
    held: VecDeque<(u64, u64)>,
    last_pts: Option<Stc>,
}

struct ProgramState {
    number: u16,
    pcr_interval: u64,
    video: VideoState,
    audio: Vec<AudioState>,
    stats: ProgramStats,
}

/// The multiplexer of one carrier.
pub struct Mux {
    clock: SlotClock,
    slot: u64,
    programs: Vec<ProgramState>,
    carousel: Carousel,
    wall: Option<WallClock>,
    queue_limit: usize,
    stats: MuxStats,
}

fn reserved(p: Pid) -> bool {
    p.get() < 0x0010
        || p == Pid::PSIP_BASE
        || p.get() >= 0x1FFE
        || (0x1D00..=0x1E80).contains(&p.get())
}

impl Mux {
    /// A mux at slot 0.
    pub fn new(cfg: MuxConfig) -> Result<Mux, ConfigError> {
        let mut seen = Vec::new();
        let mut video_total = 0u64;
        for p in &cfg.programs {
            if p.pcr_interval == 0 || p.pcr_interval > 100 * MS {
                return Err(ConfigError::PcrInterval);
            }
            if p.video.rate_bps == 0 || p.video.rx_bps == 0 {
                return Err(ConfigError::ZeroRate);
            }
            video_total += p.video.rate_bps;
            for pid in std::iter::once(p.video.pid).chain(p.audio.iter().map(|a| a.pid)) {
                if reserved(pid) || seen.contains(&pid) {
                    return Err(ConfigError::Pid(pid));
                }
                seen.push(pid);
            }
        }
        if video_total > cfg.rate.bps() {
            return Err(ConfigError::Overbooked(video_total, cfg.rate.bps()));
        }
        let clock = SlotClock::new(cfg.rate, cfg.stc0);
        let t0 = cfg.stc0.0;
        let programs = cfg
            .programs
            .iter()
            .map(|p| ProgramState {
                number: p.number,
                pcr_interval: p.pcr_interval,
                video: VideoState {
                    cfg: p.video.clone(),
                    queue: VecDeque::new(),
                    cur: None,
                    cc: Cc::new(0),
                    tb: Leaky::new(512, p.video.rx_bps, t0),
                    tokens: Tokens::new(p.video.rate_bps, cfg.token_depth.max(1), t0),
                    last_pcr: None,
                    dropping: false,
                    last_dts: None,
                },
                audio: p
                    .audio
                    .iter()
                    .map(|a| AudioState {
                        cfg: a.clone(),
                        queue: VecDeque::new(),
                        cur: None,
                        cc: Cc::new(0),
                        tb: Leaky::new(512, 2_000_000, t0),
                        held: VecDeque::new(),
                        last_pts: None,
                    })
                    .collect(),
                stats: ProgramStats::default(),
            })
            .collect();
        Ok(Mux {
            clock,
            slot: 0,
            programs,
            carousel: Carousel::new(cfg.periods, cfg.programs.len(), cfg.stt),
            wall: None,
            queue_limit: cfg.queue_limit.max(1),
            stats: MuxStats::default(),
        })
    }

    /// The slot clock.
    pub fn clock(&self) -> &SlotClock {
        &self.clock
    }

    /// The next slot [`Mux::next_slot`] will fill.
    pub fn slot(&self) -> u64 {
        self.slot
    }

    /// Carrier counters.
    pub fn stats(&self) -> MuxStats {
        self.stats
    }

    /// Counters of program `i`.
    pub fn program_stats(&self, i: usize) -> Option<ProgramStats> {
        self.programs.get(i).map(|p| p.stats)
    }

    /// Per-table statistics of the carousel.
    pub fn table_stats(&self) -> Vec<TableStat> {
        self.carousel.stats()
    }

    /// Table instances that fell due before the previous one had started.
    pub fn table_overruns(&self) -> u64 {
        self.carousel.overruns
    }

    /// Replaces the tables (atomically at section boundaries).
    pub fn set_tables(&mut self, set: &TableSet) {
        let now = self.clock.stc(self.slot).0;
        let numbers: Vec<u16> = self.programs.iter().map(|p| p.number).collect();
        self.carousel
            .set(set, now, |n| numbers.iter().position(|&m| m == n));
    }

    /// Sets the STC-to-UTC relation used for the STT.
    pub fn set_wall_clock(&mut self, w: WallClock) {
        self.wall = Some(w);
    }

    /// Changes program `i`'s video delivery rate from the current slot on (SPEC §8.5 says
    /// in which order relative to the encoder).
    pub fn set_video_rate(&mut self, i: usize, bps: u64) {
        let now = self.clock.stc(self.slot).0;
        if let Some(p) = self.programs.get_mut(i) {
            p.video.tokens.set_rate(now, bps);
            p.video.cfg.rate_bps = bps;
        }
    }

    /// Nothing queued or in progress on any stream: every access unit given has been sent.
    pub fn idle(&self) -> bool {
        self.programs.iter().all(|p| {
            p.video.queue.is_empty()
                && p.video.cur.is_none()
                && p.audio
                    .iter()
                    .all(|a| a.queue.is_empty() && a.cur.is_none())
        })
    }

    /// Queued video of program `i`: (access units, bytes).
    pub fn video_queue(&self, i: usize) -> Option<(usize, usize)> {
        self.programs.get(i).map(|p| {
            (
                p.video.queue.len(),
                p.video.queue.iter().map(|a| a.data.len()).sum(),
            )
        })
    }

    /// Queues a video access unit of program `i`.
    pub fn push_video(&mut self, i: usize, au: VideoAu) -> Result<(), PushError> {
        let v = &mut self.programs.get_mut(i).ok_or(PushError::NoStream)?.video;
        if v.queue.len() >= self.queue_limit {
            return Err(PushError::Full);
        }
        if au.dts > au.pts
            || au.pts.0 - au.dts.0 >= STC_HZ
            || v.last_dts.is_some_and(|d| au.dts <= d)
        {
            return Err(PushError::Order);
        }
        if au.data.is_empty() {
            return Err(PushError::Pes(tb_ts::pes::PesError::Empty));
        }
        v.last_dts = Some(au.dts);
        v.queue.push_back(au);
        Ok(())
    }

    /// Queues an audio frame of program `i`, track `t`.
    pub fn push_audio(&mut self, i: usize, t: usize, au: AudioAu) -> Result<(), PushError> {
        let a = self
            .programs
            .get_mut(i)
            .and_then(|p| p.audio.get_mut(t))
            .ok_or(PushError::NoStream)?;
        if a.queue.len() >= self.queue_limit {
            return Err(PushError::Full);
        }
        if a.last_pts.is_some_and(|p| au.pts <= p) {
            return Err(PushError::Order);
        }
        // Checks the length now, so the scheduler never meets an unwritable frame.
        pes::header(
            StreamId::PrivateStream1,
            au.pts.ts90k(),
            None,
            au.data.len(),
        )?;
        a.last_pts = Some(au.pts);
        a.queue.push_back(au);
        Ok(())
    }

    /// Starts the output at slot `n` (before the first packet is produced): the slots before
    /// it are never sent. Not counted as skipped.
    pub fn start_at(&mut self, n: u64) {
        if self.stats.slots == 0 {
            self.slot = n;
            self.carousel.restart(self.clock.stc(n).0);
        }
    }

    /// Moves to slot `n` without producing the slots in between (the pacer sent nulls in
    /// their place). Never moves backwards.
    pub fn skip_to(&mut self, n: u64) {
        if n > self.slot {
            self.stats.skipped += n - self.slot;
            self.slot = n;
        }
    }

    /// The packet of the next slot, and that slot's index.
    pub fn next_slot(&mut self) -> (u64, Packet) {
        let n = self.slot;
        self.slot += 1;
        self.stats.slots += 1;
        let now = self.clock.stc(n).0;
        let pcr = self.clock.pcr_stc(n);
        let pkt = self
            .pcr(now, pcr)
            .or_else(|| {
                let p = self.carousel.next_packet(now, self.wall.as_ref());
                if p.is_some() {
                    self.stats.table_packets += 1;
                }
                p
            })
            .or_else(|| self.audio(now))
            .or_else(|| self.video(now, None))
            .unwrap_or_else(|| {
                self.stats.null_packets += 1;
                Packet::null()
            });
        (n, pkt)
    }

    fn pcr(&mut self, now: u64, pcr: Stc) -> Option<Packet> {
        let slot = self.clock.slot_ticks();
        let due = self
            .programs
            .iter()
            .enumerate()
            .filter_map(|(i, p)| match p.video.last_pcr {
                None => Some((0, i)),
                // Due one slot early, so the interval between PCR values never exceeds it.
                Some(l) if now + slot + 64 >= l + p.pcr_interval => Some((l, i)),
                _ => None,
            })
            .min()?;
        let i = due.1;
        let pkt = self.video(now, Some((i, pcr))).or_else(|| {
            let p = &mut self.programs[i];
            if !p.video.tb.admits(now, 188) {
                return None;
            }
            p.video.tb.add(now, 188);
            p.stats.pcr_only_packets += 1;
            Some(Packet::adaptation_only(
                p.video.cfg.pid,
                p.video.cc.prev(),
                &Adaptation::pcr(pcr.pcr()),
            ))
        })?;
        let p = &mut self.programs[i];
        if let Some(l) = p.video.last_pcr {
            p.stats.max_pcr_gap = p.stats.max_pcr_gap.max(pcr.0 - l);
        }
        p.video.last_pcr = Some(pcr.0);
        Some(pkt)
    }

    fn audio(&mut self, now: u64) -> Option<Packet> {
        let mut best: Option<(Stc, usize, usize)> = None;
        for (i, p) in self.programs.iter_mut().enumerate() {
            for (t, a) in p.audio.iter_mut().enumerate() {
                while a.held.front().is_some_and(|&(r, _)| r <= now) {
                    a.held.pop_front();
                }
                let pts = match &a.cur {
                    Some((_, pts)) => *pts,
                    None => {
                        // Drop frames that can no longer arrive before their PTS.
                        while let Some(h) = a.queue.front() {
                            let pk = (h.data.len() as u64 + 14).div_ceil(184);
                            if now + SlotClock::ticks_for(pk, 2_000_000) > h.pts.0 {
                                a.queue.pop_front();
                                p.stats.audio_dropped += 1;
                            } else {
                                break;
                            }
                        }
                        let Some(h) = a.queue.front() else { continue };
                        let held: u64 = a.held.iter().map(|&(_, b)| b).sum();
                        if now + a.cfg.lead < h.pts.0
                            || held + h.data.len() as u64 > a.cfg.buffer_bytes
                        {
                            continue;
                        }
                        h.pts
                    }
                };
                if !a.tb.admits(now, 188) {
                    continue;
                }
                if best.is_none_or(|(b, _, _)| pts < b) {
                    best = Some((pts, i, t));
                }
            }
        }
        let (_, i, t) = best?;
        let p = &mut self.programs[i];
        let a = &mut p.audio[t];
        if a.cur.is_none() {
            let au = a.queue.pop_front()?;
            let h = pes::header(
                StreamId::PrivateStream1,
                au.pts.ts90k(),
                None,
                au.data.len(),
            )
            .ok()?;
            a.held.push_back((au.pts.0, au.data.len() as u64));
            a.cur = Some((Cursor::new(h, au.data), au.pts));
        }
        let (cur, _) = a.cur.as_mut()?;
        let pkt = cur.next_packet(a.cfg.pid, &mut a.cc, &Adaptation::default())?;
        if cur.done() {
            a.cur = None;
            p.stats.audio_aus += 1;
        }
        a.tb.add(now, 188);
        p.stats.audio_packets += 1;
        Some(pkt)
    }

    /// A video packet: of program `only.0` with a PCR when `only` is given, else of the
    /// eligible program with the earliest DTS.
    fn video(&mut self, now: u64, only: Option<(usize, Stc)>) -> Option<Packet> {
        // Among eligible programs the one with the most credit goes first, then the earliest
        // DTS: a program near its bucket's cap would otherwise lose rate it is owed while
        // others take the slot, and fall behind its delivery rate under contention.
        let mut best: Option<(std::cmp::Reverse<u128>, Stc, usize)> = None;
        for (i, p) in self.programs.iter_mut().enumerate() {
            if only.is_some_and(|(o, _)| o != i) {
                continue;
            }
            let v = &mut p.video;
            let dts = match &v.cur {
                Some((_, dts, _)) => *dts,
                None => {
                    // Discard what can no longer be decoded in time, up to a key frame.
                    while let Some(h) = v.queue.front() {
                        if v.dropping && !h.keyframe {
                            v.queue.pop_front();
                            p.stats.video_dropped += 1;
                            continue;
                        }
                        v.dropping = false;
                        let pk = (h.data.len() as u64 + 19).div_ceil(176) + 1;
                        if now + SlotClock::ticks_for(pk, v.cfg.rate_bps) > h.dts.0 {
                            v.queue.pop_front();
                            v.dropping = true;
                            p.stats.video_dropped += 1;
                            continue;
                        }
                        break;
                    }
                    let Some(h) = v.queue.front() else { continue };
                    if now + v.cfg.buffer_delay < h.dts.0 {
                        continue;
                    }
                    h.dts
                }
            };
            if !v.tokens.ready(now) || !v.tb.admits(now, 188) {
                continue;
            }
            let key = (std::cmp::Reverse(v.tokens.credit(now)), dts, i);
            if best.is_none_or(|b| key < b) {
                best = Some(key);
            }
        }
        let (_, _, i) = best?;
        let p = &mut self.programs[i];
        let v = &mut p.video;
        if v.cur.is_none() {
            let au = v.queue.pop_front()?;
            let h = pes::header(
                StreamId::Video(VideoStreamNumber::ZERO),
                au.pts.ts90k(),
                Some(au.dts.ts90k()),
                au.data.len(),
            )
            .ok()?;
            v.cur = Some((Cursor::new(h, au.data), au.dts, au.keyframe));
        }
        let (cur, dts, key) = v.cur.as_mut()?;
        let start = cur.at_start();
        let af = Adaptation {
            random_access: start && *key,
            es_priority: start && *key && v.cfg.es_priority_on_key,
            pcr: only.map(|(_, pcr)| pcr.pcr()),
            ..Adaptation::default()
        };
        let pkt = cur.next_packet(v.cfg.pid, &mut v.cc, &af)?;
        if cur.done() {
            let margin = dts.0.saturating_sub(now + self.clock.slot_ticks());
            p.stats.min_dts_margin = Some(p.stats.min_dts_margin.map_or(margin, |m| m.min(margin)));
            p.stats.video_aus += 1;
            v.cur = None;
        }
        v.tokens.spend(now);
        v.tb.add(now, 188);
        p.stats.video_packets += 1;
        Some(pkt)
    }
}
