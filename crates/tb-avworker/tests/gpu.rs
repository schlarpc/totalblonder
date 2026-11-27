//! Hardware decoding in the sandboxed worker: NVDEC through the `cuda` hwaccel on the machine
//! with the NVIDIA card (nixark), VA-API through the `vaapi` hwaccel on one with an AMD card
//! (bagel). Ignored by default; run with `--ignored` and the test of the machine's card. The clip is made with the `ffmpeg`
//! program when it is on PATH, else taken from `TB_GPU_CLIP` (an Annex B H.264 file with access
//! unit delimiters, 320x240, made elsewhere with `tests/common` `h264`), with its reference
//! decode in `TB_GPU_CLIP.yuv` (`ffmpeg -i clip -f rawvideo -pix_fmt yuv420p`).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::*;
use tb_decoder_client::Decoded;
use tb_media::{CodecId, InputEvent, PixelFormat};

#[test]
#[ignore = "needs the NVIDIA card and driver"]
fn nvdec_in_the_sandbox_matches_software() {
    assert!(
        std::path::Path::new("/dev/nvidia0").exists(),
        "no /dev/nvidia0 here"
    );
    gpu_decode_matches_software();
}

#[test]
#[ignore = "needs an AMD card, its DRM render node and Mesa's VA driver"]
fn vaapi_in_the_sandbox_matches_software() {
    assert!(
        std::path::Path::new("/dev/dri/renderD128").exists(),
        "no /dev/dri/renderD128 here"
    );
    // the worker prefers CUDA: this test is for a machine without it
    assert!(
        !std::path::Path::new("/dev/nvidia0").exists(),
        "an NVIDIA card is here: the worker would use NVDEC"
    );
    gpu_decode_matches_software();
}

/// A clip decoded in the sandboxed worker on the machine's hardware decoder equals the
/// software decode, picture for picture.
fn gpu_decode_matches_software() {
    let (clip, want) = match std::env::var_os("TB_GPU_CLIP") {
        Some(p) => {
            let p = PathBuf::from(p);
            let mut y = p.clone().into_os_string();
            y.push(".yuv");
            (p, std::fs::read(y).unwrap())
        }
        None => {
            assert!(have_ffmpeg());
            let d = dir("gpu");
            let c = h264(&d, "320x240", 60, &[]);
            let r = reference(&c, &["-f", "rawvideo", "-pix_fmt", "yuv420p"]);
            (c, r)
        }
    };
    let aus = split_aud(&std::fs::read(&clip).unwrap());
    let mut c = config();
    c.gpu = true;
    // bisecting aids: TB_GPU_RELAX=ns,mount,pid,landlock turns those layers off, errno
    // makes the filter refuse instead of kill
    let relax = std::env::var("TB_GPU_RELAX").unwrap_or_default();
    let off = tb_sandbox::Requirement::Off;
    if relax.contains("ns") {
        c.policy.user_ns = off;
        c.policy.mount_ns = off;
        c.policy.pid_ns = off;
        c.policy.net_ns = off;
        c.policy.ipc_ns = off;
        c.policy.uts_ns = off;
    }
    if relax.contains("mount") {
        c.policy.mount_ns = off;
    }
    if relax.contains("pid") {
        c.policy.pid_ns = off;
    }
    if relax.contains("errno") {
        c.seccomp = tb_decoder_client::proto::wire::SeccompMode::Errno;
    }
    if relax.contains("landlock") {
        c.policy.landlock = off;
    }
    let mut dec = spawn(c);
    eprintln!("confinement: {}", dec.confinement().unwrap());
    let mut evs = vec![InputEvent::Streams(vec![video_stream(0, CodecId::H264)])];
    for (i, au) in aus.iter().enumerate() {
        evs.push(packet(0, i as i64 * 3000, 3000, i % 30 == 0, au.clone()));
    }
    evs.push(end());
    let (w, h) = (320usize, 240usize);
    let fb = w * h * 3 / 2;
    let mut n = 0usize;
    let mut on = |f: Decoded| {
        let Decoded::Video(v) = f else { panic!() };
        // device frames come back as NV12 (av_hwframe_transfer_data)
        assert_eq!(
            v.desc.format,
            PixelFormat::Nv12,
            "not decoded on the device"
        );
        let mut y = Vec::new();
        for r in 0..h {
            y.extend_from_slice(v.row(0, r).unwrap());
        }
        assert!(
            y == want[n * fb..n * fb + w * h],
            "picture {n}: luma differs"
        );
        // NV12 chroma against planar U and V
        let (u0, v0) = (n * fb + w * h, n * fb + w * h + w * h / 4);
        for r in 0..h / 2 {
            let row = v.row(1, r).unwrap();
            for x in 0..w / 2 {
                assert_eq!(row[2 * x], want[u0 + r * w / 2 + x]);
                assert_eq!(row[2 * x + 1], want[v0 + r * w / 2 + x]);
            }
        }
        n += 1;
    };
    send_all(&mut dec, evs, &mut on);
    drain(&mut dec, &mut on, Duration::from_secs(60));
    assert_eq!(n, aus.len());
    eprintln!("stats: {:?}", dec.stats());
    dec.shutdown(Duration::from_secs(2));
}
