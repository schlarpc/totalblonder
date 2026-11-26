//! Software decoding of clips the tests encode themselves (MPEG-2 video through libavcodec,
//! muxed by libavformat into a temporary directory): no GPU, no network, no media in the tree.
#![allow(
    unsafe_code,
    reason = "two frame properties the safe layer cannot write"
)]
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests"
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::ffi;
use tb_decode::{ColorSpace, DecDevice, DecOptions, DecodeError, Frame, Input, Surface, now};

const W: u32 = 176;
const H: u32 = 144;
const U: u8 = 100;
const V: u8 = 160;
/// Caption bytes of frame `i`: one `cc_data` triplet.
fn cc_of(i: usize) -> [u8; 3] {
    [0xfc, 0x80 | (i as u8 & 0x3f), 0x80]
}
/// Luma of the test pattern: a horizontal ramp that gets brighter with the frame number.
fn luma(x: usize, i: usize) -> u8 {
    (40 + x / 2 + i) as u8
}
fn mean_luma(i: usize) -> f64 {
    (0..W as usize).map(|x| f64::from(luma(x, i))).sum::<f64>() / f64::from(W)
}

struct Clip {
    frames: usize,
    rate: (i32, i32),
    gop: u32,
    interlaced: bool,
    cc: bool,
}

impl Default for Clip {
    fn default() -> Self {
        Self {
            frames: 40,
            rate: (25, 1),
            gop: 10,
            interlaced: false,
            cc: false,
        }
    }
}

/// Encodes the test pattern as MPEG-2 video (no B pictures) into `path`; the container follows
/// from the file name.
fn make_clip(path: &Path, c: &Clip) {
    use ffmpeg::format::Pixel;
    let mut octx = ffmpeg::format::output(path).unwrap();
    let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::MPEG2VIDEO).unwrap();
    let global = octx
        .format()
        .flags()
        .contains(ffmpeg::format::Flags::GLOBAL_HEADER);
    let mut enc = ffmpeg::codec::Context::new_with_codec(codec)
        .encoder()
        .video()
        .unwrap();
    enc.set_width(W);
    enc.set_height(H);
    enc.set_format(Pixel::YUV420P);
    enc.set_time_base((c.rate.1, c.rate.0));
    enc.set_frame_rate(Some(c.rate));
    enc.set_bit_rate(3_000_000);
    enc.set_gop(c.gop);
    enc.set_max_b_frames(0);
    enc.set_aspect_ratio((12, 11));
    enc.set_colorspace(ffmpeg::color::Space::SMPTE170M);
    let mut flags = ffmpeg::codec::Flags::empty();
    if global {
        flags |= ffmpeg::codec::Flags::GLOBAL_HEADER;
    }
    if c.interlaced {
        flags |= ffmpeg::codec::Flags::INTERLACED_DCT | ffmpeg::codec::Flags::INTERLACED_ME;
    }
    enc.set_flags(flags);
    let mut enc = enc.open_as(codec).unwrap();
    {
        let mut ost = octx.add_stream(codec).unwrap();
        ost.set_time_base((c.rate.1, c.rate.0));
        ost.set_avg_frame_rate(c.rate);
        ost.copy_parameters_from_context(&enc);
    }
    octx.write_header().unwrap();
    let tb_out = octx.stream(0).unwrap().time_base();
    let mut pkt = ffmpeg::Packet::empty();
    let mut drain = |enc: &mut ffmpeg::encoder::Video,
                     octx: &mut ffmpeg::format::context::Output| {
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts((c.rate.1, c.rate.0), tb_out);
            pkt.write_interleaved(octx).unwrap();
        }
    };
    for i in 0..c.frames {
        let mut f = ffmpeg::frame::Video::new(Pixel::YUV420P, W, H);
        let stride = f.stride(0);
        for row in f.data_mut(0).chunks_mut(stride).take(H as usize) {
            for (x, p) in row.iter_mut().enumerate().take(W as usize) {
                *p = luma(x, i);
            }
        }
        f.data_mut(1).fill(U);
        f.data_mut(2).fill(V);
        f.set_pts(Some(i as i64));
        if c.interlaced {
            // SAFETY: the frame is allocated and owned here; `flags` is a plain field.
            unsafe {
                (*f.as_mut_ptr()).flags |=
                    ffi::AV_FRAME_FLAG_INTERLACED | ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST;
            }
        }
        if c.cc {
            let mut sd = f
                .new_side_data(ffmpeg::frame::side_data::Type::A53CC, 3)
                .unwrap();
            // SAFETY: the side data was allocated with three bytes just above.
            unsafe { std::slice::from_raw_parts_mut((*sd.as_mut_ptr()).data, 3) }
                .copy_from_slice(&cc_of(i));
        }
        enc.send_frame(&f).unwrap();
        drain(&mut enc, &mut octx);
    }
    enc.send_eof().unwrap();
    drain(&mut enc, &mut octx);
    octx.write_trailer().unwrap();
}

