//! The pipeline's run: inputs, channels, the tick loop (the C++ `main`).
//!
//! Per tick of the field rate (59.94 Hz): every input advances to the newest frame that is due;
//! every channel whose frame is due selects what it shows (`tb_timeline::select`, the only
//! place frames are dropped or repeated) and queues the conversion of it
//! (`tb_convert::Converter`, lowered to one plan on the kernel stream); then the encoder's
//! tick runs with the next tick as its deadline, and the coded pictures are written out.
//!
//! Decoded frames stay on the device: a decoder surface is bound to the adapter as a foreign
//! region for the tick in which a conversion reads it (`dev::PipeDevice::bind_surfaces`) and
//! is kept alive until that tick's conversion plan has completed. Software inputs (`@sw`)
//! are scaled to the channel's size in their decode thread and uploaded by the encoder.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tb_convert::{
    ConvClass, ConvKey, ConvOp, Converter, Dest, Filter, FitMode, Lowerer, OverlayPlanes, RectJob,
    Rows, Source, make_csc, make_fit,
};
use tb_decode::{ColorSpace, DecDevice, DecOptions, Frame, Input, Surface};
use tb_device::kernel::DevRef;
use tb_device::{ArenaSizes, DevBuf, HostBuf, Op, Plan};
use tb_engine::engine::{Engine, EngineError};
use tb_engine::rng::Mt19937;
use tb_m2v_encoder::{Encoder, EncoderOptions, FrameSource, SessionConfig, SessionId, Shown};
use tb_timeline::admission::{Admission, Codec, DecoderBudget, Grant, InputKind};
use tb_timeline::captions::{CaptionCarrier, CaptionCfg, CcRate};
use tb_timeline::film::{Assembler, FilmCfg, FilmStats, Key, Unit};
use tb_timeline::select::{self, Channel, ChannelCfg, Decision, SrcFrame, TileIn};

use crate::dev::PipeDevice;
use crate::opts::{FMTS, Fmt, Opts, kv};
use crate::pattern;
use crate::report::Report;

/// The tick: the field rate of the interlaced formats.
pub const PERIOD: f64 = 1001.0 / 60000.0;
/// sequence_end_code.
const SEQUENCE_END: [u8; 4] = [0, 0, 1, 0xB7];
/// Host scratch of the conversion path per plan (tap tables of new classes, job records).
const CONV_SCRATCH: u64 = 8 << 20;

