//! Cross-validation against independent decoders and against FFmpeg's own AC-3 encoder.
//!
//! These tests need `ffmpeg`, `ffprobe` and liba52's `a52dec`, which the Nix sandbox does not
//! have, so they are `#[ignore]`d. Run them with:
//!
//! ```text
//! A52DEC=$(nix build --inputs-from . nixpkgs#a52dec --print-out-paths)/bin/a52dec \
//!   nix develop -c cargo nextest run -p tb-ac3 --release --test external --run-ignored only \
//!   --no-capture
//! ```
//!
//! `A52DEC` and `FFMPEG` / `FFPROBE` override the programs found on `PATH`;
//! `TB_AC3_SCRATCH` sets the scratch directory (default: a directory under the system temp
//! directory). A test whose tools are missing prints a note and passes.
#![allow(
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: the tables are the report, helpers fail the test"
)]

mod common;

use common::{Signal, encode_all, snr_db, snr_db_lowpass};
use std::path::{Path, PathBuf};
use std::process::Command;
use tb_ac3::{
    AudioCodingMode, BitRate, CenterMixLevel, Config, ConfigBuilder, Dialnorm, Encoder,
    FRAME_SAMPLES, SurroundMixLevel,
};

fn find_tool(name: &str, env: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    a52dec: PathBuf,
    scratch: PathBuf,
}

fn tools() -> Option<Tools> {
    let ffmpeg = find_tool("ffmpeg", "FFMPEG");
    let ffprobe = find_tool("ffprobe", "FFPROBE");
    let a52dec = find_tool("a52dec", "A52DEC");
    match (ffmpeg, ffprobe, a52dec) {
        (Some(ffmpeg), Some(ffprobe), Some(a52dec)) => {
            let scratch = std::env::var_os("TB_AC3_SCRATCH")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("tb-ac3-{}", std::process::id()))
                });
            std::fs::create_dir_all(&scratch).ok()?;
            Some(Tools {
                ffmpeg,
                ffprobe,
                a52dec,
                scratch,
            })
        }
        _ => {
            println!("SKIPPED: ffmpeg, ffprobe or a52dec not found (set FFMPEG, FFPROBE, A52DEC)");
            None
        }
    }
}

fn all_modes() -> [AudioCodingMode; 7] {
    let c = CenterMixLevel::default();
    let s = SurroundMixLevel::default();
    [
        AudioCodingMode::Mono,
        AudioCodingMode::STEREO,
        AudioCodingMode::ThreeZero { center: c },
        AudioCodingMode::TwoOne { surround: s },
        AudioCodingMode::ThreeOne {
            center: c,
            surround: s,
        },
        AudioCodingMode::TwoTwo { surround: s },
        AudioCodingMode::ThreeTwo {
            center: c,
            surround: s,
        },
    ]
}

/// FFmpeg orders channels by their mask bit: FL FR FC LFE BL BR … BC … SL SR. The position
/// of each of our channels (A/52 order, LFE last) in FFmpeg's order.
fn ffmpeg_order(cfg: &Config) -> Vec<usize> {
    let bit = |name: &str| match name {
        "L" => 0,
        "R" => 1,
        "C" => 2,
        "LFE" => 3,
        "S" => 8,
        "SL" => 9,
        "SR" => 10,
        _ => 99,
    };
    let mut names: Vec<&str> = cfg.coding_mode().channel_names().to_vec();
    if cfg.lfe() {
        names.push("LFE");
    }
    let mut sorted: Vec<(usize, usize)> =
        names.iter().enumerate().map(|(i, n)| (bit(n), i)).collect();
    sorted.sort();
    // pos[our index] = ffmpeg index
    let mut pos = vec![0; names.len()];
    for (ff, (_, ours)) in sorted.iter().enumerate() {
        pos[*ours] = ff;
    }
    pos
}

