//! Film mode on the model device: frames shown for two and three fields (or one, two and three
//! frame periods) in any order, with holds, forced key frames, pictures given up by the
//! deadline policy, failed plans and rate changes in between. Whatever is submitted, the
//! access units must describe a legal display: every display unit covered exactly once, in
//! order; output fields alternating (H.262 7.12); temporal_reference and the GOP time code on
//! the display; PTS and DTS exact, with the decoding intervals of H.262 Annex C.9 and C.11;
//! and the buffer model without a violation.
//!
//! The checks read the flags back from the bytes of the access units and derive every time
//! from them with their own arithmetic: nothing is taken from the encoder's state.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use common::model::Model;
use tb_device::sim::Fault;
use tb_device::{ArenaSizes, Device, DeviceError, Nanos, Plan, StreamId, Ticket};
use tb_kernel_abi::{PIC_B, PIC_I};
use tb_m2v_encoder::{
    AccessUnit, Encoder, EncoderError, EncoderOptions, FrameSource, SessionConfig, SessionId,
    Shown, ShownError, Step,
};
use tb_m2v_syntax::headers::TimeCode;
use tb_m2v_syntax::profile::Profile;

const ARENAS: ArenaSizes = ArenaSizes {
    device: 8 << 20,
    host: 1 << 20,
};

/// 64x48 interlaced at 29.97 Hz, key frames 15 frame periods apart.
fn interlaced() -> SessionConfig {
    SessionConfig {
        w: 64,
        h: 48,
        interlaced: true,
        gop: 15,
        m: 3,
        max_bitrate: 600_000,
        vbv_bits: 196_608,
        bitrate: 400_000,
        capacity: 2,
        profile: Profile::H262,
        ..SessionConfig::default()
    }
}

/// 64x48 progressive at 59.94 Hz, key frames 30 frame periods apart.
fn progressive() -> SessionConfig {
    SessionConfig {
        interlaced: false,
        fps_n: 60000,
        gop: 30,
        ..interlaced()
    }
}

/// SplitMix64.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Rig {
    enc: Encoder,
    dev: Model,
    plan: Plan,
    s: SessionId,
    cfg: SessionConfig,
    /// What was submitted, in display order.
    shown: Vec<Shown>,
    aus: Vec<AccessUnit>,
    pts0: i64,
}

impl Rig {
    fn new(cfg: SessionConfig, opt: EncoderOptions, pts0: i64) -> Self {
        let mut enc = Encoder::new(opt, ARENAS);
        let s = enc.open(&cfg).unwrap();
        let mut r = Self {
            enc,
            dev: Model::new(ARENAS),
            plan: Plan::new(),
            s,
            cfg,
            shown: Vec::new(),
            aus: Vec::new(),
            pts0,
        };
        r.settle();
        r
    }

    /// Wait for the streams, as a driver does after opening sessions.
    fn settle(&mut self) {
        if self.enc.options().overlap {
            let step = self
                .enc
                .settle(StreamId::UPLOAD, self.dev.host_mut(), &mut self.plan);
            self.run(step);
        }
        let step = self
            .enc
            .settle(StreamId::MAIN, self.dev.host_mut(), &mut self.plan);
        self.run(step);
    }

    fn upf(&self) -> u64 {
        if self.cfg.interlaced { 2 } else { 1 }
    }

    /// The exact 90 kHz stamp of display unit `u` of this session, by the definition: the
    /// unit period is `90000 * fps_d / (fps_n * upf)` ticks, the stamp is truncated.
    fn stamp(&self, u: i64) -> i64 {
        let num = 90_000 * i128::from(self.cfg.fps_d);
        let den = i128::from(self.cfg.fps_n) * i128::from(self.upf());
        // pts0 is on the grid in these tests: unit0 = pts0 / unit period
        let unit0 = i128::from(self.pts0) * den / num;
        assert_eq!(unit0 * num / den, i128::from(self.pts0), "pts0 on the grid");
        ((unit0 + i128::from(u)) * num / den) as i64
    }

