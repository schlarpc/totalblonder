//! The loudness meter against ffmpeg on the same files: the `ebur128` filter (libavfilter's own
//! BS.1770 implementation, `peak=true` for true peak) and the `loudnorm` filter's measurement
//! pass (integrated loudness and true peak to two decimals). Independent implementations of
//! the same recommendation; agreement within 0.1 LU and 0.2 dB is expected.
//!
//! Needs the `ffmpeg` program, which the Nix sandbox does not have, so the test is ignored and
//! also passes (with a note) when ffmpeg is absent. Run it with
//! `nix develop -c cargo nextest run -p tb-audio --run-ignored all --no-capture -E 'binary(ffmpeg)'`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::path::Path;
use std::process::Command;
use tb_audio::clock::AudioFrame;
use tb_audio::layout::{InputLayout, OutputLayout};
use tb_audio::loudness::LoudnessMeter;
use tb_audio::pipeline::{InputFormat, TrackConfig, TrackPipeline};
use tb_audio::resample::InputRate;
use tb_audio::signal::{db_to_amp, program, sine};
use tb_timeline::{Source, Ticks27};

/// Write planes (in WAVE channel order) as a 32-bit float WAVE_FORMAT_EXTENSIBLE file with the
/// given channel mask.
fn write_wav(path: &Path, planes: &[Vec<f32>], mask: u32) {
    let ch = planes.len() as u16;
    let n = planes[0].len();
    let data_len = (n * planes.len() * 4) as u32;
    let mut b = Vec::with_capacity(data_len as usize + 80);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(4 + 8 + 40 + 8 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&0xFFFEu16.to_le_bytes());
    b.extend_from_slice(&ch.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&(48_000 * 4 * u32::from(ch)).to_le_bytes());
    b.extend_from_slice(&(4 * ch).to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&22u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&mask.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
    b.extend_from_slice(&[
        0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ]);
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for p in planes {
            b.extend_from_slice(&p[i].to_le_bytes());
        }
    }
    std::fs::write(path, b).unwrap();
}

