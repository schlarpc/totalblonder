//! The loudness meter against the EBU Tech 3341 (v4, 2016) minimum requirements, with the test
//! signals synthesized here (the EBU's WAV files are not in the repository; the synthetic
//! cases are the ones defined by a formula, Table 1 cases 1–6, 9 and 12, and the true-peak
//! cases built the same way as cases 15–19: sines at fractions of the sampling rate with a
//! phase that puts the samples below the waveform's peak).
//!
//! Tolerances as in Tech 3341: ±0.1 LU for loudness, +0.2 / −0.4 dB for true peak.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use tb_audio::layout::OutputLayout;
use tb_audio::loudness::LoudnessMeter;
use tb_audio::signal::{db_to_amp, sine};

const FS: u32 = 48_000;

/// Stereo 1 kHz sine segments `(dBFS per channel, seconds)`, phase-continuous.
fn segments(seg: &[(f64, f64)]) -> Vec<f32> {
    let mut out = Vec::new();
    for &(db, secs) in seg {
        let n = (secs * f64::from(FS)).round() as usize;
        let start = out.len();
        let w = 2.0 * std::f64::consts::PI * 1000.0 / f64::from(FS);
        out.extend((0..n).map(|i| (db_to_amp(db) * (w * (start + i) as f64).sin()) as f32));
    }
    out
}

fn stereo_integrated(x: &[f32]) -> f64 {
    let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
    // Push in odd-sized pieces to exercise the step bookkeeping.
    for c in x.chunks(1237) {
        m.push(&[c, c]).unwrap();
    }
    m.integrated().unwrap().lufs()
}

fn near(got: f64, want: f64, tol: f64, what: &str) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: {got:.3}, want {want} ± {tol}"
    );
}

/// Case 1: 1 kHz stereo sine at −23 dBFS, 20 s: M, S and I are −23.0.
#[test]
fn case_1_minus_23() {
    let x = segments(&[(-23.0, 20.0)]);
    let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
    m.push(&[&x, &x]).unwrap();
    near(m.momentary().unwrap().lufs(), -23.0, 0.1, "M");
    near(m.short_term().unwrap().lufs(), -23.0, 0.1, "S");
    near(m.integrated().unwrap().lufs(), -23.0, 0.1, "I");
}

/// Case 2: −33 dBFS, 20 s: −33.0.
#[test]
fn case_2_minus_33() {
    near(
        stereo_integrated(&segments(&[(-33.0, 20.0)])),
        -33.0,
        0.1,
        "I",
    );
}

/// Case 3: −36 / −23 / −36 dBFS for 10 / 60 / 10 s: I = −23.0 (the relative gate).
#[test]
fn case_3_relative_gate() {
    let x = segments(&[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)]);
    near(stereo_integrated(&x), -23.0, 0.1, "I");
}

/// Case 4: −72 / −36 / −23 / −36 / −72 dBFS for 10 / 10 / 60 / 10 / 10 s: I = −23.0 (both
/// gates).
#[test]
fn case_4_both_gates() {
    let x = segments(&[
        (-72.0, 10.0),
        (-36.0, 10.0),
        (-23.0, 60.0),
        (-36.0, 10.0),
        (-72.0, 10.0),
    ]);
    near(stereo_integrated(&x), -23.0, 0.1, "I");
}

/// Case 5: −26 / −20 / −26 dBFS for 20 / 20.1 / 20 s: I = −23.0.
#[test]
fn case_5_level_steps() {
    let x = segments(&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)]);
    near(stereo_integrated(&x), -23.0, 0.1, "I");
}

/// Case 6: 5.0 channels, 1 kHz: L and R at −28, C at −24, Ls and Rs at −30 dBFS, 20 s:
/// I = −23.0 (the surround weight of 1.41).
#[test]
fn case_6_five_channels() {
    let n = 20 * FS as usize;
    let l = sine(1000.0, db_to_amp(-28.0), FS, n, 0.0);
    let c = sine(1000.0, db_to_amp(-24.0), FS, n, 0.0);
    let s = sine(1000.0, db_to_amp(-30.0), FS, n, 0.0);
    let lfe = sine(50.0, 1.0, FS, n, 0.0);
    let mut m = LoudnessMeter::new(OutputLayout::FivePointOne, false);
    // A/52 order: L C R Ls Rs LFE. A full-scale LFE must not count.
    m.push(&[&l, &c, &l, &s, &s, &lfe]).unwrap();
    near(m.integrated().unwrap().lufs(), -23.0, 0.1, "I");
}

/// Case 9: 1.34 s at −20 dBFS and 1.66 s at −30 dBFS, repeated: S = −23.0 at every 100 ms
/// step once the first 3 s are in.
#[test]
fn case_9_short_term() {
    let mut seg = Vec::new();
    for _ in 0..5 {
        seg.push((-20.0, 1.34));
        seg.push((-30.0, 1.66));
    }
    let x = segments(&seg);
    let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
    let step = 4_800;
    for (i, c) in x.chunks(step).enumerate() {
        m.push(&[c, c]).unwrap();
        if i >= 30 && c.len() == step {
            near(m.short_term().unwrap().lufs(), -23.0, 0.1, "S");
        }
    }
}

/// Case 12: 0.18 s at −20 dBFS and 0.22 s at −30 dBFS, repeated: M = −23.0 at every step
/// once the first 400 ms are in.
#[test]
fn case_12_momentary() {
    let mut seg = Vec::new();
    for _ in 0..25 {
        seg.push((-20.0, 0.18));
        seg.push((-30.0, 0.22));
    }
    let x = segments(&seg);
    let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
    for (i, c) in x.chunks(4_800).enumerate() {
        m.push(&[c, c]).unwrap();
        if i >= 4 && c.len() == 4_800 {
            near(m.momentary().unwrap().lufs(), -23.0, 0.1, "M");
        }
    }
}

/// True peak, as cases 15–19 of Tech 3341: a sine at fs/4, fs/6, fs/8 and near 20 kHz with a
/// phase offset that puts every sample below the peak; the meter reads the waveform's peak
/// within +0.2 / −0.4 dB while the sample peak reads up to 3 dB low.
#[test]
fn true_peak_cases() {
    let cases: [(f64, f64, f64); 5] = [
        (12_000.0, 45.0, -6.0),
        (8_000.0, 60.0, -6.0),
        (6_000.0, 67.5, -6.0),
        (12_000.0, 45.0, 0.0),
        (19_997.0, 10.0, -3.0),
    ];
    for (f, phase_deg, peak_db) in cases {
        let n = FS as usize;
        let mut x = sine(f, db_to_amp(peak_db), FS, n, phase_deg.to_radians());
        // A 10 ms raised-cosine onset and end: a hard onset is a step, whose band-limited
        // waveform really does overshoot the sine's peak.
        let ramp = 480;
        for i in 0..ramp {
            let g = (0.5 - 0.5 * (std::f64::consts::PI * i as f64 / ramp as f64).cos()) as f32;
            x[i] *= g;
            x[n - 1 - i] *= g;
        }
        let sample_peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, true);
        m.push(&[&x, &x]).unwrap();
        let tp = m.true_peak().unwrap().0;
        assert!(
            tp <= peak_db + 0.2 && tp >= peak_db - 0.4,
            "{f} Hz at {phase_deg}°: true peak {tp:.3} dBTP, want {peak_db} (+0.2/−0.4); \
             sample peak {:.2} dBFS",
            20.0 * f64::from(sample_peak).log10()
        );
    }
}
