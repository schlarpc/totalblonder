//! The real worker, sandboxed, decodes H.264 and AAC: counts, sizes, timestamps, and the
//! pixels against the `ffmpeg` program's own decode of the same stream.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use common::*;
use tb_decoder_client::{Decoded, Event};
use tb_media::{PixelFormat, Range};

#[test]
fn h264_and_aac_match_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("h264-aac");
    let frames = 60;
    let evs = av_session(&d, "320x240", frames);
    let want_v = reference(
        &h264(&d, "320x240", frames, &[]),
        &["-f", "rawvideo", "-pix_fmt", "yuv420p"],
    );
    let frame_bytes = 320 * 240 * 3 / 2;
    assert_eq!(want_v.len(), frame_bytes * frames as usize);
    let r = reference(&aac(&d, 2), &["-f", "f32le", "-c:a", "pcm_f32le"]);
    let want_a: Vec<f32> = r
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();

    let mut dec = spawn(config());
    eprintln!("confinement: {}", dec.confinement().unwrap());
    let mut video = 0usize;
    let mut audio: Vec<Vec<f32>> = vec![Vec::new(), Vec::new()];
    let mut last_audio_pts = None;
    let mut on_frame = |f: Decoded| match f {
        Decoded::Video(v) => {
            assert_eq!((v.desc.width, v.desc.height), (320, 240));
            assert_eq!(v.desc.format, PixelFormat::Yuv420p);
            assert_eq!(v.desc.pts, Some(video as i64 * 3000), "picture {video}");
            assert!(!v.desc.corrupt);
            assert_ne!(v.desc.range, Range::Full);
            let got = packed_yuv(&v);
            let want = &want_v[video * frame_bytes..(video + 1) * frame_bytes];
            assert!(got == want, "picture {video} differs from ffmpeg's");
            video += 1;
        }
        Decoded::Audio(a) => {
            assert_eq!((a.desc.sample_rate, a.desc.channels), (48_000, 2));
            if let (Some(p), Some(q)) = (last_audio_pts, a.desc.pts) {
                assert!(q > p, "audio timestamps go forward");
            }
            last_audio_pts = a.desc.pts;
            for (c, buf) in audio.iter_mut().enumerate() {
                buf.extend(a.channel(c).unwrap());
            }
        }
    };
    send_all(&mut dec, evs, &mut on_frame);
    drain(&mut dec, &mut on_frame, Duration::from_secs(30));
    assert_eq!(video, frames as usize);
    // the same samples as ffmpeg's decoder (float AAC: equal to within rounding)
    assert_eq!(audio[0].len() * 2, want_a.len(), "audio sample count");
    let mut max_err = 0f32;
    for (i, pair) in want_a.as_chunks::<2>().0.iter().enumerate() {
        max_err = max_err
            .max((pair[0] - audio[0][i]).abs())
            .max((pair[1] - audio[1][i]).abs());
    }
    eprintln!(
        "audio: {} samples, max difference {max_err}",
        audio[0].len()
    );
    assert!(max_err < 1e-4, "audio differs from ffmpeg's by {max_err}");
    let s = dec.stats();
    eprintln!("stats: {s:?}");
    assert_eq!(s.violations, 0);
    assert_eq!(s.dropped_full, 0);
    assert_eq!(s.worker.dropped_no_slot, 0);
    dec.shutdown(Duration::from_secs(2));
}

#[test]
fn frames_outlive_their_worker_and_slots_come_back() {
    if !have_ffmpeg() {
        return;
    }
    let d = dir("slots");
    let mut c = config();
    // fewer slots than pictures: the session completes only if slots are given back
    c.video_slots = 2;
    let mut dec = spawn(c);
    let mut evs = av_session(&d, "320x240", 60);
    evs.retain(|e| !matches!(e, tb_media::InputEvent::Packet(p) if p.stream.0 == 1));
    let mut kept = None;
    let mut seen = 0;
    let mut on_frame = |f: Decoded| {
        seen += 1;
        if kept.is_none() {
            kept = Some(f);
        }
    };
    send_all(&mut dec, evs, &mut on_frame);
    drain(&mut dec, &mut on_frame, Duration::from_secs(60));
    assert_eq!(seen, 60);
    // the worker goes; the frame held stays readable, with the same pixels
    let Some(Decoded::Video(v)) = kept else {
        panic!()
    };
    let before = packed_yuv(&v);
    dec.shutdown(Duration::from_secs(2));
    assert_eq!(packed_yuv(&v), before);
    assert_eq!(before.len(), 320 * 240 * 3 / 2);
}

#[test]
fn refused_codecs_are_reported() {
    let mut dec = spawn(config());
    dec.try_send(tb_media::InputEvent::Streams(vec![video_stream(
        3,
        tb_media::CodecId::Text,
    )]))
    .unwrap();
    let evs = wait_event(&mut dec, Duration::from_secs(5), |e| {
        matches!(e, Event::StreamRefused { .. })
    });
    assert!(evs.iter().any(|e| matches!(
        e,
        Event::StreamRefused {
            stream,
            reason: tb_decoder_client::proto::wire::Refusal::NotAllowed
        } if stream.0 == 3
    )));
}
