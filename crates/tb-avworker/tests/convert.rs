//! Converted output (libswscale and libswresample inside the worker): the target size and
//! layout, letterboxing, the matrix, bob deinterlacing, and resampling to 48 kHz in A/52
//! channel order, against the `ffmpeg` program where it says something comparable.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use common::*;
use tb_decoder_client::Decoded;
use tb_decoder_client::proto::wire::{
    Aspect, AudioLayout, AudioSpec, Deinterlace, OutputSpec, StreamOpen, VideoSpec,
};
use tb_media::{CodecId, InputEvent, Matrix, PixelFormat, Range, Rational};

fn spec(w: u32, h: u32, dar: (u32, u32), aspect: Aspect, deint: Deinterlace) -> VideoSpec {
    VideoSpec {
        width: w,
        height: h,
        display_aspect: Rational::new(dar.0, dar.1).unwrap(),
        aspect,
        deinterlace: deint,
    }
}

/// Runs the video packets of `file` through a decoder converting to `spec`; returns the
/// pictures as (pts, packed Y U V).
fn run_video(file: &std::path::Path, spec: VideoSpec) -> Vec<(Option<i64>, Vec<u8>)> {
    let aus = split_aud(&std::fs::read(file).unwrap());
    let mut evs = Vec::new();
    for (i, au) in aus.into_iter().enumerate() {
        evs.push(packet(0, i as i64 * 3000, 3000, i % 30 == 0, au));
    }
    evs.push(end());
    let mut dec = spawn(config());
    dec.try_send_streams(vec![StreamOpen {
        info: video_stream(0, CodecId::H264),
        output: OutputSpec::Video(spec),
    }])
    .unwrap();
    let mut out = Vec::new();
    let mut on = |f: Decoded| {
        let Decoded::Video(v) = f else { panic!() };
        assert_eq!((v.desc.width, v.desc.height), (spec.width, spec.height));
        assert_eq!(v.desc.format, PixelFormat::Yuv420p);
        assert_eq!(v.desc.range, Range::Limited);
        assert_eq!(v.desc.matrix, spec.matrix());
        // no padding: Y, then U, then V
        let n = (spec.width * spec.height) as usize;
        assert_eq!(v.slot_bytes().len(), n * 3 / 2);
        out.push((v.desc.pts, v.slot_bytes().to_vec()));
    };
    send_all(&mut dec, evs, &mut on);
    drain(&mut dec, &mut on, Duration::from_secs(60));
    assert_eq!(dec.stats().violations, 0);
    out
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.len() as f64
}

#[test]
fn hd_to_sd_letterboxed_bt601_like_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("conv-letterbox");
    let src = h264(
        &d,
        "640x360",
        30,
        &[
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ],
    );
    let s = spec(704, 480, (4, 3), Aspect::Fit, Deinterlace::Off);
    let pics = run_video(&src, s);
    assert_eq!(pics.len(), 30);
    for (i, (pts, _)) in pics.iter().enumerate() {
        assert_eq!(*pts, Some(i as i64 * 3000));
    }
    // 16:9 in 4:3: 704 x 360 in the middle, 60 rows of black above and below
    let r = reference(
        &src,
        &[
            "-vf",
            "scale=704:360:flags=lanczos+accurate_rnd:in_color_matrix=bt709:out_color_matrix=bt601:out_range=tv,pad=704:480:0:60:black",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let n = 704 * 480 * 3 / 2;
    assert_eq!(r.len(), n * 30);
    let (_, p0) = &pics[0];
    // the bars are exactly black
    assert!(p0[..704 * 60].iter().all(|v| *v == 16));
    assert!(p0[704 * 420..704 * 480].iter().all(|v| *v == 16));
    assert!(
        p0[704 * 480..704 * 480 + 352 * 30]
            .iter()
            .all(|v| *v == 128)
    );
    let mut worst = 0f64;
    for (i, (_, p)) in pics.iter().enumerate() {
        worst = worst.max(mean_abs_diff(p, &r[i * n..(i + 1) * n]));
    }
    eprintln!("mean absolute difference to ffmpeg 6.1's swscale: {worst:.3}");
    assert!(worst < 1.5, "{worst}");
}

#[test]
fn stretch_to_hd() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("conv-stretch");
    let src = h264(&d, "320x240", 10, &[]);
    let pics = run_video(
        &src,
        spec(1280, 720, (16, 9), Aspect::Stretch, Deinterlace::Off),
    );
    assert_eq!(pics.len(), 10);
}

