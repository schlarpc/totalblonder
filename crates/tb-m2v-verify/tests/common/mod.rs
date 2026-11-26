//! A writer of MPEG-2 video syntax for the tests: it writes exactly the syntax elements a
//! test asks for, valid or not. It is not an encoder (no pictures go in), and it uses only
//! the verifier's own tables, generated from the standard.
#![allow(dead_code)]

use tb_m2v_verify::tables::{self, Code};
use tb_m2v_verify::{Options, Report, Rule, verify};

/// A bit writer.
#[derive(Default, Clone)]
pub struct BitW {
    pub bytes: Vec<u8>,
    cur: u8,
    n: u32,
}

impl BitW {
    pub fn put(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            self.cur = (self.cur << 1) | ((v >> i) & 1) as u8;
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.n = 0;
            }
        }
    }

    pub fn code(&mut self, c: Code) {
        self.put(u32::from(c.len), c.bits);
    }

    /// Zero bits up to the byte boundary.
    pub fn align(&mut self) {
        while self.n != 0 {
            self.put(1, 0);
        }
    }

    pub fn start(&mut self, code: u8) {
        self.align();
        self.bytes.extend_from_slice(&[0, 0, 1, code]);
    }

    pub fn raw(&mut self, b: &[u8]) {
        self.align();
        self.bytes.extend_from_slice(b);
    }

    /// Position in bits.
    pub fn pos(&self) -> usize {
        self.bytes.len() * 8 + self.n as usize
    }
}

/// Sequence header and sequence extension fields.
#[derive(Clone)]
pub struct Seq {
    pub w: u32,
    pub h: u32,
    pub aspect: u8,
    pub frc: u8,
    pub bit_rate: u32,
    pub marker: bool,
    pub vbv: u32,
    pub constrained: bool,
    pub intra_q: Option<[u8; 64]>,
    pub non_intra_q: Option<[u8; 64]>,
    pub pli: u8,
    pub progressive: bool,
    pub chroma: u8,
    pub hx: u32,
    pub vx: u32,
    pub brx: u32,
    pub ext_marker: bool,
    pub vbvx: u32,
    pub low_delay: bool,
    pub fr_n: u32,
    pub fr_d: u32,
}

impl Default for Seq {
    fn default() -> Self {
        Self {
            w: 64,
            h: 48,
            aspect: 1,
            frc: 4,
            bit_rate: 10000, // 4 Mbit/s
            marker: true,
            vbv: 20,
            constrained: false,
            intra_q: None,
            non_intra_q: None,
            pli: 0x48, // MP@ML
            progressive: true,
            chroma: 1,
            hx: 0,
            vx: 0,
            brx: 0,
            ext_marker: true,
            vbvx: 0,
            low_delay: false,
            fr_n: 0,
            fr_d: 0,
        }
    }
}

impl Seq {
    pub fn interlaced() -> Self {
        Self {
            h: 64,
            progressive: false,
            ..Self::default()
        }
    }

    pub fn mb_w(&self) -> u32 {
        self.w.div_ceil(16)
    }

    pub fn mb_h(&self) -> u32 {
        if self.progressive {
            self.h.div_ceil(16)
        } else {
            2 * self.h.div_ceil(32)
        }
    }
}

/// Picture header and picture coding extension fields.
#[derive(Clone)]
pub struct Pic {
    pub tr: u16,
    pub ptype: u8,
    pub vbv_delay: u16,
    pub full_pel: [bool; 2],
    pub hdr_f_code: [u8; 2],
    pub extra: Vec<u8>,
    pub f_code: [[u8; 2]; 2],
    pub dc_prec: u8,
    pub structure: u8,
    pub tff: bool,
    pub fpfd: bool,
    pub cmv: bool,
    pub q_scale_type: bool,
    pub intra_vlc: bool,
    pub alt_scan: bool,
    pub rff: bool,
    pub c420: bool,
    pub prog: bool,
    pub composite: bool,
}

pub const I: u8 = 1;
pub const P: u8 = 2;
pub const B: u8 = 3;

