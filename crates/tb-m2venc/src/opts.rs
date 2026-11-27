//! The command line (the C++ `parse`): the options of the C++ driver that the gate
//! and the benchmarks pass, with the same meanings and defaults, plus the options that exist
//! only here (`--device`, `--gpu`, `--check-arena`, `--device-timeout`, the test switches
//! `--test-input-copy` and `--test-deadline`).

use std::path::PathBuf;

use tb_m2v_encoder::SessionConfig;
use tb_m2v_syntax::profile::{self, Level, Profile};

/// The text printed for a wrong command line.
pub const USAGE: &str = "\
tb-m2venc [group options] [--group [group options]]... [global options]
 group:  --input f.yuv --size WxH [--fps N/D] [--interlaced] [--bff] [--gop 15] [--bframes 2]
         [--bitrate bps [--max-bitrate bps] [--vbv bits] | --q Q [--qi --qp --qb]]
         [--sessions N] [--frames F] [--stagger] [--aq 0|1] [--aq-strength S] [--field-pred 0|1]
         [--field-dct 0|1] [--range R2] [--lambda L] [--field-bits B] [--smooth 0|1] [--tpred 0|1]
         [--intra-vlc 0|1] [--alt-scan 0|1] [--rate-plan K:bps,K:bps...] [--rate-now D:bps,...]
         [--rate-walk lo:hi:seed] [--force-key D,D...] [--cc | --cc-text f.srt|f.vtt]
         [--pool] [--priority P]
         [--aspect 1|2|3] [--film 3,2,...]
 global: [--device cuda|hip|ref] [--gpu ORDINAL] [--check-arena] [--device-timeout S]
         [--conformance h262|a53|scte43-64qam|scte43-256qam]  (every session; default
         scte43-256qam: a configuration outside the profile is refused)
         [--input-mode host|dev] [--deadline] [--force-shed P:SEED] [--hold G:S:FROM:TO]
         [--churn SEED:OPS_PER_S[:naive|reserve]] [--churn-chunk K] [--reap-after TICKS]
         [--oplog f.txt] [--arena MB]
         [--output prefix] [--write N] [--recon f.yuv] [--stats f.jsonl] [--json f.json] [--log]
         [--ticklog f.txt] [--golden f.jsonl]
         [--paced] [--warmup ticks] [--profile] [--overlap] [--max-in-frames K] [--tickmark]
         [--blocking-wait]
 other:  tb-m2venc plumb ref|cuda            the plumbing proof (k_pyr through an adapter)
         tb-m2venc --golden-tables f.jsonl   the table record the kernels read
         tb-m2venc --make-clip W H FRAMES SEED [interlaced|noisy|white|patch] > clip.yuv
 test switches: [--test-input-copy] [--test-deadline NS] [--test-progressive P]";

/// Why a command line was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OptError {
    /// An option this program does not know.
    #[error("unknown option {0}")]
    Unknown(String),
    /// An option without its value.
    #[error("option {0} needs a value")]
    Missing(String),
    /// A value that does not parse.
    #[error("option {0}: cannot read {1:?}")]
    Value(String, String),
    /// A group without input or size.
    #[error("group {0}: --input and --size are required")]
    Group(usize),
}

/// Which adapter executes the plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// An NVIDIA card (`tb-device-gpu`, CUDA).
    Cuda,
    /// An AMD card (`tb-device-gpu`, HIP).
    Hip,
    /// The kernels compiled for the host (`tb-device-ref`): slow, for small pictures.
    Ref,
}

/// `--rate-walk lo:hi:seed`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Walk {
    /// Lowest rate, bit/s.
    pub lo: f64,
    /// Highest rate, bit/s.
    pub hi: f64,
    /// Seed (the session index is added).
    pub seed: i32,
}

