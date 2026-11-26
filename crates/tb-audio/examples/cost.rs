//! CPU cost of a track, per second of audio, in this build (run with `--release`):
//!
//! ```text
//! nix develop -c cargo run --release -p tb-audio --example cost
//! ```
//!
//! Feeds 60 s of program-like noise through a [`TrackPipeline`] in 1024-sample blocks (an AAC
//! frame) and pulls the AC-3 frames, as the server would; then the stages alone.
#![allow(clippy::print_stdout, reason = "the example's report")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "an example")]

use std::time::Instant;
use tb_audio::clock::AudioFrame;
use tb_audio::layout::{DownmixScaling, InputLayout, Mixer, OutputLayout};
use tb_audio::limiter::{Limiter, LimiterConfig};
use tb_audio::loudness::LoudnessMeter;
use tb_audio::normalize::{Normalizer, NormalizerConfig};
use tb_audio::pipeline::{InputFormat, TrackConfig, TrackPipeline};
use tb_audio::resample::{InputRate, Resampler, Trim};
use tb_audio::signal::program;
use tb_timeline::{Source, Ticks27};

const SECS: usize = 60;

fn input(rate: u32, ch: usize) -> Vec<Vec<f32>> {
    // Program noise is made at 48 kHz; the rate only changes how much there is per second.
    program(1, ch, rate as usize * SECS)
}

fn track(rate: u32, in_ch: usize, out: OutputLayout) -> f64 {
    let x = input(rate, in_ch);
    let mut t = TrackPipeline::new(TrackConfig::new(Ticks27(0), out)).unwrap();
    t.open(
        Source::Live,
        InputFormat {
            rate: InputRate::new(rate).unwrap(),
            layout: InputLayout::default_for(in_ch).unwrap(),
        },
    )
    .unwrap();
    t.switch(Source::Live, Ticks27(0));
    t.set_skew(Source::Live, 37_000);
    let mut f = AudioFrame::new(out);
    let start = Instant::now();
    let mut sent = 0usize;
    let mut j = 0u64;
    let jitter = Ticks27::from_ms(1000);
    while sent < x[0].len() {
        let n = 1024.min(x[0].len() - sent);
        let refs: Vec<&[f32]> = x.iter().map(|p| &p[sent..sent + n]).collect();
        let at = Ticks27(jitter.0 + (sent as i64 * 27_000_000) / i64::from(rate));
        t.push(Source::Live, at, &refs).unwrap();
        sent += n;
        // Frames due up to the time sent minus the jitter target.
        while (j as i64 + 1) * 864_000 + jitter.0 < at.0 {
            t.next_frame(&mut f);
            j += 1;
        }
    }
    start.elapsed().as_secs_f64() / SECS as f64
}

fn time(f: impl FnOnce()) -> f64 {
    let s = Instant::now();
    f();
    s.elapsed().as_secs_f64() / SECS as f64
}

fn main() {
    println!("CPU seconds per second of audio (share of one core), {SECS} s of input:");
    for (rate, ch, out, what) in [
        (44_100, 2, OutputLayout::Stereo, "44.1 kHz stereo -> stereo"),
        (48_000, 2, OutputLayout::Stereo, "48 kHz stereo -> stereo"),
        (48_000, 6, OutputLayout::Stereo, "48 kHz 5.1 -> stereo"),
        (48_000, 6, OutputLayout::FivePointOne, "48 kHz 5.1 -> 5.1"),
        (96_000, 2, OutputLayout::Stereo, "96 kHz stereo -> stereo"),
        (32_000, 1, OutputLayout::Stereo, "32 kHz mono -> stereo"),
    ] {
        let c = track(rate, ch, out);
        println!("  track {what:28} {:7.4} %", c * 100.0);
    }
    let x48 = input(48_000, 2);
    let x44 = input(44_100, 2);
    let refs = |x: &[Vec<f32>], s: usize, n: usize| -> Vec<Vec<f32>> {
        x.iter().map(|p| p[s..s + n].to_vec()).collect()
    };
    let blocks = |x: &[Vec<f32>]| -> Vec<Vec<Vec<f32>>> {
        (0..x[0].len() / 1024)
            .map(|i| refs(x, i * 1024, 1024))
            .collect()
    };
    let b44 = blocks(&x44);
    let b48 = blocks(&x48);
    let mut out = vec![Vec::new(), Vec::new()];
    let c = time(|| {
        let mut r = Resampler::new(InputRate::HZ_44100, 2);
        r.set_trim(Trim::new(37_000).unwrap());
        for b in &b44 {
            out[0].clear();
            out[1].clear();
            r.process(&[&b[0], &b[1]], &mut out).unwrap();
        }
    });
    println!(
        "  resampler 44.1 -> 48 stereo           {:7.4} %",
        c * 100.0
    );
    let c = time(|| {
        let mut r = Resampler::new(InputRate::HZ_48000, 2);
        r.set_trim(Trim::new(37_000).unwrap());
        for b in &b48 {
            out[0].clear();
            out[1].clear();
            r.process(&[&b[0], &b[1]], &mut out).unwrap();
        }
    });
    println!(
        "  resampler 48 -> 48 (+37 ppm) stereo   {:7.4} %",
        c * 100.0
    );
    let m = Mixer::new(
        InputLayout::default_for(6).unwrap(),
        OutputLayout::Stereo,
        DownmixScaling::Unscaled,
    );
    let x6 = input(48_000, 6);
    let b6 = blocks(&x6);
    let c = time(|| {
        for b in &b6 {
            let r: Vec<&[f32]> = b.iter().map(|p| p.as_slice()).collect();
            m.process(&r, &mut out).unwrap();
        }
    });
    println!(
        "  mixer 5.1 -> stereo                   {:7.4} %",
        c * 100.0
    );
    let mut b = b48.clone();
    let c = time(|| {
        let mut n = Normalizer::new(OutputLayout::Stereo, NormalizerConfig::default()).unwrap();
        for blk in &mut b {
            n.process(blk).unwrap();
        }
    });
    println!(
        "  normalizer stereo                     {:7.4} %",
        c * 100.0
    );
    let mut b = b48.clone();
    let c = time(|| {
        let mut l = Limiter::new(2, LimiterConfig::default()).unwrap();
        for blk in &mut b {
            l.process(blk).unwrap();
        }
    });
    println!(
        "  limiter stereo                        {:7.4} %",
        c * 100.0
    );
    let c = time(|| {
        let mut m = LoudnessMeter::new(OutputLayout::Stereo, true);
        for blk in &b48 {
            m.push(&[&blk[0], &blk[1]]).unwrap();
        }
    });
    println!(
        "  meter stereo with true peak           {:7.4} %",
        c * 100.0
    );
}
