//! One batch (format) and its phases of a tick (`Batch::upload`, `pad_dev`, `begin`,
//! `launch_begin`, `quantize`, `launch_qent`, `retry`, `finish_gpu`, `collect` of the C++).
//!
//! The C++ keeps its parameters in pinned host memory and edits them there. Here the batch
//! owns them (`par`, `rowq`, lists, ...) and copies them into the host arena each time a plan
//! uploads them; results are copied out of the arena when the plan has completed. The arena
//! is therefore only touched between plans.

use tb_device::kernel::{
    EntPack, EntPicSum, EntResolve, EntRowSum, EntScatter, MbStat, Me, MvSmooth, PadCols, PadRows,
    Pyr, Quant, Recon, RowStat as KRowStat, SetMode, Xf,
};
use tb_device::{DevBuf, HostBuf, Kernel, Launch, Op, Plan, StreamId};
use tb_kernel_abi::{
    Geo, MODE_FWD, MODE_INTRA, PIC_B, PIC_I, PIC_P, PicStat, RowStat, SL_CUR, SL_REC, SL_ZERO,
    SessPar,
};
use tb_m2v_rate::control::{Pass, PicCtx};
use tb_m2v_syntax::PicType;

use super::session::{Pending, Session, Source, blank_frame, find, pic_type_of};
use crate::au::AccessUnit;
use crate::config::{EncoderOptions, SessionConfig};
use crate::golden::{self, Gold, RcObs};
use crate::gop::Frame;
use crate::layout::{BS, BatchLayout, List, ME_BS, RECON_BS, RING};

/// A picture being coded in this tick (`Batch::Work`).
#[derive(Debug)]
pub(crate) struct Work {
    /// Index of the session in the encoder's list (valid for the tick).
    pub si: usize,
    /// Slot of the session in the batch.
    pub idx: usize,
    pub f: Frame,
    /// `PIC_*` as coded (an I picture the ladder replaces becomes P).
    pub ty: u8,
    pub gop_hdr: bool,
    pub seq_hdr: bool,
    pub key: bool,
    pub old_tref_base: u64,
    pub hdr: Vec<u8>,
    pub intra: u32,
    pub nfield: u32,
    /// Ticket of the rate change that takes effect with this picture.
    pub ticket: u32,
    pub ctx: PicCtx,
}

/// What a phase did, for the tick's statistics.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PhaseCount {
    pub retry: u32,
    pub rexf: u32,
}

#[derive(Debug)]
pub(crate) struct Batch {
    pub id: u32,
    /// 8x8 blocks per thread block of the transform kernel (`EncoderOptions::xf_blocks`).
    pub xf_blocks: u32,
    /// The configuration the batch was made for.
    pub fmt: SessionConfig,
    /// Every buffer, at its place in the arenas.
    pub lay: BatchLayout,
    /// Offset and length of the batch's block in the device arena.
    pub dev_block: (u64, u64),
    /// Offset and length of the batch's block in the host arena.
    pub host_block: (u64, u64),
    /// Second set of host frame regions (overlapped upload).
    pub h_frames_alt: Option<HostBuf>,
    /// Session ids by slot.
    pub sess: Vec<Option<u32>>,
    pub used: u32,
    pub nstep: u64,
    pub idle_steps: u64,
    /// The device block has not been cleared and the tables are not on the device.
    pub fresh: bool,
    /// The plan in flight clears the block and uploads the tables.
    pub setup_flying: bool,
    // what the C++ keeps in pinned host memory
    par: Vec<SessPar>,
    rowq: Vec<u8>,
    rows: Vec<RowStat>,
    pic: Vec<PicStat>,
    outpos: Vec<u32>,
    lists: [Vec<i32>; 8],
    upslot: Vec<u8>,
    devslot: Vec<u8>,
    // the tick
    pub work: Vec<Work>,
    hdr_pool: Vec<Vec<u8>>,
    slot_of_sess: Vec<usize>,
    lcur: List,
    nl: usize,
    qdone: bool,
    n_intra: usize,
    n_me: usize,
    out_words: u32,
    dump_k: Option<usize>,
}

/// Copy `bytes` to the start of a host region (nothing when the region is too small or outside
/// the arena: the plan that names it then fails validation at the adapter).
fn put(host: &mut [u8], buf: HostBuf, bytes: &[u8]) {
    if let Some(b) = buf.slice(0, bytes.len() as u64)
        && let Some(dst) = b.of_mut(host)
    {
        dst.copy_from_slice(bytes);
    }
}

/// Copy the start of a host region to `out`; false when it does not lie in the arena.
fn get(host: &[u8], buf: HostBuf, out: &mut [u8]) -> bool {
    if let Some(b) = buf.slice(0, out.len() as u64)
        && let Some(src) = b.of(host)
    {
        out.copy_from_slice(src);
        return true;
    }
    false
}

/// The first `len` bytes of a device region (the whole region when it is shorter; the plan
/// then fails validation instead of the core panicking).
fn dhead(b: DevBuf, len: u64) -> DevBuf {
    DevBuf::new(b.off, len)
}

fn hhead(b: HostBuf, len: u64) -> HostBuf {
    HostBuf::new(b.off, len)
}

fn launch(plan: &mut Plan, stream: StreamId, kernel: Kernel, count: u64, block: u32) {
    plan.push(stream, Op::Launch(Launch::covering(kernel, count, block)));
}

impl Batch {
    pub fn new(
        id: u32,
        fmt: &SessionConfig,
        lay: BatchLayout,
        dev_block: (u64, u64),
        host_block: (u64, u64),
        h_frames_alt: Option<HostBuf>,
        xf_blocks: u32,
    ) -> Self {
        let n = lay.geo.n.unsigned_abs() as usize;
        let mbh = lay.geo.mbh.unsigned_abs() as usize;
        Self {
            id,
            xf_blocks,
            fmt: *fmt,
            lay,
            dev_block,
            host_block,
            h_frames_alt,
            sess: vec![None; n],
            used: 0,
            nstep: 0,
            idle_steps: 0,
            fresh: true,
            setup_flying: false,
            par: vec![SessPar::default(); n],
            rowq: vec![0; n * mbh],
            rows: vec![RowStat::default(); n * mbh],
            pic: vec![PicStat::default(); n],
            outpos: vec![0; 2 * n],
            lists: std::array::from_fn(|_| vec![0; n]),
            upslot: vec![0; n],
            devslot: vec![0; n],
            work: Vec::with_capacity(n),
            hdr_pool: Vec::with_capacity(n),
            slot_of_sess: vec![usize::MAX; n],
            lcur: List::Code,
            nl: 0,
            qdone: true,
            n_intra: 0,
            n_me: 0,
            out_words: 0,
            dump_k: None,
        }
    }

