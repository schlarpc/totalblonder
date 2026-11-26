//! Work package E4: the paths the golden vectors do not reach, on the model device of
//! `common::model` with the simulator's faults. Everything here is verified against the text
//! of the C++ (and the error model of docs/ARCHITECTURE.md §4.5), not against the golden
//! records: mixed formats in one tick, sessions opened, closed and reconfigured while others
//! run, arena exhaustion, ladder rung 37 and the I picture replaced by a P picture, frames
//! written on the device, the overlapped upload, the deadline policy with a real deadline, a
//! plan that fails in each phase, a lost device, restore in a new encoder.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use std::collections::BTreeMap;

use common::model::Model;
use tb_device::sim::Fault;
use tb_device::{
    ArenaSizes, Device, DeviceError, EventId, HostBuf, Nanos, Op, Plan, StreamId, Ticket,
};
use tb_kernel_abi::{PIC_B, PIC_I, PIC_P, SessPar};
use tb_m2v_encoder::layout::BatchLayout;
use tb_m2v_encoder::tick::CLASS_QUANT_RETRY;
use tb_m2v_encoder::{
    AccessUnit, Encoder, EncoderError, EncoderOptions, FrameSource, Phase, SessionConfig,
    SessionId, Step, TickStats,
};
use tb_m2v_syntax::profile::ProfileError;

const ARENAS: ArenaSizes = ArenaSizes {
    device: 8 << 20,
    host: 1 << 20,
};

/// 64x48: the coded size is the picture size (one upload per frame, no padding).
fn f1() -> SessionConfig {
    SessionConfig {
        w: 64,
        h: 48,
        gop: 6,
        m: 3,
        max_bitrate: 400_000,
        vbv_bits: 196_608,
        bitrate: 400_000,
        capacity: 4,
        profile: tb_m2v_syntax::profile::Profile::H262,
        ..SessionConfig::default()
    }
}

/// 72x40 in 80x48: padded in both directions (2-D uploads, padding kernels).
fn f2() -> SessionConfig {
    SessionConfig {
        w: 72,
        h: 40,
        capacity: 2,
        ..f1()
    }
}

/// What one tick did.
#[derive(Clone, Debug, Default)]
struct TickLog {
    aus: Vec<(u32, AccessUnit)>,
    phases: Vec<Phase>,
    classes: Vec<u8>,
    plans: Vec<Plan>,
    /// Plans queued ahead of a phase's last plan (split plans).
    queued: Vec<Plan>,
    /// Every step submitted, in order.
    order: Vec<tb_device::plan::Step>,
    errors: Vec<DeviceError>,
    stats: TickStats,
    waits: usize,
    start: Nanos,
    end: Nanos,
}

/// A fault for the next plan of a phase.
#[derive(Clone, Debug)]
enum Trouble {
    Fail(DeviceError),
    Delay(u64),
}

struct Rig {
    enc: Encoder,
    dev: Model,
    plan: Plan,
    /// Frames submitted per session.
    n: BTreeMap<u32, u64>,
    ticks: Vec<TickLog>,
    /// Paced: tick t starts at `t * period` at the earliest and is due at `(t + 1) * period`.
    period: Option<u64>,
    /// Faults for the `nth` plan of a phase of the next tick.
    trouble: Vec<(Phase, usize, Trouble)>,
    sessions: Vec<SessionId>,
}

/// The content of frame `n` of a stream with this seed (the model's "first luma sample").
fn content(seed: u64, n: u64) -> u8 {
    20 + ((n * 7 + seed * 13) % 60) as u8
}

impl Rig {
    fn new(opt: EncoderOptions) -> Self {
        Self::with(opt, ARENAS)
    }

    fn with(opt: EncoderOptions, arenas: ArenaSizes) -> Self {
        Self {
            enc: Encoder::new(opt, arenas),
            dev: Model::new(arenas),
            plan: Plan::new(),
            n: BTreeMap::new(),
            ticks: Vec::new(),
            period: None,
            trouble: Vec::new(),
            sessions: Vec::new(),
        }
    }

    fn open(&mut self, c: &SessionConfig) -> SessionId {
        let s = self.enc.open(c).unwrap();
        self.sessions.push(s);
        s
    }

    /// Submit the next frame of a session from host memory, filled with `c`.
    fn feed_c(&mut self, s: SessionId, c: u8) {
        let buf = self.enc.frame_buffer(s).unwrap();
        buf.of_mut(self.dev.host_mut()).unwrap().fill(c);
        let n = self.n.entry(s.0).or_default();
        self.enc
            .submit(s, FrameSource::Host, *n as i64 * 3003, &[])
            .unwrap();
        *n += 1;
    }

    fn feed(&mut self, s: SessionId, seed: u64) {
        let n = self.n.get(&s.0).copied().unwrap_or(0);
        self.feed_c(s, content(seed, n));
    }

    fn run_plan(&mut self, mut step: Step, log: &mut TickLog) {
        let mut ticket = Ticket(u64::MAX);
        let mut seen: BTreeMap<u8, usize> = BTreeMap::new();
        loop {
            match step {
                Step::Done => return,
                Step::Run => {
                    let phase = self.enc.phase().unwrap();
                    let k = seen.entry(phase.class()).or_default();
                    for (_, _, t) in self
                        .trouble
                        .extract_if(.., |(p, nth, _)| *p == phase && *nth == *k)
                    {
                        let submit = self.dev.submits;
                        self.dev.faults.push(match t {
                            Trouble::Fail(error) => Fault::Fail { submit, error },
                            Trouble::Delay(ns) => Fault::Delay {
                                submit,
                                extra: Nanos(ns),
                            },
                        });
                    }
                    *k += 1;
                    log.phases.push(phase);
                    log.classes.push(self.enc.wait_class().unwrap());
                    log.plans.push(self.plan.clone());
                    log.order.extend(self.plan.steps.iter().copied());
                    ticket = self.dev.submit(&self.plan).expect("a valid plan");
                }
                Step::Queue => {
                    // a part of the phase: queued, not waited for before the next is written
                    log.queued.push(self.plan.clone());
                    log.order.extend(self.plan.steps.iter().copied());
                    let t = self.dev.submit(&self.plan).expect("a valid plan");
                    step = self.enc.queued(self.dev.host_mut(), &mut self.plan);
                    assert_eq!(self.dev.wait(t, None).status, Ok(()));
                    continue;
                }
                Step::Wait => log.waits += 1,
            }
            let done = self.dev.wait(ticket, Some(Nanos(2_000_000)));
            if let Err(e) = &done.status
                && *e != DeviceError::Timeout
            {
                log.errors.push(e.clone());
            }
            let now = self.dev.now();
            step = self
                .enc
                .resume(&done, now, self.dev.host_mut(), &mut self.plan);
        }
    }

    fn settle(&mut self) -> TickLog {
        let mut log = TickLog::default();
        let streams: &[StreamId] = if self.enc.options().overlap {
            &[StreamId::UPLOAD, StreamId::MAIN]
        } else {
            &[StreamId::MAIN]
        };
        for st in streams {
            let step = self.enc.settle(*st, self.dev.host_mut(), &mut self.plan);
            self.run_plan(step, &mut log);
        }
        log
    }

    fn tick(&mut self) -> &TickLog {
        let t = self.ticks.len() as u64;
        let mut log = TickLog::default();
        let deadline = self.period.map(|p| {
            self.dev.advance_to(Nanos(t * p));
            Nanos((t + 1) * p)
        });
        log.start = self.dev.now();
        let step = self
            .enc
            .begin_tick(log.start, deadline, self.dev.host_mut(), &mut self.plan);
        self.run_plan(step, &mut log);
        log.end = self.dev.now();
        log.stats = self.enc.tick_stats();
        for s in &self.sessions {
            while let Some(au) = self.enc.take_output(*s) {
                log.aus.push((s.0, au));
            }
        }
        self.ticks.push(log);
        self.ticks.last().unwrap()
    }

    /// The access units of a session so far, in coded order.
    fn stream(&self, s: SessionId) -> Vec<AccessUnit> {
        self.ticks
            .iter()
            .flat_map(|t| &t.aus)
            .filter(|(id, _)| *id == s.0)
            .map(|(_, a)| a.clone())
            .collect()
    }

    /// Feed `frames` frames to every session (seed: its id + 1), one per tick, then flush and
    /// tick until nothing is left.
    fn run(&mut self, frames: u64) {
        for _ in 0..frames {
            for s in self.sessions.clone() {
                self.feed(s, u64::from(s.0) + 1);
            }
            self.tick();
        }
        self.drain();
    }