    fn run(&mut self, mut step: Step) {
        let mut ticket = Ticket(u64::MAX);
        loop {
            match step {
                Step::Done => return,
                Step::Run => ticket = self.dev.submit(&self.plan).expect("a valid plan"),
                Step::Queue => panic!("split plans are off"),
                Step::Wait => {}
            }
            let done = self.dev.wait(ticket, Some(Nanos(2_000_000)));
            let now = self.dev.now();
            step = self
                .enc
                .resume(&done, now, self.dev.host_mut(), &mut self.plan);
        }
    }

    fn tick(&mut self) {
        let now = self.dev.now();
        let step = self
            .enc
            .begin_tick(now, None, self.dev.host_mut(), &mut self.plan);
        self.run(step);
        while let Some(au) = self.enc.take_output(self.s) {
            self.aus.push(au);
        }
    }

    /// Submit a frame shown as `shown`; `again`: the previous frame's content.
    fn feed(&mut self, shown: Shown, again: bool, cc: &[u8]) -> Result<(), EncoderError> {
        let start = self.enc.display_units(self.s).unwrap();
        let pts = self.stamp(start as i64);
        let source = if again {
            FrameSource::Again
        } else {
            let buf = self.enc.frame_buffer(self.s).unwrap();
            let c = 20 + (self.shown.len() * 7 % 60) as u8;
            buf.of_mut(self.dev.host_mut()).unwrap().fill(c);
            FrameSource::Host
        };
        self.enc.submit_shown(self.s, source, pts, cc, shown)?;
        self.shown.push(shown);
        Ok(())
    }

    /// A progressive frame of `units`, on the parity the session is at.
    fn film(&mut self, units: u8) {
        let top_first = self.cfg.interlaced && self.enc.next_top_first(self.s).unwrap();
        self.feed(
            Shown {
                units,
                top_first,
                progressive: true,
            },
            false,
            &[],
        )
        .unwrap();
    }

    fn finish(&mut self) {
        self.enc.flush(self.s).unwrap();
        for _ in 0..64 {
            if self.enc.idle() {
                return;
            }
            self.tick();
        }
        panic!("the encoder does not become idle");
    }
}

/// What the bytes of an access unit say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Flags {
    seq: bool,
    /// The GOP time code, if the access unit has a GOP header: drop_frame, h, m, s, pictures.
    gop: Option<(bool, u32, u32, u32, u32)>,
    tref: u32,
    ptype: u8,
    tff: bool,
    fpfd: bool,
    rff: bool,
    prog: bool,
    cc_count: Option<u8>,
    /// active_format of the picture's afd_data(), if it has one.
    afd: Option<u8>,
}

fn bits(d: &[u8], at: usize, n: usize) -> u32 {
    (at..at + n).fold(0, |v, k| (v << 1) | u32::from(d[k / 8] >> (7 - k % 8) & 1))
}

fn parse(a: &AccessUnit) -> Flags {
    let d = &a.data;
    let mut f = Flags {
        seq: false,
        gop: None,
        tref: 0,
        ptype: 0,
        tff: false,
        fpfd: false,
        rff: false,
        prog: false,
        cc_count: None,
        afd: None,
    };
    let mut i = 0;
    let mut in_pic = false;
    while i + 4 <= d.len() {
        if d[i..i + 3] != [0, 0, 1] {
            i += 1;
            continue;
        }
        let p = &d[i + 4..];
        match d[i + 3] {
            0xB3 => f.seq = true,
            0xB8 => {
                f.gop = Some((
                    bits(p, 0, 1) == 1,
                    bits(p, 1, 5),
                    bits(p, 6, 6),
                    bits(p, 13, 6),
                    bits(p, 19, 6),
                ));
            }
            0x00 => {
                f.tref = bits(p, 0, 10);
                f.ptype = bits(p, 10, 3) as u8;
                in_pic = true;
            }
            0xB5 if in_pic && p[0] >> 4 == 8 => {
                f.tff = bits(p, 24, 1) == 1;
                f.fpfd = bits(p, 25, 1) == 1;
                f.rff = bits(p, 30, 1) == 1;
                f.prog = bits(p, 32, 1) == 1;
                assert_eq!(bits(p, 31, 1) == 1, f.prog, "chroma_420_type");
                assert_eq!(bits(p, 22, 2), 3, "frame picture");
            }
            0xB2 if &p[..4] == b"GA94" && p[4] == 3 => f.cc_count = Some(p[5] & 31),
            0xB2 if &p[..4] == b"DTG1" => {
                assert_eq!(p[4], 0x41, "afd_data(): zero, flag set, reserved 00 0001");
                assert_eq!(p[5] >> 4, 0xF, "afd_data(): reserved 1111");
                f.afd = Some(p[5] & 0x0F);
            }
            // the first slice: the headers are over
            0x01 => break,
            _ => {}
        }
        i += 4;
    }
    assert!(in_pic, "an access unit has a picture header");
    f
}