/// One group: sessions of one format fed from one clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The raw 4:2:0 clip.
    pub input: PathBuf,
    /// The configuration of the group's sessions (before the stagger).
    pub c: SessionConfig,
    /// Sessions.
    pub n: u32,
    /// Frames per session; 0: the frames of the clip.
    pub frames: u64,
    /// GOP phases spread over the sessions.
    pub stagger: bool,
    /// Synthetic captions in every picture.
    pub cc: bool,
    /// `--cc-text`: captions made from this SubRip or WebVTT file (`tb_captions`: CC1 and
    /// CEA-708 service 1, pop-on), cue time 0 at each session's first picture. Takes the
    /// place of `--cc`.
    pub cc_text: Option<PathBuf>,
    /// The sessions are opened and closed at run time (`--churn`).
    pub pool: bool,
    /// `--rate-plan`: (key frame index, rate).
    pub rate_plan: Vec<(u64, u64)>,
    /// `--rate-now`: (display index, rate).
    pub rate_now: Vec<(u64, u64)>,
    /// `--force-key`: display indices.
    pub force_key: Vec<u64>,
    /// `--rate-walk`.
    pub walk: Option<Walk>,
    /// `--film`: film mode. The frames of the clip are film frames; frame k is shown for
    /// entry `k % len` of this cycle (see [`FilmStep`]). Empty: every frame for one frame
    /// period, as always.
    pub film: Vec<FilmStep>,
}

/// One entry of `--film`: how the next clip frame is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilmStep {
    /// `1`, `2`, `3`: a progressive frame shown for that many display units (fields of an
    /// interlaced session, frame periods of a progressive one).
    Units(u8),
    /// `i`: an ordinary picture of one frame period, not flagged progressive (video in a film
    /// channel).
    Video,
}

impl Default for Group {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            c: SessionConfig::default(),
            n: 1,
            frames: 0,
            stagger: false,
            cc: false,
            cc_text: None,
            pool: false,
            rate_plan: Vec::new(),
            rate_now: Vec::new(),
            force_key: Vec::new(),
            walk: None,
            film: Vec::new(),
        }
    }
}

impl Group {
    /// Frames per second.
    pub fn fps(&self) -> f64 {
        f64::from(self.c.fps_n) / f64::from(self.c.fps_d)
    }
    /// cc_count of a picture of this group (CTA-708-E Table 4; 20 for a frame rate that the
    /// session will refuse).
    pub fn cc_count(&self) -> usize {
        tb_m2v_syntax::captions::cc_count(
            profile::frame_rate_code(self.c.fps_n, self.c.fps_d).unwrap_or(4),
        )
    }
    /// Bytes of one frame of the clip.
    pub fn fbytes(&self) -> u64 {
        u64::from(self.c.w) * u64::from(self.c.h) * 3 / 2
    }
    /// The configuration of session `s`.
    pub fn session_cfg(&self, s: u32) -> SessionConfig {
        let mut c = self.c;
        if self.stagger && c.gop > 0 && self.n > 0 {
            c.gop_phase =
                ((u64::from(s) * u64::from(c.gop) / u64::from(self.n)) % u64::from(c.gop)) as u32;
        }
        c
    }
}

/// `--hold G:S:FROM:TO`: session `s` of group `g` is on hold for its frames `from..to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hold {
    /// Group.
    pub g: usize,
    /// Session of the group.
    pub s: u32,
    /// First frame held.
    pub from: u64,
    /// First frame not held.
    pub to: u64,
}

/// `--churn SEED:OPS_PER_S`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Churn {
    /// Seed of the schedule.
    pub seed: u32,
    /// Session operations per second.
    pub rate: f64,
}

