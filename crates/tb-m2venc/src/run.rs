//! The run loop (the C++ `main`): clips, sessions, the schedule of every frame
//! (rate plans, forced key frames, captions, holds, run-time sessions), pacing, output files,
//! the per-tick log and the statistics.
//!
//! This is the only place with a clock and with files. Per tick it issues commands to the
//! encoder, lets the [`Engine`] run the tick, and takes the access units. It allocates nothing
//! per tick in steady state beyond what the encoder documents (one vector per access unit).

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use tb_device::{ArenaSizes, DevBuf, DeviceError, HostBuf, Nanos, Op, Plan, StreamId};
use tb_m2v_encoder::layout::geometry;
use tb_m2v_encoder::{
    AccessUnit, Encoder, EncoderError, EncoderOptions, FrameSource, SessionConfig, SessionId,
};
use tb_m2v_syntax::headers::SEQUENCE_END;

use crate::opts::{FilmStep, Group, Opts};
use crate::report::{GroupReport, Report, TypeStats};
use tb_engine::device::TickDevice;
use tb_engine::engine::{Engine, EngineError};
use tb_engine::rng::{Mt19937, rate_walk};

/// Why a run could not start or go on. Streams written so far are closed properly first.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// The error.
        source: std::io::Error,
    },
    /// A clip holds less than one frame.
    #[error("{0}: input too small")]
    InputTooSmall(String),
    /// The frame rates of the groups are not integer multiples of each other.
    #[error("frame rates of the groups must be integer multiples")]
    FrameRates,
    /// A session was refused.
    #[error("{0}")]
    Encoder(#[from] EncoderError),
    /// The host arena is smaller than the run needs (a bug in the sizing).
    #[error("the host arena has no room for the clips")]
    HostArena,
    /// The device arena is smaller than the run needs (a bug in the sizing), or a session's
    /// input planes are not the padded planes of its format.
    #[error("the device arena has no room for the clips")]
    DeviceArena,
    /// The clips could not be put on the device (`--input-mode dev`).
    #[error("uploading the clips to the device: {0}")]
    ClipUpload(DeviceError),
    /// The device stopped answering.
    #[error("{0}")]
    Engine(#[from] EngineError),
    /// `--cc-text` on a frame rate captions are not made for.
    #[error("--cc-text: captions are made for 29.97, 30, 59.94 and 60 Hz output")]
    CaptionRate,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> RunError + '_ {
    move |source| RunError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// How a run ended. Every stream that was written is complete and ends with a
/// sequence_end_code in each case.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Every picture was coded.
    Complete,
    /// Plans failed; their pictures are repeats made on the host (valid streams).
    FailedPlans(u64),
    /// The device was lost: from then on every picture is a repeat made on the host, the
    /// input stopped, and the process must be started again to use the card.
    DeviceLost,
    /// The device stopped answering; the streams end where it did.
    Hung(String),
}

/// The result of a run.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// How it ended.
    pub status: Status,
    /// The statistics.
    pub report: Report,
}

/// Where a clip's frames are.
#[derive(Debug)]
enum ClipData {
    /// In the host arena (pinned on the card): frames are uploaded from where they are.
    Arena(HostBuf),
    /// In ordinary memory: every frame is copied into the session's frame buffer.
    Heap(Vec<u8>),
}

#[derive(Debug)]
struct Clip {
    path: std::path::PathBuf,
    fbytes: u64,
    frames: u64,
    data: ClipData,
}

/// A clip held on the device (`--input-mode dev`; the C++ `d_in`), every frame in the padded
/// layout of one format: the three planes one after the other, each of the length of a
/// session's input plane (`Encoder::frame_target`), rows `pitch` bytes apart. A frame is copied
/// into a session with three whole-plane device copies; what lies beyond the picture in the
/// planes is never read (the tick pads after the copy).
#[derive(Debug)]
struct DevClip {
    /// Index of the clip in the host arena.
    clip: usize,
    w: u32,
    h: u32,
    /// Padded width: bytes from one luma row to the next (chroma: half).
    pitch: u32,
    /// Padded height.
    rows: u32,
    region: DevBuf,
}

impl DevClip {
    /// Bytes of a padded luma plane (a chroma plane is a quarter).
    fn ysz(&self) -> u64 {
        u64::from(self.pitch) * u64::from(self.rows)
    }
    /// Bytes of a frame on the device.
    fn frame_len(&self) -> u64 {
        self.ysz() * 3 / 2
    }
    /// The planes (Y, U, V) of frame `f`.
    fn planes(&self, f: u64) -> Option<[DevBuf; 3]> {
        let (y, c) = (self.ysz(), self.ysz() / 4);
        let at = f.checked_mul(self.frame_len())?;
        Some([
            self.region.slice(at, y)?,
            self.region.slice(at + y, c)?,
            self.region.slice(at + y + c, c)?,
        ])
    }
}

/// The device-resident clips of the groups (one per distinct clip and padded format, not yet
/// placed) and each group's.
fn dev_clip_list(o: &Opts, clip_of: &[usize]) -> Result<(Vec<DevClip>, Vec<usize>), RunError> {
    let mut list: Vec<DevClip> = Vec::new();
    let mut of_group = Vec::with_capacity(o.groups.len());
    for (g, &clip) in o.groups.iter().zip(clip_of) {
        let (geo, _) = geometry(&g.c, g.c.capacity).map_err(EncoderError::from)?;
        let (pitch, rows) = (geo.w.unsigned_abs(), geo.h.unsigned_abs());
        let found = list
            .iter()
            .position(|d| (d.clip, d.w, d.h, d.pitch, d.rows) == (clip, g.c.w, g.c.h, pitch, rows));
        of_group.push(found.unwrap_or(list.len()));
        if found.is_none() {
            list.push(DevClip {
                clip,
                w: g.c.w,
                h: g.c.h,
                pitch,
                rows,
                region: DevBuf::default(),
            });
        }
    }
    Ok((list, of_group))
}

/// Per-session counters the report needs (the C++ keeps them in `Session`).
#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    repeats: u64,
    pictures: u64,
}

struct GroupRun {
    every: u64,
    frames: u64,
    clip: usize,
    nmb: u64,
    sess: Vec<Option<SessionId>>,
    submitted: Vec<u64>,
    /// Film mode: ticks the session has seen.
    ticks: Vec<u64>,
    plan: Vec<Vec<(u64, u64)>>,
    keys_seen: Vec<u64>,
    incarnation: Vec<u32>,
    want_open: Vec<bool>,
    want_since: Vec<Instant>,
    out: Vec<Option<BufWriter<File>>>,
    acc: Vec<Acc>,
    /// `--cc-text`: each session's captioner.
    capt: Vec<Option<tb_captions::Captioner>>,
}