    pub fn n(&self) -> usize {
        self.lay.geo.n.unsigned_abs() as usize
    }
    fn mbh(&self) -> usize {
        self.lay.geo.mbh.unsigned_abs() as usize
    }
    pub fn nmb(&self) -> u32 {
        self.lay.geo.nmb.unsigned_abs()
    }
    fn ysz(&self) -> u64 {
        u64::from(self.lay.geo.w.unsigned_abs()) * u64::from(self.lay.geo.h.unsigned_abs())
    }
    fn csz(&self) -> u64 {
        self.ysz() / 4
    }
    fn cap_bytes(&self) -> u32 {
        self.lay.cap_words.saturating_mul(4)
    }
    /// Pictures being coded on the device in this tick (`na`).
    pub fn na(&self) -> usize {
        self.work.len()
    }

    /// `Batch::matches`.
    pub fn matches(&self, c: &SessionConfig) -> bool {
        let g = &self.lay.geo;
        c.w == self.fmt.w
            && c.h == self.fmt.h
            && c.fps_n == self.fmt.fps_n
            && c.fps_d == self.fmt.fps_d
            && c.interlaced == self.fmt.interlaced
            && (c.interlaced && c.field_pred) == (g.field_pred != 0)
            && c.range2 == g.range2
            && c.tpred == (g.tpred != 0)
    }

    /// The host frame region of a slot.
    pub fn host_frame(&self, idx: usize, alt: bool) -> HostBuf {
        let fb = self.lay.fbytes();
        let base = match (alt, self.h_frames_alt) {
            (true, Some(b)) => b,
            _ => self.lay.h_frames,
        };
        HostBuf::new(base.off + idx as u64 * fb, fb)
    }

    /// Offset in the device arena of the luma plane of ring slot `ring` of session slot `idx`.
    fn ring_off(&self, ring: u8, idx: usize) -> u64 {
        let b = &self.lay.bufs;
        b.ring_y.off + (u64::from(ring) * self.n() as u64 + idx as u64) * b.ring_stride
    }

    /// The planes (Y, U, V) of a ring slot of a session and the luma pitch.
    pub fn ring_planes(&self, ring: u8, idx: usize) -> ([DevBuf; 3], u32) {
        let y = self.ring_off(ring, idx);
        let (ysz, csz) = (self.ysz(), self.csz());
        (
            [
                DevBuf::new(y, ysz),
                DevBuf::new(y + ysz, csz),
                DevBuf::new(y + ysz + csz, csz),
            ],
            self.lay.geo.w.unsigned_abs(),
        )
    }

    /// Write the first `count` entries of a list to the host arena and queue their upload.
    fn up_list(&self, l: List, count: usize, st: StreamId, host: &mut [u8], plan: &mut Plan) {
        let k = l as usize;
        let bytes: &[u8] = bytemuck::cast_slice(&self.lists[k][..count.min(self.n())]);
        put(host, self.lay.h_list[k], bytes);
        let len = bytes.len() as u64;
        plan.push(
            st,
            Op::Upload {
                dst: dhead(self.lay.d_list[k], len),
                src: hhead(self.lay.h_list[k], len),
            },
        );
    }

    /// Write the parameter records to the host arena and queue their upload.
    fn up_par(&self, host: &mut [u8], plan: &mut Plan) {
        put(host, self.lay.h_par, bytemuck::cast_slice(&self.par));
        plan.main(Op::Upload {
            dst: self.lay.d_par,
            src: self.lay.h_par,
        });
    }

    /// The set-up of a new batch (arena path of `Batch::init`): clear the
    /// device block, upload the tables.
    pub fn setup(&mut self, h_tables: HostBuf, plan: &mut Plan) {
        if !self.fresh {
            return;
        }
        plan.main(Op::Fill {
            dst: DevBuf::new(self.dev_block.0, self.dev_block.1),
            byte: 0,
        });
        plan.main(Op::Upload {
            dst: self.lay.d_tables,
            src: h_tables,
        });
        self.setup_flying = true;
    }

    /// Padding launches for the frames listed in `list` with ring slots in `slots`.
    fn pads(&self, st: StreamId, list: DevBuf, slots: DevBuf, nu: usize, plan: &mut Plan) {
        let g = &self.lay.geo;
        let b = &self.lay.bufs;
        let (gw, gh) = (g.w, g.h);
        let (w, h) = (
            i32::try_from(self.fmt.w).unwrap_or(gw),
            i32::try_from(self.fmt.h).unwrap_or(gh),
        );
        for (p, plane) in [b.ring_y, b.ring_u, b.ring_v].into_iter().enumerate() {
            let (pw, ph, sw, sh) = if p == 0 {
                (gw, gh, w, h)
            } else {
                (gw / 2, gh / 2, w / 2, h / 2)
            };
            let wide = |x: i32| u64::from(x.unsigned_abs());
            let na = nu as i32;
            if pw > sw {
                let k = Kernel::PadCols(PadCols {
                    plane,
                    stride: b.ring_stride,
                    pw,
                    sw,
                    sh,
                    n: g.n,
                    list,
                    slot: slots,
                    na,
                });
                launch(plan, st, k, nu as u64 * wide(pw - sw) * wide(sh), BS);
            }
            if ph > sh {
                let k = Kernel::PadRows(PadRows {
                    plane,
                    stride: b.ring_stride,
                    pw,
                    sh,
                    ph,
                    n: g.n,
                    list,
                    slot: slots,
                    na,
                });
                launch(plan, st, k, nu as u64 * wide(pw) * wide(ph - sh), BS);
            }
        }
    }

    fn same_size(&self) -> (bool, bool) {
        let g = &self.lay.geo;
        let same_w = self.fmt.w == g.w.unsigned_abs();
        (same_w, same_w && self.fmt.h == g.h.unsigned_abs())
    }

