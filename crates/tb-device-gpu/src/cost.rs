//! What executing plans through the protocol costs on the host, against the same driver calls
//! made directly as the C++ makes them (docs/ARCHITECTURE.md §8.1). Used by the bench
//! `benches/protocol.rs`; not part of the adapter's interface.
//!
//! For each plan shape: the host time of `Device::submit` (validation, queueing through the
//! step interpreter, the completion event, bookkeeping) per step, of `Plan::validate` alone, and
//! of the same driver calls with the addresses worked out beforehand, as the C++ has them in
//! its batch fields (one call per step, kernel argument arrays built per launch as its
//! `LAUNCH` macro does). The device work of each repetition is waited for outside the timed
//! part. Medians over the repetitions.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use tb_device::kernel::{Me, Pyr, Resolved};
use tb_device::plan::{EventId, Op, Plan, StreamId};
use tb_device::{ArenaSizes, DevBuf, Device, DeviceError, HostBuf, Kernel, Launch};
use tb_kernel_abi::{Geo, SL_CUR, SL_ZERO};

use crate::runtime::{Handle, Status};
use crate::{GpuDevice, Runtime, SUCCESS, WaitMode};

/// Arena sizes the measurements need.
pub const ARENAS: ArenaSizes = ArenaSizes {
    device: 64 << 20,
    host: 64 << 20,
};

/// One plan shape.
#[derive(Clone, Debug)]
pub struct Row {
    /// What the plan does.
    pub name: &'static str,
    /// Steps in the plan.
    pub steps: usize,
    /// `Plan::validate`, ns per step.
    pub validate_ns: f64,
    /// `Device::submit` (validation included), ns per step.
    pub submit_ns: f64,
    /// The same driver calls made directly, ns per step.
    pub direct_ns: f64,
}

const K: usize = 32;

fn small_launch() -> Op {
    // Nothing listed: every thread returns at once; 14 arguments.
    Op::Launch(Launch {
        kernel: Kernel::Pyr(Pyr {
            src: DevBuf::new(0, 0),
            sstride: 0,
            sw: 16,
            sh: 4,
            d1: DevBuf::new(0, 0),
            d1stride: 0,
            d2: DevBuf::new(0, 0),
            d2stride: 0,
            n: 0,
            list: DevBuf::new(0, 0),
            na: 0,
            par: DevBuf::new(0, 0),
            ssel: SL_CUR,
            dsel: SL_ZERO,
        }),
        grid: 1,
        block: 256,
    })
}

fn large_launch() -> Op {
    // A `Geo` and a `Bufs` by value (about 300 bytes of arguments), nothing listed.
    Op::Launch(Launch {
        kernel: Kernel::Me(Me {
            g: Geo::default(),
            b: tb_device::kernel::BufsRef::default(),
            par: DevBuf::new(0, 0),
            list: DevBuf::new(0, 0),
            na: 0,
            prev: DevBuf::new(0, 0),
            out: DevBuf::new(0, 0),
        }),
        grid: 1,
        block: 64,
    })
}

fn upload(i: usize, len: u64) -> Op {
    let off = (i as u64) * len.next_multiple_of(256);
    Op::Upload {
        dst: DevBuf::new(off, len),
        src: HostBuf::new(off, len),
    }
}

/// The plan shapes measured.
pub fn plans() -> Vec<(&'static str, Plan)> {
    let mut out = Vec::new();
    let mut p = Plan::new();
    for i in 0..K {
        p.main(upload(i, 4096));
    }
    out.push(("upload 4 KiB", p));
    let mut p = Plan::new();
    for _ in 0..K {
        p.main(small_launch());
    }
    out.push(("launch, 14 scalar arguments (k_pyr)", p));
    let mut p = Plan::new();
    for _ in 0..K {
        p.main(large_launch());
    }
    out.push(("launch, Geo + Bufs by value (k_me)", p));
    let mut p = Plan::new();
    for _ in 0..K / 2 {
        p.main(Op::Record(EventId::PARAMS));
        p.push(StreamId::UPLOAD, Op::WaitFor(EventId::PARAMS));
    }
    out.push(("record + wait for event", p));
    // The shape of one batch's begin phase: clear, parameters, fence, kernels, the next
    // frames behind the fence, results.
    let mut p = Plan::new();
    p.main(Op::Fill {
        dst: DevBuf::new(32 << 20, 1 << 16),
        byte: 0,
    });
    for i in 0..4 {
        p.main(upload(i, 2048));
    }
    p.main(Op::Record(EventId::PARAMS));
    for i in 0..10 {
        p.main(if i % 2 == 0 {
            small_launch()
        } else {
            large_launch()
        });
    }
    p.push(StreamId::UPLOAD, Op::WaitFor(EventId::PARAMS));
    for i in 0..8 {
        let len = 1 << 16;
        let off = (16 << 20) + (i as u64) * len;
        p.push(
            StreamId::UPLOAD,
            Op::Upload {
                dst: DevBuf::new(off, len),
                src: HostBuf::new(off, len),
            },
        );
    }
    p.push(StreamId::UPLOAD, Op::Record(EventId::FRAMES));
    for i in 0..4 {
        let off = (48 << 20) + (i as u64) * 4096;
        p.main(Op::Download {
            dst: HostBuf::new(off, 4096),
            src: DevBuf::new(off, 4096),
        });
    }
    out.push(("tick-shaped mix (one batch, begin phase)", p));
    out
}

