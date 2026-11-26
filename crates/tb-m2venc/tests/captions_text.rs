//! Captions made from a SubRip file (`--cc-text`, `tb_captions`) through the real encoder on
//! the reference device: the stream passes the conformance verifier's caption rules, and the
//! captions read back from the coded pictures say what the file says, when it says it.
//!
//! The second test reads the stream with real decoders (ffmpeg's CEA-608 decoder,
//! CCExtractor for CEA-608 and CEA-708). It needs those programs and is ignored; to run it:
//!
//! ```text
//! nix build --inputs-from . nixpkgs#ccextractor -o /tmp/ccx      # optional
//! CCEXTRACTOR=/tmp/ccx/bin/ccextractor nix develop -c cargo nextest run -p tb-m2venc \
//!     --release --run-ignored only -E 'test(real_decoders)'
//! ```
//!
//! A decoder that is not found is skipped with a message.

mod common;

use common::{Scratch, encode_args};
use tb_captions::cea608::Channel;
use tb_captions::cea608::decode::Decoder as Dec608;
use tb_captions::cea708::ServiceNumber;
use tb_captions::cea708::decode::Decoder as Dec708;
use tb_captions::{Cue, FieldRate, Millis};
use tb_m2v_verify::{Options, Profile, Rule, verify};

/// The `cc_data()` constructs of every picture, in stream order (A/53 Part 4 §6.2.3.1:
/// `GA94`, type 3, the flags and cc_count, 0xFF, the constructs).
fn cc_of_pictures(m2v: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let tag = [0, 0, 1, 0xB2, b'G', b'A', b'9', b'4', 3];
    let mut i = 0;
    while let Some(k) = m2v[i..].windows(tag.len()).position(|w| w == tag) {
        let at = i + k + tag.len();
        let n = usize::from(m2v[at] & 0x1F);
        out.push(m2v[at + 2..at + 2 + 3 * n].to_vec());
        i = at;
    }
    out
}

/// Changes of the visible text: `(time, text)`.
type Changes = Vec<(Millis, String)>;

/// Text changes `(time, text)` of CC1 and of service 1, the pictures in display order, picture
/// `n` shown for `fields(n)` field periods.
fn read_back(pictures: &[Vec<u8>], fields: impl Fn(usize) -> i64) -> (Changes, Changes) {
    let (mut d608, mut d708) = (
        Dec608::new(Channel::Cc1),
        Dec708::new(ServiceNumber::PRIMARY),
    );
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut p = 0;
    for (n, cc) in pictures.iter().enumerate() {
        let t = Millis::of_field_period(p, FieldRate::NTSC);
        p += fields(n);
        for c in tb_captions::ccdata::constructs(cc) {
            match c.ty {
                0 if c.valid && d608.feed(c.data) => a.push((t, d608.text())),
                2 | 3 if d708.feed(c.valid, c.ty, c.data) => b.push((t, d708.svc.text())),
                _ => {}
            }
        }
    }
    (a, b)
}

/// What the cues say: `(time, text)` at each start and at each end before a gap.
fn expected(cues: &[Cue]) -> Vec<(Millis, String)> {
    changes(cues, 0)
}

/// [`expected`], gaps of up to `gap` ms between cues taken as none (a decoder's SRT writer
/// may end a cue a few milliseconds before the next).
fn changes(cues: &[Cue], gap: i64) -> Vec<(Millis, String)> {
    let mut v = Vec::new();
    for (i, c) in cues.iter().enumerate() {
        v.push((c.start, c.text()));
        if cues.get(i + 1).is_none_or(|n| n.start.0 > c.end.0 + gap) {
            v.push((c.end, String::new()));
        }
    }
    v
}

fn same(got: &[(Millis, String)], want: &[(Millis, String)], tol_ms: i64, what: &str) {
    assert_eq!(
        got.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
        want.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
        "{what}"
    );
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.0.0 - w.0.0).abs() <= tol_ms,
            "{what}: {:?} at {}, wanted {}",
            w.1,
            g.0,
            w.0
        );
    }
}

// the second cue's load (special and extended characters, each a doubled code) takes about a
// second of CEA-608's two bytes a frame: it gets the 1.5 s the first cue is shown
const SRT: &str = "1\n00:00:01,000 --> 00:00:02,500\nHello, <i>world</i>!\n\n\
                   2\n00:00:02,500 --> 00:00:03,200\n♪ Ünïcödé ½ ¿Qué? ♪\n\n\
                   3\n00:00:03,500 --> 00:00:03,900\n{\\an8}Top\n";

