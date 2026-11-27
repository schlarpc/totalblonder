//! Command line of the pipeline harness: the options of the C++ `m2vpipe`
//! and a few of the port's own.

use std::collections::BTreeMap;

/// An output format of the harness (the C++ `FMTS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fmt {
    /// Name on the command line.
    pub name: &'static str,
    /// Picture width.
    pub w: u32,
    /// Picture height.
    pub h: u32,
    /// Frame rate numerator.
    pub fn_: u32,
    /// Frame rate denominator.
    pub fd: u32,
    /// Interlaced.
    pub il: bool,
    /// GOP length.
    pub gop: u32,
    /// Bit rate.
    pub br: u64,
    /// Buffer size in bits.
    pub vbv: u32,
}

impl Fmt {
    /// The color of the pictures the converter makes for this format, and so what the
    /// session's sequence display extension says: BT.709 for the HD formats, SMPTE 170M (the
    /// BT.601 matrix) for SD. One function for both, so that the stream cannot describe a
    /// matrix the conversion did not use.
    pub fn color(&self) -> tb_m2v_encoder::Color {
        if self.h >= 720 {
            tb_m2v_encoder::Color::BT709
        } else {
            tb_m2v_encoder::Color::SMPTE170M
        }
    }
}

/// The four formats.
pub const FMTS: [Fmt; 4] = [
    Fmt {
        name: "1080i",
        w: 1920,
        h: 1080,
        fn_: 30000,
        fd: 1001,
        il: true,
        gop: 15,
        br: 15_000_000,
        vbv: 7_487_488,
    },
    Fmt {
        name: "1080p",
        w: 1920,
        h: 1080,
        fn_: 30000,
        fd: 1001,
        il: false,
        gop: 15,
        br: 15_000_000,
        vbv: 7_487_488,
    },
    Fmt {
        name: "720p",
        w: 1280,
        h: 720,
        fn_: 60000,
        fd: 1001,
        il: false,
        gop: 30,
        br: 12_000_000,
        vbv: 5_996_544,
    },
    Fmt {
        name: "480i",
        w: 704,
        h: 480,
        fn_: 30000,
        fd: 1001,
        il: true,
        gop: 15,
        br: 4_000_000,
        vbv: 1_835_008,
    },
];

/// Which device executes the plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// An NVIDIA card (CUDA; NVDEC decoding into the adapter's context).
    Cuda,
    /// An AMD card (HIP; decoding in software for now).
    Hip,
    /// The reference adapter (software inputs only: there is no decoder on it).
    Ref,
}

/// The options.
#[derive(Clone, Debug)]
pub struct Opts {
    /// Inputs: path and options (`FILE[*N][@opt,opt=value...]`, one entry per copy).
    pub ins: Vec<(String, String)>,
    /// Channel specifications (`fmt=1080i,src=0,n=4,...`).
    pub chs: Vec<String>,
    /// `--degrade ms`: a decoder this far behind stops decoding non-reference pictures.
    pub degrade: f64,
    /// `--resync ms`: a decoder this far behind skips to its input's next key frame.
    pub resync: f64,
    /// `--secs`.
    pub secs: f64,
    /// `--lead ms`: how long after its packets a frame is due.
    pub lead: f64,
    /// `--hold ms`: an input without a frame for this long is gone (the slate).
    pub hold: f64,
    /// `--paced`.
    pub paced: bool,
    /// `--offline`: no pacing anywhere, deterministic frame selection.
    pub offline: bool,
    /// `--deadline`: the deadline policy, with the next tick as the deadline.
    pub deadline: bool,
    /// `--dec-ctx shared` (default) or `own`.
    pub shared: bool,
    /// `--no-overlap`.
    pub no_overlap: bool,
    /// `--exact-down`: no 2:1 stage in front of the resampler.
    pub exact_down: bool,
    /// `--output PREFIX`.
    pub output: Option<String>,
    /// `--dump-conv FILE`: the converted frames of channel 0.
    pub dump: Option<String>,
    /// `--ptslog FILE`.
    pub ptslog: Option<String>,
    /// `--ticklog FILE`.
    pub ticklog: Option<String>,
    /// `--json FILE`.
    pub json: Option<String>,
    /// `--write K`: streams of the first K channels are written.
    pub write_n: usize,
    /// `--warmup TICKS` (default 150 paced, 0 offline).
    pub warmup: Option<u64>,
    /// `--stall IN:FROM:TO`.
    pub stall: Option<(usize, f64, f64)>,
    /// `--mem-limit-mb`: device memory in use at which opening inputs stops.
    pub mem_limit_mb: u64,
    /// `--device cuda|hip|ref`.
    pub device: DeviceKind,
    /// `--gpu N`.
    pub gpu: i32,
    /// `--conv-mb MB`: device memory for the conversion path (tap tables, intermediate planes,
    /// half-size frames).
    pub conv_mb: u64,
    /// `--nvdec-share F`: admission control for the hardware decoder: the share of its
    /// measured capacity that inputs may take (0: off, every input is opened as asked).
    pub nvdec_share: f64,
    /// `--sw-cores C`: with admission control, the host cores that software decoding of
    /// refused inputs may take.
    pub sw_cores: f64,
    /// `--no-cc`: do not carry captions from the inputs to the coded pictures.
    pub no_cc: bool,
    /// `--device-timeout S`.
    pub device_timeout: f64,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            ins: Vec::new(),
            chs: Vec::new(),
            degrade: 0.0,
            resync: 0.0,
            secs: 20.0,
            lead: 0.30,
            hold: 1.0,
            paced: false,
            offline: false,
            deadline: false,
            shared: true,
            no_overlap: false,
            exact_down: false,
            output: None,
            dump: None,
            ptslog: None,
            ticklog: None,
            json: None,
            write_n: 0,
            warmup: None,
            stall: None,
            mem_limit_mb: 10_000,
            device: DeviceKind::Cuda,
            gpu: 0,
            conv_mb: 0,
            nvdec_share: 0.0,
            sw_cores: 2.0,
            no_cc: false,
            device_timeout: 30.0,
        }
    }
}