#[test]
fn bob_makes_two_pictures_of_each_interlaced_one() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("conv-bob");
    let src = d.join("i.h264");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=720x480:rate=30000/1001",
        "-frames:v",
        "20",
        "-vf",
        "setfield=tff",
        "-c:v",
        "libx264",
        "-flags",
        "+ildct+ilme",
        "-x264-params",
        "tff=1",
        "-bf",
        "0",
        "-g",
        "30",
        "-pix_fmt",
        "yuv420p",
        "-bsf:v",
        "h264_metadata=aud=insert",
        "-f",
        "h264",
        src.to_str().unwrap(),
    ]);
    let pics = run_video(
        &src,
        spec(704, 480, (4, 3), Aspect::Stretch, Deinterlace::Bob),
    );
    assert_eq!(pics.len(), 40);
    for (k, (pts, _)) in pics.iter().enumerate() {
        // the second field half a picture (1500 ticks of 90 kHz) after the first
        let want = (k / 2) as i64 * 3000 + (k % 2) as i64 * 1500;
        assert_eq!(*pts, Some(want), "picture {k}");
    }
}

fn run_audio(
    file: &std::path::Path,
    rate: u32,
    layout: AudioLayout,
) -> (Vec<Vec<f32>>, Vec<Option<i64>>) {
    let frames = split_adts(&std::fs::read(file).unwrap());
    let mut info = audio_stream(1, CodecId::Aac);
    info.time_base = Rational::new(1, rate).unwrap();
    info.audio.sample_rate = Some(rate);
    let mut evs = Vec::new();
    for (j, f) in frames.into_iter().enumerate() {
        evs.push(packet(1, j as i64 * 1024, 1024, true, f));
    }
    evs.push(end());
    let mut dec = spawn(config());
    dec.try_send_streams(vec![StreamOpen {
        info,
        output: OutputSpec::Audio(AudioSpec { layout }),
    }])
    .unwrap();
    let ch = usize::from(layout.channels());
    let mut planes = vec![Vec::new(); ch];
    let mut pts = Vec::new();
    let mut on = |f: Decoded| {
        let Decoded::Audio(a) = f else { panic!() };
        assert_eq!(a.desc.sample_rate, 48_000);
        assert_eq!(usize::from(a.desc.channels), ch);
        pts.push(a.desc.pts);
        for (c, p) in planes.iter_mut().enumerate() {
            p.extend(a.channel(c).unwrap());
        }
    };
    send_all(&mut dec, evs, &mut on);
    drain(&mut dec, &mut on, Duration::from_secs(60));
    assert_eq!(dec.stats().violations, 0);
    (planes, pts)
}

fn rms(v: &[f32]) -> f64 {
    (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

#[test]
fn stereo_to_5_1_in_a52_order() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("conv-51");
    let (p, pts) = run_audio(&aac(&d, 2), 48_000, AudioLayout::Surround51);
    // L C R Ls Rs LFE: the tones are on L and R only
    let r: Vec<f64> = p.iter().map(|c| rms(c)).collect();
    eprintln!("rms per channel (L C R Ls Rs LFE): {r:?}");
    // a sine of amplitude 1/8: rms 0.088
    assert!(r[0] > 0.08 && r[2] > 0.08, "{r:?}");
    assert!(
        r[1] < 1e-3 && r[3] < 1e-3 && r[4] < 1e-3 && r[5] < 1e-3,
        "{r:?}"
    );
    assert!(pts.windows(2).all(|w| w[1] > w[0]));
    // 2 s of 48 kHz, give or take the codec's priming and padding
    assert!((p[0].len() as i64 - 96_000).abs() < 2_048, "{}", p[0].len());
}

#[test]
fn resample_44k1_to_48k_stereo() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("conv-44k1");
    let src = d.join("a44.aac");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:sample_rate=44100",
        "-ac",
        "2",
        "-t",
        "2",
        "-c:a",
        "aac",
        "-f",
        "adts",
        src.to_str().unwrap(),
    ]);
    let (p, pts) = run_audio(&src, 44_100, AudioLayout::Stereo);
    let n_in = split_adts(&std::fs::read(&src).unwrap()).len() * 1024;
    let want = n_in as f64 * 48_000.0 / 44_100.0;
    eprintln!(
        "{} samples in, {} out ({want:.0} expected)",
        n_in,
        p[0].len()
    );
    assert!((p[0].len() as f64 - want).abs() < 64.0);
    // timestamps are in the stream's time base (1/44100): 1024 input samples a frame
    // (less the resampler's own delay, a few samples)
    let first = pts.first().copied().flatten().unwrap();
    eprintln!("first output pts {first}");
    assert!(first.abs() < 64, "{first}");
    let level = rms(&p[0]);
    eprintln!("rms {level}");
    assert!(level > 0.03, "silence: {level}");
}

#[test]
fn spec_matrix_follows_the_target_size() {
    assert_eq!(
        spec(1920, 1080, (16, 9), Aspect::Fit, Deinterlace::Off).matrix(),
        Matrix::Bt709
    );
    assert_eq!(
        spec(704, 480, (4, 3), Aspect::Fit, Deinterlace::Off).matrix(),
        Matrix::Bt601
    );
    let _ = InputEvent::End(tb_media::EndReason::Closed);
}
