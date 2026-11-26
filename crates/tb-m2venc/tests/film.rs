//! Film mode end to end on the reference device: the driver's `--film` (frames shown for two
//! and three fields, or one, two and three frame periods), the real kernels, and every stream
//! through the conformance verifier, which was shown right on other encoders' soft-telecined
//! streams first (`notes/m2v-conformance-findings.md` §6). No finding of any severity under
//! H.262 with the operating buffer model, and under the broadcast profile nothing but what
//! the small test picture itself brings.

mod common;

use common::{Scratch, encode_args};
use tb_m2v_verify::{Delivery, Options, Profile, Report, Rule, verify};

/// Verify a stream under H.262 (everything must be clean) and under the cable profile (only
/// the notes of a 176x144 picture: format table, level, color).
fn clean(dir: &Scratch, name: &str, what: &str) -> Report {
    let data = dir.read(name);
    let h262 = verify(
        &data,
        &Options {
            profile: Profile::H262,
            delivery: Delivery::Operating { rate: None },
            ..Options::default()
        },
    );
    assert!(
        h262.counts.is_empty(),
        "{what}: {:?}",
        h262.findings
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
    );
    let cable = verify(
        &data,
        &Options {
            profile: Profile::Scte43Qam256,
            ..Options::default()
        },
    );
    let size = [
        Rule::BcFormat,
        Rule::BcLevelLowest,
        Rule::BcColor709,
        Rule::BcSeqInterval,
    ];
    let other: Vec<_> = cable
        .findings
        .iter()
        .filter(|f| !size.contains(&f.rule))
        .map(|f| format!("{f:?}"))
        .collect();
    assert!(other.is_empty(), "{what}, cable profile: {other:?}");
    assert!(h262.pictures > 0, "{what}: no picture");
    h262
}

fn feature(r: &Report, name: &str) -> u64 {
    r.features.get(name).copied().unwrap_or(0)
}

const SD: &str =
    "--size 176x144 --interlaced --frames 40 --bitrate 600000 --max-bitrate 1000000 --vbv 327680";
const P60: &str = "--size 176x144 --fps 60000/1001 --frames 40 --bitrate 600000 --max-bitrate 1000000 --vbv 327680 --gop 30";

#[test]
fn three_two_pulldown_with_captions() {
    let dir = Scratch::new("film-32");
    let clip = dir.clip("qcif");
    for (name, args) in [
        ("tff", ""),
        ("bff", "--bff"),
        ("nob", "--bframes 0"),
        ("m2", "--bframes 1 --gop 12"),
    ] {
        let out = encode_args(&format!(
            "--input {clip} {SD} {args} --film 3,2 --cc --output {}",
            dir.path(name)
        ));
        assert_eq!(out.report.shown, [0, 0, 20, 20], "{name}");
        assert_eq!(out.report.rate.violations, 0);
        let r = clean(&dir, &format!("{name}.m2v"), name);
        assert_eq!(r.pictures, 40);
        assert_eq!(feature(&r, "repeat_first_field 1"), 20, "{name}");
        assert_eq!(feature(&r, "progressive_frame 1"), 40, "{name}");
        // 40 film frames are 100 fields: 50 frame periods, key frames 15 (or 12) apart
        let gops = feature(&r, "GOP header, closed_gop 1");
        assert_eq!(gops, if name == "m2" { 5 } else { 4 }, "{name}");
    }
}

/// Film, video and stills in one session, in any order: cadence breaks of every kind.
#[test]
fn mixed_cadences_in_one_session() {
    let dir = Scratch::new("film-mixed");
    let clip = dir.clip("qcif");
    for (name, film) in [
        ("cut", "3,2,3,2,2,3,2,3,3,2"),
        ("video", "3,2,3,2,i,i,i,3,2,2,i,3,3,i"),
        ("twos", "2,2,2,3,2,2,2,2,3,3,3"),
        ("odd", "3,i,3,i,i,3,2,3"),
    ] {
        for extra in ["", "--overlap", "--input-mode dev"] {
            let out = encode_args(&format!(
                "--input {clip} {SD} --film {film} --cc {extra} --output {}",
                dir.path(name)
            ));
            assert_eq!(out.report.rate.violations, 0);
            let r = clean(&dir, &format!("{name}.m2v"), &format!("{name} {extra}"));
            assert_eq!(r.pictures, 40);
            assert!(feature(&r, "repeat_first_field 1") > 0);
        }
    }
}