/// The captioner of a session for `--cc-text`: the file's cues on CC1 and service 1, pop-on.
/// Parse errors and cues that cannot be sent in time are printed.
fn captioner(
    g: &Group,
    console: &mut dyn Write,
) -> Result<Option<tb_captions::Captioner>, RunError> {
    use tb_captions::schedule::{Aspect, CaptionerCfg, Presentation, Track};
    let Some(path) = &g.cc_text else {
        return Ok(None);
    };
    let frc = tb_m2v_syntax::profile::frame_rate_code(g.c.fps_n, g.c.fps_d);
    let rate = frc
        .and_then(tb_captions::FieldRate::of_frame_rate_code)
        .ok_or(RunError::CaptionRate)?;
    let text = std::fs::read_to_string(path).map_err(io_err(path))?;
    let vtt = text.trim_start_matches('\u{feff}').starts_with("WEBVTT")
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("vtt"));
    let parsed = if vtt {
        tb_captions::vtt::parse(&text)
    } else {
        tb_captions::srt::parse(&text)
    };
    let mut track = Track::primary(Presentation::PopOn);
    if let Some(t) = track.cea708.as_mut()
        && g.c.w <= 720
    {
        t.aspect = Aspect::Standard;
    }
    let mut cfg = CaptionerCfg::new(vec![track]);
    cfg.rate = rate;
    let Ok(mut c) = tb_captions::Captioner::new(cfg) else {
        return Ok(None);
    };
    let rep = c.add_cues(0, &parsed.cues);
    let shout = |console: &mut dyn Write, s: String| {
        let _ = writeln!(console, "{}: {s}", path.display());
    };
    for e in &parsed.errors {
        shout(console, e.to_string());
    }
    for l in &rep.late {
        let when = l
            .shown
            .map_or("skipped".to_owned(), |t| format!("shown at {t}"));
        shout(
            console,
            format!("{:?}: cue at {} is late, {when}", l.service, l.start),
        );
    }
    Ok(Some(c))
}

struct Draining {
    sid: SessionId,
    out: Option<BufWriter<File>>,
    gi: usize,
}

/// Totals of a run (the locals of the C++ main loop).
#[derive(Default)]
struct Totals {
    pictures: u64,
    mbs: u64,
    repeats: u64,
    keys_missed: u64,
    /// Access units by display units (index 1, 2, 3).
    shown: [u64; 4],
    up_bytes: f64,
    down_bytes: f64,
    pool_opens: u64,
    pool_closes: u64,
    pool_waits: u64,
    pool_wait_ms: f64,
    pool_wait_max: f64,
    s0: [TypeStats; 4],
}

/// The output files of session 0 of group 0 and the golden records.
struct Side {
    recon: Option<(File, String)>,
    stats: Option<(BufWriter<File>, String)>,
    log: bool,
    /// Buffer delay of session 0 of group 0, for the stats lines.
    d0: f64,
    fbytes0: u64,
}

fn create(path: &Path) -> Result<File, RunError> {
    File::create(path).map_err(io_err(path))
}

/// The encoder options of a command line.
pub fn encoder_options(o: &Opts) -> EncoderOptions {
    EncoderOptions {
        overlap: o.overlap,
        keep_recon: o.recon.is_some(),
        tickmark: o.tickmark,
        // The C++ driver always seeded the hook's generator (seed 1 without the option);
        // probability 0 leaves the hook off.
        force_shed: Some(o.force_shed),
        split_plans: true,
        ..EncoderOptions::default()
    }
}

/// Frames read of a group's clip: what the file holds, at most `--max-in-frames`.
fn clip_frames(g: &Group, max_in: u64) -> Result<u64, RunError> {
    let len = std::fs::metadata(&g.input).map_err(io_err(&g.input))?.len();
    let n = (len / g.fbytes().max(1)).min(max_in);
    if n < 1 {
        return Err(RunError::InputTooSmall(g.input.display().to_string()));
    }
    Ok(n)
}

/// The clips of the groups (one per distinct file, frame size and length) and each group's
/// clip.
fn clip_list(o: &Opts) -> Result<(Vec<Clip>, Vec<usize>), RunError> {
    let mut clips: Vec<Clip> = Vec::new();
    let mut of_group = Vec::new();
    for g in &o.groups {
        let frames = clip_frames(g, o.max_in)?;
        let fbytes = g.fbytes();
        let i = match clips
            .iter()
            .position(|c| c.path == g.input && c.fbytes == fbytes && c.frames == frames)
        {
            Some(i) => i,
            None => {
                clips.push(Clip {
                    path: g.input.clone(),
                    fbytes,
                    frames,
                    data: ClipData::Heap(Vec::new()),
                });
                clips.len() - 1
            }
        };
        of_group.push(i);
    }
    Ok((clips, of_group))
}

fn round_up(x: u64, to: u64) -> u64 {
    x.div_ceil(to) * to
}

/// The arena sizes a run needs: every batch the line-up can have at once, laid out by a
/// dry run of the encoder's own allocator, plus the clips in the host arena and, with
/// `--input-mode dev`, on the device. Pool groups get `--arena MB` of device memory when given,
/// else twice what all their sessions need at once.
pub fn arena_sizes(o: &Opts) -> Result<ArenaSizes, RunError> {
    const HUGE: u64 = 1 << 56;
    let (clips, clip_of) = clip_list(o)?;
    let dev_clip_bytes: u64 = if o.dev_input {
        let (list, _) = dev_clip_list(o, &clip_of)?;
        list.iter()
            .map(|d| round_up(d.frame_len() * clips[d.clip].frames, 256))
            .sum()
    } else {
        0
    };
    let mut e = Encoder::new(
        encoder_options(o),
        ArenaSizes {
            device: HUGE,
            host: HUGE,
        },
    );
    let used = |e: &Encoder| {
        let (d, h) = e.free_bytes();
        (HUGE - d, HUGE - h)
    };
    for g in o.groups.iter().filter(|g| !g.pool) {
        for s in 0..g.n {
            e.open(&g.session_cfg(s))?;
        }
    }
    let (dev_static, host_static) = used(&e);
    for g in o.groups.iter().filter(|g| g.pool) {
        for s in 0..g.n {
            e.open(&g.session_cfg(s))?;
        }
    }
    let (dev_all, host_all) = used(&e);
    let (dev_pool, host_pool) = (dev_all - dev_static, host_all - host_static);
    let dev_pool = if o.arena_mb > 0 {
        o.arena_mb << 20
    } else {
        2 * dev_pool
    };
    // The clips on the device are uploaded from the host arena, whatever `--test-input-copy` says.
    let clip_bytes: u64 = if o.input_copy && !o.dev_input {
        0
    } else {
        clips
            .iter()
            .map(|c| round_up(c.fbytes * c.frames, 64))
            .sum()
    };
    // The tables' host region exists once a batch does; a pool-only line-up has none yet.
    let slack = 64 << 10;
    Ok(ArenaSizes {
        device: round_up(dev_static + dev_pool + dev_clip_bytes + slack, 4096),
        host: round_up(host_static + 2 * host_pool + clip_bytes + slack, 4096),
    })
}