#[test]
fn text_captions_through_the_encoder() {
    let dir = Scratch::new("cc-text");
    let clip = dir.clip("tiny-i");
    let srt = dir.path("in.srt");
    std::fs::write(&srt, SRT).unwrap();
    // no B pictures: stream order is display order, which the read-back needs
    let out = encode_args(&format!(
        "--input {clip} --size 64x48 --interlaced --gop 15 --bframes 0 --bitrate 200000 \
         --frames 120 --cc-text {srt} --output {}",
        dir.path("cc")
    ));
    assert_eq!(out.report.rate.violations, 0);
    let data = dir.read("cc.m2v");
    // the caption rules of the verifier (the cable profile; the picture is a test size)
    let r = verify(
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
    let other: Vec<_> = r
        .findings
        .iter()
        .filter(|f| !size.contains(&f.rule))
        .map(|f| format!("{f:?}"))
        .collect();
    assert!(other.is_empty(), "{other:?}");
    let pictures = cc_of_pictures(&data);
    assert_eq!(pictures.len(), 120);
    assert!(pictures.iter().all(|p| p.len() == 60));
    let cues = tb_captions::srt::parse(SRT).cues;
    let (a, b) = read_back(&pictures, |_| 2);
    let want = expected(&cues);
    // CEA-608 within a frame, CEA-708 within a picture and a field period
    same(&a, &want, 34, "CC1");
    same(&b, &want, 51, "service 1");
}

/// Film pictures of three and two fields (cc_count 30 and 20), and 59.94 Hz frames shown once
/// (cc_count 10, the CEA-608 fields in turn).
#[test]
fn text_captions_on_film_and_at_59_94_hz() {
    let dir = Scratch::new("cc-text-film");
    let clip = dir.clip("tiny");
    let srt = dir.path("in.srt");
    std::fs::write(&srt, SRT).unwrap();
    let cues = tb_captions::srt::parse(SRT).cues;
    let want = expected(&cues);
    for (name, args, fields, n) in [
        (
            "film",
            "--interlaced --film 3,2 --frames 96",
            [3i64, 2],
            96usize,
        ),
        ("p60", "--fps 60000/1001 --gop 30 --frames 240", [1, 1], 240),
    ] {
        encode_args(&format!(
            "--input {clip} --size 64x48 {args} --bframes 0 --bitrate 400000 --cc-text {srt} \
             --output {}",
            dir.path(name)
        ));
        let data = dir.read(&format!("{name}.m2v"));
        let r = verify(
            &data,
            &Options {
                profile: Profile::Scte43Qam256,
                ..Options::default()
            },
        );
        let caption_findings: Vec<_> = r
            .findings
            .iter()
            .filter(|f| format!("{:?}", f.rule).starts_with("Cc"))
            .map(|f| format!("{f:?}"))
            .collect();
        assert!(caption_findings.is_empty(), "{name}: {caption_findings:?}");
        let pictures = cc_of_pictures(&data);
        assert_eq!(pictures.len(), n, "{name}");
        for (k, p) in pictures.iter().enumerate() {
            assert_eq!(p.len() as i64, 30 * fields[k % 2], "{name} picture {k}");
        }
        let (a, b) = read_back(&pictures, |k| fields[k % 2]);
        // a film picture lasts up to three field periods (50 ms)
        same(&a, &want, 51, &format!("{name} CC1"));
        same(&b, &want, 51, &format!("{name} service 1"));
    }
}

/// A decoder's SRT output as `(time, text)` changes, like [`expected`]. `\h` (ffmpeg's hard
/// space) becomes a space; markup goes; lines are joined with `\n` after trimming.
fn decoded(srt: &str) -> Vec<(Millis, String)> {
    let cues = tb_captions::srt::parse(&srt.replace("\\h", " ")).cues;
    flat(changes(&cues, 100))
}

/// Line breaks as spaces: the layout wraps long lines, the input file does not.
fn flat(v: Vec<(Millis, String)>) -> Vec<(Millis, String)> {
    v.into_iter()
        .map(|(t, s)| (t, s.replace('\n', " ")))
        .collect()
}

fn tool(env: &str, name: &str) -> Option<String> {
    let p = std::env::var(env).unwrap_or_else(|_| name.to_owned());
    std::process::Command::new(&p)
        .arg("--help")
        .output()
        .ok()
        .map(|_| p)
}

const LONG_SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nHello, <i>world</i>!\nSecond line\n\n\
     2\n00:00:03,000 --> 00:00:04,500\nÜnïcödé ♪ “quotes” ½ ¿Qué?\n\n\
     3\n00:00:06,000 --> 00:00:07,000\n{\\an8}Top of the screen\n\n\
     4\n00:00:08,000 --> 00:00:09,500\nFaçade, naïve — crème brûlée & Ça va?\n";

#[test]
#[ignore = "needs ffmpeg and CCExtractor; see the module documentation"]
fn real_decoders() {
    let dir = Scratch::new("cc-real");
    let clip = dir.clip("qcif-i");
    let srt = dir.path("in.srt");
    std::fs::write(&srt, LONG_SRT).unwrap();
    // B pictures this time: the decoders reorder the captions with the pictures
    let m2v = dir.path("cc.m2v");
    encode_args(&format!(
        "--input {clip} --size 176x144 --interlaced --gop 15 --bframes 2 --bitrate 400000 \
         --frames 330 --cc-text {srt} --output {}",
        dir.path("cc")
    ));
    let cues = tb_captions::srt::parse(LONG_SRT).cues;
    let want = flat(expected(&cues));
    let mut ran = 0;
    let sh = |cmd: &str, args: &[&str]| {
        let o = std::process::Command::new(cmd).args(args).output().unwrap();
        assert!(
            o.status.success(),
            "{cmd} {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    if let Some(ff) = tool("FFMPEG", "ffmpeg") {
        // ffmpeg's ccaption_dec, field 1 (its automatic choice takes the field of the first
        // construct, which is field 2 in a top-field-first picture: see the log)
        let out = dir.path("ff.srt");
        let movie = format!("movie={m2v}[out0+subcc]");
        sh(
            &ff,
            &[
                "-y",
                "-loglevel",
                "error",
                "-data_field",
                "first",
                "-f",
                "lavfi",
                "-i",
                &movie,
                "-map",
                "0:s",
                "-c:s",
                "srt",
                &out,
            ],
        );
        let got = decoded(&std::fs::read_to_string(&out).unwrap());
        // ffmpeg 6.1 shows the extended em dash (0x12 0x2A) as a hyphen
        let want_ff: Vec<(Millis, String)> = want
            .iter()
            .map(|(t, s)| (*t, s.replace('—', "-")))
            .collect();
        same(&got, &want_ff, 34, "ffmpeg CC1");
        ran += 1;
        if let Some(ccx) = tool("CCEXTRACTOR", "ccextractor") {
            // CCExtractor reads only the first GOP of an elementary stream (it does the same
            // with ffmpeg's own streams): give it a transport stream
            let ts = dir.path("cc.ts");
            sh(
                &ff,
                &[
                    "-y",
                    "-loglevel",
                    "error",
                    "-fflags",
                    "+genpts",
                    "-r",
                    "30000/1001",
                    "-i",
                    &m2v,
                    "-c",
                    "copy",
                    "-f",
                    "mpegts",
                    &ts,
                ],
            );
            let c608 = dir.path("x608.srt");
            sh(&ccx, &[&ts, "-o", &c608]);
            let got = decoded(&std::fs::read_to_string(&c608).unwrap());
            // its times are about three frames late on a stream with B pictures
            same(&got, &want, 150, "CCExtractor CC1");
            let c708 = dir.path("x708.srt");
            sh(&ccx, &[&ts, "-svc", "1[ISO-8859-1]", "-o", &c708]);
            let svc = std::fs::read_dir(&dir.0)
                .unwrap()
                .filter_map(|e| e.ok())
                .find(|e| e.file_name().to_string_lossy().contains("svc01"))
                .map(|e| e.path())
                .unwrap();
            // written in the service's charset as given, or raw when the conversion fails:
            // read as Latin-1, G2 codes it writes raw (control characters) dropped
            let bytes = std::fs::read(svc).unwrap();
            let text: String = match String::from_utf8(bytes.clone()) {
                Ok(t) => t,
                Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
            };
            let text: String = text
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .collect();
            let got = decoded(&text);
            // CCExtractor 0.96 drops G2's curly quotes and writes the music note as "&l";
            // the em dash has no G2 code and is sent as a hyphen
            let want708: Vec<(Millis, String)> = want
                .iter()
                .map(|(t, s)| {
                    (
                        *t,
                        s.replace('—', "-")
                            .replace('♪', "&l")
                            .replace(['“', '”'], ""),
                    )
                })
                .collect();
            same(&got, &want708, 150, "CCExtractor service 1");
            ran += 1;
        }
    }
    if ran == 0 {
        eprintln!("real_decoders: neither ffmpeg nor CCExtractor found; nothing checked");
    }
}
