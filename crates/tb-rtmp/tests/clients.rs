//! The session over a real TCP socket with real publishers: `ffmpeg` (libavformat's RTMP
//! client), and GStreamer's `rtmp2sink` (its own client) and `rtmpsink` (librtmp, the library
//! OBS uses). Each test is skipped (with a note on stderr) when its program is not on PATH, as
//! in the Nix build sandbox. GStreamer is not in the dev shell; to run those tests, put
//! `gst-launch-1.0` on PATH and its plugins (base, good, bad, ugly) in
//! `GST_PLUGIN_SYSTEM_PATH_1_0`.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tb_media::{CodecId, EndReason, InputEvent, StreamIndex};
use tb_rtmp::{AllowList, CloseReason, ServerConfig, ServerSession};

/// ffmpeg is on PATH with libx264 and the native AAC encoder (every ffmpeg test uses them).
fn have_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("ffmpeg not found: test skipped");
    }
    ok && have_encoder("libx264") && have_encoder("aac")
}

fn have_encoder(name: &str) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .unwrap();
    let ok = String::from_utf8_lossy(&out.stdout).contains(&format!(" {name} "));
    if !ok {
        eprintln!("ffmpeg has no {name}: test skipped");
    }
    ok
}

struct Run {
    events: Vec<InputEvent>,
    input: Vec<u8>,
    close: Option<CloseReason>,
    ffmpeg_ok: bool,
    stderr: String,
}

/// Bind, start ffmpeg with `args` + the URL, drive one session over the socket.
fn publish(args: &[&str], key: &str) -> Run {
    drive(key, |url| {
        let mut c = Command::new("ffmpeg");
        c.args(["-hide_banner", "-loglevel", "warning", "-nostdin"])
            .args(args)
            .args(["-f", "flv", url]);
        c
    })
}