/// FFmpeg's name of the layout, for the layouts the comparison feeds to its encoder.
fn ffmpeg_layout_name(cfg: &Config) -> Option<&'static str> {
    Some(match (cfg.coding_mode().acmod(), cfg.lfe()) {
        (1, false) => "mono",
        (2, false) => "stereo",
        (7, false) => "5.0(side)",
        (7, true) => "5.1(side)",
        _ => return None,
    })
}

fn run(cmd: &mut Command) -> (bool, Vec<u8>, String) {
    match cmd.output() {
        Ok(o) => (
            o.status.success(),
            o.stdout,
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        Err(e) => (false, Vec::new(), e.to_string()),
    }
}

/// Decodes with FFmpeg in its strictest mode; returns channels in our order, or the error.
fn ffmpeg_decode(
    t: &Tools,
    file: &Path,
    cfg: &Config,
    extra: &[&str],
) -> Result<Vec<Vec<f32>>, String> {
    let mut cmd = Command::new(&t.ffmpeg);
    cmd.args([
        "-v",
        "error",
        "-err_detect",
        "crccheck+bitstream+buffer+explode",
    ]);
    cmd.args(extra);
    cmd.args(["-f", "ac3", "-i"])
        .arg(file)
        .args(["-f", "f32le", "-"]);
    let (ok, out, err) = run(&mut cmd);
    if !ok || !err.trim().is_empty() {
        return Err(format!("ffmpeg: ok={ok} stderr={err}"));
    }
    let nch = cfg.channels();
    let samples: Vec<f32> = out
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    let pos = ffmpeg_order(cfg);
    Ok((0..nch)
        .map(|c| samples.iter().skip(pos[c]).step_by(nch).copied().collect())
        .collect())
}

/// Decodes with liba52 (`a52dec -o wav`: 16-bit stereo, or the downmix); returns
/// (frames decoded, error lines, samples per output channel).
fn a52_decode(t: &Tools, file: &Path) -> (usize, usize, Vec<Vec<f32>>) {
    let (_, out, err) = run(Command::new(&t.a52dec).args(["-r", "-o", "wav"]).arg(file));
    let errors = err
        .lines()
        .filter(|l| l.to_lowercase().contains("error"))
        .count();
    let frames = err
        .lines()
        .find_map(|l| {
            l.split_whitespace()
                .next()
                .filter(|_| l.contains("frames decoded"))
        })
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    // WAV: find the "data" chunk.
    let mut chans = vec![Vec::new(), Vec::new()];
    if let Some(at) = out.windows(4).position(|w| w == b"data") {
        let pcm = &out[at + 8..];
        for (i, s) in pcm.as_chunks::<2>().0.iter().enumerate() {
            chans[i % 2].push(f32::from(i16::from_le_bytes(*s)) / 32768.0);
        }
    }
    (frames, errors, chans)
}

fn ffprobe_stream(t: &Tools, file: &Path) -> String {
    let (_, out, _) = run(Command::new(&t.ffprobe)
        .args(["-v", "error", "-show_entries"])
        .arg("stream=codec_name,sample_rate,channels,channel_layout,bit_rate")
        .args(["-of", "compact=p=0"])
        .arg(file));
    String::from_utf8_lossy(&out).trim().to_string()
}

fn expected_layout(cfg: &Config) -> &'static str {
    match (cfg.coding_mode().acmod(), cfg.lfe()) {
        (1, false) => "mono",
        (1, true) => "2.0", // FC+LFE has no common name; checked by channel count only
        (2, false) => "stereo",
        (2, true) => "2.1",
        (3, false) => "3.0",
        (3, true) => "3.1",
        (4, false) => "3.0(back)",
        (4, true) => "3.1(back)",
        (5, false) => "4.0",
        (5, true) => "4.1",
        (6, false) => "quad(side)",
        (6, true) => "4.1(side)",
        (7, false) => "5.0(side)",
        _ => "5.1(side)",
    }
}

