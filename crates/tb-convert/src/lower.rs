//! From conversion operations to device plans: [`ConvOp`]s become steps of a
//! [`tb_device::Plan`] with the kernel records of `tb_device::kernel`.
//!
//! The planner ([`crate::Converter`]) says what it needs by name ([`ConvBuf`]) and in the
//! order the C++ issues the calls. A [`Lowerer`] places those buffers in a region of the device
//! arena it is given, writes tap tables and job records into a region of the host arena, and
//! appends the uploads and launches to a plan, all on [`StreamId::MAIN`]: the conversion runs
//! on the encoder's stream, ahead of the tick's first kernels.
//!
//! Addresses are [`DevRef`]s: a destination plane is in the arena (the encoder's input
//! planes); a source plane is in the arena or in a foreign region bound to the adapter (a
//! decoder surface). Job records are uploaded in handle form ([`Op::UploadRefs`]).
//!
//! Memory rules, as the C++ has them:
//! - a buffer that is replaced by a larger one ([`ConvOp::Alloc`] with `replaces`) keeps its
//!   region until the plan in which that happened has completed: [`Lowerer::begin`], called
//!   when the previous conversion plan has completed, gives such regions back;
//! - the host region is scratch for one plan at a time: [`Lowerer::begin`] starts it afresh.

use std::collections::BTreeMap;

use tb_device::kernel::{
    ConvClassRef, ConvCopy, ConvH, ConvJobRef, ConvV, Csc as KCsc, DevRef, FillRect, Half, Kernel,
    Overlay, PreJobRef, RectRef, RefKind, TapsRef,
};
use tb_device::{ArenaAlloc, DevBuf, HostBuf, Launch, Op, Plan, StreamId};
use tb_kernel_abi::CscPar;

use crate::plan::{
    Axis, BLOCK, ClassId, ConvBuf, ConvJob, ConvLaunch, ConvOp, Converter, DevAddr, Plane, PreJob,
    RectJob,
};

impl DevAddr for DevRef {
    fn aligned4(&self) -> bool {
        self.off().is_multiple_of(4)
    }
}

/// Why operations could not be lowered. The plan may hold a part of them; the caller drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LowerError {
    /// The device region given to the conversion path has no free range of this size.
    #[error("conversion memory on the device is full ({bytes} bytes needed)")]
    DeviceFull {
        /// Bytes asked for.
        bytes: u64,
    },
    /// The host scratch region is too small for this plan's tap tables and job records.
    #[error("conversion scratch in the host arena is full ({bytes} bytes needed)")]
    HostFull {
        /// Bytes asked for.
        bytes: u64,
    },
    /// An operation names a buffer or class the planner never announced (a bug in the caller:
    /// operations of another converter, or out of order).
    #[error("conversion operation names an unknown buffer or class")]
    Unknown,
    /// A rectangle (fill, overlay, color matrix) outside the device arena: those kernels write
    /// the encoder's planes only.
    #[error("a rectangle job must lie in the device arena")]
    RectNotInArena,
}

/// Places the conversion path's buffers and writes its plan steps.
#[derive(Clone, Debug)]
pub struct Lowerer {
    dev_base: u64,
    dev: ArenaAlloc,
    host: HostBuf,
    host_used: u64,
    bufs: BTreeMap<ConvBuf, DevBuf>,
    retired: Vec<DevBuf>,
}

fn round_up(v: u64, to: u64) -> u64 {
    v.div_ceil(to) * to
}

impl Lowerer {
    /// A lowerer that owns `dev` of the device arena and `host` of the host arena.
    pub fn new(dev: DevBuf, host: HostBuf) -> Self {
        Self {
            dev_base: dev.off,
            dev: ArenaAlloc::new(dev.len),
            host,
            host_used: 0,
            bufs: BTreeMap::new(),
            retired: Vec::new(),
        }
    }

    /// Start a new plan. The previous plan lowered here must have completed: the regions of
    /// buffers it replaced are given back and the host scratch is used from its start again.
    pub fn begin(&mut self) {
        for b in self.retired.drain(..) {
            self.dev.give(b.off - self.dev_base, round_up(b.len, 64));
        }
        self.host_used = 0;
    }

    /// Bytes of the device region not in use.
    pub fn free_bytes(&self) -> u64 {
        self.dev.free_bytes()
    }

    /// Where a buffer is, once the planner has asked for it.
    pub fn buf(&self, buf: ConvBuf) -> Option<DevBuf> {
        self.bufs.get(&buf).copied()
    }