/// Display units of a picture by its flags (H.262 6.3.10).
fn units_of(f: &Flags, interlaced: bool) -> u8 {
    match (interlaced, f.rff, f.tff) {
        (true, false, _) => 2,
        (true, true, _) => 3,
        (false, false, _) => 1,
        (false, true, false) => 2,
        (false, true, true) => 3,
    }
}

/// Every check of the module documentation on the access units of a finished run.
/// `clean`: no picture was held, given up or forced, so the key frames are on the grid.
fn check(r: &Rig, clean: bool) {
    let il = r.cfg.interlaced;
    let upf = r.upf() as i64;
    let flags: Vec<Flags> = r.aus.iter().map(parse).collect();
    assert_eq!(r.aus.len(), r.shown.len(), "one access unit per frame");
    // ---- coded order: indices, decoding times by Annex C.11 / C.9 from the flags alone
    let mut dts = -upf;
    let mut prev_anchor = upf;
    for (k, (a, f)) in r.aus.iter().zip(&flags).enumerate() {
        assert_eq!(a.coded, k as u64);
        assert_eq!(a.pic_type, f.ptype);
        let u = units_of(f, il);
        assert_eq!(
            (a.units, il && a.top_first),
            (u, il && f.tff),
            "picture {k}"
        );
        if f.rff && il {
            // three fields only of a progressive frame coded without field tools
            assert!(f.prog && f.fpfd, "picture {k}: repeat_first_field flags");
        }
        if !il {
            assert!(f.prog && f.fpfd && (f.rff || !f.tff), "picture {k}");
        }
        assert_eq!(a.dts, r.stamp(dts), "DTS of picture {k}");
        if f.ptype == PIC_B {
            assert_eq!(a.pts, a.dts, "a B picture is shown when it is decoded");
            dts += i64::from(u);
        } else {
            assert!(a.pts > a.dts, "picture {k}");
            dts += prev_anchor;
            prev_anchor = i64::from(u);
        }
        assert!(a.room_bits >= 0.0 && a.queue_bits >= 0.0, "picture {k}");
        assert_eq!(a.keyframe, f.seq && f.gop.is_some() && f.ptype == PIC_I);
        assert_eq!(
            f.seq,
            f.gop.is_some(),
            "sequence and GOP headers go together"
        );
    }
    // ---- display order: a B picture at once, an anchor when the next one is decoded
    let mut order: Vec<usize> = Vec::new();
    let mut held = None;
    for (k, f) in flags.iter().enumerate() {
        if f.ptype == PIC_B {
            order.push(k);
        } else {
            order.extend(held.replace(k));
        }
    }
    order.extend(held);
    let mut start = 0i64;
    let mut last_top: Option<bool> = None;
    let mut tref = 0;
    let frc = tb_m2v_syntax::profile::frame_rate_code(r.cfg.fps_n, r.cfg.fps_d).unwrap();
    let grid = i64::from(r.cfg.gop) * upf;
    let mut keys = Vec::new();
    for (n, &k) in order.iter().enumerate() {
        let (a, f) = (&r.aus[k], &flags[k]);
        let want = r.shown[n];
        assert_eq!(a.disp, n as u64, "display index");
        // every display unit exactly once, as submitted
        assert_eq!(a.units, want.units, "frame {n}: duration");
        if il {
            assert_eq!(f.tff, want.top_first, "frame {n}: first field");
            assert_ne!(last_top, Some(f.tff), "frame {n}: output fields alternate");
            last_top = Some(f.tff == (a.units == 3));
        }
        assert_eq!(a.pts, r.stamp(start), "PTS of frame {n}");
        // temporal_reference counts frames, zero after a GOP header
        if f.gop.is_some() {
            tref = 0;
            let tc = TimeCode::of((start / upf) as u64, frc);
            assert_eq!(
                f.gop,
                Some((tc.drop_frame, tc.hours, tc.minutes, tc.seconds, tc.pictures)),
                "frame {n}: the time code is its display time"
            );
            keys.push(start);
        }
        assert_eq!(f.tref, tref, "frame {n}: temporal_reference");
        tref = (tref + 1) % 1024;
        start += i64::from(a.units);
    }
    assert_eq!(
        start,
        r.shown.iter().map(|s| i64::from(s.units)).sum::<i64>(),
        "the display time is what was submitted"
    );
    // the last frame's anchor chain: PTS of an anchor is the DTS of the next anchor
    let anchors: Vec<usize> = (0..flags.len())
        .filter(|k| flags[*k].ptype != PIC_B)
        .collect();
    for w in anchors.windows(2) {
        assert_eq!(r.aus[w[0]].pts, r.aus[w[1]].dts, "anchor {} is shown", w[0]);
    }
    // ---- the key frame grid is in display time
    if clean {
        let mut g = 0;
        for k in &keys {
            assert!(
                *k >= g && *k < g + 3,
                "key frame at display unit {k}, grid time {g}"
            );
            g += grid;
        }
        assert!(start <= g + 3, "no grid time without its key frame");
    }
    let snap = r.enc.snapshot(r.s).unwrap();
    assert_eq!(snap.stats.rate.violations, 0, "buffer model");
    assert_eq!(snap.stats.pts_mismatch, 0);
    let by: u64 = (1..4).map(|u| u * snap.stats.shown[u as usize]).sum();
    assert_eq!(by as i64, start, "the statistics count every display unit");
}