/// A progressive session at 59.94 Hz: frames shown one, two and three times (H.262 6.3.10,
/// Table 8-7, Annex C.9).
#[test]
fn frame_repeats_at_59_94_hz() {
    let dir = Scratch::new("film-60p");
    let clip = dir.clip("qcif");
    for (name, film, shown) in [
        ("film", "3,2", [0, 0, 20, 20]),
        ("mixed", "1,1,3,2,2,1,3,3", [0, 15, 10, 15]),
        ("nob", "3,2", [0, 0, 20, 20]),
    ] {
        let b = if name == "nob" { "--bframes 0" } else { "" };
        let out = encode_args(&format!(
            "--input {clip} {P60} {b} --film {film} --cc --output {}",
            dir.path(name)
        ));
        assert_eq!(out.report.shown, shown, "{name}");
        assert_eq!(out.report.rate.violations, 0);
        let r = clean(&dir, &format!("{name}.m2v"), name);
        assert_eq!(r.pictures, 40);
        assert_eq!(
            feature(&r, "repeat_first_field 1"),
            shown[2] + shown[3],
            "{name}"
        );
    }
}

/// Film under everything that replaces or moves pictures: a hold, pictures given up at
/// random, forced key frames, rate changes at key frames and at once.
#[test]
fn film_under_holds_shedding_keys_and_rate_changes() {
    let dir = Scratch::new("film-pressure");
    let clip = dir.clip("qcif");
    for (name, fmt, extra) in [
        ("hold", SD, "--hold 0:0:9:21"),
        ("shed", SD, "--force-shed 0.25:5"),
        ("keys", SD, "--force-key 7,8,22"),
        (
            "rate",
            SD,
            "--rate-plan 1:300000,2:900000 --rate-now 17:500000,30:250000",
        ),
        ("hold60", P60, "--hold 0:0:9:21"),
        ("shed60", P60, "--force-shed 0.25:5 --force-key 13"),
        ("rate60", P60, "--rate-now 5:300000,23:900000"),
    ] {
        let out = encode_args(&format!(
            "--input {clip} {fmt} --film 3,2 --cc {extra} --output {}",
            dir.path(name)
        ));
        assert_eq!(out.report.rate.violations, 0, "{name}");
        let r = clean(&dir, &format!("{name}.m2v"), name);
        assert_eq!(r.pictures, 40, "{name}");
        if name.starts_with("hold") || name.starts_with("shed") {
            assert!(out.report.repeats > 0, "{name}: nothing was repeated");
        }
    }
}

/// A film session and a video session of one format share a batch: neither sees the other.
#[test]
fn film_and_video_sessions_in_one_batch() {
    let dir = Scratch::new("film-batch");
    let clip = dir.clip("qcif");
    let alone = |name: &str, film: &str| {
        encode_args(&format!(
            "--input {clip} {SD} {film} --output {}",
            dir.path(name)
        ));
        dir.read(&format!("{name}.m2v"))
    };
    let (film, video) = (alone("f", "--film 3,2"), alone("v", ""));
    encode_args(&format!(
        "--input {clip} {SD} --film 3,2 --group --input {clip} {SD} --write 2 --output {}",
        dir.path("both")
    ));
    assert_eq!(dir.read("both.g0.m2v"), film);
    assert_eq!(dir.read("both.g1.m2v"), video);
    clean(&dir, "both.g0.m2v", "film next to video");
    clean(&dir, "both.g1.m2v", "video next to film");
}
