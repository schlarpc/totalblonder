//! The golden recorder: what the encoder decided and what crossed the host/device boundary,
//! as the JSON records of `crates/tb-golden/vectors/README.md` (the format the C++ encoder first wrote with
//! `--golden`, and the replay tests read).
//!
//! Off unless [`Encoder::record_golden`](crate::Encoder::record_golden) turns it on; then every
//! command and every tick appends records to a buffer that the caller drains with
//! [`Encoder::take_golden`](crate::Encoder::take_golden), in the order things happen. The
//! device operations of a plan (`op` records) are written when the plan is handed out, merged
//! with the records written while it was built at the position they were written, and followed
//! by the host's wait for it. Buffer names are the C++ encoder's expressions (`d_par`,
//! `h_list[L_CODE]`, `dy`, ...), found from the place of each region in the batch layout.
//!
//! Numbers are written as the C++ encoder's `printf` wrote them: integers as integers, doubles with
//! `%.17g` ([`g17`]), so that records compare byte for byte.

use tb_device::{DevBuf, EventId, HostBuf, Op, Plan, StreamId};
use tb_m2v_rate::control::{ChooseEvent, Observer, PassEvent, VbvEvent};
use tb_m2v_rate::{RateControl, Vbv};

pub use tb_golden::write::{Rec, g17};

use crate::au::AccessUnit;
use crate::gop::Frame;
use crate::layout::{BatchLayout, List, RING};