#[test]
fn three_two_pulldown_on_an_interlaced_session() {
    let mut r = Rig::new(interlaced(), EncoderOptions::default(), 0);
    for n in 0..60 {
        r.film(if n % 2 == 0 { 3 } else { 2 });
        r.tick();
    }
    r.finish();
    check(&r, true);
    // 60 film frames are 150 fields: 75 frame periods, five GOPs of 15
    let keys = r.aus.iter().filter(|a| a.keyframe).count();
    assert_eq!(keys, 5);
    let shown = r.enc.snapshot(r.s).unwrap().stats.shown;
    assert_eq!(shown, [0, 0, 30, 30]);
    // the cadence of the first GOP: 12 pictures, T3 B2 B3 T2
    let first: Vec<(u8, bool)> = r.shown[..4]
        .iter()
        .map(|s| (s.units, s.top_first))
        .collect();
    assert_eq!(first, [(3, true), (2, false), (3, false), (2, true)]);
}

#[test]
fn frame_repeats_on_a_progressive_session() {
    let mut r = Rig::new(progressive(), EncoderOptions::default(), 0);
    for n in 0..48 {
        r.film(if n % 2 == 0 { 3 } else { 2 });
        r.tick();
    }
    r.finish();
    check(&r, true);
    // 48 film frames are 120 frame periods: four GOPs of 30
    assert_eq!(r.aus.iter().filter(|a| a.keyframe).count(), 4);
    assert_eq!(r.enc.snapshot(r.s).unwrap().stats.shown, [0, 0, 24, 24]);
}

