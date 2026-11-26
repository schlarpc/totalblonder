//! The mutation harness: valid streams in which one fixed-length syntax element of a header
//! is overwritten. On the hand-built streams the verifier has to report exactly the rule of
//! the mutation; on streams of real encoders (every file under the directory named by the
//! environment variable `TB_M2V_VERIFY_STREAMS`, if set) it has to report that rule in
//! addition to what the unmodified stream gives. A second test flips random bits and checks
//! that the verifier never panics and never reports a position outside the stream.

mod common;

use common::{B, Bld, I, P, Pic, Seq, base_stream, find_unit, get_bits, set_bits};
use tb_m2v_verify::{Options, Profile, Rule, verify};

/// One mutation: the unit (start code value, extension identifier, which one), the bit
/// offset and width of the element in the unit's payload, the new value, the rule.
struct Mutation {
    name: &'static str,
    code: u8,
    ext: Option<u8>,
    nth: usize,
    bit: usize,
    width: usize,
    /// The new value, given the old one.
    value: fn(u32) -> u32,
    rule: Rule,
    /// Rules that may come with it in a real stream (the mutation changes how the rest is
    /// decoded or judged).
    with: &'static [Rule],
}

#[allow(clippy::too_many_arguments)]
const fn m(
    name: &'static str,
    code: u8,
    ext: Option<u8>,
    nth: usize,
    bit: usize,
    width: usize,
    value: fn(u32) -> u32,
    rule: Rule,
) -> Mutation {
    Mutation {
        name,
        code,
        ext,
        nth,
        bit,
        width,
        value,
        rule,
        with: &[],
    }
}

use Rule as R;

const SEQ: u8 = 0xB3;
const EXT: u8 = 0xB5;
const GOP: u8 = 0xB8;
const PIC: u8 = 0x00;

fn mutations() -> Vec<Mutation> {
    vec![
        m(
            "aspect_ratio_information = 0",
            SEQ,
            None,
            0,
            24,
            4,
            |_| 0,
            R::SeqAspect,
        ),
        m(
            "aspect_ratio_information = 15",
            SEQ,
            None,
            0,
            24,
            4,
            |_| 15,
            R::SeqAspect,
        ),
        m(
            "frame_rate_code = 0",
            SEQ,
            None,
            0,
            28,
            4,
            |_| 0,
            R::SeqFrameRate,
        ),
        m(
            "frame_rate_code = 9",
            SEQ,
            None,
            0,
            28,
            4,
            |_| 9,
            R::SeqFrameRate,
        ),
        m(
            "sequence marker_bit = 0",
            SEQ,
            None,
            0,
            50,
            1,
            |_| 0,
            R::SeqMarker,
        ),
        m(
            "constrained_parameters_flag = 1",
            SEQ,
            None,
            0,
            61,
            1,
            |_| 1,
            R::SeqConstrained,
        ),
        m(
            "profile_and_level_indication reserved level",
            EXT,
            Some(1),
            0,
            4,
            8,
            |v| (v & 0xF0) | 0x0F,
            R::SeqxProfile,
        ),
        m(
            "sequence extension marker_bit = 0",
            EXT,
            Some(1),
            0,
            31,
            1,
            |_| 0,
            R::SeqxMarker,
        ),
        m(
            "time_code marker_bit = 0",
            GOP,
            None,
            0,
            12,
            1,
            |_| 0,
            R::GopMarker,
        ),
        m(
            "time_code_minutes = 61",
            GOP,
            None,
            0,
            6,
            6,
            |_| 61,
            R::GopTimeCodeRange,
        ),
        m(
            "time_code_seconds = 60",
            GOP,
            None,
            0,
            13,
            6,
            |_| 60,
            R::GopTimeCodeRange,
        ),
        m(
            "broken_link = 1",
            GOP,
            None,
            0,
            26,
            1,
            |_| 1,
            R::GopBrokenLink,
        ),
        m(
            "temporal_reference + 5 in the second picture",
            PIC,
            None,
            1,
            0,
            10,
            |v| (v + 5) & 0x3FF,
            R::PicTemporalRef,
        ),
        m(
            "vbv_delay of one picture changed between 0xFFFF and a value",
            PIC,
            None,
            1,
            13,
            16,
            |v| if v == 0xFFFF { 1000 } else { 0xFFFF },
            R::PicVbvDelayMix,
        ),
        m(
            "full_pel_forward_vector = 1",
            PIC,
            None,
            1,
            29,
            1,
            |_| 1,
            R::PicFullPel,
        ),
        m(
            "forward_f_code = 1 in the picture header",
            PIC,
            None,
            1,
            30,
            3,
            |_| 1,
            R::PicFcode7,
        ),
        m(
            "f_code[1][0] = 3 in the first picture",
            EXT,
            Some(8),
            0,
            12,
            4,
            |_| 3,
            R::PceFcodeUnused,
        ),
        m(
            "f_code[1][1] = 12 in the first picture",
            EXT,
            Some(8),
            0,
            16,
            4,
            |_| 12,
            R::PceFcodeReserved,
        ),
        m(
            "chroma_420_type inverted",
            EXT,
            Some(8),
            0,
            31,
            1,
            |v| v ^ 1,
            R::PceChroma420Type,
        ),
    ]
}