fn clip(dir: &tempfile::TempDir, name: &str, c: &Clip) -> PathBuf {
    tb_decode::log_errors_only();
    let path = dir.path().join(name);
    make_clip(&path, c);
    path
}

/// Offline options: software, the queue holds the decoder back, no loop, frame times from the
/// frame count (frame k of the first lap at k / rate).
fn offline() -> DecOptions {
    DecOptions {
        sw: true,
        block: true,
        looping: false,
        cfr: true,
        ..DecOptions::default()
    }
}

/// The time of the first frame when times follow the time stamps (`cfr` off): presentation
/// times count from the first packet's decode time stamp, and libav gives MPEG-2 video a
/// reordering delay of one frame. The C++ decoder has the same offset.
const LAG: f64 = 1.0 / 25.0;

/// Takes frame after frame (each call asks for "everything up to just after the next frame
/// time") until the input has ended and its queue is empty.
fn take_all(inp: &Input, fps: f64, most: usize) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut idle = 0;
    for k in 0..10 * most {
        let (f, skipped) = inp.take((k as f64 + 0.5) / fps);
        assert_eq!(skipped, 0);
        match f {
            Some(f) => {
                out.push(f);
                idle = 0;
            }
            None => idle += 1,
        }
        // a few frame times without a frame after the end: nothing is left in the queue
        if out.len() == most || (idle > 4 && inp.ended()) {
            break;
        }
    }
    out
}

#[test]
fn default_options_are_those_of_dec_h() {
    let o = DecOptions::default();
    assert_eq!(o.decoder, None);
    assert!(o.opts.is_empty());
    assert!(!o.sw && !o.unsafe_output && !o.paced && !o.cfr && !o.block);
    assert!(o.looping);
    assert_eq!((o.sw_threads, o.qmax), (1, 4));
    assert_eq!((o.lead, o.degrade, o.resync), (0.0, 0.0, 0.0));
    assert_eq!(o.host_size, None);
}

#[test]
fn types_cross_threads() {
    fn send<T: Send>() {}
    fn sync<T: Sync>() {}
    send::<Frame>();
    send::<Input>();
    sync::<Input>();
    send::<DecDevice>();
    sync::<DecDevice>();
}