/// A session that never repeats a field is what it always was: the same access units as
/// `submit` gives.
#[test]
fn ordinary_frames_through_submit_shown_equal_submit() {
    let run = |shown: bool| {
        let mut r = Rig::new(interlaced(), EncoderOptions::default(), 0);
        for n in 0..40u64 {
            if shown {
                let s = r.enc.ordinary(r.s).unwrap();
                r.feed(s, false, &[]).unwrap();
            } else {
                let buf = r.enc.frame_buffer(r.s).unwrap();
                buf.of_mut(r.dev.host_mut())
                    .unwrap()
                    .fill(20 + (n * 7 % 60) as u8);
                r.enc
                    .submit(r.s, FrameSource::Host, n as i64 * 3003, &[])
                    .unwrap();
                let s = r.enc.ordinary(r.s).unwrap();
                r.shown.push(s);
            }
            r.tick();
        }
        r.finish();
        check(&r, true);
        r.aus
    };
    assert_eq!(run(true), run(false));
}

/// Random mixes of everything a timeline can hand over.
fn random_run(cfg: SessionConfig, seed: u64, ticks: u64) {
    let mut rng = Rng(seed);
    let opt = EncoderOptions {
        overlap: seed.is_multiple_of(3),
        // some runs give pictures up at random (the deadline policy's test hook)
        force_shed: (seed % 4 == 1).then_some((0.15, seed)),
        ..EncoderOptions::default()
    };
    let mut r = Rig::new(cfg, opt, 0);
    let il = cfg.interlaced;
    let mut clean = opt.force_shed.is_none();
    let mut held = false;
    // the kind of content the source is in for a while: film, video, a still
    let mut mode = 0;
    for t in 0..ticks {
        if rng.below(12) == 0 {
            mode = rng.below(4);
        }
        // session commands between ticks
        match rng.below(40) {
            0 => {
                held = !held;
                r.enc.hold(r.s, held).unwrap();
                clean = false;
            }
            1 => {
                r.enc.force_keyframe(r.s).unwrap();
                clean = false;
            }
            2 => {
                let rate = 250_000 + rng.below(300_000);
                r.enc.set_rate(r.s, rate, rng.below(2) == 0).unwrap();
            }
            3 if !opt.overlap => {
                // the next plan fails: its pictures become repeats, the session re-keys
                let submit = r.dev.submits;
                r.dev.faults.push(Fault::Fail {
                    submit,
                    error: DeviceError::Failed { step: 0, code: 2 },
                });
                clean = false;
            }
            _ => {}
        }
        let top_first = il && r.enc.next_top_first(r.s).unwrap();
        let again = !r.shown.is_empty() && rng.below(6) == 0;
        let shown = match mode {
            // film: three and two, sometimes out of step (a cut)
            0 => Some(Shown {
                units: if rng.below(8) == 0 {
                    2 + rng.below(2) as u8
                } else if t % 2 == 0 {
                    3
                } else {
                    2
                },
                top_first,
                progressive: true,
            }),
            // video: one frame period each, two instants in an interlaced session
            1 => Some(Shown {
                units: if il { 2 } else { 1 },
                top_first,
                progressive: !il,
            }),
            // a still: three units at a time, the frame before again
            2 => Some(Shown {
                units: 3,
                top_first,
                progressive: true,
            }),
            // anything legal, or no picture in this tick
            _ => (rng.below(3) != 0).then(|| {
                let units = if il {
                    2 + rng.below(2) as u8
                } else {
                    1 + rng.below(3) as u8
                };
                Shown {
                    units,
                    top_first,
                    progressive: !il || units == 3 || rng.below(2) == 0,
                }
            }),
        };
        if let Some(shown) = shown {
            r.feed(shown, again, &[]).unwrap();
        }
        r.tick();
    }
    r.enc.hold(r.s, false).unwrap();
    r.finish();
    check(&r, clean);
    assert!(r.aus.len() > ticks as usize / 2);
}

#[test]
fn random_cadences_interlaced() {
    for seed in 0..48 {
        random_run(interlaced(), seed, 160);
    }
}

#[test]
fn random_cadences_progressive() {
    for seed in 100..148 {
        random_run(progressive(), seed, 160);
    }
}

#[test]
fn random_cadences_without_b_pictures() {
    for seed in 200..216 {
        random_run(
            SessionConfig {
                m: 1,
                ..interlaced()
            },
            seed,
            120,
        );
        random_run(
            SessionConfig {
                m: 2,
                gop: 12,
                ..progressive()
            },
            seed,
            120,
        );
    }
}

