//! Traces and normal forms from the golden records (`crates/tb-golden/vectors/README.md`).
//!
//! - [`golden_plans`]: the plans of the recorded run, from its `op` records. Each host wait (`op`
//!   record `wait`) ends a plan; the plan waits on the waited stream. The operations keep the
//!   recorded buffer names (C++ expressions such as `d_par`, `h_list[L_CODE]`) and their normal
//!   form ([`NormalOp`]).
//! - [`golden_trace`]: the [`Trace`] a correct run of the case records, given where each named
//!   buffer lies ([`NameTable`]). Replayed with [`Compare::NormalForm`](super::Compare), it
//!   checks that a core issues the recorded operations, uploads the recorded bytes, and it hands
//!   back the recorded results.
//!
//! What the golden records carry, and therefore what the trace knows (everything else in the
//! host arena is neither checked nor written):
//!
//! | Plan | Uploads (checked at submit) | Downloads (written at wait) |
//! |---|---|---|
//! | `Begin` | `h_par` per coded slot (`begin.par`); `h_list[L_CODE]`, `[L_INTRA]`, `[L_ME]` (`lists`) | `h_rowstat` per coded slot (`rowstat.rows`) |
//! | `Quant` | `h_par` per coded slot (latest of `begin.par`, `qent.par`); `h_rowq` per coded slot (latest of `rc_plan.rowq`, `pass_out.rowq`); `h_list[L_XF]` (`qent.xf`); `h_list[L_RETRY]` (the `pass_out` records with action `again`) | `h_pic` per slot of the pass (`pass.bytes`, `pass.tex_bits`; for a picture without rate control, which has no `pass` record, `au.slice_bytes`, `au.tex_bits`) |
//! | `Finish` | `h_outpos` per coded slot (`finish.outpos`) | `h_out`: the slices of each coded picture (`au.data` without the bytes of the last `hdr` record of the session) at `4 * outpos` |
//!
//! Not known from the records, so not in the trace: the frames (`sy`, `su_`, `sv`), the upload
//! lists `h_list[L_UP]`, `h_upslot`, the reconstruction list `h_list[L_REC]`, and the bytes of
//! slots that do not code a picture in the tick.
//!
//! Only cases with one batch are supported: the `op` records do not say which batch a buffer
//! name belongs to.

use std::collections::BTreeMap;

use tb_golden::{GoldenError, Record as Gold};
use tb_kernel_abi::{PicStat, RowStat, SessPar};

use super::normal::{NormalOp, OpKind};
use super::scenario::{Phase, PlanTag};
use super::{Record, Trace};
use crate::kernel::{Kernel, Launch};
use crate::mem::{DevBuf, HostBuf};
use crate::plan::{EventId, Op, Plan, StreamId};
use crate::{ArenaSizes, Completion, Nanos, Ticket};

/// What went wrong reading or interpreting golden records.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GoldenTraceError {
    /// A record lacks a field or has one of another type.
    #[error(transparent)]
    Golden(#[from] GoldenError),
    /// A record says something this module cannot represent or that contradicts other records.
    #[error("golden record {index} (`{kind}`): {what}")]
    Record {
        /// Position of the record in the case (0-based).
        index: usize,
        /// The record's kind.
        kind: String,
        /// What is wrong.
        what: String,
    },
    /// The name table has no buffer of this name, or one too small for an operation on it.
    #[error("name table: {0}")]
    Name(String),
}

/// One recorded operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenOp {
    /// The operation in normal form.
    pub normal: NormalOp,
    /// The destination's C++ expression (copies and fills).
    pub dst: Option<String>,
    /// The source's C++ expression (copies).
    pub src: Option<String>,
    /// 2-D upload: `(dst_pitch, src_pitch, width, rows)`.
    pub pitches: Option<(u32, u32, u32, u32)>,
    /// Fill: the byte value.
    pub byte: u8,
}

/// One recorded plan: the operations up to one host wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenPlan {
    /// Tick and phase.
    pub tag: PlanTag,
    /// The stream the host waited for.
    pub wait: StreamId,
    /// The operations in issue order.
    pub ops: Vec<GoldenOp>,
}

impl GoldenPlan {
    /// The plan in normal form.
    pub fn normal(&self) -> Vec<NormalOp> {
        self.ops.iter().map(|o| o.normal).collect()
    }
}