/// The whole command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    /// The groups, at least one.
    pub groups: Vec<Group>,
    /// `--output prefix`.
    pub output: Option<String>,
    /// `--recon f.yuv`.
    pub recon: Option<PathBuf>,
    /// `--json f.json`.
    pub json: Option<PathBuf>,
    /// `--stats f.jsonl`.
    pub stats: Option<PathBuf>,
    /// `--ticklog f.txt`.
    pub ticklog: Option<PathBuf>,
    /// `--golden f.jsonl`: one record per access unit, in the format of crates/tb-golden/vectors/README.md.
    pub golden: Option<PathBuf>,
    /// `--oplog f.txt`.
    pub oplog: Option<PathBuf>,
    /// The command line as one string (for the golden file).
    pub args: String,
    /// Streams written per group.
    pub write_n: u32,
    /// Per-phase times (blocking waits, device-side times).
    pub profile: bool,
    /// One tick per frame period on the clock.
    pub paced: bool,
    /// A line per picture of session 0.
    pub log: bool,
    /// Overlapped upload.
    pub overlap: bool,
    /// `k_tick` at the start of every tick.
    pub tickmark: bool,
    /// Blocking waits instead of the hybrid wait.
    pub blocking: bool,
    /// Ticks left out of the timing statistics.
    pub warmup: u64,
    /// Frames read of each clip.
    pub max_in: u64,
    /// The tick gets the next tick as its deadline (paced runs).
    pub deadline: bool,
    /// `--test-deadline NS` (a test switch): every tick's deadline is NS nanoseconds of the
    /// device's clock after its start, whatever the clock is. On the reference adapter, whose
    /// clock counts executed steps, the deadline policy then decides deterministically (the
    /// golden case `deadline`). 0: off.
    pub test_deadline_ns: u64,
    /// `--test-progressive P` (with `--input-mode dev`): the frames are submitted as
    /// progressive (both fields one instant), except every P-th when P > 1. 0: none is.
    pub test_progressive: u64,
    /// `--force-shed P:SEED` (seed 1 without the option, as in the C++ driver).
    pub force_shed: (f64, u64),
    /// `--hold`.
    pub hold: Option<Hold>,
    /// `--churn`.
    pub churn: Option<Churn>,
    /// Slots of a batch made for a pool group.
    pub churn_chunk: u32,
    /// Ticks after which an empty batch gives its memory back.
    pub reap_after: u64,
    /// Device memory in MiB for the batches of pool groups.
    pub arena_mb: u64,
    /// The adapter.
    pub device: DeviceKind,
    /// CUDA device ordinal.
    pub gpu: i32,
    /// Check the host-arena rule around the device (always on in debug builds).
    pub check_arena: bool,
    /// `--test-input-copy` (a test switch): copy every frame into the session's frame buffer
    /// (`FrameSource::Host`) instead of uploading it from the clip where it lies
    /// (`FrameSource::HostAt`); the tests compare the two paths.
    pub input_copy: bool,
    /// `--input-mode dev`: the clips are held on the device and every frame reaches the encoder
    /// by device-to-device copies (`FrameSource::Device`); no frame crosses the bus after the
    /// start.
    pub dev_input: bool,
    /// Seconds a single plan may take before the device counts as hung.
    pub device_timeout: f64,
    /// `--conformance`: the profile of every session.
    pub conformance: Profile,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            groups: vec![Group::default()],
            output: None,
            recon: None,
            json: None,
            stats: None,
            ticklog: None,
            golden: None,
            oplog: None,
            args: String::new(),
            write_n: 1,
            profile: false,
            paced: false,
            log: false,
            overlap: false,
            tickmark: false,
            blocking: false,
            warmup: 0,
            max_in: 600,
            deadline: false,
            test_deadline_ns: 0,
            test_progressive: 0,
            force_shed: (0.0, 1),
            hold: None,
            churn: None,
            churn_chunk: 4,
            reap_after: 120,
            arena_mb: 0,
            device: DeviceKind::Cuda,
            gpu: 0,
            check_arena: cfg!(debug_assertions),
            input_copy: false,
            dev_input: false,
            device_timeout: 30.0,
            conformance: Profile::default(),
        }
    }
}

fn num<T: std::str::FromStr>(opt: &str, v: &str) -> Result<T, OptError> {
    v.trim()
        .parse()
        .map_err(|_| OptError::Value(opt.to_owned(), v.to_owned()))
}

/// `A<sep>B`.
fn pair<A: std::str::FromStr, B: std::str::FromStr>(
    opt: &str,
    v: &str,
    sep: char,
) -> Result<(A, B), OptError> {
    let (a, b) = v
        .split_once(sep)
        .ok_or_else(|| OptError::Value(opt.to_owned(), v.to_owned()))?;
    Ok((num(opt, a)?, num(opt, b)?))
}

/// `K:V,K:V,...` (empty: no entries).
fn pairs(opt: &str, v: &str) -> Result<Vec<(u64, u64)>, OptError> {
    v.split(',')
        .filter(|x| !x.is_empty())
        .map(|x| pair(opt, x, ':'))
        .collect()
}

