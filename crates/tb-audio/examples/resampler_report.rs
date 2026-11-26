//! Measurements of the resampler:
//!
//! ```text
//! nix develop -c cargo run --release -p tb-audio --example resampler_report
//! ```
//!
//! 1. The designed kernel's continuous response: passband ripple to the passband edge,
//!    worst stopband level from the stopband edge to 4× the input rate.
//! 2. End to end, through the tabulated, interpolated `f32` implementation: the gain of tones
//!    across the passband (ripple as measured), THD+N of sines (least-squares fit of the tone,
//!    everything else is distortion and noise), and the rejection of tones the filter must
//!    remove (above the output's band for downsampling).
#![allow(clippy::print_stdout, reason = "the example's report")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "an example")]

use tb_audio::resample::{Design, InputRate, Resampler, Trim};
use tb_audio::signal::{fit_sine, sine};

fn resample(rate: u32, trim: i64, x: &[f32]) -> Vec<f32> {
    let mut r = Resampler::new(InputRate::new(rate).unwrap(), 1);
    r.set_trim(Trim::new(trim).unwrap());
    let mut out = vec![Vec::new()];
    for c in x.chunks(1024) {
        r.process(&[c], &mut out).unwrap();
    }
    out.pop().unwrap()
}

/// Gain (dB) and THD+N (dB relative to the tone) of a tone through the resampler.
fn tone(rate: u32, trim: i64, f: f64) -> (f64, f64) {
    let x = sine(f, 0.5, rate, rate as usize, 0.2);
    let y = resample(rate, trim, &x);
    let step = f64::from(rate) / 48_000.0 * (1.0 + trim as f64 * 1e-9);
    let f_out = f / f64::from(rate) * step * 48_000.0;
    let (a, r) = fit_sine(&y[4800..y.len() - 4800], f_out, 48_000.0);
    (
        20.0 * (a / 0.5).log10(),
        20.0 * (r / (a / 2f64.sqrt())).log10(),
    )
}

/// Output power (dB relative to the input tone's) of a tone that must be removed.
fn rejection(rate: u32, f: f64) -> f64 {
    let x = sine(f, 0.5, rate, rate as usize, 0.2);
    let y = resample(rate, 0, &x);
    let y = &y[4800..y.len() - 4800];
    let p: f64 = y.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / y.len() as f64;
    10.0 * (p / 0.125).max(1e-30).log10()
}

fn main() {
    println!("1. Designed kernels (continuous response)");
    println!(
        "{:>8} {:>6} {:>9} {:>9} {:>12} {:>14}",
        "rate", "taps", "pass Hz", "stop Hz", "ripple dB", "stopband dB"
    );
    for rate in [
        8_000, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000,
    ] {
        let d = Design::for_rate(InputRate::new(rate).unwrap());
        let mut ripple: f64 = 0.0;
        for i in 0..=200 {
            let f = d.passband_hz * f64::from(i) / 200.0;
            ripple = ripple.max(d.response_db(f).abs());
        }
        let mut stop = f64::MIN;
        let top = 4.0 * f64::from(rate);
        for i in 0..=400 {
            let f = d.stopband_hz + (top - d.stopband_hz) * f64::from(i) / 400.0;
            stop = stop.max(d.response_db(f));
        }
        println!(
            "{rate:>8} {:>6} {:>9.0} {:>9.0} {:>12.6} {:>14.1}",
            2 * d.half_taps,
            d.passband_hz,
            d.stopband_hz,
            ripple,
            stop
        );
    }
    println!();
    println!("2a. Passband gain, end to end (min/max over 20 Hz .. 20 kHz, dB)");
    for (rate, trim) in [
        (44_100, 0),
        (48_000, 500_000),
        (48_000, -500_000),
        (96_000, 0),
    ] {
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for f in [
            20.0, 100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 18_000.0, 19_500.0, 20_000.0,
        ] {
            let (g, _) = tone(rate, trim, f);
            lo = lo.min(g);
            hi = hi.max(g);
        }
        println!("  {rate} Hz, trim {trim:>8} ppb: {lo:+.6} .. {hi:+.6}");
    }
    println!();
    println!("2b. THD+N of a −6 dBFS sine, end to end (dB relative to the tone)");
    for (rate, trim) in [
        (44_100, 0),
        (44_100, 250_000),
        (48_000, 0),
        (48_000, 1),
        (48_000, 500_000),
        (48_000, -500_000),
        (32_000, 0),
        (96_000, 0),
        (192_000, -123_456),
    ] {
        let mut row = format!("  {rate:>6} Hz, trim {trim:>8} ppb:");
        for f in [997.0, 6_000.0, 12_000.0, 19_000.0] {
            if f < 0.45 * f64::from(rate).min(48_000.0) {
                let (_, t) = tone(rate, trim, f);
                row += &format!("  {f:>6.0} Hz {t:7.1}");
            }
        }
        println!("{row}");
    }
    println!();
    println!("2c. Rejection of tones above the output band (dB relative to the input tone)");
    for (rate, f) in [
        (96_000, 28_000.0),
        (96_000, 30_000.0),
        (96_000, 40_000.0),
        (192_000, 30_000.0),
        (192_000, 70_000.0),
        (88_200, 30_000.0),
    ] {
        println!("  {rate} Hz input, {f} Hz tone: {:.1}", rejection(rate, f));
    }
    println!();
    let x = sine(997.0, 0.9, 48_000, 96_000, 0.1);
    let y = resample(48_000, 0, &x);
    let same = y.iter().zip(&x).all(|(a, b)| a == b);
    println!("3. 48 -> 48 kHz, zero trim: output identical to input: {same}");
}
