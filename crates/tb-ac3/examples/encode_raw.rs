//! Encodes raw interleaved f32le PCM (48 kHz, channels in A/52 order, LFE last) from stdin to
//! an AC-3 elementary stream on stdout. For manual validation against other decoders.
//!
//! ```text
//! cargo run --release -p tb-ac3 --example encode_raw -- <acmod 1..7> <lfe 0|1> <kbps> [dialnorm]
//! ```
#![allow(clippy::print_stderr)]

use std::io::{Read, Write};
use tb_ac3::{
    AudioCodingMode, Bandwidth, BandwidthCode, BitRate, CenterMixLevel, ConfigBuilder, Dialnorm,
    Encoder, FRAME_SAMPLES, SurroundMixLevel,
};

fn mode(acmod: u32) -> Option<AudioCodingMode> {
    let c = CenterMixLevel::default();
    let s = SurroundMixLevel::default();
    Some(match acmod {
        1 => AudioCodingMode::Mono,
        2 => AudioCodingMode::STEREO,
        3 => AudioCodingMode::ThreeZero { center: c },
        4 => AudioCodingMode::TwoOne { surround: s },
        5 => AudioCodingMode::ThreeOne {
            center: c,
            surround: s,
        },
        6 => AudioCodingMode::TwoTwo { surround: s },
        7 => AudioCodingMode::ThreeTwo {
            center: c,
            surround: s,
        },
        _ => return None,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: encode_raw <acmod 1..7> <lfe 0|1> <kbps> [dialnorm]");
        std::process::exit(2);
    }
    let m = mode(args[0].parse()?).ok_or("acmod must be 1..7")?;
    let lfe = args[1] == "1";
    let rate = BitRate::from_kbps(args[2].parse()?).ok_or("not an A/52 bit rate")?;
    let mut b = ConfigBuilder::new(m, lfe, rate);
    if let Some(d) = args.get(3) {
        b.dialnorm = Dialnorm::new(d.parse()?)?;
    }
    // Further words switch features off, or print each frame's decisions, for experiments.
    let mut stats = false;
    for flag in args.iter().skip(4) {
        match flag.as_str() {
            "noswitch" => b.block_switching = false,
            "noremat" => b.rematrixing = false,
            "stats" => stats = true,
            bw if bw.starts_with("bw=") => {
                b.bandwidth = Bandwidth::Fixed(BandwidthCode::new(bw[3..].parse()?)?);
            }
            other => return Err(format!("unknown flag {other}").into()),
        }
    }
    let cfg = b.build()?;
    let nch = cfg.channels();
    let mut enc = Encoder::new(cfg);
    let mut raw = Vec::new();
    std::io::stdin().read_to_end(&mut raw)?;
    let samples: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    let frames = (samples.len() / nch).div_ceil(FRAME_SAMPLES);
    let mut chans = vec![vec![0.0f32; FRAME_SAMPLES]; nch];
    let mut out = std::io::stdout().lock();
    for f in 0..frames {
        for (i, frame_sample) in (0..FRAME_SAMPLES).enumerate() {
            for (c, ch) in chans.iter_mut().enumerate() {
                let idx = (f * FRAME_SAMPLES + frame_sample) * nch + c;
                ch[i] = samples.get(idx).copied().unwrap_or(0.0);
            }
        }
        let refs: Vec<&[f32]> = chans.iter().map(|c| c.as_slice()).collect();
        out.write_all(enc.encode_frame(&refs)?)?;
        if stats {
            eprintln!("frame {f}: {:?}", enc.last_stats());
        }
    }
    Ok(())
}
