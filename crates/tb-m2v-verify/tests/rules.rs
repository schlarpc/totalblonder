//! One hand-built stream per rule: a valid stream in which one syntax element is altered so
//! that it violates the rule. The verifier has to report exactly that rule (and, where the
//! standards make two rules overlap, exactly the listed ones). `coverage` lists the rules of
//! the catalog without a detecting case.

mod common;

use common::{B, Bld, I, P, Pic, Seq, base_stream, expect, find_unit, set_bits};
use tb_m2v_verify::rules::{ALL_RULES, Check};
use tb_m2v_verify::{Delivery, Options, Profile, Rule, verify};

struct Case {
    name: &'static str,
    /// The rules the verifier has to report: exactly these.
    rules: Vec<Rule>,
    opts: Options,
    data: Vec<u8>,
}

fn h262() -> Options {
    Options::default()
}

fn prof(p: Profile) -> Options {
    Options {
        profile: p,
        ..Options::default()
    }
}

/// Sequence header, display extension, GOP header, then `f`, then sequence end.
fn stream(seq: Seq, f: impl FnOnce(&mut Bld)) -> Vec<u8> {
    let mut b = Bld::new(seq);
    b.seq();
    b.sde(0, Some((1, 1, 1)), true);
    b.gop(0, true);
    f(&mut b);
    b.seq_end();
    b.finish()
}

/// The base stream with changed sequence parameters and pictures.
fn ipbb(seq: Seq, mut pic: impl FnMut(usize, &mut Pic)) -> Vec<u8> {
    stream(seq, |b| {
        for (i, (t, tr)) in [(I, 0), (P, 3), (B, 1), (B, 2)].into_iter().enumerate() {
            let mut p = Pic::new(t, tr);
            pic(i, &mut p);
            b.picture(&p);
        }
    })
}

/// I, P, B, B of an interlaced sequence as film: `shown` gives (fields, top field first) of
/// each frame in display order (I, B, B, P); frames of three fields are progressive.
fn film(shown: &[(u8, bool); 4], mut pic: impl FnMut(usize, &mut Pic)) -> Vec<u8> {
    stream(Seq::interlaced(), |b| {
        for (i, (t, tr)) in [(I, 0u16), (P, 3), (B, 1), (B, 2)].into_iter().enumerate() {
            let (fields, top) = shown[usize::from(tr)];
            let mut p = Pic::interlaced(t, tr);
            p.tff = top;
            if fields == 3 {
                (p.prog, p.c420, p.rff, p.fpfd) = (true, true, true, true);
            }
            pic(i, &mut p);
            b.picture(&p);
        }
    })
}

fn ipbb_interlaced(mut pic: impl FnMut(usize, &mut Pic)) -> Vec<u8> {
    stream(Seq::interlaced(), |b| {
        for (i, (t, tr)) in [(I, 0), (P, 3), (B, 1), (B, 2)].into_iter().enumerate() {
            let mut p = Pic::interlaced(t, tr);
            pic(i, &mut p);
            b.picture(&p);
        }
    })
}

/// An I-picture whose slices `f` writes, in a valid sequence.
fn i_custom(seq: Seq, pic: Pic, f: impl FnOnce(&mut Bld)) -> Vec<u8> {
    stream(seq, |b| {
        b.pic(&pic);
        f(b);
    })
}

/// I then a P-picture whose slices `f` writes.
fn p_custom(seq: Seq, pic: Pic, f: impl FnOnce(&mut Bld)) -> Vec<u8> {
    let interlaced = !seq.progressive;
    stream(seq, |b| {
        b.picture(&if interlaced {
            Pic::interlaced(I, 0)
        } else {
            Pic::new(I, 0)
        });
        b.pic(&pic);
        f(b);
    })
}

/// I, P then a B-picture whose slices `f` writes.
fn b_custom(seq: Seq, pic: Pic, f: impl FnOnce(&mut Bld)) -> Vec<u8> {
    stream(seq, |b| {
        b.picture(&Pic::new(I, 0));
        b.picture(&Pic::new(P, 2));
        b.pic(&pic);
        f(b);
    })
}

/// Rows `rows` as default intra slices.
fn intra_rows(b: &mut Bld, rows: std::ops::Range<u32>) {
    let mw = b.seq.mb_w();
    for r in rows {
        b.slice(r, 4);
        for _ in 0..mw {
            b.intra_mb(1);
        }
    }
}

/// An intra macroblock with 63 escape-coded coefficients in every block: 9072 bits and more.
fn big_intra_mb(b: &mut Bld) {
    b.inc(1);
    b.mb_type(false, false, false, false, true);
    for i in 0..6 {
        b.dc(i < 4, 0);
        for _ in 0..63 {
            b.escape(0, 41);
        }
        b.eob(false);
    }
}

/// A field pair: I top field, P bottom field predicted from the top field, as a valid frame.
fn field_pair(b: &mut Bld, tr: u16, first: impl FnOnce(&mut Pic), second: impl FnOnce(&mut Pic)) {
    let mut p = Pic::field(I, tr, 1);
    first(&mut p);
    b.picture(&p);
    let mut p = Pic::field(P, tr, 2);
    second(&mut p);
    b.pic(&p);
    b.fsel = Some(false); // predict from the top field
    let (mw, rows) = (b.seq.mb_w(), b.seq.mb_h() / 2);
    for r in 0..rows {
        b.slice(r, 4);
        for _ in 0..mw {
            b.fwd_mb(1, 0, 0);
        }
    }
    b.fsel = None;
}

/// A valid caption user data structure for a frame of two fields with top_field_first.
fn cc(count: u8, lead: &[u8]) -> Vec<u8> {
    let mut d = vec![0x47, 0x41, 0x39, 0x34, 0x03, 0xC0 | count, 0xFF];
    for i in 0..usize::from(count) {
        match lead.get(i) {
            Some(h) => d.extend_from_slice(&[*h, 0x80, 0x80]),
            None => d.extend_from_slice(&[0xFA, 0x00, 0x00]),
        }
    }
    d.push(0xFF);
    d
}

/// The sequence display extension to write: video_format and the color description.
type SdeArg = Option<(u32, Option<(u32, u32, u32)>)>;

/// A broadcast-legal stream: 704x480 interlaced 29.97 Hz, 4:3, MP@ML, with caption data in
/// every picture. `ud` gives the user data of picture `i`; `pic` may change the picture.
fn bc(
    mut seq_f: impl FnMut(&mut Seq),
    sde: SdeArg,
    mut ud: impl FnMut(usize) -> Vec<Vec<u8>>,
    mut pic: impl FnMut(usize, &mut Pic),
) -> Vec<u8> {
    let mut seq = Seq {
        w: 704,
        h: 480,
        aspect: 2,
        frc: 4,
        progressive: false,
        bit_rate: 10000,
        vbv: 112,
        ..Seq::default()
    };
    seq_f(&mut seq);
    let interlaced = !seq.progressive;
    let mut b = Bld::new(seq);
    b.seq();
    if let Some((vf, color)) = sde {
        b.sde(vf, color, true);
    }
    b.gop(0, true);
    for (i, (t, tr)) in [(I, 0), (P, 3), (B, 1), (B, 2)].into_iter().enumerate() {
        let mut p = if interlaced {
            Pic::interlaced(t, tr)
        } else {
            Pic::new(t, tr)
        };
        pic(i, &mut p);
        b.pic(&p);
        for u in ud(i) {
            b.user_data(&u);
        }
        b.picture_data();
    }
    b.seq_end();
    b.finish()
}

fn std_cc(_: usize) -> Vec<Vec<u8>> {
    vec![cc(20, &[0xFD, 0xFC])]
}

const SDE709: SdeArg = Some((0, Some((1, 1, 1))));

fn bc_default() -> Vec<u8> {
    bc(|_| {}, SDE709, std_cc, |_, _| {})
}

/// The broadcast stream at 1280x720 progressive 59.94 Hz, MP@HL.
fn bc_720p(seq_f: impl Fn(&mut Seq), ud: impl FnMut(usize) -> Vec<Vec<u8>>) -> Vec<u8> {
    bc(
        |s| {
            s.w = 1280;
            s.h = 720;
            s.aspect = 3;
            s.frc = 7;
            s.progressive = true;
            s.pli = 0x44;
            s.bit_rate = 37500;
            s.vbv = 400;
            seq_f(s);
        },
        SDE709,
        ud,
        |_, _| {},
    )
}

/// Sets vbv_delay of picture `n` (the 16 bits after temporal_reference and the type).
fn set_vbv_delay(data: &mut [u8], n: usize, v: u32) {
    let Some(u) = find_unit(data, 0x00, None, n) else {
        panic!("no picture {n}");
    };
    set_bits(data, u, 13, 16, v);
}

