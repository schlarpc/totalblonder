//! The whole track: SPEC M9's loudness criteria, timing through the stages, the trim loop
//! against a producer whose clock is off, layouts.
//!
//! The long versions (10 minutes per input, as M9 states) are `#[ignore]`d because the sandbox
//! runs tests unoptimized; run them with
//! `nix develop -c cargo nextest run --release -p tb-audio --run-ignored all`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use tb_audio::clock::{AudioFrame, FRAME};
use tb_audio::layout::{InputLayout, OutputLayout};
use tb_audio::loudness::{LoudnessMeter, TruePeak};
use tb_audio::pipeline::{InputFormat, TrackConfig, TrackPipeline};
use tb_audio::resample::InputRate;
use tb_audio::signal::{program, sine};
use tb_timeline::{Source, Ticks27};

const P0: Ticks27 = Ticks27(90_000 * 300 * 7);
/// The jitter-buffer target: blocks are due 1 s after "arrival" (SPEC §5.3).
const J: i64 = 27_000_000;

/// A producer: `planes` at `rate`, whose clock runs `ppm` fast relative to ours (its samples
/// are due at `k / (rate · (1 + ppm))` of our seconds), delivered in `block`-sample pieces; the
/// mapper's skew is passed on when `tell_skew`. Returns the output planes and the pipeline.
struct Run {
    out: Vec<Vec<f32>>,
    track: TrackPipeline,
}

fn run(
    planes: &[Vec<f32>],
    rate: u32,
    in_layout: InputLayout,
    out_layout: OutputLayout,
    ppm: f64,
    tell_skew: bool,
) -> Run {
    run_stamped(planes, rate, in_layout, out_layout, ppm, tell_skew, true)
}

/// As [`run`]; `ms_stamps` rounds the stamps to whole milliseconds as RTMP does.
fn run_stamped(
    planes: &[Vec<f32>],
    rate: u32,
    in_layout: InputLayout,
    out_layout: OutputLayout,
    ppm: f64,
    tell_skew: bool,
    ms_stamps: bool,
) -> Run {
    let mut t = TrackPipeline::new(TrackConfig::new(P0, out_layout)).unwrap();
    t.open(
        Source::Live,
        InputFormat {
            rate: InputRate::new(rate).unwrap(),
            layout: in_layout,
        },
    )
    .unwrap();
    t.switch(Source::Live, P0);
    // out = offset + in · (1 + skew): our seconds per producer second.
    let skew = 1.0 / (1.0 + ppm * 1e-6) - 1.0;
    if tell_skew {
        t.set_skew(Source::Live, (skew * 1e9).round() as i64);
    }
    let mut out = vec![Vec::new(); out_layout.count()];
    let mut f = AudioFrame::new(out_layout);
    let n = planes[0].len();
    let mut sent = 0usize;
    let mut j = 0i64;
    let block = 1024;
    while sent < n {
        let k = block.min(n - sent);
        let refs: Vec<&[f32]> = planes.iter().map(|p| &p[sent..sent + k]).collect();
        let due = sent as f64 / (f64::from(rate) * (1.0 + ppm * 1e-6)) * 27e6;
        let due = if ms_stamps {
            (due / 27_000.0).round() * 27_000.0
        } else {
            due
        };
        let at = Ticks27(P0.0 + J + due as i64);
        t.push(Source::Live, at, &refs).unwrap();
        sent += k;
        // Pull the frames that are due once this block has "arrived" (J before it is due).
        while P0.0 + (j + 1) * 864_000 + J <= at.0 {
            t.next_frame(&mut f);
            for (o, p) in out.iter_mut().zip(f.planes().iter()) {
                o.extend_from_slice(p);
            }
            j += 1;
        }
    }
    Run { out, track: t }
}

