//! Cues through the captioner, read back by our own CEA-608 and CEA-708 decoders, picture by
//! picture, with every picture's `cc_data()` checked as the encoder checks it.

use tb_captions::ccdata::{self, Construct};
use tb_captions::cea608::decode::Decoder as Dec608;
use tb_captions::cea608::{Channel, Char608, Pair};
use tb_captions::cea708::decode::Decoder as Dec708;
use tb_captions::cea708::{Char708, ServiceNumber};
use tb_captions::schedule::{LatePolicy, Presentation, RollUpRows, ServiceId};
use tb_captions::{Captioner, CaptionerCfg, Cue, FieldRate, Line, Millis, SpanStyle, Track};

/// How the output shows pictures.
#[derive(Clone, Copy, Debug)]
enum Cadence {
    /// 29.97 Hz interlaced, top field first: two fields per picture.
    Interlaced,
    /// Film on 29.97 Hz interlaced: three fields, then two, the first field alternating.
    Film,
    /// 59.94 Hz progressive: one field period per picture.
    P60,
    /// 29.97 Hz progressive: two field periods per picture.
    P30,
}

/// One change of the visible text: field period, text.
type Events = Vec<(u64, String)>;

struct Run {
    e608: Events,
    e708: Events,
    e608_cc3: Events,
    e708_s2: Events,
    /// Field 1 pairs, one per slot.
    field1: Vec<[u8; 2]>,
    field2: Vec<[u8; 2]>,
    pictures: Vec<(u8, Option<bool>, Vec<u8>)>,
}

fn run(c: &mut Captioner, cadence: Cadence, field_periods: u64) -> Run {
    let mut r = Run {
        e608: Vec::new(),
        e708: Vec::new(),
        e608_cc3: Vec::new(),
        e708_s2: Vec::new(),
        field1: Vec::new(),
        field2: Vec::new(),
        pictures: Vec::new(),
    };
    let mut d1 = Dec608::new(Channel::Cc1);
    let mut d3 = Dec608::new(Channel::Cc3);
    let mut s1 = Dec708::new(ServiceNumber::PRIMARY);
    let mut s2 = Dec708::new(ServiceNumber::SECONDARY);
    let mut p = 0u64;
    let mut n = 0u64;
    let mut top = true;
    let mut out = Vec::new();
    while p < field_periods {
        let (fields, tf, code, per_frame) = match cadence {
            Cadence::Interlaced => (2u8, Some(true), 4u8, 2u8),
            Cadence::Film => {
                let f = if n.is_multiple_of(2) { 3 } else { 2 };
                (f, Some(top), 4, 2)
            }
            Cadence::P60 => (1, None, 7, 1),
            Cadence::P30 => (2, None, 4, 1),
        };
        c.picture(fields, tf, &mut out);
        // the encoder's check of a submitted frame
        let units = if per_frame == 1 && code == 4 {
            1
        } else {
            fields
        };
        tb_m2v_syntax::captions::validate_shown(&out, code, units, per_frame)
            .unwrap_or_else(|e| panic!("picture {n} at field period {p}: {e}"));
        let cons: Vec<Construct> = ccdata::constructs(&out).collect();
        assert_eq!(cons.len(), 10 * usize::from(fields));
        for (k, x) in cons.iter().enumerate() {
            if k < usize::from(fields) {
                let f = ccdata::field_of(p + k as u64, k as u64, tf);
                assert_eq!(usize::from(x.ty), f.index(), "608 construct order");
                if x.ty == 0 {
                    r.field1.push(x.data);
                    if x.valid && d1.feed(x.data) {
                        r.e608.push((p + k as u64, d1.text()));
                    }
                } else {
                    r.field2.push(x.data);
                    if x.valid && d3.feed(x.data) {
                        r.e608_cc3.push((p + k as u64, d3.text()));
                    }
                }
            } else {
                assert!(x.ty >= 2);
                if s1.feed(x.valid, x.ty, x.data) {
                    r.e708.push((p, s1.svc.text()));
                }
                if s2.feed(x.valid, x.ty, x.data) {
                    r.e708_s2.push((p, s2.svc.text()));
                }
            }
        }
        assert_eq!(d1.parity_errors + d3.parity_errors, 0);
        assert_eq!(s1.reassembler().seq_breaks, 0, "sequence numbers in order");
        assert_eq!(s1.reassembler().shortened, 0, "no packet is cut");
        assert_eq!(s1.reassembler().orphans, 0);
        assert_eq!(s1.svc.cut + s1.svc.unknown, 0);
        r.pictures.push((fields, tf, out.clone()));
        if fields == 3 {
            top = !top;
        }
        p += u64::from(fields);
        n += 1;
    }
    r
}