    fn drain(&mut self) {
        for s in self.sessions.clone() {
            let _ = self.enc.flush(s);
        }
        for _ in 0..64 {
            if self.enc.idle() {
                return;
            }
            self.tick();
        }
        panic!("the encoder does not become idle");
    }
}

/// Checks every stream must pass: coded indices count up, every display index appears once,
/// DTS advances by one frame period, the first picture is a key frame.
fn check_stream(aus: &[AccessUnit], frames: u64) {
    assert_eq!(aus.len() as u64, frames);
    for (k, a) in aus.iter().enumerate() {
        assert_eq!(a.coded, k as u64);
        assert_eq!(a.dts, (k as i64 - 1) * 3003, "dts of picture {k}");
        assert_eq!(a.pts, a.disp as i64 * 3003);
        assert_eq!(a.bits as usize, 8 * a.data.len());
        // A sequence header starts every key frame, and the P picture that replaces an I
        // picture on rung 37 (the C++ drops only its GOP header).
        let seq = a.keyframe || (a.rung == 37 && !a.repeat && a.data[3] == 0xB3);
        assert_eq!(&a.data[..4], &[0, 0, 1, if seq { 0xB3 } else { 0 }]);
    }
    let mut disp: Vec<u64> = aus.iter().map(|a| a.disp).collect();
    disp.sort_unstable();
    assert_eq!(disp, (0..frames).collect::<Vec<_>>());
    assert!(aus[0].keyframe && aus[0].pic_type == PIC_I);
}

fn kernels(plan: &Plan) -> Vec<&'static str> {
    plan.steps
        .iter()
        .filter_map(|s| match &s.op {
            Op::Launch(l) => Some(l.kernel.name()),
            _ => None,
        })
        .collect()
}

fn single(c: &SessionConfig, opt: EncoderOptions, frames: u64, seed: u64) -> (Rig, SessionId) {
    let mut r = Rig::new(opt);
    let s = r.open(c);
    r.settle();
    for _ in 0..frames {
        r.feed(s, seed);
        r.tick();
    }
    r.drain();
    (r, s)
}

#[test]
fn a_plain_run_on_the_model() {
    let (r, s) = single(&f1(), EncoderOptions::default(), 40, 1);
    let aus = r.stream(s);
    check_stream(&aus, 40);
    assert!(aus.iter().all(|a| !a.repeat));
    let keys: Vec<u64> = aus.iter().filter(|a| a.keyframe).map(|a| a.disp).collect();
    assert_eq!(keys, [0, 6, 12, 18, 24, 30, 36]);
    // Three plans per tick plus correction passes, classes as the C++ numbers its waits.
    for t in &r.ticks {
        assert_eq!(t.phases[0], Phase::Begin);
        assert_eq!(*t.phases.last().unwrap(), Phase::Finish);
        assert_eq!(t.classes[..2], [0, 1]);
        assert!(
            t.classes[2..t.classes.len() - 1]
                .iter()
                .all(|c| *c == CLASS_QUANT_RETRY)
        );
        assert_eq!(t.phases.len() as u32, 3 + t.stats.rounds);
    }
    assert!(r.ticks.iter().any(|t| t.stats.rounds > 0));
    assert!(r.enc.phase().is_none());
}

/// With the frames event the tick has no wait for the upload stream of its own: the kernel
/// stream waits for the event the last upload recorded, and the pictures are the same.
#[test]
fn the_frames_event_replaces_the_wait_for_the_upload_stream() {
    let run = |event: bool| {
        let mut r = Rig::new(EncoderOptions {
            overlap: true,
            frames_event: event,
            ..EncoderOptions::default()
        });
        let s1 = r.open(&f1());
        let s2 = r.open(&f2());
        r.settle();
        r.run(24);
        (r, [s1, s2])
    };
    let (wait, sw) = run(false);
    let (event, se) = run(true);
    for (a, b) in sw.iter().zip(&se) {
        assert_eq!(wait.stream(*a), event.stream(*b));
    }
    for (i, (a, b)) in wait.ticks.iter().zip(&event.ticks).enumerate() {
        assert_eq!(a.phases.first(), Some(&Phase::Sync));
        assert!(!b.phases.contains(&Phase::Sync));
        assert_eq!(a.phases.len(), b.phases.len() + 1);
        let begin = &b.plans[0];
        let waits_frames = matches!(
            begin.steps.first().map(|s| (s.stream, s.op)),
            Some((StreamId::MAIN, Op::WaitFor(EventId::FRAMES)))
        );
        // every tick after the first that queued an upload
        assert_eq!(waits_frames, i > 0, "tick {i}");
        let last_up = begin.steps.iter().rfind(|s| s.stream == StreamId::UPLOAD);
        assert!(matches!(
            last_up.map(|s| s.op),
            Some(Op::Record(EventId::FRAMES))
        ));
        // the same operations otherwise
        let strip = |p: &Plan| -> Vec<tb_device::plan::Step> {
            p.steps
                .iter()
                .filter(|s| {
                    !matches!(
                        s.op,
                        Op::WaitFor(EventId::FRAMES) | Op::Record(EventId::FRAMES)
                    )
                })
                .copied()
                .collect()
        };
        assert_eq!(strip(&a.plans[1]), strip(begin), "tick {i}");
    }
}

/// With split plans the device gets the same operations in the same order, a batch at a time,
/// and the streams are the same.
#[test]
fn split_plans_queue_the_same_operations() {
    let run = |split: bool| {
        let mut r = Rig::new(EncoderOptions {
            split_plans: split,
            ..EncoderOptions::default()
        });
        let s1 = r.open(&f1());
        let s2 = r.open(&f2());
        let s3 = r.open(&f1());
        r.settle();
        r.run(24);
        (r, [s1, s2, s3])
    };
    let (whole, sw) = run(false);
    let (split, ss) = run(true);
    for (a, b) in sw.iter().zip(&ss) {
        assert_eq!(whole.stream(*a), split.stream(*b));
    }
    assert_eq!(whole.ticks.len(), split.ticks.len());
    let mut parts = 0;
    for (a, b) in whole.ticks.iter().zip(&split.ticks) {
        assert!(a.queued.is_empty());
        assert_eq!(a.order, b.order);
        assert_eq!(a.phases, b.phases);
        // two batches: at most one part ahead of the last plan of the two split phases
        assert!(b.queued.len() <= 2);
        parts += b.queued.len();
    }
    assert!(
        parts > 20,
        "{parts} parts queued in {} ticks",
        split.ticks.len()
    );
}

/// Two batches of different formats in one tick: each session's output is what it is alone,
/// and the plans carry both batches in the order of the C++ (`Encoder::step`): the parameters
/// of all batches, then the kernels of all batches.
#[test]
fn two_formats_in_one_tick() {
    let (alone1, a1) = single(&f1(), EncoderOptions::default(), 30, 1);
    let (alone2, a2) = single(&f2(), EncoderOptions::default(), 30, 2);
    let mut r = Rig::new(EncoderOptions::default());
    let s1 = r.open(&f1());
    let s2 = r.open(&f2());
    assert_eq!(r.enc.batch_count(), 2);
    r.settle();
    r.run(30);
    check_stream(&r.stream(s1), 30);
    assert_eq!(r.stream(s1), alone1.stream(a1));
    assert_eq!(r.stream(s2), alone2.stream(a2));
    let (l1, l2) = (
        r.enc.batch_layout(s1).unwrap().0.clone(),
        r.enc.batch_layout(s2).unwrap().0.clone(),
    );
    let t = &r.ticks[3];
    let begin = &t.plans[0];
    let at = |f: &dyn Fn(&Op) -> bool| begin.steps.iter().position(|s| f(&s.op));
    let par1 = at(&|o| matches!(o, Op::Upload { dst, .. } if *dst == l1.d_par)).unwrap();
    let par2 = at(&|o| matches!(o, Op::Upload { dst, .. } if *dst == l2.d_par)).unwrap();
    let first_kernel =
        at(&|o| matches!(o, Op::Launch(l) if !l.kernel.name().starts_with("k_pad"))).unwrap();
    assert!(par1 < par2 && par2 < first_kernel);
    // the frames of the padded format go up row by row and are padded before the parameters
    assert_eq!(
        begin
            .steps
            .iter()
            .filter(|s| matches!(s.op, Op::Upload2d { .. }))
            .count(),
        3
    );
    let k = kernels(begin);
    assert_eq!(
        k,
        [
            "k_pad_cols",
            "k_pad_rows",
            "k_pad_cols",
            "k_pad_rows",
            "k_pad_cols",
            "k_pad_rows",
            "k_pyr",
            "k_me",
            "k_mvsmooth",
            "k_mbstat",
            "k_xf",
            "k_rowstat",
            "k_pyr",
            "k_me",
            "k_mvsmooth",
            "k_mbstat",
            "k_xf",
            "k_rowstat",
        ]
    );
    // the first quantization of both batches in one plan, and their downloads in one
    assert_eq!(
        kernels(&t.plans[1])
            .iter()
            .filter(|n| **n == "k_quant")
            .count(),
        2
    );
    let last = t.plans.last().unwrap();
    assert_eq!(
        kernels(last)
            .iter()
            .filter(|n| **n == "k_ent_scatter")
            .count(),
        2
    );
    assert!(begin.steps.iter().all(|s| s.stream == StreamId::MAIN));
}

