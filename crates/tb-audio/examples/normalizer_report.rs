//! How the normalizer and limiter behave over time:
//!
//! ```text
//! nix develop -c cargo run --release -p tb-audio --example normalizer_report
//! ```
//!
//! Program-like noise through a stereo [`TrackPipeline`]: a start at each M9 level, then
//! level changes (quiet to loud, loud to quiet, a long pause). Prints the output's short-term
//! loudness (3 s) once a second, the time until the gain stays within 2 dB and 1 dB of where
//! it ends, and the true peak.
#![allow(clippy::print_stdout, reason = "the example's report")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "an example")]

use tb_audio::clock::AudioFrame;
use tb_audio::layout::{InputLayout, OutputLayout};
use tb_audio::loudness::LoudnessMeter;
use tb_audio::pipeline::{InputFormat, TrackConfig, TrackPipeline};
use tb_audio::resample::InputRate;
use tb_audio::signal::program;
use tb_timeline::{Source, Ticks27};

fn at_level(lufs: f64, secs: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut x = program(seed, 2, 48_000 * secs);
    let mut m = LoudnessMeter::new(OutputLayout::Stereo, false);
    m.push(&[&x[0], &x[1]]).unwrap();
    let g = 10f32.powf(((lufs - m.integrated().unwrap().0) / 20.0) as f32);
    for p in &mut x {
        for v in p.iter_mut() {
            *v *= g;
        }
    }
    x
}

fn scenario(name: &str, parts: &[(f64, usize)], change_at: usize) {
    let mut x = [Vec::new(), Vec::new()];
    for (i, &(lufs, secs)) in parts.iter().enumerate() {
        let p = if lufs.is_finite() {
            at_level(lufs, secs, 10 + i as u64)
        } else {
            vec![vec![0.0; 48_000 * secs]; 2]
        };
        x[0].extend_from_slice(&p[0]);
        x[1].extend_from_slice(&p[1]);
    }
    let mut t = TrackPipeline::new(TrackConfig::new(Ticks27(0), OutputLayout::Stereo)).unwrap();
    t.open(
        Source::Live,
        InputFormat {
            rate: InputRate::HZ_48000,
            layout: InputLayout::default_for(2).unwrap(),
        },
    )
    .unwrap();
    t.switch(Source::Live, Ticks27(0));
    let mut f = AudioFrame::new(OutputLayout::Stereo);
    let mut m = LoudnessMeter::new(OutputLayout::Stereo, true);
    let mut line = String::new();
    let mut gains: Vec<(usize, f64)> = Vec::new();
    let mut last_out = usize::MAX;
    let mut produced = 0usize;
    let n = x[0].len();
    for s in (0..n).step_by(1536) {
        let e = (s + 1536).min(n);
        // Due 0 s after arrival: frame k carries input sample 1536·k (no jitter buffer here).
        t.push(
            Source::Live,
            Ticks27(s as i64 * 1125 / 2),
            &[&x[0][s..e], &x[1][s..e]],
        )
        .unwrap();
        // Keep half a second buffered.
        while produced + 1536 + 24_000 <= e {
            t.next_frame(&mut f);
            m.push(&[f.plane(0), f.plane(1)]).unwrap();
            produced += 1536;
            let sec = produced / 48_000;
            if sec != last_out && produced % 48_000 < 1536 {
                last_out = sec;
                let st = m.short_term().map_or(f64::NAN, |l| l.0);
                if sec >= change_at.saturating_sub(2) && sec <= change_at + 20 {
                    line += &format!(" {st:.1}");
                }
                gains.push((sec, t.gain_db(Source::Live).unwrap()));
            }
        }
    }
    println!("{name}");
    println!("  short-term LUFS, 1 s apart, from 2 s before the change:{line}");
    let last = t.gain_db(Source::Live).unwrap();
    let settle = |tol: f64| {
        gains
            .iter()
            .rev()
            .find(|(_, g)| (g - last).abs() > tol)
            .map_or(0, |(s, _)| s + 1)
            .saturating_sub(change_at)
    };
    println!(
        "  gain within 2 dB / 1 dB of its final {last:.2} dB from {} s / {} s after the change; \
         true peak overall {:.2} dBTP",
        settle(2.0),
        settle(1.0),
        m.true_peak().unwrap().0,
    );
}

fn main() {
    scenario("start at −10 LUFS", &[(-10.0, 30)], 0);
    scenario("start at −24 LUFS", &[(-24.0, 30)], 0);
    scenario("start at −35 LUFS", &[(-35.0, 30)], 0);
    scenario(
        "−35 then −10 LUFS (quiet to loud)",
        &[(-35.0, 40), (-10.0, 40)],
        40,
    );
    scenario(
        "−10 then −35 LUFS (loud to quiet)",
        &[(-10.0, 40), (-35.0, 60)],
        40,
    );
    scenario(
        "−24, 60 s of digital silence, −24 (gain held)",
        &[(-24.0, 30), (f64::NAN, 60), (-24.0, 30)],
        90,
    );
}