/// Where each buffer the records name lies: the C++ expression of a host or device pointer and
/// the region it starts. A copy of `n` bytes from or to a name covers the first `n` bytes of its
/// region. (The tick's tests fill this table from the batch layout: `d_par` -> `d_par`,
/// `h_list[L_CODE]` -> `h_list[List::Code]`, ...)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameTable {
    /// Arena sizes of the replaying device.
    pub arenas: ArenaSizes,
    /// Host regions by name (`h_par`, `h_list[L_CODE]`, `sy`, ...).
    pub host: BTreeMap<String, HostBuf>,
    /// Device regions by name (`d_par`, `d_out`, `dy`, ...).
    pub dev: BTreeMap<String, DevBuf>,
}

impl NameTable {
    /// An empty table.
    pub fn new(arenas: ArenaSizes) -> Self {
        Self {
            arenas,
            host: BTreeMap::new(),
            dev: BTreeMap::new(),
        }
    }

    /// A table for tests: every name used by the plans gets its own region, as large as the
    /// largest operation on it, the regions one after the other with 256-byte alignment, and
    /// arenas just large enough.
    pub fn packed(plans: &[GoldenPlan]) -> Self {
        let mut host: BTreeMap<String, u64> = BTreeMap::new();
        let mut dev: BTreeMap<String, u64> = BTreeMap::new();
        let need = |m: &mut BTreeMap<String, u64>, name: &Option<String>, n: u64| {
            if let Some(name) = name {
                let e = m.entry(name.clone()).or_insert(0);
                *e = (*e).max(n);
            }
        };
        for o in plans.iter().flat_map(|p| &p.ops) {
            let n = o.normal.bytes;
            let span = |pitch: u32, width: u32, rows: u32| {
                u64::from(rows.saturating_sub(1)) * u64::from(pitch) + u64::from(width)
            };
            match (o.normal.op, o.pitches) {
                (OpKind::Upload2d, Some((dp, sp, w, r))) => {
                    need(&mut dev, &o.dst, span(dp, w, r));
                    need(&mut host, &o.src, span(sp, w, r));
                }
                (OpKind::Upload, _) => {
                    need(&mut dev, &o.dst, n);
                    need(&mut host, &o.src, n);
                }
                (OpKind::Download, _) => {
                    need(&mut host, &o.dst, n);
                    need(&mut dev, &o.src, n);
                }
                (OpKind::Fill, _) => need(&mut dev, &o.dst, n),
                (OpKind::Copy, _) => {
                    need(&mut dev, &o.dst, n);
                    need(&mut dev, &o.src, n);
                }
                _ => {}
            }
        }
        fn lay<T>(m: BTreeMap<String, u64>, mk: fn(u64, u64) -> T) -> (BTreeMap<String, T>, u64) {
            let mut at = 0;
            let mut out = BTreeMap::new();
            for (name, len) in m {
                out.insert(name, mk(at, len));
                at = (at + len).div_ceil(256) * 256;
            }
            (out, at)
        }
        let (host, h) = lay(host, HostBuf::new);
        let (dev, d) = lay(dev, DevBuf::new);
        Self {
            arenas: ArenaSizes { device: d, host: h },
            host,
            dev,
        }
    }

    fn host(&self, name: &str, len: u64) -> Result<HostBuf, GoldenTraceError> {
        let b = self
            .host
            .get(name)
            .ok_or_else(|| GoldenTraceError::Name(format!("no host buffer `{name}`")))?;
        b.slice(0, len).ok_or_else(|| {
            GoldenTraceError::Name(format!("host buffer `{name}` shorter than {len} bytes"))
        })
    }

    fn dev(&self, name: &str, len: u64) -> Result<DevBuf, GoldenTraceError> {
        let b = self
            .dev
            .get(name)
            .ok_or_else(|| GoldenTraceError::Name(format!("no device buffer `{name}`")))?;
        b.slice(0, len).ok_or_else(|| {
            GoldenTraceError::Name(format!("device buffer `{name}` shorter than {len} bytes"))
        })
    }
}