/// FFmpeg's spelling of a layout without a common name: "N channels (FL+FR+…)", in its order.
fn explicit_layout(cfg: &Config) -> String {
    let mut names: Vec<&str> = cfg.coding_mode().channel_names().to_vec();
    if cfg.lfe() {
        names.push("LFE");
    }
    let pos = ffmpeg_order(cfg);
    let mut ff = vec![""; names.len()];
    for (i, n) in names.iter().enumerate() {
        ff[pos[i]] = match *n {
            "L" => "FL",
            "R" => "FR",
            "C" => "FC",
            "S" => "BC",
            other => other,
        };
    }
    format!("{} channels ({})", ff.len(), ff.join("+"))
}

fn input_for(cfg: &Config, signal: Signal, len: usize) -> Vec<Vec<f32>> {
    (0..cfg.channels())
        .map(|c| {
            // The LFE channel codes up to 656 Hz: it gets a 50 Hz tone unless all is silent.
            if c == cfg.nfchans() && signal != Signal::Silence {
                Signal::Sine(50.0, -10.0)
            } else {
                signal
            }
            .generate(len, c)
        })
        .collect()
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a52dec; see the module documentation"]
fn external_decoders_accept_every_frame() {
    let Some(t) = tools() else { return };
    let signals = [
        Signal::Pink(-20.0),
        Signal::Clicks,
        Signal::Sweep(-6.0),
        Signal::Silence,
    ];
    let rates = [
        BitRate::Kbps96,
        BitRate::Kbps192,
        BitRate::Kbps384,
        BitRate::Kbps448,
    ];
    let mut cases = 0;
    let mut frames_total = 0;
    let mut layouts_checked = 0;
    for mode in all_modes() {
        for lfe in [false, true] {
            for rate in rates {
                let cfg = Config::new(mode, lfe, rate).unwrap();
                for (si, sig) in signals.iter().enumerate() {
                    let input = input_for(&cfg, *sig, 48_000);
                    let stream = encode_all(cfg, &input);
                    let frames = stream.len() / cfg.frame_bytes();
                    let file = t.scratch.join(format!(
                        "accept-{}-{}-{}-{si}.ac3",
                        mode.acmod(),
                        u8::from(lfe),
                        rate.kbps()
                    ));
                    std::fs::write(&file, &stream).unwrap();
                    let decoded = ffmpeg_decode(&t, &file, &cfg, &[])
                        .unwrap_or_else(|e| panic!("{file:?}: {e}"));
                    assert_eq!(decoded[0].len(), frames * FRAME_SAMPLES, "{file:?}");
                    let (a52_frames, a52_errors, _) = a52_decode(&t, &file);
                    assert_eq!(a52_errors, 0, "{file:?}: a52dec reported errors");
                    assert_eq!(a52_frames, frames, "{file:?}: a52dec frame count");
                    if si == 0 {
                        let probe = ffprobe_stream(&t, &file);
                        let want = |layout: &str| {
                            format!(
                                "codec_name=ac3|sample_rate=48000|channels={}|channel_layout={layout}|bit_rate={}",
                                cfg.channels(),
                                rate.kbps() * 1000
                            )
                        };
                        assert!(
                            probe == want(expected_layout(&cfg))
                                || probe == want(&explicit_layout(&cfg)),
                            "{file:?}: {probe}"
                        );
                        layouts_checked += 1;
                    }
                    cases += 1;
                    frames_total += frames;
                }
            }
        }
    }
    println!(
        "accepted: {cases} streams, {frames_total} frames, by ffmpeg (crccheck+bitstream+buffer+explode) \
         and a52dec with no error; ffprobe layout/rate checked on {layouts_checked} streams"
    );
}

#[test]
#[ignore = "needs ffmpeg; see the module documentation"]
fn ffmpeg_reads_dialnorm() {
    let Some(t) = tools() else { return };
    // FFmpeg's decoder, given -target_level −31, applies 2^((target − (−dialnorm))/6):
    // the gain measured between a decode with and without it gives dialnorm back.
    let input = input_for(
        &Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps192).unwrap(),
        Signal::Pink(-20.0),
        48_000,
    );
    for d in [1u8, 20, 24, 27, 31] {
        let cfg = ConfigBuilder {
            dialnorm: Dialnorm::new(d).unwrap(),
            ..ConfigBuilder::new(AudioCodingMode::STEREO, false, BitRate::Kbps192)
        }
        .build()
        .unwrap();
        let file = t.scratch.join(format!("dialnorm-{d}.ac3"));
        std::fs::write(&file, encode_all(cfg, &input)).unwrap();
        let plain = ffmpeg_decode(&t, &file, &cfg, &[]).unwrap();
        let target = ffmpeg_decode(&t, &file, &cfg, &["-target_level", "-31"]).unwrap();
        let rms = |x: &[f32]| {
            (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
        };
        let gain_db = 20.0 * (rms(&target[0]) / rms(&plain[0])).log10();
        // gain_db = 20·log10(2)·(d − 31)/6
        let inferred = 31.0 + gain_db * 6.0 / (20.0 * 2f64.log10());
        println!(
            "dialnorm {d}: ffmpeg gain at target −31 = {gain_db:.3} dB → dialnorm {inferred:.2}"
        );
        assert!((inferred - f64::from(d)).abs() < 0.05);
    }
}

/// One row of the quality table.
struct Row {
    signal: String,
    config: String,
    ours: f64,
    ours_lp: f64,
    ours_a52: Option<f64>,
    ffmpeg: Option<f64>,
    ffmpeg_lp: Option<f64>,
    ffmpeg_nocpl: Option<f64>,
    /// Clicks only: error energy (dBFS) in the 256 samples that end 192 samples before each
    /// burst, the span a long transform smears noise into and a short one does not.
    pre_ours: Option<f64>,
    pre_ffmpeg: Option<f64>,
}

/// Pre-echo of the click signal (bursts every 4800 samples): mean error power in dBFS over
/// [onset − 448, onset − 192) of every burst after the first.
fn pre_echo_db(reference: &[f32], decoded: &[f32], delay: usize) -> f64 {
    let mut e = 0.0f64;
    let mut n = 0usize;
    for onset in (4800..reference.len()).step_by(4800) {
        for (i, &r) in reference
            .iter()
            .enumerate()
            .take(onset - 192)
            .skip(onset - 448)
        {
            let d = decoded.get(i + delay).copied().unwrap_or(0.0);
            e += (f64::from(r) - f64::from(d)).powi(2);
            n += 1;
        }
    }
    10.0 * (e / n.max(1) as f64 + 1e-30).log10()
}

/// The delay of `decoded` against `reference` in 0..=2048 samples, by least squares over an
/// excerpt.
fn find_delay(reference: &[f32], decoded: &[f32]) -> usize {
    let start = reference.len() / 3;
    let len = 9600.min(reference.len() - start);
    let errors: Vec<f64> = (0..=2048)
        .map(|d| {
            let mut e = 0.0f64;
            for (n, &r) in reference.iter().enumerate().skip(start).take(len) {
                let x = decoded.get(n + d).copied().unwrap_or(0.0);
                e += (f64::from(r) - f64::from(x)).powi(2);
            }
            e
        })
        .collect();
    // A periodic signal matches the excerpt at every period: among the delays as good as the
    // best (within a margin), take the one that fits the whole signal, onset included.
    let min = errors.iter().copied().fold(f64::INFINITY, f64::min);
    let whole = |d: usize| -> f64 {
        reference
            .iter()
            .enumerate()
            .map(|(n, &r)| {
                (f64::from(r) - f64::from(decoded.get(n + d).copied().unwrap_or(0.0))).powi(2)
            })
            .sum()
    };
    (0..errors.len())
        .filter(|&d| errors[d] <= min * 1.5 + 1e-12)
        .map(|d| (whole(d), d))
        .fold((f64::INFINITY, 0), |a, b| if b.0 < a.0 { b } else { a })
        .1
}

fn mean_snr(
    input: &[Vec<f32>],
    out: &[Vec<f32>],
    nf: usize,
    delay: Option<usize>,
    lowpass: bool,
) -> f64 {
    let v: Vec<f64> = (0..nf)
        .map(|c| {
            let d = delay.unwrap_or_else(|| find_delay(&input[c], &out[c]));
            if lowpass {
                snr_db_lowpass(&input[c], &out[c], d, 15_000.0)
            } else {
                snr_db(&input[c], &out[c], d)
            }
        })
        .collect();
    v.iter().sum::<f64>() / v.len() as f64
}

/// Encodes with FFmpeg's `ac3` encoder (its default options unless `extra`), decodes with
/// FFmpeg, returns channels in our order.
fn ffmpeg_encode(
    t: &Tools,
    cfg: &Config,
    input: &[Vec<f32>],
    tag: &str,
    extra: &[&str],
) -> Option<Vec<Vec<f32>>> {
    let layout = ffmpeg_layout_name(cfg)?;
    let pos = ffmpeg_order(cfg);
    let nch = cfg.channels();
    let len = input[0].len();
    let mut raw = vec![0u8; len * nch * 4];
    for (c, ch) in input.iter().enumerate() {
        for (n, s) in ch.iter().enumerate() {
            let at = (n * nch + pos[c]) * 4;
            raw[at..at + 4].copy_from_slice(&s.to_le_bytes());
        }
    }
    let raw_file = t.scratch.join(format!("{tag}.f32"));
    let out_file = t.scratch.join(format!("{tag}-ffmpeg.ac3"));
    std::fs::write(&raw_file, &raw).ok()?;
    let (ok, _, err) = run(Command::new(&t.ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ch_layout",
            layout,
            "-i",
        ])
        .arg(&raw_file)
        .args([
            "-c:a",
            "ac3",
            "-b:a",
            &format!("{}k", cfg.bit_rate().kbps()),
        ])
        .args(extra)
        .arg(&out_file));
    assert!(ok, "ffmpeg encode: {err}");
    ffmpeg_decode(t, &out_file, cfg, &[]).ok()
}