/// Where the regions of one batch lie, for naming the buffers of `op` records.
#[derive(Debug)]
struct BatchNames {
    /// Device regions by their first byte.
    dev: Vec<(u64, u64, &'static str)>,
    /// Host regions by their first byte.
    host: Vec<(u64, u64, &'static str)>,
    /// The device block of the batch (`ablk_d` of the C++ arena path).
    block: (u64, u64),
    /// The frame ring: first byte, length, stride of a slot, luma and chroma plane sizes.
    ring: (u64, u64, u64, u64, u64),
}

const LISTS: [(List, &str, &str); 8] = [
    (List::Up, "d_list[L_UP]", "h_list[L_UP]"),
    (List::Code, "d_list[L_CODE]", "h_list[L_CODE]"),
    (List::Me, "d_list[L_ME]", "h_list[L_ME]"),
    (List::Intra, "d_list[L_INTRA]", "h_list[L_INTRA]"),
    (List::Retry, "d_list[L_RETRY]", "h_list[L_RETRY]"),
    (List::Rec, "d_list[L_REC]", "h_list[L_REC]"),
    (List::Xf, "d_list[L_XF]", "h_list[L_XF]"),
    (List::Dev, "d_list[L_DEV]", "h_list[L_DEV]"),
];

impl BatchNames {
    fn new(lay: &BatchLayout, block: (u64, u64)) -> Self {
        let d = |b: DevBuf, n: &'static str| (b.off, b.len, n);
        let h = |b: HostBuf, n: &'static str| (b.off, b.len, n);
        let mut dev = vec![
            d(lay.d_tables, "d_T"),
            d(lay.d_par, "d_par"),
            d(lay.d_rowq, "d_rowq"),
            d(lay.d_upslot, "d_upslot"),
            d(lay.d_devslot, "d_devslot"),
            d(lay.d_rowstat, "d_rowstat"),
            d(lay.d_pic, "d_pic"),
            d(lay.d_out, "d_out"),
            d(lay.d_outpos, "d_outpos"),
        ];
        let mut host = vec![
            h(lay.h_par, "h_par"),
            h(lay.h_rowq, "h_rowq"),
            h(lay.h_upslot, "h_upslot"),
            h(lay.h_devslot, "h_devslot"),
            h(lay.h_rowstat, "h_rowstat"),
            h(lay.h_pic, "h_pic"),
            h(lay.h_out, "h_out"),
            h(lay.h_outpos, "h_outpos"),
        ];
        if let Some(r) = lay.h_recon {
            host.push(h(r, "h_recon"));
        }
        for (l, dn, hn) in LISTS {
            dev.push(d(lay.d_list[l as usize], dn));
            host.push(h(lay.h_list[l as usize], hn));
        }
        let g = &lay.geo;
        let ysz = u64::from(g.w.unsigned_abs()) * u64::from(g.h.unsigned_abs());
        let b = &lay.bufs;
        let ring_len = RING as u64 * u64::from(g.n.unsigned_abs()) * b.ring_stride;
        Self {
            dev,
            host,
            block,
            ring: (b.ring_y.off, ring_len, b.ring_stride, ysz, ysz / 4),
        }
    }

    /// The plane of the ring a device address lies in: 0, 1, 2 (Y, U, V), or none.
    fn plane(&self, off: u64) -> Option<usize> {
        let (base, len, stride, ysz, csz) = self.ring;
        if off < base || off >= base + len || stride == 0 {
            return None;
        }
        let rel = (off - base) % stride;
        if rel < ysz {
            Some(0)
        } else if rel < ysz + csz {
            Some(1)
        } else {
            Some(2)
        }
    }
}

/// The recorder's state.
#[derive(Debug)]
pub(crate) struct Log {
    /// Records ready for the caller.
    out: String,
    /// Records written while the current plan is built, with the number of its steps at the
    /// time.
    pend: Vec<(usize, String)>,
    /// Steps of the current plan before the records written now.
    pos: usize,
    /// The tick the records belong to (the C++ recorder's `g_gold.tick`: -1 before the first tick,
    /// then the last tick begun).
    pub tick: i64,
    /// Buffer names per batch.
    batches: Vec<BatchNames>,
    /// The host region of the tables (`e->T`).
    tables: Option<HostBuf>,
}

/// The recorder, or nothing.
#[derive(Debug, Default)]
pub(crate) struct Gold(Option<Box<Log>>);

impl Gold {
    /// Turn recording on (with an empty log) or off.
    pub fn set(&mut self, on: bool) {
        self.0 = on.then(|| {
            Box::new(Log {
                out: String::new(),
                pend: Vec::new(),
                pos: 0,
                tick: -1,
                batches: Vec::new(),
                tables: None,
            })
        });
    }

    /// Whether records are being written.
    pub fn on(&self) -> bool {
        self.0.is_some()
    }

    /// The tick of the records (-1 before the first).
    pub fn tick(&self) -> i64 {
        self.0.as_ref().map_or(-1, |l| l.tick)
    }

    /// From now on the records belong to this tick.
    pub fn set_tick(&mut self, tick: u64) {
        if let Some(l) = &mut self.0 {
            l.tick = i64::try_from(tick).unwrap_or(i64::MAX);
        }
    }

    /// Records written from now on come after the plan's present steps.
    pub fn at(&mut self, plan: &Plan) {
        if let Some(l) = &mut self.0 {
            l.pos = plan.steps.len();
        }
    }

    /// Write one record; `f` adds its fields. Nothing runs when recording is off.
    pub fn put(&mut self, kind: &str, f: impl FnOnce(&mut Rec)) {
        if let Some(l) = &mut self.0 {
            let mut r = Rec::new(kind);
            f(&mut r);
            l.pend.push((l.pos, r.finish()));
        }
    }

    /// A batch was laid out: the names of its buffers.
    pub fn batch(&mut self, lay: &BatchLayout, block: (u64, u64), tables: Option<HostBuf>) {
        if let Some(l) = &mut self.0 {
            l.batches.push(BatchNames::new(lay, block));
            if tables.is_some() {
                l.tables = tables;
            }
        }
    }

    /// A batch gave its memory back: its names are gone (a later batch may take the place).
    pub fn drop_batch(&mut self, block: (u64, u64)) {
        if let Some(l) = &mut self.0 {
            l.batches.retain(|n| n.block != block);
        }
    }

    /// The plan is handed out: its operations, merged with the records written while it was
    /// built, then (`wait`) the host's wait for it, with the C++ encoder's number of the wait
    /// (`which`: 0, 1, 2 for the three waits of a tick).
    pub fn commit(&mut self, plan: &Plan, wait: bool, which: Option<u8>) {
        let Some(l) = &mut self.0 else {
            return;
        };
        let pend = std::mem::take(&mut l.pend);
        let mut it = pend.into_iter().peekable();
        for (k, step) in plan.steps.iter().enumerate() {
            while let Some((_, text)) = it.next_if(|(p, _)| *p <= k) {
                l.out.push_str(&text);
            }
            let rec = l.op(step.stream, &step.op);
            l.out.push_str(&rec);
        }
        for (_, text) in it {
            l.out.push_str(&text);
        }
        if wait {
            let mut r = Rec::new("op");
            r.i("tick", l.tick)
                .s("op", "wait")
                .s("st", stream(plan.wait));
            if let Some(w) = which {
                r.i("which", i64::from(w));
            }
            l.out.push_str(&r.finish());
        }
        l.pos = 0;
    }

    /// No plan follows the records written since the last one: they are ready as they are.
    pub fn flush(&mut self) {
        if let Some(l) = &mut self.0 {
            for (_, text) in l.pend.drain(..) {
                l.out.push_str(&text);
            }
            l.pos = 0;
        }
    }

    /// The records ready so far, taken out.
    pub fn take(&mut self) -> String {
        self.flush();
        self.0
            .as_mut()
            .map(|l| std::mem::take(&mut l.out))
            .unwrap_or_default()
    }
}

fn stream(s: StreamId) -> &'static str {
    match s {
        StreamId::MAIN => "main",
        StreamId::UPLOAD => "upload",
        _ => "side",
    }
}

impl Log {
    fn dev_name(&self, b: DevBuf) -> String {
        for n in &self.batches {
            if (b.off, b.len) == n.block {
                return "ablk_d".to_owned();
            }
            if let Some(p) = n.plane(b.off) {
                return ["dy", "du", "dv"][p].to_owned();
            }
            if let Some((_, _, name)) = n.dev.iter().find(|(off, _, _)| *off == b.off) {
                return (*name).to_owned();
            }
        }
        format!("dev@{}", b.off)
    }