/// A trace made from golden records, with what it was made from.
#[derive(Clone, Debug, PartialEq)]
pub struct GoldenTrace {
    /// The trace: one record per plan, completions `Ok` and instantaneous (submitted and
    /// finished at 0; timing is the business of [`Fault::Delay`](super::Fault)).
    pub trace: Trace,
    /// Tick and phase of each plan.
    pub tags: Vec<PlanTag>,
    /// The recorded plans, by name.
    pub plans: Vec<GoldenPlan>,
}

fn bad(index: usize, rec: &Gold, what: impl Into<String>) -> GoldenTraceError {
    GoldenTraceError::Record {
        index,
        kind: rec.kind().to_owned(),
        what: what.into(),
    }
}

fn stream(index: usize, rec: &Gold) -> Result<StreamId, GoldenTraceError> {
    match rec.text("st")? {
        "main" => Ok(StreamId::MAIN),
        "upload" => Ok(StreamId::UPLOAD),
        "side" => Ok(StreamId::SIDE),
        s => Err(bad(index, rec, format!("unknown stream `{s}`"))),
    }
}

fn int<T: TryFrom<i64>>(index: usize, rec: &Gold, field: &str) -> Result<T, GoldenTraceError> {
    let v = rec.int(field)?;
    T::try_from(v).map_err(|_| bad(index, rec, format!("`{field}` = {v} out of range")))
}

/// The recorded plans, one per `wait` record, in order. Phases: `which` 0, 1, 2 are
/// `Begin`, `Quant`, `Finish`; a wait on the main stream without `which` between the `Quant`
/// and `Finish` waits of a tick is a correction pass (`Quant`); every other wait is `Other`.
pub fn golden_plans(records: &[Gold]) -> Result<Vec<GoldenPlan>, GoldenTraceError> {
    let mut plans = Vec::new();
    let mut ops = Vec::new();
    // (tick, in a quant phase)
    let mut quant: (i64, bool) = (i64::MIN, false);
    let mut last = None;
    for (i, r) in records.iter().enumerate().filter(|(_, r)| r.kind() == "op") {
        last = Some(i);
        let st = stream(i, r)?;
        let tick = r.int("tick")?;
        let name = r.text("op")?;
        if name == "wait" {
            if quant.0 != tick {
                quant = (tick, false);
            }
            let phase = if r.has("which") {
                match r.int("which")? {
                    0 => Phase::Begin,
                    1 => {
                        quant.1 = true;
                        Phase::Quant
                    }
                    2 => {
                        quant.1 = false;
                        Phase::Finish
                    }
                    w => return Err(bad(i, r, format!("wait `which` = {w}"))),
                }
            } else if quant.1 && st == StreamId::MAIN {
                Phase::Quant
            } else {
                Phase::Other
            };
            plans.push(GoldenPlan {
                tag: PlanTag { tick, phase },
                wait: st,
                ops: std::mem::take(&mut ops),
            });
            continue;
        }
        let op = OpKind::from_name(name)
            .ok_or_else(|| bad(i, r, format!("operation `{name}` has no plan step")))?;
        let text = |f: &str| -> Result<Option<String>, GoldenTraceError> {
            Ok(if r.has(f) {
                Some(r.text(f)?.to_owned())
            } else {
                None
            })
        };
        let mut g = GoldenOp {
            normal: NormalOp::transfer(st, op, 0),
            dst: text("dst")?,
            src: text("src")?,
            pitches: None,
            byte: 0,
        };
        match op {
            OpKind::Fill => {
                g.normal.bytes = int(i, r, "n")?;
                g.byte = int(i, r, "byte")?;
            }
            OpKind::Upload | OpKind::Download | OpKind::Copy => g.normal.bytes = int(i, r, "n")?,
            OpKind::Upload2d => {
                let p = (
                    int(i, r, "dst_pitch")?,
                    int(i, r, "src_pitch")?,
                    int(i, r, "width")?,
                    int(i, r, "rows")?,
                );
                g.normal.bytes = u64::from(p.2) * u64::from(p.3);
                g.pitches = Some(p);
            }
            OpKind::Launch => {
                let k = r.text("kernel")?;
                let name = Kernel::NAMES
                    .iter()
                    .find(|n| **n == k)
                    .ok_or_else(|| bad(i, r, format!("unknown kernel `{k}`")))?;
                g.normal.kernel = Some(name);
                g.normal.grid = int(i, r, "grid")?;
                g.normal.block = int(i, r, "block")?;
            }
            OpKind::Record | OpKind::WaitFor => {}
        }
        ops.push(g);
    }
    if let (false, Some(i)) = (ops.is_empty(), last) {
        return Err(bad(i, &records[i], "operations after the last wait"));
    }
    Ok(plans)
}