#[test]
fn software_decode_returns_every_frame_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    let o = DecOptions {
        cfr: false,
        ..offline()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    assert_eq!(inp.codec(), "mpeg2video");
    assert_eq!(inp.size(), (W, H));
    assert_eq!(inp.fps(), 25.0);
    assert!(!inp.ended());
    let t0 = now();
    inp.start(t0);
    let frames = take_all(&inp, 25.0, 1000);
    assert_eq!(frames.len(), 40);
    for (k, f) in frames.iter().enumerate() {
        assert_eq!(f.idx, k as i64);
        assert!(
            (f.t - LAG - k as f64 / 25.0).abs() < 1e-9,
            "frame {k}: t {}",
            f.t
        );
        assert_eq!((f.width, f.height), (W, H));
        assert!(f.done >= t0 && f.done <= now());
        assert!(!f.interlaced);
        assert_eq!(f.colorspace, ColorSpace::Smpte170m);
        assert!(!f.full_range);
        assert_eq!(f.sar, Some((12, 11)));
        assert!(f.cc.is_empty());
        assert!(matches!(f.surface, Surface::None));
        assert!(!f.holds_device_frame());
    }
    let s = inp.stats();
    assert_eq!(
        (s.decoded, s.dropped, s.packets, s.errors, s.loops),
        (40, 0, 40, 0, 0)
    );
    assert!(s.bytes > 1000);
    assert!(s.ended && !s.failed && !s.on_device && !s.interlaced_seen);
    assert_eq!(s.pixfmt, "yuv420p");
    // nothing more, and no waiting for it
    assert!(inp.take(1e18).0.is_none());
    inp.close();
}

#[test]
fn opened_without_a_device_decodes_in_software() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(
        &dir,
        "a.mkv",
        &Clip {
            frames: 5,
            ..Clip::default()
        },
    );
    // `sw` is not set: no device means software all the same
    let o = DecOptions {
        block: true,
        looping: false,
        ..DecOptions::default()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    assert_eq!(take_all(&inp, 25.0, 100).len(), 5);
    let s = inp.stats();
    // a software open is not a hardware open that fell back: no note
    assert_eq!((s.on_device, s.pixfmt.as_str()), (false, "yuv420p"));
}

#[test]
fn take_returns_the_newest_frame_up_to_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.start(now());
    // before the first frame's time: nothing, although frames are queued
    let (f, skipped) = inp.take(-0.001);
    assert!(f.is_none() && skipped == 0);
    // every third frame time: the two frames before it are released
    for k in 0..10 {
        let (f, skipped) = inp.take((3 * k + 2) as f64 / 25.0 + 0.001);
        assert_eq!((f.unwrap().idx, skipped), (3 * k + 2, 2), "step {k}");
    }
    // a limit beyond the end: the last frame, the others released
    let (f, skipped) = inp.take(1e18);
    assert_eq!((f.unwrap().idx, skipped), (39, 9));
    assert_eq!(inp.stats().dropped, 0);
}

#[test]
fn looping_gives_continuous_times() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.mkv", "a.ts"] {
        let path = clip(
            &dir,
            name,
            &Clip {
                frames: 12,
                gop: 6,
                ..Clip::default()
            },
        );
        for cfr in [true, false] {
            let o = DecOptions {
                looping: true,
                cfr,
                ..offline()
            };
            let mut inp = Input::open(&path, &o, None).unwrap();
            inp.start(now());
            let frames = take_all(&inp, 25.0, 50);
            assert_eq!(frames.len(), 50, "{name}");
            for (k, f) in frames.iter().enumerate() {
                assert_eq!(f.idx, k as i64);
                let what = format!("{name} cfr {cfr} frame {k}: t {}", f.t);
                if cfr {
                    // from the frame count, laps follow each other without a gap
                    assert!((f.t - k as f64 / 25.0).abs() < 1e-9, "{what}");
                } else if k > 0 {
                    // From the time stamps (as in the C++ decoder) a lap starts a frame time after
                    // the last frame, plus the distance between the first decode and presentation
                    // time stamps the demuxer reports after the rewind (none or one frame).
                    let step = f.t - frames[k - 1].t;
                    let ok = (step - 0.04).abs() < 1e-6
                        || (k % 12 == 0 && (step - 0.04 - LAG).abs() < 1e-6);
                    assert!(ok, "{what}, step {step}");
                }
            }
            let s = inp.stats();
            assert!(s.loops >= 4 && !s.ended && !s.failed, "{name}: {s:?}");
            assert_eq!(s.errors, 0);
            inp.close();
        }
    }
}

