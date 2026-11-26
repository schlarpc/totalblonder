//! `--input-mode dev` on the reference adapter: the clips are held on the device in the padded
//! layout of their format and every frame reaches the encoder by device-to-device copies. The
//! coded streams are those of the host input (the gate's `devin` case, pipe_check.py).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use common::{Scratch, encode, encode_args, parse, shape};
use tb_m2venc::{Status, arena_sizes};

/// Run `args` with host input (plus `host_extra`) and with device input (plus `dev_extra`);
/// every stream of the host run exists in the device run with the same bytes. Returns the
/// number of streams compared.
fn same_streams(dir: &Scratch, tag: &str, args: &str, host_extra: &str, dev_extra: &str) -> usize {
    let mut pictures = [0u64; 2];
    for (i, (mode, extra)) in [("host", host_extra), ("dev", dev_extra)]
        .iter()
        .enumerate()
    {
        let out = encode_args(&format!(
            "{args} --write 99 --output {} --input-mode {mode} {extra}",
            dir.path(&format!("{tag}-{mode}"))
        ));
        assert_eq!(out.status, Status::Complete, "{tag} {mode}");
        assert_eq!(out.report.dev_input, *mode == "dev");
        assert_eq!(out.report.device_errors, 0);
        let json = out.report.json();
        assert!(
            json.contains(&format!("\"dev_input\":{i},")),
            "{tag} {mode}"
        );
        pictures[i] = out.report.pictures;
    }
    assert_eq!(pictures[0], pictures[1], "{tag}");
    let prefix = format!("{tag}-host");
    let mut n = 0;
    for e in std::fs::read_dir(&dir.0).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        if !name.starts_with(&prefix) || !name.ends_with(".m2v") {
            continue;
        }
        let other = name.replacen("-host", "-dev", 1);
        assert!(dir.exists(&other), "{other} is missing");
        let (a, b) = (dir.read(&name), dir.read(&other));
        assert!(shape(&a).ended, "{name}");
        assert!(a == b, "{other} differs from the host path");
        n += 1;
    }
    n
}

/// One group, serial upload on both sides: a format whose coded size is the picture size, one
/// padded in height (interlaced 144 lines are coded as 160) and one padded in width (184
/// samples are coded as 192).
#[test]
fn device_input_codes_the_stream_of_host_input() {
    let dir = Scratch::new("devin-plain");
    let (p, i, w) = (dir.clip("qcif"), dir.clip("qcif-i"), dir.clip("w184"));
    let n = same_streams(
        &dir,
        "p",
        &format!(
            "--input {p} --size 176x144 --gop 15 --bframes 2 --bitrate 400000 --sessions 2 \
             --frames 24 --stagger"
        ),
        "",
        "",
    );
    assert_eq!(n, 2);
    // staggered sessions read the clip at different frames
    assert_ne!(dir.read("p-dev.0.m2v"), dir.read("p-dev.1.m2v"));
    let n = same_streams(
        &dir,
        "i",
        &format!(
            "--input {i} --size 176x144 --interlaced --gop 12 --bframes 2 --bitrate 400000 \
             --sessions 2 --frames 18 --stagger"
        ),
        "",
        "",
    );
    assert_eq!(n, 2);
    let n = same_streams(
        &dir,
        "w",
        &format!("--input {w} --size 184x144 --gop 6 --bframes 1 --q 6 --frames 12"),
        "",
        "",
    );
    assert_eq!(n, 1);
}

/// The overlapped upload: on the host side only (what the gate runs: the frames on the device
/// need no upload), and on both sides.
#[test]
fn device_input_and_the_overlapped_upload() {
    let dir = Scratch::new("devin-overlap");
    let (i, tiny) = (dir.clip("qcif-i"), dir.clip("tiny-i"));
    let args = format!(
        "--input {i} --size 176x144 --interlaced --gop 15 --bframes 2 --bitrate 400000 \
         --vbv 393216 --sessions 2 --frames 24 --stagger"
    );
    assert_eq!(same_streams(&dir, "gate", &args, "--overlap", ""), 2);
    let args = format!(
        "--input {tiny} --size 64x48 --interlaced --gop 6 --bframes 2 --bitrate 300000 \
         --sessions 3 --frames 20 --stagger"
    );
    assert_eq!(
        same_streams(&dir, "both", &args, "--overlap", "--overlap"),
        3
    );
    // the clip is uploaded from the host arena whatever --test-input-copy says
    assert_eq!(
        same_streams(&dir, "copy", &args, "", "--test-input-copy"),
        3
    );
}