#[test]
#[ignore = "needs ffmpeg and a52dec; see the module documentation"]
fn quality_against_ffmpeg_encoder() {
    let Some(t) = tools() else { return };
    let signals = [
        Signal::Sine(100.0, -20.0),
        Signal::Sine(1000.0, -6.0),
        Signal::Sine(1000.0, -40.0),
        Signal::Sine(10_000.0, -12.0),
        Signal::Sweep(-12.0),
        Signal::White(-20.0),
        Signal::Pink(-20.0),
        Signal::Clicks,
    ];
    let configs: Vec<Config> = vec![
        Config::new(AudioCodingMode::Mono, false, BitRate::Kbps96).unwrap(),
        Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps96).unwrap(),
        Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps192).unwrap(),
        Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps384).unwrap(),
        Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps448).unwrap(),
        Config::new(AudioCodingMode::THREE_TWO, true, BitRate::Kbps192).unwrap(),
        Config::new(AudioCodingMode::THREE_TWO, true, BitRate::Kbps384).unwrap(),
        Config::new(AudioCodingMode::THREE_TWO, true, BitRate::Kbps448).unwrap(),
    ];
    let len = 96_000;
    let mut rows = Vec::new();
    // Silence: every decoder must output exact zeros.
    for cfg in &configs {
        let input = input_for(cfg, Signal::Silence, len);
        let file = t.scratch.join("silence.ac3");
        std::fs::write(&file, encode_all(*cfg, &input)).unwrap();
        let dec = ffmpeg_decode(&t, &file, cfg, &[]).unwrap();
        let peak = dec.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
        let (_, _, a52) = a52_decode(&t, &file);
        let a52_peak = a52.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
        println!(
            "silence {}ch {} kbit/s: ffmpeg peak {peak:e}, a52dec peak {a52_peak:e}",
            cfg.channels(),
            cfg.bit_rate().kbps()
        );
        assert_eq!(peak, 0.0);
        assert_eq!(a52_peak, 0.0);
    }
    for cfg in &configs {
        let name = format!(
            "{} {} kbit/s",
            match (cfg.coding_mode().acmod(), cfg.lfe()) {
                (1, _) => "1/0",
                (2, _) => "2/0",
                (_, true) => "3/2+LFE",
                _ => "3/2",
            },
            cfg.bit_rate().kbps()
        );
        for sig in signals {
            let input = input_for(cfg, sig, len);
            let tag = format!(
                "q-{}-{}-{}",
                cfg.coding_mode().acmod(),
                cfg.bit_rate().kbps(),
                rows.len()
            );
            let file = t.scratch.join(format!("{tag}.ac3"));
            std::fs::write(&file, encode_all(*cfg, &input)).unwrap();
            let ours = ffmpeg_decode(&t, &file, cfg, &[]).unwrap();
            let nf = cfg.nfchans();
            let d = Some(Encoder::DELAY_SAMPLES);
            let ours_a52 = (nf == 2).then(|| {
                let (_, _, a) = a52_decode(&t, &file);
                mean_snr(&input, &a, 2, d, false)
            });
            let ff = ffmpeg_encode(&t, cfg, &input, &tag, &[]);
            let ff_nocpl = if nf > 1 {
                ffmpeg_encode(
                    &t,
                    cfg,
                    &input,
                    &format!("{tag}-nocpl"),
                    &["-channel_coupling", "0"],
                )
            } else {
                None
            };
            let clicks = sig == Signal::Clicks;
            rows.push(Row {
                signal: sig.name(),
                config: name.clone(),
                ours: mean_snr(&input, &ours, nf, d, false),
                ours_lp: mean_snr(&input, &ours, nf, d, true),
                ours_a52,
                ffmpeg: ff.as_ref().map(|o| mean_snr(&input, o, nf, None, false)),
                ffmpeg_lp: ff.as_ref().map(|o| mean_snr(&input, o, nf, None, true)),
                ffmpeg_nocpl: ff_nocpl
                    .as_ref()
                    .map(|o| mean_snr(&input, o, nf, None, false)),
                pre_ours: clicks.then(|| pre_echo_db(&input[0], &ours[0], 256)),
                pre_ffmpeg: ff
                    .as_ref()
                    .filter(|_| clicks)
                    .map(|o| pre_echo_db(&input[0], &o[0], find_delay(&input[0], &o[0]))),
            });
        }
        // Block switching's effect on the transient signal.
        let off = ConfigBuilder {
            block_switching: false,
            ..*cfg.settings()
        }
        .build()
        .unwrap();
        let input = input_for(&off, Signal::Clicks, len);
        let file = t.scratch.join("clicks-noswitch.ac3");
        std::fs::write(&file, encode_all(off, &input)).unwrap();
        let o = ffmpeg_decode(&t, &file, &off, &[]).unwrap();
        rows.push(Row {
            signal: "clicks, block switching off".into(),
            config: name.clone(),
            ours: mean_snr(&input, &o, off.nfchans(), Some(256), false),
            ours_lp: mean_snr(&input, &o, off.nfchans(), Some(256), true),
            ours_a52: None,
            ffmpeg: None,
            ffmpeg_lp: None,
            ffmpeg_nocpl: None,
            pre_ours: Some(pre_echo_db(&input[0], &o[0], 256)),
            pre_ffmpeg: None,
        });
    }
    let f = |v: Option<f64>| v.map_or("–".to_string(), |x| format!("{x:.1}"));
    let mut table = String::from(
        "| config | signal | ours | ours <15 kHz | ours via a52dec | ffmpeg ac3 | ffmpeg <15 kHz | ffmpeg, coupling off | pre-echo ours | pre-echo ffmpeg |\n|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for r in &rows {
        table += &format!(
            "| {} | {} | {:.1} | {:.1} | {} | {} | {} | {} | {} | {} |\n",
            r.config,
            r.signal,
            r.ours,
            r.ours_lp,
            f(r.ours_a52),
            f(r.ffmpeg),
            f(r.ffmpeg_lp),
            f(r.ffmpeg_nocpl),
            f(r.pre_ours),
            f(r.pre_ffmpeg)
        );
    }
    println!("{table}");
    std::fs::write(t.scratch.join("quality.md"), &table).ok();
}
