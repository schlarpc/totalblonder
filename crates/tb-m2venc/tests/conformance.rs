//! The driver on the reference device with content and configurations chosen to break rules,
//! each stream through the conformance verifier (`tb-m2v-verify`, written from the standards'
//! text): no finding of any severity under H.262, and the counters show that the rule was
//! really reached.

mod common;

use common::{Scratch, encode_args};
use tb_m2v_verify::{Delivery, Options, Profile, Report, Rule, verify};

/// Every finding of a stream under H.262 with the operating buffer model.
fn findings(dir: &Scratch, name: &str) -> Report {
    let data = dir.read(name);
    let opts = Options {
        profile: Profile::H262,
        delivery: Delivery::Operating { rate: None },
        ..Options::default()
    };
    verify(&data, &opts)
}

fn assert_clean(r: &Report, what: &str) {
    assert!(
        r.counts.is_empty(),
        "{what}: {:?}",
        r.findings
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
    );
    assert!(r.pictures > 0, "{what}: no picture");
}

/// H.262 Table 8-6 (p.110): at most 4608 bits per macroblock, two per row excepted. White
/// noise at a rate that gives the finest quantizers reaches 9000 bits a macroblock; the rows
/// are quantized again until the rule holds.
#[test]
fn white_noise_at_a_high_rate_keeps_the_macroblock_limit() {
    let dir = Scratch::new("mb-white");
    let clip = dir.clip("white");
    let out = encode_args(&format!(
        "--input {clip} --size 176x144 --gop 6 --bframes 2 --bitrate 15000000 --output {}",
        dir.path("w")
    ));
    assert!(out.report.rate.mbfix > 0, "the limit was never reached");
    let r = findings(&dir, "w.m2v");
    assert_eq!(r.count(Rule::MbBits), 0);
    assert_clean(&r, "white noise");
}

/// The case of `notes/format-conformance.md`, open question 5: a busy patch in an easy picture.
/// Rate control picks a fine quantizer for the whole picture and the rows of the patch break
/// the rule; only they are made coarser.
#[test]
fn a_noise_patch_in_an_easy_picture_keeps_the_macroblock_limit() {
    let dir = Scratch::new("mb-patch");
    let clip = dir.clip("patch");
    let out = encode_args(&format!(
        "--input {clip} --size 176x144 --gop 6 --bframes 2 --bitrate 8000000 --output {}",
        dir.path("p")
    ));
    assert!(out.report.rate.mbfix > 0, "the limit was never reached");
    let r = findings(&dir, "p.m2v");
    assert_eq!(r.count(Rule::MbBits), 0);
    assert_clean(&r, "noise patch");
}

/// Fixed quantizer 1 on noise (the configuration that used to lose macroblocks: pictures cut
/// at the size of the output buffer, 2188 macroblocks above 4608 bits, the buffer underflowing
/// at 18 pictures). The quantizer now stands only while the picture fits the declared buffer
/// and the macroblock size rule; the stream is whole and legal.
#[test]
fn fixed_quantizer_one_on_noise_is_a_legal_stream() {
    let dir = Scratch::new("fixed-q1");
    let clip = dir.clip("noisy352");
    let out = encode_args(&format!(
        "--input {clip} --size 352x240 --q 1 --output {}",
        dir.path("q1")
    ));
    assert!(
        out.report.rate.retries > 0,
        "no picture was quantized again"
    );
    assert_eq!(out.report.rate.violations, 0);
    let r = findings(&dir, "q1.m2v");
    assert_eq!(r.count(Rule::SliceGap), 0);
    assert_eq!(r.count(Rule::MbBits), 0);
    assert_eq!(r.count(Rule::VbvUnderflow), 0);
    assert_clean(&r, "fixed quantizer 1");
    assert_eq!(u64::from(r.pictures), out.report.pictures);
}

/// A fixed quantizer with a small declared buffer: the pictures cannot fit at that quantizer,
/// and the ladder is the backstop down to all-skipped pictures.
#[test]
fn fixed_quantizer_with_a_small_buffer_goes_up_the_ladder() {
    let dir = Scratch::new("fixed-small");
    let clip = dir.clip("white");
    let out = encode_args(&format!(
        "--input {clip} --size 176x144 --q 2 --max-bitrate 400000 --vbv 65536 --output {}",
        dir.path("f")
    ));
    assert!(out.report.rate.rung_pics > 0, "the ladder was not needed");
    assert_eq!(out.report.rate.violations, 0);
    assert_clean(&findings(&dir, "f.m2v"), "fixed quantizer, small buffer");
}

/// The lowest configuration the encoder accepts (the smallest buffer, the rate of the buffer
/// guarantee) on white noise: the first picture, which can be neither skipped nor replaced,
/// fits as the bound of `first_picture_bits` says, and no picture ever breaks the buffer.
#[test]
fn the_lowest_rate_and_buffer_hold_on_white_noise() {
    let dir = Scratch::new("lowest");
    let clip = dir.clip("white");
    for (name, args) in [
        ("p", "--gop 6 --bframes 2"),
        ("i", "--gop 2 --bframes 0 --interlaced"),
    ] {
        let out = encode_args(&format!(
            "--input {clip} --size 176x144 {args} --bitrate 140000 --vbv 16384 --output {}",
            dir.path(name)
        ));
        assert_eq!(out.report.rate.violations, 0, "{name}");
        assert!(out.report.rate.rung_pics > 0, "{name}");
        assert_clean(&findings(&dir, &format!("{name}.m2v")), name);
    }
}