/// Bind, start the client `make(url)` returns, drive one session over the socket.
fn drive(key: &str, make: impl FnOnce(&str) -> Command) -> Run {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("rtmp://127.0.0.1:{port}/live/{key}");
    let mut child = make(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    listener.set_nonblocking(true).unwrap();
    let t0 = Instant::now();
    let mut sock = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if child.try_wait().unwrap().is_some() || t0.elapsed() > Duration::from_secs(20) {
                    let _ = child.kill();
                    let out = child.wait_with_output().unwrap();
                    panic!(
                        "the client never connected: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    };
    sock.set_nonblocking(false).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    sock.set_nodelay(true).unwrap();
    let policy = AllowList::new()
        .allow("live", "key")
        .allow("live", "hevc")
        .allow("live", "ex");
    let mut s = ServerSession::new(ServerConfig::default(), policy, t0.elapsed());
    let mut events = Vec::new();
    let mut input = Vec::new();
    let mut buf = vec![0; 64 << 10];
    loop {
        let now = t0.elapsed();
        assert!(now < Duration::from_secs(60), "session did not end");
        match sock.read(&mut buf) {
            Ok(0) => {
                events.extend(s.on_eof(now));
            }
            Ok(n) => {
                input.extend_from_slice(&buf[..n]);
                events.extend(s.on_bytes(now, &buf[..n]));
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                events.extend(s.on_tick(now));
            }
            Err(e) => events.extend(s.on_transport_error(now, &e.to_string())),
        }
        while !s.pending_output().is_empty() {
            match sock.write(s.pending_output()) {
                Ok(n) => s.consume_output(n),
                Err(_) => break,
            }
        }
        if s.wants_close() {
            break;
        }
    }
    drop(sock);
    let out = child.wait_with_output().unwrap();
    Run {
        events,
        input,
        close: s.close_reason().cloned(),
        ffmpeg_ok: out.status.success(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Checks common to every accepted run; returns the video and audio packets.
fn check(
    run: &Run,
    video: CodecId,
    audio: Option<CodecId>,
) -> (Vec<tb_media::Packet>, Vec<tb_media::Packet>) {
    assert!(run.ffmpeg_ok, "ffmpeg failed: {}", run.stderr);
    assert_eq!(run.close, Some(CloseReason::Finished), "{}", run.stderr);
    let InputEvent::Streams(streams) = &run.events[0] else {
        panic!("first event {:?}", run.events[0])
    };
    let n_streams = run
        .events
        .iter()
        .filter(|e| matches!(e, InputEvent::Streams(_)))
        .count();
    assert_eq!(n_streams, 1, "one announcement");
    assert_eq!(streams[0].codec, video);
    assert!(!streams[0].extradata.is_empty());
    assert_eq!(streams[0].video.width, Some(640));
    assert_eq!(streams[0].video.height, Some(360));
    assert_eq!(streams.get(1).map(|s| s.codec), audio);
    assert_eq!(run.events.last(), Some(&InputEvent::End(EndReason::Closed)));
    let mut per = common::check_shape(&run.events);
    let v = per.remove(&StreamIndex(0)).unwrap_or_default();
    let a = per.remove(&StreamIndex(1)).unwrap_or_default();
    // Monotonic DTS; pts >= dts.
    assert!(
        v.windows(2).all(|w| w[0].dts < w[1].dts),
        "video dts monotonic"
    );
    assert!(
        a.windows(2).all(|w| w[0].dts < w[1].dts),
        "audio dts monotonic"
    );
    assert!(v.iter().all(|p| p.pts >= p.dts));
    (v, a)
}

fn keyframes(v: &[tb_media::Packet]) -> Vec<usize> {
    (0..v.len()).filter(|&i| v[i].keyframe).collect()
}

#[test]
fn ffmpeg_h264_aac() {
    if !have_ffmpeg() {
        return;
    }
    let run = publish(
        &[
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine",
            "-c:v",
            "libx264",
            "-g",
            "30",
            "-c:a",
            "aac",
            "-t",
            "5",
        ],
        "key",
    );
    let (v, a) = check(&run, CodecId::H264, Some(CodecId::Aac));
    assert_eq!(v.len(), 150, "5 s at 30 fps");
    let k = keyframes(&v);
    assert_eq!(
        k,
        (0..5).map(|i| i * 30).collect::<Vec<_>>(),
        "a key frame every 30"
    );
    let base = v[0].pts.unwrap();
    for (n, &i) in k.iter().enumerate() {
        assert_eq!(v[i].pts, Some(base + n as i64 * 1000));
    }
    // B-frames: some pts > dts.
    assert!(v.iter().any(|p| p.pts > p.dts));
    // AAC at 44.1 kHz, 1024 samples a frame: about 215 frames in 5 s.
    assert!((210..=220).contains(&a.len()), "{} audio packets", a.len());
    let InputEvent::Streams(s) = &run.events[0] else {
        unreachable!()
    };
    assert_eq!(s[1].audio.sample_rate, Some(44100));
    eprintln!(
        "h264+aac: {} video, {} audio packets, {} bytes received",
        v.len(),
        a.len(),
        run.input.len()
    );

    // The recorded input, replayed at random split points, gives the same events.
    let whole = replay(&run.input, &[]);
    assert_eq!(strip_end(&whole), strip_end(&run.events));
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..20 {
        let mut cuts = Vec::new();
        for _ in 0..50 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            cuts.push((x % run.input.len() as u64) as usize);
        }
        cuts.sort_unstable();
        assert_eq!(replay(&run.input, &cuts), whole);
    }
}

fn replay(input: &[u8], cuts: &[usize]) -> Vec<InputEvent> {
    let mut s = ServerSession::new(ServerConfig::default(), tb_rtmp::AllowAll, Duration::ZERO);
    let mut ev = Vec::new();
    let mut at = 0;
    for &c in cuts.iter().chain(std::iter::once(&input.len())) {
        ev.extend(s.on_bytes(Duration::ZERO, &input[at..c]));
        at = c;
    }
    ev.extend(s.on_eof(Duration::ZERO));
    ev
}

/// The live run may end by deleteStream; the replay too. Both end with End(Closed).
fn strip_end(ev: &[InputEvent]) -> &[InputEvent] {
    &ev[..ev.len() - 1]
}

#[test]
fn ffmpeg_hevc_enhanced_rtmp() {
    if !have_ffmpeg() || !have_encoder("libx265") {
        return;
    }
    let run = publish(
        &[
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine",
            "-c:v",
            "libx265",
            "-g",
            "30",
            "-x265-params",
            "log-level=error:scenecut=0",
            "-c:a",
            "aac",
            "-t",
            "3",
        ],
        "hevc",
    );
    // ffmpeg 6.1 and later write HEVC as Enhanced RTMP (FourCC hvc1). scenecut=0: x265 would
    // otherwise add key frames at scene cuts of testsrc2 (ffprobe shows 0, 26, 58).
    let (v, a) = check(&run, CodecId::Hevc, Some(CodecId::Aac));
    assert_eq!(v.len(), 90);
    // Open GOPs: the CRA at display frame 30 is decoded before its leading pictures, so key
    // frames are every 30 in presentation order (pts 0, 1000, 2000 ms), not in decode order.
    let key_pts: Vec<i64> = v
        .iter()
        .filter(|p| p.keyframe)
        .map(|p| p.pts.unwrap())
        .collect();
    let base = v[0].pts.unwrap();
    assert_eq!(key_pts, [base, base + 1000, base + 2000]);
    assert!(!a.is_empty());
    // Enhanced RTMP: the FourCC is on the wire.
    assert!(run.input.windows(4).any(|w| w == b"hvc1"));
    eprintln!("hevc: {} video, {} audio packets", v.len(), a.len());
}

/// Long sessions: timestamps past 0xFFFFFF ms (4.66 h, the extended-timestamp threshold; at
/// 640x360 key frames span several chunks, so format-3 continuations carry the extended
/// field) and past 2^31 ms (24.8 days), where ffmpeg's 31-bit FLV timestamps wrap to 0.
#[test]
fn ffmpeg_extended_and_wrapping_timestamps() {
    if !have_ffmpeg() {
        return;
    }
    for offset_ms in [16_776_500i64, 2_147_483_000] {
        let offset = format!("{}", offset_ms as f64 / 1000.0);
        let run = publish(
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30",
                "-c:v",
                "libx264",
                "-t",
                "1",
                "-output_ts_offset",
                &offset,
            ],
            "key",
        );
        let (v, _) = check_small(&run);
        let dts: Vec<i64> = v.iter().map(|p| p.dts.unwrap()).collect();
        eprintln!(
            "offset {offset_ms}: dts {:?} .. {:?}",
            &dts[..2],
            &dts[dts.len() - 2..]
        );
        assert_eq!(v.len(), 30);
        assert!((dts[0] - offset_ms).abs() < 100, "first dts {}", dts[0]);
        assert!(
            dts.windows(2).all(|w| (33..=34).contains(&(w[1] - w[0]))),
            "{dts:?}"
        );
        // Some video message is longer than ffmpeg's 4096-byte chunks.
        assert!(v.iter().any(|p| p.data.len() > 4096));
    }
}

/// As `check` for a tiny video-only run (no size from metadata asserted).
fn check_small(run: &Run) -> (Vec<tb_media::Packet>, Vec<tb_media::Packet>) {
    assert!(run.ffmpeg_ok, "ffmpeg failed: {}", run.stderr);
    assert_eq!(run.close, Some(CloseReason::Finished), "{}", run.stderr);
    let mut per = common::check_shape(&run.events);
    (
        per.remove(&StreamIndex(0)).unwrap_or_default(),
        per.remove(&StreamIndex(1)).unwrap_or_default(),
    )
}

#[test]
fn ffmpeg_av1_and_vp9_enhanced_rtmp() {
    if !have_ffmpeg() {
        return;
    }
    for (enc, codec, fourcc) in [
        (
            &["-c:v", "libsvtav1", "-preset", "12"][..],
            CodecId::Av1,
            b"av01",
        ),
        (
            &[
                "-c:v",
                "libvpx-vp9",
                "-deadline",
                "realtime",
                "-cpu-used",
                "8",
            ][..],
            CodecId::Vp9,
            b"vp09",
        ),
    ] {
        if !have_encoder(enc[1]) {
            continue;
        }
        let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30"];
        args.extend_from_slice(enc);
        args.extend_from_slice(&["-g", "30", "-t", "1"]);
        let run = publish(&args, "ex");
        let (v, _) = check(&run, codec, None);
        assert_eq!(v.len(), 30, "{codec:?}");
        assert!(v[0].keyframe);
        assert!(run.input.windows(4).any(|w| w == fourcc));
        eprintln!("{codec:?}: {} video packets", v.len());
    }
}

#[test]
fn ffmpeg_video_only_announces_without_waiting() {
    if !have_ffmpeg() {
        return;
    }
    // The producer's shape: H.264 only.
    let run = publish(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-c:v",
            "libx264",
            "-g",
            "30",
            "-t",
            "2",
        ],
        "key",
    );
    let (v, a) = check(&run, CodecId::H264, None);
    assert_eq!(v.len(), 60);
    assert!(a.is_empty());
}

#[test]
fn ffmpeg_rejected_key() {
    if !have_ffmpeg() {
        return;
    }
    let run = publish(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=30",
            "-c:v",
            "libx264",
            "-t",
            "1",
        ],
        "wrong",
    );
    assert!(run.events.is_empty());
    assert!(matches!(run.close, Some(CloseReason::Rejected(_))));
    assert!(!run.ffmpeg_ok, "ffmpeg should fail on a refused publish");
    eprintln!("ffmpeg said: {}", run.stderr.trim());
}

/// Writes `tests/fixtures/ffmpeg-h264-aac.rtmp`: the client-to-server bytes of a short, tiny
/// publish, replayed in `tests/replay.rs` where no ffmpeg is available. Run by hand:
/// `cargo nextest run -p tb-rtmp --test clients --run-ignored only record_fixture`.
#[test]
#[ignore = "writes the committed fixture"]
fn record_fixture() {
    if !have_ffmpeg() {
        return;
    }
    let run = publish(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x36:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=22050",
            "-c:v",
            "libx264",
            "-g",
            "10",
            "-bf",
            "2",
            "-b:v",
            "20k",
            "-c:a",
            "aac",
            "-b:a",
            "16k",
            "-ac",
            "1",
            "-t",
            "1",
            "-metadata",
            "encoder=fixture",
            "-fflags",
            "+bitexact",
        ],
        "key",
    );
    assert!(run.ffmpeg_ok, "{}", run.stderr);
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ffmpeg-h264-aac.rtmp"
    );
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, &run.input).unwrap();
    eprintln!("wrote {} bytes to {path}", run.input.len());
}