/// Sessions opened, closed and reconfigured between ticks while others run: the output of the
/// others equals an undisturbed run, access unit for access unit.
#[test]
fn churn_leaves_the_running_sessions_alone() {
    let frames = 48;
    let mut base = Rig::new(EncoderOptions::default());
    let (a, b) = (base.open(&f1()), base.open(&f1()));
    base.settle();
    base.run(frames);

    let mut r = Rig::new(EncoderOptions::default());
    let (a2, b2) = (r.open(&f1()), r.open(&f1()));
    assert_eq!((a, b), (a2, b2));
    r.settle();
    let free0 = r.enc.free_bytes();
    let mut extra: Vec<(SessionId, u64)> = Vec::new();
    let mut closed: Vec<SessionId> = Vec::new();
    let mut c = None;
    let mut d = None;
    for t in 0..frames {
        match t {
            8 => {
                // same format: a free slot of the running batch, no device work
                let s = r.enc.open(&f1()).unwrap();
                assert_eq!(r.enc.batch_count(), 1);
                assert_eq!(r.enc.batch_layout(s).unwrap().1, 2);
                c = Some(s);
                extra.push((s, 7));
            }
            12 => {
                // another format: a new batch from the arenas
                let s = r.enc.open(&f2()).unwrap();
                assert_eq!(r.enc.batch_count(), 2);
                assert!(r.enc.free_bytes().0 < free0.0);
                d = Some(s);
                extra.push((s, 9));
            }
            20 => {
                let s = c.take().unwrap();
                r.enc.close(s).unwrap();
                assert_eq!(
                    r.enc.submit(s, FrameSource::Again, 0, &[]),
                    Err(EncoderError::State("the session is closing"))
                );
                extra.retain(|(x, _)| *x != s);
                closed.push(s);
            }
            26 => {
                // reconfigured: closed and opened again at another rate, in the freed slot
                let old = closed[0];
                assert!(r.enc.is_closed(old).unwrap());
                let cfg = SessionConfig {
                    bitrate: 240_000,
                    gop: 4,
                    ..f1()
                };
                let s = r.enc.open(&cfg).unwrap();
                assert_eq!(r.enc.batch_layout(s).unwrap().1, 2);
                assert_eq!(r.enc.batch_count(), 2);
                extra.push((s, 11));
                r.sessions.push(s);
            }
            30 => {
                let s = d.take().unwrap();
                r.enc.close(s).unwrap();
                extra.retain(|(x, _)| *x != s);
                closed.push(s);
            }
            40 => {
                // the batch of the second format has been without sessions for a while
                r.enc.reap(3);
                assert_eq!(r.enc.batch_count(), 1);
                assert_eq!(r.enc.free_bytes(), free0);
            }
            _ => {}
        }
        for s in [a, b] {
            r.feed(s, u64::from(s.0) + 1);
        }
        for (s, seed) in &extra {
            if !r.sessions.contains(s) {
                r.sessions.push(*s);
            }
            r.feed(*s, *seed);
        }
        r.tick();
    }
    r.drain();
    assert_eq!(r.stream(a), base.stream(a));
    assert_eq!(r.stream(b), base.stream(b));
    check_stream(&r.stream(a), frames);
    // The closed sessions: every frame coded, the last picture flagged, then released.
    let c_au = r.stream(closed[0]);
    check_stream(&c_au, 12);
    assert!(c_au.last().unwrap().last);
    assert_eq!(c_au.iter().filter(|x| x.last).count(), 1);
    let d_au = r.stream(closed[1]);
    check_stream(&d_au, 18);
    assert!(d_au.last().unwrap().last);
    for s in closed {
        r.enc.release(s).unwrap();
        assert_eq!(r.enc.snapshot(s), Err(EncoderError::NoSession));
    }
    assert_eq!(
        r.enc.release(a),
        Err(EncoderError::State("the session is not closed"))
    );
    // The reconfigured session started with its own key frame and sequence header.
    let e = r.sessions[r.sessions.len() - 1];
    let e_au = r.stream(e);
    check_stream(&e_au, frames - 26);
    assert_eq!(e_au[0].rate, 240_000);
}

/// A batch that does not fit is an error to the caller, not a stall, and changes nothing.
#[test]
fn arena_exhaustion_is_an_error() {
    let lay1 = BatchLayout::new(&f1(), 4, false).unwrap();
    let lay2 = BatchLayout::new(&f2(), 2, false).unwrap();
    assert!(lay2.dev_bytes > 4096);
    // Room for the first batch and not quite for the second, on the device.
    let arenas = ArenaSizes {
        device: lay1.dev_bytes + lay2.dev_bytes - 256,
        host: 1 << 20,
    };
    let mut r = Rig::with(EncoderOptions::default(), arenas);
    let s = r.open(&f1());
    let free = r.enc.free_bytes();
    assert_eq!(
        r.enc.open(&f2()),
        Err(EncoderError::ArenaFull {
            which: "device",
            need: lay2.dev_bytes,
            free: lay2.dev_bytes - 256,
        })
    );
    assert_eq!((r.enc.batch_count(), r.enc.free_bytes()), (1, free));
    // The running session is not affected, and its batch still has free slots.
    r.settle();
    r.run(12);
    let (base, b) = single(&f1(), EncoderOptions::default(), 12, 1);
    assert_eq!(r.stream(s), base.stream(b));
    assert!(r.enc.open(&f1()).is_ok());

    // The same on the host side: the device block is given back.
    let arenas = ArenaSizes {
        device: 8 << 20,
        host: lay1.host_bytes
            + (size_of::<tb_kernel_abi::Tables>() as u64).div_ceil(64) * 64
            + lay2.host_bytes
            - 64,
    };
    let mut enc = Encoder::new(EncoderOptions::default(), arenas);
    let s = enc.open(&f1()).unwrap();
    let free = enc.free_bytes();
    assert!(matches!(
        enc.open(&f2()),
        Err(EncoderError::ArenaFull { which: "host", .. })
    ));
    assert_eq!(enc.free_bytes(), free);
    // Once the first batch is gone there is room.
    enc.close(s).unwrap();
    enc.release(s).unwrap();
    enc.reap(0);
    assert_eq!(enc.batch_count(), 0);
    assert!(enc.open(&f2()).is_ok());

    // Other refusals are values as well.
    let mut enc = Encoder::new(EncoderOptions::default(), ARENAS);
    let odd = SessionConfig { w: 63, ..f1() };
    assert!(matches!(
        enc.open(&odd),
        Err(EncoderError::Profile(ProfileError::Size { .. }))
    ));
    let slow = SessionConfig {
        bitrate: 1000,
        ..f1()
    };
    assert!(matches!(enc.open(&slow), Err(EncoderError::Rate(_))));
    let still = SessionConfig { fps_n: 0, ..f1() };
    assert!(matches!(
        enc.open(&still),
        Err(EncoderError::Profile(ProfileError::FrameRate { .. }))
    ));
    assert_eq!((enc.batch_count(), enc.free_bytes().0), (0, ARENAS.device));
    assert_eq!(
        enc.submit(SessionId(5), FrameSource::Host, 0, &[]),
        Err(EncoderError::NoSession)
    );
    let s = enc.open(&f1()).unwrap();
    assert!(matches!(
        enc.submit(s, FrameSource::Again, 0, &[]),
        Err(EncoderError::State(_))
    ));
    enc.submit(s, FrameSource::Host, 0, &[]).unwrap();
    assert!(matches!(
        enc.submit(s, FrameSource::Host, 0, &[]),
        Err(EncoderError::State(_))
    ));
    // ... and commands during a tick.
    let mut host = vec![0u8; ARENAS.host as usize];
    let mut plan = Plan::new();
    assert_eq!(
        enc.begin_tick(Nanos(0), None, &mut host, &mut plan),
        Step::Run
    );
    assert!(matches!(enc.open(&f1()), Err(EncoderError::State(_))));
    assert!(matches!(enc.close(s), Err(EncoderError::State(_))));
    assert!(matches!(
        enc.set_rate(s, 300_000, true),
        Err(EncoderError::State(_))
    ));
}