/// The whole chain without I/O: selections per display unit (film at 24 and near 24 Hz, video,
/// stills, cuts, drops and repeats, a source that goes away) through the timeline's assembler
/// into the encoder, two units a tick on an interlaced session and one on a progressive one.
/// Whatever the selections, every unit is displayed exactly once, in order.
#[test]
fn selections_through_the_assembler_cover_every_display_unit() {
    use tb_timeline::film::{Assembler, FilmCfg, Key, Unit};
    for seed in 0..40u64 {
        let il = seed % 2 == 0;
        let cfg = if il { interlaced() } else { progressive() };
        let mut rng = Rng(1000 + seed);
        let mut r = Rig::new(cfg, EncoderOptions::default(), 0);
        let mut asm: Assembler<()> = Assembler::new(FilmCfg {
            interlaced: il,
            top_first: il,
            three: seed % 5 != 4,
        });
        let per = if il { 2 } else { 1 };
        let (mut idx, mut mode, mut phase) = (0i64, 0u64, 0u64);
        let mut pushed = 0u64;
        let feed = |r: &mut Rig, p: tb_timeline::film::Picture<()>| {
            assert_eq!(p.start, r.enc.display_units(r.s).unwrap());
            let shown = Shown {
                units: p.units,
                top_first: p.top_first,
                progressive: p.progressive,
            };
            // the instant of the picture before: the encoder's previous frame again
            r.feed(shown, p.again, &[]).unwrap();
        };
        for t in 0..220u64 {
            if rng.below(15) == 0 {
                mode = rng.below(5);
                idx += 10;
            }
            for _ in 0..per {
                let key = match mode {
                    // film: two frames in five units
                    0 | 1 => {
                        phase += 2;
                        Key::Instant {
                            src: 0,
                            idx: idx + (phase / 5) as i64,
                        }
                    }
                    2 => Key::Unique,
                    3 => Key::Instant { src: 9, idx: 0 },
                    _ => {
                        idx += [0, 1, 1, 2][rng.below(4) as usize];
                        Key::Instant { src: 0, idx }
                    }
                };
                asm.push(Unit { key, data: () });
                pushed += 1;
            }
            if let Some(p) = asm.pull() {
                feed(&mut r, p);
            }
            assert!(asm.queued() <= 4, "seed {seed}, tick {t}");
            r.tick();
        }
        while let Some(p) = asm.flush() {
            feed(&mut r, p);
            r.tick();
        }
        r.finish();
        check(&r, true);
        let shown: u64 = r.aus.iter().map(|a| u64::from(a.units)).sum();
        assert_eq!(shown, asm.stats.units_out);
        assert!(shown == pushed || shown == pushed + 1, "seed {seed}");
        if asm.cfg().three && il {
            assert!(
                r.aus.iter().any(|a| a.units == 3),
                "seed {seed}: no film picture"
            );
        }
    }
}