/// The picture coding extensions of a stream: (picture_coding_type, frame_pred_frame_dct,
/// alternate_scan, progressive_frame) per picture, in coded order.
fn picture_flags(data: &[u8]) -> Vec<(u8, bool, bool, bool)> {
    let mut out = Vec::new();
    let mut ptype = 0;
    let mut i = 0;
    while i + 8 < data.len() {
        if data[i..i + 3] == [0, 0, 1] {
            if data[i + 3] == 0x00 {
                // temporal_reference (10 bits), picture_coding_type (3)
                ptype = (data[i + 5] >> 3) & 7;
            } else if data[i + 3] == 0xB5 && data[i + 4] >> 4 == 8 {
                // 6.2.3.1: extension id 8 and four f_codes (20 bits), intra_dc_precision 2,
                // picture_structure 2; then a byte of top_field_first, frame_pred_frame_dct,
                // concealment_motion_vectors, q_scale_type, intra_vlc_format, alternate_scan,
                // repeat_first_field, chroma_420_type; then progressive_frame
                let b = data[i + 7];
                out.push((ptype, b & 0x40 != 0, b & 0x04 != 0, data[i + 8] & 0x80 != 0));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

/// A/53 Part 4 6.1.6 (p.11) and SCTE 43 7.1.6 (p.4): frame_pred_frame_dct shall be 1 when
/// progressive_frame is 1. In an interlaced session a picture submitted as progressive is coded
/// with frame prediction and frame DCT only and says so; the others keep the field tools.
#[test]
fn a_progressive_picture_in_an_interlaced_session_has_no_field_tools() {
    let dir = Scratch::new("prog-frame");
    let clip = dir.clip("qcif");
    let base = format!(
        "--input {clip} --size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 600000 \
         --frames 18 --input-mode dev"
    );
    // every picture progressive
    encode_args(&format!(
        "{base} --test-progressive 1 --output {}",
        dir.path("all")
    ));
    let all = dir.read("all.m2v");
    let flags = picture_flags(&all);
    assert_eq!(flags.len(), 18);
    for (n, (_, fpfd, alt, prog)) in flags.iter().enumerate() {
        assert!(
            *prog && *fpfd,
            "picture {n}: progressive_frame {prog}, fpfd {fpfd}"
        );
        assert!(
            !*alt,
            "picture {n}: the zigzag scan for a progressive frame"
        );
    }
    let r = findings(&dir, "all.m2v");
    assert_clean(&r, "all progressive");
    // no macroblock says "field": the syntax has no place for it, and the verifier decoded
    // every macroblock
    assert_eq!(
        r.features
            .get("field-predicted macroblocks")
            .copied()
            .unwrap_or(0),
        0
    );
    assert_eq!(
        r.features
            .get("field DCT macroblocks")
            .copied()
            .unwrap_or(0),
        0
    );

    // none progressive: the field tools, the alternate scan, progressive_frame 0
    encode_args(&format!("{base} --output {}", dir.path("none")));
    let none = picture_flags(&dir.read("none.m2v"));
    assert!(
        none.iter()
            .all(|(_, fpfd, alt, prog)| !*fpfd && *alt && !*prog)
    );
    assert_clean(&findings(&dir, "none.m2v"), "none progressive");

    // mixed in one session (every third frame interlaced; key frames every 4, one B picture
    // between anchors, so that every picture type has both kinds): each picture by its own
    // flag, and the stream stays legal across the changes
    encode_args(&format!(
        "{base} --gop 4 --bframes 1 --test-progressive 3 --output {}",
        dir.path("mix")
    ));
    let mix = picture_flags(&dir.read("mix.m2v"));
    assert_eq!(mix.len(), 18);
    let n_prog = mix.iter().filter(|f| f.3).count();
    assert_eq!(n_prog, 12);
    for (ptype, fpfd, alt, prog) in &mix {
        assert_eq!(fpfd, prog, "type {ptype}");
        assert_eq!(*alt, !*prog, "type {ptype}");
    }
    for t in 1..=3u8 {
        assert!(
            mix.iter().any(|f| f.0 == t && f.3),
            "no progressive picture of type {t}"
        );
        assert!(
            mix.iter().any(|f| f.0 == t && !f.3),
            "no interlaced picture of type {t}"
        );
    }
    assert_clean(&findings(&dir, "mix.m2v"), "mixed");

    // a session that asks for the alternate scan keeps it on progressive pictures
    encode_args(&format!(
        "{base} --alt-scan 1 --test-progressive 1 --output {}",
        dir.path("alt")
    ));
    assert!(
        picture_flags(&dir.read("alt.m2v"))
            .iter()
            .all(|f| f.1 && f.2 && f.3)
    );
    assert_clean(&findings(&dir, "alt.m2v"), "alternate scan kept");
}