fn fp(ms: i64) -> u64 {
    Millis(ms).to_field_periods(FieldRate::NTSC) as u64
}

/// The visible text over time matches the cues: each change happens within `tol` field
/// periods of the cue time.
fn check(events: &Events, want: &[(i64, &str)], tol: u64, what: &str) {
    let got: Vec<(u64, &str)> = events.iter().map(|(p, t)| (*p, t.as_str())).collect();
    assert_eq!(got.len(), want.len(), "{what}: {got:?}");
    for ((p, t), (ms, w)) in got.iter().zip(want) {
        assert_eq!(t, w, "{what}");
        let at = fp(*ms);
        assert!(
            p.abs_diff(at) <= tol,
            "{what}: {w:?} at field period {p}, wanted {at}"
        );
    }
}

fn cues() -> Vec<Cue> {
    let mut it = Line::default();
    it.push("Hello, ", SpanStyle::default());
    it.push(
        "world",
        SpanStyle {
            italic: true,
            underline: false,
        },
    );
    it.push("!", SpanStyle::default());
    vec![
        Cue {
            start: Millis(1000),
            end: Millis(3000),
            lines: vec![it, Line::plain("second line")],
            style: Default::default(),
        },
        // contiguous: replaces at once
        Cue::plain(Millis(3000), Millis(4500), &["Ünïcödé ♪ “quotes” ½ ¿Qué?"]),
        // after a gap
        Cue::plain(Millis(6000), Millis(7000), &["Last."]),
    ]
}

fn want() -> Vec<(i64, &'static str)> {
    vec![
        (1000, "Hello, world!\nsecond line"),
        (3000, "Ünïcödé ♪ “quotes” ½ ¿Qué?"),
        (4500, ""),
        (6000, "Last."),
        (7000, ""),
    ]
}

#[test]
fn pop_on_in_every_cadence() {
    for cadence in [
        Cadence::Interlaced,
        Cadence::Film,
        Cadence::P60,
        Cadence::P30,
    ] {
        let mut c =
            Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
        let rep = c.add_cues(0, &cues());
        assert!(rep.late.is_empty(), "{rep:?}");
        let r = run(&mut c, cadence, fp(8000));
        // CEA-608: within one field period of the cue's time (slot = frame), plus the
        // film picture boundary
        check(&r.e608, &want(), 2, &format!("608 {cadence:?}"));
        // CEA-708: the packet completes inside the picture of its field period
        check(&r.e708, &want(), 3, &format!("708 {cadence:?}"));
        assert!(c.is_idle());
        // the 608 text keeps its italics
        let mut d = Dec608::new(Channel::Cc1);
        let mut seen = false;
        for (k, pair) in r.field1.iter().enumerate() {
            d.feed(*pair);
            if k as u64 * 2 > fp(2000) && !seen {
                let s = d.screen();
                let row = &s[0];
                let txt: String = row.cells.iter().map(|c| c.ch).collect();
                let w = txt.find("world").unwrap();
                assert!(row.cells[w].style.italic && !row.cells[0].style.italic);
                seen = true;
            }
        }
    }
}