fn start_codes(data: &[u8]) -> Vec<u8> {
    data.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| w[3])
        .collect()
}

/// temporal_reference of the picture header of an access unit.
fn temporal_reference(data: &[u8]) -> u32 {
    let i = data
        .windows(4)
        .position(|w| w == [0, 0, 1, 0])
        .expect("a picture header");
    (u32::from(data[i + 4]) << 2) | (u32::from(data[i + 5]) >> 6)
}

/// A run of one session in which the frame with display index `huge` cannot be made to fit.
fn ladder_run(huge: u64, frames: u64) -> (Rig, SessionId) {
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.settle();
    for n in 0..frames {
        r.feed_c(s, if n == huge { 255 } else { content(1, n) });
        r.tick();
    }
    r.drain();
    (r, s)
}

/// Ladder rung 37: a P picture that does not fit even with no coefficients
/// is coded all-skipped: forward mode everywhere, the transform repeated, `keep` 0.
#[test]
fn rung_37_codes_an_all_skipped_picture() {
    let (r, s) = ladder_run(3, 14);
    let aus = r.stream(s);
    check_stream(&aus, 14);
    let au = aus.iter().find(|a| a.disp == 3).unwrap();
    assert_eq!((au.pic_type, au.rung, au.repeat), (PIC_P, 37, false));
    assert!(au.tries >= 3, "{}", au.tries);
    assert_eq!(f64::from(au.avg_q), 112.0 * 7.0);
    assert!(au.data.len() < 200);
    assert_eq!(aus.iter().filter(|a| a.rung == 37).count(), 1);
    let t = r
        .ticks
        .iter()
        .find(|t| t.aus.iter().any(|(_, a)| a.disp == 3))
        .unwrap();
    // The last correction pass: parameters twice (as the C++), the list of all-skipped
    // sessions, forward mode, the transform again, then quantize and pack.
    let last = &t.plans[t.plans.len() - 2];
    assert_eq!(
        kernels(last),
        [
            "k_set_mode",
            "k_xf",
            "k_quant",
            "k_ent_resolve",
            "k_ent_pack",
            "k_ent_rowsum",
            "k_ent_picsum"
        ]
    );
    let lay = r.enc.batch_layout(s).unwrap().0;
    let uploads: Vec<_> = last
        .steps
        .iter()
        .filter_map(|x| match x.op {
            Op::Upload { dst, .. } => Some(dst.off),
            _ => None,
        })
        .collect();
    let list = |l: tb_m2v_encoder::layout::List| lay.d_list[l as usize].off;
    use tb_m2v_encoder::layout::List;
    assert_eq!(
        uploads,
        [
            list(List::Retry),
            lay.d_par.off,
            list(List::Rec),
            lay.d_par.off,
            lay.d_rowq.off
        ]
    );
    // The passes before it walk the ladder: only quantization is repeated.
    assert!(t.stats.rounds >= 3);
    let snap = r.enc.snapshot(s).unwrap();
    assert_eq!(snap.rc.stats.skip_pics, 1);
    assert_eq!(snap.rc.stats.isub, 0);
    assert_eq!(snap.rc.stats.violations, 0);
    // It is an anchor like any other P picture: the pictures after it are coded normally.
    assert!(aus.iter().filter(|a| a.disp > 3).all(|a| a.rung < 37));
}

/// The same for a key frame that is not the first picture: the I picture is replaced by an
/// all-skipped P picture, without sequence or GOP header, and temporal_reference keeps counting.
#[test]
fn an_i_picture_that_cannot_fit_is_replaced_by_p() {
    let (r, s) = ladder_run(6, 14);
    let aus = r.stream(s);
    check_stream(&aus, 14);
    let au = aus.iter().find(|a| a.disp == 6).unwrap();
    assert_eq!((au.pic_type, au.rung, au.repeat), (PIC_P, 37, false));
    assert!(!au.keyframe && !au.key_missed && !au.off_grid);
    let codes = start_codes(&au.data);
    // Neither the GOP header nor the sequence header (the C++ kept the latter): a sequence
    // header is a point of random access, and an all-skipped P picture is none.
    assert_eq!(codes[..2], [0x00, 0xB5]);
    assert!(!codes.contains(&0xB8), "no GOP header");
    assert!(!codes.contains(&0xB3), "no sequence header");
    assert_eq!(temporal_reference(&au.data), 6);
    let snap = r.enc.snapshot(s).unwrap();
    assert_eq!(snap.rc.stats.isub, 1);
    assert_eq!(snap.rc.stats.skip_pics, 1);
    // The pictures of the GOP that never started count on from the old one.
    let b = aus.iter().find(|a| a.disp == 7).unwrap();
    assert_eq!(temporal_reference(&b.data), 7);
    // The next key frame is where the grid has it.
    let keys: Vec<u64> = aus.iter().filter(|a| a.keyframe).map(|a| a.disp).collect();
    assert_eq!(keys, [0, 12]);
    // The device was told: P, forward reference the newest anchor, nothing kept, prediction
    // wanted (the picture is reconstructed).
    let t = r
        .ticks
        .iter()
        .find(|t| t.aus.iter().any(|(_, a)| a.disp == 6))
        .unwrap();
    assert_eq!(
        kernels(&t.plans[0])[0],
        "k_set_mode",
        "begun as an I picture"
    );
    assert!(kernels(t.plans.last().unwrap()).contains(&"k_recon"));
}

/// The first picture of a stream can be neither skipped nor replaced: it stays on rung 36.
/// The model's picture that never shrinks is larger than the output buffer even there (the
/// real kernels cannot make one: at most 125 bits a macroblock on that rung). It is never
/// written in part, as the C++ did: nothing is put out and the picture is tried again.
#[test]
fn a_first_picture_larger_than_the_output_buffer_is_not_cut() {
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.settle();
    for _ in 0..3 {
        r.feed_c(s, 255);
        r.tick();
        assert!(r.ticks.last().unwrap().aus.is_empty());
    }
    assert!(r.stream(s).is_empty());
    let snap = r.enc.snapshot(s).unwrap();
    assert!(snap.rc.stats.violations >= 1);
    assert_eq!(snap.rc.cur.rung, 36);
    assert_eq!(snap.stats.pictures, 0);
}

/// Frames written on the device by the caller (`submit_dev`): no upload; the padding is
/// filled at the head of the `Begin` plan on the kernel stream; the picture is coded in the
/// same tick; the output equals that of the same frames from host memory.
#[test]
fn frames_written_on_the_device() {
    let (base, b) = single(&f2(), EncoderOptions::default(), 20, 3);
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f2());
    // before the batch is set up a frame written to the device would be wiped
    assert!(matches!(r.enc.frame_target(s), Err(EncoderError::State(_))));
    r.settle();
    for n in 0..20u64 {
        let (planes, pitch) = r.enc.frame_target(s).unwrap();
        assert_eq!(pitch, 80);
        assert_eq!((planes[0].len, planes[1].len), (80 * 48, 40 * 24));
        assert_eq!(r.enc.frame_buffer(s).unwrap().len, 72 * 40 * 3 / 2);
        assert_eq!(planes[1].off, planes[0].end());
        // the caller's kernels: 72 x 40 samples at the pitch of the coded size
        for row in 0..40 {
            let o = planes[0].off as usize + row * pitch as usize;
            r.dev.mem[o..o + 72].fill(content(3, n));
        }
        r.enc
            .submit(
                s,
                FrameSource::Device { progressive: false },
                n as i64 * 3003,
                &[],
            )
            .unwrap();
        *r.n.entry(s.0).or_default() += 1;
        let t = r.tick().clone();
        let begin = &t.plans[0];
        assert!(
            !begin
                .steps
                .iter()
                .any(|x| matches!(x.op, Op::Upload2d { .. }))
        );
        // 72 x 40 in 80 x 48: columns and rows to fill in all three planes
        assert_eq!(kernels(begin)[..6], ["k_pad_cols", "k_pad_rows"].repeat(3));
        let lay = r.enc.batch_layout(s).unwrap().0;
        let first_two: Vec<u64> = begin.steps[..2]
            .iter()
            .map(|x| match x.op {
                Op::Upload { dst, .. } => dst.off,
                _ => 0,
            })
            .collect();
        assert_eq!(
            first_two,
            [
                lay.d_list[tb_m2v_encoder::layout::List::Dev as usize].off,
                lay.d_devslot.off
            ]
        );
    }
    r.drain();
    assert_eq!(r.stream(s), base.stream(b));
}