/// The step of a golden operation, with buffers from the table. Launches carry
/// [`Kernel::placeholder`] arguments; events are the parameter event (the only one the records
/// have).
fn step_of(o: &GoldenOp, names: &NameTable) -> Result<Op, GoldenTraceError> {
    let name = |n: &Option<String>| {
        n.clone()
            .ok_or_else(|| GoldenTraceError::Name("operation without a buffer name".to_owned()))
    };
    let n = o.normal.bytes;
    Ok(match o.normal.op {
        OpKind::Fill => Op::Fill {
            dst: names.dev(&name(&o.dst)?, n)?,
            byte: o.byte,
        },
        OpKind::Upload => Op::Upload {
            dst: names.dev(&name(&o.dst)?, n)?,
            src: names.host(&name(&o.src)?, n)?,
        },
        OpKind::Upload2d => {
            let (dp, sp, w, r) = o.pitches.unwrap_or_default();
            let span = |p: u32| u64::from(r.saturating_sub(1)) * u64::from(p) + u64::from(w);
            Op::Upload2d {
                dst: names.dev(&name(&o.dst)?, span(dp))?,
                dst_pitch: dp,
                src: names.host(&name(&o.src)?, span(sp))?,
                src_pitch: sp,
                width: w,
                rows: r,
            }
        }
        OpKind::Download => Op::Download {
            dst: names.host(&name(&o.dst)?, n)?,
            src: names.dev(&name(&o.src)?, n)?,
        },
        OpKind::Copy => Op::Copy {
            dst: names.dev(&name(&o.dst)?, n)?,
            src: names.dev(&name(&o.src)?, n)?,
        },
        OpKind::Launch => {
            let k = o.normal.kernel.unwrap_or_default();
            Op::Launch(Launch {
                kernel: Kernel::placeholder(k)
                    .ok_or_else(|| GoldenTraceError::Name(format!("unknown kernel `{k}`")))?,
                grid: o.normal.grid,
                block: o.normal.block,
            })
        }
        OpKind::Record => Op::Record(EventId::PARAMS),
        OpKind::WaitFor => Op::WaitFor(EventId::PARAMS),
    })
}

/// The plan of a golden plan, with buffers from the table (see [`golden_trace`]).
pub fn plan_of(g: &GoldenPlan, names: &NameTable) -> Result<Plan, GoldenTraceError> {
    let mut p = Plan::new();
    p.wait = g.wait;
    for o in &g.ops {
        p.push(o.normal.stream, step_of(o, names)?);
    }
    Ok(p)
}

/// Whether `buf` lies inside the host region of an upload (`up`) or download of the plan.
fn covered(plan: &Plan, buf: HostBuf, up: bool) -> bool {
    plan.steps.iter().any(|s| {
        let r = match s.op {
            Op::Upload { src, .. } if up => src,
            Op::Upload2d { src, .. } if up => src,
            Op::Download { dst, .. } if !up => dst,
            _ => return false,
        };
        buf.off >= r.off && buf.end() <= r.end()
    })
}

/// Add (or replace: the last bytes win, as the host arena holds them at submit) a region.
fn put(list: &mut Vec<(HostBuf, Vec<u8>)>, buf: HostBuf, bytes: Vec<u8>) {
    list.retain(|(b, _)| *b != buf);
    list.push((buf, bytes));
}

fn ints(v: &[i64]) -> Vec<u8> {
    v.iter().flat_map(|x| (*x as i32).to_le_bytes()).collect()
}

/// State of the tick being read.
#[derive(Default)]
struct TickState {
    tick: i64,
    /// Session (gid) -> slot, from `begin`.
    slot: BTreeMap<i64, u64>,
    /// Slots in work order (`begin` order).
    work: Vec<u64>,
    par: BTreeMap<u64, Vec<u8>>,
    rowq: BTreeMap<u64, Vec<u8>>,
    outpos: BTreeMap<u64, (u32, u32)>,
    lists: BTreeMap<&'static str, Vec<u8>>,
    /// Slots of the `pass_out` records with action `again` since the last wait.
    again: Vec<i64>,
    /// Index of the first `Quant` plan of the tick.
    first_quant: Option<usize>,
}