/// Integrated loudness of `planes` (A/52 order of `layout`) from sample `from`, and the true
/// peak of all of it.
fn measure(planes: &[Vec<f32>], layout: OutputLayout, from: usize) -> (f64, f64) {
    let mut m = LoudnessMeter::new(layout, false);
    let refs: Vec<&[f32]> = planes.iter().map(|p| &p[from..]).collect();
    m.push(&refs).unwrap();
    let mut t = TruePeak::new(planes.len());
    let all: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
    t.push(&all);
    (m.integrated().unwrap().0, t.max().0)
}

/// Integrated loudness of all of `planes`.
fn integrated(planes: &[Vec<f32>], layout: OutputLayout) -> f64 {
    let mut m = LoudnessMeter::new(layout, false);
    let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
    m.push(&refs).unwrap();
    m.integrated().unwrap().0
}

/// Program noise scaled to `lufs` integrated (measured at 48 kHz).
fn program_at(lufs: f64, channels: usize, secs: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut x = program(seed, channels, 48_000 * secs);
    let layout = if channels == 2 {
        OutputLayout::Stereo
    } else {
        OutputLayout::FivePointOne
    };
    let l = integrated(&x, layout);
    let g = 10f32.powf(((lufs - l) / 20.0) as f32);
    for p in &mut x {
        for v in p.iter_mut() {
            *v *= g;
        }
    }
    x
}

/// SPEC M9: inputs at −10, −24 and −35 LKFS come out at −24 ± 2 LKFS, true peak ≤ −2 dBTP.
fn m9(secs: usize, settle: usize) {
    for (lufs, seed) in [(-10.0, 1), (-24.0, 2), (-35.0, 3)] {
        let x = program_at(lufs, 2, secs, seed);
        let lin = integrated(&x, OutputLayout::Stereo);
        let r = run(
            &x,
            48_000,
            InputLayout::default_for(2).unwrap(),
            OutputLayout::Stereo,
            40.0,
            true,
        );
        let (l, tp_all) = measure(&r.out, OutputLayout::Stereo, 48_000 * settle);
        eprintln!(
            "M9 {secs} s: input {lin:.2} LUFS -> output {l:.2} LUFS (from {settle} s), \
             true peak {tp_all:.2} dBTP, gain {:.2} dB",
            r.track.gain_db(Source::Live).unwrap()
        );
        assert!(
            (l + 24.0).abs() <= 2.0,
            "input {lin:.2} LUFS: output {l:.2} LUFS"
        );
        // Our own, tighter expectation for a stationary program.
        assert!(
            (l + 24.0).abs() <= 0.5,
            "input {lin:.2} LUFS: output {l:.2} LUFS"
        );
        assert!(tp_all <= -2.0, "true peak {tp_all:.2} dBTP");
        let c = r.track.source_counters(Source::Live);
        assert_eq!(c.discontinuities, 0);
        assert_eq!(c.late, 0);
    }
}

#[test]
fn m9_loudness_one_minute() {
    m9(60, 20);
}

/// The M9 criterion as written: 10 minutes per input.
#[test]
#[ignore = "10 minutes of audio per level: run with --release --run-ignored all"]
fn m9_loudness_ten_minutes() {
    m9(600, 30);
}

/// A step from a quiet program to a loud one: within a few seconds the output is back near
/// the target, and the limiter holds the peaks meanwhile.
#[test]
fn program_change_from_quiet_to_loud() {
    let mut x = program_at(-35.0, 2, 40, 4);
    let y = program_at(-10.0, 2, 40, 5);
    for (a, b) in x.iter_mut().zip(&y) {
        a.extend_from_slice(b);
    }
    let r = run(
        &x,
        48_000,
        InputLayout::default_for(2).unwrap(),
        OutputLayout::Stereo,
        0.0,
        true,
    );
    // From 5 s after the change.
    let from = 48_000 * 45;
    let (l, tp_all) = measure(&r.out, OutputLayout::Stereo, from);
    assert!((l + 24.0).abs() <= 1.0, "{l:.2}");
    assert!(tp_all <= -2.0, "{tp_all:.2}");
}

