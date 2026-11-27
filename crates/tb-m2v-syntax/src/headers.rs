//! Headers of one access unit and the slices of a repeated picture.
//!
//! Ports the C++ `Batch::headers` and `Batch::skip_slices`. The inputs are plain values; nothing
//! here knows about sessions or batches.

use crate::profile::{SeqParams, time_code_base};
use crate::{BitWriter, PicType, vlc};

/// What the sequence header, sequence extension and sequence display extension say
/// (6.2.2.1, 6.2.2.3, 6.2.2.4). Constant for the life of a session: a repeated sequence header
/// must equal the first (6.1.1.6). The coded values are those [`crate::profile::check`]
/// derived, so nothing here can be out of range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqInfo {
    /// Picture width in samples (not the padded width).
    pub width: u32,
    /// Picture height in lines.
    pub height: u32,
    /// `false`: progressive_sequence = 1.
    pub interlaced: bool,
    /// The checked values: frame_rate_code, aspect_ratio_information, level, bit_rate,
    /// vbv_buffer_size, color description.
    pub par: SeqParams,
}

/// What the picture header and picture coding extension say (6.2.3, 6.2.3.1), and the time code
/// of a GOP header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PicInfo {
    /// picture_coding_type.
    pub pic_type: PicType,
    /// Display index of the picture in the session (temporal_reference counts coded frames,
    /// 6.3.9: a frame shown for three fields is still one).
    pub disp: u64,
    /// Display time of the picture's first field (or frame) in frame periods of the sequence,
    /// rounded down: what the GOP time code says. Equal to `disp` while every picture is shown
    /// for one frame period.
    pub time: u64,
    /// Display index of the key frame that started the current GOP
    /// (temporal_reference = `disp - tref_base`, modulo 1024).
    pub tref_base: u64,
    /// f_code for all four entries that the picture type uses (the others are 15).
    pub f_code: u8,
    /// top_field_first. Interlaced sequence: the first field output is the top field.
    /// Progressive sequence: with `rff`, the frame is output three times instead of twice;
    /// written as 0 without `rff` (6.3.10, p.49).
    pub tff: bool,
    /// repeat_first_field. Interlaced sequence: the picture is output for three fields (first,
    /// second, first again); written only on a picture whose progressive_frame is written as
    /// 1 (6.3.10, p.50: with progressive_frame 0 it shall be 0). Progressive sequence: the frame
    /// is output two or three times. Whether the sequence may use it at all (Table 8-7) is the
    /// caller's check: [`crate::profile::repeat_first_field_allowed`].
    pub rff: bool,
    /// frame_pred_frame_dct: the picture has no field tools (`Geo::interlaced == 0`, or a
    /// picture flagged progressive in an interlaced session). Must be true when
    /// `progressive_frame` is (A/53 Part 4 6.1.6, SCTE 43 7.1.6): [`write_headers`] writes
    /// progressive_frame = 1 in an interlaced sequence only with it.
    pub frame_pred_frame_dct: bool,
    /// intra_vlc_format.
    pub intra_vlc_format: bool,
    /// alternate_scan.
    pub alt_scan: bool,
    /// progressive_frame in an interlaced sequence: both fields are from one instant.
    pub progressive_frame: bool,
}

/// Most `cc_data` triplets one picture carries (cc_count is 5 bits, A/53 Part 4 6.2.3).
pub const MAX_CC_TRIPLETS: usize = 31;

/// The time code of a GOP header (6.3.8, Table 6-11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeCode {
    /// drop_frame_flag.
    pub drop_frame: bool,
    /// time_code_hours, 0..=23.
    pub hours: u32,
    /// time_code_minutes, 0..=59.
    pub minutes: u32,
    /// time_code_seconds, 0..=59.
    pub seconds: u32,
    /// time_code_pictures, 0..=59.
    pub pictures: u32,
}

impl TimeCode {
    /// The time code of the picture with display index `disp` of a sequence that started at
    /// 00:00:00:00 (6.3.8, p.46). Pictures are counted "assuming rounding to the nearest
    /// integral number of pictures per second". At 29.97 Hz, and only there ("It may be set to
    /// '1' only if the frame rate is 29.97 Hz"), drop_frame_flag is 1 and picture numbers 0 and
    /// 1 are omitted at the start of each minute except minutes 0, 10, 20, 30, 40 and 50, which
    /// keeps the time code on the clock. At 23.976 and 59.94 Hz the flag must be 0 and the
    /// time code runs 0.1 % slow, as the standard has it. Hours wrap at 24.
    pub fn of(disp: u64, frame_rate_code: u8) -> Self {
        let base = time_code_base(frame_rate_code);
        let drop_frame = frame_rate_code == 4;
        let mut n = disp;
        if drop_frame {
            // 17982 pictures in ten minutes (10 x 1800 - 9 x 2), 1798 in a minute that drops.
            let (tens, rest) = (n / 17982, n % 17982);
            n += 18 * tens
                + if rest >= 2 {
                    2 * ((rest - 2) / 1798)
                } else {
                    0
                };
        }
        let sec = n / base;
        Self {
            drop_frame,
            hours: ((sec / 3600) % 24) as u32,
            minutes: ((sec / 60) % 60) as u32,
            seconds: (sec % 60) as u32,
            pictures: (n % base) as u32,
        }
    }
}

