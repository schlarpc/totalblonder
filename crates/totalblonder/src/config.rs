//! The configuration file (SPEC §13.2): TOML, unknown keys refused, validated as a whole into a
//! [`Plant`] before anything starts. Every rule that can be checked without running is checked
//! here, so the runtime only meets valid configurations.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use serde::Deserialize;
use tb_mux::{QAM256_PAYLOAD, TsRate, ts_rate_for};
use tb_output::frame::PerDatagram;
use tb_psip::build::GuideConfig;
use tb_psip::descriptor::{Ac3Descriptor, CaptionKind, CaptionService, ServiceNumber};
use tb_psip::gps::{DstRule, LeapSeconds};
use tb_psip::lineup::{
    Audio, Carrier, Channel, ChannelNumber, EsPid, EtmLocation, Program, ProgramNumber,
    ServiceType, SourceId, Video, VideoCodec,
};
use tb_psip::text::{Lang, Mss, ShortName};

/// A configuration that cannot run; the text says why.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file does not parse.
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// A rule is broken.
    #[error("{0}")]
    Rule(String),
    /// A table cannot be built from the lineup.
    #[error("lineup: {0}")]
    Lineup(#[from] tb_psip::Error),
}

fn rule(s: impl Into<String>) -> ConfigError {
    ConfigError::Rule(s.into())
}

// ------------------------------------------------------------------ the file's schema

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    output: OutputFile,
    #[serde(default)]
    psip: PsipFile,
    #[serde(default)]
    video: VideoFile,
    #[serde(default)]
    control: ControlFile,
    #[serde(default)]
    persistence: PersistenceFile,
    #[serde(default)]
    statmux: StatmuxFile,
    #[serde(default)]
    overload: OverloadFile,
    device: Option<DeviceFile>,
    #[serde(default)]
    telemetry: TelemetryFile,
    #[serde(default, rename = "carrier")]
    carriers: Vec<CarrierFile>,
    #[serde(default, rename = "service")]
    services: Vec<ServiceFile>,
    #[serde(default, rename = "input")]
    inputs: Vec<InputFile>,
    #[serde(default, rename = "rating_system")]
    rating_systems: Vec<RatingSystemFile>,
}