/// D.2: every control code of field 1 is sent twice in consecutive frames, and nothing else
/// repeats a control code without a null pair between.
#[test]
fn control_codes_are_doubled() {
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    c.add_cues(0, &cues());
    let r = run(&mut c, Cadence::Interlaced, fp(8000));
    let f = &r.field1;
    let mut codes = 0;
    let mut k = 0;
    while k < f.len() {
        let p = Pair::check(f[k]).unwrap();
        if p.is_control() {
            assert_eq!(f.get(k + 1), Some(&f[k]), "slot {k}: the copy follows");
            assert_ne!(f.get(k + 2), Some(&f[k]), "slot {k}: not a third time");
            codes += 1;
            k += 2;
        } else {
            k += 1;
        }
    }
    assert!(codes > 20);
}

/// The 9600 bit/s budget: per field period one CEA-608 pair and nine DTVCC pairs, never more.
#[test]
fn byte_budget() {
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    c.add_cues(0, &cues());
    let r = run(&mut c, Cadence::Film, fp(8000));
    let mut fields = 0usize;
    let mut dtvcc = 0usize;
    for (f, _, cc) in &r.pictures {
        fields += usize::from(*f);
        let cons: Vec<Construct> = ccdata::constructs(cc).collect();
        assert_eq!(cons.len(), 10 * usize::from(*f));
        dtvcc += cons.iter().filter(|x| x.valid && x.ty >= 2).count();
    }
    assert!(dtvcc <= 9 * fields);
    assert_eq!(r.field1.len() + r.field2.len(), fields);
}

#[test]
fn every_character_round_trips() {
    // all of CEA-608's characters, in cues of three rows
    let chars: Vec<char> = Char608::all()
        .map(|c| c.to_char())
        .filter(|&c| c != ' ' && c != '\'')
        .collect();
    let mut cues = Vec::new();
    let mut want: Vec<(i64, String)> = Vec::new();
    for (i, chunk) in chars.chunks(60).enumerate() {
        // a row of extended characters takes about 2.5 frames a character in CEA-608
        let t = 8000 + 8000 * i as i64;
        let lines: Vec<String> = chunk.chunks(30).map(|l| l.iter().collect()).collect();
        let lr: Vec<&str> = lines.iter().map(String::as_str).collect();
        cues.push(Cue::plain(Millis(t), Millis(t + 3000), &lr));
        want.push((t, lines.join("\n")));
        want.push((t + 3000, String::new()));
    }
    let end = 8000 + 8000 * cues.len() as i64;
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    let rep = c.add_cues(0, &cues);
    assert!(rep.late.is_empty(), "{:?}", rep.late);
    assert_eq!((rep.layout.substituted, rep.layout.unknown), (0, 0));
    let r = run(&mut c, Cadence::Interlaced, fp(end));
    let w: Vec<(i64, &str)> = want.iter().map(|(t, s)| (*t, s.as_str())).collect();
    check(&r.e608, &w, 2, "608");
    // CEA-708 has every one but the em dash and the solid block, shown as a hyphen and G2's
    // full block
    let w708: Vec<(i64, String)> = want
        .iter()
        .map(|(t, s)| (*t, s.replace('—', "-").replace('■', "█")))
        .collect();
    let w: Vec<(i64, &str)> = w708.iter().map(|(t, s)| (*t, s.as_str())).collect();
    check(&r.e708, &w, 3, "708");
    // and CEA-708's own repertoire (G0, G1, G2)
    let chars: Vec<char> = Char708::all().filter(|&c| c != ' ').collect();
    let mut cues = Vec::new();
    let mut want: Vec<(i64, String)> = Vec::new();
    for (i, chunk) in chars.chunks(60).enumerate() {
        let t = 1000 + 5000 * i as i64;
        let lines: Vec<String> = chunk.chunks(30).map(|l| l.iter().collect()).collect();
        let lr: Vec<&str> = lines.iter().map(String::as_str).collect();
        cues.push(Cue::plain(Millis(t), Millis(t + 4000), &lr));
        want.push((t, lines.join("\n")));
        want.push((t + 4000, String::new()));
    }
    let end = 1000 + 5000 * cues.len() as i64;
    let track = Track {
        cea608: None,
        ..Track::primary(Presentation::PopOn)
    };
    let mut c = Captioner::new(CaptionerCfg::new(vec![track])).unwrap();
    let rep = c.add_cues(0, &cues);
    assert!(rep.late.is_empty(), "{:?}", rep.late);
    let r = run(&mut c, Cadence::Interlaced, fp(end));
    // the rows break where CEA-608's would (the layout is shared, and some of these take two
    // cells there, `Þ` as `Th`): compare the characters
    let flat = |e: &Events| -> Vec<(u64, String)> {
        e.iter().map(|(p, t)| (*p, t.replace('\n', ""))).collect()
    };
    let wf: Vec<(i64, String)> = want
        .iter()
        .map(|(t, s)| (*t, s.replace('\n', "")))
        .collect();
    let w: Vec<(i64, &str)> = wf.iter().map(|(t, s)| (*t, s.as_str())).collect();
    check(&flat(&r.e708), &w, 3, "708 repertoire");
    assert!(r.e608.is_empty());
}