    fn host_name(&self, b: HostBuf) -> String {
        if self.tables.is_some_and(|t| t.off == b.off) {
            return "e->T".to_owned();
        }
        for n in &self.batches {
            if let Some((_, _, name)) = n.host.iter().find(|(off, _, _)| *off == b.off) {
                return (*name).to_owned();
            }
        }
        format!("host@{}", b.off)
    }

    /// The source of a frame upload is named after the plane it goes to (`sy`, `su_`, `sv`).
    fn frame_src(&self, dst: DevBuf, src: HostBuf) -> String {
        for n in &self.batches {
            if let Some(p) = n.plane(dst.off) {
                return ["sy", "su_", "sv"][p].to_owned();
            }
        }
        self.host_name(src)
    }

    fn op(&self, st: StreamId, op: &Op) -> String {
        let mut r = Rec::new("op");
        r.i("tick", self.tick);
        match op {
            Op::Fill { dst, byte } => {
                r.s("op", "fill")
                    .s("dst", &self.dev_name(*dst))
                    .i("byte", i64::from(*byte))
                    .u("n", dst.len);
            }
            Op::Upload { dst, src } => {
                r.s("op", "upload")
                    .s("dst", &self.dev_name(*dst))
                    .s("src", &self.frame_src(*dst, *src))
                    .u("n", dst.len);
            }
            Op::UploadRefs { dst, src, .. } => {
                r.s("op", "upload")
                    .s("dst", &self.dev_name(*dst))
                    .s("src", &self.host_name(*src))
                    .u("n", dst.len);
            }
            Op::Upload2d {
                dst,
                dst_pitch,
                src,
                src_pitch,
                width,
                rows,
            } => {
                r.s("op", "upload2d")
                    .s("dst", &self.dev_name(*dst))
                    .s("src", &self.frame_src(*dst, *src))
                    .i("dst_pitch", i64::from(*dst_pitch))
                    .i("src_pitch", i64::from(*src_pitch))
                    .i("width", i64::from(*width))
                    .i("rows", i64::from(*rows));
            }
            Op::Copy { dst, src } => {
                r.s("op", "copy")
                    .s("dst", &self.dev_name(*dst))
                    .s("src", &self.dev_name(*src))
                    .u("n", dst.len);
            }
            Op::Download { dst, src } => {
                r.s("op", "download")
                    .s("dst", &self.host_name(*dst))
                    .s("src", &self.dev_name(*src))
                    .u("n", dst.len);
            }
            Op::Launch(l) => {
                r.s("op", "launch")
                    .s("kernel", l.kernel.name())
                    .i("grid", i64::from(l.grid))
                    .i("block", i64::from(l.block));
            }
            Op::Record(e) => {
                r.s("op", "record");
                if *e != EventId::PARAMS {
                    r.s("event", "frames");
                }
            }
            Op::WaitFor(e) => {
                r.s("op", "waitfor");
                if *e != EventId::PARAMS {
                    r.s("event", "frames");
                }
            }
        }
        r.s("st", stream(st));
        r.finish()
    }
}

/// The rate control's hooks, writing `vbv`, `choose` and `pass` records for session `s`.
pub(crate) struct RcObs<'a> {
    pub g: &'a mut Gold,
    pub s: u32,
}