    fn scratch<'a>(
        &mut self,
        host: &'a mut [u8],
        len: u64,
    ) -> Result<(HostBuf, &'a mut [u8]), LowerError> {
        let at = round_up(self.host_used, 8);
        let full = LowerError::HostFull { bytes: len };
        let b = self.host.slice(at, len).ok_or(full)?;
        self.host_used = at + len;
        let bytes = b.of_mut(host).ok_or(full)?;
        Ok((b, bytes))
    }

    fn place(&self, p: Plane<DevRef>) -> Result<DevRef, LowerError> {
        match p {
            Plane::Ext(a) => Ok(a),
            Plane::Conv { buf, off } => {
                let b = self.bufs.get(&buf).ok_or(LowerError::Unknown)?;
                Ok(DevRef::arena(b.off + off))
            }
        }
    }

    fn class_ref(&self, conv: &Converter<DevRef>, id: ClassId) -> Result<ConvClassRef, LowerError> {
        let c = conv.class(id).ok_or(LowerError::Unknown)?;
        let taps = |axis: Axis| -> Result<TapsRef, LowerError> {
            let Some(t) = &c.taps else {
                return Ok(TapsRef::default());
            };
            let t = t.get(axis);
            let start = self.bufs.get(&ConvBuf::TapStart { class: id, axis });
            let coef = self.bufs.get(&ConvBuf::TapCoef { class: id, axis });
            match (start, coef) {
                (Some(s), Some(k)) => Ok(TapsRef {
                    start: s.off,
                    coef: k.off,
                    n: t.n as i32,
                    nt: t.nt as i32,
                }),
                _ => Err(LowerError::Unknown),
            }
        };
        Ok(ConvClassRef {
            bps: c.bps as i32,
            dw: c.dw as i32,
            th_y: c.th_y as i32,
            th_c: c.th_c as i32,
            s_step: c.s_step as i32,
            s_off: c.s_off as i32,
            d_step: c.d_step as i32,
            d_off: c.d_off as i32,
            tstride: c.tstride as i32,
            copy_w: c.copy_w as i32,
            copy_h: c.copy_h as i32,
            hy: taps(Axis::Hy)?,
            hc: taps(Axis::Hc)?,
            vy: taps(Axis::Vy)?,
            vc: taps(Axis::Vc)?,
        })
    }

    fn rect(j: &RectJob<DevRef>) -> Result<RectRef, LowerError> {
        let (w, h, dp) = (u64::from(j.w), u64::from(j.h), u64::from(j.dst.pitch));
        let arena = |r: DevRef, len: u64| -> Result<DevBuf, LowerError> {
            if r.slot().is_some() {
                return Err(LowerError::RectNotInArena);
            }
            Ok(DevBuf::new(r.off(), len))
        };
        let luma = h.saturating_sub(1) * dp + w;
        let chroma = (h / 2).saturating_sub(1) * (dp / 2) + w / 2;
        let mut r = RectRef {
            dy: arena(j.dst.y, luma)?,
            du: arena(j.dst.u, chroma)?,
            dv: arena(j.dst.v, chroma)?,
            dp: j.dst.pitch as i32,
            w: j.w as i32,
            h: j.h as i32,
            yuv: j.yuv,
            ..RectRef::default()
        };
        if let Some(o) = &j.overlay {
            r.oy = arena(o.oy, w * h)?;
            r.ou = arena(o.ou, w * h / 4)?;
            r.ov = arena(o.ov, w * h / 4)?;
            r.oa = arena(o.oa, w * h)?;
            r.oa2 = arena(o.oa2, w * h / 4)?;
        }
        Ok(r)
    }

    /// Append the steps of `ops` (operations of `conv`, in the order it issued them) to `plan`
    /// and write what they upload into the host scratch region of `host`, the host arena.
    pub fn lower(
        &mut self,
        conv: &Converter<DevRef>,
        ops: &[ConvOp<DevRef>],
        host: &mut [u8],
        plan: &mut Plan,
    ) -> Result<(), LowerError> {
        for op in ops {
            match op {
                ConvOp::Alloc {
                    buf,
                    bytes,
                    replaces,
                } => {
                    let len = round_up(*bytes, 64);
                    let off = self
                        .dev
                        .take(len)
                        .ok_or(LowerError::DeviceFull { bytes: *bytes })?;
                    let new = DevBuf::new(self.dev_base + off, *bytes);
                    if let Some(old) = self.bufs.insert(*buf, new) {
                        // The work queued before this point may still read the old region.
                        let _ = replaces;
                        self.retired.push(old);
                    }
                }
                ConvOp::UploadTaps { class, axis } => {
                    let c = conv.class(*class).ok_or(LowerError::Unknown)?;
                    let t = c.taps.as_ref().ok_or(LowerError::Unknown)?.get(*axis);
                    let key = (*class, *axis);
                    let start = self.bufs.get(&ConvBuf::TapStart {
                        class: key.0,
                        axis: key.1,
                    });
                    let coef = self.bufs.get(&ConvBuf::TapCoef {
                        class: key.0,
                        axis: key.1,
                    });
                    let (Some(&start), Some(&coef)) = (start, coef) else {
                        return Err(LowerError::Unknown);
                    };
                    let sb: &[u8] = bytemuck::cast_slice(&t.start);
                    let cb: &[u8] = bytemuck::cast_slice(&t.coef);
                    for (dst, bytes) in [(start, sb), (coef, cb)] {
                        let len = bytes.len() as u64;
                        let (src, to) = self.scratch(host, len)?;
                        to.copy_from_slice(bytes);
                        let dst = dst.slice(0, len).ok_or(LowerError::Unknown)?;
                        plan.main(Op::Upload { dst, src });
                    }
                }
                ConvOp::UploadJobs { class, jobs } => {
                    let dst = *self
                        .bufs
                        .get(&ConvBuf::Jobs(*class))
                        .ok_or(LowerError::Unknown)?;
                    let rec = size_of::<ConvJobRef>();
                    let len = (jobs.len() * rec) as u64;
                    let mut refs = Vec::with_capacity(jobs.len());
                    for j in jobs {
                        refs.push(self.job(j)?);
                    }
                    let (src, to) = self.scratch(host, len)?;
                    to.copy_from_slice(bytemuck::cast_slice(&refs));
                    let dst = dst.slice(0, len).ok_or(LowerError::Unknown)?;
                    plan.main(Op::UploadRefs {
                        dst,
                        src,
                        kind: RefKind::ConvJobs,
                    });
                }
                ConvOp::UploadPreJobs { set, block, jobs } => {
                    let dst = *self
                        .bufs
                        .get(&ConvBuf::PreJobs {
                            set: *set,
                            block: *block,
                        })
                        .ok_or(LowerError::Unknown)?;
                    let refs: Vec<PreJobRef> = jobs.iter().map(Self::pre_job).collect();
                    let len = (refs.len() * size_of::<PreJobRef>()) as u64;
                    let (src, to) = self.scratch(host, len)?;
                    to.copy_from_slice(bytemuck::cast_slice(&refs));
                    let dst = dst.slice(0, len).ok_or(LowerError::Unknown)?;
                    plan.main(Op::UploadRefs {
                        dst,
                        src,
                        kind: RefKind::PreJobs,
                    });
                }
                ConvOp::Launch(l) => {
                    let kernel = self.kernel(conv, l)?;
                    plan.push(
                        StreamId::MAIN,
                        Op::Launch(Launch {
                            kernel,
                            grid: l.grid(),
                            block: BLOCK,
                        }),
                    );
                }
            }
        }
        Ok(())
    }

    fn job(&self, j: &ConvJob<DevRef>) -> Result<ConvJobRef, LowerError> {
        Ok(ConvJobRef {
            sy: self.place(j.sy)?,
            suv: self.place(j.suv)?,
            dy: j.dy,
            du: j.du,
            dv: j.dv,
            sp_y: j.sp_y as i32,
            sp_uv: j.sp_uv as i32,
            dp: j.dp as i32,
            pad: 0,
        })
    }

    fn pre_job(j: &PreJob<DevRef>) -> PreJobRef {
        PreJobRef {
            sy: j.sy,
            suv: j.suv,
            sp_y: j.sp_y as i32,
            sp_uv: j.sp_uv as i32,
        }
    }

    fn kernel(
        &self,
        conv: &Converter<DevRef>,
        l: &ConvLaunch<DevRef>,
    ) -> Result<Kernel, LowerError> {
        let buf = |b: &ConvBuf| self.bufs.get(b).copied().ok_or(LowerError::Unknown);
        Ok(match l {
            ConvLaunch::Half {
                jobs,
                nj,
                w2,
                h2,
                bps,
                pre,
                stride,
                ..
            } => Kernel::Half(Half {
                jobs: buf(jobs)?,
                nj: *nj as i32,
                w2: *w2 as i32,
                h2: *h2 as i32,
                bps: *bps as i32,
                pre: buf(pre)?,
                stride: *stride,
            }),
            ConvLaunch::ConvCopy {
                class, jobs, nj, ..
            } => Kernel::ConvCopy(ConvCopy {
                c: self.class_ref(conv, *class)?,
                jobs: buf(jobs)?,
                nj: *nj as i32,
            }),
            ConvLaunch::ConvH {
                class,
                jobs,
                nj,
                tmp,
                ..
            } => Kernel::ConvH(ConvH {
                c: self.class_ref(conv, *class)?,
                jobs: buf(jobs)?,
                nj: *nj as i32,
                tmp: buf(tmp)?,
            }),
            ConvLaunch::ConvV {
                class,
                jobs,
                nj,
                tmp,
                ..
            } => Kernel::ConvV(ConvV {
                c: self.class_ref(conv, *class)?,
                jobs: buf(jobs)?,
                nj: *nj as i32,
                tmp: buf(tmp)?,
            }),
            ConvLaunch::Fill { j, .. } => Kernel::FillRect(FillRect { j: Self::rect(j)? }),
            ConvLaunch::Overlay { j, .. } => Kernel::Overlay(Overlay { j: Self::rect(j)? }),
            ConvLaunch::Csc {
                j, m, interlaced, ..
            } => Kernel::Csc(KCsc {
                j: Self::rect(j)?,
                m: CscPar {
                    m: m.m,
                    off: m.off,
                    interlaced: i32::from(*interlaced),
                },
            }),
        })
    }
}