#[allow(clippy::too_many_lines)]
fn cases() -> Vec<Case> {
    let mut v: Vec<Case> = Vec::new();
    let mut add = |name: &'static str, rules: &[Rule], opts: Options, data: Vec<u8>| {
        v.push(Case {
            name,
            rules: rules.to_vec(),
            opts,
            data,
        });
    };
    use Rule as R;

    // ------------------------------------------------------------ stream
    add(
        "two escapes with level 0 make 23 zero bits",
        &[R::BlkEscZero, R::ScEmulation],
        h262(),
        i_custom(Seq::default(), Pic::new(I, 0), |b| {
            b.slice(0, 4);
            b.inc(1);
            b.mb_type(false, false, false, false, true);
            b.dc(true, 0);
            b.escape(0, 0);
            b.escape(0, 0);
            b.eob(false);
            for i in 1..6 {
                b.dc(i < 4, 0);
                b.eob(false);
            }
            for _ in 1..4 {
                b.intra_mb(1);
            }
            intra_rows(b, 1..3);
        }),
    );
    add("reserved start code B0", &[R::ScReserved], h262(), {
        let mut b = Bld::new(Seq::default());
        b.seq();
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.w.start(0xB0);
        b.seq_end();
        b.finish()
    });
    add("system start code BA", &[R::ScSystem], h262(), {
        let mut b = Bld::new(Seq::default());
        b.seq();
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.w.start(0xBA);
        b.seq_end();
        b.finish()
    });
    add("sequence_error_code", &[R::ScSequenceError], h262(), {
        let mut b = Bld::new(Seq::default());
        b.seq();
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.w.start(0xB4);
        b.seq_end();
        b.finish()
    });
    add(
        "garbage after the GOP header",
        &[R::StuffNonZero],
        h262(),
        {
            let mut b = Bld::new(Seq::default());
            b.seq();
            b.gop(0, true);
            b.w.raw(&[0x80]);
            b.picture(&Pic::new(I, 0));
            b.seq_end();
            b.finish()
        },
    );
    add(
        "non-zero bits after the last macroblock of a slice",
        &[R::StuffNonZero, R::ScEmulation],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                intra_rows(b, 0..2);
                b.slice(2, 4);
                for _ in 0..4 {
                    b.intra_mb(1);
                }
                b.w.put(23, 0);
                b.w.align();
                b.w.raw(&[0x00, 0xFF]);
            })
        },
    );
    add(
        "data before the first start code",
        &[R::StreamStart],
        h262(),
        {
            let mut d = vec![0xFF, 0x00];
            d.extend(base_stream());
            d
        },
    );
    add("no sequence_end_code", &[R::SeqEndMissing], h262(), {
        let mut d = base_stream();
        d.truncate(d.len() - 4);
        d
    });
    add("two sequence_end_codes", &[R::SeqEndPlacement], h262(), {
        let mut d = base_stream();
        d.extend_from_slice(&[0, 0, 1, 0xB7]);
        d
    });
    add(
        "sequence ends after one field",
        &[R::SeqEndComplete],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                b.picture(&Pic::field(I, 0, 1));
            })
        },
    );
    add("no sequence_extension", &[R::OrdSeqExtMissing], h262(), {
        let mut b = Bld::new(Seq::default());
        b.seq_header();
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.seq_end();
        b.finish()
    });
    add(
        "a stray sequence_extension",
        &[R::OrdSeqExtStray],
        h262(),
        {
            let mut b = Bld::new(Seq::default());
            b.seq();
            b.seq_ext();
            b.gop(0, true);
            b.picture(&Pic::new(I, 0));
            b.seq_end();
            b.finish()
        },
    );
    add(
        "no picture_coding_extension",
        &[R::OrdPceMissing],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.pic_header(&Pic::new(I, 0));
                b.picture_data();
            })
        },
    );
    add(
        "a stray picture_coding_extension",
        &[R::OrdPceStray],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.pic(&Pic::new(I, 0));
                b.pce();
                b.picture_data();
            })
        },
    );
    add(
        "a slice before any picture header",
        &[R::OrdUnit],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.pic = Pic::new(I, 0);
                intra_rows(b, 0..1);
                b.picture(&Pic::new(I, 0));
            })
        },
    );
    add("user data after the slices", &[R::OrdUnit], h262(), {
        stream(Seq::default(), |b| {
            b.picture(&Pic::new(I, 0));
            b.user_data(&[0x41, 0x42]);
        })
    });
    add(
        "sequence display extension after a picture header",
        &[R::OrdExtPlace],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.pic(&Pic::new(I, 0));
                b.sde(0, Some((1, 1, 1)), true);
                b.picture_data();
            })
        },
    );
    add(
        "two sequence display extensions",
        &[R::OrdExtDup],
        h262(),
        {
            let mut b = Bld::new(Seq::default());
            b.seq();
            b.sde(0, Some((1, 1, 1)), true);
            b.sde(0, Some((1, 1, 1)), true);
            b.gop(0, true);
            b.picture(&Pic::new(I, 0));
            b.seq_end();
            b.finish()
        },
    );
    add(
        "extension with reserved identifier",
        &[R::OrdExtReserved],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.pic(&Pic::new(I, 0));
                b.ext(&[0x0F, 0xFF]);
                b.picture_data();
            })
        },
    );
    add("picture without slices", &[R::OrdNoSlice], h262(), {
        stream(Seq::default(), |b| {
            b.picture(&Pic::new(I, 0));
            b.picture(&Pic::new(P, 1));
            b.pic(&Pic::new(P, 2));
        })
    });
    add(
        "two sequence headers in a row",
        &[R::OrdSeqNoPicture],
        h262(),
        {
            let mut b = Bld::new(Seq::default());
            b.seq();
            b.sde(0, Some((1, 1, 1)), true);
            b.seq();
            b.sde(0, Some((1, 1, 1)), true);
            b.gop(0, true);
            b.picture(&Pic::new(I, 0));
            b.seq_end();
            b.finish()
        },
    );
    add(
        "picture coding extension cut short",
        &[R::HdrTruncated],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.picture(&Pic::new(I, 0));
                b.pic_header(&Pic::new(P, 1));
                b.ext(&[0x82, 0x2F]);
                b.picture_data();
            })
        },
    );

    // ------------------------------------------------------------ sequence header
    add("horizontal_size_value 0", &[R::SeqHsizeZero], h262(), {
        let mut b = Bld::new(Seq {
            w: 0,
            ..Seq::default()
        });
        b.seq();
        b.seq.w = 16;
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.seq_end();
        b.finish()
    });
    add("vertical_size_value 0", &[R::SeqVsizeZero], h262(), {
        let mut b = Bld::new(Seq {
            h: 0,
            ..Seq::default()
        });
        b.seq();
        b.seq.h = 16;
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.seq_end();
        b.finish()
    });
    let seq_with = |f: &dyn Fn(&mut Seq)| {
        let mut s = Seq::default();
        f(&mut s);
        ipbb(s, |_, _| {})
    };
    add(
        "aspect_ratio_information 0",
        &[R::SeqAspect],
        h262(),
        seq_with(&|s| s.aspect = 0),
    );
    add(
        "aspect_ratio_information 9",
        &[R::SeqAspect],
        h262(),
        seq_with(&|s| s.aspect = 9),
    );
    add(
        "frame_rate_code 0",
        &[R::SeqFrameRate],
        h262(),
        seq_with(&|s| s.frc = 0),
    );
    add(
        "frame_rate_code 12",
        &[R::SeqFrameRate],
        h262(),
        seq_with(&|s| s.frc = 12),
    );
    add(
        "bit_rate 0",
        &[R::SeqBitRateZero],
        h262(),
        seq_with(&|s| s.bit_rate = 0),
    );
    add(
        "sequence header marker_bit 0",
        &[R::SeqMarker],
        h262(),
        seq_with(&|s| s.marker = false),
    );
    add(
        "constrained_parameters_flag 1",
        &[R::SeqConstrained],
        h262(),
        seq_with(&|s| s.constrained = true),
    );
    add("zero in the intra matrix", &[R::QmZero], h262(), {
        seq_with(&|s| {
            let mut m = [16u8; 64];
            m[0] = 8;
            m[10] = 0;
            s.intra_q = Some(m);
        })
    });
    add(
        "three zeros in the non-intra matrix emulate a start code prefix",
        &[R::QmZero, R::ScEmulation],
        h262(),
        seq_with(&|s| {
            let mut m = [16u8; 64];
            m[10] = 0;
            m[11] = 0;
            m[12] = 0;
            s.non_intra_q = Some(m);
        }),
    );
    add("intra matrix starts with 16", &[R::QmIntraDc], h262(), {
        seq_with(&|s| s.intra_q = Some([16u8; 64]))
    });
    let repeat = |change: &dyn Fn(&mut Bld), sde: bool, before: usize| {
        stream(Seq::default(), |b| {
            for (i, (t, tr)) in [(I, 0), (P, 3), (B, 1), (B, 2)].into_iter().enumerate() {
                if i == before {
                    let keep = b.seq.clone();
                    change(b);
                    b.seq();
                    b.seq = keep;
                    if sde {
                        b.sde(0, Some((1, 1, 1)), true);
                    }
                }
                b.picture(&Pic::new(t, tr));
            }
        })
    };
    add(
        "repeat sequence header with another aspect ratio",
        &[R::SeqRepeatDiff],
        h262(),
        repeat(&|b| b.seq.aspect = 2, true, 1),
    );
    add(
        "repeat sequence header with another bit_rate",
        &[R::SeqRepeatDiff],
        h262(),
        repeat(&|b| b.seq.bit_rate = 9000, true, 1),
    );
    add(
        "repeat sequence extension with another low_delay",
        &[R::SeqRepeatExtDiff],
        h262(),
        repeat(&|b| b.seq.low_delay = true, true, 1),
    );
    add(
        "repeat sequence header without the display extension",
        &[R::SeqRepeatSde],
        h262(),
        repeat(&|_| {}, false, 1),
    );
    add(
        "repeat sequence header before a B-picture",
        &[R::SeqFirstPicB],
        h262(),
        repeat(&|_| {}, true, 2),
    );
    add(
        "sequence header between two fields",
        &[R::SeqHdrBetweenFields],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                b.picture(&Pic::field(I, 0, 1));
                b.seq();
                b.sde(0, Some((1, 1, 1)), true);
                b.picture(&Pic::field(I, 0, 2));
            })
        },
    );

    // ------------------------------------------------------------ sequence extension
    add(
        "reserved level",
        &[R::SeqxProfile],
        h262(),
        seq_with(&|s| s.pli = 0x49),
    );
    add(
        "chroma_format 00",
        &[R::SeqxChroma],
        h262(),
        seq_with(&|s| s.chroma = 0),
    );
    add(
        "sequence extension marker_bit 0",
        &[R::SeqxMarker],
        h262(),
        seq_with(&|s| s.ext_marker = false),
    );
    add(
        "frame_rate_extension 1/1",
        &[R::SeqxFrameRateExt, R::PlFrameRateExt],
        h262(),
        seq_with(&|s| {
            s.fr_n = 1;
            s.fr_d = 1;
        }),
    );
    add("B-pictures with low_delay", &[R::SeqxLowDelayB], h262(), {
        seq_with(&|s| s.low_delay = true)
    });
    add(
        "interlaced frame in a progressive sequence",
        &[R::SeqxProgressive],
        h262(),
        {
            ipbb(Seq::default(), |i, p| {
                if i == 1 {
                    p.prog = false;
                    p.c420 = false;
                }
            })
        },
    );
    let sde_case = |vf: u32, color: Option<(u32, u32, u32)>, marker: bool| {
        let mut b = Bld::new(Seq::default());
        b.seq();
        b.sde(vf, color, marker);
        b.gop(0, true);
        b.picture(&Pic::new(I, 0));
        b.seq_end();
        b.finish()
    };
    add(
        "video_format 6",
        &[R::SdeVideoFormat],
        h262(),
        sde_case(6, Some((1, 1, 1)), true),
    );
    add(
        "colour_primaries 0",
        &[R::SdeColor],
        h262(),
        sde_case(0, Some((0, 1, 1)), true),
    );
    add(
        "matrix_coefficients 3",
        &[R::SdeColor],
        h262(),
        sde_case(0, Some((1, 1, 3)), true),
    );
    add(
        "display extension marker_bit 0",
        &[R::SdeMarker],
        h262(),
        sde_case(0, Some((1, 1, 1)), false),
    );
    add("display_horizontal_size 0", &[R::SdeDisplaySize], h262(), {
        let mut d = base_stream();
        let Some(u) = find_unit(&d, 0xB5, Some(2), 0) else {
            panic!("no display extension");
        };
        set_bits(&mut d, u, 32, 14, 0);
        d
    });

    // ------------------------------------------------------------ GOP
    let gop_case = |f: &dyn Fn(&mut Bld)| {
        let mut b = Bld::new(Seq::default());
        b.seq();
        f(&mut b);
        b.picture(&Pic::new(I, 0));
        b.seq_end();
        b.finish()
    };
    add("time_code marker_bit 0", &[R::GopMarker], h262(), {
        gop_case(&|b| b.gop_tc(false, 1, 2, false, 3, 4, true, false))
    });
    add(
        "time_code marker_bit 0 in a zero time code emulates a start code prefix",
        &[R::GopMarker, R::ScEmulation],
        h262(),
        gop_case(&|b| b.gop_tc(false, 0, 0, false, 0, 0, true, false)),
    );
    add("time_code_minutes 60", &[R::GopTimeCodeRange], h262(), {
        gop_case(&|b| b.gop_tc(false, 0, 60, true, 0, 0, true, false))
    });
    add("drop_frame_flag at 30 Hz", &[R::GopDropFrame], h262(), {
        let mut b = Bld::new(Seq {
            frc: 5,
            ..Seq::default()
        });
        b.seq();
        b.gop_tc(true, 0, 0, true, 0, 5, true, false);
        b.picture(&Pic::new(I, 0));
        b.seq_end();
        b.finish()
    });
    add("dropped time code", &[R::GopDropFrame], h262(), {
        gop_case(&|b| b.gop_tc(true, 0, 1, true, 0, 1, true, false))
    });
    add(
        "time_code_pictures 45 at 29.97 Hz",
        &[R::GopTimeCodePictures],
        h262(),
        gop_case(&|b| b.gop_tc(false, 0, 0, true, 0, 45, true, false)),
    );
    add("broken_link 1", &[R::GopBrokenLink], h262(), {
        gop_case(&|b| b.gop_tc(false, 0, 0, true, 0, 0, true, true))
    });
    add(
        "second GOP time code jumps",
        &[R::GopTimeCodeContinuity],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.picture(&Pic::new(I, 0));
                b.picture(&Pic::new(P, 1));
                b.gop(7, true);
                b.picture(&Pic::new(I, 0));
            })
        },
    );
    add(
        "GOP time code after three-field frames is neither the frames nor the display time",
        &[R::GopTimeCodeContinuity],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                // four frames shown for 3, 2, 3, 2 fields: 4 frames, 5 frame periods
                for (tr, top) in [(0u16, true), (1, false)] {
                    let mut p = Pic::interlaced(if tr == 0 { I } else { P }, tr);
                    (p.prog, p.c420, p.rff, p.fpfd, p.tff) = (true, true, tr == 0, true, top);
                    b.picture(&p);
                }
                for (tr, top) in [(2u16, false), (3, true)] {
                    let mut p = Pic::interlaced(P, tr);
                    (p.prog, p.c420, p.rff, p.fpfd, p.tff) = (true, true, tr == 2, true, top);
                    b.picture(&p);
                }
                b.gop(3, true);
                b.picture(&Pic::interlaced(I, 0));
            })
        },
    );
    add("P-picture after a GOP header", &[R::GopFirstI], h262(), {
        stream(Seq::default(), |b| {
            b.picture(&Pic::new(I, 0));
            b.gop(1, true);
            b.picture(&Pic::new(P, 0));
        })
    });
    let leading_b = |closed: bool| {
        let mut b = Bld::new(Seq::default());
        b.seq();
        b.gop(0, closed);
        b.picture(&Pic::new(I, 1));
        b.pic(&Pic::new(B, 0));
        for r in 0..3 {
            b.slice(r, 4);
            for _ in 0..4 {
                b.fwd_mb(1, 0, 0);
            }
        }
        b.seq_end();
        b.finish()
    };
    add(
        "forward prediction in a closed GOP",
        &[R::GopClosedPred],
        h262(),
        leading_b(true),
    );
    add(
        "forward prediction at the start of the sequence",
        &[R::GopOpenStart],
        h262(),
        leading_b(false),
    );

    // ------------------------------------------------------------ picture header
    add("picture_coding_type 4", &[R::PicType], h262(), {
        stream(Seq::default(), |b| {
            b.picture(&Pic::new(I, 0));
            b.picture(&Pic::new(P, 1));
            let mut p = Pic::new(I, 2);
            p.ptype = 4;
            b.pic(&p);
            b.pic.ptype = I;
            b.picture_data();
        })
    });
    add(
        "temporal_reference 5 where 3 is due",
        &[R::PicTemporalRef],
        h262(),
        {
            ipbb(Seq::default(), |i, p| {
                if i == 1 {
                    p.tr = 5;
                }
            })
        },
    );
    add(
        "temporal_reference 1 after a GOP header",
        &[R::PicTemporalRef],
        h262(),
        {
            stream(Seq::default(), |b| {
                b.picture(&Pic::new(I, 1));
                b.picture(&Pic::new(P, 2));
            })
        },
    );
    add(
        "fields with different temporal_reference",
        &[R::PicTemporalRefFields],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                field_pair(b, 0, |_| {}, |p| p.tr = 1);
            })
        },
    );
    add(
        "vbv_delay 0xFFFF and other values",
        &[R::PicVbvDelayMix],
        h262(),
        {
            ipbb(Seq::default(), |i, p| {
                if i == 2 {
                    p.vbv_delay = 3000;
                }
            })
        },
    );
    add("full_pel_forward_vector 1", &[R::PicFullPel], h262(), {
        ipbb(Seq::default(), |i, p| p.full_pel[0] = i == 1)
    });
    add(
        "backward_f_code 3 in the picture header",
        &[R::PicFcode7],
        h262(),
        {
            ipbb(Seq::default(), |i, p| {
                if i == 3 {
                    p.hdr_f_code[1] = 3;
                }
            })
        },
    );
    add("extra_information_picture", &[R::PicExtraInfo], h262(), {
        ipbb(Seq::default(), |i, p| {
            if i == 0 {
                p.extra = vec![0x55];
            }
        })
    });

    // ------------------------------------------------------------ picture coding extension
    add(
        "f_code 10 in a P-picture of intra macroblocks",
        &[R::PceFcodeReserved],
        h262(),
        {
            let mut p = Pic::new(P, 1);
            p.f_code[0] = [10, 2];
            p_custom(Seq::default(), p, |b| intra_rows(b, 0..3))
        },
    );
    add(
        "f_code 0 in a P-picture of intra macroblocks",
        &[R::PceFcodeReserved],
        h262(),
        {
            let mut p = Pic::new(P, 1);
            p.f_code[0] = [2, 0];
            p_custom(Seq::default(), p, |b| intra_rows(b, 0..3))
        },
    );
    add("f_code 1 in an I-picture", &[R::PceFcodeUnused], h262(), {
        ipbb(Seq::default(), |i, p| {
            if i == 0 {
                p.f_code[0] = [1, 1];
            }
        })
    });
    add(
        "backward f_code 1 in a P-picture",
        &[R::PceFcodeUnused],
        h262(),
        {
            ipbb(Seq::default(), |i, p| {
                if i == 1 {
                    p.f_code[1] = [1, 1];
                }
            })
        },
    );
    add("picture_structure 00", &[R::PcePictureStructure], h262(), {
        stream(Seq::default(), |b| {
            b.picture(&Pic::new(I, 0));
            let mut p = Pic::new(P, 1);
            p.structure = 0;
            b.pic(&p);
            b.pic.structure = 3;
            b.picture_data();
        })
    });
    add(
        "top_field_first in a field picture",
        &[R::PceTffField],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                field_pair(b, 0, |p| p.tff = true, |_| {})
            })
        },
    );
    add(
        "top_field_first without repeat in a progressive sequence",
        &[R::PceTffProgressive],
        h262(),
        ipbb(Seq::default(), |i, p| p.tff = i == 1),
    );
    add(
        "frame_pred_frame_dct in a field picture",
        &[R::PceFpfdField],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                field_pair(b, 0, |_| {}, |p| p.fpfd = true)
            })
        },
    );
    add(
        "frame_pred_frame_dct 0 in a progressive sequence",
        &[R::PceFpfdProgressive],
        h262(),
        ipbb(Seq::default(), |i, p| p.fpfd = i != 1),
    );
    add(
        "repeat_first_field in a field picture",
        &[R::PceRffField],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                field_pair(b, 0, |p| p.rff = true, |_| {})
            })
        },
    );
    add(
        "repeat_first_field on an interlaced frame",
        &[R::PceRffInterlaced],
        h262(),
        // the frame displayed after the three fields starts with the bottom field
        ipbb_interlaced(|i, p| {
            p.rff = i == 3;
            p.tff = i != 1;
        }),
    );
    add(
        "chroma_420_type differs from progressive_frame",
        &[R::PceChroma420Type],
        h262(),
        ipbb(Seq::default(), |i, p| p.c420 = i != 2),
    );
    add(
        "progressive_frame in a field picture",
        &[R::PceProgressiveFrameField],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                field_pair(
                    b,
                    0,
                    |p| {
                        p.prog = true;
                        p.c420 = true;
                    },
                    |_| {},
                );
            })
        },
    );
    add(
        "composite_display_flag with repeat_first_field",
        &[R::PceComposite],
        h262(),
        {
            ipbb_interlaced(|i, p| {
                if i == 3 {
                    p.prog = true;
                    p.c420 = true;
                    p.rff = true;
                    p.composite = true;
                }
                p.tff = i != 1;
            })
        },
    );
    // 7.12: the output fields alternate. Display order of `film` is I, B, B, P.
    add(
        "two frames top field first around a three-field frame",
        &[R::OutFieldParity],
        h262(),
        film(&[(2, true), (3, false), (2, true), (2, true)], |_, _| {}),
    );
    add(
        "a bottom-first frame after a top-first frame of two fields",
        &[R::OutFieldParity],
        h262(),
        film(&[(2, true), (2, true), (2, false), (2, false)], |_, _| {}),
    );
    add(
        "three fields, then the same parity again",
        &[R::OutFieldParity],
        h262(),
        film(&[(3, true), (3, true), (2, false), (2, false)], |_, _| {}),
    );
    add(
        "a field pair top first after a three-field frame",
        &[R::OutFieldParity],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                let mut p = Pic::interlaced(I, 0);
                (p.prog, p.c420, p.rff, p.fpfd) = (true, true, true, true);
                b.picture(&p);
                // top field, then bottom field: after top, bottom, top
                let mut p = Pic::field(P, 1, 1);
                p.f_code[0] = [2, 2];
                b.picture(&p);
                let mut p = Pic::field(P, 1, 2);
                p.f_code[0] = [2, 2];
                b.picture(&p);
            })
        },
    );
    add(
        "the parity carries over a GOP header and a repeated sequence header",
        &[R::OutFieldParity],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                let mut p = Pic::interlaced(I, 0);
                (p.prog, p.c420, p.rff, p.fpfd) = (true, true, true, true);
                b.picture(&p);
                b.seq();
                b.sde(0, Some((1, 1, 1)), true);
                b.gop(1, true);
                b.picture(&Pic::interlaced(I, 0));
            })
        },
    );
    add("two top fields", &[R::PceFieldParity], h262(), {
        stream(Seq::interlaced(), |b| {
            b.picture(&Pic::field(I, 0, 1));
            b.picture(&Pic::field(I, 0, 1));
        })
    });
    add(
        "a frame picture after a first field",
        &[R::PceFieldParity],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                b.picture(&Pic::field(I, 0, 1));
                b.picture(&Pic::interlaced(I, 0));
            })
        },
    );
    add("P field then I field", &[R::PceFieldType], h262(), {
        stream(Seq::interlaced(), |b| {
            b.picture(&Pic::interlaced(I, 0));
            b.picture(&Pic::field(P, 1, 1));
            b.picture(&Pic::field(I, 1, 2));
        })
    });

    // ------------------------------------------------------------ other extensions
    let with_ext = |sde: bool, payload: &dyn Fn(&mut common::BitW)| {
        let mut b = Bld::new(Seq::default());
        b.seq();
        if sde {
            b.sde(0, Some((1, 1, 1)), true);
        }
        b.gop(0, true);
        b.pic(&Pic::new(I, 0));
        b.w.start(0xB5);
        payload(&mut b.w);
        b.picture_data();
        b.seq_end();
        b.finish()
    };
    add(
        "chroma matrix in a 4:2:0 stream",
        &[R::QmxChroma],
        h262(),
        {
            with_ext(true, &|w| {
                w.put(4, 3);
                w.put(2, 0);
                w.put(1, 1);
                w.put(8, 8);
                for _ in 1..64 {
                    w.put(8, 16);
                }
                w.put(1, 0);
            })
        },
    );
    let pdx = |w: &mut common::BitW, marker: u32| {
        w.put(4, 7);
        w.put(16, 0x10);
        w.put(1, marker);
        w.put(16, 0x10);
        w.put(1, 1);
    };
    add(
        "picture display extension without sequence display extension",
        &[R::PdxNoSde],
        h262(),
        with_ext(false, &|w| pdx(w, 1)),
    );
    add(
        "picture display extension marker_bit 0",
        &[R::PdxMarker],
        h262(),
        with_ext(true, &|w| pdx(w, 0)),
    );
    let cpx = |w: &mut common::BitW, reserved: u32, marker: u32| {
        w.put(4, 4);
        w.put(1, 1);
        w.put(8, 0x21);
        w.put(1, 1);
        w.put(7, reserved);
        w.put(1, marker);
        w.put(20, 0x12345);
        w.put(1, 1);
        w.put(22, 0x2AAAAA);
        w.put(1, 1);
        w.put(22, 0x155555);
    };
    add(
        "copyright extension marker_bit 0",
        &[R::CpxMarker],
        h262(),
        with_ext(true, &|w| cpx(w, 0, 0)),
    );
    add(
        "copyright extension reserved bits",
        &[R::CpxReserved],
        h262(),
        with_ext(true, &|w| cpx(w, 5, 1)),
    );

    // ------------------------------------------------------------ slice
    add(
        "slice_vertical_position beyond the picture",
        &[R::SliceVpos],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                intra_rows(b, 0..3);
                intra_rows(b, 5..6);
            })
        },
    );
    add("a row coded twice", &[R::SliceOrder], h262(), {
        i_custom(Seq::default(), Pic::new(I, 0), |b| {
            intra_rows(b, 0..3);
            intra_rows(b, 1..2);
        })
    });
    add("a row missing", &[R::SliceGap], h262(), {
        i_custom(Seq::default(), Pic::new(I, 0), |b| {
            intra_rows(b, 0..1);
            intra_rows(b, 2..3);
        })
    });
    add("the last row missing", &[R::SliceGap], h262(), {
        i_custom(Seq::default(), Pic::new(I, 0), |b| intra_rows(b, 0..2))
    });
    add(
        "a slice that starts at the second macroblock",
        &[R::SliceGap],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                b.slice(0, 4);
                b.intra_mb(2);
                b.intra_mb(1);
                b.intra_mb(1);
                intra_rows(b, 1..3);
            })
        },
    );
    add(
        "a slice that ends before the row does",
        &[R::SliceGap],
        h262(),
        {
            p_custom(Seq::default(), Pic::new(P, 1), |b| {
                b.slice(0, 4);
                b.fwd_mb(1, 0, 0);
                b.fwd_mb(1, 0, 0);
                for r in 1..3 {
                    b.slice(r, 4);
                    b.fwd_mb(1, 0, 0);
                    b.fwd_mb(3, 0, 0);
                }
            })
        },
    );
    add(
        "five macroblocks in a row of four",
        &[R::SliceRowOverrun],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                b.slice(0, 4);
                for _ in 0..5 {
                    b.intra_mb(1);
                }
                intra_rows(b, 1..3);
            })
        },
    );
    add(
        "slice quantiser_scale_code 0",
        &[R::SliceQscaleZero],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                b.slice(0, 0);
                for _ in 0..4 {
                    b.intra_mb(1);
                }
                intra_rows(b, 1..3);
            })
        },
    );
    let slice_ext = |b: &mut Bld, row: u32, intra: u32, enable: u32, id: u32, extra: bool| {
        b.w.start((row + 1) as u8);
        b.w.put(5, 4);
        b.w.put(1, 1);
        b.w.put(1, intra);
        b.w.put(1, enable);
        b.w.put(6, id);
        if extra {
            b.w.put(1, 1);
            b.w.put(8, 0xAA);
        }
        b.w.put(1, 0);
    };
    add("extra_information_slice", &[R::SliceExtraInfo], h262(), {
        i_custom(Seq::default(), Pic::new(I, 0), |b| {
            slice_ext(b, 0, 1, 0, 0, true);
            for _ in 0..4 {
                b.intra_mb(1);
            }
            intra_rows(b, 1..3);
        })
    });
    add(
        "intra_slice on a slice with non-intra macroblocks",
        &[R::SliceIntraFlag],
        h262(),
        {
            p_custom(Seq::default(), Pic::new(P, 1), |b| {
                for r in 0..3 {
                    slice_ext(b, r, u32::from(r == 1), 0, 0, false);
                    b.fwd_mb(1, 0, 0);
                    b.fwd_mb(3, 0, 0);
                }
            })
        },
    );
    add(
        "slice_picture_id without enable",
        &[R::SlicePictureId],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                for r in 0..3 {
                    slice_ext(b, r, 1, 0, u32::from(r == 2) * 9, false);
                    for _ in 0..4 {
                        b.intra_mb(1);
                    }
                }
            })
        },
    );
    add(
        "slice_picture_id changes within a picture",
        &[R::SlicePictureId],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                for r in 0..3 {
                    slice_ext(b, r, 1, 1, 9 + u32::from(r == 2), false);
                    for _ in 0..4 {
                        b.intra_mb(1);
                    }
                }
            })
        },
    );
    add("a slice without macroblocks", &[R::SliceEmpty], h262(), {
        i_custom(Seq::default(), Pic::new(I, 0), |b| {
            intra_rows(b, 0..2);
            b.slice(2, 4);
        })
    });
    add(
        "a slice cut inside a macroblock",
        &[R::SliceTruncated],
        h262(),
        {
            i_custom(Seq::default(), Pic::new(I, 0), |b| {
                intra_rows(b, 0..2);
                b.slice(2, 4);
                for _ in 0..3 {
                    b.intra_mb(1);
                }
                b.inc(1);
                b.mb_type(false, false, false, false, true);
                b.dc(true, 0);
                b.eob(false);
                b.w.put(3, 0b101); // dct_dc_size 3, its differential is missing
            })
        },
    );

    // ------------------------------------------------------------ macroblock
    let i_row0 = |f: &dyn Fn(&mut Bld)| {
        i_custom(Seq::default(), Pic::new(I, 0), |b| {
            b.slice(0, 4);
            f(b);
            intra_rows(b, 1..3);
        })
    };
    add(
        "invalid macroblock_address_increment",
        &[R::MbAddrVlc],
        h262(),
        {
            i_row0(&|b| {
                b.intra_mb(1);
                b.w.put(11, 0);
                b.w.put(8, 0xFF);
            })
        },
    );
    add("macroblock_stuffing", &[R::MbStuffing], h262(), {
        i_row0(&|b| {
            b.intra_mb(1);
            b.w.put(11, 0b000_0000_1111);
            for _ in 0..3 {
                b.intra_mb(1);
            }
        })
    });
    add("invalid macroblock_type", &[R::MbTypeVlc], h262(), {
        i_row0(&|b| {
            b.intra_mb(1);
            b.inc(1);
            b.w.put(2, 0);
            b.w.put(8, 0xFF);
        })
    });
    add(
        "skipped macroblock in an I-picture",
        &[R::MbSkipI],
        h262(),
        {
            i_row0(&|b| {
                b.intra_mb(1);
                b.intra_mb(2);
                b.intra_mb(1);
            })
        },
    );
    add(
        "skipped macroblock after intra in a B-picture",
        &[R::MbSkipBAfterIntra],
        h262(),
        {
            b_custom(Seq::default(), Pic::new(B, 1), |b| {
                for r in 0..3 {
                    b.slice(r, 4);
                    if r == 1 {
                        b.intra_mb(1);
                        b.bwd_mb(3, 0, 0);
                    } else {
                        b.bwd_mb(1, 0, 0);
                        b.bwd_mb(3, 0, 0);
                    }
                }
            })
        },
    );
    add("frame_motion_type 00", &[R::MbMotionType], h262(), {
        p_custom(Seq::interlaced(), Pic::interlaced(P, 1), |b| {
            for r in 0..4 {
                b.slice(r, 4);
                if r == 2 {
                    b.inc(1);
                    b.mb_type(false, true, false, false, false);
                    b.w.put(2, 0);
                    b.w.put(8, 0xFF);
                } else {
                    for _ in 0..4 {
                        b.fwd_mb(1, 0, 0);
                    }
                }
            }
        })
    });
    // dual prime: rows 0 and 3 intra (a zero vector's derived vectors leave the picture
    // there), rows 1 and 2 dual prime with zero vector and zero dmvector
    let dual_prime_rows = |b: &mut Bld| {
        for r in 0..4 {
            b.slice(r, 4);
            for _ in 0..4 {
                if r == 0 || r == 3 {
                    b.intra_mb(1);
                } else {
                    b.inc(1);
                    b.mb_type(false, true, false, false, false);
                    b.w.put(2, 3);
                    for _ in 0..2 {
                        b.mv_component(2, 0);
                        b.w.put(1, 0);
                    }
                }
            }
        }
    };
    add("dual prime in a B-picture", &[R::MbDualPrime], h262(), {
        stream(Seq::interlaced(), |b| {
            b.picture(&Pic::interlaced(I, 0));
            b.picture(&Pic::interlaced(P, 2));
            b.pic(&Pic::interlaced(B, 1));
            dual_prime_rows(b);
        })
    });
    add(
        "B-picture after a dual-prime P-picture",
        &[R::MbDualPrimeB],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                b.picture(&Pic::interlaced(I, 0));
                b.pic(&Pic::interlaced(P, 2));
                dual_prime_rows(b);
                b.picture(&Pic::interlaced(B, 1));
            })
        },
    );
    add(
        "macroblock quantiser_scale_code 0",
        &[R::MbQscaleZero],
        h262(),
        {
            i_row0(&|b| {
                b.inc(1);
                b.mb_type(true, false, false, false, true);
                b.w.put(5, 0);
                for i in 0..6 {
                    b.dc(i < 4, 0);
                    b.eob(false);
                }
                for _ in 0..3 {
                    b.intra_mb(1);
                }
            })
        },
    );
    add(
        "marker_bit 0 after a concealment vector",
        &[R::MbConcealMarker],
        h262(),
        {
            let mut p = Pic::new(I, 0);
            p.cmv = true;
            p.f_code[0] = [1, 1];
            i_custom(Seq::default(), p, |b| {
                for r in 0..3 {
                    b.slice(r, 4);
                    for c in 0..4 {
                        b.inc(1);
                        b.mb_type(false, false, false, false, true);
                        b.mv(0, 0, 0);
                        b.w.put(1, u32::from(!(r == 1 && c == 2)));
                        for i in 0..6 {
                            b.dc(i < 4, 0);
                            b.eob(false);
                        }
                    }
                }
            })
        },
    );
    let p_row0 = |f: &dyn Fn(&mut Bld)| {
        p_custom(Seq::default(), Pic::new(P, 1), |b| {
            b.slice(0, 4);
            f(b);
            for r in 1..3 {
                b.slice(r, 4);
                b.fwd_mb(1, 0, 0);
                b.fwd_mb(3, 0, 0);
            }
        })
    };
    add("invalid coded_block_pattern", &[R::MbCbpVlc], h262(), {
        p_row0(&|b| {
            b.inc(1);
            b.mb_type(false, false, false, true, false);
            b.w.put(9, 0);
            b.w.put(8, 0xFF);
        })
    });
    add("coded_block_pattern 0", &[R::MbCbpZero], h262(), {
        p_row0(&|b| {
            b.inc(1);
            b.mb_type(false, false, false, true, false);
            b.cbp(0);
            for _ in 0..3 {
                b.fwd_mb(1, 0, 0);
            }
        })
    });
    add(
        "three macroblocks above 4608 bits in a row",
        &[R::MbBits],
        h262(),
        {
            i_row0(&|b| {
                for _ in 0..3 {
                    big_intra_mb(b);
                }
                b.intra_mb(1);
            })
        },
    );
    add(
        "P field after I field predicted from the same parity",
        &[R::MbPFieldAfterI],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                b.picture(&Pic::field(I, 0, 1));
                b.pic(&Pic::field(P, 0, 2));
                for r in 0..2 {
                    b.slice(r, 4);
                    for c in 0..4 {
                        b.fsel = Some(r == 1 && c == 1);
                        b.fwd_mb(1, 0, 0);
                    }
                }
                b.fsel = None;
            })
        },
    );
    add(
        "skipped macroblocks in a P field after an I field",
        &[R::MbPFieldAfterI],
        h262(),
        {
            stream(Seq::interlaced(), |b| {
                b.picture(&Pic::field(I, 0, 1));
                b.pic(&Pic::field(P, 0, 2));
                b.fsel = Some(false);
                for r in 0..2 {
                    b.slice(r, 4);
                    b.fwd_mb(1, 0, 0);
                    b.fwd_mb(3, 0, 0);
                }
                b.fsel = None;
            })
        },
    );

    // ------------------------------------------------------------ motion vectors
    add("invalid motion_code", &[R::MvCodeVlc], h262(), {
        p_row0(&|b| {
            b.inc(1);
            b.mb_type(false, true, false, false, false);
            b.w.put(11, 0);
            b.w.put(8, 0xFF);
        })
    });
    add(
        "a vector coded with f_code 15",
        &[R::MvFcodeUnusable],
        h262(),
        {
            let mut p = Pic::new(P, 1);
            p.f_code[0] = [15, 15];
            p_custom(Seq::default(), p, |b| {
                intra_rows(b, 0..2);
                b.slice(2, 4);
                b.inc(1);
                b.mb_type(false, true, false, false, false);
                b.w.put(2, 0b11); // two motion_codes of 0
                b.fwd_mb(3, 0, 0);
            })
        },
    );
    add(
        "vector delta of +32 with f_code 2",
        &[R::MvDeltaRange],
        h262(),
        {
            p_row0(&|b| {
                b.fwd_mb(1, 0, 0);
                b.fwd_mb(1, 32, 0);
                b.fwd_mb(1, 0, 0);
                b.fwd_mb(1, 0, 0);
            })
        },
    );
    add(
        "field vector beyond half the vertical range",
        &[R::MvFieldHalfRange],
        h262(),
        {
            p_custom(Seq::interlaced(), Pic::interlaced(P, 1), |b| {
                for r in 0..4 {
                    b.slice(r, 4);
                    for c in 0..4 {
                        if r == 1 && c == 3 {
                            b.inc(1);
                            b.mb_type(false, true, false, false, false);
                            b.w.put(2, 1); // field-based
                            b.w.put(1, 0);
                            b.mv(0, 0, 16);
                            b.w.put(1, 1);
                            b.mv(0, 0, 0);
                        } else {
                            b.fwd_mb(1, 0, 0);
                        }
                    }
                }
            })
        },
    );
    add(
        "vector half a sample left of the picture",
        &[R::MvOutside],
        h262(),
        {
            p_row0(&|b| {
                b.fwd_mb(1, -1, 0);
                b.fwd_mb(1, 1, 0);
                b.fwd_mb(2, 0, 0);
            })
        },
    );
    add(
        "half-sample position past the right edge",
        &[R::MvOutside],
        h262(),
        {
            p_row0(&|b| {
                b.fwd_mb(1, 0, 0);
                b.fwd_mb(2, 0, 0);
                b.fwd_mb(1, 1, 0);
            })
        },
    );
    add("vector below the bottom row", &[R::MvOutside], h262(), {
        p_custom(Seq::default(), Pic::new(P, 1), |b| {
            for r in 0..3 {
                b.slice(r, 4);
                b.fwd_mb(1, 0, i32::from(r == 2) * 2);
                b.fwd_mb(1, 0, -i32::from(r == 2) * 2);
                b.fwd_mb(2, 0, 0);
            }
        })
    });
    add(
        "skipped B macroblock inherits a vector that leaves the picture",
        &[R::MvOutside],
        h262(),
        {
            let mut p = Pic::new(B, 1);
            p.f_code[1] = [3, 3];
            b_custom(Seq::default(), p, |b| {
                for r in 0..3 {
                    b.slice(r, 4);
                    if r == 0 {
                        // +48 half samples is fine at columns 0 and 1, not at the skipped column 2
                        b.bwd_mb(1, 48, 0);
                        b.bwd_mb(1, 0, 0);
                        b.bwd_mb(2, -48, 0);
                    } else {
                        b.bwd_mb(1, 0, 0);
                        b.bwd_mb(3, 0, 0);
                    }
                }
            })
        },
    );
    add(
        "vertical vector of 128.5 samples",
        &[R::MvLevelVertical, R::PlFcode],
        h262(),
        {
            let seq = Seq {
                h: 176,
                ..Seq::default()
            };
            let mut p = Pic::new(P, 1);
            p.f_code[0] = [2, 6];
            p_custom(seq, p, |b| {
                for r in 0..11 {
                    b.slice(r, 4);
                    b.fwd_mb(1, 0, if r == 0 { 257 } else { 0 });
                    b.fwd_mb(1, 0, if r == 0 { -257 } else { 0 });
                    b.fwd_mb(2, 0, 0);
                }
            })
        },
    );

    // ------------------------------------------------------------ block
    let i_block = |f: &dyn Fn(&mut Bld)| {
        i_row0(&|b| {
            b.inc(1);
            b.mb_type(false, false, false, false, true);
            f(b);
            for i in 1..6 {
                b.dc(i < 4, 0);
                b.eob(false);
            }
            for _ in 0..3 {
                b.intra_mb(1);
            }
        })
    };
    add("intra DC above the range", &[R::BlkDcRange], h262(), {
        i_block(&|b| {
            b.dc(true, 200);
            b.eob(false);
        })
    });
    add("invalid DCT coefficient code", &[R::BlkCoefVlc], h262(), {
        i_row0(&|b| {
            b.inc(1);
            b.mb_type(false, false, false, false, true);
            b.dc(true, 0);
            b.w.put(16, 0);
            b.w.put(8, 0xFF);
        })
    });
    add("coefficient index past 63", &[R::BlkOverrun], h262(), {
        i_row0(&|b| {
            b.inc(1);
            b.mb_type(false, false, false, false, true);
            b.dc(true, 0);
            b.escape(40, 1);
            b.escape(40, 1);
            b.eob(false);
            b.w.put(8, 0xFF);
        })
    });
    add("65 coefficients", &[R::BlkOverrun], h262(), {
        i_row0(&|b| {
            b.inc(1);
            b.mb_type(false, false, false, false, true);
            b.dc(true, 0);
            for _ in 0..64 {
                b.coef(false, 0, 1);
            }
            b.eob(false);
            b.w.put(8, 0xFF);
        })
    });
    add("escape with level 0", &[R::BlkEscZero], h262(), {
        i_block(&|b| {
            b.dc(true, 0);
            b.escape(1, 0);
            b.eob(false);
        })
    });
    add(
        "escape with level -2048",
        &[R::BlkEscReserved, R::BlkSaturation],
        h262(),
        {
            i_block(&|b| {
                b.dc(true, 0);
                b.escape(1, 0x800);
                b.eob(false);
            })
        },
    );
    add(
        "escape for a pair that has a code",
        &[R::BlkEscRedundant],
        h262(),
        {
            i_block(&|b| {
                b.dc(true, 0);
                b.escape(0, 1);
                b.eob(false);
            })
        },
    );
    add("coefficient that saturates", &[R::BlkSaturation], h262(), {
        i_block(&|b| {
            b.dc(true, 0);
            b.escape(0, 2047);
            b.eob(false);
        })
    });

    // ------------------------------------------------------------ buffer model
    add(
        "second picture is not in the buffer in time",
        &[R::VbvUnderflow],
        h262(),
        {
            let seq = Seq {
                bit_rate: 1,
                vbv: 1,
                ..Seq::default()
            };
            stream(seq, |b| {
                for tr in 0..2 {
                    b.pic(&Pic::new(I, tr));
                    b.slice(0, 4);
                    big_intra_mb(b);
                    for _ in 0..3 {
                        b.intra_mb(1);
                    }
                    intra_rows(b, 1..3);
                }
            })
        },
    );
    add(
        "a picture larger than the buffer",
        &[R::VbvPictureSize],
        h262(),
        {
            let seq = Seq {
                vbv: 1,
                ..Seq::default()
            };
            i_custom(seq, Pic::new(I, 0), |b| {
                b.slice(0, 4);
                big_intra_mb(b);
                big_intra_mb(b);
                b.intra_mb(1);
                b.intra_mb(1);
                intra_rows(b, 1..3);
            })
        },
    );
    {
        // vbv_delay values that put three pictures of 9.5 kbit into a buffer of 16384 bits
        let seq = Seq {
            vbv: 1,
            ..Seq::default()
        };
        let mut d = stream(seq, |b| {
            for tr in 0..3 {
                b.pic(&Pic::new(if tr == 0 { I } else { P }, tr));
                if tr == 0 {
                    b.slice(0, 4);
                    big_intra_mb(b);
                    for _ in 0..3 {
                        b.intra_mb(1);
                    }
                    intra_rows(b, 1..3);
                } else {
                    b.slice(0, 4);
                    b.inc(1);
                    b.mb_type(false, false, false, false, true);
                    for i in 0..6 {
                        b.dc(i < 4, 0);
                        for _ in 0..63 {
                            b.escape(0, 41);
                        }
                        b.eob(false);
                    }
                    for _ in 0..3 {
                        b.intra_mb(1);
                    }
                    intra_rows(b, 1..3);
                }
            }
        });
        // input at 3 Mbit/s: tau(n+1) = tau(n) + T - d(n) / R
        let t = 1001.0 / 30000.0;
        let mut tau = 0.1f64;
        for n in 0..3 {
            set_vbv_delay(&mut d, n, (tau * 90000.0).round() as u32);
            if let (Some(a), Some(b)) = (find_unit(&d, 0, None, n), find_unit(&d, 0, None, n + 1)) {
                tau += t - ((b - a) * 8) as f64 / 3.0e6;
            }
        }
        add(
            "vbv_delay fills the buffer beyond its size",
            &[R::VbvOverflow],
            h262(),
            d,
        );
    }
    add(
        "vbv_delay values that run backwards",
        &[R::VbvDelayRate, R::VbvUnderflow],
        h262(),
        {
            let mut d = base_stream();
            for n in 0..4 {
                set_vbv_delay(&mut d, n, if n == 1 { 100 } else { 9000 });
            }
            d
        },
    );
    add(
        "vbv_delay values that imply 30 Mbit/s",
        &[R::VbvDelayRate],
        h262(),
        {
            let mut d = base_stream();
            for n in 0..4 {
                set_vbv_delay(&mut d, n, if n == 0 { 9000 } else { 12003 });
            }
            d
        },
    );
    add(
        "operating model at 1000 bit/s",
        &[R::VbvOperating],
        Options {
            delivery: Delivery::Operating { rate: Some(1000.0) },
            ..Options::default()
        },
        base_stream(),
    );

    // ------------------------------------------------------------ profile and level
    add(
        "High profile",
        &[R::PlProfile],
        h262(),
        seq_with(&|s| s.pli = 0x14),
    );
    add("4:2:2 in Main profile", &[R::PlChroma], h262(), {
        let seq = Seq {
            chroma: 2,
            ..Seq::default()
        };
        ipbb(seq, |_, p| p.c420 = false)
    });
    add(
        "frame_rate_extension_n 2",
        &[R::PlFrameRateExt],
        h262(),
        seq_with(&|s| s.fr_n = 2),
    );
    add(
        "aspect_ratio_information 4",
        &[R::PlAspect],
        h262(),
        seq_with(&|s| s.aspect = 4),
    );
    add(
        "B-pictures in Simple profile",
        &[R::PlPictureType],
        h262(),
        seq_with(&|s| s.pli = 0x58),
    );
    add(
        "repeat_first_field in a progressive 29.97 Hz sequence",
        &[R::PlRff],
        h262(),
        ipbb(Seq::default(), |i, p| p.rff = i == 3),
    );
    add(
        "576 lines at 29.97 Hz at Main level",
        &[R::PlRffMainLevel],
        h262(),
        {
            let seq = Seq {
                h: 512,
                ..Seq::interlaced()
            };
            stream(seq, |b| b.picture(&Pic::interlaced(I, 0)))
        },
    );
    add(
        "scalable extension in Main profile",
        &[R::PlScalable],
        h262(),
        {
            let mut b = Bld::new(Seq::default());
            b.seq();
            b.ext(&[0x50, 0x00, 0x80]);
            b.gop(0, true);
            b.picture(&Pic::new(I, 0));
            b.seq_end();
            b.finish()
        },
    );
    add("intra_dc_precision 11 bits", &[R::PlIntraDc], h262(), {
        ipbb(Seq::default(), |i, p| {
            if i == 0 {
                p.dc_prec = 3;
            }
        })
    });
    add("vertical f_code 6", &[R::PlFcode], h262(), {
        ipbb(Seq::default(), |i, p| {
            if i == 1 {
                p.f_code[0][1] = 6;
            }
        })
    });
    add(
        "horizontal f_code 9 at Main level",
        &[R::PlFcode],
        h262(),
        {
            ipbb(Seq::default(), |i, p| {
                if i == 2 {
                    p.f_code[1][0] = 9;
                }
            })
        },
    );
    add(
        "59.94 Hz at Main level",
        &[R::PlFrameRateCode],
        h262(),
        seq_with(&|s| s.frc = 7),
    );
    add(
        "736 samples per line at Main level",
        &[R::PlSize],
        h262(),
        seq_with(&|s| s.w = 736),
    );
    add(
        "1280x720 at 59.94 Hz at High-1440 level",
        &[R::PlSampleRate],
        h262(),
        {
            seq_with(&|s| {
                s.w = 1280;
                s.h = 720;
                s.frc = 7;
                s.pli = 0x46;
            })
        },
    );
    add(
        "16 Mbit/s at Main level",
        &[R::PlBitRate],
        h262(),
        seq_with(&|s| s.bit_rate = 40000),
    );
    add(
        "VBV buffer of 113 units at Main level",
        &[R::PlVbvSize],
        h262(),
        seq_with(&|s| s.vbv = 113),
    );

    // ------------------------------------------------------------ user data (H.262)
    add("24 zero bits inside user data", &[R::UdZeroRun], h262(), {
        stream(Seq::default(), |b| {
            b.pic(&Pic::new(I, 0));
            b.user_data(&[0x41, 0x00, 0x00, 0x00, 0x42]);
            b.picture_data();
        })
    });
    add("zero stuffing after user data", &[R::UdZeroRun], h262(), {
        stream(Seq::default(), |b| {
            b.pic(&Pic::new(I, 0));
            b.user_data(&[0x41, 0x42, 0x00, 0x00, 0x00]);
            b.picture_data();
        })
    });

    // ------------------------------------------------------------ broadcast profiles
    let a53 = prof(Profile::A53);
    let c64 = prof(Profile::Scte43Qam64);
    let c256 = prof(Profile::Scte43Qam256);
    add("Simple profile under A/53", &[R::BcProfile], a53, {
        // no B-pictures: I P P P
        let mut seq = Seq {
            w: 704,
            h: 480,
            aspect: 2,
            progressive: false,
            pli: 0x58,
            vbv: 112,
            ..Seq::default()
        };
        seq.bit_rate = 10000;
        let mut b = Bld::new(seq);
        b.seq();
        b.sde(0, Some((1, 1, 1)), true);
        b.gop(0, true);
        for (i, t) in [I, P, P, P].into_iter().enumerate() {
            b.pic(&Pic::interlaced(t, i as u16));
            b.user_data(&cc(20, &[0xFD, 0xFC]));
            b.picture_data();
        }
        b.seq_end();
        b.finish()
    });
    add(
        "square samples at 704x480",
        &[R::BcFormat],
        a53,
        bc(|s| s.aspect = 1, SDE709, std_cc, |_, _| {}),
    );
    add(
        "720x480 under A/53",
        &[R::BcFormat],
        a53,
        bc(|s| s.w = 720, SDE709, std_cc, |_, _| {}),
    );
    add("28 Mbit/s on 64-QAM", &[R::BcBitRate], c64, {
        bc_720p(|s| s.bit_rate = 70000, |_| vec![cc(10, &[0xFC])])
    });
    add("VBV buffer of 500 units", &[R::BcVbvSize], c256, {
        bc_720p(|s| s.vbv = 500, |_| vec![cc(10, &[0xFC])])
    });
    add(
        "frame_rate_extension under A/53",
        &[
            R::BcSeqExtFields,
            R::PlFrameRateExt,
            R::PlSampleRate,
            R::CcCount,
            R::Cc608Position,
        ],
        a53,
        bc(|s| s.fr_n = 2, SDE709, std_cc, |_, _| {}),
    );
    add("High level for 704x480", &[R::BcLevelLowest], a53, {
        bc(|s| s.pli = 0x44, SDE709, std_cc, |_, _| {})
    });
    add(
        "no color description",
        &[R::BcColor],
        a53,
        bc(|_| {}, None, std_cc, |_, _| {}),
    );
    add(
        "no color description, 720p on cable",
        &[R::BcColor],
        c256,
        {
            bc(
                |s| {
                    s.w = 1280;
                    s.h = 720;
                    s.aspect = 3;
                    s.frc = 7;
                    s.progressive = true;
                    s.pli = 0x44;
                    s.bit_rate = 37500;
                    s.vbv = 400;
                },
                Some((0, None)),
                |_| vec![cc(10, &[0xFC])],
                |_, _| {},
            )
        },
    );
    add("video_format 2", &[R::BcVideoFormat], a53, {
        bc(|_| {}, Some((2, Some((1, 1, 1)))), std_cc, |_, _| {})
    });
    add("SMPTE 170M color on cable", &[R::BcColor709], c256, {
        bc(|_| {}, Some((0, Some((6, 6, 6)))), std_cc, |_, _| {})
    });
    add("vbv_delay 50000", &[R::BcVbvDelay], a53, {
        let mut d = bc_default();
        for n in 0..4 {
            set_vbv_delay(&mut d, n, 50000);
        }
        d
    });
    add(
        "progressive_frame with field tools allowed",
        &[R::BcProgressiveFpfd],
        a53,
        {
            bc(
                |_| {},
                SDE709,
                std_cc,
                |i, p| {
                    if i == 1 {
                        p.prog = true;
                        p.c420 = true;
                    }
                },
            )
        },
    );
    add(
        "0.7 s between sequence headers",
        &[R::BcSeqInterval],
        a53,
        {
            let seq = Seq {
                w: 704,
                h: 480,
                aspect: 2,
                progressive: false,
                vbv: 112,
                ..Seq::default()
            };
            let mut b = Bld::new(seq);
            for g in 0..2 {
                b.seq();
                b.sde(0, Some((1, 1, 1)), true);
                b.gop(g * 21, true);
                for i in 0..21 {
                    b.pic(&Pic::interlaced(if i == 0 { I } else { P }, i));
                    b.user_data(&cc(20, &[0xFD, 0xFC]));
                    b.picture_data();
                }
            }
            b.seq_end();
            b.finish()
        },
    );
    add("buffer delay of 1.8 s", &[R::BcBufferDelay], a53, {
        bc(|s| s.bit_rate = 2500, SDE709, std_cc, |_, _| {})
    });
    add(
        "two caption structures in one picture",
        &[R::UdDupType],
        a53,
        {
            bc(
                |_| {},
                SDE709,
                |i| {
                    if i == 2 {
                        vec![cc(20, &[0xFD, 0xFC]); 2]
                    } else {
                        std_cc(i)
                    }
                },
                |_, _| {},
            )
        },
    );
    add("reserved user_data_type_code", &[R::UdTypeReserved], a53, {
        bc(
            |_| {},
            SDE709,
            |i| {
                let mut v = std_cc(i);
                if i == 0 {
                    v.push(vec![0x47, 0x41, 0x39, 0x34, 0x04, 0xFF]);
                }
                v
            },
            |_, _| {},
        )
    });
    let afd = |af: u8| vec![0x44, 0x54, 0x47, 0x31, 0x41, 0xF0 | af];
    add("AFD before captions", &[R::UdOrder], c256, {
        bc(
            |_| {},
            SDE709,
            |i| {
                vec![afd(8), cc(20, &[0xFD, 0xFC])]
                    .into_iter()
                    .rev()
                    .cycle()
                    .skip(usize::from(i == 0))
                    .take(2)
                    .collect()
            },
            |_, _| {},
        )
    });
    add("SCTE 20 in 720p", &[R::UdScte20Format], c256, {
        bc_720p(
            |_| {},
            |i| {
                let mut v = Vec::new();
                if i == 0 {
                    v.push(vec![0x03, 0x81, 0x8F, 0xFF]);
                }
                v.push(cc(10, &[0xFC]));
                v
            },
        )
    });
    add(
        "user data with an unknown identifier",
        &[R::UdIdentifier],
        a53,
        {
            bc(
                |_| {},
                SDE709,
                |i| {
                    let mut v = std_cc(i);
                    if i == 0 {
                        v.push(b"ABCD1234".to_vec());
                    }
                    v
                },
                |_, _| {},
            )
        },
    );
    add(
        "captions after the sequence header",
        &[R::UdPlacement],
        a53,
        {
            let mut d = bc_default();
            let Some(g) = find_unit(&d, 0xB8, None, 0) else {
                panic!("no GOP header");
            };
            let mut ud = vec![0, 0, 1, 0xB2];
            ud.extend(cc(20, &[0xFD, 0xFC]));
            d.splice(g..g, ud);
            d
        },
    );
    let cc_case = |f: &dyn Fn(usize) -> Vec<u8>| bc(|_| {}, SDE709, |i| vec![f(i)], |_, _| {});
    add("caption marker_bits 0xFE", &[R::CcTrailer], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                let n = c.len();
                c[n - 1] = 0xFE;
            }
            c
        })
    });
    add("cc_data shorter than cc_count", &[R::CcLength], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                let n = c.len();
                c.drain(n - 4..n - 1);
            }
            c
        })
    });
    add("cc_data first bit 0", &[R::CcFixedBits], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                c[5] &= 0x7F;
            }
            c
        })
    });
    add("cc_data reserved byte 0x00", &[R::CcFixedBits], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                c[6] = 0x01;
            }
            c
        })
    });
    add("caption construct with one_bit 0", &[R::CcMarker], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                c[7 + 3 * 5] = 0x7A;
            }
            c
        })
    });
    add(
        "caption constructs without marker bits make 23 zero bits",
        &[R::CcMarker, R::UdZeroRun],
        a53,
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                c[7 + 3 * 5] = 0x02;
            }
            c
        }),
    );
    add("cc_count 10 on a 29.97 Hz frame", &[R::CcCount], a53, {
        cc_case(&|i| {
            if i == 1 {
                cc(10, &[0xFD, 0xFC])
            } else {
                cc(20, &[0xFD, 0xFC])
            }
        })
    });
    add("valid CEA-608 construct after DTVCC", &[R::CcState], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                c[7 + 3 * 5] = 0xFC;
            }
            c
        })
    });
    add(
        "four leading CEA-608 constructs",
        &[R::Cc608Position],
        a53,
        {
            cc_case(&|i| {
                if i == 1 {
                    cc(20, &[0xFD, 0xFC, 0xFD, 0xFC])
                } else {
                    cc(20, &[0xFD, 0xFC])
                }
            })
        },
    );
    // CTA-708-E 4.3.2.2.1: a construct for each field, even with cc_valid 0. Field 2 data
    // with no field 1 construct at all breaks it (picture 1 goes on to DTVCC padding at once).
    add(
        "field 2 data without a field 1 construct",
        &[R::Cc608Field2],
        a53,
        {
            cc_case(&|i| {
                if i == 1 {
                    cc(20, &[0xFD])
                } else {
                    cc(20, &[0xFD, 0xFC])
                }
            })
        },
    );
    add(
        "field 1 data without a field 2 construct",
        &[R::Cc608Field2],
        a53,
        {
            cc_case(&|i| {
                if i == 1 {
                    cc(20, &[0xFC])
                } else {
                    cc(20, &[0xFD, 0xFC])
                }
            })
        },
    );
    // ... while valid field 2 data beside a field 1 construct with cc_valid 0 is the form the
    // clause prescribes (4.3.2.2: the decoder generates the field 1 run-in itself), and so is
    // a cc_data() whose CEA-608 constructs are both cc_valid 0.
    add(
        "field 2 valid, field 1 present with cc_valid 0",
        &[],
        a53,
        {
            cc_case(&|i| {
                if i == 1 {
                    cc(20, &[0xFD, 0xF8])
                } else if i == 2 {
                    cc(20, &[0xF9, 0xF8])
                } else {
                    cc(20, &[0xFD, 0xFC])
                }
            })
        },
    );
    add(
        "field 1 first with top_field_first",
        &[R::Cc608Order],
        a53,
        {
            cc_case(&|i| {
                if i == 1 {
                    cc(20, &[0xFC, 0xFD])
                } else {
                    cc(20, &[0xFD, 0xFC])
                }
            })
        },
    );
    add("a picture without cc_data", &[R::CcMissing], a53, {
        bc(
            |_| {},
            SDE709,
            |i| if i == 2 { Vec::new() } else { std_cc(i) },
            |_, _| {},
        )
    });
    add("process_cc_data_flag 0", &[R::CcProcessFlag], a53, {
        cc_case(&|i| {
            let mut c = cc(20, &[0xFD, 0xFC]);
            if i == 1 {
                c[5] &= 0xBF;
            }
            c
        })
    });
    let with_ud = |i_only: Vec<u8>| {
        bc(
            |_| {},
            SDE709,
            move |i| {
                let mut v = std_cc(i);
                if i == 0 {
                    v.push(i_only.clone());
                }
                v
            },
            |_, _| {},
        )
    };
    add("afd_data with wrong reserved bits", &[R::AfdSyntax], a53, {
        with_ud(vec![0x44, 0x54, 0x47, 0x31, 0x41, 0x08])
    });
    add(
        "afd_data too long",
        &[R::AfdSyntax],
        a53,
        with_ud(vec![0x44, 0x54, 0x47, 0x31, 0x41, 0xF8, 0xFF]),
    );
    add(
        "active_format 0001",
        &[R::AfdReserved],
        a53,
        with_ud(afd(1)),
    );
    add(
        "active_format 0010",
        &[R::AfdNotRecommended],
        a53,
        with_ud(afd(2)),
    );
    add(
        "active_format 0100 without bar data",
        &[R::AfdNotRecommended],
        a53,
        with_ud(afd(4)),
    );
    add("bar_data with wrong reserved bits", &[R::BarSyntax], a53, {
        with_ud(vec![
            0x47, 0x41, 0x39, 0x34, 0x06, 0xC0, 0xC0, 0x10, 0xC0, 0x20,
        ])
    });
    add("bar_data with a top bar only", &[R::BarFlags], a53, {
        with_ud(vec![0x47, 0x41, 0x39, 0x34, 0x06, 0x8F, 0xC0, 0x10])
    });
    let after_seq = |extra: Vec<u8>| {
        // I (with the structure), repeat sequence header, P (without it)
        let seq = Seq {
            w: 704,
            h: 480,
            aspect: 2,
            progressive: false,
            vbv: 112,
            ..Seq::default()
        };
        let mut b = Bld::new(seq);
        for i in 0..2u16 {
            b.seq();
            b.sde(0, Some((1, 1, 1)), true);
            if i == 0 {
                b.gop(0, true);
            }
            b.pic(&Pic::interlaced(if i == 0 { I } else { P }, i));
            b.user_data(&cc(20, &[0xFD, 0xFC]));
            if i == 0 {
                b.user_data(&extra);
            }
            b.picture_data();
        }
        b.seq_end();
        b.finish()
    };
    add(
        "AFD missing after a sequence header",
        &[R::AfdAfterSeq],
        a53,
        after_seq(afd(8)),
    );
    add(
        "bar data missing after a sequence header",
        &[R::BarAfterSeq],
        a53,
        {
            after_seq(vec![
                0x47, 0x41, 0x39, 0x34, 0x06, 0xCF, 0xC0, 0x10, 0xC1, 0xD0,
            ])
        },
    );
    v
}