    /// Copy the submitted frames straight into their padded planes in the ring; kernels only
    /// fill the padding (the C++ `Batch::upload`).
    pub fn upload(&mut self, sess: &mut [Session], st: StreamId, host: &mut [u8], plan: &mut Plan) {
        let (same_w, same) = self.same_size();
        let (w, h) = (u64::from(self.fmt.w), u64::from(self.fmt.h));
        let gw = u64::from(self.lay.geo.w.unsigned_abs());
        let (ysz, csz) = (self.ysz(), self.csz());
        let fbytes = self.lay.fbytes();
        let mut nu = 0;
        for idx in 0..self.n() {
            let Some(si) = self.sess[idx].and_then(|id| find(sess, id)) else {
                continue;
            };
            let s = &mut sess[si];
            let (slot, sy) = match s.up {
                Some(Pending {
                    slot,
                    src: Source::Host { alt },
                }) => (slot, self.host_frame(idx, alt).off),
                Some(Pending {
                    slot,
                    src: Source::HostAt { off },
                }) => (slot, off),
                _ => continue,
            };
            let dy = self.ring_off(slot, idx);
            let (du, dv) = (dy + ysz, dy + ysz + csz);
            let (su, sv) = (sy + w * h, sy + w * h + w * h / 4);
            if same {
                plan.push(
                    st,
                    Op::Upload {
                        dst: DevBuf::new(dy, fbytes),
                        src: HostBuf::new(sy, fbytes),
                    },
                );
            } else if same_w {
                for (d, sr, len) in [(dy, sy, w * h), (du, su, w * h / 4), (dv, sv, w * h / 4)] {
                    plan.push(
                        st,
                        Op::Upload {
                            dst: DevBuf::new(d, len),
                            src: HostBuf::new(sr, len),
                        },
                    );
                }
            } else {
                for (d, sr, dp, sp, rows) in [
                    (dy, sy, gw, w, h),
                    (du, su, gw / 2, w / 2, h / 2),
                    (dv, sv, gw / 2, w / 2, h / 2),
                ] {
                    let span = |pitch: u64| rows.saturating_sub(1) * pitch + sp;
                    let p32 = |x: u64| u32::try_from(x).unwrap_or(u32::MAX);
                    plan.push(
                        st,
                        Op::Upload2d {
                            dst: DevBuf::new(d, span(dp)),
                            dst_pitch: p32(dp),
                            src: HostBuf::new(sr, span(sp)),
                            src_pitch: p32(sp),
                            width: p32(sp),
                            rows: p32(rows),
                        },
                    );
                }
            }
            self.lists[List::Up as usize][nu] = idx as i32;
            nu += 1;
            self.upslot[idx] = slot;
            s.up = None;
            if st == StreamId::UPLOAD {
                s.fly_up = Some(slot);
            } else {
                s.fly_main = Some(slot);
            }
        }
        if nu == 0 || same {
            return;
        }
        self.up_list(List::Up, nu, st, host, plan);
        put(host, self.lay.h_upslot, &self.upslot);
        plan.push(
            st,
            Op::Upload {
                dst: self.lay.d_upslot,
                src: self.lay.h_upslot,
            },
        );
        self.pads(
            st,
            self.lay.d_list[List::Up as usize],
            self.lay.d_upslot,
            nu,
            plan,
        );
    }

    /// Frames written on the device by the caller: fill the padding of the coded size
    /// (the C++ `Batch::pad_dev`).
    pub fn pad_dev(&mut self, sess: &mut [Session], host: &mut [u8], plan: &mut Plan) {
        let mut nu = 0;
        for idx in 0..self.n() {
            let Some(si) = self.sess[idx].and_then(|id| find(sess, id)) else {
                continue;
            };
            let s = &mut sess[si];
            let Some(Pending {
                slot,
                src: Source::Dev,
            }) = s.up
            else {
                continue;
            };
            s.up = None;
            s.fly_main = Some(slot);
            self.lists[List::Dev as usize][nu] = idx as i32;
            nu += 1;
            self.devslot[idx] = slot;
        }
        if nu == 0 || self.same_size().1 {
            return;
        }
        self.up_list(List::Dev, nu, StreamId::MAIN, host, plan);
        put(host, self.lay.h_devslot, &self.devslot);
        plan.main(Op::Upload {
            dst: self.lay.d_devslot,
            src: self.lay.h_devslot,
        });
        self.pads(
            StreamId::MAIN,
            self.lay.d_list[List::Dev as usize],
            self.lay.d_devslot,
            nu,
            plan,
        );
    }

    /// Give the header buffers of the last tick's pictures back to the pool.
    fn recycle(&mut self) {
        for w in self.work.drain(..) {
            self.hdr_pool.push(w.hdr);
        }
    }

    /// Which sessions code a picture in this tick, and the repeats made on the host: the
    /// deadline-miss policy, a session on hold, B pictures between identical anchors, frames
    /// that never reached the device (first half of the C++ `Batch::begin`).
    /// `all_repeat`: the device is lost; nothing is coded, and a picture that cannot be a
    /// repeat (the first of a stream) waits.
    pub fn select(
        &mut self,
        sess: &mut [Session],
        all_repeat: bool,
        scratch: &mut Vec<u8>,
        g: &mut Gold,
    ) {
        self.recycle();
        for idx in 0..self.n() {
            let Some(si) = self.sess[idx].and_then(|id| find(sess, id)) else {
                continue;
            };
            let s = &mut sess[si];
            let Some(ff) = s.gop.coded.front() else {
                continue;
            };
            if ff.avail_step > self.nstep {
                continue;
            }
            let first = ff.pic_type == PicType::I && !s.have_ref;
            let bad = s.bad_slots & (1u8 << (ff.slot & 7)) != 0;
            let rp = all_repeat
                || s.hold
                || s.shed_now
                || (ff.pic_type == PicType::B && s.dup)
                || bad
                || (s.need_key && ff.pic_type != PicType::I);
            if rp && !first {
                if let Some(f) = s.gop.coded.pop_front() {
                    s.host_repeat(&self.lay.geo, &f, 0, scratch, g);
                }
                continue;
            }
            if all_repeat {
                continue;
            }
            let Some(f) = s.gop.coded.pop_front() else {
                continue;
            };
            let key = f.gop_start;
            let ty = f.pic_type.code();
            self.work.push(Work {
                si,
                idx,
                ty,
                gop_hdr: key,
                seq_hdr: key,
                key,
                old_tref_base: 0,
                hdr: self.hdr_pool.pop().unwrap_or_default(),
                intra: 0,
                nfield: 0,
                ticket: 0,
                ctx: PicCtx {
                    pic_type: ty,
                    key,
                    have_ref: s.have_ref,
                    nmb: self.lay.geo.nmb.unsigned_abs(),
                    fps: s.fps,
                    rate: 0.0,
                    gop: s.cfg.gop,
                    m: s.cfg.m.max(1),
                    to_next_key: 1,
                    gop_frames: 1.0,
                    frames: f64::from(f.dur.max(1)) / f64::from(s.upf.max(1)),
                    hdr_bits: 0.0,
                    cap_bytes: self.lay.cap_words.saturating_mul(4),
                    fixed_q: s.cfg.fixed_q,
                },
                f,
            });
        }
        self.nstep += 1;
        self.qdone = true;
    }