/// The overlapped upload: one tick more delay, the same pictures. A session has two host
/// frame regions, used in turn, because the upload of one tick is still in flight when the
/// next frame is written.
#[test]
fn the_overlapped_upload_codes_the_same_pictures() {
    let opt = EncoderOptions {
        overlap: true,
        ..EncoderOptions::default()
    };
    for c in [f1(), f2()] {
        let (base, b) = single(&c, EncoderOptions::default(), 24, 5);
        let mut r = Rig::new(opt);
        let s = r.open(&c);
        let st = r.settle();
        assert_eq!(st.plans[0].wait, StreamId::UPLOAD);
        assert!(st.plans[0].steps.is_empty());
        assert!(matches!(st.plans[1].steps[0].op, Op::Fill { .. }));
        let mut bufs = Vec::new();
        for _ in 0..24 {
            bufs.push(r.enc.frame_buffer(s).unwrap());
            r.feed(s, 5);
            let t = r.tick();
            assert_eq!(t.phases[..2], [Phase::Sync, Phase::Begin]);
            assert_eq!(t.classes[0], 3);
            let begin = &t.plans[1];
            let up: Vec<_> = begin
                .steps
                .iter()
                .filter(|x| x.stream == StreamId::UPLOAD)
                .collect();
            assert_eq!(up[0].op, Op::WaitFor(tb_device::EventId::PARAMS));
            assert!(up.len() > 1);
        }
        r.drain();
        assert_eq!(bufs[0], bufs[2]);
        assert_ne!(bufs[0], bufs[1]);
        assert_eq!(bufs[1], bufs[3]);
        assert_eq!(r.stream(s), base.stream(b));
        // nothing is coded in the first tick: its frame is on its way
        assert!(r.ticks[0].aus.is_empty() && !r.ticks[1].aus.is_empty());
    }
}

/// `FrameSource::HostAt`: frames uploaded from regions the caller owns (a clip held in the
/// host arena) code the same pictures as frames copied into the session's frame buffer, with
/// and without the overlapped upload; a region of the wrong size or outside the arena is
/// refused.
#[test]
fn frames_in_the_callers_own_host_regions() {
    for overlap in [false, true] {
        let opt = EncoderOptions {
            overlap,
            ..EncoderOptions::default()
        };
        for c in [f1(), f2()] {
            let (base, b) = single(&c, EncoderOptions::default(), 24, 5);
            let mut r = Rig::new(opt);
            let s = r.open(&c);
            r.settle();
            let fb = r.enc.frame_buffer(s).unwrap().len;
            // a "clip" of four frames in the host arena, outside every batch
            let free = r.enc.free_bytes().1;
            let clip = r.enc.reserve_host(4 * fb).unwrap().off;
            assert_eq!(r.enc.free_bytes().1, free - (4 * fb).div_ceil(64) * 64);
            assert_eq!(r.enc.reserve_host(ARENAS.host), None);
            assert_eq!(
                r.enc
                    .submit(s, FrameSource::HostAt(HostBuf::new(clip, fb - 1)), 0, &[]),
                Err(EncoderError::State(
                    "the frame region is not w * h * 3 / 2 bytes inside the host arena"
                ))
            );
            assert!(
                r.enc
                    .submit(
                        s,
                        FrameSource::HostAt(HostBuf::new(ARENAS.host, fb)),
                        0,
                        &[]
                    )
                    .is_err()
            );
            for n in 0..24u64 {
                // with the overlapped upload the frame of the tick before may still be read
                let at = HostBuf::new(clip + (n % 4) * fb, fb);
                at.of_mut(r.dev.host_mut()).unwrap().fill(content(5, n));
                r.enc
                    .submit(s, FrameSource::HostAt(at), n as i64 * 3003, &[])
                    .unwrap();
                let t = r.tick();
                let uploads_from_clip = t.plans.iter().flat_map(|p| &p.steps).any(|x| match x.op {
                    Op::Upload { src, .. } | Op::Upload2d { src, .. } => src.off == at.off,
                    _ => false,
                });
                assert!(
                    uploads_from_clip,
                    "frame {n} is uploaded from its own region"
                );
            }
            r.drain();
            assert_eq!(r.stream(s), base.stream(b));
        }
    }
}

/// `FrameSource::Again`: the previous frame's ring slot, no upload.
#[test]
fn a_frame_submitted_again_uses_the_same_slot() {
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.settle();
    for n in 0..12u64 {
        if n % 3 == 2 {
            r.enc
                .submit(s, FrameSource::Again, n as i64 * 3003, &common::captions())
                .unwrap();
            *r.n.entry(s.0).or_default() += 1;
        } else {
            r.feed(s, 1);
        }
        let t = r.tick();
        let uploads_frame = t.plans[0]
            .steps
            .iter()
            .any(|x| matches!(x.op, Op::Upload { src, .. } if src.len == 64 * 48 * 3 / 2));
        assert_eq!(uploads_frame, n % 3 != 2);
    }
    r.drain();
    let aus = r.stream(s);
    check_stream(&aus, 12);
    // the repeated frame carries its captions
    let again = aus.iter().find(|a| a.disp == 2).unwrap();
    assert!(start_codes(&again.data).contains(&0xB2));
}

/// A wait that times out is not a failure: the driver waits again, nothing changes.
#[test]
fn a_timeout_waits_again() {
    let (base, b) = single(&f1(), EncoderOptions::default(), 10, 1);
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.settle();
    for n in 0..10 {
        if n == 4 {
            r.trouble.push((Phase::Quant, 0, Trouble::Delay(7_000_000)));
        }
        r.feed(s, 1);
        r.tick();
    }
    r.drain();
    assert_eq!(r.ticks[4].waits, 3);
    assert!(r.ticks[4].errors.is_empty());
    assert_eq!(r.stream(s), base.stream(b));
}

/// Six sessions of one batch, three of priority 0 and three of priority 1, staggered.
fn paced(period: u64) -> Rig {
    let mut r = Rig::new(EncoderOptions {
        shed_margin_ns: 0,
        ..EncoderOptions::default()
    });
    for k in 0..6u32 {
        r.open(&SessionConfig {
            capacity: 6,
            priority: i32::from(k >= 3),
            gop_phase: k % 3 * 2,
            ..f1()
        });
    }
    r.settle();
    r.period = Some(period);
    r
}

fn feed_all(r: &mut Rig) {
    for s in r.sessions.clone() {
        r.feed(s, u64::from(s.0) + 1);
    }
}

const PERIOD: u64 = 5_000_000;

/// The class of a picture for the deadline policy: 0 B, 1 P, 2 key.
fn class(a: &AccessUnit) -> u8 {
    match a.pic_type {
        PIC_B => 0,
        PIC_P => 1,
        _ => 2,
    }
}