#[test]
fn base_streams_are_clean() {
    expect(&verify(&base_stream(), &h262()), &[]);
    expect(&verify(&ipbb_interlaced(|_, _| {}), &h262()), &[]);
    let fields = stream(Seq::interlaced(), |b| {
        field_pair(b, 0, |_| {}, |_| {});
        field_pair(
            b,
            1,
            |p| {
                p.ptype = P;
                p.f_code[0] = [2, 2];
            },
            |_| {},
        );
    });
    expect(&verify(&fields, &h262()), &[]);
    for p in [Profile::A53, Profile::Scte43Qam64, Profile::Scte43Qam256] {
        expect(&verify(&bc_default(), &prof(p)), &[]);
        expect(
            &verify(&bc_720p(|_| {}, |_| vec![cc(10, &[0xFC])]), &prof(p)),
            &[],
        );
    }
    let operating = Options {
        delivery: Delivery::Operating { rate: None },
        ..Options::default()
    };
    expect(&verify(&base_stream(), &operating), &[]);
}

/// Film cadences that are legal: the fields alternate through pictures of two and three
/// fields, whatever the first parity, and the GOP time code may count the coded frames or
/// the display time.
#[test]
fn film_cadences_are_clean() {
    for shown in [
        [(3, true), (2, false), (3, false), (2, true)],
        [(3, false), (2, true), (3, true), (2, false)],
        [(2, false), (2, false), (3, false), (3, true)],
        [(3, true), (3, false), (3, true), (3, false)],
    ] {
        expect(&verify(&film(&shown, |_, _| {}), &h262()), &[]);
    }
    // 3 + 2 + 3 + 2 fields: four frames, five frame periods
    for tc in [4, 5] {
        let s = stream(Seq::interlaced(), |b| {
            for (tr, top, rff) in [(0u16, true, true), (1, false, false), (2, false, true)] {
                let mut p = Pic::interlaced(if tr == 0 { I } else { P }, tr);
                (p.prog, p.c420, p.rff, p.fpfd, p.tff) = (true, true, rff, true, top);
                b.picture(&p);
            }
            let mut p = Pic::interlaced(P, 3);
            p.tff = true;
            b.picture(&p);
            b.gop(tc, true);
            b.picture(&Pic::interlaced(I, 0));
        });
        expect(&verify(&s, &h262()), &[]);
    }
    // a new sequence may start on either parity (7.12)
    let mut two = film(&[(3, true), (2, false), (3, false), (3, true)], |_, _| {});
    two.extend(film(
        &[(2, true), (2, true), (2, true), (2, true)],
        |_, _| {},
    ));
    expect(&verify(&two, &h262()), &[]);
    // a progressive sequence at 59.94 Hz with frames shown one, two and three times
    let seq = Seq {
        frc: 7,
        pli: 0x44,
        ..Seq::default()
    };
    let rep = ipbb(seq, |i, p| {
        (p.rff, p.tff) = [(true, true), (true, false), (false, false), (true, true)][i];
    });
    expect(&verify(&rep, &h262()), &[]);
}