    /// The pictures of this tick: their parameters to the device (second half of
    /// the C++ `Batch::begin`).
    pub fn begin(
        &mut self,
        sess: &mut [Session],
        opt: &EncoderOptions,
        host: &mut [u8],
        plan: &mut Plan,
        g: &mut Gold,
    ) {
        if self.work.is_empty() {
            return;
        }
        g.at(plan);
        let (mut nme, mut ni) = (0, 0);
        for k in 0..self.work.len() {
            let wk = &mut self.work[k];
            let s = &mut sess[wk.si];
            // a rate change takes effect here
            let mut new_rate = None;
            // (for a fixed-quantizer session too: its buffer is drained at the new rate)
            if s.pending_ticket != 0 && (!s.pending_at_gop || wk.key) {
                s.rate = s.pending_rate;
                new_rate = Some(s.rate as f64);
                wk.ticket = s.pending_ticket;
                s.pending_ticket = 0;
            }
            let fixed_q = s.cfg.fixed_q[usize::from(wk.ty.clamp(1, 3)) - 1];
            let t = s.t();
            let pi =
                s.rc.begin_picture_obs(t, new_rate, wk.ty, fixed_q, &mut RcObs { g, s: s.id });
            wk.old_tref_base = s.tref_base;
            if wk.key {
                s.tref_base = wk.f.disp;
            }
            let newer = s.newer & 1;
            let fwd = if wk.ty == PIC_B { 1 - newer } else { newer };
            let disp = i64::try_from(wk.f.disp).unwrap_or(i64::MAX);
            let dist = |r: u8| (disp - s.ref_disp[usize::from(r)]).clamp(-127, 127) as i8;
            self.par[wk.idx] = SessPar {
                pic_type: wk.ty,
                cur: wk.f.slot,
                fwd,
                bwd: newer,
                rec: 1 - newer,
                keep: 64,
                aq: u8::from(s.cfg.aq && s.rc.cbr),
                want_pred: u8::from(wk.ty != PIC_B || (opt.keep_recon && wk.idx == 0)),
                avg_act: pi.avg_act,
                dist_f: dist(fwd),
                dist_b: dist(newer),
                // the field order of this picture (the session's, unless the pictures before
                // it left an odd number of fields)
                tff: u8::from(if s.cfg.interlaced {
                    wk.f.tff
                } else {
                    s.cfg.tff
                }),
                qfloor: pi.qfloor,
                lam: pi.lam,
                pdist_f: s.last_dist_f,
                pdist_b: s.last_dist_b,
                frame_only: u8::from(s.frame_only(&self.lay.geo, &wk.f)),
                zigzag: u8::from(s.zigzag(&self.lay.geo, &wk.f)),
            };
            if g.on() {
                let (tick, bid, par) = (g.tick(), self.id, self.par[wk.idx]);
                let rc = &s.rc;
                g.put("begin", |r| {
                    r.i("tick", tick)
                        .i("s", i64::from(s.id))
                        .i("b", i64::from(bid))
                        .u("slot", wk.idx as u64)
                        .i("type", i64::from(wk.ty))
                        .b("key", wk.key)
                        .u("disp", wk.f.disp)
                        .u("coded", s.n_coded)
                        .d("t", t)
                        .b("cbr", rc.cbr)
                        .d("e_before", rc.cur.e_before)
                        .d("cap", rc.cur.cap)
                        .u("rate", s.rate)
                        .i("ticket", i64::from(wk.ticket))
                        .i("fixed_q", i64::from(fixed_q))
                        .ia("floor_hist", rc.floor_hist.iter().map(|x| i64::from(*x)))
                        .d("avg_act", rc.avg_act)
                        .d("last_qs", rc.last_qs)
                        .i("newer", i64::from(s.newer))
                        .ia("ref_disp", s.ref_disp)
                        .i("last_dist_f", i64::from(s.last_dist_f))
                        .i("last_dist_b", i64::from(s.last_dist_b))
                        .b("aq", s.cfg.aq)
                        .b("tff", s.cfg.tff)
                        .hex("par", bytemuck::bytes_of(&par));
                });
            }
            wk.ctx.rate = s.rate as f64;
            self.lists[List::Code as usize][k] = wk.idx as i32;
            if wk.ty == PIC_I {
                self.lists[List::Intra as usize][ni] = wk.idx as i32;
                ni += 1;
            } else {
                self.lists[List::Me as usize][nme] = wk.idx as i32;
                nme += 1;
            }
        }
        if g.on() {
            let (tick, bid, na) = (g.tick(), self.id, self.work.len());
            let l = &self.lists;
            let ints = |k: List, n: usize| l[k as usize][..n].iter().map(|x| i64::from(*x));
            g.put("lists", |r| {
                r.i("tick", tick)
                    .i("b", i64::from(bid))
                    .ia("code", ints(List::Code, na))
                    .ia("intra", ints(List::Intra, ni))
                    .ia("me", ints(List::Me, nme));
            });
        }
        self.up_par(host, plan);
        self.up_list(List::Code, self.work.len(), StreamId::MAIN, host, plan);
        if ni > 0 {
            self.up_list(List::Intra, ni, StreamId::MAIN, host, plan);
        }
        if nme > 0 {
            self.up_list(List::Me, nme, StreamId::MAIN, host, plan);
        }
        self.n_intra = ni;
        self.n_me = nme;
    }

    fn xf(&self, list: List, na: usize, plan: &mut Plan) {
        let l = &self.lay;
        let k = Kernel::Xf(Xf {
            g: l.geo,
            tables: l.d_tables,
            b: l.bufs,
            par: l.d_par,
            list: l.d_list[list as usize],
            na: na as i32,
            info: l.d_info,
            act: l.d_act,
            coef: l.d_coef,
            predbuf: l.d_pred,
            hist: l.d_hist,
            cmask: l.d_cmask,
        });
        let nblocks = na as u64 * u64::from(self.nmb()) * 6;
        plan.main(Op::Launch(Launch {
            kernel: k,
            grid: u32::try_from(nblocks.div_ceil(u64::from(self.xf_blocks))).unwrap_or(u32::MAX),
            block: self.xf_blocks * 8,
        }));
    }

    fn set_mode(&self, list: List, na: usize, mode: u8, plan: &mut Plan) {
        let l = &self.lay;
        let k = Kernel::SetMode(SetMode {
            info: l.d_info,
            nmb: l.geo.nmb,
            list: l.d_list[list as usize],
            na: na as i32,
            mode: i32::from(mode),
        });
        launch(
            plan,
            StreamId::MAIN,
            k,
            na as u64 * u64::from(self.nmb()),
            BS,
        );
    }

    /// The kernels of the first phase, queued after every batch's parameters
    /// (the C++ `Batch::launch_begin`).
    pub fn launch_begin(&self, plan: &mut Plan) {
        let na = self.work.len();
        if na == 0 {
            return;
        }
        let l = &self.lay;
        let (g, b) = (l.geo, l.bufs);
        let (ni, nme) = (self.n_intra, self.n_me);
        let nmb = u64::from(self.nmb());
        let ysz = self.ysz();
        let m = StreamId::MAIN;
        if ni > 0 {
            self.set_mode(List::Intra, ni, MODE_INTRA, plan);
        }
        if nme > 0 {
            let list = l.d_list[List::Me as usize];
            let k = Kernel::Pyr(Pyr {
                src: b.ring_y,
                sstride: b.ring_stride,
                sw: g.w,
                sh: g.h,
                d1: b.cur1,
                d1stride: ysz / 4,
                d2: b.cur2,
                d2stride: ysz / 16,
                n: g.n,
                list,
                na: nme as i32,
                par: l.d_par,
                ssel: SL_CUR,
                dsel: SL_ZERO,
            });
            launch(plan, m, k, nme as u64 * ysz / 64, BS);
            let k = Kernel::Me(Me {
                g,
                b,
                par: l.d_par,
                list,
                na: nme as i32,
                prev: l.d_info,
                out: l.d_info_me,
            });
            launch(plan, m, k, nme as u64 * nmb, ME_BS);
            let k = Kernel::MvSmooth(MvSmooth {
                g,
                b,
                par: l.d_par,
                list,
                na: nme as i32,
                input: l.d_info_me,
                out: l.d_info,
            });
            launch(plan, m, k, nme as u64 * nmb, BS);
        }
        let code = l.d_list[List::Code as usize];
        let k = Kernel::MbStat(MbStat {
            g,
            ring_y: b.ring_y,
            ring_stride: b.ring_stride,
            par: l.d_par,
            list: code,
            na: na as i32,
            info: l.d_info,
            act: l.d_act,
        });
        launch(plan, m, k, na as u64 * nmb, BS);
        // ---- transform and statistics
        self.xf(List::Code, na, plan);
        plan.main(Op::Launch(Launch {
            kernel: Kernel::RowStat(KRowStat {
                g,
                list: code,
                na: na as i32,
                hist: l.d_hist,
                act: l.d_act,
                info: l.d_info,
                out: l.d_rowstat,
            }),
            grid: (na * self.mbh()) as u32,
            block: l.row_bs,
        }));
        plan.main(Op::Download {
            dst: l.h_rowstat,
            src: l.d_rowstat,
        });
    }