fn have_gst(elements: &[&str]) -> bool {
    for e in elements {
        let ok = Command::new("gst-inspect-1.0")
            .arg(e)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("GStreamer element {e} not found: test skipped");
            return false;
        }
    }
    true
}

/// H.264 + AAC from GStreamer through `sink` (rtmp2sink or rtmpsink). flvmux's output is also
/// written to a file, for ffprobe to read the same tags.
fn gst_publish(sink: &str) -> (Run, std::path::PathBuf) {
    let file = std::env::temp_dir().join(format!("tb-rtmp-{sink}-{}.flv", std::process::id()));
    let f = file.display().to_string();
    let run = drive("key", |url| {
        let mut c = Command::new("gst-launch-1.0");
        let pipeline = format!(
            "videotestsrc num-buffers=90 is-live=true ! \
             video/x-raw,format=I420,width=640,height=360,framerate=30/1 ! \
             x264enc key-int-max=30 bframes=2 ! h264parse ! \
             flvmux name=m streamable=true ! tee name=t ! queue ! {sink} location={url} \
             t. ! queue ! filesink location={f} \
             audiotestsrc num-buffers=130 is-live=true ! \
             audio/x-raw,format=S16LE,rate=44100,channels=2 ! voaacenc ! aacparse ! m."
        );
        c.arg("-q").args(pipeline.split_whitespace());
        c
    });
    (run, file)
}

