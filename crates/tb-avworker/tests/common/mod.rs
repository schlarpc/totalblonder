//! Fixtures and helpers of the worker's integration tests: clips encoded with the `ffmpeg`
//! program at test time (skipped where it is absent, as in the Nix build sandbox), split into
//! access units here, and a decoder pool on the worker binary of this build.
#![allow(dead_code, reason = "each test binary uses a part")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tb_decoder_client::{Decoded, Decoder, DecoderPool, Event, PoolConfig};
use tb_media::{
    AudioHint, CodecId, EndReason, InputEvent, Packet, Rational, StreamIndex, StreamInfo, VideoHint,
};

pub const WORKER: &str = env!("CARGO_BIN_EXE_tb-avworker");

/// A scratch directory of this test process.
pub fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tb-avworker-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Whether the `ffmpeg` program is there; if not, the test is skipped.
pub fn have_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIPPED: no ffmpeg program on PATH");
    }
    ok
}

pub fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

/// An H.264 elementary stream (Annex B, an access unit delimiter before each picture, no B
/// pictures) of `frames` pictures of testsrc2 at `size`.
pub fn h264(d: &Path, size: &str, frames: u32, extra: &[&str]) -> PathBuf {
    let out = d.join(format!("v-{size}-{frames}.h264"));
    if !out.exists() {
        let n = frames.to_string();
        let src = format!("testsrc2=size={size}:rate=30");
        let mut a = vec![
            "-f",
            "lavfi",
            "-i",
            &src,
            "-frames:v",
            &n,
            "-c:v",
            "libx264",
            "-bf",
            "0",
            "-g",
            "30",
            "-pix_fmt",
            "yuv420p",
            "-bsf:v",
            "h264_metadata=aud=insert",
        ];
        a.extend_from_slice(extra);
        let o = out.to_str().unwrap().to_owned();
        a.extend(["-f", "h264", &o]);
        ffmpeg(&a);
    }
    out
}

/// AAC in ADTS, stereo 48 kHz, `secs` seconds of two tones.
pub fn aac(d: &Path, secs: u32) -> PathBuf {
    let out = d.join(format!("a-{secs}.aac"));
    if !out.exists() {
        let t = secs.to_string();
        let o = out.to_str().unwrap().to_owned();
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=660:sample_rate=48000",
            "-filter_complex",
            "[0][1]amerge=inputs=2",
            "-t",
            &t,
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-f",
            "adts",
            &o,
        ]);
    }
    out
}

/// What the `ffmpeg` program decodes from a file: raw bytes in `fmt`.
pub fn reference(input: &Path, args: &[&str]) -> Vec<u8> {
    let out = input.with_extension(format!("ref-{}", args.join("_").replace('/', "")));
    let i = input.to_str().unwrap().to_owned();
    let o = out.to_str().unwrap().to_owned();
    let mut a = vec!["-i", &i];
    a.extend_from_slice(args);
    a.push(&o);
    ffmpeg(&a);
    std::fs::read(out).unwrap()
}