#[test]
fn every_case_reports_exactly_its_rule() {
    let mut failures = Vec::new();
    for c in cases() {
        let r = verify(&c.data, &c.opts);
        let mut want = c.rules.clone();
        want.sort();
        if r.rules() != want {
            let w: Vec<&str> = want.iter().map(|x| x.info().id).collect();
            failures.push(format!(
                "{}: expected {:?}, got {:?}\n{}",
                c.name,
                w,
                common::ids(&r),
                r.details()
            ));
        }
        // every finding carries a position inside the stream and a message
        for f in &r.findings {
            assert!(
                f.position.byte <= c.data.len() as u64,
                "{}: position",
                c.name
            );
            assert!(!f.message.is_empty(), "{}: message", c.name);
        }
    }
    assert!(
        failures.is_empty(),
        "{} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The rules of the catalog that are marked as checked but have no detecting case here.
const UNCOVERED: &[&str] = &[];

#[test]
fn coverage() {
    let covered: std::collections::BTreeSet<Rule> =
        cases().iter().flat_map(|c| c.rules.clone()).collect();
    let missing: Vec<&str> = ALL_RULES
        .iter()
        .filter(|r| r.info().check == Check::Yes && !covered.contains(r))
        .map(|r| r.info().id)
        .collect();
    assert_eq!(missing, UNCOVERED, "rules without a detecting case");
}