/// (dts, pts, key) of each video packet as ffprobe reads an FLV file.
fn ffprobe_video(file: &std::path::Path) -> Option<Vec<(i64, i64, bool)>> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v", "-show_entries"])
        .args(["packet=dts,pts,flags", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(|l| {
                let f: Vec<&str> = l.split(',').collect();
                (
                    f[1].parse().unwrap(),
                    f[0].parse().unwrap(),
                    f[2].starts_with('K'),
                )
            })
            .collect(),
    )
}

fn check_gst((run, file): &(Run, std::path::PathBuf)) {
    assert!(run.ffmpeg_ok, "gst-launch failed: {}", run.stderr);
    let InputEvent::Streams(s) = &run.events[0] else {
        panic!("first event {:?}", run.events[0])
    };
    let codecs: Vec<CodecId> = s.iter().map(|s| s.codec).collect();
    assert_eq!(codecs, [CodecId::H264, CodecId::Aac]);
    assert!(matches!(run.events.last(), Some(InputEvent::End(_))));
    let per = common::check_shape(&run.events);
    let v = &per[&StreamIndex(0)];
    assert_eq!(v.len(), 90);
    assert!(v[0].keyframe);
    assert!(!per[&StreamIndex(1)].is_empty());
    // GStreamer's flvmux writes B-frame streams with a repeated first DTS (0, 0, 23, 69, ...)
    // and some pts < dts; what matters is that we read the tags exactly as ffprobe does.
    let ours: Vec<(i64, i64, bool)> = v
        .iter()
        .map(|p| (p.dts.unwrap(), p.pts.unwrap(), p.keyframe))
        .collect();
    if let Some(theirs) = ffprobe_video(file) {
        assert_eq!(
            ours, theirs,
            "video timestamps and key flags as ffprobe reads them"
        );
    }
    let _ = std::fs::remove_file(file);
    eprintln!(
        "{} video, {} audio packets; close {:?}",
        v.len(),
        per[&StreamIndex(1)].len(),
        run.close
    );
}

#[test]
fn gstreamer_rtmp2sink() {
    if !have_gst(&["rtmp2sink", "x264enc", "voaacenc", "flvmux"]) {
        return;
    }
    check_gst(&gst_publish("rtmp2sink"));
}

#[test]
fn gstreamer_rtmpsink_librtmp() {
    if !have_gst(&["rtmpsink", "x264enc", "voaacenc", "flvmux"]) {
        return;
    }
    check_gst(&gst_publish("rtmpsink"));
}
