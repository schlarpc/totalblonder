//! The CUDA adapter on a real card. Needs a GPU and its driver, so `nix flake check` does not
//! run these tests; run them where the card is:
//!
//! ```sh
//! nix run .#gpu-plumbing
//! ```
//!
//! The tests share the device's primary context (one process, and the test harness runs tests
//! on several threads), and some reset it, so each takes `CARD` first. The test that faults on
//! the device runs its body in a child process, because the fault poisons the process.
#![allow(
    clippy::print_stdout,
    reason = "digests, timings and the statuses seen are the tests' report"
)]
#![allow(clippy::unwrap_used, reason = "a test file: the helpers fail the test")]

use std::sync::{Mutex, MutexGuard};

use tb_device::kernel::{BufsRef, Me, Pyr};
use tb_device::plan::{EventId, Op, Plan, StreamId};
use tb_device::sim::{Recorder, Replay};
use tb_device::{
    ArenaSizes, DevBuf, Device, DeviceError, HostBuf, Kernel, Launch, Nanos, selftest,
};
use tb_device_cuda::{
    CUDA_ERROR_ILLEGAL_ADDRESS, CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES, CudaDevice, CudaError, IMAGE,
    WaitMode,
};
use tb_kernel_abi::{Geo, SL_CUR, SL_ZERO};

mod float_parity;

static CARD: Mutex<()> = Mutex::new(());

fn card() -> MutexGuard<'static, ()> {
    CARD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Large enough for the self-test's layout and a slow upload beside it.
const BIG: ArenaSizes = ArenaSizes {
    device: 160 << 20,
    host: 160 << 20,
};
/// A 64 MiB copy (about 5 ms over the bus) at 1 MiB, clear of the self-test's buffers.
const SLOW_DST: DevBuf = DevBuf::new(1 << 20, 64 << 20);
const SLOW_SRC: HostBuf = HostBuf::new(1 << 20, 64 << 20);

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap()
}

fn digest_of(dev: &CudaDevice, d1: HostBuf, d2: HostBuf) -> u64 {
    let out = selftest::PyrOutput {
        d1: d1.of(dev.host()).unwrap().to_vec(),
        d2: d2.of(dev.host()).unwrap().to_vec(),
        completion: tb_device::Completion {
            ticket: tb_device::Ticket(0),
            status: Ok(()),
            submitted: Nanos(0),
            finished: Nanos(0),
        },
    };
    selftest::digest(&out)
}