/// The usage text.
pub const USAGE: &str = "usage: tb-pipe --in FILE[*N][@sw[,threads=N]|@at=S|@unsafe]... \
--ch fmt=1080i|1080p|720p|480i,src=0,n=N[,step=1][,grp=G,per=P][,fit=box|crop|stretch]\
[,filter=lanczos|bicubic|bilinear][,mosaic=CxR][,ov=1][,slate=1][,bitrate=B][,prio=P][,dar=16:9]\
[,film=auto|off] ...
  (film=auto: film mode for a channel of one input: units of one source frame become one picture
   shown for two or three fields, or one to three frame periods at 720p; default off)
  [--secs S] [--paced | --offline] [--deadline] [--lead ms] [--hold ms] [--degrade ms] [--resync ms]
  [--dec-ctx shared|own] [--no-overlap] [--exact-down] [--stall IN:FROM:TO] [--warmup TICKS]
  [--output PREFIX] [--write K] [--dump-conv F.yuv] [--ptslog F] [--ticklog F] [--json F]
  [--device cuda|hip|ref] [--gpu N] [--conv-mb MB] [--mem-limit-mb MB] [--nvdec-share F] [--sw-cores C]
  [--no-cc] [--device-timeout S]
       tb-pipe --golden-taps FILE   the conversion path's golden records (crates/tb-golden/vectors/taps.jsonl)";

/// Why the command line was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OptError {
    /// An option without its value.
    #[error("missing value for {0}")]
    Missing(String),
    /// An option this program does not have.
    #[error("unknown option {0}")]
    Unknown(String),
    /// A value that does not parse.
    #[error("bad value for {0}: {1}")]
    Bad(String, String),
    /// No `--ch`.
    #[error("no channels")]
    NoChannels,
}

/// `key=value,key,key=value` as a map; a key without a value maps to "1" (the C++
/// `kv`).
pub fn kv(s: &str) -> BTreeMap<String, String> {
    s.split(',')
        .filter(|i| !i.is_empty())
        .map(|item| match item.split_once('=') {
            Some((k, v)) => (k.to_owned(), v.to_owned()),
            None => (item.to_owned(), "1".to_owned()),
        })
        .collect()
}