/// Synthetic captions: the 608 field 1 pair carries the display index. `ncc` constructs
/// (cc_count of the session's frame rate, CTA-708-E Table 4). A picture of two or more field
/// periods carries the pairs of both fields, in the display order of its fields: cc_type 01
/// goes with the top field (CTA-708-E 4.4.2), so it comes first when `tff`; a picture of one
/// field period (60 pictures per second) carries the field 1 pair alone. Then a DTVCC pair and
/// padding. Returns the number of bytes used of `cc`.
/// Synthetic captions of a picture shown for `lead.len()` field periods of 1/59.94 s (film
/// mode): one CEA-608 construct per field period in display order (`lead[k]`: cc_type of field
/// period k; CTA-708-E 4.3.1 rule 2 and Annex C: 01 with a top field, 00 with a bottom field,
/// and in a progressive session the two in turn), the field 1 constructs carrying the display
/// index, then a DTVCC pair and padding up to `ncc` constructs.
fn captions_shown(cc: &mut [u8; 96], n: u64, ncc: usize, lead: &[u8]) -> usize {
    let ncc = ncc.min(31);
    let field1 = [0xFC, ((n >> 7) & 0x7f) as u8, (n & 0x7f) as u8];
    let field2 = [0xFD, 0x80, 0x80];
    let dtvcc = [0xFF, (n & 0xff) as u8, 0x55];
    let pad = [0xFA, 0, 0];
    for (i, c) in cc.as_chunks_mut::<3>().0.iter_mut().take(ncc).enumerate() {
        *c = match lead.get(i) {
            Some(0) => field1,
            Some(_) => field2,
            None if i == lead.len() => dtvcc,
            None => pad,
        };
    }
    3 * ncc
}

fn captions(cc: &mut [u8; 96], n: u64, ncc: usize, tff: bool) -> usize {
    let ncc = ncc.min(31);
    let field1 = [0xFC, ((n >> 7) & 0x7f) as u8, (n & 0x7f) as u8];
    let field2 = [0xFD, 0x80, 0x80];
    let dtvcc = [0xFF, (n & 0xff) as u8, 0x55];
    let pad = [0xFA, 0, 0];
    let lead: &[[u8; 3]] = if ncc <= 10 {
        &[field1, dtvcc]
    } else if tff {
        &[field2, field1, dtvcc]
    } else {
        &[field1, field2, dtvcc]
    };
    for (i, c) in cc.as_chunks_mut::<3>().0.iter_mut().take(ncc).enumerate() {
        *c = *lead.get(i).unwrap_or(&pad);
    }
    3 * ncc
}

fn type_letter(t: u8) -> char {
    match t {
        1 => 'I',
        2 => 'P',
        3 => 'B',
        _ => ' ',
    }
}

/// The encoder's golden records so far, to the `--golden` file.
fn write_golden(
    enc: &mut Encoder,
    golden: &mut Option<(BufWriter<File>, String)>,
) -> Result<(), RunError> {
    if let Some((f, path)) = golden {
        f.write_all(enc.take_golden().as_bytes())
            .map_err(|source| RunError::Io {
                path: path.clone(),
                source,
            })?;
    }
    Ok(())
}

impl Side {
    /// What the C++ does for a picture of session 0 of group 0: reconstruction, stats line,
    /// log line.
    fn picture(&mut self, au: &AccessUnit, console: &mut dyn Write) -> Result<(), RunError> {
        if let Some((f, path)) = &mut self.recon
            && !au.recon.is_empty()
        {
            let e = |source| RunError::Io {
                path: path.clone(),
                source,
            };
            f.seek(SeekFrom::Start(au.disp * self.fbytes0)).map_err(e)?;
            f.write_all(&au.recon).map_err(e)?;
        }
        let t = type_letter(au.pic_type);
        if let Some((f, path)) = &mut self.stats {
            writeln!(
                f,
                "{{\"n\":{},\"disp\":{},\"type\":\"{t}\",\"key\":{},\"bits\":{},\"rate\":{},\
                 \"D\":{:.6},\"q\":{:.2},\"rung\":{},\"tries\":{},\"queue\":{:.0},\"cap\":{:.0},\
                 \"room\":{:.0},\"pts\":{},\"dts\":{},\"ticket\":{},\"intra\":{},\
                 \"complexity\":{:.0},\"repeat\":{},\"key_missed\":{},\"off_grid\":{},\
                 \"units\":{},\"top_first\":{}}}",
                au.coded,
                au.disp,
                u8::from(au.keyframe),
                au.bits,
                au.rate,
                self.d0,
                au.avg_q,
                au.rung,
                au.tries,
                au.queue_bits,
                au.cap_bits,
                au.room_bits,
                au.pts,
                au.dts,
                au.rate_ticket,
                au.intra_mbs,
                au.complexity,
                u8::from(au.repeat),
                u8::from(au.key_missed),
                u8::from(au.off_grid),
                au.units,
                u8::from(au.top_first),
            )
            .map_err(|source| RunError::Io {
                path: path.clone(),
                source,
            })?;
        }
        if self.log {
            // The console is best effort: a closed pipe must not end the encode.
            let _ = writeln!(
                console,
                "pic {:4} disp {:4} type {t} q {:6.2} bytes {:7} rate {:8} queue {:8.0} room \
                 {:8.0} rung {:2} tries {} intra {} field {}{}{}{}",
                au.coded,
                au.disp,
                au.avg_q,
                au.bits / 8,
                au.rate,
                au.queue_bits,
                au.room_bits,
                au.rung,
                au.tries,
                au.intra_mbs,
                au.field_mbs,
                if au.repeat { " REPEAT" } else { "" },
                if au.key_missed { " KEY-MISSED" } else { "" },
                if au.off_grid { " KEY-OFF-GRID" } else { "" },
            );
        }
        Ok(())
    }
}