impl Pic {
    pub fn new(ptype: u8, tr: u16) -> Self {
        let f = match ptype {
            I => [[15, 15], [15, 15]],
            P => [[2, 2], [15, 15]],
            _ => [[2, 2], [2, 2]],
        };
        Self {
            tr,
            ptype,
            vbv_delay: 0xFFFF,
            full_pel: [false; 2],
            hdr_f_code: [7; 2],
            extra: Vec::new(),
            f_code: f,
            dc_prec: 0,
            structure: 3,
            tff: false,
            fpfd: true,
            cmv: false,
            q_scale_type: false,
            intra_vlc: false,
            alt_scan: false,
            rff: false,
            c420: true,
            prog: true,
            composite: false,
        }
    }

    /// A frame picture of an interlaced sequence, with field tools allowed.
    pub fn interlaced(ptype: u8, tr: u16) -> Self {
        Self {
            tff: true,
            fpfd: false,
            c420: false,
            prog: false,
            ..Self::new(ptype, tr)
        }
    }

    /// A field picture.
    pub fn field(ptype: u8, tr: u16, structure: u8) -> Self {
        Self {
            structure,
            tff: false,
            fpfd: false,
            c420: false,
            prog: false,
            ..Self::new(ptype, tr)
        }
    }
}

/// The stream writer.
#[derive(Clone)]
pub struct Bld {
    pub w: BitW,
    pub seq: Seq,
    pub pic: Pic,
    /// motion_vertical_field_select written by `fwd_mb` and `bwd_mb` in field pictures
    /// (default: the same parity as the picture).
    pub fsel: Option<bool>,
}