#[test]
fn too_much_text_for_the_gap_is_reported() {
    let rows = |c: char| -> Vec<String> {
        (0..4)
            .map(|i| format!("{c}{i} a fairly long caption line"))
            .collect()
    };
    let (a, b, c3) = (rows('a'), rows('b'), rows('c'));
    fn lines(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    // a has five seconds to load; b 100 ms after a, c 100 ms after b
    let cues = vec![
        Cue::plain(Millis(5000), Millis(5100), &lines(&a)),
        Cue::plain(Millis(5100), Millis(5200), &lines(&b)),
        Cue::plain(Millis(5200), Millis(9000), &lines(&c3)),
    ];
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    let rep = c.add_cues(0, &cues);
    let late608: Vec<_> = rep
        .late
        .iter()
        .filter(|l| matches!(l.service, ServiceId::Cea608(_)))
        .collect();
    assert_eq!(late608.len(), 2, "{rep:?}");
    assert_eq!(late608[0].start, Millis(5100));
    assert_eq!(
        late608[0].shown, None,
        "100 ms cannot hold it, nor can it be delayed"
    );
    assert_eq!(late608[1].start, Millis(5200));
    let shown = late608[1].shown.unwrap();
    assert!(shown > Millis(5200) && shown < Millis(9000), "{shown}");
    // DTVCC has the bandwidth: nothing late there beyond a field period
    assert!(
        rep.late
            .iter()
            .filter(|l| matches!(l.service, ServiceId::Cea708(_)))
            .all(|l| l.shown.is_some_and(|t| t.0 - l.start.0 < 1000)),
        "{rep:?}"
    );
    let r = run(&mut c, Cadence::Interlaced, fp(10_000));
    let texts: Vec<&str> = r.e608.iter().map(|e| e.1.as_str()).collect();
    // a at its time and cleared at its end (b, which was to replace it, is skipped); c late;
    // cleared at its end
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert!(texts[0].starts_with("a0") && texts[1].is_empty());
    assert!(texts[2].starts_with("c0") && texts[3].is_empty());
    assert!(r.e608[1].0.abs_diff(fp(5100)) <= 2);
    assert!(r.e608[2].0.abs_diff(fp(shown.0)) <= 2);
    assert!(r.e608[3].0.abs_diff(fp(9000)) <= 2);
    // Skip: the late screens are not shown
    let mut cfg = CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)]);
    cfg.late = LatePolicy::Skip;
    let mut c = Captioner::new(cfg).unwrap();
    let rep = c.add_cues(0, &cues);
    let late608: Vec<_> = rep
        .late
        .iter()
        .filter(|l| matches!(l.service, ServiceId::Cea608(_)))
        .collect();
    assert_eq!(late608.len(), 2);
    assert!(late608.iter().all(|l| l.shown.is_none()));
    let r = run(&mut c, Cadence::Interlaced, fp(10_000));
    let texts: Vec<&str> = r.e608.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(texts.len(), 2, "a, then cleared at its end: {texts:?}");
    assert!(r.e608[1].0.abs_diff(fp(5100)) <= 2);
}