    /// Quantize and pack the sessions of the current list (the C++
    /// `Batch::launch_qent`).
    fn launch_qent(
        &mut self,
        sess: &mut [Session],
        host: &mut [u8],
        plan: &mut Plan,
        count: &mut PhaseCount,
        g: &mut Gold,
    ) {
        let mbh = self.mbh();
        let nl = self.nl;
        // sessions whose quantizer went below the floor that k_xf assumed: transform again,
        // keeping everything
        let mut nx = 0;
        for i in 0..nl {
            let sidx = self.lists[self.lcur as usize][i] as usize;
            let lo = self.rowq[sidx * mbh..(sidx + 1) * mbh]
                .iter()
                .copied()
                .fold(31, u8::min);
            if lo >= self.par[sidx].qfloor {
                continue;
            }
            self.par[sidx].qfloor = 1;
            self.lists[List::Xf as usize][nx] = sidx as i32;
            nx += 1;
            count.rexf += 1;
            if let Some(wk) = self.work.get(self.slot_of_sess[sidx]) {
                sess[wk.si].rc.stats.rexf += 1;
            }
        }
        if g.on() {
            g.at(plan);
            let (tick, bid) = (g.tick(), self.id);
            let list = &self.lists[self.lcur as usize][..nl];
            let xf = &self.lists[List::Xf as usize][..nx];
            let pars: Vec<u8> = list
                .iter()
                .flat_map(|i| bytemuck::bytes_of(&self.par[*i as usize]).to_vec())
                .collect();
            g.put("qent", |r| {
                r.i("tick", tick)
                    .i("b", i64::from(bid))
                    .ia("list", list.iter().map(|x| i64::from(*x)))
                    .ia("xf", xf.iter().map(|x| i64::from(*x)))
                    .hex("par", &pars);
            });
        }
        self.up_par(host, plan);
        if nx > 0 {
            self.up_list(List::Xf, nx, StreamId::MAIN, host, plan);
            self.xf(List::Xf, nx, plan);
        }
        let l = &self.lay;
        put(host, l.h_rowq, &self.rowq);
        plan.main(Op::Upload {
            dst: l.d_rowq,
            src: l.h_rowq,
        });
        let g = l.geo;
        let dl = l.d_list[self.lcur as usize];
        let na = nl as i32;
        let nmb = u64::from(self.nmb());
        let m = StreamId::MAIN;
        let k = Kernel::Quant(Quant {
            g,
            tables: l.d_tables,
            par: l.d_par,
            list: dl,
            na,
            rowq: l.d_rowq,
            info: l.d_info,
            act: l.d_act,
            coef: l.d_coef,
            cmask: l.d_cmask,
            lev: l.d_lev,
            nzmask: l.d_nzmask,
            nz: l.d_nz,
            mbq: l.d_mbq,
        });
        launch(plan, m, k, nl as u64 * nmb * 6, BS);
        let k = Kernel::EntResolve(EntResolve {
            g,
            par: l.d_par,
            list: dl,
            na,
            info: l.d_info,
            nz: l.d_nz,
            lev: l.d_lev,
            code: l.d_code,
        });
        launch(plan, m, k, nl as u64 * nmb, BS);
        let k = Kernel::EntPack(EntPack {
            g,
            tables: l.d_tables,
            par: l.d_par,
            list: dl,
            na,
            code: l.d_code,
            info: l.d_info,
            lev: l.d_lev,
            nzmask: l.d_nzmask,
            mbq: l.d_mbq,
            scratch: l.d_scratch,
            mbbits: l.d_mbbits,
            mbtex: l.d_mbtex,
        });
        launch(plan, m, k, nl as u64 * nmb, BS);
        plan.main(Op::Launch(Launch {
            kernel: Kernel::EntRowSum(EntRowSum {
                g,
                list: dl,
                na,
                mbbits: l.d_mbbits,
                mbtex: l.d_mbtex,
                mboff: l.d_mboff,
                slice_bytes: l.d_slice_bytes,
                slice_tex: l.d_slice_tex,
            }),
            grid: (nl * mbh) as u32,
            block: l.row_bs,
        }));
        let k = Kernel::EntPicSum(EntPicSum {
            g,
            list: dl,
            na,
            slice_bytes: l.d_slice_bytes,
            slice_tex: l.d_slice_tex,
            slice_off: l.d_slice_off,
            pic: l.d_pic,
        });
        launch(plan, m, k, nl as u64, BS);
        plan.main(Op::Download {
            dst: l.h_pic,
            src: l.d_pic,
        });
    }