#[test]
fn cfr_times_are_frame_indices_over_the_rate() {
    let dir = tempfile::tempdir().unwrap();
    let c = Clip {
        frames: 20,
        rate: (30000, 1001),
        ..Clip::default()
    };
    let path = clip(&dir, "ntsc.mkv", &c);
    let fps = 30000.0 / 1001.0;
    let mut worst = [0.0f64; 2];
    for (i, cfr) in [false, true].into_iter().enumerate() {
        let o = DecOptions {
            cfr,
            looping: true,
            ..offline()
        };
        let mut inp = Input::open(&path, &o, None).unwrap();
        assert!((inp.fps() - fps).abs() < 1e-6, "{}", inp.fps());
        inp.start(now());
        let frames = take_all(&inp, fps, 50);
        assert_eq!(frames.len(), 50);
        // the first lap; without `cfr`, relative to its first frame
        for (k, f) in frames.iter().enumerate().take(20) {
            worst[i] = worst[i].max((f.t - frames[0].t - k as f64 / inp.fps()).abs());
        }
        if cfr {
            for (k, f) in frames.iter().enumerate() {
                worst[i] = worst[i].max((f.t - k as f64 / inp.fps()).abs());
            }
        }
    }
    // Matroska rounds time stamps to milliseconds; with `cfr` the times are exact
    assert!(worst[0] > 1e-4 && worst[0] < 1e-3, "{worst:?}");
    assert!(worst[1] < 1e-9, "{worst:?}");
}

#[test]
fn host_path_converts_to_planar_420_of_the_asked_size() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    for (w, h) in [(W, H), (88, 72), (352, 240)] {
        let o = DecOptions {
            host_size: Some((w, h)),
            ..offline()
        };
        let mut inp = Input::open(&path, &o, None).unwrap();
        inp.start(now());
        let frames = take_all(&inp, 25.0, 1000);
        assert_eq!(frames.len(), 40);
        let (w, h) = (w as usize, h as usize);
        for (i, f) in frames.iter().enumerate() {
            // the frame says what the source was, the buffer is the converted picture
            assert_eq!((f.width, f.height), (W, H));
            let Surface::Host(buf) = &f.surface else {
                panic!("frame {i}: {:?}", f.surface);
            };
            assert_eq!(buf.len(), w * h * 3 / 2);
            let mean = |s: &[u8]| s.iter().map(|&b| f64::from(b)).sum::<f64>() / s.len() as f64;
            let (y, c) = buf.split_at(w * h);
            let (u, v) = c.split_at(w * h / 4);
            let what = format!("{w}x{h} frame {i}");
            assert!(
                (mean(y) - mean_luma(i)).abs() < 2.0,
                "{what}: Y {}",
                mean(y)
            );
            assert!(
                (mean(u) - f64::from(U)).abs() < 2.0,
                "{what}: U {}",
                mean(u)
            );
            assert!(
                (mean(v) - f64::from(V)).abs() < 2.0,
                "{what}: V {}",
                mean(v)
            );
            // the ramp runs left to right in every row: rows have no padding
            for row in y.chunks(w) {
                let (l, r) = row.split_at(w / 2);
                assert!(
                    (mean(r) - mean(l) - f64::from(W) / 4.0).abs() < 4.0,
                    "{what}"
                );
            }
        }
        assert_eq!(inp.stats().pixfmt, "yuv420p");
    }
}

#[test]
fn host_buffers_are_reused() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    let o = DecOptions {
        host_size: Some((88, 72)),
        looping: true,
        ..offline()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    let mut seen = std::collections::HashSet::new();
    for k in 0..200 {
        let (f, _) = inp.take((k as f64 + 0.5) / 25.0);
        if let Some(Surface::Host(buf)) = f.as_ref().map(|f| &f.surface) {
            seen.insert(buf.as_ptr() as usize);
        }
    }
    // the pool holds the queue and a few more
    assert!(seen.len() <= 4 + 6, "{} different buffers", seen.len());
}