/// Append the headers of one access unit to `out`: optionally the sequence headers, optionally
/// a GOP header (closed_gop = 1), the picture header and coding extension, and, when `cc` is not
/// empty, ATSC A/53 picture user data with these `cc_data` triplets (3 bytes each, at most 31).
/// Ends byte-aligned; the slices follow.
pub fn write_headers(
    out: &mut Vec<u8>,
    seq: &SeqInfo,
    pic: &PicInfo,
    with_seq: bool,
    with_gop: bool,
    cc: &[u8],
) {
    write_headers_afd(out, seq, pic, with_seq, with_gop, cc, None);
}

/// [`write_headers`], and when `afd` is given, the picture's Active Format Description as A/53
/// Part 4 picture user data after the captions (`'DTG1'`, afd_data() with active_format_flag
/// set; §6.2.4, Table 6.13; the order of SCTE 43 §7.3). Written in every picture it is given
/// for, so it always follows a sequence header before the next picture data, as §6.2.4
/// requires. A boxed picture's bars follow as `'GA94'` bar_data() (§6.2.3.2, Table 6.11).
/// `afd` must be one this crate writes ([`crate::afd::ActiveFormat::is_written`]).
pub fn write_headers_afd(
    out: &mut Vec<u8>,
    seq: &SeqInfo,
    pic: &PicInfo,
    with_seq: bool,
    with_gop: bool,
    cc: &[u8],
    afd: Option<crate::afd::ActiveFormat>,
) {
    let mut b = BitWriter::new(out);
    let (w, h) = (seq.width, seq.height);
    let par = &seq.par;
    if with_seq {
        // bit_rate is the ceiling of the delivery rate and never changes (6.1.1.6); vbv_delay is
        // 0xFFFF in every picture (6.3.9, C.3.2). Every value was checked when the session
        // opened (profile::check): the size fields are not zero, the extensions are zero.
        b.start(0xB3); // sequence_header (6.2.2.1)
        b.put(w & 0xfff, 12);
        b.put(h & 0xfff, 12);
        b.put(u32::from(par.aspect), 4);
        b.put(u32::from(par.frame_rate_code), 4);
        b.put(par.bit_rate_value & 0x3ffff, 18);
        b.put(1, 1);
        b.put(u32::from(par.vbv_buffer_size) & 0x3ff, 10);
        b.put(0, 1); // constrained_parameters_flag
        b.put(0, 1); // load_intra_quantiser_matrix
        b.put(0, 1); // load_non_intra_quantiser_matrix
        b.start(0xB5); // sequence_extension (6.2.2.3)
        b.put(1, 4);
        b.put(u32::from(par.level.indication()), 8); // Main Profile at the lowest level
        b.put(u32::from(!seq.interlaced), 1); // progressive_sequence
        b.put(1, 2); // chroma_format 4:2:0
        b.put(0, 2); // horizontal_size_extension (A/53 Part 4 Table 6.3: '00')
        b.put(0, 2); // vertical_size_extension
        b.put(0, 12); // bit_rate_extension
        b.put(1, 1);
        b.put(0, 8); // vbv_buffer_size_extension
        b.put(0, 1); // low_delay
        b.put(0, 2); // frame_rate_extension_n
        b.put(0, 5); // frame_rate_extension_d
        b.start(0xB5); // sequence_display_extension (6.2.2.4): color description
        b.put(2, 4);
        b.put(0, 3); // video_format (A/53 Part 4 6.1.4 wants it explicit)
        b.put(1, 1);
        b.put(u32::from(par.color.primaries), 8);
        b.put(u32::from(par.color.transfer), 8);
        b.put(u32::from(par.color.matrix), 8);
        b.put(w, 14);
        b.put(1, 1);
        b.put(h, 14);
    }
    if with_gop {
        b.start(0xB8); // group_of_pictures_header (6.2.2.6)
        let tc = TimeCode::of(pic.time, par.frame_rate_code);
        b.put(u32::from(tc.drop_frame), 1); // drop_frame_flag
        b.put(tc.hours, 5);
        b.put(tc.minutes, 6);
        b.put(1, 1); // marker_bit
        b.put(tc.seconds, 6);
        b.put(tc.pictures, 6);
        b.put(1, 1); // closed_gop
        b.put(0, 1); // broken_link
    }
    b.start(0x00); // picture_header (6.2.3)
    b.put((pic.disp.wrapping_sub(pic.tref_base) & 1023) as u32, 10); // temporal_reference
    b.put(u32::from(pic.pic_type.code()), 3);
    b.put(0xffff, 16); // vbv_delay
    if pic.pic_type != PicType::I {
        b.put(0, 1); // full_pel_forward_vector
        b.put(7, 3); // forward_f_code
    }
    if pic.pic_type == PicType::B {
        b.put(0, 1); // full_pel_backward_vector
        b.put(7, 3); // backward_f_code
    }
    b.put(0, 1); // extra_bit_picture
    b.start(0xB5); // picture_coding_extension (6.2.3.1)
    b.put(8, 4);
    let ff = if pic.pic_type != PicType::I {
        u32::from(pic.f_code)
    } else {
        15
    };
    let fb = if pic.pic_type == PicType::B {
        u32::from(pic.f_code)
    } else {
        15
    };
    b.put(ff, 4);
    b.put(ff, 4);
    b.put(fb, 4);
    b.put(fb, 4);
    b.put(0, 2); // intra_dc_precision: 8 bit
    b.put(3, 2); // picture_structure: frame picture
    // progressive_frame = 1 only with frame_pred_frame_dct = 1 (A/53 Part 4 6.1.6, p.11; SCTE 43
    // 7.1.6, p.4): a caller that flags a picture progressive but codes it with field tools gets
    // the truthful, legal header (progressive_frame = 0).
    let prog = !seq.interlaced || (pic.progressive_frame && pic.frame_pred_frame_dct);
    // repeat_first_field needs progressive_frame = 1 (6.3.10, p.50); in a progressive sequence
    // top_field_first is 0 without it (p.49). Whatever the caller passes, the three flags
    // written agree with each other.
    let rff = pic.rff && prog;
    let tff = if seq.interlaced {
        pic.tff
    } else {
        pic.tff && rff
    };
    b.put(u32::from(tff), 1); // top_field_first
    b.put(u32::from(pic.frame_pred_frame_dct), 1);
    b.put(0, 1); // concealment_motion_vectors
    b.put(1, 1); // q_scale_type = 1 (Table 7-6)
    b.put(u32::from(pic.intra_vlc_format), 1);
    b.put(u32::from(pic.alt_scan), 1);
    b.put(u32::from(rff), 1); // repeat_first_field
    let prog = u32::from(prog);
    b.put(prog, 1); // chroma_420_type = progressive_frame
    b.put(prog, 1); // progressive_frame (1 in an interlaced sequence: both fields are one instant)
    b.put(0, 1); // composite_display_flag
    if !cc.is_empty() {
        // A/53 Part 4 6.2: picture user data, ATSC_identifier 'GA94', user_data_type_code 0x03,
        // cc_data(). The C++ caps the triplets at submission; the cap is applied here as well.
        let n = (cc.len() / 3).min(MAX_CC_TRIPLETS);
        let data = if cc.len() / 3 > MAX_CC_TRIPLETS {
            &cc[..3 * MAX_CC_TRIPLETS]
        } else {
            cc
        };
        b.start(0xB2);
        b.put(0x4741_3934, 32);
        b.put(0x03, 8);
        b.put(0xC0 | n as u32, 8); // reserved 1, process_cc_data_flag 1, zero_bit, cc_count
        b.put(0xFF, 8); // reserved
        for &v in data {
            b.put(u32::from(v), 8);
        }
        b.put(0xFF, 8); // marker_bits
    }
    if let Some(af) = afd {
        debug_assert!(af.is_written(), "active format {af:?}");
        b.start(0xB2);
        b.put(0x4454_4731, 32); // 'DTG1'
        b.put(0x41, 8); // zero '0', active_format_flag '1', reserved '00 0001'
        b.put(0xF0 | u32::from(af.afd & 0x0F), 8); // reserved '1111', active_format
        if let Some(bars) = af.bars {
            use crate::afd::Bars;
            b.start(0xB2);
            b.put(0x4741_3934, 32); // 'GA94'
            b.put(0x06, 8); // user_data_type_code: bar_data()
            let (flags, v1, v2) = match bars {
                Bars::TopBottom {
                    top_end,
                    bottom_start,
                } => (0b1100, top_end, bottom_start),
                Bars::LeftRight {
                    left_end,
                    right_start,
                } => (0b0011, left_end, right_start),
            };
            // top, bottom, left, right flags; reserved '1111'
            b.put((flags << 4) | 0x0F, 8);
            for v in [v1, v2] {
                b.put(0xC000 | u32::from(v & 0x3FFF), 16); // one_bits '11', 14-bit value
            }
        }
    }
    b.align();
}