fn flag01(opt: &str, v: &str) -> Result<bool, OptError> {
    Ok(num::<i64>(opt, v)? != 0)
}

/// Read the command line (without the program name).
pub fn parse(args: &[String]) -> Result<Opts, OptError> {
    let mut o = Opts {
        args: args.join(" "),
        ..Opts::default()
    };
    let mut q: Vec<u32> = vec![0];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.as_str();
        let mut val = || {
            it.next()
                .map(String::as_str)
                .ok_or_else(|| OptError::Missing(a.to_owned()))
        };
        let gi = o.groups.len() - 1;
        let g = &mut o.groups[gi];
        match a {
            "--group" => {
                o.groups.push(Group::default());
                q.push(0);
            }
            "--input" => g.input = PathBuf::from(val()?),
            "--size" => (g.c.w, g.c.h) = pair(a, val()?, 'x')?,
            "--fps" => (g.c.fps_n, g.c.fps_d) = pair(a, val()?, '/')?,
            "--interlaced" => g.c.interlaced = true,
            "--bff" => g.c.tff = false,
            "--gop" => g.c.gop = num(a, val()?)?,
            "--bframes" => g.c.m = num::<u32>(a, val()?)? + 1,
            "--q" => q[gi] = num(a, val()?)?,
            "--qi" => g.c.fixed_q[0] = num(a, val()?)?,
            "--qp" => g.c.fixed_q[1] = num(a, val()?)?,
            "--qb" => g.c.fixed_q[2] = num(a, val()?)?,
            "--bitrate" => g.c.bitrate = num(a, val()?)?,
            "--max-bitrate" => g.c.max_bitrate = num(a, val()?)?,
            "--vbv" => g.c.vbv_bits = num(a, val()?)?,
            "--sessions" => g.n = num(a, val()?)?,
            "--frames" => g.frames = num::<i64>(a, val()?)?.max(0) as u64,
            "--stagger" => g.stagger = true,
            "--aq" => g.c.aq = flag01(a, val()?)?,
            "--aq-strength" => g.c.aq_strength = num(a, val()?)?,
            "--field-pred" => g.c.field_pred = flag01(a, val()?)?,
            "--field-dct" => g.c.field_dct = flag01(a, val()?)?,
            "--range" => g.c.range2 = num(a, val()?)?,
            "--lambda" => g.c.lambda = num(a, val()?)?,
            "--field-bits" => g.c.field_bits = num(a, val()?)?,
            "--smooth" => g.c.smooth = flag01(a, val()?)?,
            "--tpred" => g.c.tpred = flag01(a, val()?)?,
            "--intra-vlc" => g.c.intra_vlc = flag01(a, val()?)?,
            "--alt-scan" => {
                // the C++ field is an int: negative means "by the format"
                let v: i64 = num(a, val()?)?;
                g.c.alt_scan = (v >= 0).then_some(v != 0);
            }
            "--rate-plan" => g.rate_plan = pairs(a, val()?)?,
            "--rate-now" => g.rate_now = pairs(a, val()?)?,
            "--rate-walk" => {
                let v = val()?;
                let mut p = v.splitn(3, ':');
                let mut next = || {
                    p.next()
                        .ok_or_else(|| OptError::Value(a.to_owned(), v.to_owned()))
                };
                g.walk = Some(Walk {
                    lo: num(a, next()?)?,
                    hi: num(a, next()?)?,
                    seed: num(a, next()?)?,
                });
            }
            "--force-key" => {
                g.force_key = val()?
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(|x| num(a, x))
                    .collect::<Result<_, _>>()?;
            }
            "--cc" => g.cc = true,
            "--cc-text" => g.cc_text = Some(PathBuf::from(val()?)),
            "--film" => {
                let v = val()?;
                g.film = v
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(|x| match x {
                        "i" => Ok(FilmStep::Video),
                        "1" | "2" | "3" => Ok(FilmStep::Units(x.as_bytes()[0] - b'0')),
                        _ => Err(OptError::Value(a.to_owned(), v.to_owned())),
                    })
                    .collect::<Result<_, _>>()?;
            }
            "--pool" => g.pool = true,
            "--priority" => g.c.priority = num(a, val()?)?,
            "--conformance" => {
                let v = val()?;
                o.conformance = Profile::from_name(v)
                    .ok_or_else(|| OptError::Value(a.to_owned(), v.to_owned()))?;
            }
            "--aspect" => g.c.aspect = num(a, val()?)?,
            "--input-mode" => match val()? {
                "host" => o.dev_input = false,
                "dev" => o.dev_input = true,
                v => return Err(OptError::Value(a.to_owned(), v.to_owned())),
            },
            "--deadline" => o.deadline = true,
            "--test-deadline" => o.test_deadline_ns = num(a, val()?)?,
            "--test-progressive" => o.test_progressive = num(a, val()?)?,
            "--force-shed" => o.force_shed = pair(a, val()?, ':')?,
            "--hold" => {
                let v = val()?;
                let f: Vec<&str> = v.split(':').collect();
                let [g, s, from, to] = f.as_slice() else {
                    return Err(OptError::Value(a.to_owned(), v.to_owned()));
                };
                o.hold = Some(Hold {
                    g: num(a, g)?,
                    s: num(a, s)?,
                    from: num(a, from)?,
                    to: num(a, to)?,
                });
            }
            "--churn" => {
                let v = val()?;
                let mut p = v.splitn(3, ':');
                let mut next = || {
                    p.next()
                        .ok_or_else(|| OptError::Value(a.to_owned(), v.to_owned()))
                };
                o.churn = Some(Churn {
                    seed: num::<i64>(a, next()?)? as u32,
                    rate: num(a, next()?)?,
                });
                // `naive` and `reserve` chose how the C++ got the memory of a new batch (in the
                // tick, or on a side thread). Here a batch is laid out in the arena at once.
                match p.next() {
                    None | Some("naive") | Some("reserve") => {}
                    Some(_) => return Err(OptError::Value(a.to_owned(), v.to_owned())),
                }
            }
            "--churn-chunk" => o.churn_chunk = num(a, val()?)?,
            "--reap-after" => o.reap_after = num(a, val()?)?,
            "--oplog" => o.oplog = Some(PathBuf::from(val()?)),
            "--arena" => o.arena_mb = num(a, val()?)?,
            "--golden" => o.golden = Some(PathBuf::from(val()?)),
            "--output" => o.output = Some(val()?.to_owned()),
            "--recon" => o.recon = Some(PathBuf::from(val()?)),
            "--json" => o.json = Some(PathBuf::from(val()?)),
            "--stats" => o.stats = Some(PathBuf::from(val()?)),
            "--ticklog" => o.ticklog = Some(PathBuf::from(val()?)),
            "--write" => o.write_n = num::<i64>(a, val()?)?.max(0) as u32,
            "--profile" => o.profile = true,
            "--paced" => o.paced = true,
            "--overlap" => o.overlap = true,
            "--tickmark" => o.tickmark = true,
            "--blocking-wait" => o.blocking = true,
            "--max-in-frames" => o.max_in = num(a, val()?)?,
            "--warmup" => o.warmup = num::<i64>(a, val()?)?.max(0) as u64,
            "--log" => o.log = true,
            "--device" => {
                o.device = match val()? {
                    "cuda" => DeviceKind::Cuda,
                    "hip" => DeviceKind::Hip,
                    "ref" => DeviceKind::Ref,
                    v => return Err(OptError::Value(a.to_owned(), v.to_owned())),
                }
            }
            "--gpu" => o.gpu = num(a, val()?)?,
            "--check-arena" => o.check_arena = true,
            "--test-input-copy" => o.input_copy = true,
            "--device-timeout" => o.device_timeout = num(a, val()?)?,
            _ => return Err(OptError::Unknown(a.to_owned())),
        }
    }
    let chunk = o.churn_chunk;
    let conformance = o.conformance;
    for (gi, g) in o.groups.iter_mut().enumerate() {
        g.c.profile = conformance;
        if g.input.as_os_str().is_empty()
            || g.c.w == 0
            || g.c.h == 0
            || g.c.fps_n == 0
            || g.c.fps_d == 0
            || g.n == 0
        {
            return Err(OptError::Group(gi));
        }
        finish_group(g, q[gi], chunk);
    }
    Ok(o)
}