    /// Rate control on the host with the row statistics that came back, then quantization and
    /// packing (the C++ `Batch::quantize`).
    pub fn quantize(
        &mut self,
        sess: &mut [Session],
        host: &mut [u8],
        plan: &mut Plan,
        count: &mut PhaseCount,
        g: &mut Gold,
    ) {
        let na = self.work.len();
        if na == 0 {
            return;
        }
        g.at(plan);
        let mbh = self.mbh();
        get(
            host,
            self.lay.h_rowstat,
            bytemuck::cast_slice_mut(&mut self.rows),
        );
        // ---- rate control: targets and row quantizers
        for k in 0..na {
            let wk = &mut self.work[k];
            let s = &mut sess[wk.si];
            let rs = &self.rows[wk.idx * mbh..(wk.idx + 1) * mbh];
            let tick = g.tick();
            g.put("rowstat", |r| {
                r.i("tick", tick)
                    .i("s", i64::from(s.id))
                    .hex("rows", bytemuck::cast_slice(rs));
            });
            for r in rs {
                wk.intra = wk.intra.wrapping_add(r.n_intra);
                wk.nfield = wk.nfield.wrapping_add(r.n_field);
            }
            s.headers(
                &self.lay.geo,
                &wk.f,
                pic_type_of(wk.ty),
                wk.gop_hdr,
                wk.seq_hdr,
                &mut wk.hdr,
            );
            golden::hdr(
                g,
                s.id,
                &wk.f,
                wk.ty,
                wk.gop_hdr,
                wk.seq_hdr,
                s.tref_base,
                s.rc.cbr,
                &wk.hdr,
                s.film(&wk.f),
            );
            wk.ctx.hdr_bits = 8.0 * wk.hdr.len() as f64;
            wk.ctx.to_next_key = s.to_next_key(&wk.f);
            // display time to the next key frame on the grid, in frame periods
            // (at least one, as the picture count)
            wk.ctx.gop_frames = match s.gop.next_key.saturating_sub(wk.f.start) {
                0 => 1.0,
                left => left as f64 / f64::from(s.upf.max(1)),
            };
            let rq = &mut self.rowq[wk.idx * mbh..(wk.idx + 1) * mbh];
            self.par[wk.idx].keep =
                s.rc.plan_picture_obs(&wk.ctx, rs, rq, &mut RcObs { g, s: s.id });
            if g.on() {
                let (rc, keep) = (&s.rc, self.par[wk.idx].keep);
                let to_next_key = i64::from(wk.ctx.to_next_key);
                let cap_bytes = u64::from(self.lay.cap_words) * 4;
                g.put("rc_plan", |r| {
                    r.i("tick", tick)
                        .i("s", i64::from(s.id))
                        .i("type", i64::from(wk.ty))
                        .b("key", wk.key)
                        .b("cbr", rc.cbr)
                        .i("intra", i64::from(wk.intra))
                        .i("nfield", i64::from(wk.nfield))
                        .d("hdr_bits", rc.cur.hdr_bits)
                        .d("avg_act", rc.avg_act)
                        .d("rate", s.rate as f64)
                        .d("fps", s.fps)
                        .u("gop", u64::from(s.cfg.gop))
                        .u("m", u64::from(s.cfg.m))
                        .i("to_next_key", to_next_key)
                        .d("R", rc.r)
                        .da("X", rc.x)
                        .ia("left", rc.left.iter().map(|x| i64::from(*x)))
                        .d("target", rc.cur.target)
                        .d("hard", rc.cur.hard)
                        .d("limit", rc.cur.limit)
                        .i("code", i64::from(rc.cur.code))
                        .d("frac", rc.cur.frac)
                        .d("nzpred", rc.cur.nzpred)
                        .i("rung", i64::from(rc.cur.rung))
                        .i("keep", i64::from(keep))
                        .ia("last_rung", rc.last_rung.iter().map(|x| i64::from(*x)))
                        .d("last_qs", rc.last_qs)
                        .d("e_before", rc.cur.e_before)
                        .d("cap", rc.cur.cap)
                        .u("cap_bytes", cap_bytes)
                        .hex("rowq", rq);
                    // a frame shown for other than one frame period, or a GOP of such frames
                    if wk.ctx.frames != 1.0
                        || wk.ctx.gop_frames != f64::from(wk.ctx.to_next_key.max(1))
                    {
                        r.d("gop_frames", wk.ctx.gop_frames)
                            .d("frames", wk.ctx.frames);
                    }
                });
            }
        }
        // ---- quantize + pack; retry() repeats it for the sessions that must or should be
        // corrected
        self.nl = na;
        self.lcur = List::Code;
        self.slot_of_sess.fill(usize::MAX);
        for (k, wk) in self.work.iter().enumerate() {
            self.slot_of_sess[wk.idx] = k;
        }
        self.qdone = false;
        self.launch_qent(sess, host, plan, count, g);
    }

    /// After the sizes are back: quantize again the sessions that need it; false when the
    /// batch is done (the C++ `Batch::retry`).
    pub fn retry(
        &mut self,
        sess: &mut [Session],
        host: &mut [u8],
        plan: &mut Plan,
        count: &mut PhaseCount,
        g: &mut Gold,
    ) -> bool {
        if self.qdone {
            return false;
        }
        g.at(plan);
        let mbh = self.mbh();
        get(
            host,
            self.lay.h_pic,
            bytemuck::cast_slice_mut(&mut self.pic),
        );
        let (mut nr, mut nskip) = (0, 0);
        for i in 0..self.nl {
            let sidx = self.lists[self.lcur as usize][i] as usize;
            let Some(wk) = self.work.get_mut(self.slot_of_sess[sidx]) else {
                continue;
            };
            let s = &mut sess[wk.si];
            let rs = &self.rows[sidx * mbh..(sidx + 1) * mbh];
            let rq = &mut self.rowq[sidx * mbh..(sidx + 1) * mbh];
            let pass = s.rc.after_pass_obs(
                &mut wk.ctx,
                rs,
                self.pic[sidx],
                rq,
                &mut RcObs { g, s: s.id },
            );
            if wk.ctx.pic_type != wk.ty {
                // an I picture replaced by a P picture: no GOP header, temporal_reference keeps
                // counting; and no sequence header. One before a P picture is legal (H.262
                // 6.1.1.6, p.13: the first picture after a repeated sequence header "shall be
                // either an I-picture or a P-picture"), but a repeated header is there for
                // random access, and this picture is all-skipped: a decoder that starts on it
                // has no reference and shows an undefined picture until the next I picture. The
                // session's sequence headers stay on pictures a decoder can start with.
                wk.ty = wk.ctx.pic_type;
                wk.gop_hdr = false;
                wk.seq_hdr = false;
                wk.key = false;
                self.par[sidx].pic_type = PIC_P;
                self.par[sidx].fwd = s.newer & 1;
                s.tref_base = wk.old_tref_base;
            }
            if let Pass::Again {
                keep, all_skipped, ..
            } = pass
            {
                self.par[sidx].keep = keep;
                if all_skipped {
                    self.par[sidx].want_pred = 1;
                    self.lists[List::Rec as usize][nskip] = sidx as i32;
                    nskip += 1;
                }
                self.lists[List::Retry as usize][nr] = sidx as i32;
                nr += 1;
            }
            if g.on() {
                let (tick, par, rc) = (g.tick(), self.par[sidx], &s.rc);
                let again = matches!(pass, Pass::Again { .. });
                let m = rc.mdl[usize::from(wk.ty)];
                g.put("pass_out", |r| {
                    r.i("tick", tick)
                        .i("s", i64::from(s.id))
                        .s("action", if again { "again" } else { "accept" })
                        .i("type", i64::from(wk.ty))
                        .b("key", wk.key)
                        .i("rung", i64::from(rc.cur.rung))
                        .i("keep", i64::from(par.keep))
                        .i("want_pred", i64::from(par.want_pred))
                        .i("code", i64::from(rc.cur.code))
                        .d("frac", rc.cur.frac)
                        .d("nzpred", rc.cur.nzpred)
                        .i("tries", i64::from(rc.cur.tries))
                        .u("violations", rc.stats.violations)
                        .u("isub", rc.stats.isub)
                        .d("theta", m.theta)
                        .d("hdr", m.hdr)
                        .hex("rowq", rq);
                });
            }
        }
        if nr == 0 {
            self.qdone = true;
            return false;
        }
        count.retry += nr as u32;
        self.up_list(List::Retry, nr, StreamId::MAIN, host, plan);
        if nskip > 0 {
            // all-skipped pictures: zero forward vectors everywhere, prediction recomputed for
            // the reconstruction
            self.up_par(host, plan);
            self.up_list(List::Rec, nskip, StreamId::MAIN, host, plan);
            self.set_mode(List::Rec, nskip, MODE_FWD, plan);
            self.xf(List::Rec, nskip, plan);
        }
        self.nl = nr;
        self.lcur = List::Retry;
        self.launch_qent(sess, host, plan, count, g);
        true
    }