/// A plan that completes late leaves the next tick too little time: pictures are given up in
/// the order of the policy (lower priority first; among equals B, then P, key frames last;
/// in rotation), one picture is still coded while there is time, and the tick after is back
/// to normal. With a deadline that has already passed, everything is given up.
#[test]
fn a_late_completion_sheds_in_policy_order() {
    let mut base = paced(PERIOD);
    for _ in 0..40 {
        feed_all(&mut base);
        base.tick();
    }
    assert!(base.ticks.iter().all(|t| t.stats.shed == 0));
    assert!(base.ticks[20..].iter().all(|t| t.stats.candidates == 6));
    base.period = None;
    base.drain();
    let typical = base.ticks[25].end.0 - base.ticks[25].start.0;
    assert!(typical * 4 < PERIOD);

    let mut seen_partial = 0;
    for late_tick in 22..28u64 {
        let mut r = paced(PERIOD);
        for t in 0..40u64 {
            if t == late_tick {
                // this tick ends when the next one has 55 % of a typical tick left
                r.trouble.push((
                    Phase::Finish,
                    0,
                    Trouble::Delay(2 * PERIOD - typical * 55 / 100 - typical),
                ));
            }
            feed_all(&mut r);
            r.tick();
        }
        r.period = None;
        r.drain();
        // Up to and including the late tick nothing differs but the time.
        for t in 0..=late_tick as usize {
            assert_eq!(r.ticks[t].aus, base.ticks[t].aus);
        }
        let next = late_tick as usize + 1;
        let (got, was) = (&r.ticks[next], &base.ticks[next]);
        assert!(got.start.0 > next as u64 * PERIOD, "the tick started late");
        assert_eq!(got.stats.candidates, 6);
        let n = got.stats.shed as usize;
        assert!(n > 0 && n < 6, "tick {next}: {n} given up");
        seen_partial += 1;
        // The order of the policy: priority, class, position in collection order (the rotation
        // value grows with it within a tick).
        let mut order: Vec<(i32, u8, u32)> = was
            .aus
            .iter()
            .map(|(s, a)| (i32::from(*s >= 3), class(a), *s))
            .collect();
        order.sort_unstable();
        let expect: Vec<u32> = order[..n].iter().map(|o| o.2).collect();
        let mut shed: Vec<u32> = got
            .aus
            .iter()
            .filter(|(_, a)| a.repeat)
            .map(|(s, _)| *s)
            .collect();
        let mut expect_sorted = expect.clone();
        expect_sorted.sort_unstable();
        shed.sort_unstable();
        assert_eq!(shed, expect_sorted, "tick {next}: {order:?}");
        // The pictures kept are coded as they would have been.
        for ((s, a), (_, b)) in got.aus.iter().zip(&was.aus) {
            if !a.repeat {
                assert_eq!(a.data, b.data, "session {s}");
            } else {
                assert_eq!(a.key_missed, b.keyframe);
                assert_eq!(
                    (a.disp, a.coded, a.dts, a.rung),
                    (b.disp, b.coded, b.dts, 37)
                );
            }
        }
        // Priority 1 gives nothing up before priority 0 has given up everything it can.
        if shed.iter().any(|s| *s >= 3) {
            assert_eq!(shed.iter().filter(|s| **s < 3).count(), 3);
        }
        // The tick after is on time again and gives nothing up.
        assert_eq!(r.ticks[next + 1].stats.shed, 0);
        assert!(r.ticks[next + 1].start.0 < (next as u64 + 1) * PERIOD + typical);
        for s in r.sessions.clone() {
            check_stream(&r.stream(s), 40);
        }
    }
    assert_eq!(seen_partial, 6);

    // A tick that starts after its deadline gives up everything.
    let mut r = paced(PERIOD);
    for t in 0..30u64 {
        if t == 22 {
            r.trouble
                .push((Phase::Begin, 0, Trouble::Delay(2 * PERIOD + 1000)));
        }
        feed_all(&mut r);
        r.tick();
    }
    assert_eq!(r.ticks[23].stats.shed, 6);
    assert!(r.ticks[23].aus.iter().all(|(_, a)| a.repeat));
    assert_eq!(r.ticks[23].stats.mbs, 0.0);
    assert_eq!(r.ticks[24].stats.shed, 0);
    // Without a deadline, or with the policy off, the same late tick gives nothing up.
    for off in [true, false] {
        let mut r = paced(PERIOD);
        if off {
            r = Rig::new(EncoderOptions {
                shed: false,
                ..EncoderOptions::default()
            });
            r.open(&f1());
            r.settle();
            r.period = Some(PERIOD);
        } else {
            r.period = None;
        }
        for t in 0..26u64 {
            if t == 22 {
                r.trouble
                    .push((Phase::Begin, 0, Trouble::Delay(2 * PERIOD + 1000)));
            }
            feed_all(&mut r);
            r.tick();
        }
        assert!(r.ticks.iter().all(|t| t.stats.shed == 0));
    }
}

/// A key frame given up: its position carries a repeat flagged `key_missed`; the next frame
/// submitted is the key frame, off the grid; the cadence stays on its grid. If the make-up
/// key frame is given up as well there is no second attempt.
#[test]
fn a_missed_key_frame_is_made_up_once_and_the_grid_stays() {
    let mut base = paced(PERIOD);
    for _ in 0..48 {
        feed_all(&mut base);
        base.tick();
    }
    base.period = None;
    base.drain();
    // A tick after the warm-up in which session 0 codes a key frame.
    let key_tick = (20..40)
        .find(|t| {
            base.ticks[*t]
                .aus
                .iter()
                .any(|(s, a)| *s == 0 && a.keyframe)
        })
        .unwrap();
    let grid_disp = base.ticks[key_tick]
        .aus
        .iter()
        .find(|(s, _)| *s == 0)
        .unwrap()
        .1
        .disp;
    // The make-up key frame is the frame submitted after the miss; it is coded a few ticks
    // later (the pictures queued before it come first).
    let made_up = key_tick as u64 + 1;
    let mut made_up_tick = 0;
    for misses in [1usize, 2] {
        let mut r = paced(PERIOD);
        for t in 0..48 {
            // the tick before ends after the deadline of this one: everything is given up
            if t + 1 == key_tick || (misses == 2 && t + 1 == made_up_tick) {
                r.trouble
                    .push((Phase::Begin, 0, Trouble::Delay(2 * PERIOD + 1000)));
            }
            feed_all(&mut r);
            r.tick();
        }
        r.period = None;
        r.drain();
        let s0 = r.stream(SessionId(0));
        check_stream(&s0, 48);
        let missed: Vec<&AccessUnit> = s0.iter().filter(|a| a.key_missed).collect();
        assert_eq!(missed.len(), misses);
        assert_eq!(missed[0].disp, grid_disp);
        assert!(missed.iter().all(|a| a.repeat && a.pic_type == PIC_P));
        let off: Vec<u64> = s0.iter().filter(|a| a.off_grid).map(|a| a.disp).collect();
        let keys: Vec<u64> = s0
            .iter()
            .filter(|a| a.keyframe && a.disp >= grid_disp)
            .map(|a| a.disp)
            .collect();
        if misses == 1 {
            // one attempt, at the next frame submitted
            assert_eq!(off, [made_up]);
            assert_eq!(keys[..2], [made_up, grid_disp + 6]);
            made_up_tick = r
                .ticks
                .iter()
                .position(|t| t.aus.iter().any(|(s, a)| *s == 0 && a.off_grid))
                .unwrap();
            assert!(made_up_tick > key_tick);
        } else {
            // the make-up key frame was given up too: no second attempt, wait for the grid
            assert_eq!(missed[1].disp, made_up);
            assert!(off.is_empty());
            assert_eq!(keys[0], grid_disp + 6);
        }
        // The cadence did not move: every later key frame of the undisturbed run is one.
        let base_keys: Vec<u64> = base
            .stream(SessionId(0))
            .iter()
            .filter(|a| a.keyframe && a.disp > grid_disp)
            .map(|a| a.disp)
            .collect();
        assert!(base_keys.len() >= 2);
        for k in &base_keys {
            assert!(keys.contains(k), "key frame at {k} of {keys:?}");
        }
        let snap = r.enc.snapshot(SessionId(0)).unwrap();
        assert_eq!(snap.stats.keys_missed as usize, misses);
        // The others gave their pictures up as well and all streams stay whole.
        for s in r.sessions.clone() {
            check_stream(&r.stream(s), 48);
        }
    }
}

fn three(opt: EncoderOptions) -> Rig {
    let mut r = Rig::new(opt);
    for k in 0..3u32 {
        r.open(&SessionConfig {
            gop_phase: k * 2,
            ..f1()
        });
    }
    r
}

