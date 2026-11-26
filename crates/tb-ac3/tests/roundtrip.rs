//! Encoder → strict parser → reference decoder (the standard's own inverse transform), with no
//! external tools: runs everywhere, including the Nix sandbox.
#![allow(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: the tables are the report, helpers fail the test"
)]

mod common;

use common::{Signal, encode_all, snr_db};
use tb_ac3::decode::ReferenceDecoder;
use tb_ac3::parse::{self, parse_frame};
use tb_ac3::{
    AudioCodingMode, AudioProduction, BitRate, BitstreamMode, Config, ConfigBuilder, Dialnorm,
    Encoder, FRAME_SAMPLES, MixLevel, RoomType, silent_frame,
};

/// Decodes a stream of frames with the reference decoder.
fn decode_all(stream: &[u8], cfg: &Config) -> Vec<Vec<f32>> {
    let fb = cfg.frame_bytes();
    let mut dec = ReferenceDecoder::new(cfg.channels());
    let mut out = vec![Vec::new(); cfg.channels()];
    for frame in stream.chunks(fb) {
        let parsed = parse_frame(frame).expect("parse");
        dec.decode_frame(&parsed, &mut out);
    }
    out
}

fn roundtrip_snr(cfg: Config, signal: Signal, seconds: f64) -> Vec<f64> {
    let len = (seconds * 48_000.0) as usize;
    // The LFE channel codes bins 0–6 only (up to 656 Hz): give it a 50 Hz tone.
    let input: Vec<Vec<f32>> = (0..cfg.channels())
        .map(|c| {
            if c == cfg.nfchans() {
                Signal::Sine(50.0, -10.0)
            } else {
                signal
            }
            .generate(len, c)
        })
        .collect();
    let stream = encode_all(cfg, &input);
    let decoded = decode_all(&stream, &cfg);
    input
        .iter()
        .zip(&decoded)
        .map(|(i, d)| snr_db(i, d, Encoder::DELAY_SAMPLES))
        .collect()
}

#[test]
fn every_rate_and_layout_makes_valid_frames_of_the_nominal_size() {
    let modes = [
        AudioCodingMode::Mono,
        AudioCodingMode::STEREO,
        AudioCodingMode::ThreeZero {
            center: Default::default(),
        },
        AudioCodingMode::TwoOne {
            surround: Default::default(),
        },
        AudioCodingMode::ThreeOne {
            center: Default::default(),
            surround: Default::default(),
        },
        AudioCodingMode::TwoTwo {
            surround: Default::default(),
        },
        AudioCodingMode::THREE_TWO,
    ];
    for rate in BitRate::ALL.into_iter().filter(|r| *r <= BitRate::Kbps448) {
        for mode in modes {
            for lfe in [false, true] {
                let cfg = Config::new(mode, lfe, rate).unwrap();
                // White noise is the hardest case for the bit budget.
                let input: Vec<Vec<f32>> = (0..cfg.channels())
                    .map(|c| Signal::White(-10.0).generate(FRAME_SAMPLES * 2, c))
                    .collect();
                let stream = encode_all(cfg, &input);
                assert_eq!(stream.len(), 3 * cfg.frame_bytes());
                for frame in stream.chunks(cfg.frame_bytes()) {
                    let h = parse::parse_header(frame).unwrap();
                    assert_eq!(h.syncinfo.kbps(), rate.kbps());
                    assert_eq!(h.bsi.acmod, mode.acmod());
                    assert_eq!(h.bsi.lfeon, lfe);
                    assert_eq!(h.bsi.dialnorm, 24);
                }
            }
        }
    }
}

#[test]
fn silence_decodes_to_exact_zeros() {
    for mode in [
        AudioCodingMode::Mono,
        AudioCodingMode::STEREO,
        AudioCodingMode::THREE_TWO,
    ] {
        for lfe in [false, true] {
            let cfg = Config::new(mode, lfe, BitRate::Kbps192).unwrap();
            let frame = silent_frame(&cfg);
            assert_eq!(frame.len(), cfg.frame_bytes());
            let parsed = parse_frame(&frame).unwrap();
            for blk in &parsed.blocks {
                assert!(blk.dithflag.iter().all(|d| !d), "dither on in silence");
                assert!(blk.coefs.iter().all(|c| c.iter().all(|&v| v == 0.0)));
            }
            let mut dec = ReferenceDecoder::new(cfg.channels());
            let mut out = vec![Vec::new(); cfg.channels()];
            dec.decode_frame(&parsed, &mut out);
            assert!(out.iter().all(|c| c.iter().all(|&v| v == 0.0)));
            // An encoder fed zeros makes the same frame every time.
            let mut enc = Encoder::new(cfg);
            let z = [0.0f32; FRAME_SAMPLES];
            let refs: Vec<&[f32]> = (0..cfg.channels()).map(|_| &z[..]).collect();
            for _ in 0..3 {
                assert_eq!(enc.encode_frame(&refs).unwrap(), &frame[..]);
            }
        }
    }
}

