//! Streams made by the real `ffmpeg` (`-f mpegts`, `-f rtp_mpegts`, UDP), demuxed and compared
//! with what `ffprobe` reads from the same bytes. Skipped (with a note on stderr) when ffmpeg
//! or ffprobe is not on PATH, as in the Nix build sandbox.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::net::UdpSocket;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tb_media::{CodecId, InputEvent, Packet, StreamIndex, StreamInfo};
use tb_tsin::{DemuxConfig, Demuxer, Transport, TsEvent, TsInput, TsInputConfig};

fn have(tool: &str) -> bool {
    let ok = Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("{tool} not found: test skipped");
    }
    ok
}

/// ffmpeg (and ffprobe if `probe`) on PATH, with these encoders.
fn ready(probe: bool, encoders: &[&str]) -> bool {
    if !have("ffmpeg") || (probe && !have("ffprobe")) {
        return false;
    }
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&out.stdout);
    for e in encoders {
        if !list.contains(&format!(" {e} ")) {
            eprintln!("ffmpeg has no {e}: test skipped");
            return false;
        }
    }
    true
}

const VIDEO_SRC: [&str; 4] = ["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30"];
const AUDIO_SRC: [&str; 4] = ["-f", "lavfi", "-i", "sine=sample_rate=48000"];

