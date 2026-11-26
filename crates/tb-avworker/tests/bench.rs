//! Overhead of the sandboxed worker against in-process decoding (`tb-decode`), 1080p30 H.264
//! decoded in software as fast as it goes, one decoder thread each. Ignored by default (it
//! takes seconds and measures the machine); run with
//! `cargo test -p tb-avworker --release --test bench -- --ignored --nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a bench"
)]
#![allow(unsafe_code, reason = "getrusage")]

mod common;

use std::time::{Duration, Instant};

use common::*;
use tb_decoder_client::Decoded;
use tb_decoder_client::proto::wire::{Aspect, Deinterlace, OutputSpec, StreamOpen, VideoSpec};
use tb_media::{CodecId, Rational};

fn rusage(who: i32) -> f64 {
    // SAFETY: getrusage writes the struct it is given; a zeroed rusage is a valid value.
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    unsafe { libc::getrusage(who, &raw mut r) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 * 1e-6;
    t(r.ru_utime) + t(r.ru_stime)
}

const FRAMES: u32 = 600;

fn clip() -> std::path::PathBuf {
    h264(
        &dir("bench"),
        "1920x1080",
        FRAMES,
        &["-preset", "medium", "-b:v", "8M"],
    )
}

fn worker(output: OutputSpec, threads: u8) -> (f64, f64, f64) {
    let aus = split_aud(&std::fs::read(clip()).unwrap());
    let mut c = config();
    c.threads = threads;
    c.slot_wait_ms = 1000;
    c.input_ring_bytes = 32 << 20;
    let mut dec = spawn(c);
    dec.try_send_streams(vec![StreamOpen {
        info: video_stream(0, CodecId::H264),
        output,
    }])
    .unwrap();
    let children0 = rusage(libc::RUSAGE_CHILDREN);
    let thread0 = rusage(libc::RUSAGE_THREAD);
    let t0 = Instant::now();
    let mut n = 0u32;
    let mut evs = Vec::new();
    for (i, au) in aus.into_iter().enumerate() {
        evs.push(packet(0, i as i64 * 3000, 3000, i % 30 == 0, au));
    }
    evs.push(end());
    let mut on = |f: Decoded| {
        let Decoded::Video(v) = f else { panic!() };
        // touch the picture as a consumer would (one read of each row)
        std::hint::black_box(v.slot_bytes()[0]);
        n += 1;
    };
    send_all(&mut dec, evs, &mut on);
    drain(&mut dec, &mut on, Duration::from_secs(600));
    let wall = t0.elapsed().as_secs_f64();
    let parent = rusage(libc::RUSAGE_THREAD) - thread0;
    let s = dec.stats();
    dec.shutdown(Duration::from_secs(5));
    let child = rusage(libc::RUSAGE_CHILDREN) - children0;
    assert_eq!(n, FRAMES, "{s:?}");
    (wall, child, parent)
}

fn in_process(host: bool) -> (f64, f64) {
    use tb_decode::{DecOptions, Input};
    let o = DecOptions {
        sw: true,
        sw_threads: 1,
        block: true,
        looping: false,
        qmax: 8,
        host_size: host.then_some((1920, 1080)),
        ..DecOptions::default()
    };
    let path = clip();
    let mut inp = Input::open(&path, &o, None).unwrap();
    let t0 = Instant::now();
    inp.start(tb_decode::now());
    let mut taken = 0u64;
    loop {
        let (f, skipped) = inp.take(1e18);
        taken += u64::from(skipped) + u64::from(f.is_some());
        if f.is_none() && inp.ended() {
            break;
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    let st = inp.stats();
    assert_eq!(st.decoded, u64::from(FRAMES));
    assert_eq!(taken, u64::from(FRAMES));
    (wall, st.cpu_us as f64 * 1e-6)
}

#[test]
#[ignore = "a measurement: run by hand"]
fn start_up_latency() {
    let mut t = Vec::new();
    for _ in 0..20 {
        let t0 = Instant::now();
        let d = spawn(config());
        t.push(t0.elapsed().as_secs_f64() * 1e3);
        d.shutdown(Duration::from_secs(2));
    }
    t.sort_by(f64::total_cmp);
    println!(
        "spawn to Ready, 20 runs: min {:.1} ms, median {:.1} ms, max {:.1} ms",
        t[0], t[10], t[19]
    );
}

#[test]
#[ignore = "a measurement: run by hand"]
fn overhead_1080p30_h264() {
    if !have_ffmpeg() {
        return;
    }
    let secs = f64::from(FRAMES) / 30.0;
    let report = |name: &str, wall: f64, cpu: f64| {
        println!(
            "{name:<48} {:7.1} frames/s  {:6.3} s CPU  {:5.3} cores per 1080p30 stream",
            f64::from(FRAMES) / wall,
            cpu,
            cpu / secs
        );
    };
    // warm the file cache and the fixture
    let _ = in_process(false);
    for _ in 0..2 {
        let (w, c) = in_process(false);
        report("in-process tb-decode (decode only)", w, c);
        let (w, c) = in_process(true);
        report("in-process tb-decode (+ swscale copy to host)", w, c);
        let (w, child, parent) = worker(OutputSpec::Native, 1);
        report("sandboxed worker, native (worker CPU)", w, child);
        report("  parent thread (feed, validate, poll)", w, parent);
        let (w, child, parent) = worker(
            OutputSpec::Video(VideoSpec {
                width: 704,
                height: 480,
                display_aspect: Rational::new(4, 3).unwrap(),
                aspect: Aspect::Fit,
                deinterlace: Deinterlace::Off,
            }),
            1,
        );
        report("sandboxed worker, converted to 704x480 (worker)", w, child);
        report("  parent thread", w, parent);
    }
}