/// A plan that fails in each phase (docs/ARCHITECTURE.md §4.5): the pictures it was coding
/// become repeats, their sessions start again with a key frame at the next frame submitted,
/// the cadence stays on its grid, and the encoder goes on with the same device.
#[test]
fn a_failed_plan_in_each_phase_and_the_encoder_goes_on() {
    let frames = 40;
    for overlap in [false, true] {
        let opt = EncoderOptions {
            overlap,
            ..EncoderOptions::default()
        };
        let mut base = three(opt);
        base.settle();
        base.run(frames);
        for (phase, nth) in [
            (Phase::Begin, 0),
            (Phase::Quant, 0),
            (Phase::Quant, 1),
            (Phase::Finish, 0),
        ] {
            // a tick whose first quantization is corrected, so that there is a second plan
            let at = (12..30)
                .find(|t| base.ticks[*t].stats.rounds > 0 && base.ticks[*t].aus.len() == 3)
                .unwrap();
            let mut r = three(opt);
            r.settle();
            let error = DeviceError::Failed { step: 1, code: 2 };
            for t in 0..frames as usize {
                if t == at {
                    r.trouble.push((phase, nth, Trouble::Fail(error.clone())));
                }
                feed_all(&mut r);
                r.tick();
            }
            r.drain();
            let what = format!("overlap {overlap}, {phase:?} #{nth}");
            let bad = &r.ticks[at];
            assert_eq!(bad.errors, [error], "{what}");
            assert!(bad.stats.failed, "{what}");
            assert_eq!(
                *bad.phases.last().unwrap(),
                phase,
                "{what}: the tick ended there"
            );
            assert_eq!(bad.aus.len(), 3, "{what}");
            for ((s, a), (_, b)) in bad.aus.iter().zip(&base.ticks[at].aus) {
                assert!(a.repeat && a.rung == 37, "{what}: session {s}");
                assert_eq!((a.disp, a.coded, a.dts), (b.disp, b.coded, b.dts));
                assert_eq!(a.key_missed, b.keyframe);
                assert_eq!(start_codes(&a.data)[..2], [0x00, 0xB5]);
            }
            for t in 0..at {
                assert_eq!(r.ticks[t].aus, base.ticks[t].aus, "{what}: tick {t}");
            }
            assert!(r.ticks[at + 1..].iter().all(|t| t.errors.is_empty()));
            assert!(!r.enc.is_lost());
            for s in r.sessions.clone() {
                let aus = r.stream(s);
                check_stream(&aus, frames);
                let snap = r.enc.snapshot(s).unwrap();
                assert_eq!(snap.stats.failed, 1, "{what}");
                // The frame submitted next became a key frame, and it was coded as one: off the
                // grid unless the grid has a key frame there anyway.
                let submitted = at as u64 + 1;
                let made_up = aus.iter().find(|a| a.disp == submitted).unwrap();
                assert!(
                    made_up.keyframe && !made_up.repeat,
                    "{what}: session {}",
                    s.0
                );
                assert_eq!(
                    made_up.off_grid,
                    !(submitted + u64::from(s.0) * 2).is_multiple_of(6),
                    "{what}: session {}",
                    s.0
                );
                // The cadence stayed on its grid.
                let grid: Vec<u64> = base
                    .stream(s)
                    .iter()
                    .filter(|a| a.keyframe && a.disp > submitted)
                    .map(|a| a.disp)
                    .collect();
                assert!(!grid.is_empty());
                for k in grid {
                    assert!(
                        aus.iter().any(|a| a.disp == k && a.keyframe),
                        "{what}: session {}, key frame at {k}",
                        s.0
                    );
                }
            }
        }
    }
}

/// A failed wait for the upload stream (overlapped upload): the frames it should have
/// confirmed never reached the device, so their pictures are repeats; the tick itself goes on.
#[test]
fn a_failed_upload_marks_its_frames() {
    let opt = EncoderOptions {
        overlap: true,
        ..EncoderOptions::default()
    };
    let mut r = three(opt);
    r.settle();
    for t in 0..30 {
        if t == 14 {
            r.trouble.push((
                Phase::Sync,
                0,
                Trouble::Fail(DeviceError::Failed { step: 0, code: 9 }),
            ));
        }
        feed_all(&mut r);
        r.tick();
    }
    r.drain();
    let bad = &r.ticks[14];
    assert_eq!(bad.errors.len(), 1);
    assert!(!bad.stats.failed, "the tick went on");
    assert!(bad.phases.contains(&Phase::Finish));
    for s in r.sessions.clone() {
        let aus = r.stream(s);
        check_stream(&aus, 30);
        // the frame uploaded during tick 13 is display index 13
        let lost: Vec<u64> = aus.iter().filter(|a| a.repeat).map(|a| a.disp).collect();
        assert!(lost.contains(&13), "session {}: {lost:?}", s.0);
        assert_eq!(r.enc.snapshot(s).unwrap().stats.failed, 0);
    }
}

/// The set-up of a new batch that fails is queued again with the next plan.
#[test]
fn a_failed_set_up_is_queued_again() {
    let (base, b) = single(&f1(), EncoderOptions::default(), 10, 1);
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.trouble.push((
        Phase::Settle,
        0,
        Trouble::Fail(DeviceError::Failed { step: 0, code: 2 }),
    ));
    let st = r.settle();
    assert_eq!(st.errors.len(), 1);
    assert!(matches!(st.plans[0].steps[0].op, Op::Fill { .. }));
    // the device block is still what the model started with
    let block = r.enc.batch_block(s).unwrap();
    assert_eq!(r.dev.mem[block.off as usize + 100], 0xA5);
    for _ in 0..10 {
        r.feed(s, 1);
        r.tick();
    }
    r.drain();
    let fills = |t: &TickLog| {
        t.plans[0]
            .steps
            .iter()
            .filter(|x| matches!(x.op, Op::Fill { dst, .. } if dst == block))
            .count()
    };
    assert_eq!(fills(&r.ticks[0]), 1);
    assert_eq!(fills(&r.ticks[1]), 0);
    let lay = r.enc.batch_layout(s).unwrap().0;
    let tables = tb_m2v_syntax::tables::tables();
    let at = lay.d_tables.off as usize;
    assert_eq!(
        &r.dev.mem[at..at + lay.d_tables.len as usize],
        bytemuck::bytes_of(&tables)
    );
    assert_eq!(r.stream(s), base.stream(b));
}

/// The device is lost: every session repeats, no plan is written, the timeline goes on.
/// With a new device in the same process the sessions start again with a key frame; in a new
/// process they are restored from their snapshots.
#[test]
fn a_lost_device_then_restored_or_restarted() {
    let mut r = three(EncoderOptions::default());
    r.settle();
    for t in 0..20 {
        if t == 10 {
            r.trouble.push((
                Phase::Quant,
                0,
                Trouble::Fail(DeviceError::Lost { code: 7 }),
            ));
        }
        feed_all(&mut r);
        r.tick();
    }
    assert!(r.enc.is_lost());
    let submits = r.dev.submits;
    assert!(r.ticks[11..].iter().all(|t| t.plans.is_empty()));
    assert!(
        r.ticks[10..]
            .iter()
            .flat_map(|t| &t.aus)
            .all(|(_, a)| a.repeat)
    );
    // One access unit per session and tick while the device is away.
    assert!(r.ticks[12..].iter().all(|t| t.aus.len() == 3));
    let mut plan = Plan::new();
    assert_eq!(
        r.enc.settle(StreamId::MAIN, r.dev.host_mut(), &mut plan),
        Step::Done
    );
    let snaps: Vec<_> = r
        .sessions
        .iter()
        .map(|s| r.enc.snapshot(*s).unwrap())
        .collect();
    for sn in &snaps {
        assert!(sn.have_ref && sn.rc.cbr);
        assert_eq!(sn.n_in, 20);
    }

    // (a) a new device in the same process
    r.dev = Model::new(ARENAS);
    r.enc.device_restored();
    assert!(!r.enc.is_lost());
    for _ in 20..40 {
        feed_all(&mut r);
        r.tick();
    }
    r.drain();
    assert!(submits > 0 && r.dev.submits > 0);
    assert!(matches!(r.ticks[20].plans[0].steps[0].op, Op::Fill { .. }));
    assert!(matches!(
        r.ticks[20].plans[0].steps[1].op,
        Op::Upload { .. }
    ));
    for s in r.sessions.clone() {
        let aus = r.stream(s);
        check_stream(&aus, 40);
        // After the loss: repeats, then a key frame (the first frame submitted to the new
        // device), then pictures coded as always.
        let first_coded = aus.iter().position(|a| a.coded > 11 && !a.repeat).unwrap();
        assert!(aus[first_coded].keyframe, "session {}", s.0);
        assert_eq!(aus[first_coded].disp, 20);
        let later = &aus[first_coded..];
        assert!(later.iter().filter(|a| !a.repeat).count() > later.len() / 2);
        // nothing predicted from the lost device
        assert!(
            aus.iter()
                .filter(|a| a.coded > 11 && (a.coded as usize) < first_coded)
                .all(|a| a.repeat)
        );
    }

    // (b) a new process: a new encoder and device, the sessions restored from the snapshots
    let mut n = Rig::new(EncoderOptions::default());
    for sn in &snaps {
        let s = n.enc.restore(sn).unwrap();
        n.sessions.push(s);
        n.n.insert(s.0, sn.n_in);
    }
    n.settle();
    for _ in 0..18 {
        feed_all(&mut n);
        n.tick();
    }
    n.drain();
    for (s, sn) in n.sessions.clone().into_iter().zip(&snaps) {
        let aus = n.stream(s);
        assert_eq!(aus.len(), 18);
        for (k, a) in aus.iter().enumerate() {
            assert_eq!(a.coded, sn.n_coded + k as u64);
            assert_eq!(a.dts, (a.coded as i64 - 1) * 3003);
        }
        let first = aus.iter().find(|a| !a.repeat).unwrap();
        assert!(first.keyframe && first.disp == 20);
        assert_eq!(start_codes(&first.data)[0], 0xB3);
        assert!(aus.iter().filter(|a| !a.repeat).count() >= 15);
        // the buffer model went on from where it was
        let after = n.enc.snapshot(s).unwrap();
        assert_eq!(after.rc.vbv.d, sn.rc.vbv.d);
        assert_eq!(after.n_coded, sn.n_coded + 18);
        assert_eq!(after.next_ticket, sn.next_ticket);
    }
}