impl Bld {
    pub fn new(seq: Seq) -> Self {
        Self {
            w: BitW::default(),
            seq,
            pic: Pic::new(I, 0),
            fsel: None,
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.w.align();
        self.w.bytes
    }

    pub fn seq_header(&mut self) {
        let s = self.seq.clone();
        let w = &mut self.w;
        w.start(0xB3);
        w.put(12, s.w & 0xFFF);
        w.put(12, s.h & 0xFFF);
        w.put(4, u32::from(s.aspect));
        w.put(4, u32::from(s.frc));
        w.put(18, s.bit_rate);
        w.put(1, u32::from(s.marker));
        w.put(10, s.vbv);
        w.put(1, u32::from(s.constrained));
        for m in [&s.intra_q, &s.non_intra_q] {
            match m {
                Some(m) => {
                    w.put(1, 1);
                    for v in m {
                        w.put(8, u32::from(*v));
                    }
                }
                None => w.put(1, 0),
            }
        }
    }

    pub fn seq_ext(&mut self) {
        let s = self.seq.clone();
        let w = &mut self.w;
        w.start(0xB5);
        w.put(4, 1);
        w.put(8, u32::from(s.pli));
        w.put(1, u32::from(s.progressive));
        w.put(2, u32::from(s.chroma));
        w.put(2, s.hx);
        w.put(2, s.vx);
        w.put(12, s.brx);
        w.put(1, u32::from(s.ext_marker));
        w.put(8, s.vbvx);
        w.put(1, u32::from(s.low_delay));
        w.put(2, s.fr_n);
        w.put(5, s.fr_d);
    }

    /// Sequence header and extension.
    pub fn seq(&mut self) {
        self.seq_header();
        self.seq_ext();
    }

    pub fn sde(&mut self, video_format: u32, color: Option<(u32, u32, u32)>, marker: bool) {
        let (dw, dh) = (self.seq.w, self.seq.h);
        let w = &mut self.w;
        w.start(0xB5);
        w.put(4, 2);
        w.put(3, video_format);
        match color {
            Some((p, t, m)) => {
                w.put(1, 1);
                w.put(8, p);
                w.put(8, t);
                w.put(8, m);
            }
            None => w.put(1, 0),
        }
        w.put(14, dw);
        w.put(1, u32::from(marker));
        w.put(14, dh);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn gop_tc(
        &mut self,
        drop: bool,
        h: u32,
        m: u32,
        marker: bool,
        s: u32,
        p: u32,
        closed: bool,
        broken: bool,
    ) {
        let w = &mut self.w;
        w.start(0xB8);
        w.put(1, u32::from(drop));
        w.put(5, h);
        w.put(6, m);
        w.put(1, u32::from(marker));
        w.put(6, s);
        w.put(6, p);
        w.put(1, u32::from(closed));
        w.put(1, u32::from(broken));
    }

    /// A GOP header with time code 0 plus `frames` frames at 30 per second.
    pub fn gop(&mut self, frames: u32, closed: bool) {
        self.gop_tc(false, 0, 0, true, frames / 30, frames % 30, closed, false);
    }

    pub fn pic_header(&mut self, p: &Pic) {
        self.pic = p.clone();
        let w = &mut self.w;
        w.start(0x00);
        w.put(10, u32::from(p.tr));
        w.put(3, u32::from(p.ptype));
        w.put(16, u32::from(p.vbv_delay));
        if p.ptype == P || p.ptype == B {
            w.put(1, u32::from(p.full_pel[0]));
            w.put(3, u32::from(p.hdr_f_code[0]));
        }
        if p.ptype == B {
            w.put(1, u32::from(p.full_pel[1]));
            w.put(3, u32::from(p.hdr_f_code[1]));
        }
        for e in &p.extra {
            w.put(1, 1);
            w.put(8, u32::from(*e));
        }
        w.put(1, 0);
    }

    pub fn pce(&mut self) {
        let p = self.pic.clone();
        let w = &mut self.w;
        w.start(0xB5);
        w.put(4, 8);
        for row in p.f_code {
            for f in row {
                w.put(4, u32::from(f));
            }
        }
        w.put(2, u32::from(p.dc_prec));
        w.put(2, u32::from(p.structure));
        for b in [
            p.tff,
            p.fpfd,
            p.cmv,
            p.q_scale_type,
            p.intra_vlc,
            p.alt_scan,
            p.rff,
            p.c420,
            p.prog,
            p.composite,
        ] {
            w.put(1, u32::from(b));
        }
        if p.composite {
            w.put(20, 0);
        }
    }

    /// Picture header and picture coding extension.
    pub fn pic(&mut self, p: &Pic) {
        self.pic_header(p);
        self.pce();
    }

    pub fn user_data(&mut self, d: &[u8]) {
        self.w.start(0xB2);
        self.w.raw(d);
    }

    pub fn ext(&mut self, payload: &[u8]) {
        self.w.start(0xB5);
        self.w.raw(payload);
    }

    pub fn seq_end(&mut self) {
        self.w.start(0xB7);
    }

    // ---- slice and macroblock layer

    pub fn slice(&mut self, row: u32, qscale: u32) {
        self.w.start((row + 1) as u8);
        self.w.put(5, qscale);
        self.w.put(1, 0);
    }

    pub fn inc(&mut self, mut n: u32) {
        while n > 33 {
            self.w.code(tables::MB_ESCAPE);
            n -= 33;
        }
        let Some(c) = tables::MB_ADDR_INC.iter().find(|c| u32::from(c.1) == n) else {
            panic!("no increment {n}");
        };
        self.w.code(c.0);
    }

    /// macroblock_type for the current picture type.
    pub fn mb_type(&mut self, quant: bool, fwd: bool, bwd: bool, pattern: bool, intra: bool) {
        let t = match self.pic.ptype {
            I => tables::MB_TYPE_I,
            P => tables::MB_TYPE_P,
            _ => tables::MB_TYPE_B,
        };
        let Some(c) = t.iter().find(|m| {
            m.quant == quant
                && m.motion_forward == fwd
                && m.motion_backward == bwd
                && m.pattern == pattern
                && m.intra == intra
        }) else {
            panic!("no such macroblock_type");
        };
        self.w.code(c.code);
    }

    /// One motion vector component as a delta against the predictor.
    pub fn mv_component(&mut self, f_code: u8, delta: i32) {
        let r_size = u32::from(f_code) - 1;
        let f = 1i32 << r_size;
        let (code, residual) = if delta == 0 {
            (0, 0)
        } else {
            let a = delta.abs() - 1;
            let c = a / f + 1;
            (if delta < 0 { -c } else { c }, a % f)
        };
        let Some(c) = tables::MOTION_CODE.iter().find(|c| i32::from(c.1) == code) else {
            panic!("no motion_code {code}");
        };
        self.w.code(c.0);
        if r_size > 0 && code != 0 {
            self.w.put(r_size, residual as u32);
        }
    }

    /// motion_vector(r, s) with the deltas (horizontal, vertical).
    pub fn mv(&mut self, s: usize, dh: i32, dv: i32) {
        let f = self.pic.f_code[s];
        self.mv_component(f[0], dh);
        self.mv_component(f[1], dv);
    }

    pub fn cbp(&mut self, v: u8) {
        let Some(c) = tables::CBP.iter().find(|c| c.1 == v) else {
            panic!("no cbp {v}");
        };
        self.w.code(c.0);
    }

    /// dct_dc_size and dct_dc_differential for a differential.
    pub fn dc(&mut self, luma: bool, diff: i32) {
        let size = if diff == 0 {
            0
        } else {
            32 - diff.unsigned_abs().leading_zeros()
        };
        self.dc_sized(luma, size, diff);
    }

    pub fn dc_sized(&mut self, luma: bool, size: u32, diff: i32) {
        let t = if luma {
            tables::DC_SIZE_LUMA
        } else {
            tables::DC_SIZE_CHROMA
        };
        let Some(c) = t.iter().find(|c| u32::from(c.1) == size) else {
            panic!("no dc size {size}");
        };
        self.w.code(c.0);
        if size > 0 {
            let v = if diff > 0 {
                diff
            } else {
                diff + (1 << size) - 1
            };
            self.w.put(size, v as u32);
        }
    }

    /// A (run, level) coefficient with its table code. `table_one` selects Table B.15.
    pub fn coef(&mut self, table_one: bool, run: u8, level: i32) {
        let t = if table_one {
            tables::DCT_ONE
        } else {
            tables::DCT_ZERO
        };
        let Some(c) = t
            .iter()
            .find(|c| c.run == run && i32::from(c.level) == level.abs())
        else {
            panic!("no code for run {run} level {level}");
        };
        self.w.code(c.code);
        self.w.put(1, u32::from(level < 0));
    }

    /// The first coefficient of a non-intra block with run 0 and level +-1.
    pub fn first_one(&mut self, negative: bool) {
        self.w.code(tables::DCT_ZERO_FIRST);
        self.w.put(1, u32::from(negative));
    }

    pub fn escape(&mut self, run: u32, level_bits: u32) {
        self.w.code(tables::DCT_ESCAPE);
        self.w.put(6, run);
        self.w.put(12, level_bits);
    }

    pub fn eob(&mut self, table_one: bool) {
        self.w.code(if table_one {
            tables::DCT_ONE_EOB
        } else {
            tables::DCT_ZERO_EOB
        });
    }

    /// An intra macroblock with DC differentials zero and no other coefficient.
    pub fn intra_mb(&mut self, inc: u32) {
        self.inc(inc);
        self.intra_mb_body();
    }

    /// The same without the address increment (4:2:0, frame_pred_frame_dct as in the picture).
    pub fn intra_mb_body(&mut self) {
        self.mb_type(false, false, false, false, true);
        if self.pic.structure == 3 && !self.pic.fpfd {
            self.w.put(1, 0); // dct_type
        }
        let one = self.pic.intra_vlc;
        let blocks = match self.seq.chroma {
            2 => 8,
            3 => 12,
            _ => 6,
        };
        for i in 0..blocks {
            self.dc(i < 4, 0);
            self.eob(one);
        }
    }

    /// A forward-predicted macroblock without coefficients, vector delta (dh, dv).
    pub fn fwd_mb(&mut self, inc: u32, dh: i32, dv: i32) {
        self.inc(inc);
        self.mb_type(false, true, false, false, false);
        if self.pic.structure == 3 {
            if !self.pic.fpfd {
                self.w.put(2, 2); // frame-based
            }
        } else {
            self.w.put(2, 1); // field-based
            let sel = self.fsel.unwrap_or(self.pic.structure == 2);
            self.w.put(1, u32::from(sel));
        }
        self.mv(0, dh, dv);
    }

    /// A backward-predicted macroblock without coefficients (B-pictures).
    pub fn bwd_mb(&mut self, inc: u32, dh: i32, dv: i32) {
        self.inc(inc);
        self.mb_type(false, false, true, false, false);
        if self.pic.structure == 3 {
            if !self.pic.fpfd {
                self.w.put(2, 2);
            }
        } else {
            self.w.put(2, 1);
            let sel = self.fsel.unwrap_or(self.pic.structure == 2);
            self.w.put(1, u32::from(sel));
        }
        self.mv(1, dh, dv);
    }

    fn rows(&self) -> u32 {
        if self.pic.structure == 3 {
            self.seq.mb_h()
        } else {
            self.seq.mb_h() / 2
        }
    }

    /// All slices of a picture: one slice per row; intra macroblocks in I-pictures, zero
    /// forward vectors in P-pictures (the middle ones skipped), zero backward vectors in
    /// B-pictures.
    pub fn picture_data(&mut self) {
        let (mw, rows) = (self.seq.mb_w(), self.rows());
        for row in 0..rows {
            self.slice(row, 4);
            match self.pic.ptype {
                I => {
                    for _ in 0..mw {
                        self.intra_mb(1);
                    }
                }
                P => {
                    self.fwd_mb(1, 0, 0);
                    self.fwd_mb(mw - 1, 0, 0);
                }
                _ => {
                    self.bwd_mb(1, 0, 0);
                    self.bwd_mb(mw - 1, 0, 0);
                }
            }
        }
    }

    /// Header, extension and data of a default picture.
    pub fn picture(&mut self, p: &Pic) {
        self.pic(p);
        self.picture_data();
    }
}

/// The valid base stream: sequence header, GOP, I P B B in coded order (display I B B P),
/// sequence end. `f` may change the writer before each stage.
pub fn base_stream() -> Vec<u8> {
    let mut b = Bld::new(Seq::default());
    b.seq();
    b.sde(0, Some((1, 1, 1)), true);
    b.gop(0, true);
    b.picture(&Pic::new(I, 0));
    b.picture(&Pic::new(P, 3));
    b.picture(&Pic::new(B, 1));
    b.picture(&Pic::new(B, 2));
    b.seq_end();
    b.finish()
}

/// Verifies with the given options and returns the report.
pub fn run(data: &[u8], opts: &Options) -> Report {
    verify(data, opts)
}

/// The identifiers of the rules a report holds.
pub fn ids(r: &Report) -> Vec<&'static str> {
    r.rules().iter().map(|x| x.info().id).collect()
}

/// Asserts that the report holds exactly these rules.
#[track_caller]
pub fn expect(r: &Report, rules: &[Rule]) {
    let mut want: Vec<Rule> = rules.to_vec();
    want.sort();
    want.dedup();
    let got = r.rules();
    if got != want {
        let w: Vec<&str> = want.iter().map(|x| x.info().id).collect();
        panic!(
            "expected exactly {:?}, got {:?}\n{}",
            w,
            ids(r),
            r.details()
        );
    }
}

/// Offset of the n-th unit with this start code value (and, for extensions, identifier).
pub fn find_unit(data: &[u8], code: u8, ext_id: Option<u8>, nth: usize) -> Option<usize> {
    let mut seen = 0;
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let c = data[i + 3];
            let id_ok = match ext_id {
                Some(id) => data.get(i + 4).is_some_and(|b| b >> 4 == id),
                None => true,
            };
            if c == code && id_ok {
                if seen == nth {
                    return Some(i);
                }
                seen += 1;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    None
}

/// Overwrites `width` bits at bit offset `bit` of the payload of the unit at `unit`.
pub fn set_bits(data: &mut [u8], unit: usize, bit: usize, width: usize, value: u32) {
    for k in 0..width {
        let pos = (unit + 4) * 8 + bit + k;
        let b = (value >> (width - 1 - k)) & 1;
        let mask = 0x80u8 >> (pos % 8);
        if b == 1 {
            data[pos / 8] |= mask;
        } else {
            data[pos / 8] &= !mask;
        }
    }
}

/// Reads `width` bits at bit offset `bit` of the payload of the unit at `unit`.
pub fn get_bits(data: &[u8], unit: usize, bit: usize, width: usize) -> u32 {
    let mut v = 0;
    for k in 0..width {
        let pos = (unit + 4) * 8 + bit + k;
        v = (v << 1) | u32::from((data[pos / 8] >> (7 - pos % 8)) & 1);
    }
    v
}