/// ffmpeg writes a TS to stdout.
fn ffmpeg_ts(args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(VIDEO_SRC)
        .args(AUDIO_SRC)
        .args(args)
        .args(["-t", "2", "-f", "mpegts", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn demux(bytes: &[u8]) -> Vec<TsEvent> {
    let mut d = Demuxer::new(DemuxConfig::default());
    // Uneven pieces, as a byte stream.
    for c in bytes.chunks(1000) {
        d.push_bytes(c);
    }
    d.flush();
    let s = d.stats();
    assert_eq!(
        (s.cc_errors, s.malformed, s.bad_sections),
        (0, 0, 0),
        "{s:?}"
    );
    d.take_events()
}

fn streams(ev: &[TsEvent]) -> Vec<StreamInfo> {
    let all: Vec<&Vec<StreamInfo>> = ev
        .iter()
        .filter_map(|e| match &e.event {
            InputEvent::Streams(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(all.len(), 1, "one announcement");
    all[0].clone()
}

fn packets(ev: &[TsEvent], index: StreamIndex) -> Vec<Packet> {
    ev.iter()
        .filter_map(|e| match &e.event {
            InputEvent::Packet(p) if p.stream == index => {
                assert!(!e.after_loss);
                Some(p.clone())
            }
            _ => None,
        })
        .collect()
}

/// ffprobe's CSV rows of `entries` for the packets of stream `select` in `bytes`.
fn ffprobe(bytes: &[u8], name: &str, select: &str, entries: &str) -> Vec<Vec<String>> {
    let path = std::env::temp_dir().join(format!("tb-tsin-{name}-{}.ts", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", select, "-show_entries"])
        .args([&format!("packet={entries}"), "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.split(',').map(str::to_owned).collect())
        .collect()
}

/// (pts, dts, key) of the first video stream's packets, as ffprobe reads `bytes`.
fn ffprobe_video(bytes: &[u8], name: &str) -> Vec<(i64, i64, bool)> {
    ffprobe(bytes, name, "v:0", "pts,dts,flags")
        .iter()
        .map(|f| {
            (
                f[0].parse().unwrap(),
                f[1].parse().unwrap(),
                f[2].starts_with('K'),
            )
        })
        .collect()
}

fn check_video_against_ffprobe(bytes: &[u8], ev: &[TsEvent], index: StreamIndex, name: &str) {
    let v = packets(ev, index);
    let ours: Vec<(i64, i64, bool)> = v
        .iter()
        .map(|p| (p.pts.unwrap(), p.dts.unwrap(), p.keyframe))
        .collect();
    assert_eq!(
        ours,
        ffprobe_video(bytes, name),
        "{name}: video as ffprobe reads it"
    );
    assert_eq!(v.len(), 60, "{name}: 2 s at 30 fps");
    assert!(v.windows(2).all(|w| w[0].dts < w[1].dts));
}

/// ADTS frames in the concatenated audio payloads (ISO/IEC 14496-3 §1.A.2: syncword 0xFFF,
/// frame_length in 13 bits).
fn adts_frames(data: &[u8]) -> usize {
    let mut at = 0;
    let mut n = 0;
    while at + 7 <= data.len() {
        assert_eq!(
            (data[at], data[at + 1] & 0xF0),
            (0xFF, 0xF0),
            "ADTS sync at {at}"
        );
        let len = (usize::from(data[at + 3] & 3) << 11)
            | (usize::from(data[at + 4]) << 3)
            | usize::from(data[at + 5] >> 5);
        at += len;
        n += 1;
    }
    assert_eq!(at, data.len());
    n
}

#[test]
fn h264_aac() {
    if !ready(true, &["libx264", "aac"]) {
        return;
    }
    let bytes = ffmpeg_ts(&["-c:v", "libx264", "-g", "30", "-c:a", "aac"]);
    let ev = demux(&bytes);
    let s = streams(&ev);
    let codecs: Vec<CodecId> = s.iter().map(|s| s.codec).collect();
    assert_eq!(codecs, [CodecId::H264, CodecId::Aac]);
    check_video_against_ffprobe(&bytes, &ev, s[0].index, "h264");
    let v = packets(&ev, s[0].index);
    let keys: Vec<usize> = (0..v.len()).filter(|&i| v[i].keyframe).collect();
    assert_eq!(keys, [0, 30]);
    let a = packets(&ev, s[1].index);
    let all: Vec<u8> = a.iter().flat_map(|p| p.data.clone()).collect();
    let frames = adts_frames(&all);
    // 2 s at 48 kHz, 1024 samples a frame.
    assert!((92..=96).contains(&frames), "{frames} AAC frames");
    eprintln!(
        "h264+aac: {} video packets, {} audio PES, {frames} ADTS frames",
        v.len(),
        a.len()
    );
}

#[test]
fn hevc() {
    if !ready(true, &["libx265"]) {
        return;
    }
    let bytes = ffmpeg_ts(&[
        "-c:v",
        "libx265",
        "-g",
        "30",
        "-x265-params",
        "log-level=error",
        "-an",
    ]);
    let ev = demux(&bytes);
    let s = streams(&ev);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].codec, CodecId::Hevc);
    check_video_against_ffprobe(&bytes, &ev, s[0].index, "hevc");
}

#[test]
fn mpeg2_ac3_with_language() {
    if !ready(true, &["mpeg2video", "ac3"]) {
        return;
    }
    let bytes = ffmpeg_ts(&[
        "-c:v",
        "mpeg2video",
        "-g",
        "15",
        "-bf",
        "2",
        "-c:a",
        "ac3",
        "-metadata:s:a:0",
        "language=spa",
    ]);
    let ev = demux(&bytes);
    let s = streams(&ev);
    let codecs: Vec<CodecId> = s.iter().map(|s| s.codec).collect();
    assert_eq!(codecs, [CodecId::Mpeg2Video, CodecId::Ac3]);
    assert_eq!(s[1].language, Some(*b"spa"));
    check_video_against_ffprobe(&bytes, &ev, s[0].index, "mpeg2");
    let a = packets(&ev, s[1].index);
    // AC-3 sync frames: syncword 0x0B77 at the start of each PES payload.
    assert!(a.iter().all(|p| p.data.starts_with(&[0x0B, 0x77])));
}

#[test]
fn eac3_and_opus() {
    if !ready(false, &["libx264", "eac3", "libopus"]) {
        return;
    }
    let bytes = ffmpeg_ts(&["-c:v", "libx264", "-c:a", "eac3"]);
    let ev = demux(&bytes);
    let s = streams(&ev);
    assert_eq!(s[1].codec, CodecId::Eac3);
    assert!(!packets(&ev, s[1].index).is_empty());

    let bytes = ffmpeg_ts(&["-c:v", "libx264", "-c:a", "libopus", "-ac", "2"]);
    let ev = demux(&bytes);
    let s = streams(&ev);
    assert_eq!(s[1].codec, CodecId::Opus);
    assert_eq!(&s[1].extradata[..8], b"OpusHead");
    let a = packets(&ev, s[1].index);
    // 20 ms packets; libopus adds one for its pre-skip (ffprobe agrees: 101).
    assert!(a.iter().all(|p| p.duration == Some(1800)));
    assert!(
        a.windows(2)
            .all(|w| w[1].pts.unwrap() - w[0].pts.unwrap() == 1800)
    );
    if have("ffprobe") {
        let theirs: Vec<(i64, i64)> = ffprobe(&bytes, "opus", "a:0", "pts,duration")
            .iter()
            .map(|f| (f[0].parse().unwrap(), f[1].parse().unwrap()))
            .collect();
        let ours: Vec<(i64, i64)> = a
            .iter()
            .map(|p| (p.pts.unwrap(), p.duration.unwrap()))
            .collect();
        assert_eq!(ours, theirs, "Opus access units as ffprobe reads them");
    }
}

/// ffmpeg sends to a UDP socket (`-f rtp_mpegts` or `-f mpegts udp://`); the datagrams go
/// through TsInput.
fn over_udp(rtp: bool) -> (Vec<TsEvent>, TsInput) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = sock.local_addr().unwrap().port();
    sock.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let (fmt, url) = if rtp {
        ("rtp_mpegts", format!("rtp://127.0.0.1:{port}"))
    } else {
        ("mpegts", format!("udp://127.0.0.1:{port}?pkt_size=1316"))
    };
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-re"])
        .args(VIDEO_SRC)
        .args(AUDIO_SRC)
        .args([
            "-c:v", "libx264", "-g", "30", "-c:a", "aac", "-t", "2", "-f", fmt, &url,
        ])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let mut input = TsInput::new(TsInputConfig::default());
    let mut ev = Vec::new();
    let mut buf = vec![0; 65_536];
    let mut exited_at = None;
    loop {
        let now = t0.elapsed();
        match sock.recv(&mut buf) {
            Ok(n) => ev.extend(input.on_datagram(now, &buf[..n])),
            Err(_) => ev.extend(input.on_tick(now)),
        }
        if exited_at.is_none() && child.try_wait().unwrap().is_some() {
            exited_at = Some(now);
        }
        // Drain for a moment after ffmpeg exits, then stop.
        if exited_at.is_some_and(|t| now > t + Duration::from_millis(300)) {
            break;
        }
        assert!(now < Duration::from_secs(30));
    }
    assert!(child.wait().unwrap().success());
    ev.extend(input.finish(t0.elapsed()));
    (ev, input)
}

#[test]
fn rtp_and_raw_udp() {
    if !ready(false, &["libx264", "aac"]) {
        return;
    }
    for rtp in [true, false] {
        let (ev, input) = over_udp(rtp);
        assert_eq!(
            input.transport(),
            if rtp {
                Transport::Rtp
            } else {
                Transport::RawUdp
            }
        );
        let s = streams(&ev);
        let codecs: Vec<CodecId> = s.iter().map(|s| s.codec).collect();
        assert_eq!(codecs, [CodecId::H264, CodecId::Aac]);
        let v = packets(&ev, s[0].index);
        assert_eq!(v.len(), 60);
        assert!(v.windows(2).all(|w| w[0].dts < w[1].dts));
        assert!(v[0].keyframe && v[30].keyframe);
        assert!(matches!(ev.last().unwrap().event, InputEvent::End(_)));
        let st = input.stats();
        assert_eq!(st.demux.cc_errors, 0);
        eprintln!(
            "{}: {} datagrams, rtp {:?}",
            if rtp { "rtp_mpegts" } else { "udp" },
            st.datagrams,
            st.rtp
        );
    }
}
