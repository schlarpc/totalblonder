//! The driver: splits the stream at start codes, follows the video_sequence() syntax of
//! H.262 6.2.2, parses headers and extensions, and runs the picture, cross-picture, buffer
//! and profile checks.

use crate::bits::BitReader;
use crate::mb::{self, B, FRAME, I, P, PicParams, PicState};
use crate::report::{Position, Report, Sink};
use crate::rules::Rule;
use crate::tables;
use crate::userdata::{self, CcInfo, UdKind};
use crate::vbv::{self, Delivery, VbvPic, VbvSeq};
use crate::vlc::Tables;

/// The broadcast rule set applied on top of H.262.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// H.262 only.
    H262,
    /// ATSC A/53 Part 4:2009 (terrestrial, 19.4 Mbit/s) with CTA-708 captions.
    A53,
    /// ANSI/SCTE 43 2015 on 64-QAM (27.0 Mbit/s).
    Scte43Qam64,
    /// ANSI/SCTE 43 2015 on 256-QAM (38.8 Mbit/s).
    Scte43Qam256,
}

impl Profile {
    /// Parses the command line name.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "h262-only" | "h262" => Some(Profile::H262),
            "a53" => Some(Profile::A53),
            "scte43-64qam" => Some(Profile::Scte43Qam64),
            "scte43-256qam" => Some(Profile::Scte43Qam256),
            _ => None,
        }
    }

    fn broadcast(self) -> bool {
        self != Profile::H262
    }

    fn scte43(self) -> bool {
        matches!(self, Profile::Scte43Qam64 | Profile::Scte43Qam256)
    }
}

/// What to verify against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Broadcast profile.
    pub profile: Profile,
    /// Delivery model of the buffer check.
    pub delivery: Delivery,
    /// How many findings of each rule to keep with position and message (all are counted).
    pub max_examples: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            profile: Profile::H262,
            delivery: Delivery::Normative,
            max_examples: 8,
        }
    }
}

/// Verifies one elementary stream held in memory.
pub fn verify(data: &[u8], options: &Options) -> Report {
    let mut v = Verifier::new(data, *options);
    v.run();
    v.finish()
}

// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeqHdr {
    hsize: u32,
    vsize: u32,
    aspect: u8,
    frc: u8,
    bit_rate: u32,
    vbv: u32,
    constrained: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeqExt {
    pli: u8,
    progressive: bool,
    chroma: u8,
    hsize_ext: u32,
    vsize_ext: u32,
    bit_rate_ext: u32,
    vbv_ext: u32,
    low_delay: bool,
    fr_n: u32,
    fr_d: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sde {
    video_format: u8,
    color: Option<(u8, u8, u8)>,
    dh: u32,
    dv: u32,
}

/// Level limits of Tables 8-8 and 8-11 to 8-14 for Main and Simple profile.
#[derive(Debug, Clone, Copy)]
struct Level {
    name: &'static str,
    id: u8,
    hsize: u32,
    vsize: u32,
    max_frc: u8,
    sample_rate: u64,
    bit_rate: u64,
    vbv: u64,
    f_h: u8,
    f_v_frame: u8,
    f_v_field: u8,
    /// vertical vector range in half samples, frame and field pictures
    vr_frame: (i32, i32),
    vr_field: (i32, i32),
}

const LEVELS: [Level; 4] = [
    Level {
        name: "Low",
        id: 0b1010,
        hsize: 352,
        vsize: 288,
        max_frc: 5,
        sample_rate: 3_041_280,
        bit_rate: 4_000_000,
        vbv: 475_136,
        f_h: 7,
        f_v_frame: 4,
        f_v_field: 3,
        vr_frame: (-128, 127),
        vr_field: (-64, 63),
    },
    Level {
        name: "Main",
        id: 0b1000,
        hsize: 720,
        vsize: 576,
        max_frc: 5,
        sample_rate: 10_368_000,
        bit_rate: 15_000_000,
        vbv: 1_835_008,
        f_h: 8,
        f_v_frame: 5,
        f_v_field: 4,
        vr_frame: (-256, 255),
        vr_field: (-128, 127),
    },
    Level {
        name: "High-1440",
        id: 0b0110,
        hsize: 1440,
        vsize: 1088,
        max_frc: 8,
        sample_rate: 47_001_600,
        bit_rate: 60_000_000,
        vbv: 7_340_032,
        f_h: 9,
        f_v_frame: 5,
        f_v_field: 4,
        vr_frame: (-256, 255),
        vr_field: (-128, 127),
    },
    Level {
        name: "High",
        id: 0b0100,
        hsize: 1920,
        vsize: 1088,
        max_frc: 8,
        sample_rate: 62_668_800,
        bit_rate: 80_000_000,
        vbv: 9_781_248,
        f_h: 9,
        f_v_frame: 5,
        f_v_field: 4,
        vr_frame: (-256, 255),
        vr_field: (-128, 127),
    },
];

/// frame_rate_value of Table 6-4 as a fraction.
fn frame_rate_value(code: u8) -> Option<(u32, u32)> {
    match code {
        1 => Some((24000, 1001)),
        2 => Some((24, 1)),
        3 => Some((25, 1)),
        4 => Some((30000, 1001)),
        5 => Some((30, 1)),
        6 => Some((50, 1)),
        7 => Some((60000, 1001)),
        8 => Some((60, 1)),
        _ => None,
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// One row of A/53 Part 4 Table 6.2 or SCTE 43 Table 3: vertical, horizontal, aspect codes,
/// progressive frame rate codes, interlaced frame rate codes, and whether A/53 has the row.
type FormatRow = (u32, u32, &'static [u8], &'static [u8], &'static [u8], bool);
const FORMATS: &[FormatRow] = &[
    (1080, 1920, &[1, 3], &[1, 2, 4, 5], &[4, 5], true),
    (1080, 1440, &[3], &[1, 2, 4, 5], &[4, 5], false),
    (720, 1280, &[1, 3], &[1, 2, 4, 5, 7, 8], &[], true),
    (480, 720, &[2, 3], &[1, 2, 4, 5, 7, 8], &[4, 5], false),
    (480, 704, &[2, 3], &[1, 2, 4, 5, 7, 8], &[4, 5], true),
    (480, 640, &[1, 2], &[1, 2, 4, 5, 7, 8], &[4, 5], true),
    (480, 544, &[2], &[1], &[4], false),
    (480, 528, &[2], &[1], &[4], false),
    (480, 352, &[2], &[1], &[4], false),
];

/// Everything known about the current sequence.
#[derive(Debug, Clone)]
struct Seq {
    hdr: SeqHdr,
    ext: SeqExt,
    /// The sequence display extension after the first header (None: there was none).
    sde: Option<Sde>,
    /// True once the first picture header of the sequence was seen.
    started: bool,
    width: u32,
    height: u32,
    mb_w: u32,
    mb_h: u32,
    bit_rate: u64,
    vbv_bits: u64,
    frame_rate: f64,
    /// Main or Simple profile with a defined level.
    level: Option<Level>,
    simple: bool,
    // quantizer matrices in raster order: intra, non-intra, chroma intra, chroma non-intra
    matrices: [[u8; 64]; 4],
    // cross-picture state
    refs: u32,
    seen_ffff: bool,
    seen_delay: bool,
    mix_reported: bool,
    pics: Vec<VbvPic>,
    sde_this_header: bool,
    pictures_since_header: u32,
    seconds_since_header: f64,
    first_picture_after_header: bool,
}

#[derive(Debug, Clone)]
struct Gop {
    drop_frame: bool,
    frames: u64,
    closed: bool,
    broken: bool,
}

/// The picture being decoded.
#[derive(Debug)]
struct Pic {
    idx: u32,
    pos: Position,
    ptype: u8,
    tr: u16,
    params: Option<PicParams>,
    f_code: [[u8; 2]; 2],
    dc_prec: u8,
    structure: u8,
    tff: bool,
    fpfd: bool,
    cmv: bool,
    q_scale_type: bool,
    intra_vlc: bool,
    alt_scan: bool,
    rff: bool,
    progressive_frame: bool,
    have_pce: bool,
    decodable: bool,
    second_field: bool,
    second_field_after_i: bool,
    state: PicState,
    userdata: Vec<UdKind>,
    after_gop: bool,
}

#[derive(Debug, Clone)]
struct FirstField {
    structure: u8,
    ptype: u8,
    tr: u16,
}

/// A coded frame, for display order and what depends on it.
#[derive(Debug, Clone)]
struct Frame {
    pic_idx: u32,
    pos: Position,
    ptype: u8,
    tr: u16,
    gop_id: u32,
    gop: Option<Gop>,
    /// Duration in field periods (of a frame period for progressive sequences: 2, 4 or 6).
    fields: u32,
    frame_picture: bool,
    tff: bool,
    /// Parity of the first field output (interlaced sequences): top.
    first_top: bool,
    cc: Option<CcInfo>,
    has_pic_userdata: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    Init,
    SeqHdr,
    SeqExt,
    Gop,
    PicHdr,
    PicExt,
    Slices,
    End,
}

struct Verifier<'a> {
    data: &'a [u8],
    opts: Options,
    t: Tables,
    sink: Sink,
    st: St,
    seq: Option<Seq>,
    /// The sequence header just read, waiting for its extension.
    pending_hdr: Option<(SeqHdr, [[u8; 64]; 2], Position)>,
    seq_usable: bool,
    pic: Option<Pic>,
    pic_count: u32,
    frame_count: u32,
    ext_seen: u16,
    gop_id: u32,
    pending_gop: Option<Gop>,
    gop_for_next_frame: bool,
    first_field: Option<FirstField>,
    /// The I-frame after a GOP header (or the first of the sequence) and the B-pictures right
    /// after it: (closed_gop, broken_link, had a GOP header).
    leading: Option<(bool, bool, bool)>,
    last_ref_dual_prime: bool,
    pending_ref: Option<Frame>,
    last_displayed: Option<Frame>,
    display_count: u32,
    display_of_pic: Vec<(u32, u32)>,
    /// Time code of the last GOP in frames, the display time of its first frame in field
    /// periods and that frame's display index, drop_frame_flag.
    last_gop_tc: Option<(u64, u64, u32, bool)>,
    /// Field periods output so far in this sequence (two per frame period of a progressive
    /// sequence).
    out_fields: u64,
    /// Parity of the last field output (interlaced sequences): top.
    last_field_top: Option<bool>,
    data_start: Option<u64>,
    after_end_reported: bool,
    seen_cc: bool,
    pics_without_cc: u32,
    cc_gap_reported: bool,
    afd_in_use: bool,
    bar_in_use: bool,
    seqhdr_between_fields: bool,
}

fn next_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let n = data.len();
    while i + 2 < n {
        let b2 = data.get(i + 2).copied().unwrap_or(0xFF);
        if b2 > 1 {
            i += 3;
        } else if b2 == 1 {
            if data.get(i).copied() == Some(0) && data.get(i + 1).copied() == Some(0) {
                return Some(i);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    None
}

impl<'a> Verifier<'a> {
    fn new(data: &'a [u8], opts: Options) -> Self {
        Self {
            data,
            opts,
            t: Tables::new(),
            sink: Sink::new(opts.max_examples),
            st: St::Init,
            seq: None,
            pending_hdr: None,
            seq_usable: false,
            pic: None,
            pic_count: 0,
            frame_count: 0,
            ext_seen: 0,
            gop_id: 0,
            pending_gop: None,
            gop_for_next_frame: false,
            first_field: None,
            leading: None,
            last_ref_dual_prime: false,
            pending_ref: None,
            last_displayed: None,
            display_count: 0,
            display_of_pic: Vec::new(),
            last_gop_tc: None,
            out_fields: 0,
            last_field_top: None,
            data_start: None,
            after_end_reported: false,
            seen_cc: false,
            pics_without_cc: 0,
            cc_gap_reported: false,
            afd_in_use: false,
            bar_in_use: false,
            seqhdr_between_fields: false,
        }
    }

    fn at(&self, byte: usize) -> Position {
        Position {
            byte: byte as u64,
            picture_coded: self.pic.as_ref().map(|p| p.idx),
            ..Position::default()
        }
    }

    fn at_bits(&self, payload_at: usize, bit: usize) -> Position {
        let mut p = self.at(payload_at + (bit >> 3));
        p.bit = (bit & 7) as u8;
        p
    }

    fn emit(&mut self, rule: Rule, pos: Position, message: impl FnOnce() -> String) {
        self.sink.emit(rule, pos, message);
    }

    fn run(&mut self) {
        let data = self.data;
        let Some(first) = next_start_code(data, 0) else {
            if !data.is_empty() {
                self.emit(Rule::StreamStart, Position::default(), || {
                    "the stream contains no start code".to_string()
                });
            }
            return;
        };
        if let Some(i) = data
            .get(..first)
            .and_then(|d| d.iter().position(|&b| b != 0))
        {
            self.emit(Rule::StreamStart, self.at(i), || {
                format!(
                    "{} bytes of non-zero data precede the first start code",
                    first - i
                )
            });
        }
        let mut cur = Some(first);
        while let Some(s) = cur {
            let next = next_start_code(data, s + 3);
            let end = next.unwrap_or(data.len());
            let Some(&code) = data.get(s + 3) else {
                self.emit(Rule::HdrTruncated, self.at(s), || {
                    "the stream ends inside a start code".to_string()
                });
                break;
            };
            let payload = data.get(s + 4..end).unwrap_or(&[]);
            self.unit(code, s, payload);
            cur = next;
        }
        self.end_of_stream();
    }

    fn finish(mut self) -> Report {
        self.sink.report.bytes = self.data.len() as u64;
        self.sink.report.pictures = self.pic_count;
        self.sink.report.frames = self.frame_count;
        // fill in the display numbers
        self.display_of_pic.sort_unstable();
        let map = &self.display_of_pic;
        for f in &mut self.sink.report.findings {
            if let Some(pc) = f.position.picture_coded
                && let Ok(i) = map.binary_search_by_key(&pc, |e| e.0)
            {
                f.position.picture_display = map.get(i).map(|e| e.1);
            }
        }
        self.sink.report
    }

    /// 23 or more zero bits followed by a one inside a unit (not a start code).
    fn scan_emulation(&mut self, start: usize, payload: &[u8]) {
        let mut i = 0usize;
        let n = payload.len();
        while i + 1 < n {
            if payload.get(i).copied() != Some(0) {
                i += 1;
                continue;
            }
            // a run of zero bytes from i
            let mut j = i;
            while j < n && payload.get(j).copied() == Some(0) {
                j += 1;
            }
            if j >= n {
                break; // trailing stuffing before the next start code
            }
            if j - i >= 2 {
                let before = if i == 0 {
                    self.data.get(start + 3).map_or(0, |b| b.trailing_zeros())
                } else {
                    payload.get(i - 1).map_or(0, |b| b.trailing_zeros())
                };
                let after = payload.get(j).map_or(0, |b| b.leading_zeros());
                let zeros = before + 8 * (j - i) as u32 + after;
                if zeros >= 23 {
                    let pos = self.at(start + 4 + i);
                    self.emit(Rule::ScEmulation, pos, || {
                        format!("{zeros} consecutive zero bits followed by a one that is not a byte-aligned start code")
                    });
                }
            }
            i = j;
        }
    }

    fn unit(&mut self, code: u8, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        if self.st == St::End && code != 0xB3 {
            if !self.after_end_reported {
                self.after_end_reported = true;
                self.emit(Rule::SeqEndPlacement, pos, || {
                    format!("start code {code:02X} follows sequence_end_code without a new sequence header")
                });
            }
            return;
        }
        if code != 0xB2 {
            self.scan_emulation(s, payload);
        }
        // the two "immediately followed by" rules
        if self.st == St::SeqHdr && !(code == 0xB5 && payload.first().is_some_and(|b| b >> 4 == 1))
        {
            self.emit(Rule::OrdSeqExtMissing, pos, || {
                format!("start code {code:02X} follows the sequence header where sequence_extension() shall be")
            });
            self.pending_hdr = None;
            self.seq_usable = false;
            self.st = St::SeqExt;
            self.ext_seen = 0;
        }
        if self.st == St::PicHdr && !(code == 0xB5 && payload.first().is_some_and(|b| b >> 4 == 8))
        {
            self.emit(Rule::OrdPceMissing, pos, || {
                format!("start code {code:02X} follows the picture header where picture_coding_extension() shall be")
            });
            if let Some(p) = self.pic.as_mut() {
                p.decodable = false;
            }
            self.st = St::PicExt;
            self.ext_seen = 0;
        }
        // where the picture data of the next picture start (for the buffer model)
        if !(0x01..=0xAF).contains(&code)
            && self.data_start.is_none()
            && code != 0xB7
            && matches!(self.st, St::Slices | St::Init | St::End)
        {
            self.data_start = Some(s as u64);
        }
        match code {
            0x00 => self.picture_header(s, payload),
            0x01..=0xAF => self.slice(code, s, payload),
            0xB2 => self.user_data(s, payload),
            0xB3 => self.sequence_header(s, payload),
            0xB5 => self.extension(s, payload),
            0xB7 => self.sequence_end(s, payload),
            0xB8 => self.gop_header(s, payload),
            0xB0 | 0xB1 | 0xB6 => self.emit(Rule::ScReserved, pos, || {
                format!("reserved start code {code:02X}")
            }),
            0xB4 => self.emit(Rule::ScSequenceError, pos, || {
                "sequence_error_code".to_string()
            }),
            _ => self.emit(Rule::ScSystem, pos, || {
                format!("system start code {code:02X} in the video elementary stream")
            }),
        }
    }

    fn trailing(&mut self, br: &BitReader<'_>, payload_at: usize, what: &str) {
        if br.overrun() {
            let pos = self.at(payload_at + br.len() / 8);
            self.emit(Rule::HdrTruncated, pos, || {
                format!(
                    "{what} needs {} bits, only {} are there before the next start code",
                    br.pos(),
                    br.len()
                )
            });
        } else if !br.rest_is_zero() {
            let pos = self.at_bits(payload_at, br.pos());
            self.emit(Rule::StuffNonZero, pos, || {
                format!("non-zero bits follow {what} before the next start code")
            });
        }
    }

    // -------------------------------------------------------------------------- sequence

    fn read_matrix(
        &mut self,
        br: &mut BitReader<'_>,
        payload_at: usize,
        intra: bool,
        what: &str,
    ) -> [u8; 64] {
        let mut m = [16u8; 64];
        let at = br.pos();
        let mut zero = false;
        let mut first = 0u8;
        for i in 0..64usize {
            let v = br.read(8) as u8;
            if i == 0 {
                first = v;
            }
            if v == 0 {
                zero = true;
            }
            let raster = self.t.scan_pos[0].get(i).copied().unwrap_or(0);
            if let Some(e) = m.get_mut(usize::from(raster)) {
                *e = v.max(1);
            }
        }
        if br.overrun() {
            return m;
        }
        let pos = self.at_bits(payload_at, at);
        if zero {
            self.emit(Rule::QmZero, pos, || {
                format!("{what} contains the value 0 (forbidden)")
            });
        }
        if intra && first != 8 {
            self.emit(Rule::QmIntraDc, pos, || {
                format!("the first value of {what} is {first}, not 8")
            });
        }
        self.sink.feature("quantizer matrix loaded");
        m
    }

    fn default_matrices() -> [[u8; 64]; 4] {
        let mut m = [[16u8; 64]; 4];
        for (v, row) in tables::DEFAULT_INTRA_MATRIX.iter().enumerate() {
            for (u, &x) in row.iter().enumerate() {
                for k in [0usize, 2] {
                    if let Some(e) = m.get_mut(k).and_then(|a| a.get_mut(v * 8 + u)) {
                        *e = x;
                    }
                }
            }
        }
        for (v, row) in tables::DEFAULT_NON_INTRA_MATRIX.iter().enumerate() {
            for (u, &x) in row.iter().enumerate() {
                for k in [1usize, 3] {
                    if let Some(e) = m.get_mut(k).and_then(|a| a.get_mut(v * 8 + u)) {
                        *e = x;
                    }
                }
            }
        }
        m
    }

    fn sequence_header(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        match self.st {
            St::Init | St::End => {}
            St::Slices => self.finish_picture(),
            St::SeqExt | St::SeqHdr => self.emit(Rule::OrdSeqNoPicture, pos, || {
                "a sequence header follows a sequence header with no coded picture between them".to_string()
            }),
            St::Gop => self.emit(Rule::OrdUnit, pos, || {
                "a sequence header follows a group_of_pictures_header() with no picture between them".to_string()
            }),
            St::PicHdr | St::PicExt => {
                self.emit(Rule::OrdUnit, pos, || {
                    "a sequence header follows a picture header whose picture has no slices".to_string()
                });
                self.finish_picture();
            }
        }
        if self.st == St::End {
            self.seq = None;
            self.after_end_reported = false;
        }
        if self.first_field.is_some() {
            self.seqhdr_between_fields = true;
        }
        let pa = s + 4;
        let mut br = BitReader::new(payload);
        let hdr = SeqHdr {
            hsize: br.read(12),
            vsize: br.read(12),
            aspect: br.read(4) as u8,
            frc: br.read(4) as u8,
            bit_rate: br.read(18),
            vbv: {
                let marker = br.bit();
                if !marker && !br.overrun() {
                    self.emit(Rule::SeqMarker, self.at_bits(pa, 54), || {
                        "marker_bit of the sequence header is '0'".to_string()
                    });
                }
                br.read(10)
            },
            constrained: br.bit(),
        };
        let mut mats = [[0u8; 64]; 2];
        let d = Self::default_matrices();
        mats[0] = d[0];
        mats[1] = d[1];
        if br.bit() {
            mats[0] = self.read_matrix(&mut br, pa, true, "intra_quantiser_matrix");
        }
        if br.bit() {
            mats[1] = self.read_matrix(&mut br, pa, false, "non_intra_quantiser_matrix");
        }
        self.trailing(&br, pa, "sequence_header()");
        if br.overrun() {
            self.seq_usable = false;
            self.pending_hdr = None;
            self.st = St::SeqHdr;
            return;
        }
        if hdr.hsize == 0 {
            self.emit(Rule::SeqHsizeZero, pos, || {
                "horizontal_size_value is 0".to_string()
            });
        }
        if hdr.vsize == 0 {
            self.emit(Rule::SeqVsizeZero, pos, || {
                "vertical_size_value is 0".to_string()
            });
        }
        if hdr.aspect == 0 || hdr.aspect > 4 {
            let a = hdr.aspect;
            self.emit(Rule::SeqAspect, pos, || {
                format!(
                    "aspect_ratio_information {a} is {}",
                    if a == 0 { "forbidden" } else { "reserved" }
                )
            });
        }
        if frame_rate_value(hdr.frc).is_none() {
            let c = hdr.frc;
            self.emit(Rule::SeqFrameRate, pos, || {
                format!(
                    "frame_rate_code {c} is {}",
                    if c == 0 { "forbidden" } else { "reserved" }
                )
            });
        }
        if hdr.constrained {
            self.emit(Rule::SeqConstrained, pos, || {
                "constrained_parameters_flag is '1'".to_string()
            });
        }
        self.sink
            .report
            .features
            .entry("sequence header")
            .and_modify(|n| *n += 1)
            .or_insert(1);
        self.pending_hdr = Some((hdr, mats, pos));
        self.st = St::SeqHdr;
    }

    fn sequence_extension(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let pa = s + 4;
        let mut br = BitReader::new(payload);
        br.skip(4);
        let pli = br.read(8) as u8;
        let progressive = br.bit();
        let chroma = br.read(2) as u8;
        let hsize_ext = br.read(2);
        let vsize_ext = br.read(2);
        let bit_rate_ext = br.read(12);
        let marker = br.bit();
        let vbv_ext = br.read(8);
        let low_delay = br.bit();
        let fr_n = br.read(2);
        let fr_d = br.read(5);
        self.trailing(&br, pa, "sequence_extension()");
        self.st = St::SeqExt;
        self.ext_seen = 0;
        let Some((hdr, mats, hpos)) = self.pending_hdr.take() else {
            return;
        };
        if br.overrun() {
            self.seq_usable = false;
            return;
        }
        let ext = SeqExt {
            pli,
            progressive,
            chroma,
            hsize_ext,
            vsize_ext,
            bit_rate_ext,
            vbv_ext,
            low_delay,
            fr_n,
            fr_d,
        };
        if !marker {
            self.emit(Rule::SeqxMarker, self.at_bits(pa, 31), || {
                "marker_bit of the sequence extension is '0'".to_string()
            });
        }
        if chroma == 0 {
            self.emit(Rule::SeqxChroma, pos, || {
                "chroma_format '00' is reserved".to_string()
            });
        }
        // repeat header?
        if let Some(seq) = self.seq.as_mut() {
            let same_hdr = seq.hdr == hdr;
            let same_ext = seq.ext == ext;
            let first = (seq.hdr.clone(), seq.ext.clone());
            if seq.pictures_since_header > 0 || seq.started {
                self.sink.feature("repeat sequence header");
            }
            let secs = seq.seconds_since_header;
            seq.matrices[0] = mats[0];
            seq.matrices[1] = mats[1];
            seq.matrices[2] = mats[0];
            seq.matrices[3] = mats[1];
            seq.sde_this_header = false;
            seq.pictures_since_header = 0;
            seq.seconds_since_header = 0.0;
            seq.first_picture_after_header = true;
            if !same_hdr {
                self.emit(Rule::SeqRepeatDiff, hpos, || {
                    format!(
                        "repeat sequence header differs from the first: first {:?}, now {:?}",
                        first.0, hdr
                    )
                });
            }
            if !same_ext {
                self.emit(Rule::SeqRepeatExtDiff, pos, || {
                    format!("sequence extension after a repeat header differs from the first: first {:?}, now {:?}", first.1, ext)
                });
            }
            if self.opts.profile.broadcast() && secs > 0.6 {
                self.emit(Rule::BcSeqInterval, hpos, || {
                    format!("{secs:.3} s of video since the previous sequence header")
                });
            }
            if !same_hdr || !same_ext {
                // keep decoding with the first header's parameters
            }
            self.seq_usable = true;
            return;
        }
        // first header of a sequence
        self.sink.report.sequences += 1;
        let width = (hsize_ext << 12) | hdr.hsize;
        let height = (vsize_ext << 12) | hdr.vsize;
        let mb_w = width.div_ceil(16).max(1);
        let mb_h = if progressive {
            height.div_ceil(16).max(1)
        } else {
            2 * height.div_ceil(32).max(1)
        };
        let bit_rate = (u64::from(bit_rate_ext) << 18 | u64::from(hdr.bit_rate)) * 400;
        let vbv_bits = (u64::from(vbv_ext) << 10 | u64::from(hdr.vbv)) * 16384;
        let fr = frame_rate_value(hdr.frc).unwrap_or((30000, 1001));
        let frame_rate =
            f64::from(fr.0) * f64::from(fr_n + 1) / (f64::from(fr.1) * f64::from(fr_d + 1));
        if bit_rate == 0 {
            self.emit(Rule::SeqBitRateZero, hpos, || {
                "bit_rate is 0 (forbidden)".to_string()
            });
        }
        if (fr_n != 0 || fr_d != 0) && gcd(fr_n + 1, fr_d + 1) > 1 {
            self.emit(Rule::SeqxFrameRateExt, pos, || {
                format!("frame_rate_extension_n + 1 = {} and frame_rate_extension_d + 1 = {} have a common divisor", fr_n + 1, fr_d + 1)
            });
        } else if fr_n != 0 || fr_d != 0 {
            // is the resulting rate a Table 6-4 value?
            let num = u64::from(fr.0) * u64::from(fr_n + 1);
            let den = u64::from(fr.1) * u64::from(fr_d + 1);
            if (1..=8)
                .filter_map(frame_rate_value)
                .any(|(a, b)| u64::from(a) * den == u64::from(b) * num)
            {
                self.emit(Rule::SeqxFrameRateExt, pos, || {
                    format!("frame_rate_extension {fr_n}/{fr_d} gives a frame rate that Table 6-4 has directly")
                });
            }
        }
        // profile and level
        let escape = pli & 0x80 != 0;
        let profile = (pli >> 4) & 7;
        let level_id = pli & 0x0F;
        let mut level = None;
        let mut simple = false;
        if escape {
            // Table 8-4: 4:2:2 profile (1000 0101 Main, 1000 0010 High) and Multi-view
            if !matches!(pli, 0x85 | 0x82 | 0x8E | 0x8D | 0x8B | 0x8A) {
                self.emit(Rule::SeqxProfile, pos, || {
                    format!("profile_and_level_indication 0x{pli:02X} (escape bit set) is not a defined value of Table 8-4")
                });
            }
            self.emit(Rule::PlProfile, pos, || {
                format!("profile_and_level_indication 0x{pli:02X}: not Main or Simple profile; profile and level limits are not checked")
            });
        } else {
            let lv = LEVELS.iter().find(|l| l.id == level_id).copied();
            let profile_ok = (1..=5).contains(&profile);
            if lv.is_none() || !profile_ok {
                self.emit(Rule::SeqxProfile, pos, || {
                    format!("profile_and_level_indication 0x{pli:02X}: profile {profile:03b} or level {level_id:04b} is reserved")
                });
            }
            if profile == 4 || profile == 5 {
                level = lv;
                simple = profile == 5;
            } else if profile_ok {
                self.emit(Rule::PlProfile, pos, || {
                    format!("profile_and_level_indication 0x{pli:02X}: not Main or Simple profile; profile and level limits are not checked")
                });
            }
        }
        let d = Self::default_matrices();
        let mut seq = Seq {
            hdr,
            ext,
            sde: None,
            started: false,
            width,
            height,
            mb_w,
            mb_h,
            bit_rate,
            vbv_bits,
            frame_rate,
            level,
            simple,
            matrices: d,
            refs: 0,
            seen_ffff: false,
            seen_delay: false,
            mix_reported: false,
            pics: Vec::new(),
            sde_this_header: false,
            pictures_since_header: 0,
            seconds_since_header: 0.0,
            first_picture_after_header: true,
        };
        seq.matrices[0] = mats[0];
        seq.matrices[1] = mats[1];
        seq.matrices[2] = mats[0];
        seq.matrices[3] = mats[1];
        self.sequence_limits(&seq, hpos, pos);
        self.sink.feature(if seq.ext.progressive {
            "progressive_sequence 1"
        } else {
            "progressive_sequence 0"
        });
        if seq.ext.low_delay {
            self.sink.feature("low_delay 1");
        }
        self.seq = Some(seq);
        self.seq_usable = true;
        self.pending_ref = None;
        self.last_displayed = None;
        self.leading = None;
        self.first_field = None;
        self.last_gop_tc = None;
        self.out_fields = 0;
        self.last_field_top = None;
    }

    /// Profile, level and broadcast limits on the sequence parameters.
    fn sequence_limits(&mut self, seq: &Seq, hpos: Position, xpos: Position) {
        let h = &seq.hdr;
        let x = &seq.ext;
        let luma_rate = |seq: &Seq| -> f64 {
            let w = 16 * u64::from(seq.width.div_ceil(16));
            let hh = if seq.ext.progressive {
                16 * u64::from(seq.height.div_ceil(16))
            } else {
                32 * u64::from(seq.height.div_ceil(32))
            };
            (w * hh) as f64 * seq.frame_rate
        };
        let p_rate = luma_rate(seq);
        let fits = |l: &Level| -> bool {
            seq.width <= l.hsize
                && seq.height <= l.vsize
                && h.frc <= l.max_frc
                && p_rate <= l.sample_rate as f64 + 0.5
                && seq.bit_rate <= l.bit_rate
                && seq.vbv_bits <= l.vbv
        };
        if let Some(l) = seq.level {
            if x.chroma > 1 {
                self.emit(Rule::PlChroma, xpos, || {
                    format!(
                        "chroma_format {} in Main or Simple profile (4:2:0 only)",
                        x.chroma
                    )
                });
            }
            if x.fr_n != 0 || x.fr_d != 0 {
                self.emit(Rule::PlFrameRateExt, xpos, || {
                    format!(
                        "frame_rate_extension_n/d = {}/{} (shall be 0)",
                        x.fr_n, x.fr_d
                    )
                });
            }
            if h.aspect == 4 {
                self.emit(Rule::PlAspect, hpos, || {
                    "aspect_ratio_information 4 (2.21:1) is not allowed in Main profile".to_string()
                });
            }
            if frame_rate_value(h.frc).is_some() && h.frc > l.max_frc {
                self.emit(Rule::PlFrameRateCode, hpos, || {
                    format!(
                        "frame_rate_code {} at {} level (1 to {})",
                        h.frc, l.name, l.max_frc
                    )
                });
            }
            if seq.width > l.hsize || seq.height > l.vsize {
                self.emit(Rule::PlSize, hpos, || {
                    format!(
                        "{}x{} exceeds {}x{} of {} level",
                        seq.width, seq.height, l.hsize, l.vsize, l.name
                    )
                });
            }
            if p_rate > l.sample_rate as f64 + 0.5 {
                self.emit(Rule::PlSampleRate, hpos, || {
                    format!(
                        "luminance sample rate {:.0} exceeds {} of {} level",
                        p_rate, l.sample_rate, l.name
                    )
                });
            }
            if seq.bit_rate > l.bit_rate {
                self.emit(Rule::PlBitRate, hpos, || {
                    format!(
                        "bit_rate {} exceeds {} of {} level",
                        seq.bit_rate, l.bit_rate, l.name
                    )
                });
            }
            if seq.vbv_bits > l.vbv {
                self.emit(Rule::PlVbvSize, hpos, || {
                    format!(
                        "vbv_buffer_size {} bits exceeds {} of {} level",
                        seq.vbv_bits, l.vbv, l.name
                    )
                });
            }
            if l.id == 0b1000 && seq.height > 480 && h.frc != 3 {
                self.emit(Rule::PlRffMainLevel, hpos, || {
                    format!("Main level with vertical_size {} > 480 needs a frame rate of 25 Hz (frame_rate_code {})", seq.height, h.frc)
                });
            }
        }
        if !self.opts.profile.broadcast() {
            return;
        }
        let prof = self.opts.profile;
        if seq.level.is_none() || x.pli & 0x80 != 0 || (x.pli >> 4) & 7 != 4 {
            self.emit(Rule::BcProfile, xpos, || {
                format!(
                    "profile_and_level_indication 0x{:02X} is not Main profile",
                    x.pli
                )
            });
        }
        let row_ok = FORMATS.iter().any(|&(v, hz, aspects, prog, inter, a53)| {
            (a53 || prof.scte43())
                && v == h.vsize
                && hz == h.hsize
                && aspects.contains(&h.aspect)
                && (if x.progressive { prog } else { inter }).contains(&h.frc)
        });
        if !row_ok {
            self.emit(Rule::BcFormat, hpos, || {
                format!(
                    "{}x{} aspect_ratio_information {} frame_rate_code {} progressive_sequence {} is not a row of the format table",
                    h.hsize,
                    h.vsize,
                    h.aspect,
                    h.frc,
                    u8::from(x.progressive)
                )
            });
        }
        let max_rate = match prof {
            Profile::A53 => 48500,
            Profile::Scte43Qam64 => 67500,
            _ => 97000,
        };
        if h.bit_rate > max_rate {
            self.emit(Rule::BcBitRate, hpos, || {
                format!(
                    "bit_rate_value {} ({} bit/s) exceeds {} ({} bit/s)",
                    h.bit_rate,
                    u64::from(h.bit_rate) * 400,
                    max_rate,
                    u64::from(max_rate) * 400
                )
            });
        }
        if h.vbv > 488 {
            self.emit(Rule::BcVbvSize, hpos, || {
                format!("vbv_buffer_size_value {} exceeds 488", h.vbv)
            });
        }
        if x.chroma != 1
            || x.hsize_ext != 0
            || x.vsize_ext != 0
            || x.bit_rate_ext != 0
            || x.vbv_ext != 0
            || x.fr_n != 0
            || x.fr_d != 0
        {
            self.emit(Rule::BcSeqExtFields, xpos, || {
                format!(
                    "chroma_format {} size extensions {}/{} bit_rate_extension {} vbv_buffer_size_extension {} frame_rate_extension {}/{}: shall be '01' and zeros",
                    x.chroma, x.hsize_ext, x.vsize_ext, x.bit_rate_ext, x.vbv_ext, x.fr_n, x.fr_d
                )
            });
        }
        if let Some(l) = seq.level
            && let Some(lowest) = LEVELS.iter().find(|c| fits(c))
            && lowest.id != l.id
            && fits(&l)
        {
            self.emit(Rule::BcLevelLowest, xpos, || {
                format!(
                    "the stream says {} level; {} level is the lowest that holds its parameters",
                    l.name, lowest.name
                )
            });
        }
        if seq.bit_rate > 0 && seq.vbv_bits as f64 / seq.bit_rate as f64 > 1.0 {
            self.emit(Rule::BcBufferDelay, hpos, || {
                format!(
                    "vbv_buffer_size / bit_rate = {:.3} s, more than the one second of the T-STD",
                    seq.vbv_bits as f64 / seq.bit_rate as f64
                )
            });
        }
    }

    fn sequence_display_extension(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let pa = s + 4;
        let mut br = BitReader::new(payload);
        br.skip(4);
        let video_format = br.read(3) as u8;
        let color = if br.bit() {
            Some((br.read(8) as u8, br.read(8) as u8, br.read(8) as u8))
        } else {
            None
        };
        let dh = br.read(14);
        let marker = br.bit();
        let dv = br.read(14);
        self.trailing(&br, pa, "sequence_display_extension()");
        if br.overrun() {
            return;
        }
        if !marker {
            self.emit(Rule::SdeMarker, pos, || {
                "marker_bit of the sequence display extension is '0'".to_string()
            });
        }
        if video_format > 5 {
            self.emit(Rule::SdeVideoFormat, pos, || {
                format!("video_format {video_format} is reserved")
            });
        }
        if let Some((p, t, m)) = color {
            let bad = |v: u8, max: u8| v == 0 || v == 3 || v > max;
            if bad(p, 7) || bad(t, 8) || bad(m, 7) {
                self.emit(Rule::SdeColor, pos, || {
                    format!("colour_primaries {p}, transfer_characteristics {t}, matrix_coefficients {m}: a forbidden or reserved value")
                });
            }
        }
        if dh == 0 || dv == 0 {
            self.emit(Rule::SdeDisplaySize, pos, || {
                format!("display size {dh}x{dv}")
            });
        }
        let sde = Sde {
            video_format,
            color,
            dh,
            dv,
        };
        let prof = self.opts.profile;
        let Some(seq) = self.seq.as_mut() else {
            return;
        };
        seq.sde_this_header = true;
        if !seq.started {
            seq.sde = Some(sde.clone());
        } else if seq.sde.as_ref() != Some(&sde) {
            let first = seq.sde.clone();
            self.emit(Rule::SeqRepeatSde, pos, || {
                format!("sequence_display_extension() after a repeat header differs from the first: first {first:?}, now {sde:?}")
            });
        }
        if prof.broadcast() {
            if video_format != 0 {
                self.emit(Rule::BcVideoFormat, pos, || {
                    format!("video_format is {video_format}, not '000'")
                });
            }
            if prof.scte43()
                && let Some(c) = color
                && c != (1, 1, 1)
            {
                self.emit(Rule::BcColor709, pos, || {
                    format!("color description {c:?} is not ITU-R BT.709 (1, 1, 1)")
                });
            }
        }
    }

    /// Checks made when the headers of a sequence (or repeat) are complete: at the first GOP
    /// or picture header after them.
    fn headers_complete(&mut self, pos: Position) {
        let prof = self.opts.profile;
        let Some(seq) = self.seq.as_mut() else {
            return;
        };
        if !seq.first_picture_after_header || seq.pictures_since_header > 0 {
            return;
        }
        if seq.started {
            let had = seq.sde.is_some();
            if had != seq.sde_this_header {
                let now = seq.sde_this_header;
                self.emit(Rule::SeqRepeatSde, pos, || {
                    if now {
                        "a sequence_display_extension() follows this repeat header, none followed the first".to_string()
                    } else {
                        "no sequence_display_extension() follows this repeat header, one followed the first".to_string()
                    }
                });
            }
            return;
        }
        seq.started = true;
        let color_explicit = seq.sde.as_ref().is_some_and(|s| s.color.is_some());
        let ntsc = seq.hdr.vsize == 480 && seq.hdr.frc == 4 && !seq.ext.progressive;
        if prof.broadcast() && !color_explicit && !(prof.scte43() && ntsc) {
            self.emit(Rule::BcColor, pos, || {
                "no sequence_display_extension() with colour_description after the sequence header"
                    .to_string()
            });
        }
    }

    fn sequence_end(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        match self.st {
            St::Slices | St::PicHdr | St::PicExt => self.finish_picture(),
            St::SeqExt | St::SeqHdr => self.emit(Rule::OrdSeqNoPicture, pos, || {
                "sequence_end_code follows a sequence header with no coded picture between them"
                    .to_string()
            }),
            _ => self.emit(Rule::SeqEndPlacement, pos, || {
                "sequence_end_code does not follow picture data".to_string()
            }),
        }
        if payload.iter().any(|&b| b != 0) {
            self.emit(Rule::StuffNonZero, self.at(s + 4), || {
                "non-zero bytes follow sequence_end_code".to_string()
            });
        }
        self.sink.feature("sequence_end_code");
        self.end_sequence(Some(s as u64 + 4));
        self.st = St::End;
    }

    fn end_sequence(&mut self, end: Option<u64>) {
        if self.first_field.take().is_some() {
            let pos = self.at(end.unwrap_or(self.data.len() as u64) as usize);
            self.emit(Rule::SeqEndComplete, pos, || {
                "the sequence ends after the first field picture of a frame".to_string()
            });
        }
        if let Some(f) = self.pending_ref.take() {
            self.display(f);
        }
        let Some(mut seq) = self.seq.take() else {
            return;
        };
        let end = end.unwrap_or(self.data.len() as u64);
        if let Some(last) = seq.pics.last_mut() {
            last.end = end;
        }
        let vs = VbvSeq {
            rate: seq.bit_rate as f64,
            size: seq.vbv_bits as f64,
            frame_rate: seq.frame_rate,
            progressive: seq.ext.progressive,
            low_delay: seq.ext.low_delay,
        };
        vbv::check(&vs, &seq.pics, self.opts.delivery, &mut self.sink);
        self.data_start = None;
        self.leading = None;
        self.last_displayed = None;
    }

    fn end_of_stream(&mut self) {
        match self.st {
            St::Slices | St::PicHdr | St::PicExt => self.finish_picture(),
            _ => {}
        }
        if self.st != St::End && self.st != St::Init {
            let pos = self.at(self.data.len());
            self.emit(Rule::SeqEndMissing, pos, || {
                "the stream ends without sequence_end_code".to_string()
            });
        }
        self.end_sequence(None);
    }

    // -------------------------------------------------------------------------- extensions

    fn extension(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let Some(id) = payload.first().map(|b| b >> 4) else {
            self.emit(Rule::HdrTruncated, pos, || {
                "extension_start_code with no identifier".to_string()
            });
            return;
        };
        match id {
            1 => {
                if self.st == St::SeqHdr {
                    self.sequence_extension(s, payload);
                } else {
                    self.emit(Rule::OrdSeqExtStray, pos, || {
                        "sequence_extension() does not immediately follow a sequence header"
                            .to_string()
                    });
                }
                return;
            }
            8 => {
                if self.st == St::PicHdr {
                    self.picture_coding_extension(s, payload);
                } else {
                    self.emit(Rule::OrdPceStray, pos, || {
                        "picture_coding_extension() does not immediately follow a picture header"
                            .to_string()
                    });
                }
                return;
            }
            0 | 6 | 13..=15 => {
                self.emit(Rule::OrdExtReserved, pos, || {
                    format!("extension_start_code_identifier {id} is reserved")
                });
                return;
            }
            _ => {}
        }
        let seq_level = matches!(id, 2 | 5);
        let ok = if seq_level {
            self.st == St::SeqExt
        } else {
            self.st == St::PicExt
        };
        if !ok {
            let st = self.st;
            self.emit(Rule::OrdExtPlace, pos, || {
                format!(
                    "extension with identifier {id} at a point where it is not allowed ({st:?})"
                )
            });
            return;
        }
        if self.ext_seen & (1 << id) != 0 {
            self.emit(Rule::OrdExtDup, pos, || {
                format!("the extension with identifier {id} occurs a second time at this point")
            });
        }
        self.ext_seen |= 1 << id;
        let main_profile = self.seq.as_ref().is_some_and(|q| q.level.is_some());
        match id {
            2 => self.sequence_display_extension(s, payload),
            3 => self.quant_matrix_extension(s, payload),
            4 => self.copyright_extension(s, payload),
            7 => self.picture_display_extension(s, payload),
            5 | 9 | 10 => {
                if main_profile {
                    self.emit(Rule::PlScalable, pos, || {
                        format!("scalable extension (identifier {id}) in a Main or Simple profile stream")
                    });
                }
                if self.opts.profile.broadcast() {
                    self.emit(Rule::BcProfile, pos, || {
                        format!("scalable extension (identifier {id})")
                    });
                }
                self.sink.feature("scalable extension");
            }
            _ => self.sink.feature("camera parameters or ITU-T extension"),
        }
    }

    fn quant_matrix_extension(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let pa = s + 4;
        let mut br = BitReader::new(payload);
        br.skip(4);
        let mut loaded: [Option<[u8; 64]>; 4] = [None; 4];
        let names = [
            "intra_quantiser_matrix",
            "non_intra_quantiser_matrix",
            "chroma_intra_quantiser_matrix",
            "chroma_non_intra_quantiser_matrix",
        ];
        for (k, name) in names.iter().enumerate() {
            if br.bit() {
                let m = self.read_matrix(&mut br, pa, k % 2 == 0, name);
                if let Some(l) = loaded.get_mut(k) {
                    *l = Some(m);
                }
            }
        }
        self.trailing(&br, pa, "quant_matrix_extension()");
        if br.overrun() {
            return;
        }
        self.sink.feature("quant matrix extension");
        let Some(seq) = self.seq.as_mut() else {
            return;
        };
        let c420 = seq.ext.chroma == 1;
        for k in 0..2usize {
            if let Some(m) = loaded.get(k).copied().flatten() {
                // 6.3.11: with 4:2:2 and 4:4:4 the luminance matrix is also loaded into the
                // chrominance matrix, which may then be loaded with a different one
                if let Some(e) = seq.matrices.get_mut(k) {
                    *e = m;
                }
                if let Some(e) = seq.matrices.get_mut(k + 2) {
                    *e = m;
                }
            }
        }
        for k in 2..4usize {
            if let Some(m) = loaded.get(k).copied().flatten()
                && let Some(e) = seq.matrices.get_mut(k)
            {
                *e = m;
            }
        }
        if c420 && (loaded[2].is_some() || loaded[3].is_some()) {
            self.emit(Rule::QmxChroma, pos, || {
                "a chroma quantizer matrix is loaded in a 4:2:0 stream".to_string()
            });
        }
    }

    fn copyright_extension(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let mut br = BitReader::new(payload);
        br.skip(4);
        let flag = br.bit();
        let ident = br.read(8);
        let _original = br.bit();
        let reserved = br.read(7);
        let m1 = br.bit();
        let n1 = br.read(20);
        let m2 = br.bit();
        let n2 = br.read(22);
        let m3 = br.bit();
        let n3 = br.read(22);
        self.trailing(&br, s + 4, "copyright_extension()");
        if br.overrun() {
            return;
        }
        self.sink.feature("copyright extension");
        if !(m1 && m2 && m3) {
            self.emit(Rule::CpxMarker, pos, || {
                "a marker_bit of the copyright extension is '0'".to_string()
            });
        }
        if reserved != 0 || (!flag && (ident != 0 || n1 != 0 || n2 != 0 || n3 != 0)) {
            self.emit(Rule::CpxReserved, pos, || {
                format!("copyright extension: reserved {reserved}, copyright_flag {}, identifier {ident}, number {n1}/{n2}/{n3}", u8::from(flag))
            });
        }
    }

    fn picture_display_extension(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let (prog_seq, has_sde) = self
            .seq
            .as_ref()
            .map_or((true, true), |q| (q.ext.progressive, q.sde.is_some()));
        let Some(p) = self.pic.as_ref() else {
            return;
        };
        // number of frame center offsets, 6.3.12
        let n = if prog_seq {
            if p.rff { if p.tff { 3 } else { 2 } } else { 1 }
        } else if p.structure != FRAME {
            1
        } else if p.rff {
            3
        } else {
            2
        };
        let mut br = BitReader::new(payload);
        br.skip(4);
        let mut markers = true;
        for _ in 0..n {
            br.skip(16);
            markers &= br.bit();
            br.skip(16);
            markers &= br.bit();
        }
        self.trailing(&br, s + 4, "picture_display_extension()");
        self.sink.feature("picture display extension");
        if br.overrun() {
            return;
        }
        if !markers {
            self.emit(Rule::PdxMarker, pos, || {
                "a marker_bit of the picture display extension is '0'".to_string()
            });
        }
        if !has_sde {
            self.emit(Rule::PdxNoSde, pos, || {
                "picture_display_extension() in a sequence without sequence_display_extension()"
                    .to_string()
            });
        }
    }

    // -------------------------------------------------------------------------- GOP

    fn gop_header(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        match self.st {
            St::SeqExt => {}
            St::Slices => self.finish_picture(),
            st => {
                self.emit(Rule::OrdUnit, pos, || {
                    format!(
                        "group_of_pictures_header() at a point where it is not allowed ({st:?})"
                    )
                });
                if matches!(st, St::PicHdr | St::PicExt) {
                    self.finish_picture();
                }
            }
        }
        self.headers_complete(pos);
        if self.first_field.take().is_some() {
            self.emit(Rule::PceFieldParity, pos, || {
                "a group_of_pictures_header() follows the first field picture of a frame; the second field is missing".to_string()
            });
        }
        let mut br = BitReader::new(payload);
        let drop_frame = br.bit();
        let hours = br.read(5);
        let minutes = br.read(6);
        let marker = br.bit();
        let seconds = br.read(6);
        let pictures = br.read(6);
        let closed = br.bit();
        let broken = br.bit();
        self.trailing(&br, s + 4, "group_of_pictures_header()");
        self.st = St::Gop;
        self.ext_seen = 0;
        if br.overrun() {
            return;
        }
        if !marker {
            self.emit(Rule::GopMarker, self.at_bits(s + 4, 12), || {
                "marker_bit of time_code is '0'".to_string()
            });
        }
        if hours > 23 || minutes > 59 || seconds > 59 || pictures > 59 {
            self.emit(Rule::GopTimeCodeRange, pos, || {
                format!("time_code {hours}:{minutes}:{seconds}:{pictures} is out of range")
            });
        }
        let (frc, rate) = self
            .seq
            .as_ref()
            .map_or((0, 30.0), |q| (q.hdr.frc, q.frame_rate));
        let nominal = rate.round() as u32;
        if drop_frame && frc != 4 {
            self.emit(Rule::GopDropFrame, pos, || {
                format!(
                    "drop_frame_flag is '1' at frame_rate_code {frc} (allowed only at 29.97 Hz)"
                )
            });
        } else if drop_frame && seconds == 0 && pictures < 2 && !minutes.is_multiple_of(10) {
            self.emit(Rule::GopDropFrame, pos, || {
                format!("time_code {hours}:{minutes}:{seconds}:{pictures} does not exist in drop-frame counting")
            });
        }
        if pictures >= nominal && pictures <= 59 && nominal > 0 {
            self.emit(Rule::GopTimeCodePictures, pos, || {
                format!(
                    "time_code_pictures {pictures} at a frame rate counted as {nominal} per second"
                )
            });
        }
        if broken {
            self.emit(Rule::GopBrokenLink, pos, || {
                "broken_link is '1'".to_string()
            });
        }
        let n = u64::from(nominal.max(1));
        let mut frames = ((u64::from(hours) * 60 + u64::from(minutes)) * 60 + u64::from(seconds))
            * n
            + u64::from(pictures);
        if drop_frame {
            let total_minutes = u64::from(hours) * 60 + u64::from(minutes);
            frames -= 2 * (total_minutes - total_minutes / 10);
        }
        self.sink.feature(if closed {
            "GOP header, closed_gop 1"
        } else {
            "GOP header, closed_gop 0"
        });
        self.gop_id += 1;
        self.pending_gop = Some(Gop {
            drop_frame,
            frames,
            closed,
            broken,
        });
        self.gop_for_next_frame = true;
    }

    // -------------------------------------------------------------------------- picture

    fn picture_header(&mut self, s: usize, payload: &[u8]) {
        let mut pos = self.at(s);
        match self.st {
            St::SeqExt | St::Gop => {}
            St::Slices => self.finish_picture(),
            st => {
                self.emit(Rule::OrdUnit, pos, || {
                    format!("picture header at a point where it is not allowed ({st:?})")
                });
                if matches!(st, St::PicHdr | St::PicExt) {
                    self.finish_picture();
                }
            }
        }
        pos.picture_coded = Some(self.pic_count);
        self.headers_complete(pos);
        let mut br = BitReader::new(payload);
        let tr = br.read(10) as u16;
        let ptype = br.read(3) as u8;
        let vbv_delay = br.read(16) as u16;
        let mut full_pel = false;
        let mut fcode_ok = true;
        if ptype == P || ptype == B {
            full_pel |= br.bit();
            fcode_ok &= br.read(3) == 7;
        }
        if ptype == B {
            full_pel |= br.bit();
            fcode_ok &= br.read(3) == 7;
        }
        let mut extra = 0u32;
        while br.peek(1) == 1 && !br.overrun() {
            br.skip(9);
            extra += 1;
        }
        br.skip(1);
        let idx = self.pic_count;
        self.pic_count += 1;
        let after_gop = self.st == St::Gop || self.gop_for_next_frame;
        let mut pic = Pic {
            idx,
            pos,
            ptype,
            tr,
            params: None,
            f_code: [[15; 2]; 2],
            dc_prec: 0,
            structure: FRAME,
            tff: false,
            fpfd: true,
            cmv: false,
            q_scale_type: false,
            intra_vlc: false,
            alt_scan: false,
            rff: false,
            progressive_frame: true,
            have_pce: false,
            decodable: self.seq_usable && self.seq.is_some(),
            second_field: false,
            second_field_after_i: false,
            state: PicState::default(),
            userdata: Vec::new(),
            after_gop,
        };
        self.pic = None;
        self.trailing(&br, s + 4, "picture_header()");
        self.st = St::PicHdr;
        if br.overrun() {
            pic.decodable = false;
            self.pic = Some(pic);
            return;
        }
        if !(I..=B).contains(&ptype) {
            pic.decodable = false;
            self.emit(Rule::PicType, pos, || {
                format!(
                    "picture_coding_type {ptype} is {}",
                    match ptype {
                        0 => "forbidden",
                        4 => "a D-picture, which H.262 does not have",
                        _ => "reserved",
                    }
                )
            });
        }
        if full_pel {
            self.emit(Rule::PicFullPel, pos, || {
                "full_pel_forward_vector or full_pel_backward_vector is '1'".to_string()
            });
        }
        if !fcode_ok {
            self.emit(Rule::PicFcode7, pos, || {
                "forward_f_code or backward_f_code of the picture header is not '111'".to_string()
            });
        }
        if extra > 0 {
            self.emit(Rule::PicExtraInfo, pos, || {
                format!("{extra} bytes of extra_information_picture (extra_bit_picture '1')")
            });
        }
        // the buffer model's record
        let start = self.data_start.take().unwrap_or(s as u64);
        if let Some(seq) = self.seq.as_mut() {
            if let Some(last) = seq.pics.last_mut() {
                last.end = start;
            }
            if vbv_delay == 0xFFFF {
                seq.seen_ffff = true;
            } else {
                seq.seen_delay = true;
            }
            let mix = seq.seen_ffff && seq.seen_delay && !seq.mix_reported;
            if mix {
                seq.mix_reported = true;
            }
            seq.pics.push(VbvPic {
                ptype,
                structure: FRAME,
                second_field: false,
                rff: false,
                tff: false,
                tr,
                gop_before: after_gop,
                vbv_delay,
                start,
                psc_end: s as u64 + 4,
                end: self.data.len() as u64,
                pos,
            });
            if mix {
                self.emit(Rule::PicVbvDelayMix, pos, || {
                    format!("vbv_delay is {vbv_delay} here; the sequence has both 0xFFFF and other values")
                });
            }
        }
        self.sink.feature(match ptype {
            I => "I-picture",
            P => "P-picture",
            B => "B-picture",
            _ => "picture of invalid type",
        });
        if vbv_delay != 0xFFFF {
            self.sink.feature("vbv_delay coded (not 0xFFFF)");
            if self.opts.profile.broadcast() && vbv_delay > 45000 {
                self.emit(Rule::BcVbvDelay, pos, || {
                    format!("vbv_delay {vbv_delay} exceeds 45000")
                });
            }
        }
        self.pic = Some(pic);
    }

    fn picture_coding_extension(&mut self, s: usize, payload: &[u8]) {
        let pa = s + 4;
        let mut br = BitReader::new(payload);
        br.skip(4);
        let mut f_code = [[0u8; 2]; 2];
        for row in &mut f_code {
            for f in row.iter_mut() {
                *f = br.read(4) as u8;
            }
        }
        let dc_prec = br.read(2) as u8;
        let structure = br.read(2) as u8;
        let tff = br.bit();
        let fpfd = br.bit();
        let cmv = br.bit();
        let q_scale_type = br.bit();
        let intra_vlc = br.bit();
        let alt_scan = br.bit();
        let rff = br.bit();
        let c420 = br.bit();
        let progressive_frame = br.bit();
        let composite = br.bit();
        if composite {
            br.skip(20);
        }
        self.trailing(&br, pa, "picture_coding_extension()");
        self.st = St::PicExt;
        self.ext_seen = 0;
        let prof = self.opts.profile;
        let Some(mut pic) = self.pic.take() else {
            return;
        };
        let pos = pic.pos;
        if br.overrun() {
            pic.decodable = false;
            self.pic = Some(pic);
            return;
        }
        pic.have_pce = true;
        pic.f_code = f_code;
        pic.dc_prec = dc_prec;
        pic.structure = structure;
        pic.tff = tff;
        pic.fpfd = fpfd;
        pic.cmv = cmv;
        pic.q_scale_type = q_scale_type;
        pic.intra_vlc = intra_vlc;
        pic.alt_scan = alt_scan;
        pic.rff = rff;
        pic.progressive_frame = progressive_frame;
        let ptype = pic.ptype;

        for (s_i, row) in f_code.iter().enumerate() {
            for (t_i, &f) in row.iter().enumerate() {
                if f == 0 || (10..=14).contains(&f) {
                    self.emit(Rule::PceFcodeReserved, pos, || {
                        format!(
                            "f_code[{s_i}][{t_i}] = {f} is {}",
                            if f == 0 { "forbidden" } else { "reserved" }
                        )
                    });
                }
            }
        }
        let unused_fwd = ptype == I && !cmv;
        let unused_bwd = ptype == I || ptype == P;
        if (unused_fwd && f_code[0] != [15, 15]) || (unused_bwd && f_code[1] != [15, 15]) {
            self.emit(Rule::PceFcodeUnused, pos, || {
                format!(
                    "f_code {:?} in {}: the unused ones are not 15",
                    f_code,
                    match ptype {
                        I => "an I-picture",
                        P => "a P-picture",
                        _ => "a picture",
                    }
                )
            });
        }
        if structure == 0 {
            pic.decodable = false;
            self.emit(Rule::PcePictureStructure, pos, || {
                "picture_structure '00' is reserved".to_string()
            });
            pic.structure = FRAME;
        }
        let structure = pic.structure;
        let frame_pic = structure == FRAME;
        let Some(seq) = self.seq.as_mut() else {
            pic.decodable = false;
            self.pic = Some(pic);
            return;
        };
        let prog_seq = seq.ext.progressive;
        let chroma = seq.ext.chroma.max(1);
        let level = seq.level;
        let simple = seq.simple;
        let low_delay = seq.ext.low_delay;
        let frc = seq.hdr.frc;
        let height = seq.height;
        if let Some(v) = seq.pics.last_mut() {
            v.structure = structure;
            v.rff = rff;
            v.tff = tff;
        }
        if !frame_pic && structure != 0 {
            if tff {
                self.emit(Rule::PceTffField, pos, || {
                    "top_field_first is '1' in a field picture".to_string()
                });
            }
            if fpfd {
                self.emit(Rule::PceFpfdField, pos, || {
                    "frame_pred_frame_dct is '1' in a field picture".to_string()
                });
            }
            if rff {
                self.emit(Rule::PceRffField, pos, || {
                    "repeat_first_field is '1' in a field picture".to_string()
                });
            }
            if progressive_frame {
                self.emit(Rule::PceProgressiveFrameField, pos, || {
                    "progressive_frame is '1' in a field picture".to_string()
                });
            }
        }
        if prog_seq {
            if !rff && tff && frame_pic {
                self.emit(Rule::PceTffProgressive, pos, || {
                    "top_field_first is '1' with repeat_first_field '0' in a progressive sequence"
                        .to_string()
                });
            }
            if !fpfd && frame_pic {
                self.emit(Rule::PceFpfdProgressive, pos, || {
                    "frame_pred_frame_dct is '0' in a progressive sequence".to_string()
                });
            }
            if !frame_pic || !progressive_frame {
                self.emit(Rule::SeqxProgressive, pos, || {
                    format!("picture_structure {structure:02b}, progressive_frame {} in a progressive sequence", u8::from(progressive_frame))
                });
            }
        } else if !progressive_frame && rff && frame_pic {
            self.emit(Rule::PceRffInterlaced, pos, || {
                "repeat_first_field is '1' with progressive_frame '0' in an interlaced sequence"
                    .to_string()
            });
        }
        if (chroma == 1 && c420 != progressive_frame) || (chroma != 1 && c420) {
            self.emit(Rule::PceChroma420Type, pos, || {
                format!(
                    "chroma_420_type {} with progressive_frame {}",
                    u8::from(c420),
                    u8::from(progressive_frame)
                )
            });
        }
        if composite && rff {
            self.emit(Rule::PceComposite, pos, || {
                "composite_display_flag and repeat_first_field are both set".to_string()
            });
        }
        if ptype == B && low_delay {
            self.emit(Rule::SeqxLowDelayB, pos, || {
                "B-picture in a sequence with low_delay '1'".to_string()
            });
        }
        // profile and level
        if let Some(l) = level {
            if dc_prec == 3 {
                self.emit(Rule::PlIntraDc, pos, || {
                    "intra_dc_precision 11 bits in Main or Simple profile".to_string()
                });
            }
            if simple && ptype == B {
                self.emit(Rule::PlPictureType, pos, || {
                    "B-picture in a Simple profile stream".to_string()
                });
            }
            let vmax = if frame_pic { l.f_v_frame } else { l.f_v_field };
            for (s_i, row) in f_code.iter().enumerate() {
                let (h, v) = (row[0], row[1]);
                if (1..=9).contains(&h) && h > l.f_h {
                    self.emit(Rule::PlFcode, pos, || {
                        format!(
                            "f_code[{s_i}][0] = {h} exceeds {} of {} level",
                            l.f_h, l.name
                        )
                    });
                }
                if (1..=9).contains(&v) && v > vmax {
                    self.emit(Rule::PlFcode, pos, || {
                        format!(
                            "f_code[{s_i}][1] = {v} exceeds {vmax} of {} level in a {} picture",
                            l.name,
                            if frame_pic { "frame" } else { "field" }
                        )
                    });
                }
            }
            if rff && frame_pic {
                let allowed = if prog_seq {
                    frc == 7 || frc == 8
                } else {
                    (3..=8).contains(&frc)
                };
                if !allowed {
                    self.emit(Rule::PlRff, pos, || {
                        format!(
                            "repeat_first_field '1' at frame_rate_code {frc} with progressive_sequence {}",
                            u8::from(prog_seq)
                        )
                    });
                }
                if l.id == 0b1000 && ptype == B && (height > 480 || frc == 3) {
                    self.emit(Rule::PlRffMainLevel, pos, || {
                        "repeat_first_field '1' in a B-picture at Main level with more than 480 lines or 25 Hz".to_string()
                    });
                }
            }
        }
        if prof.broadcast() && progressive_frame && !fpfd && frame_pic {
            self.emit(Rule::BcProgressiveFpfd, pos, || {
                "progressive_frame is '1' with frame_pred_frame_dct '0'".to_string()
            });
        }

        // field pairing
        let seqhdr_between = std::mem::take(&mut self.seqhdr_between_fields);
        if frame_pic || structure == 0 {
            if self.first_field.take().is_some() {
                self.emit(Rule::PceFieldParity, pos, || {
                    "a frame picture follows the first field picture of a frame; the second field is missing".to_string()
                });
            }
        } else if let Some(ff) = self.first_field.clone() {
            pic.second_field = true;
            if seqhdr_between {
                self.emit(Rule::SeqHdrBetweenFields, pos, || {
                    "a sequence header precedes the second field picture of a frame".to_string()
                });
            }
            if ff.structure == structure {
                self.emit(Rule::PceFieldParity, pos, || {
                    format!(
                        "both field pictures of the frame have picture_structure {structure:02b}"
                    )
                });
            }
            if !(ff.ptype == ptype || (ff.ptype == I && ptype == P)) {
                self.emit(Rule::PceFieldType, pos, || {
                    format!(
                        "field pictures of one frame have picture_coding_type {} and {}",
                        ff.ptype, ptype
                    )
                });
            }
            if ff.tr != pic.tr && !low_delay {
                self.emit(Rule::PicTemporalRefFields, pos, || {
                    format!(
                        "temporal_reference {} in the second field, {} in the first",
                        pic.tr, ff.tr
                    )
                });
            }
            pic.second_field_after_i = ff.ptype == I && ptype == P;
        }
        if let Some(v) = self.seq.as_mut().and_then(|q| q.pics.last_mut()) {
            v.second_field = pic.second_field;
        }

        let s = &mut self.sink;
        s.feature(if frame_pic {
            "frame picture"
        } else {
            "field picture"
        });
        if rff {
            s.feature("repeat_first_field 1");
        }
        if tff {
            s.feature("top_field_first 1");
        }
        s.feature(if progressive_frame {
            "progressive_frame 1"
        } else {
            "progressive_frame 0"
        });
        s.feature(if fpfd {
            "frame_pred_frame_dct 1"
        } else {
            "frame_pred_frame_dct 0"
        });
        s.feature(if q_scale_type {
            "q_scale_type 1 (non-linear)"
        } else {
            "q_scale_type 0 (linear)"
        });
        s.feature(if intra_vlc {
            "intra_vlc_format 1"
        } else {
            "intra_vlc_format 0"
        });
        s.feature(if alt_scan {
            "alternate_scan 1"
        } else {
            "alternate_scan 0"
        });
        s.feature(match dc_prec {
            0 => "intra_dc_precision 8",
            1 => "intra_dc_precision 9",
            2 => "intra_dc_precision 10",
            _ => "intra_dc_precision 11",
        });
        if cmv {
            s.feature("concealment_motion_vectors 1");
        }
        if composite {
            s.feature("composite_display_flag 1");
        }
        self.pic = Some(pic);
    }

    fn build_params(&self, pic: &Pic) -> Option<PicParams> {
        let seq = self.seq.as_ref()?;
        let frame_pic = pic.structure == FRAME;
        let mut wq = [[16u16; 64]; 4];
        let scan = self.t.scan_pos.get(usize::from(pic.alt_scan))?;
        for (k, w) in wq.iter_mut().enumerate() {
            let m = seq.matrices.get(k)?;
            for (n, e) in w.iter_mut().enumerate() {
                let raster = usize::from(scan.get(n).copied().unwrap_or(0));
                *e = u16::from(m.get(raster).copied().unwrap_or(16));
            }
        }
        let level_vrange = seq
            .level
            .map(|l| if frame_pic { l.vr_frame } else { l.vr_field });
        Some(PicParams {
            ptype: pic.ptype,
            structure: pic.structure,
            f_code: pic.f_code,
            dc_prec: pic.dc_prec,
            tff: pic.tff,
            fpfd: pic.fpfd,
            cmv: pic.cmv,
            q_scale_type: pic.q_scale_type,
            intra_vlc: pic.intra_vlc,
            chroma: seq.ext.chroma.max(1),
            mb_w: seq.mb_w,
            mb_h: if frame_pic { seq.mb_h } else { seq.mb_h / 2 },
            vsize_gt_2800: seq.height > 2800,
            wq,
            second_field_after_i: pic.second_field_after_i,
            level_vrange,
            mb_bit_limit: match seq.ext.chroma {
                2 => 6144,
                3 => 9216,
                _ => 4608,
            },
        })
    }

    fn slice(&mut self, code: u8, s: usize, payload: &[u8]) {
        if !matches!(self.st, St::PicExt | St::Slices) {
            let pos = self.at(s);
            let st = self.st;
            self.emit(Rule::OrdUnit, pos, || {
                format!("slice start code {code:02X} at a point where no slice is allowed ({st:?})")
            });
            return;
        }
        self.st = St::Slices;
        let Some(mut pic) = self.pic.take() else {
            return;
        };
        if pic.decodable && pic.params.is_none() {
            pic.params = self.build_params(&pic);
            if pic.params.is_none() {
                pic.decodable = false;
            }
        }
        if pic.decodable
            && let Some(pp) = pic.params.as_ref()
        {
            let base = Position {
                byte: s as u64 + 4,
                bit: 0,
                picture_coded: Some(pic.idx),
                picture_display: None,
                slice: Some(pic.state.slices),
                macroblock: None,
            };
            mb::decode_slice(
                &self.t,
                pp,
                &mut pic.state,
                &mut self.sink,
                base,
                u32::from(code),
                payload,
            );
        } else {
            pic.state.slices += 1;
        }
        self.pic = Some(pic);
    }

    fn user_data(&mut self, s: usize, payload: &[u8]) {
        let pos = self.at(s);
        let level = match self.st {
            St::SeqExt => 0,
            St::Gop => 1,
            St::PicExt => 2,
            st => {
                self.emit(Rule::OrdUnit, pos, || {
                    format!("user_data() at a point where it is not allowed ({st:?})")
                });
                return;
            }
        };
        let (run, at) = userdata::longest_zero_run(payload);
        if run >= 23 {
            let p = self.at(s + 4 + at.saturating_sub((run / 8) as usize));
            self.emit(Rule::UdZeroRun, p, || {
                if at == payload.len() {
                    format!("user data end with {run} zero bits (zero stuffing after user data is user data)")
                } else {
                    format!("user data contain {run} consecutive zero bits")
                }
            });
        }
        self.sink.feature(match level {
            0 => "user data after sequence header",
            1 => "user data after GOP header",
            _ => "user data after picture header",
        });
        let (kind, issues) = userdata::parse(payload);
        match &kind {
            UdKind::Captions(_) => self.sink.feature("user data: GA94 captions"),
            UdKind::Afd(_) => self.sink.feature("user data: AFD"),
            UdKind::Bar => self.sink.feature("user data: bar data"),
            UdKind::Scte20 => self.sink.feature("user data: SCTE 20"),
            _ => self.sink.feature("user data: other"),
        }
        if !self.opts.profile.broadcast() {
            return;
        }
        for (rule, off, msg) in issues {
            let p = self.at(s + 4 + off);
            self.emit(rule, p, || msg);
        }
        if level != 2 {
            if kind.order().is_some() {
                self.emit(Rule::UdPlacement, pos, || {
                    format!(
                        "{kind:?} user data after a {} header, not a picture header",
                        if level == 0 { "sequence" } else { "GOP" }
                    )
                });
            }
            return;
        }
        if matches!(kind, UdKind::Scte20)
            && let Some(q) = self.seq.as_ref()
            && (q.hdr.vsize != 480 || q.ext.progressive)
        {
            self.emit(Rule::UdScte20Format, pos, || {
                "SCTE 20 caption data in a format that is not standard-definition interlaced"
                    .to_string()
            });
        }
        if let Some(p) = self.pic.as_mut() {
            if let Some(t) = kind.atsc_type()
                && p.userdata.iter().any(|k| k.atsc_type() == Some(t))
            {
                self.sink.emit(Rule::UdDupType, pos, || {
                    format!("a second user_data() with user_data_type_code 0x{t:02X} after this picture header")
                });
            }
            if self.opts.profile.scte43()
                && let Some(o) = kind.order()
                && let Some(prev) = p.userdata.iter().filter_map(UdKind::order).max()
                && prev > o
            {
                self.sink.emit(Rule::UdOrder, pos, || {
                    format!("{kind:?} follows a structure that SCTE 43 places after it (order: SCTE 20, captions, AFD, bar data)")
                });
            }
            p.userdata.push(kind);
        }
    }

    // -------------------------------------------------------------------------- picture end

    fn finish_picture(&mut self) {
        let Some(mut pic) = self.pic.take() else {
            return;
        };
        let pos = pic.pos;
        let prof = self.opts.profile;
        if pic.state.slices == 0 {
            self.emit(Rule::OrdNoSlice, pos, || {
                "the picture has no slice".to_string()
            });
        }
        let Some(seq) = self.seq.as_mut() else {
            return;
        };
        let frame_pic = pic.structure == FRAME;
        let rows = if frame_pic { seq.mb_h } else { seq.mb_h / 2 };
        let total = seq.mb_w * rows;
        let after_header = seq.first_picture_after_header && seq.pictures_since_header == 0;
        seq.pictures_since_header += 1;
        seq.first_picture_after_header = false;
        let prog_seq = seq.ext.progressive;
        let frame_rate = seq.frame_rate;
        let refs = seq.refs;
        let mb_w = seq.mb_w;
        if pic.decodable && pic.state.slices > 0 && pic.state.next_mb < total {
            let next = pic.state.next_mb;
            self.emit(Rule::SliceGap, pos, || {
                format!(
                    "macroblocks {next} to {} (from row {}, column {}) are in no slice",
                    total - 1,
                    next / mb_w + 1,
                    next % mb_w
                )
            });
        }
        // features of the macroblock layer
        let st = std::mem::take(&mut pic.state.stats);
        let s = &mut self.sink;
        s.feature_n("macroblocks coded", st.coded);
        s.feature_n("macroblocks intra", st.intra);
        match pic.ptype {
            P => s.feature_n("intra macroblocks in P-pictures", st.intra),
            B => s.feature_n("intra macroblocks in B-pictures", st.intra),
            _ => {}
        }
        s.feature_n("macroblocks skipped in P-pictures", st.skipped_p);
        s.feature_n("macroblocks skipped in B-pictures", st.skipped_b);
        s.feature_n("macroblocks forward predicted", st.forward);
        s.feature_n("macroblocks backward predicted", st.backward);
        s.feature_n("macroblocks bidirectionally predicted", st.bidirectional);
        s.feature_n("macroblocks no-MC (P)", st.no_mc);
        s.feature_n("field prediction in frame pictures", st.field_pred_in_frame);
        s.feature_n("16x8 prediction", st.mc16x8);
        s.feature_n("dual-prime prediction", st.dual_prime);
        s.feature_n("field DCT macroblocks", st.field_dct);
        s.feature_n("macroblock_quant", st.mb_quant);
        s.feature_n("concealment motion vectors", st.conceal);
        s.feature_n("escape-coded coefficients", st.escapes);
        s.feature_n("coefficients", st.coefficients);
        s.feature_n("coded blocks", st.blocks);
        s.feature_n("macroblock_escape", st.mb_escape);
        s.feature_n("macroblocks above the Table 8-6 limit", st.big_mbs);
        s.feature_n("intra DC at 0 or maximum", st.full_dc_range);
        s.feature_n("slices", u64::from(pic.state.slices));
        if u64::from(pic.state.slices) > u64::from(rows) {
            s.feature("pictures with several slices per row");
        }
        for (name, v) in [
            ("max macroblock bits", u64::from(st.max_mb_bits)),
            ("max |level|", u64::from(st.max_abs_level)),
            (
                "max |vector| horizontal (half samples)",
                st.max_vec_h as u64,
            ),
            ("max |vector| vertical (half samples)", st.max_vec_v as u64),
        ] {
            let e = s.report.features.entry(name).or_insert(0);
            if v > *e {
                *e = v;
            }
        }

        // user data of this picture
        let cc = pic.userdata.iter().find_map(|k| match k {
            UdKind::Captions(c) => Some(c.clone()),
            _ => None,
        });
        if prof.broadcast() {
            let has_afd = pic.userdata.iter().any(|k| matches!(k, UdKind::Afd(_)));
            let has_bar = pic.userdata.iter().any(|k| matches!(k, UdKind::Bar));
            if after_header {
                if self.afd_in_use && !has_afd {
                    self.emit(Rule::AfdAfterSeq, pos, || {
                        "the stream uses AFD, but the first picture after this sequence header carries none".to_string()
                    });
                }
                if self.bar_in_use && !has_bar {
                    self.emit(Rule::BarAfterSeq, pos, || {
                        "the stream uses bar data, but the first picture after this sequence header carries none".to_string()
                    });
                }
            }
            self.afd_in_use |= has_afd;
            self.bar_in_use |= has_bar;
            if pic
                .userdata
                .iter()
                .any(|k| matches!(k, UdKind::Afd(Some(0b0100))))
                && !has_bar
            {
                self.emit(Rule::AfdNotRecommended, pos, || {
                    "active_format 0100 without bar data".to_string()
                });
            }
            if cc.is_some() {
                if !self.seen_cc && self.pics_without_cc > 0 && !self.cc_gap_reported {
                    self.cc_gap_reported = true;
                    let n = self.pics_without_cc;
                    self.emit(Rule::CcMissing, pos, || {
                        format!("captions start here; the {n} pictures before carry no cc_data()")
                    });
                }
                self.seen_cc = true;
            } else {
                self.pics_without_cc += 1;
                if self.seen_cc {
                    self.emit(Rule::CcMissing, pos, || {
                        "the stream carries captions, but this picture has no cc_data()".to_string()
                    });
                }
            }
            if !frame_pic && let Some(c) = cc.as_ref() {
                // a field picture lasts one field
                let expect = (600.0 / (2.0 * frame_rate)).round() as u8;
                if c.cc_count != expect {
                    self.emit(Rule::CcCount, pos, || {
                        format!("cc_count {} in a field picture; one field at {frame_rate:.3} Hz frames needs {expect}", c.cc_count)
                    });
                }
            }
        }

        // cross-picture rules in coded order
        if after_header && pic.ptype == B {
            self.emit(Rule::SeqFirstPicB, pos, || {
                "the first picture after a sequence header is a B-picture".to_string()
            });
        }
        let first_field_of_frame = !frame_pic && !pic.second_field;
        if pic.after_gop && !pic.second_field && pic.ptype != I {
            self.emit(Rule::GopFirstI, pos, || {
                format!("the first picture after a group_of_pictures_header() has picture_coding_type {}", pic.ptype)
            });
        }
        if pic.after_gop && pic.second_field && pic.ptype != I && pic.ptype != P {
            self.emit(Rule::GopFirstI, pos, || {
                "the first coded frame after a group_of_pictures_header() is not an I-frame"
                    .to_string()
            });
        }
        // leading B-pictures
        if pic.ptype == B {
            let fwd = pic.state.forward_pred_mbs;
            let lead = self.leading;
            if fwd > 0 {
                if let Some((true, _, _)) = lead {
                    self.emit(Rule::GopClosedPred, pos, || {
                        format!("{fwd} macroblocks of this B-picture use forward prediction although closed_gop is '1'")
                    });
                } else if refs < 2 && !matches!(lead, Some((_, true, _))) {
                    self.emit(Rule::GopOpenStart, pos, || {
                        format!("{fwd} macroblocks of this B-picture use forward prediction, but the sequence has no earlier reference frame (and broken_link is not set)")
                    });
                }
            }
            if self.last_ref_dual_prime {
                self.last_ref_dual_prime = false;
                self.emit(Rule::MbDualPrimeB, pos, || {
                    "a B-picture lies between a P-picture that uses dual-prime prediction and its reference".to_string()
                });
            }
        }
        if pic.ptype == P && st.dual_prime > 0 {
            self.last_ref_dual_prime = true;
        } else if pic.ptype != B && (frame_pic || pic.second_field) {
            self.last_ref_dual_prime = false;
        }

        if first_field_of_frame {
            self.first_field = Some(FirstField {
                structure: pic.structure,
                ptype: pic.ptype,
                tr: pic.tr,
            });
            if pic.after_gop {
                // the GOP belongs to the frame; keep the flag for the second field
                self.gop_for_next_frame = true;
            }
            return;
        }
        // ---- a frame is complete
        let frame_tr = self.first_field.take().map_or(pic.tr, |ff| ff.tr);
        let frame_type = if pic.second_field && pic.second_field_after_i {
            I
        } else {
            pic.ptype
        };
        let gop = if self.gop_for_next_frame {
            self.pending_gop.take()
        } else {
            None
        };
        self.gop_for_next_frame = false;
        if frame_type != B {
            if let Some(q) = self.seq.as_mut() {
                q.refs += 1;
            }
            self.leading = match (&gop, frame_type) {
                (Some(g), I) => Some((g.closed, g.broken, true)),
                _ => None,
            };
        }
        let fields = if prog_seq {
            if pic.rff {
                if pic.tff { 6 } else { 4 }
            } else {
                2
            }
        } else if frame_pic && pic.rff {
            3
        } else {
            2
        };
        if let Some(q) = self.seq.as_mut() {
            q.seconds_since_header += f64::from(fields) / (2.0 * frame_rate);
        }
        self.frame_count += 1;
        let frame = Frame {
            pic_idx: pic.idx,
            pos,
            ptype: frame_type,
            tr: frame_tr,
            gop_id: self.gop_id,
            gop,
            fields,
            frame_picture: frame_pic,
            tff: pic.tff,
            // a pair of field pictures is output in coded order; this is the second one
            // (picture_structure 1 is "Top field", Table 6-14)
            first_top: if frame_pic {
                pic.tff
            } else {
                pic.structure != 1
            },
            cc,
            has_pic_userdata: !pic.userdata.is_empty(),
        };
        if frame_type == B {
            self.display(frame);
        } else {
            if let Some(prev) = self.pending_ref.take() {
                self.display(prev);
            }
            self.pending_ref = Some(frame);
        }
    }

    /// A frame leaves the decoder in display order.
    fn display(&mut self, f: Frame) {
        let n = self.display_count;
        self.display_count += 1;
        self.display_of_pic.push((f.pic_idx, n));
        if !f.frame_picture {
            // the first field picture of the pair is the one before
            self.display_of_pic.push((f.pic_idx.saturating_sub(1), n));
        }
        let (low_delay, frame_rate, prog_seq) =
            self.seq.as_ref().map_or((false, 30.0, false), |q| {
                (q.ext.low_delay, q.frame_rate, q.ext.progressive)
            });
        // temporal_reference, 6.3.9
        if let Some(prev) = self.last_displayed.as_ref() {
            let expect = if prev.gop_id != f.gop_id {
                0
            } else {
                (prev.tr + 1) % 1024
            };
            if f.tr != expect && !low_delay {
                let (ptr, new_gop) = (prev.tr, prev.gop_id != f.gop_id);
                self.emit(Rule::PicTemporalRef, f.pos, || {
                    if new_gop {
                        format!("temporal_reference {} in the frame displayed first after a GOP header (shall be 0)", f.tr)
                    } else {
                        format!("temporal_reference {} follows {} in display order (shall be {})", f.tr, ptr, expect)
                    }
                });
            }
        } else if f.gop.is_some() && f.tr != 0 && !low_delay {
            self.emit(Rule::PicTemporalRef, f.pos, || {
                format!("temporal_reference {} in the frame displayed first after a GOP header (shall be 0)", f.tr)
            });
        }
        // output field parity, 7.12: "the fields at the output of the decoding process shall
        // always be alternately top and bottom (note that the very first field of a sequence
        // may be either top or bottom)"
        if !prog_seq {
            if let Some(last_top) = self.last_field_top
                && last_top == f.first_top
            {
                let name = |top: bool| if top { "top" } else { "bottom" };
                self.emit(Rule::OutFieldParity, f.pos, || {
                    format!(
                        "this frame is output {} field first, right after a {} field",
                        name(f.first_top),
                        name(last_top)
                    )
                });
            }
            // two fields end on the other parity, three on the first one again
            self.last_field_top = Some(f.first_top == (f.fields == 3));
        }
        // GOP time code continuity. 6.3.8 counts "pictures" and does not say what a picture
        // shown for three fields (or two or three frame periods) counts for. Both readings are
        // accepted: the coded frames displayed since the last GOP header (what mpeg2enc and
        // mencoder write), or the display time since then in frame periods (the time code stays
        // on the clock of the sequence's frame rate; a GOP that starts in the middle of a
        // frame period may round either way).
        let start = self.out_fields;
        self.out_fields += u64::from(f.fields);
        let first_of_gop = self
            .last_displayed
            .as_ref()
            .is_none_or(|p| p.gop_id != f.gop_id);
        if first_of_gop && let Some(g) = self.pending_gop_of(&f) {
            if let Some((frames, at, at_n, drop)) = self.last_gop_tc
                && drop == g.drop_frame
            {
                let nominal = (frame_rate.round() as u64).max(1);
                // a day of time code: 24 h of counted pictures, less the dropped numbers
                let day = if g.drop_frame {
                    24 * 3600 * nominal - 2 * (24 * 60 - 24 * 6)
                } else {
                    24 * 3600 * nominal
                };
                let fields = start - at;
                let (lo, hi) = (frames + fields / 2, frames + fields.div_ceil(2));
                let coded = frames + u64::from(n - at_n);
                if [lo, hi, coded].iter().all(|e| g.frames % day != e % day) {
                    self.emit(Rule::GopTimeCodeContinuity, f.pos, || {
                        format!(
                            "GOP time code is frame {} of the day; the previous GOP's plus the {} frames ({} field periods) displayed since gives {} (or {} by display time)",
                            g.frames,
                            n - at_n,
                            fields,
                            coded,
                            lo
                        )
                    });
                }
            }
            self.last_gop_tc = Some((g.frames, start, n, g.drop_frame));
        }
        // cc_count against the presentation time
        if self.opts.profile.broadcast()
            && f.frame_picture
            && let Some(c) = f.cc.as_ref()
        {
            let ms = |fields: u32| 1000.0 * f64::from(fields) / (2.0 * frame_rate);
            let own = (0.6 * ms(f.fields)).round() as u8;
            let prev = self
                .last_displayed
                .as_ref()
                .map(|p| (0.6 * ms(p.fields)).round() as u8);
            if c.cc_count != own && Some(c.cc_count) != prev {
                self.emit(Rule::CcCount, f.pos, || {
                    format!(
                        "cc_count {} in a picture presented for {:.2} ms; 9600 bit/s needs {}",
                        c.cc_count,
                        ms(f.fields),
                        own
                    )
                });
            }
            // CEA-608 constructs: positions, pairing, order
            let slots = ((ms(f.fields) * 59.94 / 1000.0) - 0.05).ceil().max(1.0) as usize;
            let per_field = |ty: u8| c.lead_608.iter().filter(|t| **t == ty).count();
            if c.lead_608.len() > slots.min(3) || per_field(0) > 2 || per_field(1) > 2 {
                self.emit(Rule::Cc608Position, f.pos, || {
                    format!(
                        "{} CEA-608 constructs (cc_type sequence {:?}) at the start of cc_data(); this picture has room for {}",
                        c.lead_608.len(),
                        c.lead_608,
                        slots.min(3)
                    )
                });
            }
            // CTA-708-E 4.3.2.2.1: when CEA-608 data is present, a construct for each field is
            // included, "even if a decoder is being instructed not to generate the run-in clock
            // and start bit on one field (by setting cc_valid to 0 for that field type)". So
            // valid data of one field with a cc_valid 0 construct of the other is what the
            // clause prescribes (and 4.3.2.2 says what a decoder does then: with captions on
            // field 2 it generates the run-in of field 1 too); what it forbids is a field
            // without any construct. Its exception: above 30/1.001 frames per second the
            // constructs may be reduced.
            let has_valid = c.valid_608[0] > 0 || c.valid_608[1] > 0;
            let reduced_allowed = frame_rate > 30.0 / 1.001 + 1e-6;
            if slots >= 2
                && has_valid
                && !reduced_allowed
                && (per_field(0) == 0 || per_field(1) == 0)
            {
                let missing = if per_field(0) == 0 { "00" } else { "01" };
                self.emit(Rule::Cc608Field2, f.pos, || {
                    format!(
                        "CEA-608 data is present but cc_data() has no construct of cc_type {missing} (the other field), valid or not"
                    )
                });
            }
            if !prog_seq && c.lead_608.len() >= 2 {
                let first = c.lead_608.first().copied().unwrap_or(0);
                let want = u8::from(f.tff);
                if first != want {
                    self.emit(Rule::Cc608Order, f.pos, || {
                        format!(
                            "CEA-608 constructs in order {:?} with top_field_first {} (CTA-708 Annex C puts cc_type {:02b} first)",
                            c.lead_608,
                            u8::from(f.tff),
                            want
                        )
                    });
                }
            }
        }
        let _ = f.has_pic_userdata;
        let _ = f.ptype;
        self.last_displayed = Some(f);
    }

    fn pending_gop_of(&self, f: &Frame) -> Option<Gop> {
        // the GOP header belongs to the I-frame coded after it; the frame displayed first
        // after the header may be a B-frame coded later, which has no `gop` of its own
        f.gop.clone().or_else(|| {
            self.pending_ref.as_ref().and_then(|r| {
                if r.gop_id == f.gop_id {
                    r.gop.clone()
                } else {
                    None
                }
            })
        })
    }
}