/// Append the slices of a picture in which every macroblock repeats the co-located one of the
/// forward reference: per row the first and the last macroblock are coded with a zero vector
/// and no coefficients, the rest are skipped. `pic_type` is P or B. `frame_motion_type` is
/// written when `frame_pred_frame_dct` is false.
pub fn write_skip_slices(
    out: &mut Vec<u8>,
    mbw: u32,
    mbh: u32,
    pic_type: PicType,
    frame_pred_frame_dct: bool,
) {
    let mut b = BitWriter::new(out);
    for r in 0..mbh {
        b.start((r + 1) as u8); // slice_start_code: slice_vertical_position
        b.put(8, 5); // quantiser_scale_code
        b.put(0, 1); // extra_bit_slice
        let coded = if mbw > 1 { 2 } else { 1 };
        for k in 0..coded {
            let mut inc = if k == 0 { 1 } else { mbw - 1 };
            while inc > 33 {
                b.put(0x08, 11); // macroblock_escape
                inc -= 33;
            }
            let c = vlc::MBAI[inc as usize];
            b.put(u32::from(c.code), u32::from(c.len));
            if pic_type == PicType::P {
                b.put(1, 3); // macroblock_type: MC, Not Coded (Table B.3)
            } else {
                b.put(2, 4); // macroblock_type: Forward, Not Coded (Table B.4)
            }
            if !frame_pred_frame_dct {
                b.put(2, 2); // frame_motion_type: frame-based
            }
            b.put(1, 1); // motion_code[0][0][0] = 0
            b.put(1, 1); // motion_code[0][0][1] = 0
        }
        b.align();
    }
}