/// What the encoder refuses, with the clause, leaving the session as it was.
#[test]
fn illegal_displays_are_refused() {
    let mut r = Rig::new(interlaced(), EncoderOptions::default(), 0);
    let s = |units, top_first, progressive| Shown {
        units,
        top_first,
        progressive,
    };
    let err = |r: &mut Rig, shown| match r.feed(shown, false, &[]) {
        Err(EncoderError::Shown(e)) => e,
        other => panic!("{other:?}"),
    };
    r.feed(s(3, true, true), false, &[]).unwrap();
    // top, bottom, top: the next frame starts with the bottom field
    let e = err(&mut r, s(2, true, true));
    assert_eq!(e, ShownError::Parity { got: "top" });
    assert!(e.to_string().contains("H.262 §7.12"), "{e}");
    assert_eq!(
        r.enc.submit(r.s, FrameSource::Again, 0, &[]),
        Err(EncoderError::Shown(ShownError::Parity { got: "top" })),
        "an ordinary frame in the session's field order does not fit either"
    );
    let e = err(&mut r, s(3, false, false));
    assert_eq!(e, ShownError::NotProgressive);
    assert!(e.to_string().contains("§6.3.10"), "{e}");
    for units in [0, 1, 4] {
        assert!(matches!(
            err(&mut r, s(units, false, true)),
            ShownError::Units { .. }
        ));
    }
    assert_eq!(r.enc.display_units(r.s).unwrap(), 3, "nothing was accepted");
    r.tick();
    r.feed(s(2, false, false), false, &[]).unwrap();
    r.tick();
    r.finish();
    check(&r, true);

    // Table 8-7: no repeat_first_field in a progressive sequence below 59.94 Hz, nor at 24 Hz
    for (fps_n, il) in [(30000, false), (24000, false), (24000, true)] {
        let cfg = SessionConfig {
            fps_n,
            interlaced: il,
            ..interlaced()
        };
        let mut r = Rig::new(cfg, EncoderOptions::default(), 0);
        let e = err(&mut r, s(3, il, true));
        assert!(matches!(e, ShownError::NotAllowed { .. }), "{e:?}");
        assert!(e.to_string().contains("Table 8-7"), "{e}");
        // one frame period is fine
        r.feed(s(if il { 2 } else { 1 }, il, true), false, &[])
            .unwrap();
    }
    // 59.94 Hz progressive: one, two and three frame periods
    let mut r = Rig::new(progressive(), EncoderOptions::default(), 0);
    for units in [1, 2, 3] {
        r.feed(s(units, false, true), false, &[]).unwrap();
        r.tick();
    }
    r.finish();
    check(&r, true);
}

/// Captions: the cc_count of the picture's display time, and nothing else.
#[test]
fn captions_follow_the_display_time() {
    let cc = |n: usize, lead: &[u8]| {
        let mut v = Vec::new();
        for ty in lead {
            v.extend([0xFC | ty, 0x80, 0x80]);
        }
        while v.len() < 3 * n {
            v.extend([0xFA, 0, 0]);
        }
        v
    };
    let mut r = Rig::new(interlaced(), EncoderOptions::default(), 0);
    let s = |units, top_first| Shown {
        units,
        top_first,
        progressive: true,
    };
    r.feed(s(3, true), false, &cc(30, &[1, 0, 1])).unwrap();
    r.tick();
    assert!(matches!(
        r.feed(s(2, false), false, &cc(30, &[0, 1, 0])),
        Err(EncoderError::Captions(_))
    ));
    r.feed(s(2, false), false, &cc(20, &[0, 1])).unwrap();
    r.tick();
    assert!(matches!(
        r.feed(s(3, false), false, &cc(20, &[0, 1])),
        Err(EncoderError::Captions(_))
    ));
    r.feed(s(3, false), false, &cc(30, &[0, 1, 0])).unwrap();
    r.tick();
    r.finish();
    check(&r, true);
    let counts: Vec<Option<u8>> = r.aus.iter().map(|a| parse(a).cc_count).collect();
    assert_eq!(counts.len(), 3);
    let mut by_disp: Vec<(u64, Option<u8>)> = r.aus.iter().map(|a| a.disp).zip(counts).collect();
    by_disp.sort_unstable();
    assert_eq!(by_disp, [(0, Some(30)), (1, Some(20)), (2, Some(30))]);

    let mut r = Rig::new(progressive(), EncoderOptions::default(), 0);
    for (units, n) in [(1, 10), (2, 20), (3, 30)] {
        let lead: Vec<u8> = (0..units).map(|k| k % 2).collect();
        r.feed(s(units, false), false, &cc(n, &lead)).unwrap();
        r.tick();
    }
    r.finish();
    check(&r, true);
}