#[test]
fn bsi_carries_the_configuration() {
    let b = ConfigBuilder {
        dialnorm: Dialnorm::new(27).unwrap(),
        bitstream_mode: BitstreamMode::Commentary,
        copyright: true,
        original: false,
        audio_production: Some(AudioProduction {
            mix_level: MixLevel::new(25).unwrap(),
            room_type: RoomType::SmallRoom,
        }),
        ..ConfigBuilder::new(AudioCodingMode::THREE_TWO, true, BitRate::Kbps384)
    };
    let cfg = b.build().unwrap();
    let h = parse::parse_header(&silent_frame(&cfg)).unwrap();
    assert_eq!(h.bsi.bsid, 8);
    assert_eq!(h.bsi.dialnorm, 27);
    assert_eq!(h.bsi.bsmod, 5);
    assert_eq!(h.bsi.acmod, 7);
    assert_eq!(h.bsi.cmixlev, Some(1));
    assert_eq!(h.bsi.surmixlev, Some(1));
    assert_eq!(h.bsi.dsurmod, None);
    assert!(h.bsi.lfeon && h.bsi.copyright && !h.bsi.original);
    assert_eq!(h.bsi.audio_production, Some((25, 2)));
    assert_eq!(h.syncinfo.frmsizecod, 28);
}

#[test]
fn input_shape_is_checked() {
    let cfg = Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps192).unwrap();
    let mut enc = Encoder::new(cfg);
    let a = [0.0f32; FRAME_SAMPLES];
    let short = [0.0f32; 100];
    assert!(enc.encode_frame(&[&a]).is_err());
    assert!(enc.encode_frame(&[&a, &short]).is_err());
    assert!(enc.encode_frame(&[&a, &a]).is_ok());
}

/// Identical left and right channels are rematrixed (A/52 §7.5): every band's flag is set, the
/// difference channel decodes to nothing, and turning rematrixing off costs quality at a low
/// rate, because then both channels must be coded in full.
#[test]
fn identical_channels_are_rematrixed() {
    let len = FRAME_SAMPLES * 12;
    let x = Signal::Pink(-15.0).generate(len, 0);
    let input = vec![x.clone(), x];
    let mut snr = Vec::new();
    for remat in [true, false] {
        let cfg = ConfigBuilder {
            rematrixing: remat,
            ..ConfigBuilder::new(AudioCodingMode::STEREO, false, BitRate::Kbps96)
        }
        .build()
        .unwrap();
        let stream = encode_all(cfg, &input);
        let mut flags = 0;
        let mut blocks = 0;
        // The last frame is the flush after the input ends: silence, nothing to rematrix.
        let frames = stream.len() / cfg.frame_bytes();
        for frame in stream.chunks(cfg.frame_bytes()).take(frames - 1) {
            for blk in parse_frame(frame).unwrap().blocks {
                flags += blk.rematflg.iter().filter(|&&f| f).count();
                blocks += 1;
            }
        }
        if remat {
            assert_eq!(flags, 4 * blocks);
        } else {
            assert_eq!(flags, 0);
        }
        let decoded = decode_all(&stream, &cfg);
        snr.push(snr_db(&input[0], &decoded[0], Encoder::DELAY_SAMPLES));
    }
    println!(
        "pink noise, L = R, 96 kbit/s: SNR {:.1} dB rematrixed, {:.1} dB not",
        snr[0], snr[1]
    );
    assert!(snr[0] > snr[1] + 2.0, "{snr:?}");
}