/// Take every access unit a session has ready (the C++ `take`).
#[allow(clippy::too_many_arguments, reason = "the captures of the C++ lambda")]
fn take(
    enc: &mut Encoder,
    sid: SessionId,
    nmb: u64,
    out: &mut Option<BufWriter<File>>,
    name: &dyn Fn() -> String,
    acc: Option<&mut Acc>,
    side: Option<&mut Side>,
    tot: &mut Totals,
    tick_mb: &mut [u64; 4],
    console: &mut dyn Write,
) -> Result<(), RunError> {
    let mut acc = acc;
    let mut side = side;
    while let Some(au) = enc.take_output(sid) {
        tot.pictures += 1;
        if au.repeat {
            tot.repeats += 1;
        } else {
            tot.mbs += nmb;
            tick_mb[usize::from(au.pic_type & 3)] += nmb;
        }
        if au.key_missed {
            tot.keys_missed += 1;
        }
        tot.shown[usize::from(au.units.min(3))] += 1;
        tot.down_bytes += au.data.len() as f64;
        if let Some(a) = acc.as_deref_mut() {
            a.pictures += 1;
            a.repeats += u64::from(au.repeat);
        }
        if let Some(f) = out {
            f.write_all(&au.data).map_err(|source| RunError::Io {
                path: name(),
                source,
            })?;
        }
        if let Some(sd) = side.as_deref_mut() {
            let t = &mut tot.s0[usize::from(au.pic_type & 3)];
            t.count += 1;
            t.bytes += (au.bits / 8) as f64;
            if !au.repeat {
                t.qsum += f64::from(au.avg_q);
            }
            sd.picture(&au, console)?;
        }
        // the next access unit of the encoder takes this vector instead of allocating
        enc.recycle(au.data);
    }
    Ok(())
}

fn end_stream(out: &mut Option<BufWriter<File>>, name: &str) -> Result<(), RunError> {
    if let Some(mut f) = out.take() {
        f.write_all(&SEQUENCE_END)
            .and_then(|()| f.flush())
            .map_err(|source| RunError::Io {
                path: name.to_owned(),
                source,
            })?;
    }
    Ok(())
}

/// Name of a stream file.
pub fn out_name(o: &Opts, gi: usize, s: usize, incarnation: u32) -> String {
    let g = &o.groups[gi];
    let mut name = o.output.clone().unwrap_or_default();
    if o.groups.len() > 1 {
        name.push_str(&format!(".g{gi}"));
    }
    if g.n > 1 {
        name.push_str(&format!(".{s}"));
    }
    if g.pool {
        name.push_str(&format!(".c{incarnation}"));
    }
    name.push_str(".m2v");
    name
}

fn cpu_seconds() -> f64 {
    // utime and stime of the whole process, in clock ticks (100 per second on Linux).
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return 0.0;
    };
    let Some(rest) = stat.rsplit(')').next() else {
        return 0.0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    (tick(11) + tick(12)) / 100.0
}

fn loadavg() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse().ok()))
        .unwrap_or(0.0)
}