/// Hours of film across the wrap of the 33-bit time stamps: the stamps are exact to the last
/// picture, and only their 33-bit form wraps.
#[test]
fn time_stamps_are_exact_across_the_33_bit_wrap() {
    // 3003 ticks a frame period: start 40 frame periods before 2^33
    let wrap = 1i64 << 33;
    let pts0 = (wrap / 3003 - 40) * 3003;
    let mut r = Rig::new(interlaced(), EncoderOptions::default(), pts0);
    for n in 0..120 {
        r.film(if n % 2 == 0 { 3 } else { 2 });
        r.tick();
    }
    r.finish();
    check(&r, true);
    assert!(r.aus[0].pts < wrap && r.aus.last().unwrap().pts > wrap);
    for a in &r.aus {
        assert_eq!(a.pts33(), (a.pts % wrap) as u64);
        assert_eq!(a.dts33(), (a.dts.rem_euclid(wrap)) as u64);
        assert!(a.pts33() < 1 << 33);
    }
    // a long run: no drift between the display time and the stamps after a million pictures
    // (the stamp is a function of the display unit, not a sum)
    let last = 2_500_000i64;
    assert_eq!(r.stamp(last), pts0 + last * 3003 / 2);
    // a session that starts on PTS 0 has the one negative stamp: the first DTS
    let mut z = Rig::new(progressive(), EncoderOptions::default(), 0);
    z.film(3);
    z.tick();
    z.finish();
    assert_eq!((z.aus[0].pts, z.aus[0].dts), (0, -1501));
    assert_eq!(z.aus[0].dts33(), (wrap - 1501) as u64);
}

/// A session restored from a snapshot in another encoder goes on in display time: parity,
/// stamps, the grid.
#[test]
fn a_restored_session_continues_the_display() {
    let mut r = Rig::new(interlaced(), EncoderOptions::default(), 0);
    for n in 0..21 {
        r.film(if n % 2 == 0 { 3 } else { 2 });
        r.tick();
    }
    r.finish();
    let snap = r.enc.snapshot(r.s).unwrap();
    assert!(
        !snap.next_top,
        "21 frames are 53 fields: the next starts on the bottom field"
    );
    let mut n = Rig::new(interlaced(), EncoderOptions::default(), 0);
    // (the rig opened a session of its own; the restored one is the one under test)
    n.s = n.enc.restore(&snap).unwrap();
    n.settle();
    assert_eq!(n.enc.display_units(n.s).unwrap(), snap.start);
    assert!(!n.enc.next_top_first(n.s).unwrap());
    let before = r.aus.len();
    n.shown = r.shown.clone();
    n.aus = r.aus.clone();
    for k in 0..20 {
        n.film(if k % 2 == 0 { 2 } else { 3 });
        n.tick();
    }
    n.finish();
    // one stream: the pictures of both encoders
    let first_new = &n.aus[before];
    assert!(
        first_new.keyframe,
        "a restored session starts with a key frame"
    );
    assert_eq!(first_new.pic_type, PIC_I);
    check(&n, false);
}

/// The Active Format Description set on a session goes into the pictures of the frames
/// submitted after it, whatever their coding order, with the captions before it; `None` stops
/// it.
#[test]
fn active_format_follows_the_submitted_frames() {
    use tb_m2v_syntax::afd::{LETTER_16_9, PILLAR_4_3};
    let want = |n: u64| match n {
        0..10 => Some(LETTER_16_9),
        10..20 => None,
        _ => Some(PILLAR_4_3),
    };
    let mut r = Rig::new(interlaced(), EncoderOptions::default(), 0);
    // a frame's worth at 29.97 Hz: the two CEA-608 constructs and 18 DTVCC padding constructs
    let mut cc = vec![0xFC, 0x80, 0x80, 0xFD, 0x80, 0x80];
    for _ in 0..18 {
        cc.extend_from_slice(&[0xFA, 0, 0]);
    }
    for n in 0..30u64 {
        if n == 0 || n == 10 || n == 20 {
            r.enc.set_active_format(r.s, want(n)).unwrap();
        }
        let s = r.enc.ordinary(r.s).unwrap();
        r.feed(s, false, if n % 2 == 0 { &cc } else { &[] })
            .unwrap();
        r.tick();
    }
    r.finish();
    check(&r, true);
    assert_eq!(r.aus.len(), 30);
    for a in &r.aus {
        let n = (a.pts / 3003) as u64;
        assert_eq!(parse(a).afd, want(n), "frame {n}");
    }
    assert!(matches!(
        r.enc.set_active_format(r.s, Some(0b0100)),
        Err(EncoderError::ActiveFormat(0b0100))
    ));
}