impl Observer for RcObs<'_> {
    fn vbv(&mut self, event: VbvEvent, vbv: &Vbv) {
        if !self.g.on() {
            return;
        }
        let (op, an, a, bn, b) = match event {
            VbvEvent::Advance { to } => ("advance", "to", to, "cap", vbv.cap()),
            VbvEvent::SetRate { at, rate } => ("set_rate", "at", at, "rate", rate),
            VbvEvent::Add { bits } => ("add", "bits", bits, "room", vbv.room()),
        };
        let s = self.s;
        self.g.put("vbv", |r| {
            r.i("s", i64::from(s))
                .s("op", op)
                .d(an, a)
                .d(bn, b)
                .d("D", vbv.d)
                .d("queue", vbv.queue)
                .d("t", vbv.t)
                .da("chg", vbv.changes.iter().flat_map(|c| [c.t, c.rate]));
        });
    }

    fn choose(&mut self, e: &ChooseEvent<'_>) {
        let s = self.s;
        self.g.put("choose", |r| {
            r.i("s", i64::from(s))
                .i("type", i64::from(e.pic_type))
                .d("tgt", e.target)
                .d("theta", e.theta)
                .d("hdr", e.hdr)
                .b("ok", e.ok)
                .i("code", i64::from(e.code))
                .d("frac", e.frac)
                .d("nzpred", e.nzpred)
                .hex("rowq", e.rowq);
        });
    }

    fn pass(&mut self, e: &PassEvent) {
        let (s, tick) = (self.s, self.g.tick());
        self.g.put("pass", |r| {
            r.i("tick", tick)
                .i("s", i64::from(s))
                .i("type", i64::from(e.pic_type))
                .b("have_ref", e.have_ref)
                .i("tries", i64::from(e.tries))
                .i("rung", i64::from(e.rung))
                .i("bytes", i64::from(e.bytes))
                .i("tex_bits", i64::from(e.tex_bits))
                .b("over_cap", e.over_cap)
                .ia("big_rows", e.big_rows.iter().map(|w| i64::from(*w)))
                .d("tgt", e.tgt)
                .d("limit", e.limit)
                .b("must", e.must)
                .b("want", e.want)
                .i("code", i64::from(e.code))
                .d("frac", e.frac)
                .d("nzpred", e.nzpred)
                .d("theta", e.theta)
                .d("hdr", e.hdr);
        });
    }
}

/// An `emit` record: a frame leaves the GOP structure for the coded-order queue.
pub(crate) fn emit(g: &mut Gold, s: u32, f: &Frame, film: Option<FilmRec>) {
    g.put("emit", |r| {
        r.i("s", i64::from(s))
            .u("disp", f.disp)
            .i("type", i64::from(f.pic_type.code()))
            .b("gop_start", f.gop_start)
            .b("off_grid", f.off_grid)
            .u("slot", u64::from(f.slot))
            .i("pts", f.pts)
            .u("avail_step", f.avail_step)
            .b("prog", f.progressive)
            .hex("cc", &f.cc);
        if film.is_some() {
            r.u("start", f.start)
                .u("units", u64::from(f.dur))
                .b("top_first", f.tff);
        }
    });
}

/// An `hdr` record: the headers just written for frame `f` as a picture of type `ty`.
#[allow(clippy::too_many_arguments, reason = "the fields of the record")]
pub(crate) fn hdr(
    g: &mut Gold,
    s: u32,
    f: &Frame,
    ty: u8,
    gop_hdr: bool,
    seq_hdr: bool,
    tref_base: u64,
    cbr: bool,
    bytes: &[u8],
    film: Option<FilmRec>,
) {
    g.put("hdr", |r| {
        r.i("s", i64::from(s))
            .i("type", i64::from(ty))
            .b("gop_hdr", gop_hdr)
            .b("seq_hdr", seq_hdr)
            .u("disp", f.disp)
            .u("tref_base", tref_base)
            .b("prog", f.progressive)
            .b("cbr", cbr)
            .hex("cc", &f.cc)
            .hex("bytes", bytes);
        if let Some(x) = film {
            r.u("time", x.time).b("tff", x.tff).b("rff", x.rff);
        }
    });
}