#[test]
fn interlaced_flags_and_captions_come_through() {
    let dir = tempfile::tempdir().unwrap();
    let c = Clip {
        frames: 12,
        interlaced: true,
        cc: true,
        ..Clip::default()
    };
    let path = clip(&dir, "i.ts", &c);
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.start(now());
    let frames = take_all(&inp, 25.0, 1000);
    assert_eq!(frames.len(), 12);
    for (i, f) in frames.iter().enumerate() {
        assert!(f.interlaced && f.top_field_first, "frame {i}");
        assert_eq!(f.cc, cc_of(i), "frame {i}");
    }
    assert!(inp.stats().interlaced_seen);
}

#[test]
fn a_stall_shifts_the_times() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(
        &dir,
        "a.mkv",
        &Clip {
            frames: 10,
            ..Clip::default()
        },
    );
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.set_stall(true);
    inp.start(now());
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(inp.stats().decoded, 0);
    inp.set_stall(false);
    let mut frames = Vec::new();
    while let (Some(f), _) = inp.take(1e18) {
        // one at a time: with a queue of 4 the limit takes the newest queued
        frames.push(f);
    }
    assert!(inp.ended());
    let shift = frames[0].t - frames[0].idx as f64 / 25.0;
    assert!((0.149..5.0).contains(&shift), "shift {shift}");
    for f in &frames {
        assert!((f.t - f.idx as f64 / 25.0 - shift).abs() < 1e-9);
    }
    assert_eq!(frames.last().unwrap().idx, 9);
}

