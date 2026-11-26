//! Every fault-injection mode of the worker is detected and classified, the parent keeps
//! running, and the restart policy acts; from inside the worker, the sandbox denies what it
//! must. These need no `ffmpeg` (faults trigger on the packet count; packets are junk).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::{Duration, Instant};

use common::*;
use tb_decoder_client::proto::supervise::{ExitClass, KillReason, RestartPolicy};
use tb_decoder_client::proto::wire::{Fault, ProbeOp, ProbeStage, SeccompMode};
use tb_decoder_client::{Decoder, Event};
use tb_media::{CodecId, InputEvent};

fn faulty(fault: Fault) -> tb_decoder_client::PoolConfig {
    let mut c = config();
    c.fault = fault;
    c.watchdog.heartbeat_interval_ms = 100;
    c.watchdog.heartbeat_timeout_ms = 1_000;
    c.watchdog.stall_ms = 1_000;
    c.restart = RestartPolicy {
        initial_backoff_ms: 50,
        ..RestartPolicy::default()
    };
    c
}

/// Feeds junk H.264 packets (a key frame every 10) until an event matches.
fn feed_until(d: &mut Decoder, timeout: Duration, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let end = Instant::now() + timeout;
    let mut evs = Vec::new();
    let _ = d.try_send(InputEvent::Streams(vec![video_stream(0, CodecId::H264)]));
    let mut i = 0i64;
    loop {
        let _ = d.try_send(packet(
            0,
            i * 3000,
            3000,
            i % 10 == 0,
            vec![0, 0, 0, 1, 9, 0xf0, 1, 2, 3],
        ));
        i += 1;
        let _ = d.poll(Some(Instant::now() + Duration::from_millis(5)));
        let new: Vec<Event> = d.events().collect();
        let hit = new.iter().any(&pred);
        evs.extend(new);
        if hit {
            return evs;
        }
        assert!(Instant::now() < end, "event not seen: {}", brief(&evs));
    }
}