    fn recon(&self, list: List, na: usize, out: [DevBuf; 3], osel: i32, plan: &mut Plan) {
        let l = &self.lay;
        let k = Kernel::Recon(Recon {
            g: l.geo,
            tables: l.d_tables,
            par: l.d_par,
            list: l.d_list[list as usize],
            na: na as i32,
            lev: l.d_lev,
            nzmask: l.d_nzmask,
            nz: l.d_nz,
            predbuf: l.d_pred,
            info: l.d_info,
            mbq: l.d_mbq,
            out_y: out[0],
            out_u: out[1],
            out_v: out[2],
            osel,
        });
        launch(
            plan,
            StreamId::MAIN,
            k,
            na as u64 * u64::from(self.nmb()) * 6,
            RECON_BS,
        );
    }

    /// Final bit packing, reconstruction, download (the C++ `Batch::finish_gpu`).
    pub fn finish_gpu(
        &mut self,
        opt: &EncoderOptions,
        host: &mut [u8],
        plan: &mut Plan,
        g: &mut Gold,
    ) {
        let na = self.work.len();
        if na == 0 {
            return;
        }
        g.at(plan);
        let cap = u64::from(self.cap_bytes());
        // ---- final bit packing: the pictures of all sessions one after the other in one buffer
        self.out_words = 0;
        for wk in &self.work {
            // No accepted pass is larger than the output buffer (`after_pass`: `over_cap` is
            // never accepted); the bound keeps the plan inside the buffer if that ever broke,
            // and `collect` then drops the picture instead of writing a part of it.
            let bytes = u64::from(self.pic[wk.idx].bytes).min(cap);
            let nw = (bytes.div_ceil(4) + 2) as u32;
            self.outpos[2 * wk.idx] = self.out_words;
            self.outpos[2 * wk.idx + 1] = nw;
            self.out_words = self.out_words.saturating_add(nw);
        }
        if g.on() {
            let (tick, bid, words) = (g.tick(), self.id, self.out_words);
            let pos: Vec<i64> = self
                .work
                .iter()
                .flat_map(|wk| {
                    [
                        i64::from(self.outpos[2 * wk.idx]),
                        i64::from(self.outpos[2 * wk.idx + 1]),
                    ]
                })
                .collect();
            g.put("finish", |r| {
                r.i("tick", tick)
                    .i("b", i64::from(bid))
                    .u("out_words", u64::from(words))
                    .ia("outpos", pos);
            });
        }
        let l = &self.lay;
        let (g, b) = (l.geo, l.bufs);
        let m = StreamId::MAIN;
        let out_bytes = u64::from(self.out_words) * 4;
        put(host, l.h_outpos, bytemuck::cast_slice(&self.outpos));
        plan.main(Op::Upload {
            dst: l.d_outpos,
            src: l.h_outpos,
        });
        plan.main(Op::Fill {
            dst: dhead(l.d_out, out_bytes),
            byte: 0,
        });
        let k = Kernel::EntScatter(EntScatter {
            g,
            list: l.d_list[List::Code as usize],
            na: na as i32,
            code: l.d_code,
            scratch: l.d_scratch,
            mbbits: l.d_mbbits,
            mboff: l.d_mboff,
            slice_off: l.d_slice_off,
            out: l.d_out,
            outpos: l.d_outpos,
        });
        launch(plan, m, k, na as u64 * u64::from(self.nmb()), BS);
        let mut nrec = 0;
        self.dump_k = None;
        for (k, wk) in self.work.iter().enumerate() {
            if wk.ty != PIC_B {
                self.lists[List::Rec as usize][nrec] = wk.idx as i32;
                nrec += 1;
            }
            if opt.keep_recon && wk.idx == 0 {
                self.dump_k = Some(k);
            }
        }
        let (ysz, csz) = (self.ysz(), self.csz());
        if nrec > 0 {
            self.up_list(List::Rec, nrec, m, host, plan);
            self.recon(List::Rec, nrec, [b.ref_y, b.ref_u, b.ref_v], SL_REC, plan);
            let k = Kernel::Pyr(Pyr {
                src: b.ref_y,
                sstride: ysz,
                sw: g.w,
                sh: g.h,
                d1: b.ref_y1,
                d1stride: ysz / 4,
                d2: b.ref_y2,
                d2stride: ysz / 16,
                n: g.n,
                list: l.d_list[List::Rec as usize],
                na: nrec as i32,
                par: l.d_par,
                ssel: SL_REC,
                dsel: SL_REC,
            });
            launch(plan, m, k, nrec as u64 * ysz / 64, BS);
        }
        if let (Some(k), Some(brec), Some(h_recon)) = (self.dump_k, l.d_brec, l.h_recon) {
            let src = if self.work[k].ty == PIC_B {
                // The C++ writes the list of one entry (slot 0) with a synchronous copy; here it
                // is an upload like any other. The list of I pictures is free at this point.
                self.lists[List::Intra as usize][0] = 0;
                self.up_list(List::Intra, 1, m, host, plan);
                self.recon(List::Intra, 1, brec, SL_ZERO, plan);
                brec
            } else {
                let o = u64::from(self.par[0].rec) * self.n() as u64;
                [
                    DevBuf::new(b.ref_y.off + o * ysz, ysz),
                    DevBuf::new(b.ref_u.off + o * csz, csz),
                    DevBuf::new(b.ref_v.off + o * csz, csz),
                ]
            };
            for (s, off, len) in [
                (src[0], 0, ysz),
                (src[1], ysz, csz),
                (src[2], ysz + csz, csz),
            ] {
                plan.main(Op::Download {
                    dst: HostBuf::new(h_recon.off + off, len),
                    src: dhead(s, len),
                });
            }
        }
        plan.main(Op::Download {
            dst: hhead(l.h_out, out_bytes),
            src: dhead(l.d_out, out_bytes),
        });
    }