/// Why a run ended early.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The command line asks for something the harness cannot do.
    #[error("{0}")]
    Usage(String),
    /// A file could not be written.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// The error.
        source: std::io::Error,
    },
    /// An input could not be opened.
    #[error("input {index} ({path}): {source}")]
    Input {
        /// Index of the input.
        index: usize,
        /// Its path.
        path: String,
        /// The decoder's error.
        source: tb_decode::DecodeError,
    },
    /// The encoder refused a command.
    #[error(transparent)]
    Encoder(#[from] tb_m2v_encoder::EncoderError),
    /// The device did not finish a plan.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// A conversion could not be planned.
    #[error(transparent)]
    Convert(#[from] tb_convert::ConvertError),
    /// The conversion path ran out of its memory.
    #[error(transparent)]
    Lower(#[from] tb_convert::LowerError),
    /// The arenas are too small (a bug in the sizing).
    #[error("the {0} arena is too small for the pipeline's own buffers")]
    Arena(&'static str),
    /// A frame that should be on the device is not.
    #[error("input {0} is not decoded on the device")]
    NotOnDevice(usize),
    /// A plan of the pipeline's own failed on the device.
    #[error("device: {0}")]
    Device(tb_device::DeviceError),
    /// The card's memory in use passed `--mem-limit-mb`.
    #[error("device memory limit ({limit} MB) reached at input {index}")]
    MemLimit {
        /// The limit.
        limit: u64,
        /// The input at which it was passed.
        index: usize,
    },
}

fn io(path: &str) -> impl Fn(std::io::Error) -> RunError + '_ {
    move |source| RunError::Io {
        path: path.to_owned(),
        source,
    }
}

/// One rectangle of a channel fed by one input.
#[derive(Clone, Copy, Debug)]
struct Tile {
    input: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// One output channel.
struct Chan {
    fmt: Fmt,
    fi: usize,
    sid: SessionId,
    sel: Channel,
    tiles: Vec<Tile>,
    fit: FitMode,
    filter: Filter,
    ov: bool,
    aspect: u8,
    every: u64,
    phase: u64,
    pictures: u64,
    repeats: u64,
    nmb: u64,
    out: Option<(BufWriter<File>, String)>,
    cc: Option<CaptionCarrier>,
    /// Film mode (`film=auto`): the selections per display unit, grouped into pictures.
    film: Option<Assembler<FieldSrc>>,
}

/// What one display unit of a film-mode channel shows: a source frame (`None`: the slate) and
/// the rows of it (all, or one field of an interlaced frame).
#[derive(Clone)]
struct FieldSrc {
    frame: Option<Arc<Frame>>,
    srows: select::Rows,
    /// The vertical low-pass when it is woven with another instant (sources above 40 Hz).
    lp: bool,
}

/// The slate as a source of [`Key::Instant`].
const SLATE_SRC: u32 = u32::MAX;

/// The display units of one output frame period of a film-mode channel with a single input:
/// two fields of an interlaced channel (in display order), one frame of a progressive one.
/// The same choices as [`tb_timeline::select`] makes (a field per tick from a source at
/// another rate, both fields from a source at the channel's rate, the fields of an interlaced
/// source in their own order, the slate when the input is gone), said per unit.
fn film_units(f: &Fmt, input_idx: usize, input: &In, stale: bool, out: &mut Vec<Unit<FieldSrc>>) {
    out.clear();
    let per = if f.il { 2 } else { 1 };
    let (Some(now), false) = (input.now.as_ref(), stale) else {
        for _ in 0..per {
            out.push(Unit {
                key: Key::Instant {
                    src: SLATE_SRC,
                    idx: 0,
                },
                data: FieldSrc {
                    frame: None,
                    srows: select::Rows::All,
                    lp: false,
                },
            });
        }
        return;
    };
    let src = input_idx as u32;
    let whole = |fr: &Arc<Frame>| Unit {
        key: Key::Instant { src, idx: fr.idx },
        data: FieldSrc {
            frame: Some(fr.clone()),
            srows: select::Rows::All,
            lp: input.fps > 40.0,
        },
    };
    // one field of an interlaced frame: the first or the second in its own field order
    let field = |fr: &Arc<Frame>, second: bool| Unit {
        key: Key::Unique,
        data: FieldSrc {
            frame: Some(fr.clone()),
            srows: if fr.top_field_first != second {
                select::Rows::Top
            } else {
                select::Rows::Bottom
            },
            lp: false,
        },
    };
    let same_rate = (input.fps * f64::from(f.fd) / f64::from(f.fn_) - 1.0).abs() < 0.01;
    if f.il {
        if now.interlaced {
            out.push(field(now, false));
            out.push(field(now, true));
        } else {
            // a field per tick, except from a source at the channel's frame rate
            let first = match &input.prev {
                Some(prev) if !same_rate && !prev.interlaced => prev,
                _ => now,
            };
            out.push(whole(first));
            out.push(whole(now));
        }
    } else if now.interlaced {
        // bob: the fields of a frame in turn (the second when the frame is selected again)
        let again = input.prev.as_ref().is_some_and(|p| p.idx == now.idx);
        let mut u = field(now, again && f.fn_ == 60000);
        u.key = Key::Instant {
            src: src | 0x8000_0000,
            idx: 2 * now.idx + i64::from(again && f.fn_ == 60000),
        };
        out.push(u);
    } else {
        out.push(whole(now));
    }
}

/// What the side thread that opens an input hands back: the started input, how long the
/// open took (ms), the time it finished, and the classes of the conversions the input will
/// most likely need, with their tap tables made.
type Opened = (Input, f64, f64, Vec<ConvClass>);

/// One input.
struct In {
    path: String,
    opts: DecOptions,
    dec: Option<Input>,
    now: Option<Arc<Frame>>,
    prev: Option<Arc<Frame>>,
    skipped: u64,
    off: f64,
    fps: f64,
    late_at: Option<f64>,
    opener: Option<JoinHandle<Option<Opened>>>,
    open_ms: f64,
    open_tick_max: f64,
    grant: Option<Grant>,
    refused: bool,
}

impl In {
    /// The C++ `In::advance`: the newest frame due at `limit` on the tick clock.
    /// Caption bytes of every frame the decoder delivered since the last call come back in
    /// `cc`, in decode order with their frame index: of the frames this call skipped, and
    /// last of the frame it returns.
    fn advance(&mut self, limit: f64, cc: &mut Vec<(i64, Vec<u8>)>) -> Option<Arc<Frame>> {
        self.prev = self.now.clone();
        cc.clear();
        let (f, sk) = self.dec.as_ref()?.take_with_captions(limit - self.off, cc);
        self.skipped += u64::from(sk);
        let f = Arc::new(f?);
        if !f.cc.is_empty() {
            cc.push((f.idx, f.cc.clone()));
        }
        self.now = Some(f.clone());
        Some(f)
    }
}

fn src_frame(f: &Frame) -> SrcFrame {
    SrcFrame {
        idx: f.idx,
        interlaced: f.interlaced,
        tff: f.top_field_first,
        w: f.width,
        h: f.height,
    }
}

fn rows(r: select::Rows) -> Rows {
    match r {
        select::Rows::All => Rows::All,
        select::Rows::Top => Rows::Top,
        select::Rows::Bottom => Rows::Bottom,
    }
}

fn session_cfg(
    f: &Fmt,
    m: &std::collections::BTreeMap<String, String>,
    k: u32,
    n: u32,
) -> SessionConfig {
    let bitrate = m
        .get("bitrate")
        .and_then(|v| v.parse().ok())
        .unwrap_or(f.br);
    SessionConfig {
        w: f.w,
        h: f.h,
        fps_n: f.fn_,
        fps_d: f.fd,
        interlaced: f.il,
        gop: f.gop,
        m: 3,
        bitrate,
        max_bitrate: bitrate,
        // the format's buffer, or at a lower rate what one second of it holds (the T-STD
        // limit of H.222.0 2.4.2.6, which the session's profile enforces)
        vbv_bits: u64::from(f.vbv).min(bitrate / 16384 * 16384),
        capacity: n,
        priority: m.get("prio").and_then(|v| v.parse().ok()).unwrap_or(0),
        gop_phase: ((u64::from(k) * u64::from(f.gop) / u64::from(n.max(1))) % u64::from(f.gop))
            as u32,
        aspect: if m.get("dar").is_some_and(|d| d == "16:9") {
            3
        } else {
            0
        },
        // what the conversion to this format produces (not a guess from the height)
        color: Some(f.color()),
        ..SessionConfig::default()
    }
}

/// A channel as the command line describes it, before anything is opened.
struct ChanPlan {
    fi: usize,
    cfg: SessionConfig,
    tiles: Vec<Tile>,
    fit: FitMode,
    filter: Filter,
    mosaic: bool,
    ov: bool,
    /// `film=auto`: film mode for a channel of one input.
    film: bool,
}

/// The channels of `--ch` specifications.
fn plan_channels(o: &Opts) -> Result<Vec<ChanPlan>, RunError> {
    let mut out = Vec::new();
    let nin = o.ins.len();
    for spec in &o.chs {
        let m = kv(spec);
        let get = |k: &str, d: usize| m.get(k).and_then(|v| v.parse::<usize>().ok()).unwrap_or(d);
        let name = m.get("fmt").map(String::as_str).unwrap_or("");
        let fi = FMTS
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| RunError::Usage("channel format?".to_owned()))?;
        let f = FMTS[fi];
        let (n, src, step) = (get("n", 1), get("src", 0), get("step", 1));
        for k in 0..n {
            let mut tiles = Vec::new();
            let mosaic = m.contains_key("mosaic");
            if let Some(g) = m.get("mosaic") {
                let (cx, ry) = g
                    .split_once('x')
                    .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
                    .unwrap_or((1, 1));
                let (cx, ry) = (cx.max(1), ry.max(1));
                let (tw, th) = ((f.w / cx) / 8 * 8, (f.h / ry) / 4 * 4);
                let (ox, oy) = (((f.w - tw * cx) / 2) / 8 * 8, ((f.h - th * ry) / 2) / 4 * 4);
                let per = (cx * ry) as usize;
                for j in 0..per {
                    tiles.push(Tile {
                        input: (src + k * step * per + j) % nin.max(1),
                        x: ox + (j as u32 % cx) * tw,
                        y: oy + (j as u32 / cx) * th,
                        w: tw,
                        h: th,
                    });
                }
            } else if !m.contains_key("slate") {
                if nin == 0 {
                    return Err(RunError::Usage("no inputs".to_owned()));
                }
                let idx = match (m.get("grp"), m.get("per")) {
                    (Some(g), per) => {
                        let g: usize = g.parse().unwrap_or(1).max(1);
                        let per: usize = per.and_then(|p| p.parse().ok()).unwrap_or(0);
                        src + (k / g) * per + k % g
                    }
                    _ => src + k * step,
                };
                tiles.push(Tile {
                    input: idx % nin,
                    x: 0,
                    y: 0,
                    w: f.w,
                    h: f.h,
                });
            }
            out.push(ChanPlan {
                fi,
                cfg: session_cfg(&f, &m, k as u32, n as u32),
                tiles,
                fit: match m.get("fit").map(String::as_str) {
                    Some("crop") => FitMode::Crop,
                    Some("stretch") => FitMode::Stretch,
                    _ => FitMode::Box,
                },
                filter: match m.get("filter").map(String::as_str) {
                    Some("bilinear") => Filter::Bilinear,
                    Some("bicubic") => Filter::Bicubic,
                    _ => Filter::Lanczos3,
                },
                mosaic,
                ov: m.contains_key("ov"),
                film: match m.get("film").map(String::as_str) {
                    None | Some("off") => false,
                    Some("auto") => true,
                    Some(v) => {
                        return Err(RunError::Usage(format!("film={v}: auto or off")));
                    }
                },
            });
        }
    }
    Ok(out)
}

fn encoder_options(o: &Opts) -> EncoderOptions {
    EncoderOptions {
        overlap: !o.no_overlap,
        keep_recon: false,
        split_plans: true,
        ..EncoderOptions::default()
    }
}

/// Device memory of the conversion path: `--conv-mb`, or what the line-up is expected to need
/// (tap tables, intermediate planes of every job of a tick, half-size frames of large sources).
fn conv_bytes(o: &Opts, chans: &[ChanPlan]) -> u64 {
    if o.conv_mb > 0 {
        return o.conv_mb << 20;
    }
    let tiles: u64 = chans.iter().map(|c| c.tiles.len() as u64).sum();
    (192 << 20) + tiles * (10 << 20)
}

/// The arena sizes a run needs: the encoder's batches by a dry run of its own allocator, the
/// slates and overlays, the conversion path, scratch and staging in the host arena.
pub fn arena_sizes(o: &Opts) -> Result<ArenaSizes, RunError> {
    const HUGE: u64 = 1 << 56;
    let chans = plan_channels(o)?;
    let mut e = Encoder::new(
        encoder_options(o),
        ArenaSizes {
            device: HUGE,
            host: HUGE,
        },
    );
    for c in &chans {
        e.open(&c.cfg)?;
    }
    let (d, h) = e.free_bytes();
    let (mut dev, mut host) = (HUGE - d, HUGE - h);
    let mut stage = 0u64;
    for (fi, f) in FMTS.iter().enumerate() {
        if !chans.iter().any(|c| c.fi == fi) {
            continue;
        }
        // padded frame (slate) and the overlay planes, with rounding slack
        let frame =
            u64::from(f.w.next_multiple_of(16)) * u64::from(f.h.next_multiple_of(32)) * 3 / 2;
        let ovl = u64::from(f.w) * u64::from(f.h) * 11 / 4;
        dev += frame + ovl + 4096;
        stage = stage.max(frame).max(ovl);
    }
    dev += conv_bytes(o, &chans) + (1 << 20);
    host += CONV_SCRATCH + 2 * stage + (1 << 20);
    Ok(ArenaSizes {
        device: dev.next_multiple_of(4096),
        host: host.next_multiple_of(4096),
    })
}

fn cpu_ticks(path: &str) -> f64 {
    // utime + stime in clock ticks (100 per second on Linux)
    let Ok(stat) = std::fs::read_to_string(path) else {
        return 0.0;
    };
    let Some(rest) = stat.rsplit(')').next() else {
        return 0.0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

/// The kind of an input for the decoder budget.
fn input_kind(dec: &Input) -> Option<InputKind> {
    let codec = match dec.codec() {
        "h264" => Codec::H264,
        "hevc" => Codec::Hevc,
        "mpeg2video" => Codec::Mpeg2,
        "av1" | "libdav1d" => Codec::Av1,
        "vp9" => Codec::Vp9,
        _ => return None,
    };
    let (w, h) = dec.size();
    Some(InputKind::new(codec, w, h, dec.fps()))
}

/// Everything conversion-related that one tick queues.
#[derive(Default)]
struct Queue {
    ops: Vec<ConvOp<DevRef>>,
    fills: Vec<RectJob<DevRef>>,
    cscs: Vec<(RectJob<DevRef>, tb_convert::Csc, bool)>,
    overlays: Vec<RectJob<DevRef>>,
    copies: Vec<([DevBuf; 3], usize)>,
    planes: Vec<(u64, u64)>,
    held: Vec<Arc<Frame>>,
    dump: Option<([DevBuf; 3], u32)>,
}

impl Queue {
    fn clear(&mut self) {
        self.ops.clear();
        self.fills.clear();
        self.cscs.clear();
        self.overlays.clear();
        self.copies.clear();
        self.planes.clear();
        self.held.clear();
        self.dump = None;
    }

    /// The foreign slot of a surface plane, bound once per tick.
    fn slot(&mut self, base: u64, len: u64) -> u16 {
        let i = match self.planes.iter().position(|p| p.0 == base) {
            Some(i) => i,
            None => {
                self.planes.push((base, len));
                self.planes.len() - 1
            }
        };
        i as u16
    }
}

/// One conversion into a channel's frame: a source frame (held until the tick's conversion
/// plan has completed), the rows read and the rows written.
struct ConvJob {
    tile: Tile,
    frame: Arc<Frame>,
    srows: select::Rows,
    drows: select::Rows,
    weave_lp: bool,
}

/// Queue the conversion of `job` into the channel's frame at `planes`, with its color
/// conversion. `progressive`: the picture is one instant. Returns whether the job covers the
/// whole picture.
fn queue_job(
    conv: &mut Converter<DevRef>,
    q: &mut Queue,
    c: &Chan,
    job: ConvJob,
    progressive: bool,
    planes: &[DevBuf; 3],
    pitch: u32,
) -> Result<bool, RunError> {
    let f = c.fmt;
    let mut full = false;
    let Surface::Cuda {
        y,
        uv,
        pitch_y,
        pitch_uv,
        bps,
    } = job.frame.surface
    else {
        return Err(RunError::NotOnDevice(job.tile.input));
    };
    let sar = match job.frame.sar {
        Some((n, d)) if n > 0 && d != 0 => f64::from(n) / f64::from(d),
        _ if (job.frame.width == 704 || job.frame.width == 720) && job.frame.height == 480 => {
            10.0 / 11.0
        }
        _ => 1.0,
    };
    let dpar = match (f.w, c.aspect) {
        (704, 3) => 40.0 / 33.0,
        (704, _) => 10.0 / 11.0,
        _ => 1.0,
    };
    let (sw, sh) = (job.frame.width & !1, job.frame.height & !1);
    let ft = make_fit(
        sw as i32,
        sh as i32,
        sar,
        job.tile.x as i32,
        job.tile.y as i32,
        job.tile.w as i32,
        job.tile.h as i32,
        dpar,
        c.fit,
        f.il,
    );
    if ft.rw as u32 == f.w && ft.rh as u32 == f.h {
        full = true;
    }
    let key = ConvKey {
        sw,
        sh,
        bps,
        cx: ft.cx as u32,
        cy: ft.cy as u32,
        cw: ft.cw as u32,
        ch: ft.ch as u32,
        dw: ft.rw as u32,
        dh: ft.rh as u32,
        srows: rows(job.srows),
        drows: rows(job.drows),
        filter: c.filter,
        weave_lp: job.weave_lp,
    };
    let hrows = u64::from(job.frame.height);
    let sy = q.slot(y, u64::from(pitch_y) * hrows);
    let suv = q.slot(uv, u64::from(pitch_uv) * hrows.div_ceil(2));
    let dst = rect(planes, pitch, ft.rx as u32, ft.ry as u32);
    conv.add(
        key,
        Source {
            y: DevRef::foreign(sy, 0),
            sp_y: pitch_y,
            uv: DevRef::foreign(suv, 0),
            sp_uv: pitch_uv,
        },
        dst,
        &mut q.ops,
    )?;
    // color: BT.709 for sources of 720 lines and more unless they say
    // otherwise; the output is BT.709 for HD, BT.601 for SD
    let s709 = job.frame.colorspace == ColorSpace::Bt709
        || (job.frame.colorspace != ColorSpace::Smpte170m
            && job.frame.colorspace != ColorSpace::Bt470bg
            && job.frame.height >= 720);
    let d709 = f.color() == tb_m2v_encoder::Color::BT709;
    if (s709 != d709 || job.frame.full_range) && job.drows != select::Rows::Bottom {
        q.cscs.push((
            RectJob {
                dst,
                w: ft.rw as u32,
                h: ft.rh as u32,
                yuv: 0,
                overlay: None,
            },
            make_csc(s709, d709, job.frame.full_range),
            f.il && !progressive,
        ));
    }
    q.held.push(job.frame);
    Ok(full)
}

/// The host path of film mode: rows `srows` of the planar 4:2:0 frame `src` (at the picture
/// size) into rows `drows` of `dst`. All into all is a copy; a whole frame into one field
/// gives that field's lines; one field into all lines repeats each of its lines (bob). A
/// field into the other parity's lines moves it by one line, which the device path's
/// converter does with the right phase and this comparison path does not.
fn host_rows(
    dst: &mut [u8],
    w: usize,
    h: usize,
    src: &[u8],
    srows: select::Rows,
    drows: select::Rows,
) {
    let parity = |r: select::Rows| match r {
        select::Rows::All => None,
        select::Rows::Top => Some(0),
        select::Rows::Bottom => Some(1),
    };
    let mut at = 0;
    for (pw, ph) in [(w, h), (w / 2, h / 2), (w / 2, h / 2)] {
        for row in 0..ph {
            if parity(drows).is_some_and(|p| row % 2 != p) {
                continue;
            }
            // the source line: the same line, or the line of the source field
            let from = match parity(srows) {
                None => row,
                Some(p) => (row & !1) + p,
            }
            .min(ph.saturating_sub(1));
            let (d, s) = (at + row * pw, at + from * pw);
            if let (Some(d), Some(s)) = (dst.get_mut(d..d + pw), src.get(s..s + pw)) {
                d.copy_from_slice(s);
            }
        }
        at += pw * ph;
    }
}

fn rect(planes: &[DevBuf; 3], pitch: u32, x: u32, y: u32) -> Dest<DevRef> {
    let (p, x, y) = (u64::from(pitch), u64::from(x), u64::from(y));
    Dest {
        y: DevRef::arena(planes[0].off + y * p + x),
        u: DevRef::arena(planes[1].off + (y / 2) * (p / 2) + x / 2),
        v: DevRef::arena(planes[2].off + (y / 2) * (p / 2) + x / 2),
        pitch,
    }
}

/// Run a command line on a device whose arenas are at least [`arena_sizes`].
#[allow(
    clippy::too_many_lines,
    reason = "the tick loop of the C++ main, kept in one piece"
)]
pub fn run<D: PipeDevice>(o: &Opts, dev: D, console: &mut dyn Write) -> Result<Report, RunError> {
    let plans = plan_channels(o)?;
    let mut dev = dev;
    let mem0 = dev.mem_used_mb();
    let decdev: Option<Arc<DecDevice>> = dev
        .decoder(o.shared, o.gpu.max(0) as u32)
        .map_err(|source| RunError::Input {
            index: 0,
            path: "decoder device".to_owned(),
            source,
        })?
        .map(Arc::new);
    let mut eng = Engine::new(dev, encoder_options(o), o.device_timeout);
    tb_decode::log_errors_only();

    // ---- channels
    let mut chans: Vec<Chan> = Vec::with_capacity(plans.len());
    for (ci, p) in plans.iter().enumerate() {
        let f = FMTS[p.fi];
        let sid = eng.enc_mut().open(&p.cfg)?;
        let out = match &o.output {
            Some(prefix) if ci < o.write_n => {
                let name = format!("{prefix}.{ci}.m2v");
                Some((
                    BufWriter::new(File::create(&name).map_err(io(&name))?),
                    name,
                ))
            }
            _ => None,
        };
        let every = if f.fn_ == 60000 { 1 } else { 2 };
        let (lay, _) = eng.enc().batch_layout(sid)?;
        let nmb = u64::from(lay.geo.nmb.unsigned_abs());
        chans.push(Chan {
            fmt: f,
            fi: p.fi,
            sid,
            sel: Channel::new(ChannelCfg {
                w: f.w,
                h: f.h,
                fps_n: f.fn_,
                fps_d: f.fd,
                interlaced: f.il,
                mosaic: p.mosaic,
                host: false,
                tiles: p.tiles.len(),
            }),
            tiles: p.tiles.clone(),
            fit: p.fit,
            filter: p.filter,
            ov: p.ov,
            aspect: p.cfg.aspect,
            every,
            phase: ci as u64 % every,
            pictures: 0,
            repeats: 0,
            nmb,
            out,
            cc: None,
            // Film mode needs one input (a mosaic shows several instants at once) and a
            // sequence that may use repeat_first_field (H.262 Table 8-7: every format here).
            film: (p.film && p.tiles.len() == 1 && !p.mosaic).then(|| {
                Assembler::new(FilmCfg {
                    interlaced: f.il,
                    top_first: f.il && p.cfg.tff,
                    three: true,
                })
            }),
        });
    }
    // The batches are set up on the device before anything is written into their planes.
    eng.settle()?;
    let enc_dev_bytes = {
        let a = eng.enc().arenas();
        a.device - eng.enc().free_bytes().0
    };

    // ---- the pipeline's own memory
    let conv_len = conv_bytes(o, &plans);
    let conv_dev = eng
        .enc_mut()
        .reserve_device(conv_len)
        .ok_or(RunError::Arena("device"))?;
    let scratch = eng
        .enc_mut()
        .reserve_host(CONV_SCRATCH)
        .ok_or(RunError::Arena("host"))?;
    let mut conv: Converter<DevRef> = Converter::new();
    conv.fast_down = !o.exact_down;
    let mut low = Lowerer::new(conv_dev, scratch);
    let mut plan = Plan::new();

    // ---- slate and overlay per format in use, uploaded once
    let mut slate: [Option<[DevBuf; 3]>; 4] = [None; 4];
    let mut ovl: [Option<OverlayPlanes<DevRef>>; 4] = [None; 4];
    let mut stage_len = 0u64;
    for c in &chans {
        let (planes, _) = eng.enc().frame_target(c.sid)?;
        stage_len = stage_len
            .max(planes.iter().map(|p| p.len).sum())
            .max(u64::from(c.fmt.w) * u64::from(c.fmt.h) * 11 / 4);
    }
    let stage = eng
        .enc_mut()
        .reserve_host(stage_len.max(64))
        .ok_or(RunError::Arena("host"))?;
    let dump_host = eng
        .enc_mut()
        .reserve_host(stage_len.max(64))
        .ok_or(RunError::Arena("host"))?;
    for c in &chans {
        let (fi, f, ov, sid) = (c.fi, c.fmt, c.ov, c.sid);
        if slate[fi].is_none() {
            let (planes, pitch) = eng.enc().frame_target(sid)?;
            let gh = (planes[0].len / u64::from(pitch)) as u32;
            let pic = pattern::slate(f.w, f.h, pitch, gh);
            let total: u64 = planes.iter().map(|p| p.len).sum();
            let d = eng
                .enc_mut()
                .reserve_device(total)
                .ok_or(RunError::Arena("device"))?;
            let mut at = 0u64;
            let mut parts = [DevBuf::default(); 3];
            plan.clear();
            for (k, bytes) in pic.iter().enumerate() {
                let len = bytes.len() as u64;
                let src = stage.slice(at, len).ok_or(RunError::Arena("host"))?;
                src.of_mut(eng.host_mut())
                    .ok_or(RunError::Arena("host"))?
                    .copy_from_slice(bytes);
                parts[k] = d.slice(at, len).ok_or(RunError::Arena("device"))?;
                plan.main(Op::Upload { dst: parts[k], src });
                at += len;
            }
            run_plan(&mut eng, &plan)?;
            slate[fi] = Some(parts);
        }
        if ov && ovl[fi].is_none() {
            let pic = pattern::overlay(f.w, f.h);
            let all = [&pic.y, &pic.a, &pic.u, &pic.v, &pic.a2];
            let total: u64 = all.iter().map(|p| p.len() as u64).sum();
            let d = eng
                .enc_mut()
                .reserve_device(total + 64)
                .ok_or(RunError::Arena("device"))?;
            let mut at = 0u64;
            let mut refs = [DevRef::default(); 5];
            plan.clear();
            for (k, bytes) in all.iter().enumerate() {
                let len = bytes.len() as u64;
                let src = stage.slice(at, len).ok_or(RunError::Arena("host"))?;
                src.of_mut(eng.host_mut())
                    .ok_or(RunError::Arena("host"))?
                    .copy_from_slice(bytes);
                let dst = d.slice(at, len).ok_or(RunError::Arena("device"))?;
                refs[k] = DevRef::arena(dst.off);
                plan.main(Op::Upload { dst, src });
                at += len;
            }
            run_plan(&mut eng, &plan)?;
            ovl[fi] = Some(OverlayPlanes {
                oy: refs[0],
                oa: refs[1],
                ou: refs[2],
                ov: refs[3],
                oa2: refs[4],
            });
        }
    }

    // ---- inputs
    let warmup = o.warmup_ticks();
    let mut budget = (o.nvdec_share > 0.0).then(|| DecoderBudget::new(o.nvdec_share, o.sw_cores));
    let mut ins: Vec<In> = Vec::with_capacity(o.ins.len());
    let (mut routed_sw, mut refused) = (0u64, 0u64);
    for (i, (path, opt)) in o.ins.iter().enumerate() {
        let m = kv(opt);
        if let Some(k) = m
            .keys()
            .find(|k| !["sw", "threads", "at", "unsafe"].contains(&k.as_str()))
        {
            return Err(RunError::Usage(format!(
                "input option @{k}: the options are sw, threads=N, at=S, unsafe"
            )));
        }
        let mut d = DecOptions {
            resync: if o.offline { 0.0 } else { o.resync },
            degrade: if o.offline { 0.0 } else { o.degrade },
            paced: !o.offline,
            block: o.offline,
            looping: true,
            qmax: if o.offline { 4 } else { 26 },
            cfr: true,
            unsafe_output: m.contains_key("unsafe"),
            ..DecOptions::default()
        };
        // the (single) channel that shows this input, for the host path
        let user = chans
            .iter()
            .position(|c| c.tiles.iter().any(|t| t.input == i));
        let host_path = |d: &mut DecOptions, chans: &mut [Chan], threads: u32| {
            d.sw = true;
            d.sw_threads = threads;
            d.qmax = 12;
            for c in chans.iter_mut() {
                if c.tiles.iter().any(|t| t.input == i) {
                    d.host_size = Some((c.fmt.w, c.fmt.h));
                    c.sel = Channel::new(ChannelCfg {
                        host: true,
                        ..*c.sel.cfg()
                    });
                }
            }
        };
        if m.contains_key("sw") || decdev.is_none() {
            let threads = m.get("threads").and_then(|t| t.parse().ok()).unwrap_or(1);
            host_path(&mut d, &mut chans, threads);
        }
        let mut input = In {
            path: path.clone(),
            opts: d.clone(),
            dec: None,
            now: None,
            prev: None,
            skipped: 0,
            off: 0.0,
            fps: 0.0,
            late_at: m.get("at").and_then(|a| a.parse().ok()),
            opener: None,
            open_ms: 0.0,
            open_tick_max: 0.0,
            grant: None,
            refused: false,
        };
        if input.late_at.is_none() {
            let open = |d: &DecOptions| {
                Input::open(
                    Path::new(path),
                    d,
                    if d.sw { None } else { decdev.as_deref() },
                )
                .map_err(|source| RunError::Input {
                    index: i,
                    path: path.clone(),
                    source,
                })
            };
            let mut dec = open(&d)?;
            // Admission: the hardware decoder is shared by the picture and nothing is ever
            // dropped inside it, so an input over the budget goes to the CPU when that is cheap
            // and its channel can take host frames, and is refused otherwise (its channel shows
            // the slate).
            if let (Some(b), false) = (budget.as_mut(), d.sw) {
                let single = user.is_some_and(|c| chans[c].tiles.len() == 1);
                match input_kind(&dec).map(|k| b.admit(&k)) {
                    Some(Admission::Hardware(g)) => input.grant = Some(g),
                    Some(Admission::Software(g)) if single => {
                        drop(dec);
                        host_path(&mut d, &mut chans, 1);
                        dec = open(&d)?;
                        input.grant = Some(g);
                        input.opts = d.clone();
                        routed_sw += 1;
                    }
                    Some(Admission::Software(g)) => {
                        b.release(g);
                        input.refused = true;
                    }
                    Some(Admission::Refuse) => input.refused = true,
                    // a kind the budget has no figure for is let through
                    None => {}
                }
            }
            input.fps = dec.fps();
            if input.refused {
                refused += 1;
                tracing::warn!(input = i, path, "refused: the decoder budget is used up");
            } else {
                input.dec = Some(dec);
            }
            if let Some(used) = eng.dev().mem_used_mb()
                && used > o.mem_limit_mb as f64
            {
                return Err(RunError::MemLimit {
                    limit: o.mem_limit_mb,
                    index: i,
                });
            }
        }
        ins.push(input);
    }
    // Tap tables of every conversion the opened inputs can need, made before the first tick
    // (3.7 ms per class on the tick thread otherwise, spike notes C.9).
    let mut q = Queue::default();
    if !o.offline {
        for c in &chans {
            for t in &c.tiles {
                let Some(dec) = ins.get(t.input).and_then(|i| i.dec.as_ref()) else {
                    continue;
                };
                if ins[t.input].opts.sw {
                    continue;
                }
                for key in likely_keys(&KeyCtx::of(c, *t), dec.size(), !o.exact_down) {
                    if let Ok(class) = ConvClass::new(key) {
                        conv.adopt(class, &mut q.ops);
                    }
                }
            }
        }
        if !q.ops.is_empty() {
            plan.clear();
            low.begin();
            low.lower(&conv, &q.ops, eng.host_mut(), &mut plan)?;
            run_plan(&mut eng, &plan)?;
            q.ops.clear();
        }
    }
    let prepared_classes = conv.classes();
    let mem1 = eng.dev().mem_used_mb();

    let mut fpts = open_log(&o.ptslog)?;
    let mut ftick = open_log(&o.ticklog)?;
    let mut fdump = match &o.dump {
        Some(p) => Some((File::create(p).map_err(io(p))?, p.clone())),
        None => None,
    };

    // ---- tick loop
    eng.settle()?;
    let now_s = tb_decode::now;
    let t_start = now_s() + if o.offline { 0.0 } else { 0.3 };
    let cpu0 = cpu_ticks("/proc/self/stat");
    let main_cpu0 = cpu_ticks("/proc/thread-self/stat");
    let mut rng = Mt19937::new(7);
    for input in &mut ins {
        input.off = if o.offline {
            0.0
        } else {
            rng.uniform(0.0, 0.033)
        };
        if let Some(dec) = input.dec.as_mut() {
            dec.set_late_from(warmup as f64 * PERIOD + 0.5);
            dec.start(if o.offline { 0.0 } else { t_start });
        }
    }
    let nticks = (o.secs / PERIOD) as u64;
    let mut tick_times: Vec<f64> = Vec::with_capacity(nticks as usize);
    let (mut late_dl, mut counted, mut shed_total) = (0u64, 0u64, 0u64);
    let (mut sum_tick, mut worst, mut conv_host_ms) = (0.0f64, 0.0f64, 0.0f64);
    let (mut up_bytes, mut down_bytes) = (0.0f64, 0.0f64);
    let mut device_errors = 0u64;
    let mut ccbuf: Vec<u8> = Vec::with_capacity(96);
    let mut cc_seen: Vec<(i64, Vec<u8>)> = Vec::new();
    // tap tables of classes that came with an input opened while running
    let mut adopted: Vec<ConvOp<DevRef>> = Vec::new();
    let mut adopted_n = 0usize;
    let mut tiles_in: Vec<TileIn> = Vec::new();
    let mut units: Vec<Unit<FieldSrc>> = Vec::new();
    for t in 0..nticks {
        let tt = if o.offline {
            t as f64 * PERIOD
        } else {
            t_start + t as f64 * PERIOD
        };
        if t == warmup && t > 0 {
            for c in &mut chans {
                c.sel.reset_counters();
            }
        }
        let mut tick0 = now_s();
        if o.paced && !o.offline && tick0 < tt {
            std::thread::sleep(Duration::from_secs_f64(tt - tick0));
            tick0 = now_s();
        }
        // input time up to which frames are due
        let tsel = if o.offline {
            tt + 1e-4
        } else {
            (tt - t_start) - o.lead
        };
        let run_s = t as f64 * PERIOD;
        for (i, input) in ins.iter_mut().enumerate() {
            if input.dec.is_none() {
                // an input that appears while the pipeline runs: opened on a side thread
                match (&input.opener, input.late_at) {
                    (None, Some(at)) if run_s >= at && !input.refused => {
                        let (path, opts, dd) =
                            (input.path.clone(), input.opts.clone(), decdev.clone());
                        // the tap tables of its conversions are made on the side thread too
                        let users: Vec<KeyCtx> = chans
                            .iter()
                            .flat_map(|c| {
                                c.tiles
                                    .iter()
                                    .filter(|t| t.input == i)
                                    .map(|t| KeyCtx::of(c, *t))
                            })
                            .collect();
                        let fast_down = !o.exact_down;
                        input.opener = Some(std::thread::spawn(move || {
                            let a = tb_decode::now();
                            let dev = if opts.sw { None } else { dd.as_deref() };
                            let mut dec = Input::open(Path::new(&path), &opts, dev)
                                .map_err(|e| tracing::error!(error = %e, "late input"))
                                .ok()?;
                            let b = tb_decode::now();
                            dec.set_late_from(1e9);
                            dec.start(b);
                            let mut classes = Vec::new();
                            if !opts.sw {
                                let keys = users
                                    .iter()
                                    .flat_map(|u| likely_keys(u, dec.size(), fast_down));
                                for k in keys {
                                    if let Ok(c) = ConvClass::new(k) {
                                        classes.push(c);
                                    }
                                }
                            }
                            Some((dec, 1e3 * (b - a), b, classes))
                        }));
                    }
                    (Some(h), _) if h.is_finished() => {
                        let done = input.opener.take().and_then(|h| h.join().ok()).flatten();
                        match done {
                            Some((dec, ms, b, classes)) => {
                                let before = conv.classes();
                                for class in classes {
                                    conv.adopt(class, &mut adopted);
                                }
                                adopted_n += conv.classes() - before;
                                input.open_ms = ms;
                                input.off += b - t_start; // its time 0 is now
                                input.fps = dec.fps();
                                input.dec = Some(dec);
                                input.late_at = Some(input.late_at.unwrap_or(run_s));
                            }
                            None => input.refused = true,
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if let (Some((si, from, to)), Some(dec)) = (o.stall, input.dec.as_ref()) {
                dec.set_stall(si == i && run_s >= from && run_s < to);
            }
            input.advance(tsel, &mut cc_seen);
            if !o.no_cc && !cc_seen.is_empty() {
                // captions travel with the pictures of every single-input channel of this
                // input: every decoded frame's, also of frames the frame clock skips
                for c in chans
                    .iter_mut()
                    .filter(|c| c.tiles.len() == 1 && c.tiles[0].input == i)
                {
                    let rate = if c.fmt.fn_ == 60000 {
                        CcRate::F5994
                    } else {
                        CcRate::F2997
                    };
                    // (an interlaced channel begins with the top field: field 2 first)
                    let cfg = if c.fmt.il {
                        CaptionCfg::top_first(rate)
                    } else {
                        CaptionCfg::new(rate)
                    };
                    let carrier = c.cc.get_or_insert_with(|| CaptionCarrier::new(cfg));
                    for (idx, bytes) in &cc_seen {
                        carrier.push(*idx, bytes);
                    }
                }
            }
        }
        let h0 = Instant::now();
        let before: (i64, i64, i64) = chans.iter().fold((0, 0, 0), |a, c| {
            let k = &c.sel.counters;
            (a.0 + k.again, a.1 + k.fresh, a.2 + k.slates)
        });
        q.clear();
        q.ops.append(&mut adopted);
        for (ci, c) in chans.iter_mut().enumerate() {
            if (t + c.phase) % c.every != c.every - 1 {
                continue;
            }
            let f = c.fmt;
            tiles_in.clear();
            for tl in &c.tiles {
                let input = &ins[tl.input];
                tiles_in.push(TileIn {
                    now: input.now.as_deref().map(src_frame),
                    prev: input.prev.as_deref().map(src_frame),
                    fps: input.fps,
                    stale: input
                        .now
                        .as_ref()
                        .is_some_and(|b| (tsel - input.off) - b.t > o.hold),
                });
            }
            if c.film.is_some() {
                // ---- film mode: the units of this frame period go to the assembler; it
                // gives a picture when one is decided (none in some ticks: nothing is due)
                let tl = c.tiles[0];
                let stale = tiles_in.first().is_some_and(|t| t.stale);
                film_units(&f, tl.input, &ins[tl.input], stale, &mut units);
                let Some(asm) = c.film.as_mut() else {
                    continue;
                };
                for u in units.drain(..) {
                    asm.push(u);
                }
                let Some(pic) = asm.pull() else {
                    continue;
                };
                let upf: i128 = if f.il { 2 } else { 1 };
                let pts = (i128::from(pic.start) * 90_000 * i128::from(f.fd)
                    / (i128::from(f.fn_) * upf)) as i64;
                let shown = Shown {
                    units: pic.units,
                    top_first: pic.top_first,
                    progressive: pic.progressive,
                };
                let first = pic.first().data.clone();
                let second = pic.second().map(|u| u.data.clone());
                let idx_of = |d: &FieldSrc| d.frame.as_ref().map_or(-1, |fr| fr.idx);
                let (src_a, src_b) = (
                    idx_of(&first),
                    second.as_ref().map_or(idx_of(&first), idx_of),
                );
                ccbuf.clear();
                if let Some(carrier) = c.cc.as_mut() {
                    let newest = src_a.max(src_b);
                    let up_to = if newest >= 0 { newest } else { i64::MAX };
                    carrier.take_shown(up_to, pic.units, f.il.then_some(pic.top_first), &mut ccbuf);
                }
                c.sel.counters.frames += 1;
                let enc = eng.enc_mut();
                let act;
                if pic.again {
                    // the instant of the picture before: nothing to convert
                    act = if first.frame.is_none() { 's' } else { 'A' };
                    if first.frame.is_none() {
                        c.sel.counters.slates += 1;
                    } else {
                        c.sel.counters.again += 1;
                    }
                    enc.submit_shown(c.sid, FrameSource::Again, pts, &ccbuf, shown)?;
                } else if first.frame.is_none() && second.as_ref().is_none_or(|d| d.frame.is_none())
                {
                    act = 'S';
                    c.sel.counters.slates += 1;
                    let (planes, _) = enc.frame_target(c.sid)?;
                    enc.submit_shown(
                        c.sid,
                        FrameSource::Device { progressive: true },
                        pts,
                        &ccbuf,
                        Shown {
                            progressive: true,
                            ..shown
                        },
                    )?;
                    q.copies.push((planes, c.fi));
                } else if c.sel.cfg().host {
                    // the host path (software inputs, scaled to the picture size by the
                    // decoder): the rows of each field copied on the host
                    c.sel.counters.fresh += 1;
                    if f.il && pic.progressive {
                        c.sel.counters.psf += 1;
                    }
                    act = if pic.progressive { 'H' } else { 'W' };
                    let buf = enc.frame_buffer(c.sid)?;
                    let dst = buf.of_mut(eng.host_mut()).ok_or(RunError::Arena("host"))?;
                    let all = select::Rows::All;
                    let parts: [(Option<&FieldSrc>, select::Rows); 2] = match (&second, f.il) {
                        (Some(second), true) if pic.top_first => [
                            (Some(&first), select::Rows::Top),
                            (Some(second), select::Rows::Bottom),
                        ],
                        (Some(second), true) => [
                            (Some(second), select::Rows::Top),
                            (Some(&first), select::Rows::Bottom),
                        ],
                        _ => [(Some(&first), all), (None, all)],
                    };
                    for (d, drows) in parts {
                        let Some(d) = d else {
                            continue;
                        };
                        match d.frame.as_deref().map(|fr| &fr.surface) {
                            Some(Surface::Host(bytes)) => {
                                host_rows(dst, f.w as usize, f.h as usize, bytes, d.srows, drows);
                            }
                            Some(_) => {
                                return Err(RunError::Usage(format!(
                                    "input {} gives no host frames",
                                    tl.input
                                )));
                            }
                            // a field of the slate in a host frame: mid gray
                            None => {}
                        }
                    }
                    eng.enc_mut()
                        .submit_shown(c.sid, FrameSource::Host, pts, &ccbuf, shown)?;
                    up_bytes += f64::from(f.w) * f64::from(f.h) * 1.5;
                } else {
                    c.sel.counters.fresh += 1;
                    let (planes, pitch) = enc.frame_target(c.sid)?;
                    enc.submit_shown(
                        c.sid,
                        FrameSource::Device {
                            progressive: pic.progressive,
                        },
                        pts,
                        &ccbuf,
                        shown,
                    )?;
                    let mut full = false;
                    let all = select::Rows::All;
                    match (&second, f.il) {
                        // one instant, or one field shown as a frame: one conversion
                        (None, _) | (_, false) => {
                            act = if pic.progressive { 'N' } else { 'W' };
                            if f.il && pic.progressive {
                                c.sel.counters.psf += 1;
                            }
                            if let Some(frame) = first.frame {
                                let job = ConvJob {
                                    tile: tl,
                                    frame,
                                    srows: first.srows,
                                    drows: all,
                                    weave_lp: false,
                                };
                                full |= queue_job(
                                    &mut conv,
                                    &mut q,
                                    c,
                                    job,
                                    pic.progressive,
                                    &planes,
                                    pitch,
                                )?;
                            }
                        }
                        // two fields from two units: each into the rows of its parity
                        (Some(second), true) => {
                            act = 'W';
                            let (top, bot) = if pic.top_first {
                                (&first, second)
                            } else {
                                (second, &first)
                            };
                            let whole = |d: &FieldSrc, want: select::Rows| {
                                d.frame.as_ref().is_some_and(|fr| {
                                    d.srows == want && fr.width == f.w && fr.height == f.h
                                })
                            };
                            let same = match (&top.frame, &bot.frame) {
                                (Some(a), Some(b)) => a.idx == b.idx,
                                _ => false,
                            };
                            if same
                                && whole(top, select::Rows::Top)
                                && whole(bot, select::Rows::Bottom)
                            {
                                // both fields of one interlaced frame of the channel's size, on
                                // their own lines: the frame as it is
                                if let Some(frame) = top.frame.clone() {
                                    let job = ConvJob {
                                        tile: tl,
                                        frame,
                                        srows: all,
                                        drows: all,
                                        weave_lp: false,
                                    };
                                    full |= queue_job(
                                        &mut conv, &mut q, c, job, false, &planes, pitch,
                                    )?;
                                }
                            } else {
                                if top.frame.as_ref().is_some_and(|fr| !fr.interlaced) {
                                    c.sel.counters.weaves += 1;
                                }
                                for (d, drows) in
                                    [(top, select::Rows::Top), (bot, select::Rows::Bottom)]
                                {
                                    // (a field of the slate is not woven: the picture keeps
                                    // the fill there)
                                    if let Some(frame) = d.frame.clone() {
                                        let job = ConvJob {
                                            tile: tl,
                                            frame,
                                            srows: d.srows,
                                            drows,
                                            weave_lp: d.lp && d.srows == all,
                                        };
                                        full |= queue_job(
                                            &mut conv, &mut q, c, job, false, &planes, pitch,
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    let whole = RectJob {
                        dst: rect(&planes, pitch, 0, 0),
                        w: f.w,
                        h: f.h,
                        yuv: 16 | (128 << 8) | (128 << 16),
                        overlay: None,
                    };
                    if !full {
                        q.fills.push(whole);
                    }
                    if c.ov {
                        q.overlays.push(RectJob {
                            yuv: 0,
                            overlay: ovl[c.fi],
                            ..whole
                        });
                    }
                    if fdump.is_some() && ci == 0 {
                        q.dump = Some((planes, pitch));
                    }
                }
                if let (Some((fp, path)), true) = (&mut fpts, ci < 8) {
                    // as the line of an ordinary channel, with the picture's display after it:
                    // start (display units), units, first field (T, B, - progressive channel),
                    // the cadence break it is (- none)
                    writeln!(
                        fp,
                        "{ci} {t} {} {pts} {act} {src_a} {src_b} {} {} {} {}",
                        c.sel.counters.frames - 1,
                        pic.start,
                        pic.units,
                        if !f.il {
                            '-'
                        } else if pic.top_first {
                            'T'
                        } else {
                            'B'
                        },
                        pic.brk.map_or("-", tb_timeline::film::Break::name),
                    )
                    .map_err(io(path))?;
                }
                continue;
            }
            let s = c.sel.select(&tiles_in);
            ccbuf.clear();
            if let Some(carrier) = c.cc.as_mut() {
                let shown = if s.src_bot >= 0 { s.src_bot } else { i64::MAX };
                carrier.take(shown, &mut ccbuf);
            }
            let enc = eng.enc_mut();
            match &s.decision {
                Decision::Again | Decision::Slate { copy: false } => {
                    enc.submit(c.sid, FrameSource::Again, s.pts, &ccbuf)?;
                }
                Decision::Slate { copy: true } => {
                    let (planes, _) = enc.frame_target(c.sid)?;
                    enc.submit(
                        c.sid,
                        FrameSource::Device { progressive: true },
                        s.pts,
                        &ccbuf,
                    )?;
                    q.copies.push((planes, c.fi));
                }
                Decision::Host { .. } => {
                    let frame = ins[c.tiles[0].input].now.clone();
                    let Some(Surface::Host(bytes)) = frame.as_deref().map(|f| &f.surface) else {
                        return Err(RunError::Usage(format!(
                            "input {} gives no host frames",
                            c.tiles[0].input
                        )));
                    };
                    let buf = enc.frame_buffer(c.sid)?;
                    let dst = buf.of_mut(eng.host_mut()).ok_or(RunError::Arena("host"))?;
                    let n = dst.len().min(bytes.len());
                    dst[..n].copy_from_slice(&bytes[..n]);
                    eng.enc_mut()
                        .submit(c.sid, FrameSource::Host, s.pts, &ccbuf)?;
                    up_bytes += f64::from(f.w) * f64::from(f.h) * 1.5;
                }
                Decision::Convert { progressive, jobs } => {
                    let (planes, pitch) = enc.frame_target(c.sid)?;
                    enc.submit(
                        c.sid,
                        FrameSource::Device {
                            progressive: *progressive,
                        },
                        s.pts,
                        &ccbuf,
                    )?;
                    let mut full = false;
                    for jb in jobs {
                        let tl = c.tiles[jb.tile];
                        let input = &ins[tl.input];
                        // the frame the job names: the one selected now or at the tick before
                        let frame = match (&input.now, &input.prev) {
                            (Some(n), _) if n.idx == jb.frame.idx => n.clone(),
                            (_, Some(p)) if p.idx == jb.frame.idx => p.clone(),
                            _ => return Err(RunError::NotOnDevice(tl.input)),
                        };
                        let job = ConvJob {
                            tile: tl,
                            frame,
                            srows: jb.srows,
                            drows: jb.drows,
                            weave_lp: jb.weave_lp,
                        };
                        full |= queue_job(&mut conv, &mut q, c, job, *progressive, &planes, pitch)?;
                    }
                    let whole = RectJob {
                        dst: rect(&planes, pitch, 0, 0),
                        w: f.w,
                        h: f.h,
                        yuv: 16 | (128 << 8) | (128 << 16),
                        overlay: None,
                    };
                    if !full {
                        q.fills.push(whole);
                    }
                    if c.ov {
                        q.overlays.push(RectJob {
                            yuv: 0,
                            overlay: ovl[c.fi],
                            ..whole
                        });
                    }
                    if fdump.is_some() && ci == 0 {
                        q.dump = Some((planes, pitch));
                    }
                }
            }
            if let (Some((fp, path)), true) = (&mut fpts, ci < 8) {
                writeln!(
                    fp,
                    "{ci} {t} {} {} {} {} {}",
                    s.n, s.pts, s.act, s.src_top, s.src_bot
                )
                .map_err(io(path))?;
            }
        }
        // queue: fills, conversions, color, overlays, slate copies; then the encoder's tick
        for f in q.fills.drain(..) {
            conv.fill(f, &mut q.ops);
        }
        conv.run(&mut q.ops);
        for (j, m, il) in q.cscs.drain(..) {
            conv.csc(j, m, il, &mut q.ops);
        }
        for f in q.overlays.drain(..) {
            conv.overlay(f, &mut q.ops);
        }
        plan.clear();
        low.begin();
        low.lower(&conv, &q.ops, eng.host_mut(), &mut plan)?;
        for (planes, fi) in &q.copies {
            if let Some(src) = slate[*fi] {
                for (d, s) in planes.iter().zip(src) {
                    plan.main(Op::Copy { dst: *d, src: s });
                }
            }
        }
        eng.dev_mut().bind_surfaces(&q.planes, &q.held);
        let conv_ticket = if plan.steps.is_empty() {
            None
        } else {
            match eng.dev_mut().submit(&plan) {
                Ok(tk) => Some(tk),
                Err(e) => return Err(RunError::Device(e)),
            }
        };
        let h1 = Instant::now();
        let host_ms = 1e3 * h1.duration_since(h0).as_secs_f64();
        conv_host_ms += host_ms;
        if let (Some((planes, pitch)), Some((fd, path))) = (q.dump, &mut fdump) {
            // the converted frame of channel 0, after the kernels
            let f = chans[0].fmt;
            plan.clear();
            let mut at = 0u64;
            let mut where_ = [HostBuf::default(); 3];
            for (k, p) in planes.iter().enumerate() {
                let dst = dump_host.slice(at, p.len).ok_or(RunError::Arena("host"))?;
                where_[k] = dst;
                plan.main(Op::Download { dst, src: *p });
                at += p.len;
            }
            run_plan(&mut eng, &plan)?;
            let host = eng.dev().host();
            for (k, hb) in where_.iter().enumerate() {
                let (w, h, p) = if k == 0 {
                    (f.w as usize, f.h as usize, pitch as usize)
                } else {
                    (f.w as usize / 2, f.h as usize / 2, pitch as usize / 2)
                };
                let bytes = hb.of(host).ok_or(RunError::Arena("host"))?;
                for row in 0..h {
                    fd.write_all(&bytes[row * p..row * p + w])
                        .map_err(io(path))?;
                }
            }
        }
        let deadline = (o.deadline && o.paced && !o.offline).then(|| {
            let left = (t_start + (t + 1) as f64 * PERIOD - now_s()).max(0.0);
            Instant::now() + Duration::from_secs_f64(left)
        });
        eng.tick(deadline)?;
        if let Some(tk) = conv_ticket {
            // it ran ahead of the tick's first kernels on the same stream
            let done = eng.dev_mut().wait(tk, None);
            if let Err(e) = done.status {
                device_errors += 1;
                tracing::warn!(error = %e, tick = t, "the conversion plan failed");
                if device_errors > 100 {
                    return Err(RunError::Device(e));
                }
            }
        }
        let stats = eng.enc().tick_stats();
        shed_total += u64::from(stats.shed);
        for c in &mut chans {
            take(&mut eng, c, &mut down_bytes)?;
        }
        let dt = now_s() - tick0;
        let is_late = o.paced && now_s() > t_start + (t + 1) as f64 * PERIOD;
        if let Some((ft, path)) = &mut ftick {
            let after = chans.iter().fold((0, 0, 0), |a, c| {
                let k = &c.sel.counters;
                (a.0 + k.again, a.1 + k.fresh, a.2 + k.slates)
            });
            let tm = eng.times;
            writeln!(
                ft,
                "{t} {:.3} 0 0 0 {:.3} {:.3} {:.3} {:.3} {} {} {} {:.3} {:.3} {} 0 {:.3} {} {} {} {} {}",
                1e3 * dt, tm.upwait, tm.phase[0], tm.phase[1], tm.phase[2], stats.rounds, stats.retry,
                stats.rexf, tm.begin, 0.0, stats.shed, host_ms, stats.candidates, u8::from(is_late),
                after.0 - before.0, after.1 - before.1, after.2 - before.2,
            )
            .map_err(io(path))?;
        }
        for input in &mut ins {
            if let Some(at) = input.late_at
                && run_s >= at
                && run_s < at + 1.5
            {
                input.open_tick_max = input.open_tick_max.max(1e3 * dt);
            }
        }
        if t < warmup {
            continue;
        }
        counted += 1;
        tick_times.push(dt);
        sum_tick += dt;
        worst = worst.max(dt);
        late_dl += u64::from(is_late);
    }
    let wall = now_s() - t_start;
    let cpu = cpu_ticks("/proc/self/stat") - cpu0;
    let main_cpu = cpu_ticks("/proc/thread-self/stat") - main_cpu0;

    // ---- the inputs end here: what they decoded during the run is what the report counts
    // (the frames go before the inputs, the inputs before the decoder device and the adapter)
    q.clear();
    eng.dev_mut().bind_surfaces(&[], &[]);
    let mut ended: Vec<(usize, Input, u64, bool)> = Vec::new();
    for (i, input) in ins.iter_mut().enumerate() {
        input.now = None;
        input.prev = None;
        if let Some(h) = input.opener.take()
            && let Ok(Some((dec, ..))) = h.join()
        {
            drop(dec);
        }
        if let Some(dec) = input.dec.take() {
            ended.push((i, dec, input.skipped, input.opts.sw));
        }
    }
    let stats: Vec<_> = ended.iter().map(|(_, d, ..)| d.stats()).collect();
    let t_flush = Instant::now();

    // ---- flush
    for c in &chans {
        eng.enc_mut().flush(c.sid)?;
    }
    for _ in 0..8 {
        if eng.enc().idle() {
            break;
        }
        eng.tick(None)?;
        for c in &mut chans {
            take(&mut eng, c, &mut down_bytes)?;
        }
    }
    eng.settle()?;
    for c in &mut chans {
        if let Some((mut f, name)) = c.out.take() {
            f.write_all(&SEQUENCE_END)
                .and_then(|()| f.flush())
                .map_err(io(&name))?;
        }
    }
    let mem2 = eng.dev().mem_used_mb();
    let flush_ms = 1e3 * t_flush.elapsed().as_secs_f64();

    // ---- report
    let mut r = Report {
        channels: chans.len(),
        inputs: ins.len(),
        shared_ctx: o.shared,
        paced: o.paced,
        offline: o.offline,
        ticks: nticks,
        counted,
        late: late_dl,
        sum_tick,
        worst,
        wall,
        cpu,
        main_cpu,
        shed: shed_total,
        conv_classes: conv.classes(),
        classes_before_start: prepared_classes,
        classes_in_ticks: conv.classes() - prepared_classes - adopted_n,
        classes_adopted: adopted_n,
        conv_launches: conv.launches(),
        conv_host_ms,
        enc_mb: enc_dev_bytes as f64 / 1_048_576.0,
        conv_mb: conv.dev_bytes() as f64 / 1_048_576.0,
        // the arena was taken before the first reading
        ctx_mb: match (mem0, mem2) {
            (Some(a), Some(b)) => b - a + eng.enc().arenas().device as f64 / 1_048_576.0,
            _ => 0.0,
        },
        dec_mb: match (mem1, mem2) {
            (Some(a), Some(b)) => (b - a).max(0.0),
            _ => 0.0,
        },
        up_bytes,
        down_bytes,
        lead: o.lead,
        device_errors: device_errors + eng.errors,
        routed_sw,
        refused,
        all_on_device: true,
        ..Report::default()
    };
    tick_times.sort_by(f64::total_cmp);
    r.tick_times = tick_times;
    let (mut film, mut film_channels) = (FilmStats::default(), 0u64);
    for c in &chans {
        let k = c.sel.counters;
        r.frames += k.frames;
        r.fresh += k.fresh;
        r.again += k.again;
        r.slates += k.slates;
        r.weaves += k.weaves;
        r.drops += k.drops;
        r.psf += k.psf;
        r.pictures += c.pictures;
        r.repeats += c.repeats;
        r.mbs += (c.pictures - c.repeats) as f64 * c.nmb as f64;
        r.violations += eng.enc().snapshot(c.sid)?.stats.rate.violations;
        if let Some(cc) = &c.cc {
            r.cc_in += cc.stats.in_608.iter().sum::<u64>() + cc.stats.in_dtvcc;
            r.cc_out += cc.stats.out_608.iter().sum::<u64>() + cc.stats.out_dtvcc;
        }
        if let Some(a) = &c.film {
            film_channels += 1;
            let st = a.stats;
            film.units_in += st.units_in;
            film.units_out += st.units_out;
            film.progressive += st.progressive;
            film.again += st.again;
            film.idle += st.idle;
            for (x, y) in film.pictures.iter_mut().zip(st.pictures) {
                *x += y;
            }
            for (x, y) in film.breaks.iter_mut().zip(st.breaks) {
                *x += y;
            }
        }
    }
    r.film = film;
    r.film_channels = film_channels;
    for (i, input) in ins.iter().enumerate() {
        if let Some(at) = input.late_at
            && input.open_ms > 0.0
        {
            r.late_inputs
                .push((i, at, input.open_ms, input.open_tick_max));
        }
    }
    for ((i, dec, skipped, sw), st) in ended.into_iter().zip(stats) {
        let late = dec.take_lateness();
        r.dec_resync += st.resyncs;
        r.dec_skip += st.skipped;
        r.dec_degr += st.degrades;
        r.dec_cpu += st.cpu_us as f64 * 1e-6;
        r.dec_frames += st.decoded;
        r.dec_drop += st.dropped;
        r.in_skipped += skipped;
        r.dec_err += st.errors;
        r.cc_lost += st.cc_lost;
        if !sw && !st.on_device {
            r.all_on_device = false;
        }
        let name = ins[i].path.rsplit('/').next().unwrap_or("").to_owned();
        r.lateness.entry(name).or_default().extend_from_slice(&late);
        dec.close();
        if let (Some(b), Some(g)) = (budget.as_mut(), ins[i].grant.take()) {
            b.release(g);
        }
    }
    r.flush_ms = flush_ms;
    drop(decdev);
    r.print(console).map_err(io("standard output"))?;
    if let Some(p) = &o.json {
        std::fs::write(p, r.json()).map_err(io(p))?;
    }
    for (f, path) in [&mut fpts, &mut ftick].into_iter().flatten() {
        f.flush().map_err(io(path))?;
    }
    Ok(r)
}

/// What the likely conversions of a tile depend on besides the input's size.
#[derive(Clone, Copy, Debug)]
struct KeyCtx {
    fmt: Fmt,
    aspect: u8,
    fit: FitMode,
    filter: Filter,
    single: bool,
    tile: Tile,
}

impl KeyCtx {
    fn of(c: &Chan, tile: Tile) -> Self {
        Self {
            fmt: c.fmt,
            aspect: c.aspect,
            fit: c.fit,
            filter: c.filter,
            single: c.tiles.len() == 1,
            tile,
        }
    }
}

/// The conversions a tile of a channel will most likely ask for, from the input's size alone
/// (progressive NV12 assumed until a frame says otherwise; a class that turns out to be needed
/// and is not here is made in the tick, as before).
fn likely_keys(c: &KeyCtx, size: (u32, u32), fast_down: bool) -> Vec<ConvKey> {
    let (f, t) = (c.fmt, &c.tile);
    let (sw, sh) = (size.0 & !1, size.1 & !1);
    let sar = if (size.0 == 704 || size.0 == 720) && size.1 == 480 {
        10.0 / 11.0
    } else {
        1.0
    };
    let dpar = match (f.w, c.aspect) {
        (704, 3) => 40.0 / 33.0,
        (704, _) => 10.0 / 11.0,
        _ => 1.0,
    };
    let ft = make_fit(
        sw as i32, sh as i32, sar, t.x as i32, t.y as i32, t.w as i32, t.h as i32, dpar, c.fit,
        f.il,
    );
    let base = ConvKey {
        sw,
        sh,
        bps: 1,
        cx: ft.cx as u32,
        cy: ft.cy as u32,
        cw: ft.cw as u32,
        ch: ft.ch as u32,
        dw: ft.rw as u32,
        dh: ft.rh as u32,
        srows: Rows::All,
        drows: Rows::All,
        filter: c.filter,
        weave_lp: false,
    };
    let mut keys = vec![base];
    if f.il && c.single {
        for (lp, drows) in [
            (false, Rows::Top),
            (false, Rows::Bottom),
            (true, Rows::Top),
            (true, Rows::Bottom),
        ] {
            keys.push(ConvKey {
                drows,
                weave_lp: lp,
                ..base
            });
        }
    }
    // the pre-stage halves large sources before the class is looked up
    for k in &mut keys {
        let halve = fast_down
            && k.srows == Rows::All
            && k.cw >= 3 * k.dw
            && k.ch >= 3 * k.dh
            && k.sw.is_multiple_of(8)
            && k.sh.is_multiple_of(4)
            && [k.cx, k.cy, k.cw, k.ch].iter().all(|v| v.is_multiple_of(4));
        if halve {
            k.sw /= 2;
            k.sh /= 2;
            k.cx /= 2;
            k.cy /= 2;
            k.cw /= 2;
            k.ch /= 2;
        }
    }
    keys
}

type Log = Option<(BufWriter<File>, String)>;

fn open_log(path: &Option<String>) -> Result<Log, RunError> {
    match path {
        Some(p) => Ok(Some((
            BufWriter::new(File::create(p).map_err(io(p))?),
            p.clone(),
        ))),
        None => Ok(None),
    }
}

/// Submit a plan of the pipeline's own and wait for it.
fn run_plan<D: PipeDevice>(eng: &mut Engine<D>, plan: &Plan) -> Result<(), RunError> {
    if plan.steps.is_empty() {
        return Ok(());
    }
    let t = eng.dev_mut().submit(plan).map_err(RunError::Device)?;
    eng.dev_mut().wait(t, None).status.map_err(RunError::Device)
}

/// The coded pictures of a channel since the last call.
fn take<D: PipeDevice>(eng: &mut Engine<D>, c: &mut Chan, down: &mut f64) -> Result<(), RunError> {
    while let Some(au) = eng.enc_mut().take_output(c.sid) {
        c.pictures += 1;
        c.repeats += u64::from(au.repeat);
        *down += au.data.len() as f64;
        if let Some((f, name)) = &mut c.out {
            f.write_all(&au.data).map_err(io(name))?;
        }
        eng.enc_mut().recycle(au.data);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every channel's session takes its color description and its buffer from the format
    /// the converter targets: the stream says the matrix the conversion used, and every
    /// configuration the harness can make opens under the default (cable) profile.
    #[test]
    fn session_configurations_are_legal_and_say_the_converters_color() {
        use tb_m2v_encoder::{Color, Profile};
        let none = std::collections::BTreeMap::new();
        for f in &crate::opts::FMTS {
            let c = session_cfg(f, &none, 0, 1);
            assert_eq!(c.color, Some(f.color()), "{}", f.name);
            assert_eq!(
                f.color(),
                if f.h >= 720 {
                    Color::BT709
                } else {
                    Color::SMPTE170M
                }
            );
            assert_eq!(c.profile, Profile::Scte43Qam256);
            let par = tb_m2v_encoder::profile::check(&c.seq_config(), c.profile);
            assert_eq!(par.map(|p| p.color), Ok(f.color()), "{}", f.name);
            // a lower rate keeps the buffer within one second of it (H.222.0 2.4.2.6)
            for rate in [1_000_000u64, 2_500_000, 6_000_000] {
                let mut m = std::collections::BTreeMap::new();
                m.insert("bitrate".to_owned(), rate.to_string());
                let c = session_cfg(f, &m, 0, 1);
                assert!(c.vbv_bits <= rate && c.vbv_bits.is_multiple_of(16384));
                assert!(
                    tb_m2v_encoder::profile::check(&c.seq_config(), c.profile).is_ok(),
                    "{} at {rate}",
                    f.name
                );
            }
        }
    }

    fn opts(s: &str) -> Opts {
        Opts::parse(s.split_whitespace().map(str::to_owned)).unwrap()
    }

    #[test]
    fn channels_take_their_inputs_as_the_cpp_does() {
        // units of six channels over nine inputs: grp / per
        let o = opts(
            "--in a*18 --ch fmt=1080i,src=0,n=2,grp=1,per=9 --ch fmt=720p,src=1,n=2,grp=1,per=9 \
             --ch fmt=480i,src=2,n=8,grp=4,per=9",
        );
        let c = plan_channels(&o).unwrap();
        assert_eq!(c.len(), 12);
        let ins: Vec<usize> = c.iter().map(|c| c.tiles[0].input).collect();
        assert_eq!(ins, [0, 9, 1, 10, 2, 3, 4, 5, 11, 12, 13, 14]);
        // GOP phases are spread over a group
        assert_eq!(c[4].cfg.gop_phase, 0);
        assert_eq!(c[5].cfg.gop_phase, 1);
        assert_eq!(c[11].cfg.gop_phase, 13);
        assert_eq!(c[0].cfg.capacity, 2);
        // step
        let o = opts("--in a*8 --ch fmt=720p,src=1,n=3,step=2");
        let c = plan_channels(&o).unwrap();
        assert_eq!(
            c.iter().map(|c| c.tiles[0].input).collect::<Vec<_>>(),
            [1, 3, 5]
        );
    }

    #[test]
    fn mosaic_tiles_are_centered_multiples_of_eight_by_four() {
        let o = opts("--in a*9 --ch fmt=1080i,src=0,mosaic=3x3,ov=1");
        let c = plan_channels(&o).unwrap();
        let t = &c[0].tiles;
        assert_eq!(t.len(), 9);
        assert_eq!((t[0].w, t[0].h), (640, 360));
        assert_eq!((t[0].x, t[0].y), (0, 0));
        assert_eq!((t[4].x, t[4].y, t[4].input), (640, 360, 4));
        assert!(c[0].mosaic && c[0].ov);
        let o = opts("--in a --ch fmt=480i,src=0,mosaic=3x2");
        let t = &plan_channels(&o).unwrap()[0].tiles;
        assert_eq!((t[0].w, t[0].h), (232, 240));
        assert_eq!((t[0].x, t[5].x, t[5].y), (0, 464, 240));
        assert_eq!(t[5].input, 0);
    }

    #[test]
    fn a_slate_channel_has_no_tiles_and_needs_no_input() {
        let c = plan_channels(&opts("--ch fmt=720p,slate=1")).unwrap();
        assert!(c[0].tiles.is_empty());
        assert!(matches!(
            plan_channels(&opts("--ch fmt=720p,src=0")),
            Err(RunError::Usage(_))
        ));
        assert!(plan_channels(&opts("--ch fmt=576i")).is_err());
    }

    #[test]
    fn the_arenas_hold_the_sessions_and_the_conversion_path() {
        let a = arena_sizes(&opts("--in a --ch fmt=480i,src=0,n=2")).unwrap();
        // two 480i slots (about 10 MB each) and the conversion region
        assert!(
            a.device > (192 << 20) + (20 << 20) && a.device < (400 << 20),
            "{a:?}"
        );
        assert!(a.host > CONV_SCRATCH);
        let b = arena_sizes(&opts("--in a --ch fmt=480i,src=0,n=2 --conv-mb 64")).unwrap();
        assert!(b.device < a.device);
    }
}