#[test]
fn a_full_queue_drops_the_oldest_and_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    let o = DecOptions {
        sw: true,
        looping: false,
        qmax: 4,
        ..DecOptions::default()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    let until = now() + 20.0;
    while !inp.ended() {
        assert!(now() < until, "the input did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    let s = inp.stats();
    assert_eq!((s.decoded, s.dropped), (40, 36));
    let (f, skipped) = inp.take(1e18);
    assert_eq!((f.unwrap().idx, skipped), (39, 3));
    assert!(inp.take(1e18).0.is_none());
}

/// Appends what one call gave to the caption record of a test: the skipped frames' captions,
/// then the returned frame's. Checks the count of released frames against the captions (every
/// frame of these clips has captions).
fn note_captions(
    all: &mut Vec<(i64, Vec<u8>)>,
    skipped_cc: &mut Vec<(i64, Vec<u8>)>,
    got: (Option<Frame>, u32),
    dropped_before: usize,
) -> Option<i64> {
    let (f, skipped) = got;
    assert_eq!(skipped_cc.len(), dropped_before + skipped as usize);
    all.append(skipped_cc);
    let f = f?;
    all.push((f.idx, f.cc.clone()));
    Some(f.idx)
}

#[test]
fn captions_of_skipped_frames_come_with_the_take() {
    let dir = tempfile::tempdir().unwrap();
    let c = Clip {
        frames: 32,
        cc: true,
        ..Clip::default()
    };
    let path = clip(&dir, "c.ts", &c);
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.start(now());
    let mut all = Vec::new();
    let mut skipped_cc = Vec::new();
    // before the first frame: nothing, and nothing appended
    let got = inp.take_with_captions(-0.001, &mut skipped_cc);
    assert_eq!(note_captions(&mut all, &mut skipped_cc, got, 0), None);
    assert!(all.is_empty());
    // every third frame time, then one frame at a time, then the rest at once
    let mut limits: Vec<f64> = (0..5).map(|k| (3 * k + 2) as f64 / 25.0 + 0.001).collect();
    limits.extend([15, 16, 17].map(|k| f64::from(k) / 25.0 + 0.001));
    limits.push(1e18);
    let mut returned = Vec::new();
    for limit in limits {
        let got = inp.take_with_captions(limit, &mut skipped_cc);
        returned.extend(note_captions(&mut all, &mut skipped_cc, got, 0));
    }
    assert_eq!(returned, [2, 5, 8, 11, 14, 15, 16, 17, 31]);
    // every frame's captions exactly once, in decode order
    let want: Vec<(i64, Vec<u8>)> = (0..32).map(|i| (i as i64, cc_of(i).to_vec())).collect();
    assert_eq!(all, want);
    let got = inp.take_with_captions(1e18, &mut skipped_cc);
    assert!(got.0.is_none() && skipped_cc.is_empty());
    assert_eq!(inp.stats().cc_lost, 0);

    // the vector is appended to, and `take` is the same call without the captions
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.start(now());
    let mut skipped_cc = vec![(-1, vec![0u8])];
    let (f, skipped) = inp.take_with_captions(3.0 / 25.0 + 0.001, &mut skipped_cc);
    assert_eq!((f.unwrap().idx, skipped), (3, 3));
    let idx: Vec<i64> = skipped_cc.iter().map(|e| e.0).collect();
    assert_eq!(idx, [-1, 0, 1, 2]);
    let (f, skipped) = inp.take(6.0 / 25.0 + 0.001);
    let f = f.unwrap();
    assert_eq!((f.idx, skipped), (6, 2));
    assert_eq!(f.cc, cc_of(6));
    skipped_cc.clear();
    let (f, skipped) = inp.take_with_captions(8.0 / 25.0 + 0.001, &mut skipped_cc);
    assert_eq!((f.unwrap().idx, skipped), (8, 1));
    assert_eq!(skipped_cc, [(7, cc_of(7).to_vec())]);
}

/// Waits for the end of an input nobody takes frames from.
fn run_out(inp: &Input) {
    let until = now() + 60.0;
    while !inp.ended() {
        assert!(now() < until, "the input did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn captions_of_frames_dropped_from_a_full_queue_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let o = DecOptions {
        sw: true,
        looping: false,
        qmax: 4,
        ..DecOptions::default()
    };
    // 36 frames leave the full queue before anybody takes one
    let c = Clip {
        cc: true,
        ..Clip::default()
    };
    let path = clip(&dir, "c.ts", &c);
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    run_out(&inp);
    let s = inp.stats();
    assert_eq!((s.decoded, s.dropped, s.cc_lost), (40, 36, 0));
    let mut all = Vec::new();
    let mut skipped_cc = Vec::new();
    let got = inp.take_with_captions(1e18, &mut skipped_cc);
    assert_eq!((got.0.as_ref().unwrap().idx, got.1), (39, 3));
    note_captions(&mut all, &mut skipped_cc, got, 36);
    let want: Vec<(i64, Vec<u8>)> = (0..40).map(|i| (i as i64, cc_of(i).to_vec())).collect();
    assert_eq!(all, want);
    // they are handed over once
    let got = inp.take_with_captions(1e18, &mut skipped_cc);
    assert!(got.0.is_none() && skipped_cc.is_empty());

    // more dropped frames than the list keeps: the oldest captions are lost and counted
    let c = Clip {
        frames: 270,
        cc: true,
        ..Clip::default()
    };
    let path = clip(&dir, "long.ts", &c);
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    run_out(&inp);
    let s = inp.stats();
    assert_eq!((s.decoded, s.dropped, s.cc_lost), (270, 266, 10));
    let got = inp.take_with_captions(1e18, &mut skipped_cc);
    assert_eq!((got.0.as_ref().unwrap().idx, got.1), (269, 3));
    let mut all = Vec::new();
    note_captions(&mut all, &mut skipped_cc, got, 256);
    let want: Vec<(i64, Vec<u8>)> = (10..270).map(|i| (i as i64, cc_of(i).to_vec())).collect();
    assert_eq!(all, want);
}

#[test]
fn paced_input_follows_the_clock_and_records_lateness() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(
        &dir,
        "a.mkv",
        &Clip {
            frames: 10,
            ..Clip::default()
        },
    );
    let o = DecOptions {
        sw: true,
        paced: true,
        cfr: true,
        looping: false,
        qmax: 64,
        ..DecOptions::default()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.set_late_from(0.1);
    let t0 = now() + 0.05;
    inp.start(t0);
    let until = now() + 20.0;
    while !inp.ended() {
        assert!(now() < until, "the input did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    // ten frames at 25 per second, the first due at t0: the last packet is due 0.36 s later
    let took = now() - t0;
    assert!(took > 0.35, "took {took}");
    let late = inp.take_lateness();
    // frames 3..9 (0.12 s on)
    assert_eq!(late.len(), 7, "{late:?}");
    // a frame leaves the decoder when the next packet is sent, a frame time after its own
    // time (the last one at the end of the file)
    assert!(late.iter().all(|&l| (-0.001..1.0).contains(&l)), "{late:?}");
    assert!(late[..6].iter().all(|&l| l >= 0.039), "{late:?}");
    assert!(inp.take_lateness().is_empty());
    let mut n = 0;
    let mut last = None;
    while let (Some(f), _) = inp.take(n as f64 / 25.0 + 0.001) {
        assert!(f.done >= t0 + f.t - 0.001);
        last = Some(f.idx);
        n += 1;
    }
    assert_eq!((n, last), (10, Some(9)));
}

#[test]
fn a_lead_sends_packets_early() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(
        &dir,
        "a.mkv",
        &Clip {
            frames: 10,
            ..Clip::default()
        },
    );
    let o = DecOptions {
        sw: true,
        paced: true,
        lead: 0.3,
        looping: false,
        qmax: 64,
        ..DecOptions::default()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    let t0 = now() + 0.35;
    inp.start(t0);
    let until = now() + 20.0;
    while !inp.ended() {
        assert!(now() < until, "the input did not end");
        std::thread::sleep(Duration::from_millis(2));
    }
    let late = inp.take_lateness();
    assert_eq!(late.len(), 10);
    // every frame was there about 0.3 s before its time (the last one, which the end of the
    // file brings out, a frame time more)
    assert!(late.iter().all(|&l| (-0.35..-0.1).contains(&l)), "{late:?}");
}

/// Runs `f` in a thread and fails if it does not return in time.
fn within(secs: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .expect("did not return in time");
}

#[test]
fn close_returns_while_the_thread_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    // held back by a full queue (block mode, nobody takes)
    let o = DecOptions {
        looping: true,
        ..offline()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    while inp.stats().decoded < 5 {
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(inp.stats().decoded, 5);
    within(10, move || inp.close());

    // asleep until a packet's time, far in the future
    let o = DecOptions {
        sw: true,
        paced: true,
        ..DecOptions::default()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now() + 3600.0);
    std::thread::sleep(Duration::from_millis(20));
    within(10, move || inp.close());

    // stalled
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.set_stall(true);
    inp.start(now());
    std::thread::sleep(Duration::from_millis(20));
    within(10, move || inp.close());

    // never started; and dropped instead of closed
    let inp = Input::open(&path, &offline(), None).unwrap();
    assert!(inp.take(1e18).0.is_none());
    inp.close();
    let mut inp = Input::open(&path, &offline(), None).unwrap();
    inp.start(now());
    within(10, move || drop(inp));
}

#[test]
fn frames_outlive_their_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(&dir, "a.mkv", &Clip::default());
    let o = DecOptions {
        host_size: Some((88, 72)),
        ..offline()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    let f = inp.take(0.001).0.unwrap();
    inp.close();
    let Surface::Host(buf) = &f.surface else {
        panic!("{:?}", f.surface);
    };
    assert_eq!(buf.len(), 88 * 72 * 3 / 2);
    assert!((f64::from(buf[0]) - 40.0).abs() < 6.0);
}

#[test]
fn bad_inputs_and_options_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(
        &dir,
        "a.mkv",
        &Clip {
            frames: 3,
            ..Clip::default()
        },
    );
    let open = |o: &DecOptions| Input::open(&path, o, None).map(|_| ());

    let missing = Input::open(&dir.path().join("none.mkv"), &offline(), None).map(|_| ());
    assert!(
        matches!(missing, Err(DecodeError::Open { .. })),
        "{missing:?}"
    );
    let junk = dir.path().join("junk.mkv");
    std::fs::write(&junk, vec![0x55u8; 4096]).unwrap();
    let r = Input::open(&junk, &offline(), None).map(|_| ());
    assert!(
        matches!(r, Err(DecodeError::Open { .. } | DecodeError::NoVideo(_))),
        "{r:?}"
    );

    let r = open(&DecOptions {
        decoder: Some("no_such_decoder".into()),
        ..offline()
    });
    assert!(matches!(r, Err(DecodeError::NoDecoder(_))), "{r:?}");
    let r = open(&DecOptions {
        decoder: Some("mpeg2\0video".into()),
        ..offline()
    });
    assert!(matches!(r, Err(DecodeError::BadOption(_))), "{r:?}");
    let r = open(&DecOptions {
        opts: vec![("a\0".into(), "b".into())],
        ..offline()
    });
    assert!(matches!(r, Err(DecodeError::BadOption(_))), "{r:?}");
    let r = open(&DecOptions {
        qmax: 0,
        ..offline()
    });
    assert!(matches!(r, Err(DecodeError::BadOption(_))), "{r:?}");
    for size in [(0, 72), (87, 72), (88, 71), (1 << 16, 72)] {
        let r = open(&DecOptions {
            host_size: Some(size),
            ..offline()
        });
        assert!(
            matches!(r, Err(DecodeError::BadOption(_))),
            "{size:?}: {r:?}"
        );
    }
    // a decoder by name, a private option it knows, one it does not (logged, not an error)
    let o = DecOptions {
        decoder: Some("mpeg2video".into()),
        opts: vec![
            ("skip_loop_filter".into(), "all".into()),
            ("no_such".into(), "1".into()),
        ],
        ..offline()
    };
    let mut inp = Input::open(&path, &o, None).unwrap();
    inp.start(now());
    assert_eq!(take_all(&inp, 25.0, 100).len(), 3);
}

#[test]
fn a_cuvid_decoder_that_returns_fields_doubles_the_rate() {
    let dir = tempfile::tempdir().unwrap();
    let path = clip(
        &dir,
        "a.mkv",
        &Clip {
            frames: 3,
            ..Clip::default()
        },
    );
    // the rule looks at the options only; mpeg2video ignores them (unknown options are logged)
    let fps = |opts: &[(&str, &str)]| {
        let o = DecOptions {
            opts: opts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..offline()
        };
        Input::open(&path, &o, None).unwrap().fps()
    };
    assert_eq!(fps(&[]), 25.0);
    assert_eq!(fps(&[("deint", "adaptive")]), 50.0);
    assert_eq!(fps(&[("deint", "weave")]), 25.0);
    assert_eq!(fps(&[("deint", "bob"), ("drop_second_field", "1")]), 25.0);
    assert_eq!(fps(&[("deint", "bob"), ("drop_second_field", "0")]), 50.0);
}

#[test]
fn device_contexts_fail_gracefully_without_a_card() {
    tb_decode::log_errors_only();
    // SAFETY: a null context is refused before anything is done with it.
    let r = unsafe { DecDevice::wrap(std::ptr::null_mut(), std::ptr::null_mut()) };
    assert!(matches!(r, Err(DecodeError::NullContext)), "{r:?}");
    // on a machine with the card and its driver this succeeds; here it must be an error value
    match DecDevice::own(0) {
        Ok(d) => assert!(!d.is_shared()),
        Err(e) => {
            assert!(matches!(e, DecodeError::Device { .. }), "{e:?}");
            assert!(!e.to_string().is_empty());
        }
    }
    let r = DecDevice::own(9999);
    assert!(matches!(r, Err(DecodeError::Device { .. })), "{r:?}");
}