/// The derived values of a group.
fn finish_group(g: &mut Group, q: u32, churn_chunk: u32) {
    if q != 0 {
        let q8 = q.min(255) as u8;
        g.c.fixed_q = [q8, q8, ((q * 5 + 2) / 4).min(31) as u8];
    }
    // The level the format needs (a format that has none is refused when the session opens).
    let level = profile::frame_rate_code(g.c.fps_n, g.c.fps_d)
        .and_then(|code| Level::lowest(g.c.w, g.c.h, g.c.interlaced, code))
        .unwrap_or(Level::High);
    if g.c.bitrate > 0 {
        if g.c.max_bitrate == 0 {
            let hi = g.walk.map_or(0, |w| w.hi as i64).max(0) as u64;
            g.c.max_bitrate = g.c.bitrate.max(hi);
            for (_, r) in g.rate_plan.iter().chain(&g.rate_now) {
                g.c.max_bitrate = g.c.max_bitrate.max(*r);
            }
        }
        if g.c.vbv_bits == 0 {
            // half a second of the ceiling, within what the level and the profile allow
            g.c.vbv_bits = (g.c.max_bitrate / 2).min(profile::max_vbv_bits(
                level,
                g.c.profile,
                g.c.max_bitrate,
            ));
            g.c.vbv_bits = g.c.vbv_bits / profile::VBV_UNIT * profile::VBV_UNIT;
        }
    } else {
        // Fixed quantizer: the stream declares the largest rate and buffer its level and
        // profile have, unless the command line says otherwise.
        if g.c.max_bitrate == 0 {
            g.c.max_bitrate = profile::max_bit_rate(level, g.c.profile);
        }
        if g.c.vbv_bits == 0 {
            g.c.vbv_bits = profile::max_vbv_bits(level, g.c.profile, g.c.max_bitrate);
        }
    }
    g.c.capacity = if g.pool { churn_chunk } else { g.n };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_gate_command_of_a_standard_case() {
        let o = parse(&words(
            "--input media/crowd_run_480i.yuv --size 704x480 --fps 30000/1001 --gop 15 \
             --bframes 2 --bitrate 4000000 --vbv 1835008 --interlaced --frames 150 \
             --output out/check/x --recon out/x.recon.yuv --json out/x.json --stats out/x.jsonl",
        ))
        .unwrap();
        let g = &o.groups[0];
        assert_eq!(
            (g.c.w, g.c.h, g.c.fps_n, g.c.fps_d),
            (704, 480, 30000, 1001)
        );
        assert!(g.c.interlaced && g.c.tff);
        assert_eq!((g.c.gop, g.c.m, g.frames, g.n), (15, 3, 150, 1));
        assert_eq!(
            (g.c.bitrate, g.c.max_bitrate, g.c.vbv_bits),
            (4_000_000, 4_000_000, 1_835_008)
        );
        assert_eq!(g.c.capacity, 1);
        assert_eq!(o.output.as_deref(), Some("out/check/x"));
        assert_eq!(o.force_shed, (0.0, 1));
        assert_eq!(o.device, DeviceKind::Cuda);
    }

    #[test]
    fn derived_rates_and_buffers() {
        // the ceiling is the largest rate of any plan; the buffer half of it, in 16384s
        let o = parse(&words(
            "--input a --size 176x144 --bitrate 300000 --rate-plan 2:150000,3:500000 \
             --rate-now 8:650001",
        ))
        .unwrap();
        let c = &o.groups[0].c;
        assert_eq!(c.max_bitrate, 650_001);
        assert_eq!(c.vbv_bits, 325_000 / 16384 * 16384);
        assert_eq!(o.groups[0].rate_plan, [(2, 150_000), (3, 500_000)]);
        // the level's and the profile's buffer limit the default
        let o = parse(&words("--input a --size 1920x1080 --bitrate 38000000")).unwrap();
        assert_eq!(o.groups[0].c.vbv_bits, 7_995_392);
        let o = parse(&words(
            "--input a --size 1920x1080 --bitrate 80000000 --conformance h262",
        ))
        .unwrap();
        assert_eq!(o.groups[0].c.vbv_bits, 9_781_248);
        assert_eq!(o.groups[0].c.profile, Profile::H262);
        // 720p at 29.97 Hz is High-1440 level: 448 units
        let o = parse(&words("--input a --size 1280x720 --bitrate 20000000")).unwrap();
        assert_eq!(o.groups[0].c.vbv_bits, 448 * 16384);
        // an explicit buffer is not rounded: one that is no whole number of units is refused
        // when the session opens
        let o = parse(&words(
            "--input a --size 704x480 --bitrate 4000000 --vbv 1835000",
        ))
        .unwrap();
        assert_eq!(o.groups[0].c.vbv_bits, 1_835_000);
        let o = parse(&words(
            "--input a --size 704x480 --bitrate 3000000 --rate-walk 1000000:9000000:3",
        ))
        .unwrap();
        assert_eq!(o.groups[0].c.max_bitrate, 9_000_000);
        assert_eq!(o.groups[0].c.vbv_bits, 1_835_008);
        // fixed quantizer
        let o = parse(&words("--input a --size 184x144 --q 6 --force-key 7")).unwrap();
        assert_eq!(o.groups[0].c.fixed_q, [6, 6, 8]);
        assert_eq!(o.groups[0].c.bitrate, 0);
        // which declares what Main level allows (H.262 Tables 8-13, 8-14)
        assert_eq!(
            (o.groups[0].c.max_bitrate, o.groups[0].c.vbv_bits),
            (15_000_000, 1_835_008)
        );
        let hd = parse(&words("--input a --size 1920x1080 --interlaced --q 6")).unwrap();
        assert_eq!(
            (hd.groups[0].c.max_bitrate, hd.groups[0].c.vbv_bits),
            (38_800_000, 7_995_392)
        );
        assert_eq!(o.groups[0].force_key, [7]);
        let o = parse(&words("--input a --size 184x144 --q 30")).unwrap();
        assert_eq!(o.groups[0].c.fixed_q, [30, 30, 31]);
    }

    #[test]
    fn groups_stagger_and_pools() {
        let o = parse(&words(
            "--input a --size 1920x1080 --sessions 4 --stagger --gop 15 --group --input b \
             --size 704x480 --sessions 6 --pool --churn 5:6:reserve --churn-chunk 2 --write 99 \
             --hold 0:1:10:16 --force-shed 0.3:7 --device ref",
        ))
        .unwrap();
        assert_eq!(o.groups.len(), 2);
        let phases: Vec<u32> = (0..4)
            .map(|s| o.groups[0].session_cfg(s).gop_phase)
            .collect();
        assert_eq!(phases, [0, 3, 7, 11]);
        assert_eq!(o.groups[0].c.capacity, 4);
        assert!(o.groups[1].pool);
        assert_eq!(o.groups[1].c.capacity, 2);
        assert_eq!(o.churn, Some(Churn { seed: 5, rate: 6.0 }));
        assert_eq!(
            o.hold,
            Some(Hold {
                g: 0,
                s: 1,
                from: 10,
                to: 16
            })
        );
        assert_eq!(o.force_shed, (0.3, 7));
        assert_eq!(o.device, DeviceKind::Ref);
    }

    #[test]
    fn refusals() {
        assert_eq!(parse(&words("--size 16x16")), Err(OptError::Group(0)));
        assert!(matches!(
            parse(&words("--input a --size 16x16 --input-mode disk")),
            Err(OptError::Value(..))
        ));
        assert_eq!(
            parse(&words("--input a --size 16x16 --frobnicate")),
            Err(OptError::Unknown("--frobnicate".to_owned()))
        );
        assert_eq!(
            parse(&words("--input a --size 16x16 --gop")),
            Err(OptError::Missing("--gop".to_owned()))
        );
        assert!(matches!(
            parse(&words("--input a --size 16by16")),
            Err(OptError::Value(..))
        ));
    }
}