#[test]
fn sine_round_trip_snr() {
    let cfg = Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps192).unwrap();
    for (f, l) in [(1000.0, -10.0), (100.0, -20.0), (5000.0, -6.0)] {
        let snr = roundtrip_snr(cfg, Signal::Sine(f, l), 0.5);
        println!("sine {f} Hz {l} dBFS: {snr:?}");
        assert!(snr.iter().all(|&s| s > 40.0), "{snr:?}");
    }
}

#[test]
fn noise_and_transients_round_trip() {
    let cfg = Config::new(AudioCodingMode::THREE_TWO, true, BitRate::Kbps384).unwrap();
    for sig in [Signal::Pink(-20.0), Signal::Clicks, Signal::Sweep(-12.0)] {
        let snr = roundtrip_snr(cfg, sig, 0.5);
        println!("{}: {snr:?}", sig.name());
        assert!(snr.iter().all(|&s| s > 5.0), "{snr:?}");
    }
}

#[test]
fn transients_use_short_blocks() {
    let cfg = Config::new(AudioCodingMode::Mono, false, BitRate::Kbps96).unwrap();
    let x = Signal::Clicks.generate(FRAME_SAMPLES * 10, 0);
    let mut enc = Encoder::new(cfg);
    let mut short = 0;
    for f in x.chunks(FRAME_SAMPLES) {
        enc.encode_frame(&[f]).unwrap();
        short += enc.last_stats().short_blocks;
    }
    // A burst every 100 ms: about 3 per 10 frames (320 ms).
    assert!(short >= 3, "{short} short blocks");
}

/// Noise in one block only, after silence: the §5.5 limits (A/52 p.52) bind. Energy in block 5
/// needs skip padding to push block 5's mantissas past the 5/8 point; energy in blocks 0–1 must
/// stay within the first 5/8. The parser checks both limits on every frame.
#[test]
fn five_eighths_limits_hold_when_one_block_has_all_the_energy() {
    let mut padded = 0;
    for rate in [BitRate::Kbps96, BitRate::Kbps192, BitRate::Kbps448] {
        for mode in [AudioCodingMode::Mono, AudioCodingMode::STEREO] {
            let cfg = Config::new(mode, false, rate).unwrap();
            for loud_block in [0usize, 1, 5] {
                let mut enc = Encoder::new(cfg);
                let noise = Signal::White(-3.0).generate(FRAME_SAMPLES, 0);
                let x: Vec<f32> = (0..FRAME_SAMPLES)
                    .map(|n| if n / 256 == loud_block { noise[n] } else { 0.0 })
                    .collect();
                let refs: Vec<&[f32]> = (0..cfg.channels()).map(|_| &x[..]).collect();
                // Two frames: the first block of the second frame overlaps the first's end.
                for f in 0..2 {
                    let frame = enc.encode_frame(&refs).unwrap().to_vec();
                    let stats = enc.last_stats();
                    parse_frame(&frame).unwrap_or_else(|e| {
                        panic!("{mode:?} {rate:?} block {loud_block} frame {f}: {e}")
                    });
                    if stats.skip_bytes > 0 {
                        padded += 1;
                    }
                }
            }
        }
    }
    // The padding path was exercised.
    assert!(padded > 0);
}

/// The bit allocation fills the frame: on noise, which can use every bit, less than 2 % of the
/// frame is left as aux filler.
#[test]
fn frames_are_filled() {
    for (mode, lfe, rate) in [
        (AudioCodingMode::STEREO, false, BitRate::Kbps192),
        (AudioCodingMode::THREE_TWO, true, BitRate::Kbps384),
        (AudioCodingMode::Mono, false, BitRate::Kbps64),
    ] {
        let cfg = Config::new(mode, lfe, rate).unwrap();
        let mut enc = Encoder::new(cfg);
        let chans: Vec<Vec<f32>> = (0..cfg.channels())
            .map(|c| Signal::Pink(-12.0).generate(FRAME_SAMPLES * 8, c))
            .collect();
        for f in 0..8 {
            let refs: Vec<&[f32]> = chans
                .iter()
                .map(|c| &c[f * FRAME_SAMPLES..(f + 1) * FRAME_SAMPLES])
                .collect();
            enc.encode_frame(&refs).unwrap();
            let s = enc.last_stats();
            assert!(
                s.aux_bits * 50 < s.frame_bits,
                "{mode:?} {rate:?} frame {f}: {s:?}"
            );
        }
    }
}