/// ffmpeg's readings: (ebur128 I, ebur128 true peak, loudnorm I, loudnorm true peak).
fn ffmpeg(path: &Path) -> (f64, f64, f64, f64) {
    let run = |filter: &str| -> String {
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostats", "-i"])
            .arg(path)
            .args(["-af", filter, "-f", "null", "-"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let after = |err: &str, key: &str, from: &str| -> f64 {
        let s = &err[err.find(from).unwrap_or(0)..];
        let i = s.find(key).unwrap_or_else(|| panic!("{key} in {err}")) + key.len();
        s[i..]
            .trim_start_matches([' ', ':', '"'])
            .split(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    let e = run("ebur128=peak=true:framelog=quiet");
    let l = run("loudnorm=print_format=json");
    (
        after(&e, "I:", "Summary"),
        after(&e, "Peak:", "True peak"),
        after(&l, "\"input_i\"", "{"),
        after(&l, "\"input_tp\"", "{"),
    )
}

fn ours(planes_a52: &[Vec<f32>], layout: OutputLayout) -> (f64, f64) {
    let mut m = LoudnessMeter::new(layout, true);
    let r: Vec<&[f32]> = planes_a52.iter().map(|p| p.as_slice()).collect();
    m.push(&r).unwrap();
    (m.integrated().unwrap().0, m.true_peak().unwrap().0)
}

fn tone_segments(seg: &[(f64, f64)]) -> Vec<f32> {
    let mut out = Vec::new();
    for &(db, secs) in seg {
        let n = (secs * 48_000.0) as usize;
        let start = out.len();
        let w = 2.0 * std::f64::consts::PI * 1000.0 / 48_000.0;
        out.extend((0..n).map(|i| (db_to_amp(db) * (w * (start + i) as f64).sin()) as f32));
    }
    out
}

fn faded(mut x: Vec<f32>) -> Vec<f32> {
    let n = x.len();
    for i in 0..480 {
        let g = (0.5 - 0.5 * (std::f64::consts::PI * i as f64 / 480.0).cos()) as f32;
        x[i] *= g;
        x[n - 1 - i] *= g;
    }
    x
}

/// The output of our pipeline for program noise at `lufs` (an M9 input), 60 s.
fn pipeline_output(lufs: f64) -> Vec<Vec<f32>> {
    let mut x = program(11, 2, 48_000 * 60);
    let (l, _) = ours(&x, OutputLayout::Stereo);
    let g = 10f32.powf(((lufs - l) / 20.0) as f32);
    for p in &mut x {
        for v in p.iter_mut() {
            *v *= g;
        }
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
    let mut out = [Vec::new(), Vec::new()];
    let mut f = AudioFrame::new(OutputLayout::Stereo);
    for (k, s) in (0..x[0].len()).step_by(1024).enumerate() {
        let e = (s + 1024).min(x[0].len());
        t.push(
            Source::Live,
            Ticks27(27_000_000 + s as i64 * 562 + s as i64 / 2),
            &[&x[0][s..e], &x[1][s..e]],
        )
        .unwrap();
        while (out[0].len() + 1536) as i64 * 1125 / 2 <= s as i64 * 1125 / 2 {
            t.next_frame(&mut f);
            out[0].extend_from_slice(f.plane(0));
            out[1].extend_from_slice(f.plane(1));
        }
        let _ = k;
    }
    // Drop the first second of silence (the jitter buffer).
    out.iter().map(|p| p[48_000..].to_vec()).collect()
}

#[test]
#[ignore = "needs ffmpeg: run with --run-ignored all (see the file's header)"]
fn meter_agrees_with_ffmpeg() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg not found: skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // name, planes in A/52 order, layout, WAVE order of the A/52 planes, mask
    let mut cases: Vec<(String, Vec<Vec<f32>>, OutputLayout)> = Vec::new();
    let st = |x: Vec<f32>| vec![x.clone(), x];
    cases.push((
        "3341-1 -23 dBFS".into(),
        st(tone_segments(&[(-23.0, 20.0)])),
        OutputLayout::Stereo,
    ));
    cases.push((
        "3341-2 -33 dBFS".into(),
        st(tone_segments(&[(-33.0, 20.0)])),
        OutputLayout::Stereo,
    ));
    cases.push((
        "3341-3 -36/-23/-36".into(),
        st(tone_segments(&[
            (-36.0, 10.0),
            (-23.0, 60.0),
            (-36.0, 10.0),
        ])),
        OutputLayout::Stereo,
    ));
    cases.push((
        "3341-4 -72/-36/-23/-36/-72".into(),
        st(tone_segments(&[
            (-72.0, 10.0),
            (-36.0, 10.0),
            (-23.0, 60.0),
            (-36.0, 10.0),
            (-72.0, 10.0),
        ])),
        OutputLayout::Stereo,
    ));
    cases.push((
        "3341-5 -26/-20/-26".into(),
        st(tone_segments(&[
            (-26.0, 20.0),
            (-20.0, 20.1),
            (-26.0, 20.0),
        ])),
        OutputLayout::Stereo,
    ));
    {
        let n = 48_000 * 20;
        let l = sine(1000.0, db_to_amp(-28.0), 48_000, n, 0.0);
        let c = sine(1000.0, db_to_amp(-24.0), 48_000, n, 0.0);
        let s = sine(1000.0, db_to_amp(-30.0), 48_000, n, 0.0);
        let z = vec![0.0; n];
        cases.push((
            "3341-6 5.0 channels".into(),
            vec![l.clone(), c, l, s.clone(), s, z],
            OutputLayout::FivePointOne,
        ));
    }
    for (f, ph, db) in [
        (12_000.0, 45.0f64, -6.0),
        (8_000.0, 60.0, -6.0),
        (6_000.0, 67.5, -6.0),
        (19_997.0, 10.0, -3.0),
    ] {
        cases.push((
            format!("true peak {f} Hz {ph} deg {db} dBTP"),
            st(faded(sine(
                f,
                db_to_amp(db),
                48_000,
                48_000 * 5,
                ph.to_radians(),
            ))),
            OutputLayout::Stereo,
        ));
    }
    cases.push((
        "program noise stereo".into(),
        program(21, 2, 48_000 * 60),
        OutputLayout::Stereo,
    ));
    cases.push((
        "program noise 5.1".into(),
        program(22, 6, 48_000 * 60),
        OutputLayout::FivePointOne,
    ));
    for lufs in [-10.0, -24.0, -35.0] {
        cases.push((
            format!("pipeline output, input {lufs} LUFS"),
            pipeline_output(lufs),
            OutputLayout::Stereo,
        ));
    }
    eprintln!(
        "{:44} {:>8} {:>8} {:>8} | {:>7} {:>7} {:>7}",
        "case", "I ours", "ebur128", "loudnrm", "TP ours", "ebur128", "loudnrm"
    );
    for (i, (name, planes, layout)) in cases.iter().enumerate() {
        let (oi, otp) = ours(planes, *layout);
        let path = dir.path().join(format!("{i}.wav"));
        match layout {
            OutputLayout::Stereo => write_wav(&path, planes, 0x3),
            // A/52 L C R Ls Rs LFE -> WAVE L R C LFE BL BR
            OutputLayout::FivePointOne => {
                let p = planes;
                let w = vec![
                    p[0].clone(),
                    p[2].clone(),
                    p[1].clone(),
                    p[5].clone(),
                    p[3].clone(),
                    p[4].clone(),
                ];
                write_wav(&path, &w, 0x3F);
            }
        }
        let (ei, etp, li, ltp) = ffmpeg(&path);
        eprintln!("{name:44} {oi:8.2} {ei:8.1} {li:8.2} | {otp:7.2} {etp:7.1} {ltp:7.2}");
        // ebur128 prints one decimal: allow its rounding on top of 0.1 LU.
        assert!((oi - ei).abs() <= 0.15, "{name}: I {oi} vs ebur128 {ei}");
        // loudnorm measures at 192 kHz after its own resampling and reads 0.05–0.15 LU low on
        // the EBU cases, whose true value is known (−23.0): a looser bound for it.
        assert!((oi - li).abs() <= 0.2, "{name}: I {oi} vs loudnorm {li}");
        assert!(
            (otp - etp).abs() <= 0.25,
            "{name}: TP {otp} vs ebur128 {etp}"
        );
        assert!(
            (otp - ltp).abs() <= 0.2,
            "{name}: TP {otp} vs loudnorm {ltp}"
        );
    }
}