fn extra_for(mu: &Mutation) -> &'static [Rule] {
    match mu.rule {
        // a reserved f_code is also not 15
        R::PceFcodeReserved => &[R::PceFcodeUnused],
        // an unusable frame rate changes what the buffer model and time codes see
        R::SeqFrameRate => &[
            R::GopTimeCodePictures,
            R::GopTimeCodeContinuity,
            R::GopDropFrame,
            R::VbvUnderflow,
            R::VbvOverflow,
            R::VbvDelayRate,
        ],
        R::PicVbvDelayMix => &[R::VbvUnderflow, R::VbvOverflow, R::VbvDelayRate],
        R::SeqxProfile => &[R::PlProfile],
        R::SeqAspect => &[R::PlAspect],
        // a zero time code without its marker bit is 25 zero bits
        R::GopMarker => &[R::ScEmulation],
        _ => mu.with,
    }
}

/// Applies a mutation; None if the stream has no such unit.
fn apply(data: &[u8], mu: &Mutation) -> Option<Vec<u8>> {
    let u = find_unit(data, mu.code, mu.ext, mu.nth)?;
    let mut d = data.to_vec();
    let old = get_bits(&d, u, mu.bit, mu.width);
    let new = (mu.value)(old);
    if new == old {
        return None;
    }
    set_bits(&mut d, u, mu.bit, mu.width, new);
    Some(d)
}

#[test]
fn header_mutations_on_the_built_stream() {
    let base = base_stream();
    let opts = Options::default();
    assert!(verify(&base, &opts).counts.is_empty());
    let mut failures = Vec::new();
    for mu in mutations() {
        let Some(d) = apply(&base, &mu) else {
            failures.push(format!("{}: not applicable", mu.name));
            continue;
        };
        let r = verify(&d, &opts);
        let got = r.rules();
        let ok = got.contains(&mu.rule)
            && got
                .iter()
                .all(|x| *x == mu.rule || extra_for(&mu).contains(x));
        // on the built stream only the rule itself is acceptable, except where two rules
        // are one fact
        let strict = got == vec![mu.rule]
            || matches!(mu.rule, R::PceFcodeReserved | R::SeqxProfile | R::GopMarker);
        if !ok || !strict {
            failures.push(format!("{}: got {:?}", mu.name, common::ids(&r)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn stream_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            stream_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "m2v") {
            out.push(p);
        }
    }
}

#[test]
fn header_mutations_on_encoder_streams() {
    let Ok(dir) = std::env::var("TB_M2V_VERIFY_STREAMS") else {
        eprintln!("TB_M2V_VERIFY_STREAMS is not set: no encoder streams mutated");
        return;
    };
    let mut files = Vec::new();
    stream_files(std::path::Path::new(&dir), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no *.m2v under {dir}");
    let opts = Options {
        profile: Profile::H262,
        ..Options::default()
    };
    let mut failures = Vec::new();
    let mut applied = 0;
    for f in &files {
        let Ok(mut base) = std::fs::read(f) else {
            continue;
        };
        // the first 4 MiB are enough; cut at a picture start code so that the cut itself
        // does not break a slice
        if base.len() > 4 << 20 {
            let mut cut = 4 << 20;
            while cut > 0 && base.get(cut..cut + 4) != Some(&[0, 0, 1, 0]) {
                cut -= 1;
            }
            base.truncate(cut);
        }
        let before = verify(&base, &opts).rules();
        for mu in mutations() {
            if before.contains(&mu.rule) {
                continue;
            }
            let Some(d) = apply(&base, &mu) else {
                continue;
            };
            applied += 1;
            let got = verify(&d, &opts).rules();
            // in a real stream the first header has repeats, and GOP time codes a history
            let context: &[Rule] = match (mu.code, mu.ext) {
                (SEQ, _) => &[R::SeqRepeatDiff],
                (EXT, Some(1)) => &[R::SeqRepeatExtDiff],
                (GOP, _) => &[R::GopTimeCodeContinuity],
                _ => &[],
            };
            let ok = got.contains(&mu.rule)
                && got.iter().all(|x| {
                    *x == mu.rule
                        || before.contains(x)
                        || extra_for(&mu).contains(x)
                        || context.contains(x)
                });
            if !ok {
                let ids: Vec<&str> = got.iter().map(|x| x.info().id).collect();
                failures.push(format!("{}: {}: got {:?}", f.display(), mu.name, ids));
            }
        }
    }
    eprintln!("{applied} mutations applied to {} streams", files.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A stream that uses most of the syntax, for the bit-flip test.
fn rich_stream() -> Vec<u8> {
    let mut b = Bld::new(Seq::interlaced());
    b.seq();
    b.sde(0, Some((1, 1, 1)), true);
    b.gop(0, true);
    b.picture(&Pic::interlaced(I, 0));
    b.picture(&Pic::interlaced(P, 3));
    b.picture(&Pic::interlaced(B, 1));
    b.picture(&Pic::interlaced(B, 2));
    b.picture(&Pic::field(P, 4, 1));
    b.picture(&Pic::field(P, 4, 2));
    b.seq_end();
    b.finish()
}

#[test]
fn random_bit_flips_never_panic() {
    let opts = [
        Options::default(),
        Options {
            profile: Profile::Scte43Qam256,
            delivery: tb_m2v_verify::Delivery::Operating { rate: None },
            ..Options::default()
        },
    ];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for base in [base_stream(), rich_stream()] {
        for round in 0..1200 {
            let mut d = base.clone();
            let flips = 1 + (round % 4);
            for _ in 0..flips {
                let bit = (next() % (d.len() as u64 * 8)) as usize;
                d[bit / 8] ^= 0x80 >> (bit % 8);
            }
            if round % 7 == 0 {
                let cut = (next() % d.len() as u64) as usize;
                d.truncate(cut);
            }
            for o in &opts {
                let r = verify(&d, o);
                for f in &r.findings {
                    assert!(f.position.byte <= d.len() as u64);
                }
            }
        }
    }
}