/// The trace a correct run of a golden case records (see the module documentation for what it
/// holds), with its tags and the recorded plans.
///
/// The plans' buffers come from `names`; their kernel arguments are placeholders, so the trace
/// is replayed with [`Compare::NormalForm`](super::Compare). The uploads and downloads of each
/// record are the regions of `names` the golden records give bytes for.
pub fn golden_trace(records: &[Gold], names: &NameTable) -> Result<GoldenTrace, GoldenTraceError> {
    let plans = golden_plans(records)?;
    let batches: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.kind() == "batch")
        .collect();
    let (bi, batch) = match batches.as_slice() {
        [b] => *b,
        [] => {
            return Err(GoldenTraceError::Name(
                "the case has no `batch` record".to_owned(),
            ));
        }
        [_, (i, r), ..] => return Err(bad(*i, r, "more than one batch is not supported")),
    };
    let geo = batch
        .pods::<tb_kernel_abi::Geo>("geo")?
        .first()
        .copied()
        .ok_or_else(|| bad(bi, batch, "empty `geo`"))?;
    let mbh = u64::try_from(geo.mbh).map_err(|_| bad(bi, batch, "negative mbh"))?;
    let cap_bytes = u64::try_from(batch.int("cap_words")?)
        .map_err(|_| bad(bi, batch, "negative cap_words"))?
        * 4;
    const PAR: u64 = size_of::<SessPar>() as u64;
    const ROW: u64 = size_of::<RowStat>() as u64;
    const PIC: u64 = size_of::<PicStat>() as u64;

    let mut out: Vec<Record> = Vec::with_capacity(plans.len());
    let mut cur = TickState::default();
    let mut open_uploads: Vec<(HostBuf, Vec<u8>)> = Vec::new();
    // The plan being read (built from its first op on) and how many of its ops have been seen.
    let mut open_plan: Option<Plan> = None;
    let mut ops_seen = 0usize;

    let base = |name: &str| {
        names
            .host
            .get(name)
            .copied()
            .ok_or_else(|| GoldenTraceError::Name(format!("no host buffer `{name}`")))
    };
    let at = |name: &str, off: u64, len: u64| -> Result<HostBuf, GoldenTraceError> {
        base(name)?.slice(off, len).ok_or_else(|| {
            GoldenTraceError::Name(format!("host buffer `{name}` has no {len} bytes at {off}"))
        })
    };

    for (i, r) in records.iter().enumerate() {
        let slot_of = |cur: &TickState, s: i64| {
            cur.slot
                .get(&s)
                .copied()
                .ok_or_else(|| bad(i, r, format!("session {s} codes no picture in this tick")))
        };
        // A download into the last plan closed, which must be of `phase`.
        let download = |out: &mut Vec<Record>,
                        phase: Phase,
                        plan: Option<usize>,
                        buf: HostBuf,
                        bytes: Vec<u8>|
         -> Result<(), GoldenTraceError> {
            let k = plan.or(out.len().checked_sub(1));
            let rec = k
                .and_then(|k| out.get_mut(k))
                .ok_or_else(|| bad(i, r, "no plan before it"))?;
            let tag = k.and_then(|k| plans.get(k)).map(|p| p.tag);
            if tag.map(|t| t.phase) != Some(phase) {
                return Err(bad(i, r, format!("follows a {tag:?} plan, not {phase:?}")));
            }
            if !covered(&rec.plan, buf, false) {
                return Err(bad(i, r, format!("{buf:?} is not downloaded by its plan")));
            }
            put(&mut rec.downloads, buf, bytes);
            Ok(())
        };
        match r.kind() {
            "tick" => {
                cur = TickState {
                    tick: r.int("tick")?,
                    ..TickState::default()
                };
            }
            "begin" => {
                let slot: u64 = int(i, r, "slot")?;
                let par = r.bytes("par")?;
                if par.len() as u64 != PAR {
                    return Err(bad(i, r, "`par` is not one SessPar"));
                }
                cur.slot.insert(r.int("s")?, slot);
                cur.work.push(slot);
                cur.par.insert(slot, par);
            }
            "lists" => {
                cur.lists.insert("h_list[L_CODE]", ints(&r.ints("code")?));
                cur.lists.insert("h_list[L_INTRA]", ints(&r.ints("intra")?));
                cur.lists.insert("h_list[L_ME]", ints(&r.ints("me")?));
            }
            "rc_plan" | "pass_out" => {
                let slot = slot_of(&cur, r.int("s")?)?;
                let rowq = r.bytes("rowq")?;
                if rowq.len() as u64 != mbh {
                    return Err(bad(i, r, "`rowq` is not mbh bytes"));
                }
                cur.rowq.insert(slot, rowq);
                if r.kind() == "pass_out" && r.text("action")? == "again" {
                    cur.again.push(slot as i64);
                }
            }
            "qent" => {
                let list = r.ints("list")?;
                let par = r.bytes("par")?;
                if par.len() as u64 != PAR * list.len() as u64 {
                    return Err(bad(i, r, "`par` is not one SessPar per listed slot"));
                }
                for (s, p) in list.iter().zip(par.as_chunks::<{ PAR as usize }>().0) {
                    let s = u64::try_from(*s).map_err(|_| bad(i, r, "negative slot"))?;
                    cur.par.insert(s, p.to_vec());
                }
                cur.lists.insert("h_list[L_XF]", ints(&r.ints("xf")?));
            }
            "finish" => {
                let pos = r.ints("outpos")?;
                if pos.len() != 2 * cur.work.len() {
                    return Err(bad(i, r, "not one `outpos` pair per coded picture"));
                }
                for (slot, p) in cur.work.iter().zip(pos.as_chunks::<2>().0) {
                    let w = |x: i64| u32::try_from(x).map_err(|_| bad(i, r, "outpos range"));
                    cur.outpos.insert(*slot, (w(p[0])?, w(p[1])?));
                }
            }
            "rowstat" => {
                let slot = slot_of(&cur, r.int("s")?)?;
                let rows = r.bytes("rows")?;
                if rows.len() as u64 != mbh * ROW {
                    return Err(bad(i, r, "`rows` is not mbh RowStat records"));
                }
                let buf = at("h_rowstat", slot * mbh * ROW, mbh * ROW)?;
                download(&mut out, Phase::Begin, None, buf, rows)?;
            }
            "pass" => {
                let slot = slot_of(&cur, r.int("s")?)?;
                let mut pic = PicStat {
                    bytes: int(i, r, "bytes")?,
                    tex_bits: int(i, r, "tex_bits")?,
                    ..PicStat::default()
                };
                if r.has("big_rows") {
                    for (w, v) in pic.big_rows.iter_mut().zip(r.ints("big_rows")?) {
                        *w = v as u32;
                    }
                }
                let buf = at("h_pic", slot * PIC, PIC)?;
                download(
                    &mut out,
                    Phase::Quant,
                    None,
                    buf,
                    bytemuck::bytes_of(&pic).to_vec(),
                )?;
            }
            "au" if !r.flag("repeat")? => {
                let s = r.int("s")?;
                let slot = slot_of(&cur, s)?;
                let data = r.bytes("data")?;
                // The headers: the last `hdr` record of the session before this one.
                let hdr = records[..i]
                    .iter()
                    .rev()
                    .find(|h| h.kind() == "hdr" && h.int("s").ok() == Some(s))
                    .ok_or_else(|| bad(i, r, "no `hdr` record of the session before it"))?
                    .bytes("bytes")?;
                let Some(slices) = data.strip_prefix(hdr.as_slice()) else {
                    return Err(bad(
                        i,
                        r,
                        "`data` does not start with the session's last headers",
                    ));
                };
                let slice_bytes: u64 = int(i, r, "slice_bytes")?;
                if slices.len() as u64 != slice_bytes.min(cap_bytes) {
                    return Err(bad(
                        i,
                        r,
                        format!(
                            "{} slice bytes in `data`, {} from `slice_bytes` and the cap",
                            slices.len(),
                            slice_bytes.min(cap_bytes)
                        ),
                    ));
                }
                let (first, words) = cur
                    .outpos
                    .get(&slot)
                    .copied()
                    .ok_or_else(|| bad(i, r, "no `finish` position for the picture"))?;
                if (slices.len() as u64).div_ceil(4) + 2 != u64::from(words) {
                    return Err(bad(
                        i,
                        r,
                        "words reserved by `finish` do not fit the slices",
                    ));
                }
                let buf = at("h_out", u64::from(first) * 4, slices.len() as u64)?;
                download(&mut out, Phase::Finish, None, buf, slices.to_vec())?;
                // A picture without rate control has no `pass` record: its picture statistics
                // are those of the first pass, which `au` repeats.
                if let Some(q) = cur.first_quant {
                    let pic = at("h_pic", slot * PIC, PIC)?;
                    if out
                        .get(q)
                        .is_some_and(|rec| !rec.downloads.iter().any(|(b, _)| *b == pic))
                    {
                        let ps = PicStat {
                            bytes: int(i, r, "slice_bytes")?,
                            tex_bits: int(i, r, "tex_bits")?,
                            ..PicStat::default()
                        };
                        download(
                            &mut out,
                            Phase::Quant,
                            Some(q),
                            pic,
                            bytemuck::bytes_of(&ps).to_vec(),
                        )?;
                    }
                }
            }
            "op" => {
                let g = plans
                    .get(out.len())
                    .ok_or_else(|| bad(i, r, "more operations than plans"))?;
                let plan = open_plan.get_or_insert_with(|| {
                    ops_seen = 0;
                    Plan::new()
                });
                if r.text("op")? == "wait" {
                    if ops_seen != g.ops.len() {
                        return Err(bad(i, r, "plan boundaries disagree"));
                    }
                    plan.wait = g.wait;
                    let plan = open_plan.take().unwrap_or_default();
                    if g.tag.phase == Phase::Quant && g.tag.tick == cur.tick {
                        cur.first_quant.get_or_insert(out.len());
                    }
                    out.push(Record {
                        plan,
                        uploads: std::mem::take(&mut open_uploads),
                        completion: Some(Completion {
                            ticket: Ticket(out.len() as u64),
                            status: Ok(()),
                            submitted: Nanos(0),
                            finished: Nanos(0),
                        }),
                        downloads: Vec::new(),
                    });
                    cur.again.clear();
                    continue;
                }
                let o = g
                    .ops
                    .get(ops_seen)
                    .ok_or_else(|| bad(i, r, "plan boundaries disagree"))?;
                ops_seen += 1;
                let op = step_of(o, names)?;
                plan.push(o.normal.stream, op);
                let (Op::Upload { src, .. }, Some(name)) = (op, o.src.as_deref()) else {
                    continue;
                };
                // The bytes the golden records give for this upload's source.
                let mut known: Vec<(HostBuf, Vec<u8>)> = Vec::new();
                match name {
                    "h_par" => {
                        for (slot, p) in &cur.par {
                            known.push((at(name, slot * PAR, PAR)?, p.clone()));
                        }
                    }
                    "h_rowq" => {
                        for (slot, q) in &cur.rowq {
                            known.push((at(name, slot * mbh, mbh)?, q.clone()));
                        }
                    }
                    "h_outpos" => {
                        for (slot, (a, b)) in &cur.outpos {
                            let mut v = a.to_le_bytes().to_vec();
                            v.extend(b.to_le_bytes());
                            known.push((at(name, slot * 8, 8)?, v));
                        }
                    }
                    "h_list[L_RETRY]" => {
                        let v = ints(&cur.again);
                        known.push((at(name, 0, v.len() as u64)?, v));
                    }
                    _ => {
                        if let Some(v) = cur.lists.get(name) {
                            known.push((at(name, 0, v.len() as u64)?, v.clone()));
                        }
                    }
                }
                for (buf, bytes) in known {
                    if bytes.is_empty() {
                        continue;
                    }
                    if !covered(plan, buf, true) {
                        return Err(bad(
                            i,
                            r,
                            format!(
                                "golden bytes for `{name}` at {buf:?} lie outside the upload {src:?}"
                            ),
                        ));
                    }
                    put(&mut open_uploads, buf, bytes);
                }
            }
            _ => {}
        }
    }
    if open_plan.is_some() || out.len() != plans.len() {
        return Err(GoldenTraceError::Name(
            "operations after the last wait".to_owned(),
        ));
    }
    Ok(GoldenTrace {
        trace: Trace {
            arenas: names.arenas,
            records: out,
        },
        tags: plans.iter().map(|p| p.tag).collect(),
        plans,
    })
}

#[cfg(test)]
mod tests;