/// Access units of an Annex B stream: split before every access unit delimiter.
pub fn split_aud(b: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 < b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 && b[i + 3] & 0x1f == 9 {
            // include a leading zero of a four-byte start code
            starts.push(if i > 0 && b[i - 1] == 0 { i - 1 } else { i });
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(b.len());
    starts.windows(2).map(|w| b[w[0]..w[1]].to_vec()).collect()
}

/// ADTS frames.
pub fn split_adts(b: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 7 <= b.len() {
        assert!(b[i] == 0xff && b[i + 1] & 0xf0 == 0xf0, "ADTS sync at {i}");
        let len = (usize::from(b[i + 3] & 3) << 11)
            | (usize::from(b[i + 4]) << 3)
            | usize::from(b[i + 5] >> 5);
        out.push(b[i..i + len].to_vec());
        i += len;
    }
    out
}

pub fn video_stream(i: u16, codec: CodecId) -> StreamInfo {
    StreamInfo {
        index: StreamIndex(i),
        codec,
        time_base: Rational::MPEG_90K,
        extradata: vec![],
        video: VideoHint::default(),
        audio: AudioHint::default(),
        language: None,
    }
}

pub fn audio_stream(i: u16, codec: CodecId) -> StreamInfo {
    StreamInfo {
        index: StreamIndex(i),
        codec,
        time_base: Rational::new(1, 48_000).unwrap(),
        extradata: vec![],
        video: VideoHint::default(),
        audio: AudioHint {
            sample_rate: Some(48_000),
            channels: Some(2),
        },
        language: None,
    }
}

pub fn packet(stream: u16, pts: i64, dur: i64, key: bool, data: Vec<u8>) -> InputEvent {
    InputEvent::Packet(Packet {
        stream: StreamIndex(stream),
        pts: Some(pts),
        dts: Some(pts),
        duration: Some(dur),
        keyframe: key,
        data,
    })
}

pub fn end() -> InputEvent {
    InputEvent::End(EndReason::Closed)
}

pub fn config() -> PoolConfig {
    let mut c = PoolConfig::new(WORKER);
    c.policy.stderr = tb_sandbox::Stderr::Inherit;
    // tests take their time with frames
    c.slot_wait_ms = 5_000;
    c.watchdog.stall_ms = 10_000;
    c
}

pub fn spawn(c: PoolConfig) -> Decoder {
    let mut d = DecoderPool::new(c).unwrap().spawn("test").unwrap();
    assert!(d.wait_ready(Duration::from_secs(10)), "worker not ready");
    d
}

/// Sends every event, retrying while the input is full (a test may wait; the server drops).
pub fn send_all(d: &mut Decoder, evs: Vec<InputEvent>, out: &mut impl FnMut(Decoded)) {
    for ev in evs {
        let mut tries = 0;
        loop {
            match d.try_send(ev.clone()) {
                Ok(()) => break,
                Err(tb_decoder_client::SendError::Full) => {
                    while let Some(f) = d.poll(Some(Instant::now() + Duration::from_millis(1))) {
                        out(f);
                    }
                    tries += 1;
                    assert!(tries < 100_000, "input stays full");
                }
                Err(e) => panic!("{e:?}"),
            }
        }
        while let Some(f) = d.poll(None) {
            out(f);
        }
    }
}

/// Takes frames until `Drained`, or panics after `timeout`.
pub fn drain(d: &mut Decoder, out: &mut impl FnMut(Decoded), timeout: Duration) -> Vec<Event> {
    let end = Instant::now() + timeout;
    let mut evs = Vec::new();
    loop {
        if let Some(f) = d.poll(Some(Instant::now() + Duration::from_millis(50))) {
            out(f);
        }
        let new: Vec<Event> = d.events().collect();
        let done = new.contains(&Event::Drained);
        evs.extend(new);
        if done {
            while let Some(f) = d.poll(None) {
                out(f);
            }
            return evs;
        }
        assert!(
            Instant::now() < end,
            "no Drained; stats {:?}; events {evs:?}",
            d.stats()
        );
    }
}

/// Collects events until `pred` holds for one, or panics after `timeout`.
pub fn wait_event(d: &mut Decoder, timeout: Duration, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let end = Instant::now() + timeout;
    let mut evs = Vec::new();
    loop {
        // frames are dropped at once: their slots go back
        let _ = d.poll(Some(Instant::now() + Duration::from_millis(20)));
        let new: Vec<Event> = d.events().collect();
        let hit = new.iter().any(&pred);
        evs.extend(new);
        if hit {
            return evs;
        }
        assert!(Instant::now() < end, "event not seen; events {evs:?}");
    }
}

/// The picture's planes, rows without padding (Y, U, V).
pub fn packed_yuv(f: &tb_decoder_client::VideoFrame) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..f.desc.planes.len() {
        let mut r = 0;
        while let Some(row) = f.row(i, r) {
            v.extend_from_slice(row);
            r += 1;
        }
    }
    v
}

/// The H.264 + AAC session of `frames` pictures of `size`.
pub fn av_session(d: &Path, size: &str, frames: u32) -> Vec<InputEvent> {
    let v = split_aud(&std::fs::read(h264(d, size, frames, &[])).unwrap());
    assert_eq!(v.len(), frames as usize);
    let a = split_adts(&std::fs::read(aac(d, frames.div_ceil(30))).unwrap());
    let mut evs = vec![InputEvent::Streams(vec![
        video_stream(0, CodecId::H264),
        audio_stream(1, CodecId::Aac),
    ])];
    // interleaved by time: picture i at i/30 s, AAC frame j at j*1024/48000 s
    let (mut i, mut j) = (0usize, 0usize);
    while i < v.len() || j < a.len() {
        let tv = i as f64 / 30.0;
        let ta = j as f64 * 1024.0 / 48_000.0;
        if j >= a.len() || (i < v.len() && tv <= ta) {
            evs.push(packet(0, i as i64 * 3000, 3000, i % 30 == 0, v[i].clone()));
            i += 1;
        } else {
            evs.push(packet(1, j as i64 * 1024, 1024, true, a[j].clone()));
            j += 1;
        }
    }
    evs.push(end());
    evs
}