/// A rating system of our own (SPEC §7.7): sent as an RRT, used by schedule events.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RatingSystemFile {
    region: u8,
    name: String,
    #[serde(rename = "dimension")]
    dimensions: Vec<RatingDimensionFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RatingDimensionFile {
    name: String,
    #[serde(default = "yes")]
    graduated: bool,
    values: Vec<RatingValueFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RatingValueFile {
    abbrev: String,
    #[serde(default)]
    text: String,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct OutputFile {
    ts_rate: u64,
    null_floor: f64,
    packets_per_datagram: u8,
    framing: String,
    pcr_interval_ms: u64,
    mux_lead_ms: u64,
    delay_ms: u64,
    self_check: bool,
    #[serde(rename = "interface")]
    interfaces: Vec<InterfaceFile>,
}

impl Default for OutputFile {
    fn default() -> Self {
        OutputFile {
            ts_rate: TsRate::DEFAULT.bps(),
            null_floor: 0.02,
            packets_per_datagram: 7,
            framing: "udp".into(),
            pcr_interval_ms: 30,
            mux_lead_ms: 20,
            delay_ms: 1100,
            self_check: true,
            interfaces: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfaceFile {
    name: String,
    /// Local address: the multicast interface, or the unicast source address.
    address: Option<Ipv4Addr>,
    carriers: Vec<u8>,
    #[serde(default = "one")]
    multicast_ttl: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct PsipFile {
    gps_utc_offset: u8,
    dst_rule: String,
    eit_windows: u8,
    ett_unique_extension: bool,
    service_location: bool,
    placeholders: bool,
    /// The guide's schedule file (JSON, see `schedule.rs`).
    schedule: Option<PathBuf>,
}

impl Default for PsipFile {
    fn default() -> Self {
        let g = GuideConfig::default();
        PsipFile {
            gps_utc_offset: 18,
            dst_rule: "us".into(),
            eit_windows: g.windows,
            ett_unique_extension: g.ett_unique_extension,
            service_location: g.service_location,
            placeholders: g.placeholders,
            schedule: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct VideoFile {
    device: String,
    gpu: u32,
}

impl Default for VideoFile {
    fn default() -> Self {
        VideoFile {
            device: "cuda".into(),
            gpu: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ControlFile {
    bind: Option<std::net::SocketAddr>,
}

impl Default for ControlFile {
    fn default() -> Self {
        ControlFile {
            bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 8090))),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct TelemetryFile {
    logs: Option<String>,
    metrics: Option<String>,
    log_level: String,
    metrics_interval_s: u64,
    summary_interval_s: Option<u64>,
    timeout_s: u64,
    service_name: String,
    instance: Option<String>,
    resource: std::collections::BTreeMap<String, String>,
    headers: std::collections::BTreeMap<String, String>,
}

impl Default for TelemetryFile {
    fn default() -> Self {
        TelemetryFile {
            logs: None,
            metrics: None,
            log_level: "info".into(),
            metrics_interval_s: 10,
            summary_interval_s: None,
            timeout_s: 5,
            service_name: "totalblonder".into(),
            instance: None,
            resource: Default::default(),
            headers: Default::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceFile {
    base_url: String,
    /// Two lines: user name, password.
    credentials_file: PathBuf,
    #[serde(default = "report")]
    reconcile: String,
    #[serde(default = "thirty")]
    status_interval_s: u64,
    level_dbmv: u8,
    rf_module: Vec<RfModuleFile>,
    defect_d_limit_h: Option<u64>,
}

fn report() -> String {
    "report".into()
}
fn thirty() -> u64 {
    30
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RfModuleFile {
    module: u8,
    first_channel: u8,
    #[serde(default = "q256b")]
    qam: String,
    #[serde(default = "std_plan")]
    plan: String,
    #[serde(default = "all_on")]
    carriers_on: [bool; 4],
}

fn q256b() -> String {
    "256B".into()
}
fn std_plan() -> String {
    "STD".into()
}
fn all_on() -> [bool; 4] {
    [true; 4]
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct PersistenceFile {
    /// Where table versions are kept across restarts.
    versions: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierFile {
    id: u8,
    tsid: Option<u16>,
    /// `udp://a.b.c.d:port`, `rtp://a.b.c.d:port` or `file:/path.ts`.
    destination: String,
    #[serde(default, rename = "program")]
    programs: Vec<ProgramFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramFile {
    number: u16,
    service: String,
    video: VideoProgFile,
    #[serde(default)]
    audio: Vec<AudioFile>,
    #[serde(default)]
    captions: bool,
    /// A SubRip or WebVTT file sent as CEA-608 CC1 and CEA-708 service 1 (times from the
    /// program's start).
    subtitles: Option<PathBuf>,
    /// Film mode: `auto` codes film sources as film frames with display flags (3:2 by
    /// repeat_first_field), `off` codes every frame period.
    #[serde(default)]
    film: Option<String>,
    /// An input shown while the live input is away (SPEC §9.5 level 2).
    standby: Option<String>,
    live: Option<String>,
    /// The formats this program may step down to under sustained overload, after its own
    /// (SPEC §12.1): default 480i for a program above 480i, `[]` for none.
    ladder: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VideoProgFile {
    format: String,
    max_kbps: u64,
    /// Statmux floor (default a quarter of the ceiling, at least 1 Mbit/s).
    min_kbps: Option<u64>,
    /// Statmux weight (default 1).
    weight: Option<f64>,
    #[serde(default)]
    priority: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct OverloadFile {
    enabled: bool,
    window_s: f64,
    threshold: f64,
    recover_s: f64,
}

impl Default for OverloadFile {
    fn default() -> Self {
        OverloadFile {
            enabled: true,
            window_s: 10.0,
            threshold: 0.02,
            recover_s: 300.0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct StatmuxFile {
    enabled: bool,
    alpha: f64,
    max_step: f64,
    hysteresis: f64,
}

impl Default for StatmuxFile {
    fn default() -> Self {
        StatmuxFile {
            enabled: false,
            alpha: 0.85,
            max_step: 0.15,
            hysteresis: 0.03,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioFile {
    #[serde(default = "eng")]
    language: String,
    #[serde(default = "stereo")]
    layout: String,
    #[serde(default = "k192")]
    kbps: u32,
    name: Option<String>,
    /// Which of the live input's audio streams (1-based) feeds this track; by default the
    /// one in the track's language (`audio_routes`).
    input_track: Option<u8>,
}

fn eng() -> String {
    "eng".into()
}
fn stereo() -> String {
    "stereo".into()
}
fn k192() -> u32 {
    192
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceFile {
    id: String,
    name: String,
    long_name: Option<String>,
    channel: String,
    source_id: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFile {
    id: String,
    kind: String,
    bind: Option<std::net::SocketAddr>,
    app: Option<String>,
    key: Option<String>,
    path: Option<PathBuf>,
    /// SRT: the passphrase (10 to 79 characters) a caller must use; none: unencrypted.
    passphrase: Option<String>,
    /// SRT: the receive latency, ms (default 200).
    latency_ms: Option<u64>,
    /// LL-HLS: the playlist URL.
    url: Option<String>,
}

// ------------------------------------------------------------------ the validated model

/// An output video format (SPEC §8.2, the formats the encoder accepts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// 1920×1080 interlaced, 29.97 frames/s.
    F1080i,
    /// 1920×1080 progressive, 29.97 frames/s.
    F1080p,
    /// 1280×720 progressive, 59.94 frames/s.
    F720p,
    /// 704×480 interlaced, 29.97 frames/s.
    F480i,
}

impl Format {
    /// `1080i`, `1080p`, `720p` or `480i`, as the file names it.
    pub fn name(self) -> &'static str {
        match self {
            Format::F1080i => "1080i",
            Format::F1080p => "1080p",
            Format::F720p => "720p",
            Format::F480i => "480i",
        }
    }

    fn parse(s: &str) -> Result<Format, ConfigError> {
        Ok(match s {
            "1080i29.97" | "1080i" => Format::F1080i,
            "1080p29.97" | "1080p" => Format::F1080p,
            "720p59.94" | "720p" => Format::F720p,
            "480i29.97" | "480i" => Format::F480i,
            _ => {
                return Err(rule(format!(
                    "unknown video format {s:?} (1080i, 1080p, 720p, 480i)"
                )));
            }
        })
    }
    /// Width, height.
    pub fn size(self) -> (u32, u32) {
        match self {
            Format::F1080i | Format::F1080p => (1920, 1080),
            Format::F720p => (1280, 720),
            Format::F480i => (704, 480),
        }
    }
    /// Frame rate as a fraction.
    pub fn fps(self) -> (u32, u32) {
        match self {
            Format::F720p => (60_000, 1001),
            _ => (30_000, 1001),
        }
    }
    /// Display aspect ratio of the coded frame, width : height: what the encoder signals in
    /// aspect_ratio_information (its default for the size: 4:3 at 480 lines, 16:9 above;
    /// `tb_m2v_syntax::profile`).
    pub fn display_aspect(self) -> (u32, u32) {
        match self {
            Format::F480i => (4, 3),
            _ => (16, 9),
        }
    }
    /// Interlaced.
    pub fn interlaced(self) -> bool {
        matches!(self, Format::F1080i | Format::F480i)
    }
    /// Pictures per GOP: 0.5 s (SPEC §8.2).
    pub fn gop(self) -> u32 {
        match self {
            Format::F720p => 30,
            _ => 15,
        }
    }
    /// The transport buffer's drain rate Rx: 1.2 × Rmax of the level (H.222.0 §2.4.2.3;
    /// Rmax 80 Mbit/s at High level, 15 Mbit/s at Main level).
    pub fn rx_bps(self) -> u64 {
        match self {
            Format::F480i => 18_000_000,
            _ => 96_000_000,
        }
    }
    /// The largest vbv_buffer_size, bits: 112 × 16,384 at Main level (H.262 Table 8-14) and
    /// 488 × 16,384 at High level (A/53 Part 4 Table 6.2 limit, SPEC §8.2).
    pub fn max_vbv(self) -> u64 {
        match self {
            Format::F480i => 112 * 16_384,
            _ => 488 * 16_384,
        }
    }
    /// The highest ceiling: the level's rate at Main level (15 Mbit/s), and at High level the
    /// buffer limit at D = 0.5 s (vbv_buffer_size ≤ 488 × 16,384 bits; SPEC §8.2).
    pub fn max_bps(self) -> u64 {
        match self {
            Format::F480i => 15_000_000,
            _ => 15_990_784,
        }
    }
}

/// How the video encoder runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDevice {
    /// An NVIDIA card (CUDA), by ordinal.
    Cuda(u32),
    /// An AMD card (HIP), by ordinal.
    Hip(u32),
    /// The kernels compiled for the host: slow, for tests and offline runs.
    Ref,
}

/// Where a carrier goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// UDP to an address (multicast or unicast).
    Udp(SocketAddrV4),
    /// RTP to an address.
    Rtp(SocketAddrV4),
    /// A file of TS packets.
    File(PathBuf),
}

/// One audio track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPlan {
    /// Language.
    pub lang: Lang,
    /// Channels: 1 (1/0), 2 (2/0) or 6 (3/2 + LFE).
    pub channels: u8,
    /// Bit rate, kbit/s (an A/52 rate ≤ 448).
    pub kbps: u32,
    /// `input_track`: an explicit 1-based position among the live input's audio streams.
    pub input_track: Option<u8>,
    /// `layout = "auto"`: the coding mode follows the source (mono, stereo or 5.1; the
    /// descriptor says "at most 6 channels"); `channels` is then 6.
    pub auto: bool,
    /// An auto track's rate in each mode, kbit/s: mono, stereo, 5.1 (`kbps`, the ceiling;
    /// the others at most 96 and 192). With the statmux the difference to the ceiling goes to
    /// video while it is not used.
    pub mode_kbps: [u32; 3],
}

impl AudioPlan {
    /// The rate of the mode for `channels` (1, 2 or 6), kbit/s.
    pub fn kbps_for(&self, channels: usize) -> u32 {
        match channels {
            1 => self.mode_kbps[0],
            2 => self.mode_kbps[1],
            _ => self.mode_kbps[2],
        }
    }
}

/// One program as the runtime needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramPlan {
    /// Index of the program within its carrier.
    pub index: usize,
    /// The service id from the file.
    pub service: String,
    /// Its PSIP description (PIDs, numbers, names).
    pub psip: Program,
    /// Output format.
    pub format: Format,
    /// The encoder's ceiling, bit/s.
    pub max_bps: u64,
    /// The statmux floor, bit/s.
    pub min_bps: u64,
    /// The statmux weight.
    pub weight: f64,
    /// The encoder's buffer, bits (`max_bps` × 0.5 s, within the level's limit).
    pub vbv_bits: u64,
    /// Deadline-miss priority.
    pub priority: i32,
    /// Audio tracks.
    pub audio: Vec<AudioPlan>,
    /// The live input feeding it, if any.
    pub live: Option<String>,
    /// Timed text to caption, if any.
    pub subtitles: Option<PathBuf>,
    /// Film mode (`film = "auto"`).
    pub film: bool,
    /// The standby input, if any.
    pub standby: Option<String>,
    /// Its formats, best first: `ladder[0]` is `format` as configured, the others what it
    /// may step down to under sustained overload (SPEC §12.1).
    pub ladder: Vec<Format>,
    /// Which of `ladder` it is on now, shared by every copy of the plan (the video hub sets
    /// it, the program's decoder threads follow it).
    pub rung: Rung,
}

/// The index of a program's current format in its ladder, shared between threads.
#[derive(Debug, Clone, Default)]
pub struct Rung(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Rung {
    /// The rung now.
    pub fn get(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Moves to rung `r`.
    pub fn set(&self, r: usize) {
        self.0.store(r, std::sync::atomic::Ordering::Release);
    }
}

impl PartialEq for Rung {
    /// Shared state, not configuration: plans compare equal whatever rung they are on.
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// How the video hub reacts to sustained overload (SPEC §12.1, `[overload]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverloadPlan {
    /// Step-down on.
    pub enabled: bool,
    /// The window over which pictures given up are counted, process ticks.
    pub window: u64,
    /// The share of pictures given up in the window that steps a program down.
    pub threshold: f64,
    /// Time without a picture given up before a program steps back up, process ticks.
    pub recover: u64,
}

impl ProgramPlan {
    /// The same program in format `f` (a rung of its ladder): its ceiling within the format's
    /// limit, its floor within its ceiling, its buffer derived as at configuration.
    pub fn at(&self, f: Format) -> ProgramPlan {
        let max_bps = self.max_bps.min(f.max_bps());
        let min_bps = self.min_bps.min(max_bps);
        let vbv_bits = ((max_bps / 2).min(f.max_vbv()) / 16_384) * 16_384;
        ProgramPlan {
            format: f,
            max_bps,
            min_bps,
            vbv_bits,
            ..self.clone()
        }
    }

    /// The video delivery rate of the mux at the ceiling, bit/s.
    pub fn video_ts_bps(&self, pcr_ms: u64) -> u64 {
        self.ts_bps_for(self.max_bps, pcr_ms)
    }

    /// The transport rate that delivers `es_bps` of this program's video.
    pub fn ts_bps_for(&self, es_bps: u64, pcr_ms: u64) -> u64 {
        let (n, d) = self.format.fps();
        ts_rate_for(es_bps, u64::from(n), u64::from(d), pcr_ms)
    }
}

/// One carrier.
#[derive(Debug, Clone, PartialEq)]
pub struct CarrierPlan {
    /// 1..=16: the modulator's input slot.
    pub id: u8,
    /// Where its stream goes.
    pub destination: Destination,
    /// The lineup the tables are built from.
    pub lineup: Carrier,
    /// The programs, in lineup order.
    pub programs: Vec<ProgramPlan>,
    /// Interface (index into [`OutputPlan::interfaces`]), if any.
    pub interface: Option<usize>,
    /// Transport rate left for video after audio, tables and the null floor (the statmux
    /// pool, before conversion to elementary rates).
    pub video_pool_ts: u64,
}

/// Statistical multiplexing settings (SPEC §8.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatmuxPlan {
    /// Exponent α.
    pub alpha: f64,
    /// Largest change per tick.
    pub max_step: f64,
    /// Smallest change made.
    pub hysteresis: f64,
}

/// One output interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfacePlan {
    /// Its name.
    pub name: String,
    /// Local address.
    pub address: Option<Ipv4Addr>,
    /// Multicast TTL.
    pub ttl: u32,
}

/// Output settings.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputPlan {
    /// Carrier rate.
    pub rate: TsRate,
    /// Packets per datagram.
    pub per: PerDatagram,
    /// RTP framing (else UDP) for destinations that do not say.
    pub pcr_interval_ms: u64,
    /// Mux lead, ms.
    pub lead_ms: u64,
    /// The constant delay Δ from a frame's capture deadline to its presentation (SPEC §5.2).
    pub delay_ms: u64,
    /// Interfaces.
    pub interfaces: Vec<InterfacePlan>,
    /// Null floor kept free, fraction.
    pub null_floor: f64,
    /// Run the independent checker on every carrier's output (SPEC §11.2).
    pub self_check: bool,
}

/// A live input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPlan {
    /// An RTMP listener accepting one publisher on `app`/`key`.
    Rtmp {
        /// The input's id.
        id: String,
        /// Where to listen.
        bind: std::net::SocketAddr,
        /// Application name.
        app: String,
        /// Stream key.
        key: String,
    },
    /// MPEG-TS over UDP or RTP.
    TsUdp {
        /// The input's id.
        id: String,
        /// Where to listen.
        bind: std::net::SocketAddr,
    },
    /// MPEG-TS over SRT, as a listener serving one caller at a time.
    Srt {
        /// The input's id.
        id: String,
        /// Where to listen.
        bind: std::net::SocketAddr,
        /// The passphrase callers must use (AES-128), if any.
        passphrase: Option<String>,
        /// The receive latency.
        latency: std::time::Duration,
    },
    /// A file, looped (for tests and standby).
    File {
        /// The input's id.
        id: String,
        /// The file.
        path: PathBuf,
    },
}

impl InputPlan {
    /// `rtmp`, `ts_udp` or `file`, as the file names the kinds.
    pub fn kind(&self) -> &'static str {
        match self {
            InputPlan::Rtmp { .. } => "rtmp",
            InputPlan::TsUdp { .. } => "ts_udp",
            InputPlan::Srt { .. } => "srt",
            InputPlan::File { .. } => "file",
        }
    }

    /// The input's id.
    pub fn id(&self) -> &str {
        match self {
            InputPlan::Rtmp { id, .. }
            | InputPlan::TsUdp { id, .. }
            | InputPlan::Srt { id, .. }
            | InputPlan::File { id, .. } => id,
        }
    }
}

/// The whole validated configuration.
#[derive(Debug, Clone)]
pub struct Plant {
    /// Output.
    pub output: OutputPlan,
    /// GPS − UTC.
    pub leap: LeapSeconds,
    /// Daylight saving.
    pub dst: DstRule,
    /// Guide.
    pub guide: GuideConfig,
    /// Video device.
    pub video: VideoDevice,
    /// Control API address.
    pub control: Option<std::net::SocketAddr>,
    /// The table version file.
    pub versions: Option<PathBuf>,
    /// The modulator's supervisor, if configured.
    pub device: Option<DevicePlan>,
    /// The guide's schedule file.
    pub schedule: Option<PathBuf>,
    /// Statistical multiplexing, if enabled.
    pub statmux: Option<StatmuxPlan>,
    /// Carriers.
    pub carriers: Vec<CarrierPlan>,
    /// Inputs.
    pub inputs: Vec<InputPlan>,
    /// Rating systems of our own, one RRT each, regions distinct.
    pub ratings: Vec<tb_psip::rating::RatingSystem>,
    /// Step-down under sustained overload.
    pub overload: OverloadPlan,
    /// Telemetry.
    pub telemetry: TelemetryPlan,
}

/// Telemetry settings (`[telemetry]`, docs/SERVER.md "Telemetry").
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryPlan {
    /// The exporters.
    pub export: tb_telemetry::Config,
    /// How often the carrier, service, input and process summaries are reported; `None`: never.
    pub summaries: Option<std::time::Duration>,
}

fn telemetry_plan(t: &TelemetryFile) -> Result<TelemetryPlan, ConfigError> {
    let url = |what: &str, u: &Option<String>| -> Result<Option<String>, ConfigError> {
        match u {
            Some(u) if !(u.starts_with("http://") || u.starts_with("https://")) => Err(rule(
                format!("telemetry.{what}: {u:?} is not an http:// or https:// URL"),
            )),
            Some(u) => Ok(Some(u.clone())),
            None => Ok(None),
        }
    };
    let log_level = match t.log_level.as_str() {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "info" => tracing::Level::INFO,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        x => {
            return Err(rule(format!(
                "telemetry.log_level: {x:?} is not trace, debug, info, warn or error"
            )));
        }
    };
    if t.metrics_interval_s == 0 || t.timeout_s == 0 {
        return Err(rule(
            "telemetry: metrics_interval_s and timeout_s must be at least 1",
        ));
    }
    let export = tb_telemetry::Config {
        logs: url("logs", &t.logs)?,
        metrics: url("metrics", &t.metrics)?,
        log_level,
        metrics_interval: std::time::Duration::from_secs(t.metrics_interval_s),
        timeout: std::time::Duration::from_secs(t.timeout_s),
        service_name: t.service_name.clone(),
        instance: t.instance.clone(),
        resource: t.resource.clone().into_iter().collect(),
        headers: t.headers.clone().into_iter().collect(),
    };
    // Summaries by default only when there is somewhere for them to go.
    let every = t
        .summary_interval_s
        .unwrap_or(if export.logs.is_some() { 10 } else { 0 });
    Ok(TelemetryPlan {
        summaries: (every > 0).then(|| std::time::Duration::from_secs(every)),
        export,
    })
}

/// The modulator's supervisor settings (SPEC §10, §13.2 `[device]`).
#[derive(Debug, Clone)]
pub struct DevicePlan {
    /// `http://10.42.0.184`.
    pub base_url: String,
    /// Where the credentials are read from at start (never from this file).
    pub credentials_file: PathBuf,
    /// Supervisor settings.
    pub supervisor: tb_eqam::supervisor::SupervisorConfig,
    /// What the box should hold: the RF plan and one stream per carrier.
    pub desired: tb_eqam::plan::DesiredState,
}

impl PartialEq for DevicePlan {
    fn eq(&self, o: &Self) -> bool {
        self.base_url == o.base_url && self.credentials_file == o.credentials_file
    }
}

fn device_plan(d: &DeviceFile, carriers: &[CarrierPlan]) -> Result<DevicePlan, ConfigError> {
    use tb_eqam::plan::{DesiredState, ModulePlan, RfPlan, SlotConfig, StreamTable};
    use tb_eqam::supervisor::{ReconcileMode, SupervisorConfig};
    use tb_eqam::types::{FirstChannel, OutputLevel, Proto, Slot, StreamName, StreamUrl};
    let bad = |what: &str, e: &dyn std::fmt::Display| rule(format!("[device] {what}: {e}"));
    let mode = match d.reconcile.as_str() {
        "report" => ReconcileMode::Report,
        "apply" => ReconcileMode::Apply,
        x => return Err(rule(format!("[device] reconcile {x:?}: report or apply"))),
    };
    if d.rf_module.len() != 4 {
        return Err(rule("[device] needs exactly four rf_module entries"));
    }
    let mut mods = Vec::new();
    for want in 1..=4u8 {
        let m = d
            .rf_module
            .iter()
            .find(|m| m.module == want)
            .ok_or_else(|| rule(format!("[device] rf_module {want} missing")))?;
        mods.push(ModulePlan {
            first_channel: FirstChannel::new(m.first_channel)
                .map_err(|e| bad("first_channel", &e))?,
            qam: m.qam.parse().map_err(|e| bad("qam", &e))?,
            map: m.plan.parse().map_err(|e| bad("plan", &e))?,
            carriers_on: m.carriers_on,
        });
    }
    let modules: [ModulePlan; 4] = [mods[0], mods[1], mods[2], mods[3]];
    let rf = RfPlan::new(
        OutputLevel::new(d.level_dbmv).map_err(|e| bad("level_dbmv", &e))?,
        modules,
    )
    .map_err(|e| bad("rf plan", &e))?;
    let mut streams = StreamTable::empty();
    let mut programs = std::collections::BTreeMap::new();
    for c in carriers {
        let (proto, a) = match &c.destination {
            Destination::Udp(a) => (Proto::Udp, a),
            Destination::Rtp(a) => (Proto::Rtp, a),
            Destination::File(_) => continue,
        };
        let url = StreamUrl::new(proto, *a.ip(), a.port()).map_err(|e| bad("stream URL", &e))?;
        let name: StreamName = format!("TB{}", c.id)
            .parse()
            .map_err(|e| bad("stream name", &e))?;
        let slot = Slot::new(c.id).map_err(|e| bad("slot", &e))?;
        streams = streams
            .with(slot, SlotConfig::Assigned { url, name })
            .map_err(|e| bad("streams", &e))?;
        programs.insert(slot, stream_info_programs(&c.lineup));
    }
    Ok(DevicePlan {
        base_url: d.base_url.clone(),
        credentials_file: d.credentials_file.clone(),
        supervisor: SupervisorConfig {
            mode,
            status_interval: std::time::Duration::from_secs(d.status_interval_s.max(10)),
            defect_d_limit: d
                .defect_d_limit_h
                .map(|h| std::time::Duration::from_secs(h * 3600)),
            ..SupervisorConfig::default()
        },
        desired: DesiredState {
            rf,
            streams,
            programs,
        },
    })
}

/// A carrier's programs as the modulator's Stream Info page should list them.
fn stream_info_programs(lineup: &Carrier) -> Vec<tb_eqam::pages::streaminfo::DetectedProgram> {
    use tb_eqam::pages::streaminfo::{DetectedEs, DetectedProgram};
    lineup
        .programs()
        .iter()
        .map(|p| {
            let video = match p.video.codec {
                VideoCodec::Mpeg2 => "MPEG-2 Video",
                VideoCodec::H264 { .. } => "AVC/H.264 Video",
            };
            let mut streams = vec![DetectedEs {
                pid: p.video.pid.pid().get(),
                kind: video.into(),
            }];
            streams.extend(p.audio.iter().map(|a| DetectedEs {
                pid: a.pid.pid().get(),
                kind: "AC3 Audio".into(),
            }));
            DetectedProgram {
                pmt_pid: p.pmt_pid.pid().get(),
                program_number: p.number.get(),
                streams,
            }
        })
        .collect()
}

/// A/52 Table 5.18 rates, kbit/s, by `frmsizecod / 2`.
pub const AC3_RATES: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

/// Bytes the transport stream spends on one AC-3 frame of `kbps` at 48 kHz: the frame
/// (`kbps` × 4 bytes), a 14-byte PES header, whole packets (SPEC §6.2).
pub fn ac3_ts_bps(kbps: u32) -> u64 {
    let frame = u64::from(kbps) * 4;
    let packets = (frame + 14).div_ceil(184);
    // 31.25 frames per second.
    packets * 1504 * 125 / 4
}

fn parse_dest(s: &str) -> Result<Destination, ConfigError> {
    let addr = |a: &str| -> Result<SocketAddrV4, ConfigError> {
        a.parse().map_err(|_| rule(format!("bad address {a:?}")))
    };
    if let Some(a) = s
        .strip_prefix("udp://")
        .or_else(|| s.strip_prefix("UDP://"))
    {
        Ok(Destination::Udp(addr(a)?))
    } else if let Some(a) = s
        .strip_prefix("rtp://")
        .or_else(|| s.strip_prefix("RTP://"))
    {
        Ok(Destination::Rtp(addr(a)?))
    } else if let Some(p) = s.strip_prefix("file:") {
        Ok(Destination::File(PathBuf::from(p)))
    } else {
        Err(rule(format!("destination {s:?}: udp://, rtp:// or file:")))
    }
}

/// Parses and validates a configuration.
pub fn load(text: &str) -> Result<Plant, ConfigError> {
    let f: File = toml::from_str(text)?;
    let o = &f.output;
    let rate = TsRate::new(o.ts_rate).ok_or_else(|| rule("ts_rate out of range"))?;
    if rate.bps() >= QAM256_PAYLOAD {
        return Err(rule(format!(
            "ts_rate must stay below the 256-QAM payload rate {QAM256_PAYLOAD}"
        )));
    }
    let per = PerDatagram::new(o.packets_per_datagram)
        .ok_or_else(|| rule("packets_per_datagram 1..=7"))?;
    let default_rtp = match o.framing.as_str() {
        "udp" => false,
        "rtp" => true,
        x => return Err(rule(format!("framing {x:?}: udp or rtp"))),
    };
    if o.pcr_interval_ms == 0 || o.pcr_interval_ms > 100 {
        return Err(rule("pcr_interval_ms must be 1..=100"));
    }
    if !(0.0..0.5).contains(&o.null_floor) {
        return Err(rule("null_floor must be in [0, 0.5)"));
    }
    if o.delay_ms < 600 || o.delay_ms > 5000 {
        return Err(rule(
            "delay_ms must be 600..=5000 (encode budget + D + margin)",
        ));
    }
    let dst = match f.psip.dst_rule.as_str() {
        "us" => DstRule::Us,
        "none" => DstRule::None,
        x => return Err(rule(format!("dst_rule {x:?}: us or none"))),
    };
    let guide = GuideConfig {
        windows: f.psip.eit_windows,
        ett_unique_extension: f.psip.ett_unique_extension,
        placeholders: f.psip.placeholders,
        service_location: f.psip.service_location,
    };
    if guide.windows == 0 || guide.windows > 128 {
        return Err(rule("eit_windows must be 1..=128"));
    }
    let video = match f.video.device.as_str() {
        "cuda" => VideoDevice::Cuda(f.video.gpu),
        "hip" => VideoDevice::Hip(f.video.gpu),
        "ref" => VideoDevice::Ref,
        x => return Err(rule(format!("video device {x:?}: cuda, hip or ref"))),
    };

    let mut services = HashMap::new();
    for s in &f.services {
        if services.insert(s.id.clone(), s).is_some() {
            return Err(rule(format!("service {:?} defined twice", s.id)));
        }
    }
    let mut inputs = Vec::new();
    let mut input_ids = HashSet::new();
    for i in &f.inputs {
        if !input_ids.insert(i.id.clone()) {
            return Err(rule(format!("input {:?} defined twice", i.id)));
        }
        let need_bind = || {
            i.bind
                .ok_or_else(|| rule(format!("input {:?} needs bind", i.id)))
        };
        inputs.push(match i.kind.as_str() {
            "rtmp" => InputPlan::Rtmp {
                id: i.id.clone(),
                bind: need_bind()?,
                app: i.app.clone().unwrap_or_else(|| "live".into()),
                key: i
                    .key
                    .clone()
                    .ok_or_else(|| rule(format!("rtmp input {:?} needs key", i.id)))?,
            },
            "ts_udp" => InputPlan::TsUdp {
                id: i.id.clone(),
                bind: need_bind()?,
            },
            "file" => InputPlan::File {
                id: i.id.clone(),
                path: i
                    .path
                    .clone()
                    .ok_or_else(|| rule(format!("file input {:?} needs path", i.id)))?,
            },
            "srt" => {
                if let Some(p) = &i.passphrase
                    && !(10..=79).contains(&p.chars().count())
                {
                    return Err(rule(format!(
                        "srt input {:?}: passphrase must be 10 to 79 characters",
                        i.id
                    )));
                }
                let ms = i.latency_ms.unwrap_or(200);
                if !(20..=5000).contains(&ms) {
                    return Err(rule(format!(
                        "srt input {:?}: latency_ms must be 20..=5000",
                        i.id
                    )));
                }
                InputPlan::Srt {
                    id: i.id.clone(),
                    bind: need_bind()?,
                    passphrase: i.passphrase.clone(),
                    latency: std::time::Duration::from_millis(ms),
                }
            }
            x => {
                return Err(rule(format!(
                    "input kind {x:?}: rtmp, ts_udp, srt, hls or file"
                )));
            }
        });
    }

    let mut carriers = Vec::new();
    let mut ids = HashSet::new();
    let mut tsids = HashSet::new();
    let mut dests = HashSet::new();
    let mut channels = HashSet::new();
    let mut source_ids = HashSet::new();
    let mut used_services = HashSet::new();
    let mut used_inputs = HashSet::new();
    for c in &f.carriers {
        if !(1..=16).contains(&c.id) || !ids.insert(c.id) {
            return Err(rule(format!(
                "carrier id {} must be 1..=16 and unique",
                c.id
            )));
        }
        let tsid = c.tsid.unwrap_or(0x0100 + u16::from(c.id));
        if !tsids.insert(tsid) {
            return Err(rule(format!("TSID {tsid:#x} used twice")));
        }
        let mut destination = parse_dest(&c.destination)?;
        if default_rtp && let Destination::Udp(a) = destination {
            destination = Destination::Rtp(a);
        }
        if !dests.insert(format!("{destination:?}")) {
            return Err(rule(format!("destination {} used twice", c.destination)));
        }
        if c.programs.len() > 6 {
            return Err(rule(format!(
                "carrier {}: at most 6 programs in MPTS mode",
                c.id
            )));
        }
        let mut programs = Vec::new();
        let mut budget = 0u64;
        let mut video_budget = 0u64;
        for (index, p) in c.programs.iter().enumerate() {
            let s = services.get(&p.service).ok_or_else(|| {
                rule(format!(
                    "program {}: unknown service {:?}",
                    p.number, p.service
                ))
            })?;
            if !used_services.insert(p.service.clone()) {
                return Err(rule(format!("service {:?} placed twice", p.service)));
            }
            let number = ProgramNumber::new(p.number).ok_or_else(|| rule("program number 0"))?;
            if !(1..=6).contains(&p.number) {
                return Err(rule(
                    "program numbers are 1..=6 (PIDs follow from them, SPEC §7.1)",
                ));
            }
            let base = 0x30 + 0x10 * (p.number - 1);
            let format = Format::parse(&p.video.format)?;
            let max_bps = p.video.max_kbps * 1000;
            if max_bps < 1_000_000 || max_bps > format.max_bps() {
                return Err(rule(format!(
                    "program {}: max_kbps must be 1000..={} for {:?}",
                    p.number,
                    format.max_bps() / 1000,
                    format
                )));
            }
            // vbv_buffer_size is coded in units of 16,384 bits (H.262 §6.3.3).
            let vbv_bits = ((max_bps / 2).min(format.max_vbv()) / 16_384) * 16_384;
            if p.audio.len() > 3 {
                return Err(rule(format!(
                    "program {}: at most 3 audio tracks",
                    p.number
                )));
            }
            let mut audio = Vec::new();
            let mut ps_audio = Vec::new();
            for (k, a) in p.audio.iter().enumerate() {
                let lang = Lang::new(&a.language)?;
                let idx = AC3_RATES
                    .iter()
                    .position(|&r| r == a.kbps)
                    .filter(|&i| i <= 15)
                    .ok_or_else(|| {
                        rule(format!("audio kbps {} is not an A/52 rate ≤ 448", a.kbps))
                    })?;
                let (channels, acmod) = match a.layout.as_str() {
                    "mono" => (1u8, 1u8),
                    "stereo" => (2, 2),
                    "5.1" | "auto" => (6, 7),
                    x => {
                        return Err(rule(format!(
                            "audio layout {x:?}: mono, stereo, 5.1 or auto"
                        )));
                    }
                };
                let auto = a.layout == "auto";
                if a.input_track == Some(0) {
                    return Err(rule("audio input_track counts from 1"));
                }
                audio.push(AudioPlan {
                    lang,
                    channels,
                    kbps: a.kbps,
                    input_track: a.input_track,
                    auto,
                    mode_kbps: if auto {
                        [a.kbps.min(96), a.kbps.min(192), a.kbps]
                    } else {
                        [a.kbps; 3]
                    },
                });
                ps_audio.push(Audio {
                    pid: EsPid::new(base + 4 + k as u16)?,
                    ac3: Ac3Descriptor {
                        bit_rate_index: idx as u8,
                        surround_mode: 0,
                        bsmod: 0,
                        acmod,
                        max_channels: auto.then_some(6),
                        rate_upper_limit: auto,
                        mainid: k as u8,
                        priority: if k == 0 { 1 } else { 2 },
                        asvcflags: 0,
                        lang,
                    },
                    audio_type: 0,
                    component_name: a.name.as_deref().map(Mss::eng),
                });
                budget += ac3_ts_bps(a.kbps);
            }
            let number_ch = ChannelNumber::parse(&s.channel).ok_or_else(|| {
                rule(format!(
                    "service {:?}: channel {:?} is not major.minor",
                    s.id, s.channel
                ))
            })?;
            if !channels.insert(number_ch) {
                return Err(rule(format!("channel {number_ch} used twice")));
            }
            let source_id = SourceId::new(s.source_id)
                .ok_or_else(|| rule(format!("service {:?}: source_id must be 1..=4095", s.id)))?;
            if !source_ids.insert(source_id) {
                return Err(rule(format!("source_id {} used twice", s.source_id)));
            }
            let captions = if p.captions || p.subtitles.is_some() {
                vec![
                    CaptionService {
                        lang: Lang::ENG,
                        kind: CaptionKind::Line21 { field2: false },
                        easy_reader: false,
                        wide: true,
                    },
                    CaptionService {
                        lang: Lang::ENG,
                        kind: CaptionKind::Digital(
                            ServiceNumber::new(1).ok_or_else(|| rule("service 1"))?,
                        ),
                        easy_reader: false,
                        wide: true,
                    },
                ]
            } else {
                Vec::new()
            };
            for l in p.live.iter().chain(&p.standby) {
                if !input_ids.contains(l) {
                    return Err(rule(format!("program {}: unknown input {l:?}", p.number)));
                }
                // One use per input: each has one socket (or file reader) and one decoder.
                if !used_inputs.insert(l.clone()) {
                    return Err(rule(format!("input {l:?} is used twice")));
                }
            }
            let psip = Program {
                number,
                pmt_pid: EsPid::new(base)?,
                video: Video {
                    pid: EsPid::new(base + 1)?,
                    codec: VideoCodec::Mpeg2,
                    captions,
                },
                audio: ps_audio,
                rating: None,
                channel: Channel {
                    number: number_ch,
                    short_name: ShortName::new(&s.name)?,
                    long_name: s.long_name.as_deref().map(Mss::eng),
                    source_id,
                    service_type: ServiceType::DigitalTv,
                    hidden: false,
                    hide_guide: false,
                    etm: EtmLocation::None,
                },
            };
            let min_bps = p
                .video
                .min_kbps
                .map_or((max_bps / 4).max(1_000_000), |k| k * 1000);
            if min_bps == 0 || min_bps > max_bps {
                return Err(rule(format!(
                    "program {}: min_kbps must be 1..=max_kbps",
                    p.number
                )));
            }
            let mut ladder = vec![format];
            match &p.ladder {
                None if format != Format::F480i => ladder.push(Format::F480i),
                None => {}
                Some(list) => {
                    for s in list {
                        let f = Format::parse(s)?;
                        let (w, _) = f.size();
                        let (lw, _) = ladder[ladder.len() - 1].size();
                        if w >= lw || ladder.contains(&f) {
                            return Err(rule(format!(
                                "program {}: ladder {list:?}: each format smaller than the one before",
                                p.number
                            )));
                        }
                        ladder.push(f);
                    }
                }
            }
            let weight = p.video.weight.unwrap_or(1.0);
            if weight.is_nan() || weight <= 0.0 {
                return Err(rule(format!(
                    "program {}: weight must be positive",
                    p.number
                )));
            }
            let plan = ProgramPlan {
                index,
                service: p.service.clone(),
                psip,
                format,
                max_bps,
                min_bps,
                weight,
                vbv_bits,
                priority: p.video.priority,
                audio,
                live: p.live.clone(),
                subtitles: p.subtitles.clone(),
                standby: p.standby.clone(),
                ladder,
                rung: Rung::default(),
                film: match p.film.as_deref() {
                    None | Some("off") => false,
                    Some("auto") => true,
                    Some(x) => {
                        return Err(rule(format!(
                            "program {}: film {x:?}: auto or off",
                            p.number
                        )));
                    }
                },
            };
            video_budget += if f.statmux.enabled {
                plan.ts_bps_for(plan.min_bps, o.pcr_interval_ms)
            } else {
                plan.video_ts_bps(o.pcr_interval_ms)
            };
            programs.push(plan);
        }
        // Tables: PAT, PMTs, PSIP base and the guide, generously (SPEC §6.5 table).
        budget += 300_000;
        let usable = (rate.bps() as f64 * (1.0 - o.null_floor)) as u64;
        let video_pool_ts = usable.saturating_sub(budget);
        budget += video_budget;
        if budget > usable {
            return Err(rule(format!(
                "carrier {}: programs need {} bit/s, {} available after the null floor",
                c.id, budget, usable
            )));
        }
        let lineup = Carrier::new(tsid, programs.iter().map(|p| p.psip.clone()).collect())?;
        carriers.push(CarrierPlan {
            id: c.id,
            destination,
            lineup,
            programs,
            interface: None,
            video_pool_ts,
        });
    }

    let mut interfaces = Vec::new();
    for (k, i) in o.interfaces.iter().enumerate() {
        for &cid in &i.carriers {
            let c = carriers
                .iter_mut()
                .find(|c| c.id == cid)
                .ok_or_else(|| rule(format!("interface {}: no carrier {cid}", i.name)))?;
            if c.interface.replace(k).is_some() {
                return Err(rule(format!("carrier {cid} on two interfaces")));
            }
        }
        interfaces.push(InterfacePlan {
            name: i.name.clone(),
            address: i.address,
            ttl: i.multicast_ttl,
        });
    }

    let ov = &f.overload;
    if !(ov.window_s >= 1.0
        && ov.recover_s >= ov.window_s
        && ov.threshold > 0.0
        && ov.threshold < 1.0)
    {
        return Err(rule(
            "overload: window_s at least 1, recover_s at least window_s, threshold in (0, 1)",
        ));
    }
    let overload = OverloadPlan {
        enabled: ov.enabled,
        window: (ov.window_s * crate::time::HZ as f64) as u64,
        threshold: ov.threshold,
        recover: (ov.recover_s * crate::time::HZ as f64) as u64,
    };
    let mut ratings: Vec<tb_psip::rating::RatingSystem> = Vec::new();
    for r in &f.rating_systems {
        let dims = r
            .dimensions
            .iter()
            .map(|d| tb_psip::rating::RatingDimension {
                name: d.name.clone(),
                graduated: d.graduated,
                values: d
                    .values
                    .iter()
                    .map(|v| tb_psip::rating::RatingValue {
                        abbrev: v.abbrev.clone(),
                        text: v.text.clone(),
                    })
                    .collect(),
            })
            .collect();
        let sys = tb_psip::rating::RatingSystem::new(r.region, &r.name, dims)?;
        // the RRT must fit its one section
        tb_psip::tables::rrt(&sys, tb_ts::section::Version::new(0))
            .map_err(|e| rule(format!("rating system {} ({}): {e}", r.region, r.name)))?;
        if ratings
            .iter()
            .any(|o| o.region() == sys.region() || o.name() == sys.name())
        {
            return Err(rule(format!(
                "rating system {} ({}): region and name must be unique",
                r.region, r.name
            )));
        }
        ratings.push(sys);
    }

    Ok(Plant {
        output: OutputPlan {
            rate,
            per,
            pcr_interval_ms: o.pcr_interval_ms,
            lead_ms: o.mux_lead_ms,
            delay_ms: o.delay_ms,
            interfaces,
            null_floor: o.null_floor,
            self_check: o.self_check,
        },
        leap: LeapSeconds(f.psip.gps_utc_offset),
        dst,
        guide,
        video,
        control: f.control.bind,
        versions: f.persistence.versions.clone(),
        schedule: f.psip.schedule.clone(),
        statmux: f.statmux.enabled.then_some(StatmuxPlan {
            alpha: f.statmux.alpha,
            max_step: f.statmux.max_step,
            hysteresis: f.statmux.hysteresis,
        }),
        device: f
            .device
            .as_ref()
            .map(|d| device_plan(d, &carriers))
            .transpose()?,
        carriers,
        inputs,
        ratings,
        overload,
        telemetry: telemetry_plan(&f.telemetry)?,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const EXAMPLE: &str = include_str!("../totalblonder.example.toml");

    #[test]
    fn example_loads() {
        let p = load(EXAMPLE).unwrap();
        assert_eq!(p.carriers.len(), 2);
        assert_eq!(p.carriers[0].programs.len(), 2);
        assert_eq!(p.carriers[0].lineup.tsid(), 0x0101);
        assert_eq!(p.ratings.len(), 1);
        assert_eq!(p.ratings[0].lookup("Eirin", "R15+"), Some((0, 3)));
    }

    #[test]
    fn mono_audio() {
        let text = "[[carrier]]\nid = 1\ndestination = \"udp://239.1.1.1:5000\"\n[[carrier.program]]\nnumber = 1\nservice = \"s\"\nvideo = { format = \"480i\", max_kbps = 4000 }\naudio = [ { layout = \"mono\", kbps = 96 } ]\n[[service]]\nid = \"s\"\nname = \"S\"\nchannel = \"20.1\"\nsource_id = 1\n";
        let p = load(text).unwrap();
        let pp = &p.carriers[0].programs[0];
        assert_eq!(pp.audio[0].channels, 1);
        assert_eq!(pp.psip.audio[0].ac3.acmod, 1);
        assert!(load(&text.replace("mono", "quad")).is_err());
    }

    #[test]
    fn ladders() {
        let one = |format: &str, ladder: &str| {
            format!(
                "[[carrier]]\nid = 1\ndestination = \"udp://239.1.1.1:5000\"\n[[carrier.program]]\nnumber = 1\nservice = \"s\"\nvideo = {{ format = \"{format}\", max_kbps = 15000 }}\n{ladder}\n[[service]]\nid = \"s\"\nname = \"S\"\nchannel = \"20.1\"\nsource_id = 1\n"
            )
        };
        let p = load(&one("1080i", "")).unwrap();
        let pp = &p.carriers[0].programs[0];
        assert_eq!(pp.ladder, vec![Format::F1080i, Format::F480i]);
        let sd = pp.at(Format::F480i);
        assert_eq!((sd.max_bps, sd.vbv_bits), (15_000_000, 112 * 16_384));
        assert_eq!(sd.min_bps, pp.min_bps);
        assert_eq!(
            load(&one("480i", "")).unwrap().carriers[0].programs[0].ladder,
            vec![Format::F480i]
        );
        assert_eq!(
            load(&one("1080i", "ladder = []")).unwrap().carriers[0].programs[0].ladder,
            vec![Format::F1080i]
        );
        assert_eq!(
            load(&one("1080i", "ladder = [\"720p\", \"480i\"]"))
                .unwrap()
                .carriers[0]
                .programs[0]
                .ladder,
            vec![Format::F1080i, Format::F720p, Format::F480i]
        );
        let e = load(&one("720p", "ladder = [\"1080i\"]"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("smaller"), "{e}");
        let e = load(&one("720p", "[overload]\nwindow_s = 0\n"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("overload"), "{e}");
        // the rung is shared by every copy of a plan
        let c = pp.clone();
        pp.rung.set(1);
        assert_eq!(c.rung.get(), 1);
    }

    #[test]
    fn rating_system_refusals() {
        let sys = |region: u8, name: &str| {
            format!(
                "[[rating_system]]\nregion = {region}\nname = \"{name}\"\n[[rating_system.dimension]]\nname = \"D\"\nvalues = [ {{ abbrev = \"A\" }} ]\n"
            )
        };
        assert!(load(&sys(16, "X")).is_ok());
        for (t, want) in [
            (sys(1, "X"), "CTA-766"),
            (sys(2, "X"), "CTA-766"),
            (sys(16, "X") + &sys(16, "Y"), "unique"),
            (sys(16, "X") + &sys(17, "X"), "unique"),
            (sys(16, &"n".repeat(33)), "1 to 32"),
        ] {
            let e = load(&t).unwrap_err().to_string();
            assert!(e.contains(want), "{t}: {e}");
        }
        // too many bytes for one section: 14 dimensions of 14 values with 150-character texts
        let mut big = String::from("[[rating_system]]\nregion = 16\nname = \"Big\"\n");
        for d in 0..14 {
            big += &format!("[[rating_system.dimension]]\nname = \"D{d}\"\nvalues = [");
            for v in 0..14 {
                big += &format!("{{ abbrev = \"V{v}\", text = \"{}\" }},", "t".repeat(150));
            }
            big += "]\n";
        }
        assert!(
            load(&big)
                .unwrap_err()
                .to_string()
                .contains("rating system 16")
        );
    }

    #[test]
    fn refusals() {
        let bad = [
            ("[output]\nts_rate = 39000000\n", "payload"),
            ("[output]\nbogus = 1\n", "unknown field"),
            (
                "[[carrier]]\nid = 17\ndestination = \"udp://239.1.1.1:5000\"\n",
                "1..=16",
            ),
        ];
        for (t, want) in bad {
            let e = load(t).unwrap_err().to_string();
            assert!(e.contains(want), "{t}: {e}");
        }
        // Over budget: six 15 Mbit/s programs.
        let mut t = String::from("[[carrier]]\nid = 1\ndestination = \"udp://239.1.1.1:5000\"\n");
        for n in 1..=6 {
            t += &format!(
                "[[carrier.program]]\nnumber = {n}\nservice = \"s{n}\"\nvideo = {{ format = \"720p\", max_kbps = 15000 }}\n"
            );
        }
        for n in 1..=6 {
            t += &format!(
                "[[service]]\nid = \"s{n}\"\nname = \"S{n}\"\nchannel = \"20.{n}\"\nsource_id = {n}\n"
            );
        }
        assert!(load(&t).unwrap_err().to_string().contains("available"));
    }

    #[test]
    fn ac3_overhead() {
        // 192 kbit/s: 768-byte frames, 5 packets, 235 kbit/s (SPEC §6.2).
        assert_eq!(ac3_ts_bps(192), 235_000);
        assert_eq!(ac3_ts_bps(384), 423_000);
    }
}