/// The parameters as the device received them (the model keeps what was uploaded).
#[test]
fn the_parameters_reach_the_device() {
    let opt = EncoderOptions {
        tickmark: true,
        ..EncoderOptions::default()
    };
    let mut r = Rig::new(opt);
    let s = r.open(&SessionConfig {
        bitrate: 0,
        max_bitrate: 15_000_000,
        vbv_bits: 1_835_008,
        fixed_q: [5, 7, 9],
        ..f1()
    });
    r.settle();
    let mut types = Vec::new();
    for _ in 0..8 {
        // content that fits the output buffer at these quantizers: a fixed quantizer stands
        // only while the picture fits
        r.feed_c(s, 2);
        let t = r.tick().clone();
        assert_eq!(kernels(&t.plans[0])[0], "k_tick");
        let Some((_, au)) = t.aus.first() else {
            continue;
        };
        let lay = r.enc.batch_layout(s).unwrap().0;
        let at = lay.d_par.off as usize;
        let par: SessPar = bytemuck::pod_read_unaligned(&r.dev.mem[at..at + size_of::<SessPar>()]);
        assert_eq!(par.pic_type, au.pic_type);
        let q = [5, 7, 9][usize::from(au.pic_type) - 1];
        assert_eq!((par.qfloor, par.keep, par.aq), (q, 64, 0));
        let rowq = &r.dev.mem[lay.d_rowq.off as usize..][..3];
        assert_eq!(rowq, [q; 3]);
        assert_eq!(
            f64::from(au.avg_q),
            f64::from(tb_m2v_syntax::tables::QSCALE[usize::from(q)])
        );
        assert_eq!(par.want_pred, u8::from(au.pic_type != PIC_B));
        types.push(au.pic_type);
    }
    assert_eq!(types, [PIC_I, PIC_P, PIC_B, PIC_B, PIC_P, PIC_B]);
}

/// A rate change on a fixed-quantizer session takes effect like any other: the buffer is
/// drained at the new rate and the first access unit coded under it carries the ticket (the
/// C++ never applied it: the ticket stayed pending for the life of the session).
#[test]
fn a_rate_change_on_a_fixed_quantizer_session_is_applied() {
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&SessionConfig {
        bitrate: 0,
        max_bitrate: 15_000_000,
        vbv_bits: 1_835_008,
        fixed_q: [5, 7, 9],
        ..f1()
    });
    r.settle();
    r.feed_c(s, 2);
    r.tick();
    let first = r.stream(s);
    assert_eq!((first[0].rate, first[0].rate_ticket), (15_000_000, 0));
    let ticket = r.enc.set_rate(s, 2_000_000, false).unwrap();
    assert_ne!(ticket, 0);
    for _ in 0..4 {
        r.feed_c(s, 2);
        r.tick();
    }
    let aus = r.stream(s);
    let with: Vec<_> = aus.iter().filter(|a| a.rate_ticket == ticket).collect();
    assert_eq!(with.len(), 1, "one access unit carries the ticket");
    assert_eq!(with[0].coded, 1);
    assert!(aus[1..].iter().all(|a| a.rate == 2_000_000));
    let snap = r.enc.snapshot(s).unwrap();
    assert_eq!(snap.pending_rate, None);
    assert_eq!(snap.rc.vbv.rate(), 2_000_000.0);
    // the quantizer is still the fixed one
    assert!(aus.iter().all(|a| a.rung == 0 && a.tries == 0));
}

/// Caption bytes are checked when a frame is submitted: a `cc_data()` that is not legal
/// (CTA-708-E 4.3), or that could emulate a start code (H.262 6.3.4.1), is refused and the
/// frame is not queued.
#[test]
fn illegal_caption_bytes_are_refused_at_submit() {
    use tb_m2v_syntax::captions::CaptionError;
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.settle();
    let good = common::captions();
    let refused = |r: &mut Rig, cc: &[u8]| match r.enc.submit(s, FrameSource::Host, 0, cc) {
        Err(EncoderError::Captions(e)) => e,
        other => panic!("{other:?}"),
    };
    assert_eq!(refused(&mut r, &good[..59]), CaptionError::Length(59));
    assert_eq!(
        refused(&mut r, &good[..6]),
        CaptionError::Count { got: 2, want: 20 }
    );
    assert_eq!(
        refused(&mut r, &[0u8; 60]),
        CaptionError::MarkerBits { index: 0, byte: 0 }
    );
    let mut late = good.clone();
    late[30] = 0xFC;
    assert_eq!(refused(&mut r, &late), CaptionError::Cea608AfterDtvcc(10));
    // nothing was queued by the refused calls; the legal bytes go through and into the stream
    assert_eq!(r.enc.snapshot(s).unwrap().n_in, 0);
    r.enc.submit(s, FrameSource::Host, 0, &good).unwrap();
    r.tick();
    let aus = r.stream(s);
    let at = aus[0]
        .data
        .windows(4)
        .position(|w| w == [0, 0, 1, 0xB2])
        .unwrap();
    assert_eq!(
        aus[0].data[at + 4..at + 11],
        [0x47, 0x41, 0x39, 0x34, 3, 0xD4, 0xFF]
    );
    assert_eq!(aus[0].data[at + 11..at + 71], good[..]);
    assert_eq!(aus[0].data[at + 71], 0xFF);
}

/// `keep_recon`: the reconstruction of slot 0 comes back with every picture, cropped to the
/// picture size; a B picture is reconstructed into its own buffer for it.
#[test]
fn the_reconstruction_of_slot_0_is_kept() {
    let opt = EncoderOptions {
        keep_recon: true,
        ..EncoderOptions::default()
    };
    let mut r = Rig::new(opt);
    let (s0, s1) = (r.open(&f2()), r.open(&f2()));
    r.settle();
    r.run(10);
    let lay = r.enc.batch_layout(s0).unwrap().0.clone();
    let brec = lay.d_brec.unwrap();
    for a in r.stream(s0) {
        assert_eq!(a.recon.len(), 72 * 40 * 3 / 2, "picture {}", a.coded);
    }
    assert!(r.stream(s1).iter().all(|a| a.recon.is_empty()));
    let mut b_pictures = 0;
    for t in &r.ticks {
        let Some((_, au)) = t.aus.iter().find(|(s, _)| *s == s0.0) else {
            continue;
        };
        let last = t.plans.last().unwrap();
        let down: Vec<_> = last
            .steps
            .iter()
            .filter_map(|x| match x.op {
                Op::Download { dst, src } => Some((dst, src)),
                _ => None,
            })
            .collect();
        // luma, two chroma planes, then the coded pictures
        assert_eq!(down.len(), 4);
        let h = lay.h_recon.unwrap();
        assert_eq!(down[0].0.off, h.off);
        assert_eq!(down[2].0.end(), h.end());
        assert_eq!(down[3].0.off, lay.h_out.off);
        let from_brec = down[0].1.off == brec[0].off;
        assert_eq!(from_brec, au.pic_type == PIC_B);
        if from_brec {
            b_pictures += 1;
            let k = kernels(last);
            assert_eq!(
                k.iter().filter(|n| **n == "k_recon").count(),
                1 + usize::from(k.contains(&"k_pyr"))
            );
        }
    }
    assert!(b_pictures >= 3);
}

/// A tick started while another is in progress abandons the first as if its plan had failed.
#[test]
fn a_tick_begun_during_a_tick_abandons_the_first() {
    let mut r = Rig::new(EncoderOptions::default());
    let s = r.open(&f1());
    r.settle();
    for _ in 0..5 {
        r.feed(s, 1);
        r.tick();
    }
    r.feed(s, 1);
    let mut plan = Plan::new();
    let now = r.dev.now();
    assert_eq!(
        r.enc.begin_tick(now, None, r.dev.host_mut(), &mut plan),
        Step::Run
    );
    assert_eq!(r.enc.phase(), Some(Phase::Begin));
    // the driver forgets the plan and starts over
    for _ in 5..12 {
        if r.n[&s.0] < 12 && r.ticks.len() > 5 {
            r.feed(s, 1);
        }
        r.tick();
    }
    r.drain();
    let aus = r.stream(s);
    check_stream(&aus, r.n[&s.0]);
    assert_eq!(r.enc.snapshot(s).unwrap().stats.failed, 1);
    assert_eq!(aus.iter().filter(|a| a.repeat).count(), 1);
}