/// A step with its addresses worked out.
#[allow(
    clippy::large_enum_variant,
    reason = "a vector built once per measured plan; the launch is the common case"
)]
enum Raw {
    Memset(u64, u8, usize, Handle),
    HtoD(u64, *const c_void, usize, Handle),
    DtoH(*mut c_void, u64, usize, Handle),
    Launch(Handle, u32, u32, Handle, Resolved),
    Record(Handle, Handle),
    Wait(Handle, Handle),
}

fn raw<R: Runtime>(dev: &GpuDevice<R>, plan: &Plan) -> Result<Vec<Raw>, DeviceError> {
    plan.validate(dev.arenas)?;
    let mut out = Vec::with_capacity(plan.steps.len());
    for (i, s) in plan.steps.iter().enumerate() {
        let st = dev.streams[usize::from(s.stream.0)];
        out.push(match s.op {
            Op::Fill { dst, byte } => {
                Raw::Memset(dev.dev_base + dst.off, byte, dst.len as usize, st)
            }
            Op::Upload { dst, src } => Raw::HtoD(
                dev.dev_base + dst.off,
                dev.host_base.wrapping_add(src.off as usize).cast(),
                src.len as usize,
                st,
            ),
            Op::Download { dst, src } => Raw::DtoH(
                dev.host_base.wrapping_add(dst.off as usize).cast(),
                dev.dev_base + src.off,
                src.len as usize,
                st,
            ),
            Op::Launch(l) => Raw::Launch(
                dev.functions[l.kernel.index()],
                l.grid,
                l.block,
                st,
                l.kernel.resolve(dev.dev_base),
            ),
            Op::Record(e) => Raw::Record(dev.events[usize::from(e.0)], st),
            Op::WaitFor(e) => Raw::Wait(st, dev.events[usize::from(e.0)]),
            Op::Upload2d { .. } | Op::UploadRefs { .. } | Op::Copy { .. } => {
                return Err(DeviceError::BadPlan {
                    step: i,
                    what: "not measured",
                });
            }
        });
    }
    Ok(out)
}

/// Make the calls; the time they took.
fn direct<R: Runtime>(dev: &GpuDevice<R>, raws: &mut [Raw]) -> Result<Duration, DeviceError> {
    let api = &dev.api;
    let t = Instant::now();
    for (i, r) in raws.iter_mut().enumerate() {
        // SAFETY: the handles are the adapter's; the addresses were worked out from a
        // validated plan (`raw`); kernel argument pointers point into `a`, which lives across
        // the call.
        let s: Status = unsafe {
            match r {
                Raw::Memset(d, b, n, st) => api.memset_d8(*d, *b, *n, *st),
                Raw::HtoD(d, h, n, st) => api.htod(*d, *h, *n, *st),
                Raw::DtoH(h, d, n, st) => api.dtoh(*h, *d, *n, *st),
                Raw::Launch(f, g, b, st, a) => {
                    let mut p = a.pointers();
                    api.launch(*f, *g, *b, *st, p.as_mut_ptr())
                }
                Raw::Record(e, st) => api.event_record(*e, *st),
                Raw::Wait(st, e) => api.stream_wait_event(*st, *e),
            }
        };
        if s != SUCCESS {
            return Err(crate::device_error::<R>(i, s));
        }
    }
    let d = t.elapsed();
    for st in dev.streams {
        // SAFETY: a stream of the adapter.
        let s = unsafe { api.stream_sync(st) };
        if s != SUCCESS {
            return Err(crate::device_error::<R>(raws.len(), s));
        }
    }
    Ok(d)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

/// Measure every shape of [`plans`], `reps` repetitions each, alternating protocol and direct.
pub fn measure<R: Runtime>(dev: &mut GpuDevice<R>, reps: usize) -> Result<Vec<Row>, DeviceError> {
    let mut rows = Vec::new();
    for (name, plan) in plans() {
        let n = plan.steps.len() as f64;
        let mut raws = raw(dev, &plan)?;
        let (mut v, mut s, mut d) = (Vec::new(), Vec::new(), Vec::new());
        for rep in 0..reps + reps / 10 {
            let t = Instant::now();
            plan.validate(dev.arenas)?;
            let tv = t.elapsed();
            let t = Instant::now();
            let ticket = dev.submit(&plan)?;
            let ts = t.elapsed();
            dev.wait(ticket, None).status?;
            let td = direct(dev, &mut raws)?;
            if rep >= reps / 10 {
                // The first tenth warms up caches, slots and the driver's queues.
                v.push(tv.as_nanos() as f64 / n);
                s.push(ts.as_nanos() as f64 / n);
                d.push(td.as_nanos() as f64 / n);
            }
        }
        rows.push(Row {
            name,
            steps: plan.steps.len(),
            validate_ns: median(&mut v),
            submit_ns: median(&mut s),
            direct_ns: median(&mut d),
        });
    }
    Ok(rows)
}

/// One wait mode on a plan of a few milliseconds.
#[derive(Clone, Debug)]
pub struct WaitRow {
    /// The mode.
    pub mode: WaitMode,
    /// Mean time of submit + wait per plan, µs.
    pub loop_us: f64,
    /// Device time of the plan (timing events), µs, median.
    pub device_us: f64,
    /// CPU time of this thread per plan, µs.
    pub cpu_us: f64,
}

/// CPU time this thread has run, from `/proc/thread-self/schedstat` (ns).
fn thread_cpu_ns() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|t| t.parse().ok()))
        .unwrap_or(0)
}