/// Groups of different formats and frame rates on one tick (the gate's mixed line-up in
/// small); two of them read one file, one as interlaced and one as progressive pictures, so the
/// clip is on the device in two padded layouts.
#[test]
fn device_input_with_groups_of_different_formats() {
    let dir = Scratch::new("devin-mixed");
    let (p, i, w) = (dir.clip("qcif"), dir.clip("qcif-i"), dir.clip("w184"));
    let args = format!(
        "--input {i} --size 176x144 --interlaced --gop 15 --bframes 2 --bitrate 400000 \
         --sessions 2 --frames 12 --stagger \
         --group --input {w} --size 184x144 --fps 60000/1001 --gop 12 --bframes 2 \
         --bitrate 500000 --sessions 2 --frames 24 --stagger \
         --group --input {p} --size 176x144 --gop 15 --q 7 --frames 12 \
         --group --input {i} --size 176x144 --gop 6 --bframes 0 --q 9 --frames 12"
    );
    assert_eq!(same_streams(&dir, "m", &args, "--overlap", ""), 6);
    for (name, frames) in [("g0.0", 12), ("g1.1", 24), ("g2", 12), ("g3", 12)] {
        assert_eq!(
            shape(&dir.read(&format!("m-dev.{name}.m2v"))).types.len(),
            frames
        );
    }
}

/// A run with captions (the gate's line-ups all carry them): the user data of every picture
/// is that of the host run.
#[test]
fn device_input_with_captions() {
    let dir = Scratch::new("devin-cc");
    let i = dir.clip("qcif-i");
    let args = format!(
        "--input {i} --size 176x144 --interlaced --gop 12 --bframes 2 --bitrate 400000 \
         --sessions 2 --frames 24 --stagger"
    );
    assert_eq!(
        same_streams(&dir, "cc", &format!("{args} --cc"), "--overlap", ""),
        2
    );
    // the captions are in the stream: user data with the ATSC identifier in every picture
    let ga94 = |name: &str| {
        let s = dir.read(name);
        s.windows(8).filter(|w| *w == b"\0\0\x01\xB2GA94").count()
    };
    assert_eq!(ga94("cc-dev.0.m2v"), 24);
    let plain = encode_args(&format!(
        "{args} --frames 6 --input-mode dev --output {}",
        dir.path("nocc")
    ));
    assert_eq!(plain.status, Status::Complete);
    assert_eq!(ga94("nocc.0.m2v"), 0);
}

/// A rate plan, a rate change, a forced key frame and a session on hold act on a frame from
/// the device as on one from the host. (Serial upload on both sides: with the overlapped
/// upload a frame is coded a tick later, which moves what a hold and a rate change act on.)
#[test]
fn device_input_and_the_schedule() {
    let dir = Scratch::new("devin-schedule");
    let i = dir.clip("tiny-i");
    let args = format!(
        "--input {i} --size 64x48 --interlaced --gop 12 --bframes 2 --bitrate 300000 \
         --sessions 2 --frames 36 --stagger --cc --rate-plan 1:200000 --rate-now 20:250000 \
         --force-key 17 --hold 0:1:8:14"
    );
    assert_eq!(same_streams(&dir, "s", &args, "", ""), 2);
}

/// Sessions opened and closed at run time (`--pool`, `--churn`) take their frames from the
/// device as the stable ones do.
#[test]
fn device_input_with_sessions_at_run_time() {
    let dir = Scratch::new("devin-churn");
    let (p, i) = (dir.clip("tiny"), dir.clip("tiny-i"));
    let args = format!(
        "--input {p} --size 64x48 --gop 6 --bframes 2 --bitrate 300000 --sessions 2 \
         --frames 60 --stagger \
         --group --input {i} --size 64x48 --interlaced --gop 6 --bframes 2 --bitrate 300000 \
         --sessions 3 --pool --churn 3:12 --churn-chunk 2 --reap-after 3"
    );
    let n = same_streams(&dir, "c", &args, "", "");
    assert!(n > 3, "{n} streams: the pool opened sessions");
}

/// The device arena holds the clips: every frame in the padded layout of each format it is
/// read in; the host arena holds them as without the mode.
#[test]
fn the_arena_has_room_for_the_clips_on_the_device() {
    let dir = Scratch::new("devin-arena");
    let i = dir.clip("qcif-i");
    let args = format!(
        "--input {i} --size 176x144 --interlaced --gop 6 --q 8 --frames 12 \
         --group --input {i} --size 176x144 --gop 6 --q 8 --frames 12"
    );
    let host = arena_sizes(&parse(&args)).unwrap();
    let o = parse(&format!("{args} --input-mode dev"));
    let dev = arena_sizes(&o).unwrap();
    // 24 frames, coded as 176x160 (interlaced) and as 176x144
    let clips = 24 * (176 * 160 * 3 / 2) + 24 * (176 * 144 * 3 / 2);
    assert_eq!(dev.host, host.host);
    // both sizes are rounded up to 4096
    assert!(dev.device + 4096 > host.device + clips && dev.device < host.device + clips + 4096);
    let out = encode(&o);
    assert_eq!(out.status, Status::Complete);
    assert_eq!(out.report.up_bytes, 0.0);
    assert!(
        out.report
            .text()
            .contains("input on the device (no host frames)")
    );
    // the batches' share of the device arena does not count the clips
    assert!(out.report.dev_bytes >= clips);
}