#[test]
fn roll_up() {
    let mut c = Captioner::new(CaptionerCfg::new(vec![Track::primary(
        Presentation::RollUp(RollUpRows::Two),
    )]))
    .unwrap();
    let cues = vec![
        Cue::plain(Millis(1000), Millis(3000), &["one"]),
        Cue::plain(Millis(3000), Millis(5000), &["two"]),
        Cue::plain(Millis(5000), Millis(6000), &["three"]),
    ];
    let rep = c.add_cues(0, &cues);
    assert!(rep.late.is_empty());
    let r = run(&mut c, Cadence::Interlaced, fp(7000));
    // text paints in as it arrives; at the end of the cues the screen is cleared
    let final608: Vec<&str> = r.e608.iter().map(|e| e.1.as_str()).collect();
    assert!(final608.contains(&"one\ntwo"), "{final608:?}");
    assert!(final608.contains(&"two\nthree"), "{final608:?}");
    assert_eq!(final608.last(), Some(&""));
    let final708: Vec<&str> = r.e708.iter().map(|e| e.1.as_str()).collect();
    assert!(final708.contains(&"one\ntwo"), "{final708:?}");
    assert!(final708.contains(&"two\nthree"), "{final708:?}");
    assert_eq!(final708.last(), Some(&""));
    // the first row starts on time
    let first = r.e608.iter().find(|e| !e.1.is_empty()).unwrap();
    assert!(first.0.abs_diff(fp(1000)) <= 3, "{first:?}");
}

#[test]
fn two_languages() {
    let mut c = Captioner::new(CaptionerCfg::new(vec![
        Track::primary(Presentation::PopOn),
        Track::secondary(Presentation::PopOn),
    ]))
    .unwrap();
    c.add_cues(0, &[Cue::plain(Millis(1000), Millis(2000), &["Hello"])]);
    c.add_cues(1, &[Cue::plain(Millis(1000), Millis(2000), &["Hola"])]);
    let r = run(&mut c, Cadence::Interlaced, fp(3000));
    check(&r.e608, &[(1000, "Hello"), (2000, "")], 2, "CC1");
    check(&r.e608_cc3, &[(1000, "Hola"), (2000, "")], 2, "CC3");
    check(&r.e708, &[(1000, "Hello"), (2000, "")], 3, "service 1");
    check(&r.e708_s2, &[(1000, "Hola"), (2000, "")], 3, "service 2");
    // CC3's control codes go once (D.2, B.14)
    let codes: Vec<&[u8; 2]> = r
        .field2
        .iter()
        .filter(|p| Pair::check(**p).is_ok_and(|p| p.is_control()))
        .collect();
    assert!(codes.windows(2).all(|w| w[0] != w[1]));
    // the same field twice is refused
    let mut t = Track::secondary(Presentation::PopOn);
    t.cea608 = Track::primary(Presentation::PopOn).cea608;
    t.cea708 = None;
    assert!(
        Captioner::new(CaptionerCfg::new(vec![
            Track::primary(Presentation::PopOn),
            t
        ]))
        .is_err()
    );
}