    /// Access units and model updates on the host (the C++ `Batch::collect`).
    ///
    /// `spare`: byte vectors the caller gave back; an access unit takes one when there is one.
    pub fn collect(
        &mut self,
        sess: &mut [Session],
        host: &[u8],
        spare: &mut Vec<Vec<u8>>,
        g: &mut Gold,
    ) {
        let mbh = self.mbh();
        let cap = self.cap_bytes();
        for k in 0..self.work.len() {
            let wk = &mut self.work[k];
            let s = &mut sess[wk.si];
            let sidx = wk.idx;
            if wk.ty != wk.f.pic_type.code() {
                // I replaced by P
                s.headers(
                    &self.lay.geo,
                    &wk.f,
                    pic_type_of(wk.ty),
                    wk.gop_hdr,
                    wk.seq_hdr,
                    &mut wk.hdr,
                );
                golden::hdr(
                    g,
                    s.id,
                    &wk.f,
                    wk.ty,
                    wk.gop_hdr,
                    wk.seq_hdr,
                    s.tref_base,
                    s.rc.cbr,
                    &wk.hdr,
                    s.film(&wk.f),
                );
            }
            let pic = self.pic[sidx];
            if pic.bytes > cap {
                // A picture larger than the output buffer is never written in part (a cut
                // picture has macroblocks missing: H.262 6.1.2.2). Rate control accepts no
                // such pass but for the first picture of a stream on rung 36, which the kernels
                // cannot make that large (at most 125 bits a macroblock against the 3072 of the
                // buffer); if it ever happens the picture is given up like one whose plan
                // failed, and the first picture of a stream is tried again.
                let mut scratch = Vec::new();
                Self::fail_one(wk, s, &self.lay.geo, &mut scratch, g);
                continue;
            }
            let position_type = wk.f.pic_type.code();
            let pb = pic.bytes as usize;
            let at = self.lay.h_out.off + u64::from(self.outpos[2 * sidx]) * 4;
            let mut data = spare.pop().unwrap_or_default();
            data.reserve(wk.hdr.len() + pb + 4);
            data.extend_from_slice(&wk.hdr);
            match HostBuf::new(at, pb as u64).of(host) {
                Some(src) => data.extend_from_slice(src),
                // Outside the arena (a driver that lends the wrong memory): the picture keeps
                // its size, with stuffing in place of slices.
                None => data.resize(wk.hdr.len() + pb, 0),
            }
            let bits = 8.0 * data.len() as f64;
            let rq = &self.rowq[sidx * mbh..(sidx + 1) * mbh];
            let res =
                s.rc.finish_picture_obs(&wk.ctx, pic, bits, rq, &mut RcObs { g, s: s.id });
            let qs = s.rc.last_qs;
            let mut au = AccessUnit {
                data,
                pts: wk.f.pts,
                dts: s.dts(),
                disp: wk.f.disp,
                coded: s.n_coded,
                pic_type: wk.ty,
                keyframe: wk.key,
                avg_q: qs as f32,
                complexity: bits * qs,
                bits: bits as u64,
                rate: s.rate,
                rate_ticket: wk.ticket,
                queue_bits: res.queue_bits,
                room_bits: res.room_bits,
                cap_bits: res.cap_bits,
                rung: res.rung,
                tries: res.tries,
                intra_mbs: wk.intra,
                field_mbs: wk.nfield,
                repeat: false,
                key_missed: false,
                off_grid: wk.key && wk.f.off_grid,
                last: false,
                units: wk.f.dur,
                top_first: wk.f.tff,
                recon: Vec::new(),
            };
            let par = &self.par[sidx];
            s.last_dist_f = if wk.ty == PIC_I { 0 } else { par.dist_f };
            s.last_dist_b = if wk.ty == PIC_B { par.dist_b } else { 0 };
            if wk.ty != PIC_B {
                s.newer = 1 - (s.newer & 1);
                s.ref_disp[usize::from(s.newer)] = i64::try_from(wk.f.disp).unwrap_or(i64::MAX);
                s.have_ref = true;
                s.dup = false;
            }
            if wk.f.gop_start && wk.ty == PIC_I {
                s.need_key = false;
            }
            let (film, dur, gop_start) = (s.film(&wk.f), wk.f.dur, wk.f.gop_start);
            let is_b = wk.ty == PIC_B;
            if self.dump_k == Some(k)
                && let Some(h_recon) = self.lay.h_recon
            {
                au.recon = self.cropped(h_recon, host);
            }
            s.pictures += 1;
            s.total_bits += bits;
            if g.on() {
                golden::au(
                    g,
                    &s.au_state(film),
                    &au,
                    position_type,
                    (pic.bytes, pic.tex_bits),
                    Some(rq),
                );
            }
            s.coded(dur, gop_start, is_b);
            s.out.push_back(au);
        }
        self.recycle();
    }

    /// The reconstruction in `h_recon` cropped to the picture size, 4:2:0.
    fn cropped(&self, h_recon: HostBuf, host: &[u8]) -> Vec<u8> {
        let (w, h) = (self.fmt.w as usize, self.fmt.h as usize);
        let gw = self.lay.geo.w.unsigned_abs() as usize;
        let (ysz, csz) = (self.ysz() as usize, self.csz() as usize);
        let mut out = Vec::with_capacity(self.lay.fbytes() as usize);
        let Some(src) = h_recon.of(host) else {
            return out;
        };
        let mut rows = |base: usize, pitch: usize, width: usize, n: usize| {
            for y in 0..n {
                if let Some(r) = src.get(base + y * pitch..base + y * pitch + width) {
                    out.extend_from_slice(r);
                }
            }
        };
        rows(0, gw, w, h);
        rows(ysz, gw / 2, w / 2, h / 2);
        rows(ysz + csz, gw / 2, w / 2, h / 2);
        out
    }

    /// The plan that carried this tick's pictures failed: each becomes a repeat made on the
    /// host and its session starts again with a key frame at the next frame submitted. The
    /// first picture of a stream cannot be a repeat; it goes back to the queue.
    pub fn fail_work(&mut self, sess: &mut [Session], scratch: &mut Vec<u8>, g: &mut Gold) {
        for k in 0..self.work.len() {
            let wk = &mut self.work[k];
            let s = &mut sess[wk.si];
            Self::fail_one(wk, s, &self.lay.geo, scratch, g);
        }
        self.recycle();
        self.qdone = true;
    }

    /// One picture of [`Batch::fail_work`].
    fn fail_one(wk: &mut Work, s: &mut Session, geo: &Geo, scratch: &mut Vec<u8>, g: &mut Gold) {
        if wk.key {
            // no GOP header after all
            s.tref_base = wk.old_tref_base;
        }
        let f = std::mem::replace(&mut wk.f, blank_frame());
        if f.pic_type == PicType::I && !s.have_ref {
            if wk.ticket != 0 {
                s.pending_ticket = wk.ticket;
                s.pending_rate = s.rate;
            }
            s.gop.coded.push_front(f);
            return;
        }
        s.host_repeat(geo, &f, wk.ticket, scratch, g);
        s.gop.rekey = true;
        s.failed += 1;
    }
}

/// Ring slots of a session (`Batch::RING`) as the type of a slot number.
pub(crate) const RING_SLOTS: u64 = RING as u64;