/// 44.1 kHz 5.1 input to a stereo track: downmixed, resampled, normalized.
#[test]
fn five_one_at_44_1_to_stereo() {
    let x = program_at(-30.0, 6, 40, 6);
    let r = run(
        &x,
        44_100,
        InputLayout::default_for(6).unwrap(),
        OutputLayout::Stereo,
        -25.0,
        true,
    );
    let (l, tp) = measure(&r.out, OutputLayout::Stereo, 48_000 * 15);
    assert!((l + 24.0).abs() <= 1.0, "{l:.2}");
    assert!(tp <= -2.0);
    assert_eq!(r.track.source_counters(Source::Live).discontinuities, 0);
}

/// Mono 32 kHz into a 5.1 track: the center carries it, the rest is silent.
#[test]
fn mono_to_five_one() {
    let x = vec![sine(440.0, 0.1, 32_000, 32_000 * 10, 0.0)];
    let r = run(
        &x,
        32_000,
        InputLayout::default_for(1).unwrap(),
        OutputLayout::FivePointOne,
        0.0,
        true,
    );
    let e = |p: &[f32]| p[48_000 * 5..].iter().map(|v| v * v).sum::<f32>();
    assert!(e(&r.out[1]) > 1.0);
    for c in [0, 2, 3, 4, 5] {
        assert_eq!(e(&r.out[c]), 0.0, "channel {c}");
    }
}

/// A tone burst at a known input time comes out at the sample its stamp says, through the
/// resampler and the limiter's delay (the pipeline accounts for both).
#[test]
fn timing_through_the_stages() {
    for rate in [44_100u32, 48_000, 96_000] {
        let n = rate as usize * 4;
        let mut x = vec![0.0f32; n];
        let k0 = rate as usize * 2 + 123;
        let len = rate as usize / 100;
        for i in 0..len {
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / len as f64).cos();
            x[k0 + i] = (0.5
                * w
                * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / f64::from(rate)).sin())
                as f32;
        }
        let r = run_stamped(
            &[x.clone(), x],
            rate,
            InputLayout::default_for(2).unwrap(),
            OutputLayout::Stereo,
            0.0,
            true,
            false,
        );
        // Expected center: the burst's center input time, as an output sample.
        let center_in = k0 as f64 + len as f64 / 2.0;
        let expect = (J as f64 / 27e6 + center_in / f64::from(rate)) * 48_000.0;
        // The output's energy centroid.
        let p = &r.out[0];
        let (mut sw, mut s) = (0.0, 0.0);
        for (i, v) in p.iter().enumerate() {
            let e = f64::from(v * v);
            sw += e * i as f64;
            s += e;
        }
        let got = sw / s;
        assert!(
            (got - expect).abs() < 0.5,
            "{rate} Hz: burst at {got:.2}, expected {expect:.2}"
        );
    }
}

/// A producer 300 ppm fast whose skew the pipeline is not told: the drift loop alone keeps
/// the count on the stamps (no discontinuity), at a steady drift of a few ms; told the skew,
/// the drift is near zero.
#[test]
fn trim_loop_follows_a_fast_producer() {
    let x = program_at(-24.0, 2, 60, 7);
    for (tell, bound) in [(false, 400.0), (true, 30.0)] {
        let r = run(
            &x,
            48_000,
            InputLayout::default_for(2).unwrap(),
            OutputLayout::Stereo,
            300.0,
            tell,
        );
        let c = r.track.source_counters(Source::Live);
        assert_eq!(c.discontinuities, 0, "told {tell}");
        let d = r.track.clock().drift(Source::Live).unwrap();
        eprintln!("trim loop, skew told {tell}: drift {d:.1} samples");
        assert!(d.abs() < bound, "told {tell}: drift {d:.1} samples");
        let trim = r.track.trim(Source::Live).unwrap().ppb();
        assert!(
            (trim - 300_000).abs() < 30_000,
            "told {tell}: trim {trim} ppb"
        );
        assert_eq!(r.out[0].len() % FRAME, 0);
    }
}
