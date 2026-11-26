//! Encode cost: CPU time per second of audio, per layout and rate, on one thread.
//!
//! ```text
//! cargo run --release -p tb-ac3 --example throughput [seconds]
//! ```
//!
//! The input is a mix that keeps the encoder busy: a tone, noise, and a burst every 100 ms, so
//! that block switching, exponent updates and the SNR search all run. Prints, per case, the
//! time to encode one second of audio and that time as a share of one core.
#![allow(clippy::print_stdout)]

use std::time::Instant;
use tb_ac3::{AudioCodingMode, BitRate, Config, Encoder, FRAME_SAMPLES};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: f64 = std::env::args()
        .nth(1)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(20.0);
    let cases = [
        (AudioCodingMode::Mono, false, BitRate::Kbps96),
        (AudioCodingMode::STEREO, false, BitRate::Kbps96),
        (AudioCodingMode::STEREO, false, BitRate::Kbps192),
        (AudioCodingMode::STEREO, false, BitRate::Kbps448),
        (AudioCodingMode::THREE_TWO, true, BitRate::Kbps384),
        (AudioCodingMode::THREE_TWO, true, BitRate::Kbps448),
    ];
    let frames = (seconds * 48_000.0 / FRAME_SAMPLES as f64).ceil() as usize;
    println!("| layout | kbit/s | ms per second of audio | share of one core |");
    println!("|---|---|---|---|");
    for (mode, lfe, rate) in cases {
        let cfg = Config::new(mode, lfe, rate)?;
        let nch = cfg.channels();
        // Pre-generate the input so that only encoding is timed.
        let mut seed = 0x1234_5678u32;
        let input: Vec<Vec<f32>> = (0..nch)
            .map(|c| {
                (0..frames * FRAME_SAMPLES)
                    .map(|n| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.05;
                        let t = n as f32 / 48_000.0;
                        let tone = 0.2
                            * (2.0 * std::f32::consts::PI * (440.0 + 110.0 * c as f32) * t).sin();
                        let k = n % 4800;
                        let burst = if k < 480 {
                            0.5 * (-(k as f32) / 60.0).exp()
                        } else {
                            0.0
                        };
                        tone + noise + burst
                    })
                    .collect()
            })
            .collect();
        let mut enc = Encoder::new(cfg);
        let mut bytes = 0usize;
        let start = Instant::now();
        for f in 0..frames {
            let refs: Vec<&[f32]> = input
                .iter()
                .map(|ch| &ch[f * FRAME_SAMPLES..(f + 1) * FRAME_SAMPLES])
                .collect();
            bytes += enc.encode_frame(&refs)?.len();
        }
        let elapsed = start.elapsed().as_secs_f64();
        let audio = (frames * FRAME_SAMPLES) as f64 / 48_000.0;
        let ms_per_s = elapsed / audio * 1000.0;
        println!(
            "| {} | {} | {:.2} | {:.3} % |",
            mode.channel_names().join(" ") + if lfe { " LFE" } else { "" },
            rate.kbps(),
            ms_per_s,
            ms_per_s / 10.0
        );
        std::hint::black_box(bytes);
    }
    Ok(())
}