/// What the records of a frame add when it is not shown for one frame period in the
/// session's field order (film mode): the fields are absent otherwise, so the records of
/// sessions without such frames are what they always were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FilmRec {
    /// Display time of the first field in frame periods, rounded down (`hdr`: `time`).
    pub time: u64,
    /// top_field_first as written (`hdr`).
    pub tff: bool,
    /// repeat_first_field as written (`hdr`).
    pub rff: bool,
    /// Display units (`au`: `units`).
    pub units: u8,
    /// First field parity (`au`: `top_first`).
    pub top_first: bool,
}

/// What an `au` record says about the session after the picture.
pub(crate) struct AuState<'a> {
    pub s: u32,
    pub rc: &'a RateControl,
    pub newer: u8,
    pub ref_disp: [i64; 2],
    pub dup: bool,
    pub have_ref: bool,
    pub rekey: bool,
    pub tref_base: u64,
    /// The frame of the access unit, if shown for other than one frame period.
    pub film: Option<FilmRec>,
    /// The picture's time on the session's decoding clock, seconds.
    pub t: f64,
}

/// An `au` record: a picture as it leaves the encoder, with the session's state after it.
/// `pic`: what the kernels measured (zero for a repeat); `rowq`: the row codes of a coded
/// picture.
pub(crate) fn au(
    g: &mut Gold,
    st: &AuState<'_>,
    a: &AccessUnit,
    position_type: u8,
    pic: (u32, u32),
    rowq: Option<&[u8]>,
) {
    let tick = g.tick();
    let rc = st.rc;
    g.put("au", |r| {
        r.i("tick", tick)
            .i("s", i64::from(st.s))
            .u("coded", a.coded)
            .u("disp", a.disp)
            .i("type", i64::from(a.pic_type))
            .i("position_type", i64::from(position_type))
            .b("key", a.keyframe)
            .b("off_grid", a.off_grid)
            .b("repeat", a.repeat)
            .b("key_missed", a.key_missed)
            .u("len", a.data.len() as u64)
            .hex("fnv", &fnv1a(&a.data).to_le_bytes())
            .i("pts", a.pts)
            .i("dts", a.dts)
            .d("avg_q", f64::from(a.avg_q))
            .d("complexity", a.complexity)
            .u("bits", a.bits)
            .u("rate", a.rate)
            .i("ticket", i64::from(a.rate_ticket))
            .d("queue", a.queue_bits)
            .d("room", a.room_bits)
            .d("cap", a.cap_bits)
            .i("rung", i64::from(a.rung))
            .i("tries", i64::from(a.tries))
            .i("intra", i64::from(a.intra_mbs))
            .i("field", i64::from(a.field_mbs))
            .i("slice_bytes", i64::from(pic.0))
            .i("tex_bits", i64::from(pic.1))
            .b("cbr", rc.cbr)
            .da("theta", rc.mdl.iter().map(|m| m.theta))
            .da("mdl_hdr", rc.mdl.iter().map(|m| m.hdr))
            .da("X", rc.x)
            .d("R", rc.r)
            .ia("left", rc.left.iter().map(|x| i64::from(*x)))
            .ia("floor_hist", rc.floor_hist.iter().map(|x| i64::from(*x)))
            .ia("last_rung", rc.last_rung.iter().map(|x| i64::from(*x)))
            .d("last_qs", rc.last_qs)
            .d("avg_act", rc.avg_act)
            .u("violations", rc.stats.violations)
            .i("newer", i64::from(st.newer))
            .ia("ref_disp", st.ref_disp)
            .b("dup", st.dup)
            .b("have_ref", st.have_ref)
            .b("rekey", st.rekey)
            .u("tref_base", st.tref_base);
        if let Some(x) = st.film {
            // (the decoding time is `coded / fps` otherwise)
            r.u("units", u64::from(x.units))
                .b("top_first", x.top_first)
                .d("t", st.t);
        }
        if let Some(q) = rowq {
            r.hex("rowq", q);
        }
        if a.data.len() <= 65536 {
            r.hex("data", &a.data);
        }
    });
}

/// FNV-1a (64 bit), the digest of `au` records.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