/// sequence_end_code, which the caller appends after the last picture of a closed session.
pub const SEQUENCE_END: [u8; 4] = [0, 0, 1, 0xB7];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tb_golden::Record;
    use tb_kernel_abi::Geo;

    use super::*;
    use crate::profile::{Color, Profile, SeqConfig, check};

    fn pic_type(code: i64) -> PicType {
        match code {
            1 => PicType::I,
            2 => PicType::P,
            3 => PicType::B,
            _ => panic!("picture type {code}"),
        }
    }

    /// The static inputs of every session of a case: its `open` record and its batch's `Geo`.
    struct Case {
        recs: Vec<Record>,
        open: HashMap<i64, Record>,
        geo: HashMap<i64, Geo>,
    }

    impl Case {
        fn load(name: &str) -> Self {
            let recs = tb_golden::case(name).unwrap();
            let mut open = HashMap::new();
            let mut geo = HashMap::new();
            for r in &recs {
                match r.kind() {
                    "open" if r.text("error").unwrap().is_empty() => {
                        open.insert(r.int("s").unwrap(), r.clone());
                    }
                    "batch" => {
                        geo.insert(r.int("b").unwrap(), r.pods::<Geo>("geo").unwrap()[0]);
                    }
                    _ => {}
                }
            }
            Self { recs, open, geo }
        }

        fn session(&self, s: i64) -> (&Record, Geo) {
            let o = &self.open[&s];
            (o, self.geo[&o.int("b").unwrap()])
        }

        /// `write_headers` for one `hdr` record.
        fn headers(&self, h: &Record) -> Vec<u8> {
            let (o, g) = self.session(h.int("s").unwrap());
            let int = |r: &Record, f: &str| r.int(f).unwrap();
            let (width, height) = (int(o, "w") as u32, int(o, "h") as u32);
            let interlaced = o.flag("interlaced").unwrap();
            let color = o.ints("color").unwrap();
            let cfg = SeqConfig {
                width,
                height,
                fps_n: int(o, "fps_n") as u32,
                fps_d: int(o, "fps_d") as u32,
                interlaced,
                aspect: int(o, "aspect") as u8,
                max_bitrate: int(o, "max_bitrate") as u64,
                vbv_bits: int(o, "vbv_bits") as u64,
                color: (!color.is_empty()).then(|| Color {
                    primaries: color[0] as u8,
                    transfer: color[1] as u8,
                    matrix: color[2] as u8,
                }),
            };
            let profile = Profile::from_name(o.text("profile").unwrap()).unwrap();
            let seq = SeqInfo {
                width,
                height,
                interlaced,
                par: check(&cfg, profile).unwrap(),
            };
            let pic = PicInfo {
                pic_type: pic_type(int(h, "type")),
                disp: int(h, "disp") as u64,
                // records of pictures shown for other than one frame period say so
                time: h.int("time").map_or(int(h, "disp") as u64, |t| t as u64),
                tref_base: int(h, "tref_base") as u64,
                f_code: g.f_code as u8,
                tff: h.flag("tff").unwrap_or(o.flag("tff").unwrap()),
                rff: h.flag("rff").unwrap_or(false),
                // a frame flagged progressive is coded without field tools, in the zigzag scan
                // unless the session asked for the alternate one
                frame_pred_frame_dct: g.interlaced == 0 || h.flag("prog").unwrap(),
                intra_vlc_format: g.intra_vlc_format != 0,
                alt_scan: g.alt_scan != 0
                    && !(g.interlaced != 0
                        && h.flag("prog").unwrap()
                        && o.int("alt_scan").unwrap() < 0),
                progressive_frame: h.flag("prog").unwrap(),
            };
            let mut out = Vec::new();
            write_headers(
                &mut out,
                &seq,
                &pic,
                h.flag("seq_hdr").unwrap(),
                h.flag("gop_hdr").unwrap(),
                &h.bytes("cc").unwrap(),
            );
            out
        }
    }

    /// Acceptance 2: every `hdr` record of the golden cases.
    #[test]
    fn headers_equal_the_records() {
        for name in [
            "cbr",
            "ladder",
            "fixedq",
            "interlaced",
            "shed",
            "skip",
            "deadline",
            "churn",
            "devin",
            "twofmt",
            "film",
            "film60",
        ] {
            let c = Case::load(name);
            let mut n = 0;
            for h in c.recs.iter().filter(|r| r.kind() == "hdr") {
                assert_eq!(
                    c.headers(h),
                    h.bytes("bytes").unwrap(),
                    "{name}: s {} disp {}",
                    h.int("s").unwrap(),
                    h.int("disp").unwrap()
                );
                n += 1;
            }
            assert!(n > 0, "{name}: no hdr records");
        }
    }

    /// Acceptance 3: the repeated pictures of `shed` are their headers and the skip slices.
    #[test]
    fn repeats_equal_the_records() {
        let c = Case::load("shed");
        let mut last_hdr: HashMap<i64, &Record> = HashMap::new();
        let mut n = 0;
        for r in &c.recs {
            match r.kind() {
                "hdr" => {
                    last_hdr.insert(r.int("s").unwrap(), r);
                }
                "au" if r.int("repeat").unwrap() == 1 => {
                    let s = r.int("s").unwrap();
                    let h = last_hdr[&s];
                    assert_eq!(h.int("disp").unwrap(), r.int("disp").unwrap());
                    let (_, g) = c.session(s);
                    let mut want = c.headers(h);
                    assert_eq!(want, h.bytes("bytes").unwrap());
                    write_skip_slices(
                        &mut want,
                        g.mbw as u32,
                        g.mbh as u32,
                        pic_type(r.int("type").unwrap()),
                        g.interlaced == 0,
                    );
                    assert_eq!(want, r.bytes("data").unwrap(), "s {s} au {r:?}");
                    n += 1;
                }
                _ => {}
            }
        }
        assert!(n > 0, "shed has no repeated pictures");
    }

    /// A progressive sequence at Main level's maxima, checked under `h262`.
    fn seq(width: u32, height: u32, fps_n: u32, fps_d: u32) -> SeqInfo {
        seq_with(width, height, fps_n, fps_d, 0)
    }

    fn seq_with(width: u32, height: u32, fps_n: u32, fps_d: u32, aspect: u8) -> SeqInfo {
        let cfg = SeqConfig {
            width,
            height,
            fps_n,
            fps_d,
            interlaced: false,
            aspect,
            max_bitrate: 15_000_000,
            vbv_bits: 1_835_008,
            color: None,
        };
        SeqInfo {
            width,
            height,
            interlaced: false,
            par: check(&cfg, Profile::H262).unwrap(),
        }
    }

    fn pic(pic_type: PicType, disp: u64) -> PicInfo {
        PicInfo {
            pic_type,
            disp,
            time: disp,
            tref_base: 0,
            f_code: 3,
            tff: false,
            rff: false,
            frame_pred_frame_dct: true,
            intra_vlc_format: false,
            alt_scan: false,
            progressive_frame: true,
        }
    }

    /// Reads bits most significant first.
    struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl Reader<'_> {
        fn get(&mut self, len: usize) -> u32 {
            let mut v = 0;
            for _ in 0..len {
                let bit = (self.data[self.pos / 8] >> (7 - self.pos % 8)) & 1;
                v = (v << 1) | u32::from(bit);
                self.pos += 1;
            }
            v
        }
    }

    fn sequence_header(s: &SeqInfo) -> Vec<u8> {
        let mut out = Vec::new();
        write_headers(&mut out, s, &pic(PicType::I, 0), true, false, &[]);
        out
    }

    /// The 4-bit aspect and frame-rate codes after the 24 bits of size (6.2.2.1).
    fn aspect_and_rate(s: &SeqInfo) -> (u32, u32) {
        let out = sequence_header(s);
        assert_eq!(out[..4], [0, 0, 1, 0xB3]);
        let mut r = Reader {
            data: &out[4..],
            pos: 24,
        };
        (r.get(4), r.get(4))
    }

    #[test]
    fn aspect_selection() {
        assert_eq!(aspect_and_rate(&seq(720, 480, 30000, 1001)).0, 2);
        assert_eq!(aspect_and_rate(&seq(704, 480, 30000, 1001)).0, 2);
        assert_eq!(aspect_and_rate(&seq(640, 480, 30000, 1001)).0, 2);
        assert_eq!(aspect_and_rate(&seq(720, 576, 25, 1)).0, 3);
        assert_eq!(aspect_and_rate(&seq(1920, 1080, 30000, 1001)).0, 3);
        assert_eq!(aspect_and_rate(&seq(176, 144, 30000, 1001)).0, 3);
        let s = seq_with(720, 480, 30000, 1001, 3);
        assert_eq!(aspect_and_rate(&s).0, 3);
        let s = seq_with(1920, 1080, 30000, 1001, 2);
        assert_eq!(aspect_and_rate(&s).0, 2);
    }

    #[test]
    fn frame_rate_codes() {
        for (n, d, code) in [
            (30000, 1001, 4),
            (60000, 1001, 7),
            (25, 1, 3),
            (50, 1, 6),
            (30, 1, 5),
            (60, 1, 8),
            (24000, 1001, 1),
            (24, 1, 2),
            (50000, 1000, 6),
            (25000, 1000, 3),
        ] {
            assert_eq!(aspect_and_rate(&seq(720, 480, n, d)).1, code, "{n}/{d}");
        }
    }

    /// A 720x480 sequence header at Main Level with the level's maxima (fixed quantizer), bit
    /// for bit from 6.2.2.1 and 6.2.2.3.
    #[test]
    fn sequence_header_720x480() {
        let out = sequence_header(&seq(720, 480, 30000, 1001));
        let mut r = Reader { data: &out, pos: 0 };
        assert_eq!(r.get(32), 0x1B3);
        assert_eq!(r.get(12), 720);
        assert_eq!(r.get(12), 480);
        assert_eq!(r.get(4), 2);
        assert_eq!(r.get(4), 4);
        assert_eq!(r.get(18), 15_000_000 / 400);
        assert_eq!(r.get(1), 1);
        assert_eq!(r.get(10), 1_835_008 / 16384);
        assert_eq!(r.get(3), 0);
        r.pos = r.pos.div_ceil(8) * 8;
        assert_eq!(r.get(32), 0x1B5);
        assert_eq!(r.get(4), 1);
        assert_eq!(r.get(8), 0x48);
        assert_eq!(r.get(1), 1); // progressive_sequence
        // 480 lines at 59.94 Hz: High-1440 level (20.7 Msample/s, Table 8-12), not High.
        let out = sequence_header(&seq(720, 480, 60000, 1001));
        assert_eq!(out[16], 0x14);
        assert_eq!(out[17] >> 4, 0x6);
        // 1280x720 at 59.94 Hz needs High level, at 29.97 Hz High-1440.
        let level = |s: &SeqInfo| {
            let out = sequence_header(s);
            ((out[16] & 0xf) << 4) | (out[17] >> 4)
        };
        assert_eq!(level(&seq(1280, 720, 60000, 1001)), 0x44);
        assert_eq!(level(&seq(1280, 720, 30000, 1001)), 0x46);
        assert_eq!(level(&seq(1920, 1080, 24000, 1001)), 0x44);
        assert_eq!(level(&seq(176, 144, 24, 1)), 0x48);
    }

    /// The color description is the session's (what its converter produced), not a guess
    /// from the height: an SD session can say BT.709 (which SCTE 43 7.1.4 prefers), and without
    /// one the default stands (BT.709 above 576 lines, SMPTE 170M otherwise).
    #[test]
    fn color_description_from_the_session() {
        let triple = |height: u32, color: Option<Color>| {
            let cfg = SeqConfig {
                width: 704,
                height,
                fps_n: 30000,
                fps_d: 1001,
                interlaced: false,
                aspect: 0,
                max_bitrate: 4_000_000,
                vbv_bits: 1_835_008,
                color,
            };
            let s = SeqInfo {
                width: 704,
                height,
                interlaced: false,
                par: check(&cfg, Profile::H262).unwrap(),
            };
            let out = sequence_header(&s);
            // sequence_display_extension: the second extension start code; id 2, video_format
            // 000, colour_description 1, then the three codes
            let at = out
                .windows(4)
                .enumerate()
                .filter(|(_, w)| *w == [0, 0, 1, 0xB5])
                .nth(1)
                .unwrap()
                .0;
            assert_eq!(out[at + 4], 0x21);
            [out[at + 5], out[at + 6], out[at + 7]]
        };
        assert_eq!(triple(480, None), [6, 6, 6]);
        assert_eq!(triple(576, None), [6, 6, 6]);
        assert_eq!(triple(720, None), [1, 1, 1]);
        assert_eq!(triple(480, Some(Color::BT709)), [1, 1, 1]);
        assert_eq!(triple(720, Some(Color::SMPTE170M)), [6, 6, 6]);
        let mixed = Color {
            primaries: 5,
            transfer: 4,
            matrix: 7,
        };
        assert_eq!(triple(480, Some(mixed)), [5, 4, 7]);
    }

    fn gop_time_code(s: &SeqInfo, disp: u64) -> (u32, u32, u32, u32, u32) {
        let mut out = Vec::new();
        write_headers(&mut out, s, &pic(PicType::I, disp), false, true, &[]);
        let mut r = Reader { data: &out, pos: 0 };
        assert_eq!(r.get(32), 0x1B8);
        let drop = r.get(1);
        let h = r.get(5);
        let m = r.get(6);
        assert_eq!(r.get(1), 1);
        let sec = r.get(6);
        let pics = r.get(6);
        assert_eq!(r.get(2), 0b10);
        (drop, h, m, sec, pics)
    }

    /// 6.3.8: drop_frame_flag 1 at 29.97 Hz, where picture numbers 0 and 1 are left out at the
    /// start of every minute but each tenth; the time code then follows the clock.
    #[test]
    fn gop_time_code_drop_frame() {
        let s = seq(720, 480, 30000, 1001);
        assert_eq!(gop_time_code(&s, 0), (1, 0, 0, 0, 0));
        assert_eq!(gop_time_code(&s, 29), (1, 0, 0, 0, 29));
        assert_eq!(gop_time_code(&s, 1799), (1, 0, 0, 59, 29));
        // minute 1 starts at picture number 2
        assert_eq!(gop_time_code(&s, 1800), (1, 0, 1, 0, 2));
        assert_eq!(gop_time_code(&s, 1800 + 1797), (1, 0, 1, 59, 29));
        assert_eq!(gop_time_code(&s, 1800 + 1798), (1, 0, 2, 0, 2));
        // minute 10 keeps pictures 0 and 1
        assert_eq!(gop_time_code(&s, 17982), (1, 0, 10, 0, 0));
        assert_eq!(gop_time_code(&s, 17982 + 1800), (1, 0, 11, 0, 2));
        // one hour of pictures at 30000/1001 is 107 892: the time code says one hour
        assert_eq!(gop_time_code(&s, 107_892), (1, 1, 0, 0, 0));
        // every time code of a day is a legal one: never picture 0 or 1 in a dropped minute,
        // strictly ascending, and 24 hours of clock take 24 hours of time code
        let mut last = None;
        for disp in 0..24 * 107_892u64 {
            let t = TimeCode::of(disp, 4);
            assert!(t.drop_frame && t.hours < 24 && t.minutes < 60 && t.seconds < 60);
            assert!(t.pictures < 30);
            assert!(!(t.seconds == 0 && t.pictures < 2 && !t.minutes.is_multiple_of(10)));
            let v = ((t.hours * 60 + t.minutes) * 60 + t.seconds) * 30 + t.pictures;
            assert!(last.is_none_or(|l| l < v), "disp {disp}");
            last = Some(v);
        }
        assert_eq!(TimeCode::of(24 * 107_892, 4).hours, 0);
    }

    /// 6.3.8: at every other rate the flag is 0 (it "may be set to '1' only if the frame rate is
    /// 29.97 Hz") and pictures are counted at the rounded rate.
    #[test]
    fn gop_time_code_plain() {
        let d = (3600 + 2 * 60 + 3) * 30 + 7;
        assert_eq!(gop_time_code(&seq(720, 480, 30, 1), d), (0, 1, 2, 3, 7));
        for (n, den, base) in [
            (24000, 1001, 24),
            (24, 1, 24),
            (25, 1, 25),
            (50, 1, 50),
            (60000, 1001, 60),
            (60, 1, 60),
        ] {
            let s = seq(704, 480, n, den);
            let d = (3600 + 2 * 60 + 3) * base + base - 1;
            assert_eq!(
                gop_time_code(&s, d),
                (0, 1, 2, 3, base as u32 - 1),
                "{n}/{den}"
            );
            assert_eq!(gop_time_code(&s, d + 1), (0, 1, 2, 4, 0), "{n}/{den}");
        }
    }

    /// A/53 Part 4 6.1.6 and SCTE 43 7.1.6: progressive_frame = 1 only with
    /// frame_pred_frame_dct = 1. The picture coding extension of an interlaced sequence says
    /// progressive only for a picture coded without field tools.
    #[test]
    fn progressive_frame_needs_frame_pred_frame_dct() {
        let flags = |interlaced: bool, fpfd: bool, prog: bool| {
            let mut s = seq(704, 480, 30000, 1001);
            s.interlaced = interlaced;
            let mut p = pic(PicType::P, 1);
            p.frame_pred_frame_dct = fpfd;
            p.progressive_frame = prog;
            let mut out = Vec::new();
            write_headers(&mut out, &s, &p, false, false, &[]);
            let at = out.windows(4).position(|w| w == [0, 0, 1, 0xB5]).unwrap();
            // frame_pred_frame_dct, chroma_420_type, progressive_frame
            (
                out[at + 7] & 0x40 != 0,
                out[at + 7] & 0x01 != 0,
                out[at + 8] & 0x80 != 0,
            )
        };
        assert_eq!(flags(true, true, true), (true, true, true));
        assert_eq!(flags(true, true, false), (true, false, false));
        assert_eq!(flags(true, false, false), (false, false, false));
        // field tools in use: the picture is not announced as progressive
        assert_eq!(flags(true, false, true), (false, false, false));
        // a progressive sequence: always 1, 1
        assert_eq!(flags(false, true, false), (true, true, true));
    }

    fn user_data(cc: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_headers(
            &mut out,
            &seq(720, 480, 30000, 1001),
            &pic(PicType::P, 1),
            false,
            false,
            cc,
        );
        let at = out.windows(4).position(|w| w == [0, 0, 1, 0xB2]).unwrap();
        out[at..].to_vec()
    }

    /// AFD follows the captions as its own user_data(), or stands alone without them.
    #[test]
    fn active_format_user_data() {
        let ud = |cc: &[u8], afd| {
            let mut out = Vec::new();
            write_headers_afd(
                &mut out,
                &seq(720, 480, 30000, 1001),
                &pic(PicType::P, 1),
                false,
                false,
                cc,
                afd,
            );
            let at = out.windows(4).position(|w| w == [0, 0, 1, 0xB2]);
            at.map(|a| out[a..].to_vec()).unwrap_or_default()
        };
        use crate::afd::{ActiveFormat, Bars, LETTER_16_9, WIDER};
        let letter = ActiveFormat {
            afd: LETTER_16_9,
            bars: None,
        };
        let dtg1 = [0, 0, 1, 0xB2, 0x44, 0x54, 0x47, 0x31, 0x41, 0xFA];
        assert_eq!(ud(&[], Some(letter)), dtg1);
        // AFD 0100 with the bars of ST 2016-1 Table B.3: lines 653 and 491
        let wide = ActiveFormat {
            afd: WIDER,
            bars: Some(Bars::TopBottom {
                top_end: 653,
                bottom_start: 491,
            }),
        };
        let mut want = vec![0, 0, 1, 0xB2, 0x44, 0x54, 0x47, 0x31, 0x41, 0xF4];
        want.extend_from_slice(&[0, 0, 1, 0xB2, 0x47, 0x41, 0x39, 0x34, 0x06, 0xCF]);
        want.extend_from_slice(&[0xC2, 0x8D, 0xC1, 0xEB]);
        assert_eq!(ud(&[], Some(wide)), want);
        let both = ud(&[0xFC, 0x80, 0x80], Some(letter));
        assert_eq!(both[..15], user_data(&[0xFC, 0x80, 0x80])[..]);
        assert_eq!(both[15..], dtg1);
        // without AFD the output is what write_headers writes
        assert_eq!(
            ud(&[0xFC, 0x80, 0x80], None),
            user_data(&[0xFC, 0x80, 0x80])
        );
        assert!(ud(&[], None).is_empty());
    }

    /// At most 31 triplets: cc_count is 5 bits and the zero_bit stays 0.
    #[test]
    fn caption_limit() {
        let cc: Vec<u8> = (0..32 * 3).map(|i| i as u8).collect();
        let ud = user_data(&cc[..31 * 3]);
        assert_eq!(ud[4..9], [0x47, 0x41, 0x39, 0x34, 0x03]);
        assert_eq!(ud[9], 0xC0 | 31);
        assert_eq!(ud[10], 0xFF);
        assert_eq!(ud[11..11 + 93], cc[..93]);
        assert_eq!(ud[11 + 93..], [0xFF]);
        assert_eq!(user_data(&cc), ud);
        let one = user_data(&[0xFC, 0x80, 0x80]);
        assert_eq!(
            one,
            [
                0, 0, 1, 0xB2, 0x47, 0x41, 0x39, 0x34, 3, 0xC1, 0xFF, 0xFC, 0x80, 0x80, 0xFF
            ]
        );
    }

    /// Wider than 33 macroblocks: the address increment of the last macroblock is written with
    /// macroblock_escape (Table B.1). 1920 / 16 = 120 macroblocks: increment 119 = 3 * 33 + 20.
    #[test]
    fn skip_slices_escape() {
        let mut out = Vec::new();
        write_skip_slices(&mut out, 120, 2, PicType::P, true);
        let mut r = Reader { data: &out, pos: 0 };
        for row in 1..=2 {
            assert_eq!(r.get(32), 0x100 + row);
            assert_eq!(r.get(5), 8);
            assert_eq!(r.get(1), 0);
            assert_eq!(r.get(1), 1); // increment 1
            assert_eq!(r.get(3), 1); // MC, Not Coded
            assert_eq!(r.get(2), 0b11); // two zero motion codes
            for _ in 0..3 {
                assert_eq!(r.get(11), 0x08);
            }
            assert_eq!(r.get(10), 0x13); // increment 20 (Table B.1: 0000 0100 11)
            assert_eq!(r.get(3), 1);
            assert_eq!(r.get(2), 0b11);
            r.pos = r.pos.div_ceil(8) * 8;
        }
        assert_eq!(r.pos, out.len() * 8);
    }

    /// Exactly 33 + 1 macroblocks: one escape and increment 0 is never written; B pictures and
    /// frame_motion_type; a single-macroblock row codes one macroblock.
    #[test]
    fn skip_slices_shapes() {
        let mut out = Vec::new();
        write_skip_slices(&mut out, 35, 1, PicType::B, false);
        let mut r = Reader { data: &out, pos: 0 };
        assert_eq!(r.get(32), 0x101);
        r.get(6);
        assert_eq!(r.get(1), 1);
        assert_eq!(r.get(4), 2); // Forward, Not Coded
        assert_eq!(r.get(2), 2); // frame_motion_type
        assert_eq!(r.get(2), 0b11);
        assert_eq!(r.get(11), 0x08); // 34 = 33 + 1
        assert_eq!(r.get(1), 1);
        assert_eq!(r.get(4), 2);
        assert_eq!(r.get(2), 2);
        assert_eq!(r.get(2), 0b11);

        let mut out = Vec::new();
        write_skip_slices(&mut out, 1, 1, PicType::P, true);
        // quantiser_scale_code 01000, extra_bit_slice 0, increment 1, type 001, motion codes 1 1.
        assert_eq!(out, [0, 0, 1, 1, 0b0100_0010, 0b0111_0000]);
    }
}