/// Run a plan of eight `k_pyr` launches over 16 1920x1088 planes (about 2 ms on the RTX 2000)
/// `iters` times in each wait mode, alternating the modes in blocks of `iters / 4`.
pub fn measure_wait<R: Runtime>(
    dev: &mut GpuDevice<R>,
    iters: usize,
) -> Result<Vec<WaitRow>, DeviceError> {
    const N: usize = 16;
    const SW: usize = 1920;
    const SH: usize = 1088;
    let ysz = (SW * SH) as u64;
    let src = DevBuf::new(0, N as u64 * ysz);
    let d1 = DevBuf::new(src.end(), N as u64 * ysz / 4);
    let d2 = DevBuf::new(d1.end(), N as u64 * ysz / 16);
    let list = DevBuf::new(d2.end().next_multiple_of(256), (N * 4) as u64);
    let par = DevBuf::new(
        list.end().next_multiple_of(256),
        (N * size_of::<tb_kernel_abi::SessPar>()) as u64,
    );
    let hlist = HostBuf::new(0, list.len);
    for (i, c) in dev.host_mut()[..N * 4]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        c.copy_from_slice(&(i as i32).to_ne_bytes());
    }
    let mut p = Plan::new();
    p.main(Op::Upload {
        dst: list,
        src: hlist,
    });
    for _ in 0..8 {
        p.main(Op::Launch(Launch::covering(
            Kernel::Pyr(Pyr {
                src,
                sstride: ysz,
                sw: SW as i32,
                sh: SH as i32,
                d1,
                d1stride: ysz / 4,
                d2,
                d2stride: ysz / 16,
                n: N as i32,
                list,
                na: N as i32,
                par,
                ssel: SL_CUR,
                dsel: SL_ZERO,
            }),
            (N * SW * SH / 64) as u64,
            256,
        )));
    }
    let modes = [WaitMode::Blocking, WaitMode::Hybrid];
    let mut acc: [(f64, u64, Vec<f64>); 2] = std::array::from_fn(|_| (0.0, 0, Vec::new()));
    dev.set_timing(true);
    // Warm up both modes' averages.
    for m in modes {
        dev.set_wait_mode(m);
        for _ in 0..20 {
            let t = dev.submit_class(&p, 0)?;
            dev.wait(t, None).status?;
        }
    }
    let block = (iters / 4).max(1);
    for round in 0..4 {
        let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
        for k in order {
            dev.set_wait_mode(modes[k]);
            let c0 = thread_cpu_ns();
            let t0 = Instant::now();
            for _ in 0..block {
                let t = dev.submit_class(&p, 0)?;
                dev.wait(t, None).status?;
                if let Some((_, d)) = dev.last_device_time() {
                    acc[k].2.push(d.0 as f64 / 1e3);
                }
            }
            acc[k].0 += t0.elapsed().as_secs_f64();
            acc[k].1 += thread_cpu_ns().saturating_sub(c0);
        }
    }
    dev.set_timing(false);
    dev.set_wait_mode(WaitMode::Hybrid);
    let total = (block * 4) as f64;
    Ok(modes
        .iter()
        .zip(acc.iter_mut())
        .map(|(m, (wall, cpu, dt))| WaitRow {
            mode: *m,
            loop_us: *wall / total * 1e6,
            device_us: median(dt),
            cpu_us: *cpu as f64 / total / 1e3,
        })
        .collect())
}