/// The self-test's download targets, read from its plan.
fn outputs(plan: &Plan) -> (HostBuf, HostBuf) {
    let d: Vec<HostBuf> = plan
        .steps
        .iter()
        .filter_map(|s| match s.op {
            Op::Download { dst, .. } => Some(dst),
            _ => None,
        })
        .collect();
    (d[0], d[1])
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn pyramid_matches_the_definition_on_the_card() {
    let _card = card();
    let mut dev = CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap();
    let out = selftest::run(&mut dev).unwrap();
    let (d1, d2) = selftest::expected(&selftest::source());
    assert_eq!(out.d1, d1);
    assert_eq!(out.d2, d2);
    assert_eq!(selftest::digest(&out), selftest::EXPECTED_DIGEST);
    println!(
        "k_pyr on {}: digest {:016x}, {} ns from submit to completion",
        dev.name(),
        selftest::digest(&out),
        out.completion.finished.0 - out.completion.submitted.0
    );
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn recorded_run_on_the_card_replays() {
    let _card = card();
    let mut rec = Recorder::new(CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap());
    let live = selftest::run(&mut rec).unwrap();
    let (trace, _) = rec.finish();
    let again = selftest::run(&mut Replay::new(trace)).unwrap();
    assert_eq!((again.d1, again.d2), (live.d1, live.d2));
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn repeated_plans_and_a_timed_wait() {
    let _card = card();
    let mut dev = CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap();
    selftest::fill_host(dev.host_mut()).unwrap();
    for mode in [WaitMode::Blocking, WaitMode::Hybrid] {
        dev.set_wait_mode(mode);
        for _ in 0..100 {
            let t = dev.submit(&selftest::plan()).unwrap();
            let c = dev.wait(t, Some(Nanos(2_000_000_000)));
            assert_eq!(c.status, Ok(()));
            assert_eq!(dev.wait(t, None).status, Err(DeviceError::UnknownTicket));
        }
    }
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn upload2d_lands_at_its_pitch() {
    let _card = card();
    let mut dev = CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap();
    let (width, rows, src_pitch, dst_pitch) = (90u32, 7u32, 100u32, 128u32);
    let src = HostBuf::new(0, u64::from((rows - 1) * src_pitch + width));
    let dst = DevBuf::new(1024, u64::from((rows - 1) * dst_pitch + width));
    let back = HostBuf::new(4096, dst.len);
    for (i, b) in src.of_mut(dev.host_mut()).unwrap().iter_mut().enumerate() {
        *b = (i * 7 + 3) as u8;
    }
    let mut p = Plan::new();
    p.main(Op::Fill { dst, byte: 0xee });
    p.main(Op::Upload2d {
        dst,
        dst_pitch,
        src,
        src_pitch,
        width,
        rows,
    });
    p.main(Op::Download {
        dst: back,
        src: dst,
    });
    let t = dev.submit(&p).unwrap();
    assert_eq!(dev.wait(t, None).status, Ok(()));
    let got = back.of(dev.host()).unwrap();
    let s = src.of(dev.host()).unwrap();
    for (i, b) in got.iter().enumerate() {
        let (r, x) = (i / dst_pitch as usize, i % dst_pitch as usize);
        let want = if x < width as usize {
            s[r * src_pitch as usize + x]
        } else {
            0xee
        };
        assert_eq!(*b, want, "byte {i} (row {r}, column {x})");
    }
}

/// The self-test's plan with a slow copy queued on the upload stream ahead of the frames, so
/// that the frames arrive milliseconds after the kernel would have run without its event.
fn fenced_plan(with_wait: bool) -> Plan {
    let mut p = Plan::new();
    for s in selftest::plan().steps {
        if s.op == Op::WaitFor(EventId::FRAMES) && !with_wait {
            continue;
        }
        p.steps.push(s);
        if s.op == Op::WaitFor(EventId::PARAMS) {
            p.push(
                StreamId::UPLOAD,
                Op::Upload {
                    dst: SLOW_DST,
                    src: SLOW_SRC,
                },
            );
        }
    }
    p
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn events_order_streams_in_one_plan_and_across_plans() {
    let _card = card();
    let mut dev = CudaDevice::new(0, BIG, &IMAGE).unwrap();
    selftest::fill_host(dev.host_mut()).unwrap();
    let (h1, h2) = outputs(&selftest::plan());

    // One plan: the kernel waits for FRAMES, recorded by the upload stream of the same plan.
    let t = dev.submit(&fenced_plan(true)).unwrap();
    assert_eq!(dev.wait(t, None).status, Ok(()));
    assert_eq!(digest_of(&dev, h1, h2), selftest::EXPECTED_DIGEST);

    // Two plans: the first queues parameters and frames and waits on the upload stream; the
    // second's kernel waits for FRAMES recorded by the first. The second is waited for first.
    let full = fenced_plan(true);
    let cut = full
        .steps
        .iter()
        .position(|s| s.op == Op::WaitFor(EventId::FRAMES))
        .unwrap();
    let a = Plan {
        steps: full.steps[..cut].to_vec(),
        wait: StreamId::UPLOAD,
    };
    let b = Plan {
        steps: full.steps[cut..].to_vec(),
        wait: StreamId::MAIN,
    };
    for o in [h1, h2] {
        o.of_mut(dev.host_mut()).unwrap().fill(0);
    }
    let ta = dev.submit(&a).unwrap();
    let tb = dev.submit(&b).unwrap();
    assert_eq!(dev.wait(tb, None).status, Ok(()));
    assert_eq!(dev.wait(ta, None).status, Ok(()));
    assert_eq!(digest_of(&dev, h1, h2), selftest::EXPECTED_DIGEST);

    // Control: without the wait the kernel reads the frames before they arrive.
    dev.restore().unwrap(); // a clean device arena
    selftest::fill_host(dev.host_mut()).unwrap();
    let t = dev.submit(&fenced_plan(false)).unwrap();
    assert_eq!(dev.wait(t, None).status, Ok(()));
    let unfenced = digest_of(&dev, h1, h2);
    println!(
        "without WaitFor(FRAMES) the digest is {unfenced:016x} ({})",
        if unfenced == selftest::EXPECTED_DIGEST {
            "the same: the copy happened to finish first"
        } else {
            "different: the event is what orders the kernel after the copy"
        }
    );
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn plans_in_flight_complete_in_any_order() {
    let _card = card();
    let mut dev = CudaDevice::new(0, BIG, &IMAGE).unwrap();
    let mut slow = Plan::new();
    slow.push(
        StreamId::UPLOAD,
        Op::Upload {
            dst: SLOW_DST,
            src: SLOW_SRC,
        },
    );
    slow.wait = StreamId::UPLOAD;
    // A quick plan on MAIN only, independent of the upload stream.
    let quick_dst = HostBuf::new(0, 4096);
    let mut quick = Plan::new();
    quick.main(Op::Fill {
        dst: DevBuf::new(0, 4096),
        byte: 0x5a,
    });
    quick.main(Op::Download {
        dst: quick_dst,
        src: DevBuf::new(0, 4096),
    });
    for _ in 0..10 {
        quick_dst.of_mut(dev.host_mut()).unwrap().fill(0);
        let ts = dev.submit(&slow).unwrap();
        let tq = dev.submit(&quick).unwrap();
        // The quick plan completes while the slow copy is still running.
        assert_eq!(dev.wait(tq, None).status, Ok(()));
        assert!(quick_dst.of(dev.host()).unwrap().iter().all(|b| *b == 0x5a));
        let early = dev.wait(ts, Some(Nanos(0)));
        assert_eq!(early.status, Err(DeviceError::Timeout));
        let c = dev.wait(ts, None);
        assert_eq!(c.status, Ok(()));
        assert!(c.finished.0 - c.submitted.0 > 1_000_000, "{c:?}");
    }
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn timing_events_report_device_time() {
    let _card = card();
    let mut dev = CudaDevice::new(0, BIG, &IMAGE).unwrap();
    dev.set_timing(true);
    let p = fenced_plan(true);
    let t = dev.submit(&p).unwrap();
    let c = dev.wait(t, None);
    assert_eq!(c.status, Ok(()));
    let (tt, d) = dev.last_device_time().unwrap();
    assert_eq!(tt, t);
    // The plan's wait stream (MAIN) waits for the 64 MiB copy: milliseconds on the device,
    // within the host's view of the plan.
    assert!(
        d.0 > 1_000_000 && d.0 <= c.finished.0 - c.submitted.0 + 1_000_000,
        "{d:?} {c:?}"
    );
    println!(
        "device time {} µs, host {} µs",
        d.0 / 1000,
        (c.finished.0 - c.submitted.0) / 1000
    );
}

fn me_launch(block: u32) -> Op {
    let d = DevBuf::new(0, 0);
    Op::Launch(Launch {
        kernel: Kernel::Me(Me {
            g: Geo::default(),
            b: BufsRef::default(),
            par: d,
            list: d,
            na: 0,
            prev: d,
            out: d,
        }),
        grid: 1,
        block,
    })
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn launch_out_of_resources_fails_the_plan_only() {
    let _card = card();
    let mut dev = CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap();
    let mut p = Plan::new();
    p.main(Op::Fill {
        dst: DevBuf::new(0, 4096),
        byte: 1,
    });
    // k_me uses about 168 registers per thread: 1024 threads need more than a multiprocessor
    // has. cuLaunchKernel refuses it.
    p.main(me_launch(1024));
    p.main(Op::Download {
        dst: HostBuf::new(0, 4096),
        src: DevBuf::new(0, 4096),
    });
    let t = dev.submit(&p).unwrap();
    let c = dev.wait(t, None);
    println!("k_me with 1024 threads per block: {:?}", c.status);
    assert_eq!(
        c.status,
        Err(DeviceError::Failed {
            step: 1,
            code: CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES
        })
    );
    assert!(!dev.is_lost());
    // The same kernel at a size it can run, and the self-test, work on the same context.
    let mut ok = Plan::new();
    ok.main(me_launch(64));
    let t = dev.submit(&ok).unwrap();
    assert_eq!(dev.wait(t, None).status, Ok(()));
    let out = selftest::run(&mut dev).unwrap();
    assert_eq!(selftest::digest(&out), selftest::EXPECTED_DIGEST);
}

/// A plan whose kernel reads far outside the device arena: `k_pyr` with a session list naming
/// session 2^30, so that it reads its parameters about 2^36 bytes past the arena.
fn faulting_plan(dev: &mut CudaDevice) -> Plan {
    let list = HostBuf::new(0, 4);
    list.of_mut(dev.host_mut())
        .unwrap()
        .copy_from_slice(&(1i32 << 30).to_ne_bytes());
    let dlist = DevBuf::new(0, 4);
    let mut p = Plan::new();
    p.main(Op::Upload {
        dst: dlist,
        src: list,
    });
    p.main(Op::Launch(Launch {
        kernel: Kernel::Pyr(Pyr {
            src: DevBuf::new(4096, 2048),
            sstride: 2048,
            sw: 64,
            sh: 32,
            d1: DevBuf::new(8192, 512),
            d1stride: 512,
            d2: DevBuf::new(9216, 128),
            d2stride: 128,
            n: 1,
            list: dlist,
            na: 1,
            par: DevBuf::new(256, 64),
            ssel: SL_CUR,
            dsel: SL_ZERO,
        }),
        grid: 1,
        block: 32,
    }));
    p
}

/// Run test `name` of this binary in a child process: for a test that poisons the process.
/// The child sees `TB_IN_CHILD` and runs the body; the parent checks that it passed.
fn in_child(name: &str) -> bool {
    if std::env::var_os("TB_IN_CHILD").is_some() {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            name,
            "--exact",
            "--include-ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("TB_IN_CHILD", "1")
        .output()
        .unwrap();
    print!("{}", String::from_utf8_lossy(&out.stdout));
    print!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "child process failed: {:?}",
        out.status
    );
    false
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn device_fault_is_lost_and_poisons_the_process() {
    let _card = card();
    if !in_child("device_fault_is_lost_and_poisons_the_process") {
        return;
    }
    let mut dev = CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap();
    selftest::fill_host(dev.host_mut()).unwrap();
    // A plan in flight beside the faulting one also ends lost.
    let other = dev.submit(&selftest::plan()).unwrap();
    let p = faulting_plan(&mut dev);
    let t = dev.submit(&p).unwrap();
    let c = dev.wait(t, None);
    println!(
        "kernel reading outside the arena: submit Ok, wait {:?}",
        c.status
    );
    let Err(DeviceError::Lost { code }) = c.status else {
        panic!("expected Lost, got {:?}", c.status);
    };
    assert_eq!(code, CUDA_ERROR_ILLEGAL_ADDRESS);
    assert!(dev.is_lost());
    let o = dev.wait(other, None).status;
    assert!(matches!(o, Ok(()) | Err(DeviceError::Lost { .. })), "{o:?}");
    // Nothing more is accepted.
    assert_eq!(
        dev.submit(&selftest::plan()),
        Err(DeviceError::Lost { code })
    );
    // The driver will not make a context in this process again.
    for _ in 0..2 {
        let r = dev.restore();
        println!("restore: {r:?}");
        assert_eq!(r, Err(CudaError::ProcessPoisoned { status: code }));
        assert!(dev.is_lost());
        assert!(dev.host().is_empty());
        assert_eq!(
            dev.submit(&selftest::plan()),
            Err(DeviceError::Lost {
                code: tb_device_cuda::CUDA_ERROR_CONTEXT_IS_DESTROYED
            })
        );
    }
    drop(dev);
    let again = CudaDevice::new(0, selftest::ARENAS, &IMAGE);
    println!(
        "a new adapter in the same process: {:?}",
        again.as_ref().err()
    );
    assert!(matches!(again, Err(CudaError::ProcessPoisoned { .. })));
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn restore_does_not_leak_the_pinned_arena() {
    let _card = card();
    let arenas = ArenaSizes {
        device: 64 << 20,
        host: 128 << 20,
    };
    let mut dev = CudaDevice::new(0, arenas, &IMAGE).unwrap();
    dev.restore().unwrap();
    let before = rss_kib();
    for _ in 0..20 {
        // With a plan in flight: restore drains it and forgets its ticket.
        selftest::fill_host(dev.host_mut()).unwrap();
        let t = dev.submit(&selftest::plan()).unwrap();
        dev.restore().unwrap();
        assert_eq!(dev.wait(t, None).status, Err(DeviceError::UnknownTicket));
        let out = selftest::run(&mut dev).unwrap();
        assert_eq!(selftest::digest(&out), selftest::EXPECTED_DIGEST);
    }
    let after = rss_kib();
    println!("20 restores of a 128 MiB pinned arena: RSS {before} -> {after} KiB");
    // One leaked arena would be 131072 KiB.
    assert!(after < before + 32 * 1024, "{before} -> {after} KiB");
}

#[test]
#[ignore = "needs a CUDA device and its driver"]
fn ten_thousand_plans_without_growth() {
    let _card = card();
    let mut dev = CudaDevice::new(0, selftest::ARENAS, &IMAGE).unwrap();
    selftest::fill_host(dev.host_mut()).unwrap();
    let plan = selftest::plan();
    let (h1, h2) = outputs(&plan);
    let run = |dev: &mut CudaDevice, n: usize, class: Option<u8>| {
        for _ in 0..n {
            let t = match class {
                Some(c) => dev.submit_class(&plan, c),
                None => dev.submit(&plan),
            }
            .unwrap();
            assert_eq!(dev.wait(t, None).status, Ok(()));
        }
    };
    run(&mut dev, 500, None);
    run(&mut dev, 500, Some(1));
    let before = rss_kib();
    let t0 = std::time::Instant::now();
    run(&mut dev, 5_000, None);
    run(&mut dev, 5_000, Some(1));
    let el = t0.elapsed();
    let after = rss_kib();
    assert_eq!(digest_of(&dev, h1, h2), selftest::EXPECTED_DIGEST);
    println!(
        "10000 plans in {:.2} s ({:.1} µs each): RSS {before} -> {after} KiB",
        el.as_secs_f64(),
        el.as_secs_f64() * 1e6 / 10_000.0
    );
    assert!(after <= before + 256, "{before} -> {after} KiB");
}