/// Our output has the form of a source's captions carried through the timeline: the carrier
/// passes it through unchanged.
#[test]
fn the_timeline_carrier_passes_our_output_through() {
    use tb_timeline::captions::{CaptionCarrier, CaptionCfg, CcRate};
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    c.add_cues(0, &cues());
    let r = run(&mut c, Cadence::Interlaced, fp(8000));
    let mut carrier = CaptionCarrier::new(CaptionCfg::top_first(CcRate::F2997));
    let mut out = Vec::new();
    let (mut sent, mut got): (Vec<[u8; 3]>, Vec<[u8; 3]>) = (Vec::new(), Vec::new());
    let by_type = |v: &[[u8; 3]], dtvcc: bool| -> Vec<[u8; 3]> {
        v.iter()
            .copied()
            .filter(|t| (t[0] & 3 >= 2) == dtvcc)
            .collect()
    };
    let valid = |cc: &[u8]| -> Vec<[u8; 3]> {
        cc.as_chunks::<3>()
            .0
            .iter()
            .copied()
            .filter(|t| {
                t[0] & 4 != 0
                    && (t[0] & 3 >= 2 || Pair::check([t[1], t[2]]).is_ok_and(|p| !p.is_null()))
            })
            .collect()
    };
    for (k, (_, _, cc)) in r.pictures.iter().enumerate() {
        carrier.push(k as i64, cc);
        carrier.take(k as i64, &mut out);
        // a picture of the carrier is legal, and has our 608 pairs unchanged: the carrier
        // holds a DTVCC packet back until all of it is in, so packets that run into the next
        // picture come a picture later
        tb_m2v_syntax::captions::validate_shown(&out, 4, 2, 2).unwrap();
        assert_eq!(out[..6], cc[..6], "picture {k}");
        sent.extend(valid(cc));
        got.extend(valid(&out));
    }
    for _ in 0..4 {
        carrier.take(i64::MAX, &mut out);
        got.extend(valid(&out));
    }
    assert_eq!(by_type(&got, false), by_type(&sent, false));
    assert_eq!(by_type(&got, true), by_type(&sent, true));
    assert_eq!(carrier.stats.orphans, 0);
}

/// Cues added later (live) and cues already in the past.
#[test]
fn live_cues() {
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    let mut out = Vec::new();
    // two seconds pass
    for _ in 0..60 {
        c.picture(2, Some(true), &mut out);
    }
    assert!(c.position() >= Millis(2000));
    // a cue due a second ago is shown at once, late
    let rep = c.add_cues(0, &[Cue::plain(Millis(1000), Millis(4000), &["late"])]);
    assert_eq!(rep.late.len(), 2, "{rep:?}");
    assert!(rep.late.iter().all(|l| l.shown.is_some()));
    let r = run(&mut c, Cadence::Interlaced, fp(3000));
    assert_eq!(r.e608.first().map(|e| e.1.as_str()), Some("late"));
}

/// Arbitrary bytes into the decoders, the parser and the merger: no panic.
#[test]
fn arbitrary_input_never_panics() {
    use tb_captions::ccdata::{MergeCfg, Merger, Parser};
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut d = [
        Dec608::new(Channel::Cc1),
        Dec608::new(Channel::Cc2),
        Dec608::new(Channel::Cc3),
        Dec608::new(Channel::Cc4),
    ];
    let mut s = Dec708::new(ServiceNumber::PRIMARY);
    let mut parser = Parser::new();
    let mut m = Merger::new(MergeCfg::default());
    let mut c =
        Captioner::new(CaptionerCfg::new(vec![Track::primary(Presentation::PopOn)])).unwrap();
    let (mut ours, mut out) = (Vec::new(), Vec::new());
    for _ in 0..20_000 {
        let r = next();
        // bias toward control codes and DTVCC commands
        let a = [
            (r & 0xFF) as u8,
            0x10 | (r & 0x0F) as u8,
            0x80 | (r & 0x1F) as u8,
        ][(r >> 60) as usize % 3];
        let b = (r >> 8) as u8;
        for dd in &mut d {
            dd.feed([a, b]);
            let _ = dd.screen();
        }
        s.feed(r & (1 << 20) != 0, ((r >> 21) & 3) as u8, [a, b]);
        let _ = s.svc.screen();
        let fields = 1 + (r >> 30) as u8 % 3;
        let src: Vec<u8> = (0..30 * usize::from(fields))
            .map(|_| next() as u8)
            .collect();
        let _ = parser.picture(&src);
        c.picture(fields, Some(r & 2 != 0), &mut ours);
        m.merge(&src, &ours, fields, &mut out).unwrap();
        assert_eq!(out.len(), ours.len());
    }
}