impl Opts {
    /// Parse the arguments (without the program name).
    pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Result<Self, OptError> {
        let mut o = Opts::default();
        while let Some(a) = args.next() {
            let mut val = || args.next().ok_or_else(|| OptError::Missing(a.clone()));
            let num = |v: String| -> Result<f64, OptError> {
                v.parse().map_err(|_| OptError::Bad(a.clone(), v))
            };
            match a.as_str() {
                "--in" => {
                    let v = val()?;
                    let (v, opt) = match v.split_once('@') {
                        Some((p, o)) => (p.to_owned(), o.to_owned()),
                        None => (v, String::new()),
                    };
                    let (path, n) = match v.split_once('*') {
                        Some((p, n)) => (p.to_owned(), n.parse::<usize>().unwrap_or(0)),
                        None => (v, 1),
                    };
                    for _ in 0..n {
                        o.ins.push((path.clone(), opt.clone()));
                    }
                }
                "--ch" => o.chs.push(val()?),
                "--secs" => o.secs = num(val()?)?,
                "--lead" => o.lead = num(val()?)? / 1e3,
                "--hold" => o.hold = num(val()?)? / 1e3,
                "--resync" => o.resync = num(val()?)? / 1e3,
                "--degrade" => o.degrade = num(val()?)? / 1e3,
                "--paced" => o.paced = true,
                "--offline" => o.offline = true,
                "--deadline" => o.deadline = true,
                "--no-overlap" => o.no_overlap = true,
                "--exact-down" => o.exact_down = true,
                "--dec-ctx" => o.shared = val()? == "shared",
                "--output" => o.output = Some(val()?),
                "--write" => o.write_n = num(val()?)? as usize,
                "--dump-conv" => o.dump = Some(val()?),
                "--ptslog" => o.ptslog = Some(val()?),
                "--ticklog" => o.ticklog = Some(val()?),
                "--json" => o.json = Some(val()?),
                "--warmup" => o.warmup = Some(num(val()?)? as u64),
                "--mem-limit-mb" => o.mem_limit_mb = num(val()?)? as u64,
                "--stall" => {
                    let v = val()?;
                    let p: Vec<&str> = v.split(':').collect();
                    let bad = || OptError::Bad(a.clone(), v.clone());
                    let [i, from, to] = p.as_slice() else {
                        return Err(bad());
                    };
                    o.stall = Some((
                        i.parse().map_err(|_| bad())?,
                        from.parse().map_err(|_| bad())?,
                        to.parse().map_err(|_| bad())?,
                    ));
                }
                "--device" => {
                    let v = val()?;
                    o.device = match v.as_str() {
                        "cuda" => DeviceKind::Cuda,
                        "hip" => DeviceKind::Hip,
                        "ref" => DeviceKind::Ref,
                        _ => return Err(OptError::Bad(a.clone(), v)),
                    };
                }
                "--gpu" => o.gpu = num(val()?)? as i32,
                "--conv-mb" => o.conv_mb = num(val()?)? as u64,
                "--nvdec-share" => o.nvdec_share = num(val()?)?,
                "--sw-cores" => o.sw_cores = num(val()?)?,
                "--no-cc" => o.no_cc = true,
                "--device-timeout" => o.device_timeout = num(val()?)?,
                _ => return Err(OptError::Unknown(a)),
            }
        }
        if o.chs.is_empty() {
            return Err(OptError::NoChannels);
        }
        Ok(o)
    }

    /// Ticks left out of the statistics at the start.
    pub fn warmup_ticks(&self) -> u64 {
        self.warmup.unwrap_or(if self.offline { 0 } else { 150 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Opts, OptError> {
        Opts::parse(s.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn inputs_multiply_and_carry_options() {
        let o = parse("--in a.mkv*3@sw,threads=2 --in b.ts --ch fmt=1080i,src=0,n=2 --lead 250")
            .unwrap();
        assert_eq!(o.ins.len(), 4);
        assert_eq!(o.ins[2], ("a.mkv".to_owned(), "sw,threads=2".to_owned()));
        assert_eq!(o.ins[3], ("b.ts".to_owned(), String::new()));
        assert_eq!(o.lead, 0.25);
        let m = kv(&o.ins[0].1);
        assert_eq!(m["sw"], "1");
        assert_eq!(m["threads"], "2");
        assert_eq!(o.warmup_ticks(), 150);
    }

    #[test]
    fn refusals() {
        assert_eq!(parse("--in a").unwrap_err(), OptError::NoChannels);
        assert!(matches!(
            parse("--ch x --frobnicate"),
            Err(OptError::Unknown(_))
        ));
        assert!(matches!(parse("--ch x --secs"), Err(OptError::Missing(_))));
        assert!(matches!(
            parse("--ch x --stall 1:2"),
            Err(OptError::Bad(..))
        ));
        assert_eq!(
            parse("--ch x --stall 1:2:3.5").unwrap().stall,
            Some((1, 2.0, 3.5))
        );
    }
}