fn brief(evs: &[Event]) -> String {
    evs.iter()
        .map(|e| match e {
            Event::Started { generation, .. } => format!("Started({generation})"),
            e => format!("{e:?}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn exited(evs: &[Event]) -> Vec<ExitClass> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Exited { class, .. } => Some(*class),
            _ => None,
        })
        .collect()
}

#[test]
fn crash_is_classified_and_the_worker_restarts() {
    let mut d = spawn(faulty(Fault::CrashAfter(5)));
    let evs = feed_until(&mut d, Duration::from_secs(10), |e| {
        matches!(e, Event::Started { generation: 2, .. })
    });
    eprintln!("{}", brief(&evs));
    assert_eq!(exited(&evs)[0], ExitClass::Crash(libc::SIGSEGV));
    assert!(evs.iter().any(|e| matches!(e, Event::Restarting { .. })));
    assert_eq!(d.stats().supervisor.crash, 1);
}

#[test]
fn hang_is_killed_by_the_watchdog() {
    let mut d = spawn(faulty(Fault::HangAfter(3)));
    let evs = feed_until(&mut d, Duration::from_secs(10), |e| {
        matches!(e, Event::Exited { .. })
    });
    eprintln!("{}", brief(&evs));
    let c = exited(&evs)[0];
    assert!(
        matches!(
            c,
            ExitClass::Killed(KillReason::Heartbeat | KillReason::Stall)
        ),
        "{c:?}"
    );
}

#[test]
fn spin_is_killed_by_the_watchdog() {
    let mut d = spawn(faulty(Fault::SpinAfter(3)));
    let evs = feed_until(&mut d, Duration::from_secs(10), |e| {
        matches!(e, Event::Exited { .. })
    });
    eprintln!("{}", brief(&evs));
    assert!(matches!(
        exited(&evs)[0],
        ExitClass::Killed(KillReason::Heartbeat | KillReason::Stall)
    ));
}

#[test]
fn a_lying_worker_is_killed() {
    let mut d = spawn(faulty(Fault::BadDescriptorAfter(3)));
    let evs = feed_until(&mut d, Duration::from_secs(10), |e| {
        matches!(e, Event::Exited { .. })
    });
    eprintln!("{}", brief(&evs));
    assert!(evs.iter().any(|e| matches!(e, Event::Violation { .. })));
    assert_eq!(exited(&evs)[0], ExitClass::Killed(KillReason::Violation));
    assert_eq!(d.stats().violations, 1);
}

#[test]
fn unbounded_allocation_ends_in_the_address_space_limit() {
    let mut c = faulty(Fault::AllocAfter(3));
    c.policy.rlimits.address_space = Some(1 << 30);
    let mut d = spawn(c);
    let evs = feed_until(&mut d, Duration::from_secs(20), |e| {
        matches!(e, Event::Exited { .. })
    });
    eprintln!("{}", brief(&evs));
    assert_eq!(exited(&evs)[0], ExitClass::OutOfMemory);
}

#[test]
fn repeated_crashes_open_the_circuit() {
    let mut c = faulty(Fault::CrashAfter(2));
    c.restart.breaker_failures = 3;
    c.restart.breaker_cooldown_ms = 60_000;
    let mut d = spawn(c);
    let evs = feed_until(&mut d, Duration::from_secs(20), |e| {
        matches!(e, Event::CircuitOpen { .. })
    });
    eprintln!("{}", brief(&evs));
    assert_eq!(exited(&evs).len(), 3);
    // the parent goes on: packets are refused, not blocked on
    assert_eq!(
        d.try_send(packet(0, 0, 3000, true, vec![1, 2, 3])),
        Err(tb_decoder_client::SendError::Unavailable)
    );
    assert_eq!(d.stats().supervisor.circuit_opens, 1);
}

fn probe(mode: SeccompMode) -> (Vec<Event>, Decoder) {
    let mut c = faulty(Fault::Probe);
    c.seccomp = mode;
    let mut d = DecoderPoolSpawn::spawn(c);
    let evs = wait_event(&mut d, Duration::from_secs(10), |e| match mode {
        SeccompMode::Kill => matches!(e, Event::Exited { .. }),
        _ => matches!(
            e,
            Event::Probe {
                stage: ProbeStage::AfterSeccomp,
                ..
            }
        ),
    });
    (evs, d)
}

struct DecoderPoolSpawn;
impl DecoderPoolSpawn {
    fn spawn(c: tb_decoder_client::PoolConfig) -> Decoder {
        tb_decoder_client::DecoderPool::new(c)
            .unwrap()
            .spawn("probe")
            .unwrap()
    }
}

fn results(evs: &[Event], stage: ProbeStage) -> Vec<(ProbeOp, i32)> {
    evs.iter()
        .find_map(|e| match e {
            Event::Probe { stage: s, results } if *s == stage => {
                Some(results.iter().map(|r| (r.op, r.errno)).collect())
            }
            _ => None,
        })
        .unwrap()
}

#[test]
fn inside_the_worker_the_sandbox_denies() {
    let (evs, d) = probe(SeccompMode::Errno);
    let conf = d.confinement().cloned();
    eprintln!("confinement: {conf:?}");
    let before = results(&evs, ProbeStage::BeforeSeccomp);
    let after = results(&evs, ProbeStage::AfterSeccomp);
    eprintln!("before seccomp: {before:?}\nafter seccomp: {after:?}");
    for (op, e) in &after {
        assert_ne!(*e, 0, "{op:?} succeeded inside the worker");
    }
    let get = |r: &[(ProbeOp, i32)], op| r.iter().find(|(o, _)| *o == op).unwrap().1;
    assert_eq!(get(&after, ProbeOp::Socket), libc::EPERM);
    assert_eq!(get(&after, ProbeOp::Exec), libc::EPERM);
    // the file system denies on its own, before the filter (mount namespace or Landlock)
    assert_ne!(get(&before, ProbeOp::ReadPasswd), 0);
    assert_ne!(get(&before, ProbeOp::WriteFile), 0);
}

#[test]
fn a_forbidden_call_in_kill_mode_is_a_seccomp_violation() {
    let (evs, _d) = probe(SeccompMode::Kill);
    eprintln!("{}", brief(&evs));
    assert_eq!(exited(&evs)[0], ExitClass::SeccompViolation);
}