/// Run a command line on a device whose arenas are at least [`arena_sizes`]. `console`: where
/// the lines of `--log` go.
pub fn run<D: TickDevice>(o: &Opts, dev: D, console: &mut dyn Write) -> Result<Outcome, RunError> {
    let mut dev = dev;
    dev.set_blocking(o.blocking || o.profile);
    dev.set_timing(o.profile);
    let device = dev.describe();
    let mut eng = Engine::new(dev, encoder_options(o), o.device_timeout);
    // `--golden`: the encoder's records of golden/README.md, written as they come
    eng.enc_mut().record_golden(o.golden.is_some());

    let base_fps = o.groups.iter().map(Group::fps).fold(0.0, f64::max);
    let period = 1.0 / base_fps;

    // ---- clips
    let (mut clips, clip_of) = clip_list(o)?;
    for c in &mut clips {
        let len = c.fbytes * c.frames;
        let mut f = File::open(&c.path).map_err(io_err(&c.path))?;
        if o.input_copy && !o.dev_input {
            let mut v = vec![0u8; usize::try_from(len).map_err(|_| RunError::HostArena)?];
            f.read_exact(&mut v).map_err(io_err(&c.path))?;
            c.data = ClipData::Heap(v);
        } else {
            let region = eng.enc_mut().reserve_host(len).ok_or(RunError::HostArena)?;
            let dst = region.of_mut(eng.host_mut()).ok_or(RunError::HostArena)?;
            f.read_exact(dst).map_err(io_err(&c.path))?;
            c.data = ClipData::Arena(region);
        }
    }

    // ---- clips on the device: uploaded once, before anything else takes device memory
    let (mut dev_clips, dev_clip_of) = if o.dev_input {
        dev_clip_list(o, &clip_of)?
    } else {
        (Vec::new(), Vec::new())
    };
    // The driver's own plan: the upload of the clips, then every tick's frame copies.
    let mut dplan = Plan::new();
    for d in &mut dev_clips {
        let c = &clips[d.clip];
        let ClipData::Arena(host) = &c.data else {
            return Err(RunError::HostArena);
        };
        d.region = eng
            .enc_mut()
            .reserve_device(d.frame_len() * c.frames)
            .ok_or(RunError::DeviceArena)?;
        let (ylen, clen) = (
            u64::from(d.w) * u64::from(d.h),
            u64::from(d.w / 2) * u64::from(d.h / 2),
        );
        dplan.clear();
        for f in 0..c.frames {
            let dst = d.planes(f).ok_or(RunError::DeviceArena)?;
            let at = f * c.fbytes;
            let src = [(at, ylen), (at + ylen, clen), (at + ylen + clen, clen)];
            for (i, (dst, (off, len))) in dst.into_iter().zip(src).enumerate() {
                let sh = u32::from(i > 0);
                dplan.main(Op::Upload2d {
                    dst,
                    dst_pitch: d.pitch >> sh,
                    src: host.slice(off, len).ok_or(RunError::HostArena)?,
                    src_pitch: d.w >> sh,
                    width: d.w >> sh,
                    rows: d.h >> sh,
                });
            }
        }
        let ticket = eng.dev_mut().submit(&dplan).map_err(RunError::ClipUpload)?;
        eng.dev_mut()
            .wait(ticket, None)
            .status
            .map_err(RunError::ClipUpload)?;
    }
    dplan.clear();

    // ---- groups and sessions
    let mut total_sessions = 0u64;
    let mut groups: Vec<GroupRun> = Vec::with_capacity(o.groups.len());
    for (gi, g) in o.groups.iter().enumerate() {
        let ratio = base_fps / g.fps();
        let every = ratio.round();
        if (ratio - every).abs() > 1e-6 || every < 1.0 {
            return Err(RunError::FrameRates);
        }
        let clip = clip_of[gi];
        let frames = if g.frames == 0 {
            clips[clip].frames
        } else {
            g.frames
        };
        let (geo, _) = geometry(&g.c, g.c.capacity).map_err(EncoderError::from)?;
        let n = g.n as usize;
        let mut gr = GroupRun {
            every: every as u64,
            frames,
            clip,
            nmb: u64::from(geo.nmb.unsigned_abs()),
            sess: vec![None; n],
            submitted: vec![0; n],
            ticks: vec![0; n],
            plan: Vec::with_capacity(n),
            keys_seen: vec![0; n],
            incarnation: vec![0; n],
            want_open: vec![false; n],
            want_since: vec![Instant::now(); n],
            out: (0..n).map(|_| None).collect(),
            acc: vec![Acc::default(); n],
            capt: Vec::with_capacity(n),
        };
        for _ in 0..n {
            let c = captioner(g, console)?;
            gr.capt.push(c);
        }
        for s in 0..g.n {
            if !g.pool {
                gr.sess[s as usize] = Some(eng.enc_mut().open(&g.session_cfg(s))?);
            }
            let mut plan = g.rate_plan.clone();
            if let Some(w) = g.walk
                && w.hi > 0.0
            {
                plan.extend(rate_walk(w.lo, w.hi, w.seed, s, g.c.gop, frames));
            }
            gr.plan.push(plan);
        }
        if !g.pool {
            total_sessions += u64::from(g.n);
        }
        groups.push(gr);
    }

    // ---- outputs: the first write_n sessions of every group
    if o.output.is_some() {
        for (gi, g) in o.groups.iter().enumerate() {
            if g.pool {
                continue;
            }
            for s in 0..(g.n.min(o.write_n) as usize) {
                let name = out_name(o, gi, s, 0);
                groups[gi].out[s] = Some(BufWriter::new(create(Path::new(&name))?));
            }
        }
    }
    let buffered = |p: &Option<std::path::PathBuf>| -> Result<_, RunError> {
        p.as_ref()
            .map(|p| Ok((BufWriter::new(create(p)?), p.display().to_string())))
            .transpose()
    };
    let d0 = match groups[0].sess[0] {
        Some(sid) => eng.enc().snapshot(sid)?.rc.vbv.d,
        None => 0.0,
    };
    let mut side = Side {
        recon: o
            .recon
            .as_ref()
            .map(|p| Ok::<_, RunError>((create(p)?, p.display().to_string())))
            .transpose()?,
        stats: buffered(&o.stats)?,
        log: o.log,
        d0,
        fbytes0: o.groups[0].fbytes(),
    };
    let mut golden = buffered(&o.golden)?;
    if let Some((f, path)) = &mut golden {
        writeln!(
            f,
            "{{\"k\":\"golden\",\"version\":1,\"args\":\"{}\"}}",
            o.args.replace('\\', "\\\\").replace('"', "\\\"")
        )
        .map_err(|source| RunError::Io {
            path: path.clone(),
            source,
        })?;
    }
    let mut ticklog = buffered(&o.ticklog)?;
    let mut oplog = buffered(&o.oplog)?;

    // ---- main loop
    let total_frames: u64 = groups.iter().map(|g| g.frames).max().unwrap_or(0);
    let mut tick_times: Vec<f64> = Vec::with_capacity((2 * total_frames + 64) as usize);
    let (mut late, mut late_dl, mut late_op, mut op_ticks) = (0u64, 0u64, 0u64, 0u64);
    let (mut worst_tick, mut sum_tick) = (0.0f64, 0.0f64);
    let load0 = loadavg();
    let mut tot = Totals::default();
    let mut draining: Vec<Draining> = Vec::new();
    let mut crng = Mt19937::new(o.churn.map_or(0, |c| c.seed));
    let pool: Vec<(usize, usize)> = o
        .groups
        .iter()
        .enumerate()
        .filter(|(_, g)| g.pool)
        .flat_map(|(gi, g)| (0..g.n as usize).map(move |s| (gi, s)))
        .collect();
    let mut cc = [0u8; 96];
    let mut failed_ticks = 0u64;
    let mut lost = false;
    let mut idle_ticks = 0u32;
    let mut hung: Option<String> = None;

    eng.settle()?;
    let t_start = Instant::now();
    let c_start = cpu_seconds();
    let due = |t: u64| t_start + Duration::from_secs_f64(t as f64 * period);

    let mut t = 0u64;
    loop {
        let mut tick0 = Instant::now();
        if o.paced {
            let d = due(t);
            if tick0 < d {
                std::thread::sleep(d - tick0);
                tick0 = Instant::now();
            }
        }
        // ---- sessions opened, closed and reconfigured at run time
        let mut ops = 0u32;
        let op_t0 = Instant::now();
        let stable_more = !lost
            && o.groups
                .iter()
                .zip(&groups)
                .any(|(g, gr)| !g.pool && gr.submitted.iter().any(|n| *n < gr.frames));
        if let Some(churn) = o.churn {
            if stable_more {
                if !pool.is_empty() && crng.uniform(0.0, 1.0) < churn.rate * period {
                    let k = (crng.uniform(0.0, 1.0) * pool.len() as f64) as usize % pool.len();
                    let (gi, s) = pool[k];
                    let what = crng.uniform(0.0, 1.0);
                    let gr = &mut groups[gi];
                    match gr.sess[s] {
                        None if !gr.want_open[s] => {
                            gr.want_open[s] = true;
                            gr.want_since[s] = Instant::now();
                            ops += 1;
                        }
                        None => {}
                        Some(sid) => {
                            // close, or (the other half) reconfigure: close and open again
                            eng.enc_mut().close(sid)?;
                            draining.push(Draining {
                                sid,
                                out: gr.out[s].take(),
                                gi,
                            });
                            gr.sess[s] = None;
                            tot.pool_closes += 1;
                            ops += 1;
                            if what >= 0.5 {
                                gr.want_open[s] = true;
                                gr.want_since[s] = Instant::now();
                            }
                        }
                    }
                }
                for &(gi, s) in &pool {
                    if !groups[gi].want_open[s] {
                        continue;
                    }
                    // A batch is laid out in the arena at once: there is no reserved batch to
                    // wait for (the C++ `--churn ...:naive --arena`).
                    let g = &o.groups[gi];
                    let gr = &mut groups[gi];
                    let mut c: SessionConfig = g.session_cfg(s as u32);
                    // every second time a slot is reopened it runs at 60 % of the rate
                    if gr.incarnation[s] % 2 == 1 && c.bitrate > 0 {
                        c.bitrate = c.bitrate * 6 / 10;
                    }
                    gr.sess[s] = Some(eng.enc_mut().open(&c)?);
                    gr.submitted[s] = 0;
                    gr.keys_seen[s] = 0;
                    gr.want_open[s] = false;
                    gr.acc[s] = Acc::default();
                    if o.output.is_some() && s < o.write_n as usize {
                        let name = out_name(o, gi, s, gr.incarnation[s]);
                        gr.out[s] = Some(BufWriter::new(create(Path::new(&name))?));
                    }
                    gr.incarnation[s] += 1;
                    tot.pool_opens += 1;
                    ops += 1;
                    let w = 1e3 * gr.want_since[s].elapsed().as_secs_f64();
                    tot.pool_wait_ms += w;
                    tot.pool_wait_max = tot.pool_wait_max.max(w);
                    tot.pool_waits += 1;
                }
                eng.enc_mut().reap(o.reap_after);
                if o.dev_input && ops > 0 {
                    // A new batch is cleared by the next plan of the encoder: that must run
                    // before this tick's frame copies into it, which are queued ahead of the
                    // tick's first plan. (The C++ clears a batch where it makes it.)
                    eng.settle()?;
                }
            } else {
                for &(gi, s) in &pool {
                    let gr = &mut groups[gi];
                    if let Some(sid) = gr.sess[s].take() {
                        eng.enc_mut().close(sid)?;
                        draining.push(Draining {
                            sid,
                            out: gr.out[s].take(),
                            gi,
                        });
                        tot.pool_closes += 1;
                    }
                }
            }
        }
        let op_ms = 1e3 * op_t0.elapsed().as_secs_f64();

        // ---- one frame per session that is due
        let mut more = false;
        dplan.clear();
        for (gi, (g, gr)) in o.groups.iter().zip(&mut groups).enumerate() {
            let clip = &clips[gr.clip];
            for s in 0..g.n as usize {
                let Some(sid) = gr.sess[s] else {
                    continue;
                };
                let n = gr.submitted[s];
                if lost {
                    // The device is gone: no more input; what is queued leaves as repeats.
                    eng.enc_mut().flush(sid)?;
                    continue;
                }
                if !g.pool {
                    if n >= gr.frames {
                        eng.enc_mut().flush(sid)?;
                        continue;
                    }
                    more = true;
                }
                if gr.every > 1 && !(t + s as u64).is_multiple_of(gr.every) {
                    continue;
                }
                // Film mode: a picture is due when its display time has come. The session's
                // tick k covers the display units [k * upf, (k + 1) * upf); a session whose
                // next picture starts later has nothing to code in this tick.
                let upf: u64 = if g.c.interlaced { 2 } else { 1 };
                if !g.film.is_empty() {
                    // staggered sessions begin their cadence in different ticks (channels do
                    // not share a phase), so that the ticks without a picture fall apart
                    if g.stagger && t < s as u64 % 5 {
                        continue;
                    }
                    let k = gr.ticks[s];
                    gr.ticks[s] += 1;
                    if eng.enc().display_units(sid)? >= (k + 1) * upf {
                        continue;
                    }
                }
                let enc = eng.enc_mut();
                if let Some(h) = o.hold
                    && h.g == gi
                    && h.s as usize == s
                {
                    enc.hold(sid, n >= h.from && n < h.to)?;
                }
                // rate plan and forced key frames act on the frame about to be submitted
                let mut key_due = enc.key_due(sid)?;
                for d in &g.force_key {
                    if *d == n {
                        enc.force_keyframe(sid)?;
                        key_due = true;
                    }
                }
                // --rate-now: a new rate from the next coded picture on, at any frame
                for (d, rate) in &g.rate_now {
                    if *d == n {
                        enc.set_rate(sid, *rate, false)?;
                    }
                }
                if key_due {
                    for (k, rate) in &gr.plan[s] {
                        if *k == gr.keys_seen[s] {
                            enc.set_rate(sid, *rate, true)?;
                        }
                    }
                    gr.keys_seen[s] += 1;
                }
                // how this frame is shown: one frame period, or what --film says
                let mut shown = enc.ordinary(sid)?;
                if !g.film.is_empty() {
                    shown.top_first = g.c.interlaced && enc.next_top_first(sid)?;
                    if let FilmStep::Units(u) = g.film[(n % g.film.len() as u64) as usize] {
                        shown.units = u;
                        shown.progressive = true;
                    }
                }
                let ncc = if let Some(capt) = gr.capt[s].as_mut() {
                    // the captions of a picture shown for this many field periods, in the
                    // order of its fields
                    let frc = tb_m2v_syntax::profile::frame_rate_code(g.c.fps_n, g.c.fps_d);
                    let (count, _) = tb_m2v_syntax::captions::shown_counts(
                        frc.unwrap_or(4),
                        shown.units,
                        upf as u8,
                    );
                    let fields = (count / 10) as u8;
                    let tf = g.c.interlaced.then_some(shown.top_first);
                    let mut v = Vec::with_capacity(96);
                    capt.picture(fields, tf, &mut v);
                    let n = v.len().min(cc.len());
                    cc[..n].copy_from_slice(&v[..n]);
                    n
                } else if !g.cc {
                    0
                } else if g.film.is_empty() {
                    captions(&mut cc, n, g.cc_count(), g.c.interlaced && g.c.tff)
                } else {
                    // one CEA-608 construct per field period shown, in display order
                    let at = enc.display_units(sid)?;
                    let lead: Vec<u8> = (0..u64::from(shown.units))
                        .map(|k| {
                            if g.c.interlaced {
                                u8::from(shown.top_first == (k % 2 == 0))
                            } else {
                                ((at + k) % 2) as u8
                            }
                        })
                        .collect();
                    let frc = tb_m2v_syntax::profile::frame_rate_code(g.c.fps_n, g.c.fps_d);
                    let (count, slots) = tb_m2v_syntax::captions::shown_counts(
                        frc.unwrap_or(4),
                        shown.units,
                        upf as u8,
                    );
                    captions_shown(&mut cc, n, count, &lead[..lead.len().min(slots)])
                };
                let fidx = (n + s as u64 * 7) % clip.frames;
                // the display time of the frame: frame n while every frame is shown for one
                // frame period
                let units = enc.display_units(sid)?;
                let pts = (i128::from(units) * 90_000 * i128::from(g.c.fps_d)
                    / (i128::from(g.c.fps_n) * i128::from(upf))) as i64;
                let source = match &clip.data {
                    _ if o.dev_input => {
                        // the frame goes from the clip on the device into the session's input
                        // planes, ahead of the tick's kernels on the same stream
                        let (dst, _) = eng.enc().frame_target(sid)?;
                        let src = dev_clips[dev_clip_of[gi]]
                            .planes(fidx)
                            .ok_or(RunError::DeviceArena)?;
                        for (dst, src) in dst.into_iter().zip(src) {
                            if dst.len != src.len {
                                return Err(RunError::DeviceArena);
                            }
                            dplan.push(StreamId::MAIN, Op::Copy { dst, src });
                        }
                        // the test switch: frames flagged progressive, all or all but every
                        // P-th (the clips' content does not change with it)
                        let p = o.test_progressive;
                        FrameSource::Device {
                            progressive: p == 1 || (p > 1 && n % p != p - 1),
                        }
                    }
                    ClipData::Arena(region) => FrameSource::HostAt(
                        region
                            .slice(fidx * clip.fbytes, clip.fbytes)
                            .ok_or(RunError::HostArena)?,
                    ),
                    ClipData::Heap(v) => {
                        let buf = eng.enc().frame_buffer(sid)?;
                        let at = (fidx * clip.fbytes) as usize;
                        buf.of_mut(eng.host_mut())
                            .ok_or(RunError::HostArena)?
                            .copy_from_slice(&v[at..at + clip.fbytes as usize]);
                        FrameSource::Host
                    }
                };
                eng.enc_mut()
                    .submit_shown(sid, source, pts, &cc[..ncc], shown)?;
                if !o.dev_input {
                    tot.up_bytes += clip.fbytes as f64;
                }
                gr.submitted[s] += 1;
            }
        }
        if !more && eng.enc().idle() && draining.is_empty() {
            break;
        }

        // The frame copies of all sessions: one plan, queued ahead of the tick's first.
        let mut copies = None;
        if !dplan.steps.is_empty() {
            match eng.dev_mut().submit(&dplan) {
                Ok(ticket) => copies = Some(ticket),
                Err(e) => eng.device_error(&e),
            }
        }
        let deadline = (o.deadline && o.paced).then(|| due(t + 1));
        let ticked = if o.test_deadline_ns > 0 {
            eng.tick_within(Nanos(o.test_deadline_ns))
        } else {
            eng.tick(deadline)
        };
        if let Err(e) = ticked {
            // The plan is still in flight: nothing more can be asked of this device. What
            // has been written is whole pictures; the streams end here.
            tracing::error!(error = %e, tick = t, "the device stopped answering");
            hung = Some(e.to_string());
            break;
        }
        if let Some(ticket) = copies
            && let Err(e) = eng.dev_mut().wait(ticket, None).status
        {
            // it ran ahead of the tick's kernels on the same stream: it is complete
            eng.device_error(&e);
        }
        let stats = eng.enc().tick_stats();
        if stats.failed {
            failed_ticks += 1;
        }
        if eng.enc().is_lost() && !lost {
            lost = true;
            tracing::error!(
                tick = t,
                error = ?eng.first_error,
                "the device is lost: every session repeats pictures from here on; no more \
                 input is taken"
            );
        }

        // ---- collect
        let pictures_before = tot.pictures;
        let mut tick_mb = [0u64; 4];
        for (gi, (g, gr)) in o.groups.iter().zip(&mut groups).enumerate() {
            for s in 0..g.n as usize {
                let Some(sid) = gr.sess[s] else {
                    continue;
                };
                let first = gi == 0 && s == 0 && !g.pool;
                let inc = gr.incarnation[s].saturating_sub(u32::from(g.pool));
                take(
                    eng.enc_mut(),
                    sid,
                    gr.nmb,
                    &mut gr.out[s],
                    &|| out_name(o, gi, s, inc),
                    (!g.pool).then_some(&mut gr.acc[s]),
                    first.then_some(&mut side),
                    &mut tot,
                    &mut tick_mb,
                    console,
                )?;
            }
        }
        let mut i = 0;
        while i < draining.len() {
            let d = &mut draining[i];
            let gi = d.gi;
            take(
                eng.enc_mut(),
                d.sid,
                groups[gi].nmb,
                &mut d.out,
                &|| format!("a stream of group {gi}"),
                None,
                None,
                &mut tot,
                &mut tick_mb,
                console,
            )?;
            if !eng.enc().is_closed(d.sid)? {
                i += 1;
                continue;
            }
            end_stream(&mut d.out, "a stream of a closed session")?;
            eng.enc_mut().release(d.sid)?;
            draining.remove(i);
        }

        write_golden(eng.enc_mut(), &mut golden)?;
        let now = Instant::now();
        let dt = now.duration_since(tick0).as_secs_f64();
        let is_late = o.paced && now > due(t + 1);
        if let Some((f, path)) = &mut ticklog {
            let tm = eng.times;
            writeln!(
                f,
                "{t} {:.3} {} {} {} {:.3} {:.3} {:.3} {:.3} {} {} {} {:.3} {:.3} {} {ops} \
                 {op_ms:.3} {} {}",
                1e3 * dt,
                tick_mb[1],
                tick_mb[2],
                tick_mb[3],
                tm.upwait,
                tm.phase[0],
                tm.phase[1],
                tm.phase[2],
                stats.rounds,
                stats.retry,
                stats.rexf,
                tm.begin,
                0.0,
                stats.shed,
                stats.candidates,
                u8::from(is_late),
            )
            .map_err(|source| RunError::Io {
                path: path.clone(),
                source,
            })?;
        }
        if lost {
            // Sessions that never coded a picture cannot repeat: when nothing but those is
            // left, no tick produces a picture any more.
            idle_ticks = if tot.pictures == pictures_before {
                idle_ticks + 1
            } else {
                0
            };
            if idle_ticks >= 4 {
                break;
            }
        }
        // Timing statistics leave out the warm-up and the ticks after the input has ended.
        if t >= o.warmup && more {
            tick_times.push(dt);
            sum_tick += dt;
            worst_tick = worst_tick.max(dt);
            if dt > period {
                late += 1;
            }
            if is_late {
                late_dl += 1;
            }
            if ops > 0 {
                op_ticks += 1;
                if is_late {
                    late_op += 1;
                }
                if let Some((f, path)) = &mut oplog {
                    writeln!(
                        f,
                        "{t} {ops} {op_ms:.3} {:.3} {}",
                        1e3 * dt,
                        u8::from(is_late)
                    )
                    .map_err(|source| RunError::Io {
                        path: path.clone(),
                        source,
                    })?;
                }
            }
        }
        t += 1;
    }
    if hung.is_none()
        && let Err(e) = eng.settle()
    {
        hung = Some(e.to_string());
    }
    let wall = t_start.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - c_start;
    let load1 = loadavg();

    // ---- close the streams
    for (gi, (g, gr)) in o.groups.iter().zip(&mut groups).enumerate() {
        for s in 0..g.n as usize {
            let name = out_name(
                o,
                gi,
                s,
                gr.incarnation[s].saturating_sub(u32::from(g.pool)),
            );
            end_stream(&mut gr.out[s], &name)?;
        }
    }
    for d in &mut draining {
        end_stream(&mut d.out, "a stream of a closed session")?;
    }
    write_golden(eng.enc_mut(), &mut golden)?;
    for (f, path) in [&mut side.stats, &mut golden, &mut ticklog, &mut oplog]
        .into_iter()
        .flatten()
    {
        f.flush().map_err(|source| RunError::Io {
            path: path.clone(),
            source,
        })?;
    }

    // ---- report
    tick_times.sort_by(f64::total_cmp);
    let ticks = tick_times.len();
    let pct = |p: f64| {
        if ticks == 0 {
            0.0
        } else {
            tick_times[(ticks - 1).min((ticks as f64 * p) as usize)]
        }
    };
    let g0 = &o.groups[0];
    let video_s = groups[0].frames as f64 / g0.fps();
    let mut rate = tb_m2v_encoder::SessionStats::default().rate;
    let (mut room_min, mut bits_all, mut shed_total) = (1e18f64, 0.0f64, 0u64);
    let mut s0_cbr = false;
    let mut group_reports = Vec::with_capacity(groups.len());
    let enc = eng.enc();
    for (gi, (g, gr)) in o.groups.iter().zip(&groups).enumerate() {
        let mut bits = 0.0;
        let mut geo = None;
        for (s, sid) in gr.sess.iter().enumerate() {
            let Some(sid) = sid else {
                continue;
            };
            let snap = enc.snapshot(*sid)?;
            let r = snap.stats.rate;
            rate.rexf += r.rexf;
            rate.mbfix += r.mbfix;
            rate.retries += r.retries;
            rate.rung_pics += r.rung_pics;
            rate.skip_pics += r.skip_pics;
            rate.isub += r.isub;
            rate.violations += r.violations;
            if snap.rc.cbr {
                room_min = room_min.min(r.room_min / snap.config.vbv_bits as f64);
            }
            bits += snap.stats.total_bits;
            shed_total += snap.stats.shed;
            if gi == 0 && s == 0 {
                s0_cbr = snap.rc.cbr;
            }
            if geo.is_none() {
                geo = Some(enc.batch_layout(*sid)?.0.geo);
            }
        }
        bits_all += bits;
        group_reports.push(GroupReport {
            pool: g.pool,
            w: g.c.w,
            h: g.c.h,
            interlaced: g.c.interlaced,
            fps: g.fps(),
            coded: geo.map_or((0, 0), |x| (x.w, x.h)),
            gop: g.c.gop,
            m: g.c.m,
            stagger: g.stagger,
            f_code: geo.map_or(0, |x| x.f_code),
            range2: g.c.range2,
            field_pred: geo.is_some_and(|x| x.field_pred != 0),
            aq: g.c.aq,
            sessions: g.n,
            frames: gr.frames,
            priority: g.c.priority,
            mean_mbps: bits / f64::from(g.n) / (gr.frames as f64 / g.fps()) / 1e6,
            repeats: gr.acc.iter().map(|a| a.repeats).sum(),
            pictures: gr.acc.iter().map(|a| a.pictures).sum(),
        });
    }
    let arenas = eng.dev().arenas();
    let (free_dev, free_host) = enc.free_bytes();
    let report = Report {
        device,
        w: g0.c.w,
        h: g0.c.h,
        interlaced: g0.c.interlaced,
        fps: g0.fps(),
        sessions: total_sessions.max(1),
        frames: groups[0].frames,
        groups: group_reports,
        ticks: ticks as u64,
        paced: o.paced,
        overlap: o.overlap,
        input_copy: o.input_copy && !o.dev_input,
        dev_input: o.dev_input,
        wall_s: wall,
        video_s,
        mbs: tot.mbs,
        pictures: tot.pictures,
        period_ms: 1e3 * period,
        tick_mean_ms: 1e3 * sum_tick / ticks.max(1) as f64,
        tick_p50_ms: 1e3 * pct(0.5),
        tick_p99_ms: 1e3 * pct(0.99),
        tick_p999_ms: 1e3 * pct(0.999),
        tick_max_ms: 1e3 * worst_tick,
        late,
        late_deadline: late_dl,
        cpu_s: cpu,
        bits_all,
        dev_bytes: arenas.device - free_dev,
        host_bytes: arenas.host - free_host,
        arena_dev_bytes: arenas.device,
        arena_host_bytes: arenas.host,
        load_before: load0,
        load_after: load1,
        repeats: tot.repeats,
        shed: shed_total,
        keys_missed: tot.keys_missed,
        shown: tot.shown,
        churn: o.churn.is_some(),
        pool_opens: tot.pool_opens,
        pool_closes: tot.pool_closes,
        pool_waits: tot.pool_waits,
        pool_wait_ms: tot.pool_wait_ms,
        pool_wait_max: tot.pool_wait_max,
        op_ticks,
        op_ticks_late: late_op,
        up_bytes: tot.up_bytes,
        down_bytes: tot.down_bytes,
        rate,
        room_min,
        s0: tot.s0,
        s0_cbr,
        profile: o.profile,
        total_ticks: t,
        classes: eng.classes,
        host_between_s: eng.host_between,
        failed_ticks,
        device_errors: eng.errors,
        device_lost: lost,
    };
    let status = if let Some(why) = hung {
        Status::Hung(why)
    } else if lost {
        Status::DeviceLost
    } else if failed_ticks > 0 || eng.errors > 0 {
        Status::FailedPlans(failed_ticks.max(eng.errors))
    } else {
        Status::Complete
    };
    Ok(Outcome { status, report })
}
