//! A real publisher's bytes (recorded from ffmpeg 6.1 by `tests/clients.rs::record_fixture`:
//! 1 s of 64x36 H.264 at 30 fps with a key frame every 10, B-frames, mono AAC at 22.05 kHz),
//! replayed with no ffmpeg: every split point gives the same events, and every truncation
//! ends cleanly.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use tb_media::{CodecId, EndReason, InputEvent, StreamIndex};
use tb_rtmp::{AllowAll, CloseReason, ServerConfig, ServerSession};

const FIXTURE: &[u8] = include_bytes!("fixtures/ffmpeg-h264-aac.rtmp");

fn run(chunks: &[&[u8]]) -> (Vec<InputEvent>, Option<CloseReason>) {
    let mut s = ServerSession::new(ServerConfig::default(), AllowAll, Duration::ZERO);
    let mut ev = Vec::new();
    for c in chunks {
        ev.extend(s.on_bytes(Duration::ZERO, c));
    }
    ev.extend(s.on_eof(Duration::ZERO));
    (ev, s.close_reason().cloned())
}

#[test]
fn fixture_events() {
    let (ev, close) = run(&[FIXTURE]);
    assert_eq!(close, Some(CloseReason::Finished));
    let InputEvent::Streams(s) = &ev[0] else {
        panic!()
    };
    assert_eq!(s.len(), 2);
    assert_eq!((s[0].codec, s[1].codec), (CodecId::H264, CodecId::Aac));
    assert_eq!((s[0].video.width, s[0].video.height), (Some(64), Some(36)));
    // avcC starts with configurationVersion 1.
    assert_eq!(s[0].extradata[0], 1);
    assert_eq!(s[1].audio.sample_rate, Some(22050));
    assert_eq!(s[1].audio.channels, Some(1));
    assert_eq!(ev.last(), Some(&InputEvent::End(EndReason::Closed)));
    let per = common::check_shape(&ev);
    let v = &per[&StreamIndex(0)];
    assert_eq!(v.len(), 30);
    let keys: Vec<usize> = (0..30).filter(|&i| v[i].keyframe).collect();
    assert_eq!(keys, [0, 10, 20]);
    assert!(v.windows(2).all(|w| w[0].dts < w[1].dts));
    assert!(v.iter().any(|p| p.pts > p.dts), "B-frames");
    // Each video packet is length-prefixed NAL units that exactly fill it.
    for p in v {
        let mut at = 0;
        while at < p.data.len() {
            let n = u32::from_be_bytes(p.data[at..at + 4].try_into().unwrap()) as usize;
            at += 4 + n;
        }
        assert_eq!(at, p.data.len());
    }
    assert!(per[&StreamIndex(1)].len() >= 20);
}

#[test]
fn fixture_every_split_point() {
    let (whole, _) = run(&[FIXTURE]);
    for k in 0..=FIXTURE.len() {
        let (ev, _) = run(&[&FIXTURE[..k], &FIXTURE[k..]]);
        assert_eq!(ev, whole, "split at {k}");
    }
    let bytes: Vec<&[u8]> = FIXTURE.chunks(1).collect();
    assert_eq!(run(&bytes).0, whole);
    for n in [2, 3, 7, 127, 128, 129, 1536, 1537, 4096] {
        let chunks: Vec<&[u8]> = FIXTURE.chunks(n).collect();
        assert_eq!(run(&chunks).0, whole, "chunks of {n}");
    }
}

#[test]
fn fixture_every_truncation_ends_cleanly() {
    for k in 0..FIXTURE.len() {
        let (ev, close) = run(&[&FIXTURE[..k]]);
        assert!(close.is_some());
        if !ev.is_empty() {
            common::check_shape(&ev);
            assert!(matches!(ev.last(), Some(InputEvent::End(_))));
        }
    }
}
