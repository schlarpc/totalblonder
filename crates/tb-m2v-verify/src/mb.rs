//! The slice, macroblock and block layers: a full decode of the syntax of H.262 6.2.4 to
//! 6.2.6 with the decoding process of 7.2 to 7.4 (coefficients) and 7.6.3 (motion vectors),
//! as far as the rules need it. No samples are reconstructed.

use crate::bits::BitReader;
use crate::report::{Position, Sink};
use crate::rules::Rule;
use crate::tables;
use crate::vlc::{DctSym, Tables};

/// picture_coding_type values.
pub(crate) const I: u8 = 1;
pub(crate) const P: u8 = 2;
pub(crate) const B: u8 = 3;
/// picture_structure "Frame picture".
pub(crate) const FRAME: u8 = 3;

/// What the slice decoder needs to know about the picture.
#[derive(Debug, Clone)]
pub(crate) struct PicParams {
    pub(crate) ptype: u8,
    pub(crate) structure: u8,
    pub(crate) f_code: [[u8; 2]; 2],
    pub(crate) dc_prec: u8,
    pub(crate) tff: bool,
    pub(crate) fpfd: bool,
    pub(crate) cmv: bool,
    pub(crate) q_scale_type: bool,
    pub(crate) intra_vlc: bool,
    /// chroma_format: 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub(crate) chroma: u8,
    pub(crate) mb_w: u32,
    /// Macroblock rows of this picture (half the frame's for a field picture).
    pub(crate) mb_h: u32,
    pub(crate) vsize_gt_2800: bool,
    /// Weights in scan order: intra luma, non-intra luma, intra chroma, non-intra chroma.
    pub(crate) wq: [[u16; 64]; 4],
    /// A P field picture that is the second field of a frame whose first field is an I field.
    pub(crate) second_field_after_i: bool,
    /// Vertical vector range of the level, in half samples, if the level is known.
    pub(crate) level_vrange: Option<(i32, i32)>,
    /// Macroblock bit limit of Table 8-6.
    pub(crate) mb_bit_limit: u32,
}

/// Counters of what the macroblock layer used.
#[derive(Debug, Clone, Default)]
pub(crate) struct MbStats {
    pub(crate) coded: u64,
    pub(crate) intra: u64,
    pub(crate) skipped_p: u64,
    pub(crate) skipped_b: u64,
    pub(crate) forward: u64,
    pub(crate) backward: u64,
    pub(crate) bidirectional: u64,
    pub(crate) no_mc: u64,
    pub(crate) field_pred_in_frame: u64,
    pub(crate) mc16x8: u64,
    pub(crate) dual_prime: u64,
    pub(crate) field_dct: u64,
    pub(crate) mb_quant: u64,
    pub(crate) conceal: u64,
    pub(crate) escapes: u64,
    pub(crate) coefficients: u64,
    pub(crate) blocks: u64,
    pub(crate) pattern: u64,
    pub(crate) mb_escape: u64,
    pub(crate) max_mb_bits: u32,
    pub(crate) big_mbs: u64,
    pub(crate) nonzero_residual: u64,
    pub(crate) max_abs_level: u32,
    pub(crate) max_vec_h: i32,
    pub(crate) max_vec_v: i32,
    pub(crate) full_dc_range: u64,
}

/// Decoding state of one picture across its slices.
#[derive(Debug, Clone, Default)]
pub(crate) struct PicState {
    /// The next macroblock address not yet covered by a slice.
    pub(crate) next_mb: u32,
    pub(crate) slices: u32,
    pub(crate) stats: MbStats,
    /// Macroblocks (coded or skipped) predicted from the forward reference.
    pub(crate) forward_pred_mbs: u64,
    pub(crate) non_intra_mbs: u64,
    big_row: u32,
    big_in_row: u32,
    slice_picture_id: Option<(bool, u8)>,
    /// The previous slice was not decoded to its end.
    resync: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Frame-based prediction in a frame picture.
    Frame,
    /// Field-based prediction in a frame picture (two vectors).
    FieldInFrame,
    /// Field-based prediction in a field picture.
    Field,
    /// 16x8 motion compensation in a field picture.
    Mc16x8,
    DualPrime,
}

/// Why decoding of a slice stops early. The finding has been emitted.
struct Abort;

struct Dec<'a, 'b> {
    t: &'a Tables,
    pp: &'a PicParams,
    st: &'a mut PicState,
    sink: &'a mut Sink,
    br: BitReader<'b>,
    base: Position,
    mb: Option<u32>,
    // predictors and state of 7.2.1 and 7.6.3
    dc_pred: [i32; 3],
    pmv: [[[i32; 2]; 2]; 2],
    qscale: i32,
    // geometry
    ref_w: i32,
    ref_h: i32,
}

/// Decodes one slice. `payload` is the bytes after the slice start code up to the next
/// start code; `base` gives the byte offset of the payload and the picture and slice numbers.
pub(crate) fn decode_slice(
    t: &Tables,
    pp: &PicParams,
    st: &mut PicState,
    sink: &mut Sink,
    base: Position,
    vertical_position: u32,
    payload: &[u8],
) {
    let ref_w = (pp.mb_w * 16) as i32;
    let ref_h = (pp.mb_h * 16) as i32;
    let mut d = Dec {
        t,
        pp,
        st,
        sink,
        br: BitReader::new(payload),
        base,
        mb: None,
        dc_pred: [0; 3],
        pmv: [[[0; 2]; 2]; 2],
        qscale: 2,
        ref_w,
        ref_h,
    };
    d.st.slices += 1;
    if d.slice(vertical_position).is_err() {
        // the rest of the slice is unknown: assume it reached the end of its row, and do not
        // judge where the next slice starts
        let row = vertical_position
            .saturating_sub(1)
            .min(pp.mb_h.saturating_sub(1));
        let row_end = (row + 1) * pp.mb_w;
        if d.st.next_mb < row_end && vertical_position >= 1 && vertical_position <= pp.mb_h {
            d.st.next_mb = row_end;
        }
        d.st.resync = true;
    }
}

#[inline]
fn div2_trunc(v: i32) -> i32 {
    v / 2
}

/// The "//" of H.262 4.1: division with rounding to the nearest integer, halves away from
/// zero.
#[inline]
fn div2_round(v: i32) -> i32 {
    if v >= 0 {
        (v + 1) >> 1
    } else {
        -((-v + 1) >> 1)
    }
}

impl Dec<'_, '_> {
    fn pos(&self) -> Position {
        self.pos_at(self.br.pos())
    }

    fn pos_at(&self, bit: usize) -> Position {
        Position {
            byte: self.base.byte + (bit >> 3) as u64,
            bit: (bit & 7) as u8,
            macroblock: self.mb,
            ..self.base
        }
    }

    fn emit(&mut self, rule: Rule, message: impl FnOnce() -> String) {
        let p = self.pos();
        self.sink.emit(rule, p, message);
    }

    fn abort(&mut self, rule: Rule, message: impl FnOnce() -> String) -> Abort {
        if self.br.overrun() || self.br.rest_is_zero() {
            let p = self.pos_at(self.br.len());
            self.sink.emit(Rule::SliceTruncated, p, || {
                "the slice data end inside a macroblock (cut short, or non-zero bits after the last macroblock)".to_string()
            });
        } else {
            self.emit(rule, message);
        }
        Abort
    }

    fn reset_dc(&mut self) {
        self.dc_pred = [1 << (7 + i32::from(self.pp.dc_prec)); 3];
    }

    fn slice(&mut self, vertical_position: u32) -> Result<(), Abort> {
        let pp = self.pp;
        let mut row = vertical_position.wrapping_sub(1);
        if pp.vsize_gt_2800 {
            let ext = self.br.read(3);
            row += ext << 7;
        }
        if row >= pp.mb_h {
            let p = self.base;
            self.sink.emit(Rule::SliceVpos, p, || {
                format!(
                    "slice_vertical_position {} addresses row {} of a picture with {} macroblock rows",
                    vertical_position,
                    row + 1,
                    pp.mb_h
                )
            });
            return Err(Abort);
        }
        let q = self.br.read(5);
        if q == 0 {
            self.emit(Rule::SliceQscaleZero, || {
                "quantiser_scale_code of the slice is 0 (forbidden)".to_string()
            });
        }
        self.set_qscale(q.max(1));
        let mut intra_slice = false;
        if self.br.peek(1) == 1 {
            self.br.skip(1);
            intra_slice = self.br.bit();
            let enable = self.br.bit();
            let id = self.br.read(6) as u8;
            if !enable && id != 0 {
                self.emit(Rule::SlicePictureId, || {
                    format!("slice_picture_id is {id} with slice_picture_id_enable '0'")
                });
            }
            match self.st.slice_picture_id {
                None => self.st.slice_picture_id = Some((enable, id)),
                Some(first) if first != (enable, id) => self.emit(Rule::SlicePictureId, || {
                    format!(
                        "slice_picture_id_enable/slice_picture_id {}/{} differ from the first slice's {}/{}",
                        u8::from(enable),
                        id,
                        u8::from(first.0),
                        first.1
                    )
                }),
                Some(_) => {}
            }
            let mut n = 0u32;
            while self.br.peek(1) == 1 && !self.br.overrun() {
                self.br.skip(9);
                n += 1;
            }
            if n > 0 {
                self.emit(Rule::SliceExtraInfo, || {
                    format!("the slice carries {n} bytes of extra_information_slice (extra_bit_slice '1')")
                });
            }
        } else if let Some(first) = self.st.slice_picture_id
            && first.0
        {
            self.emit(Rule::SlicePictureId, || {
                "slice_picture_id_enable is absent (0) here but '1' in the first slice".to_string()
            });
        }
        self.br.skip(1); // extra_bit_slice '0'
        if self.br.overrun() || self.br.peek(23) == 0 {
            let p = self.base;
            self.sink.emit(Rule::SliceEmpty, p, || {
                "the slice contains no macroblock".to_string()
            });
            return Err(Abort);
        }

        self.reset_dc();
        self.pmv = [[[0; 2]; 2]; 2];
        let row_base = row * pp.mb_w;
        let row_end = row_base + pp.mb_w - 1;
        if self.st.big_row != row || self.st.slices == 1 {
            self.st.big_row = row;
            self.st.big_in_row = 0;
        }
        let mut prev_addr: i64 = i64::from(row_base) - 1;
        let mut first = true;
        let mut prev_intra = false;
        let mut prev_dirs = (false, false);
        let mut non_intra_in_slice = false;

        loop {
            let mb_start = self.br.pos();
            let mut inc = 0u32;
            loop {
                let w = self.br.peek(11);
                if w == tables::MB_ESCAPE.bits {
                    self.br.skip(11);
                    inc += 33;
                    self.st.stats.mb_escape += 1;
                    if inc > 33 * 400 {
                        return Err(
                            self.abort(Rule::MbAddrVlc, || "endless macroblock_escape".to_string())
                        );
                    }
                } else if w == 0b000_0000_1111 {
                    self.emit(Rule::MbStuffing, || {
                        "macroblock_stuffing ('0000 0001 111') of ISO/IEC 11172-2".to_string()
                    });
                    self.br.skip(11);
                } else {
                    break;
                }
            }
            let Some((idx, len)) = self
                .t
                .mb_addr_inc
                .get(self.br.peek(self.t.mb_addr_inc.bits()))
            else {
                let w = self.br.peek(11);
                return Err(self.abort(Rule::MbAddrVlc, || {
                    format!("no macroblock_address_increment code starts with {w:011b}")
                }));
            };
            self.br.skip(len);
            inc += u32::from(tables::MB_ADDR_INC.get(idx).map_or(1, |c| c.1));
            let addr64 = prev_addr + i64::from(inc);
            if addr64 > i64::from(row_end) {
                self.mb = Some(addr64.min(i64::from(u32::MAX)) as u32);
                return Err(self.abort(Rule::SliceRowOverrun, || {
                    format!(
                        "macroblock_address {} is beyond the end of row {} (last address {})",
                        addr64,
                        row + 1,
                        row_end
                    )
                }));
            }
            let addr = addr64 as u32;
            self.mb = Some(addr);
            let col = addr - row_base;
            if first {
                let next = self.st.next_mb;
                if std::mem::take(&mut self.st.resync) && addr < next {
                    // continues the row of a slice that was not decoded to its end
                } else if addr < next {
                    self.emit(Rule::SliceOrder, || {
                        format!(
                            "the slice starts at macroblock {addr}, but slices already cover up to macroblock {}",
                            next - 1
                        )
                    });
                } else if addr > next {
                    self.emit(Rule::SliceGap, || {
                        format!(
                            "macroblocks {next} to {} are in no slice (the slice starts at row {}, column {})",
                            addr - 1,
                            row + 1,
                            col
                        )
                    });
                }
            } else if inc > 1 {
                self.skipped(prev_addr as u32 + 1, addr, prev_intra, prev_dirs);
            }

            // ---- macroblock_modes()
            let ty_lut = self.t.mb_type.get(usize::from(pp.ptype) - 1).ok_or(Abort)?;
            let Some((ti, tlen)) = ty_lut.get(self.br.peek(ty_lut.bits())) else {
                let w = self.br.peek(6);
                return Err(self.abort(Rule::MbTypeVlc, || {
                    format!("no macroblock_type code of this picture type starts with {w:06b}")
                }));
            };
            self.br.skip(tlen);
            let table = match pp.ptype {
                I => tables::MB_TYPE_I,
                P => tables::MB_TYPE_P,
                _ => tables::MB_TYPE_B,
            };
            let ty = table.get(ti).copied().ok_or(Abort)?;
            let frame_pic = pp.structure == FRAME;
            let mut mode = if frame_pic { Mode::Frame } else { Mode::Field };
            if ty.motion_forward || ty.motion_backward {
                if frame_pic {
                    if !pp.fpfd {
                        mode = match self.br.read(2) {
                            1 => Mode::FieldInFrame,
                            2 => Mode::Frame,
                            3 => Mode::DualPrime,
                            _ => {
                                return Err(self.abort(Rule::MbMotionType, || {
                                    "frame_motion_type '00' is reserved".to_string()
                                }));
                            }
                        };
                    }
                } else {
                    mode = match self.br.read(2) {
                        1 => Mode::Field,
                        2 => Mode::Mc16x8,
                        3 => Mode::DualPrime,
                        _ => {
                            return Err(self.abort(Rule::MbMotionType, || {
                                "field_motion_type '00' is reserved".to_string()
                            }));
                        }
                    };
                }
            }
            if frame_pic && !pp.fpfd && (ty.intra || ty.pattern) && self.br.bit() {
                self.st.stats.field_dct += 1;
            }
            if ty.quant {
                let q = self.br.read(5);
                if q == 0 {
                    self.emit(Rule::MbQscaleZero, || {
                        "quantiser_scale_code of the macroblock is 0 (forbidden)".to_string()
                    });
                } else {
                    self.set_qscale(q);
                }
                self.st.stats.mb_quant += 1;
            }
            if mode == Mode::DualPrime {
                self.st.stats.dual_prime += 1;
                if pp.ptype != P || ty.motion_backward {
                    self.emit(Rule::MbDualPrime, || {
                        "dual-prime prediction outside a forward-predicted P-picture macroblock"
                            .to_string()
                    });
                }
            }

            // ---- motion vectors
            let mut vec = [[[0i32; 2]; 2]; 2];
            let mut fsel = [[false; 2]; 2];
            let mut dmv = [0i32; 2];
            let conceal = ty.intra && pp.cmv;
            if ty.motion_forward || conceal {
                self.motion_vectors(0, mode, &mut vec, &mut fsel, &mut dmv)?;
            }
            if ty.motion_backward {
                self.motion_vectors(1, mode, &mut vec, &mut fsel, &mut dmv)?;
            }
            if conceal {
                self.st.stats.conceal += 1;
                if !self.br.bit() {
                    self.emit(Rule::MbConcealMarker, || {
                        "marker_bit after the concealment motion vector is '0'".to_string()
                    });
                }
            }
            // predictor updates of Tables 7-9 and 7-10, resets of 7.6.3.4
            if ty.intra {
                if pp.cmv {
                    self.pmv[1][0] = self.pmv[0][0];
                } else {
                    self.pmv = [[[0; 2]; 2]; 2];
                }
            } else {
                match mode {
                    Mode::Frame | Mode::Field => {
                        if ty.motion_forward {
                            self.pmv[1][0] = self.pmv[0][0];
                        }
                        if ty.motion_backward {
                            self.pmv[1][1] = self.pmv[0][1];
                        }
                    }
                    Mode::DualPrime => self.pmv[1][0] = self.pmv[0][0],
                    Mode::FieldInFrame | Mode::Mc16x8 => {}
                }
                if pp.ptype == P && !ty.motion_forward {
                    self.pmv = [[[0; 2]; 2]; 2];
                }
            }
            if !ty.intra {
                self.predictions(
                    addr,
                    col,
                    row,
                    mode,
                    ty.motion_forward,
                    ty.motion_backward,
                    &vec,
                    &fsel,
                    &dmv,
                );
            }

            // ---- coded_block_pattern()
            let block_count: u32 = match pp.chroma {
                1 => 6,
                2 => 8,
                _ => 12,
            };
            let mut pattern: u32 = if ty.intra { (1 << block_count) - 1 } else { 0 };
            if ty.pattern {
                let Some((ci, clen)) = self.t.cbp.get(self.br.peek(self.t.cbp.bits())) else {
                    let w = self.br.peek(9);
                    return Err(self.abort(Rule::MbCbpVlc, || {
                        format!("no coded_block_pattern code starts with {w:09b}")
                    }));
                };
                self.br.skip(clen);
                let cbp = u32::from(tables::CBP.get(ci).map_or(0, |c| c.1));
                match pp.chroma {
                    1 => {
                        if cbp == 0 {
                            self.emit(Rule::MbCbpZero, || {
                                "coded_block_pattern is 0 with 4:2:0 (the code '0000 0000 1' shall not be used)".to_string()
                            });
                        }
                        pattern = cbp;
                    }
                    2 => pattern = (cbp << 2) | self.br.read(2),
                    _ => pattern = (cbp << 6) | self.br.read(6),
                }
                self.st.stats.pattern += 1;
            }

            // ---- block()
            if ty.intra {
                self.st.stats.intra += 1;
            } else {
                self.reset_dc();
                non_intra_in_slice = true;
                self.st.non_intra_mbs += 1;
            }
            for i in 0..block_count {
                if pattern & (1 << (block_count - 1 - i)) != 0 {
                    self.block(i, ty.intra)?;
                }
            }
            if self.br.overrun() {
                return Err(self.abort(Rule::SliceTruncated, String::new));
            }

            // ---- per-macroblock bookkeeping
            let s = &mut self.st.stats;
            s.coded += 1;
            if !ty.intra {
                match (ty.motion_forward, ty.motion_backward) {
                    (true, true) => s.bidirectional += 1,
                    (true, false) => s.forward += 1,
                    (false, true) => s.backward += 1,
                    (false, false) => s.no_mc += 1,
                }
                match mode {
                    Mode::FieldInFrame => s.field_pred_in_frame += 1,
                    Mode::Mc16x8 => s.mc16x8 += 1,
                    _ => {}
                }
                if ty.motion_forward || pp.ptype == P {
                    self.st.forward_pred_mbs += 1;
                }
            }
            let bits = (self.br.pos() - mb_start) as u32;
            if bits > self.st.stats.max_mb_bits {
                self.st.stats.max_mb_bits = bits;
            }
            if bits > pp.mb_bit_limit {
                self.st.stats.big_mbs += 1;
                self.st.big_in_row += 1;
                if self.st.big_in_row > 2 {
                    let n = self.st.big_in_row;
                    let limit = pp.mb_bit_limit;
                    let p = self.pos_at(mb_start);
                    self.sink.emit(Rule::MbBits, p, || {
                        format!(
                            "macroblock of {bits} bits; it is number {n} above {limit} bits in row {} (two are allowed)",
                            row + 1
                        )
                    });
                }
            }
            if pp.second_field_after_i && !ty.intra && !ty.motion_forward {
                self.emit(Rule::MbPFieldAfterI, || {
                    "no-MC macroblock in a P field that follows the I field of its frame (it would predict from the same parity field of the previous frame)".to_string()
                });
            }
            prev_intra = ty.intra;
            prev_dirs = (ty.motion_forward, ty.motion_backward);
            prev_addr = i64::from(addr);
            first = false;
            if addr + 1 > self.st.next_mb {
                self.st.next_mb = addr + 1;
            }
            if self.br.peek(23) == 0 {
                break;
            }
        }
        if intra_slice && non_intra_in_slice {
            self.mb = None;
            let p = self.base;
            self.sink.emit(Rule::SliceIntraFlag, p, || {
                "intra_slice is '1' but the slice has non-intra macroblocks".to_string()
            });
        }
        if !self.br.rest_is_zero() {
            self.emit(Rule::StuffNonZero, || {
                "non-zero bits after the 23 zero bits that end the slice".to_string()
            });
        }
        Ok(())
    }

    fn set_qscale(&mut self, code: u32) {
        let table = if self.pp.q_scale_type {
            &tables::QUANTIZER_SCALE_NON_LINEAR
        } else {
            &tables::QUANTIZER_SCALE_LINEAR
        };
        self.qscale = i32::from(table.get(code as usize).copied().unwrap_or(2));
    }

    /// Macroblocks `from..to` are skipped.
    fn skipped(&mut self, from: u32, to: u32, prev_intra: bool, prev_dirs: (bool, bool)) {
        let pp = self.pp;
        let n = u64::from(to - from);
        self.reset_dc();
        match pp.ptype {
            I => {
                self.mb = Some(from);
                self.emit(Rule::MbSkipI, || {
                    format!("{n} skipped macroblock(s) in an I-picture, from address {from}")
                });
            }
            P => {
                self.st.stats.skipped_p += n;
                self.st.forward_pred_mbs += n;
                self.st.non_intra_mbs += n;
                self.pmv = [[[0; 2]; 2]; 2];
                if pp.second_field_after_i {
                    self.mb = Some(from);
                    self.emit(Rule::MbPFieldAfterI, || {
                        format!("{n} skipped macroblock(s) in a P field that follows the I field of its frame")
                    });
                }
            }
            _ => {
                self.st.stats.skipped_b += n;
                self.st.non_intra_mbs += n;
                if prev_intra {
                    self.mb = Some(from);
                    self.emit(Rule::MbSkipBAfterIntra, || {
                        format!("{n} skipped macroblock(s) in a B-picture right after an intra macroblock, from address {from}")
                    });
                } else {
                    if prev_dirs.0 {
                        self.st.forward_pred_mbs += n;
                    }
                    for a in from..to {
                        let col = a % pp.mb_w;
                        let row = a / pp.mb_w;
                        for s in 0..2usize {
                            let used = if s == 0 { prev_dirs.0 } else { prev_dirs.1 };
                            if !used {
                                continue;
                            }
                            let v = self.pmv[0][s];
                            // frame-based in a frame picture, field-based (same parity) in a
                            // field picture: a 16x16 region of the reference either way
                            let ok =
                                self.inside(col * 16, row * 16, 16, 16, v, self.ref_w, self.ref_h);
                            if !ok {
                                self.mb = Some(a);
                                self.emit(Rule::MvOutside, || {
                                    format!(
                                        "skipped macroblock at column {col}, row {row}: the predictor vector ({}, {}) half samples points outside the coded picture",
                                        v[0], v[1]
                                    )
                                });
                            }
                        }
                    }
                }
            }
        }
        self.mb = Some(to);
    }

    /// motion_vectors(s) of 6.2.5.2 with the decoding of 7.6.3.1.
    fn motion_vectors(
        &mut self,
        s: usize,
        mode: Mode,
        vec: &mut [[[i32; 2]; 2]; 2],
        fsel: &mut [[bool; 2]; 2],
        dmv: &mut [i32; 2],
    ) -> Result<(), Abort> {
        let count = if matches!(mode, Mode::FieldInFrame | Mode::Mc16x8) {
            2
        } else {
            1
        };
        let field_format = mode != Mode::Frame;
        let dual = mode == Mode::DualPrime;
        for r in 0..count {
            if count == 2 || (field_format && !dual) {
                fsel[r][s] = self.br.bit();
            }
            for t in 0..2usize {
                let Some((mi, mlen)) = self
                    .t
                    .motion_code
                    .get(self.br.peek(self.t.motion_code.bits()))
                else {
                    let w = self.br.peek(11);
                    return Err(self.abort(Rule::MvCodeVlc, || {
                        format!("no motion_code starts with {w:011b}")
                    }));
                };
                self.br.skip(mlen);
                let code = i32::from(tables::MOTION_CODE.get(mi).map_or(0, |c| c.1));
                let f_code = self.pp.f_code[s][t];
                if !(1..=9).contains(&f_code) {
                    return Err(self.abort(Rule::MvFcodeUnusable, || {
                        format!("a motion vector is coded with f_code[{s}][{t}] = {f_code}")
                    }));
                }
                let r_size = u32::from(f_code) - 1;
                let f = 1i32 << r_size;
                let high = 16 * f - 1;
                let low = -16 * f;
                let range = 32 * f;
                let delta = if f == 1 || code == 0 {
                    code
                } else {
                    let residual = self.br.read(r_size) as i32;
                    let d = (code.abs() - 1) * f + residual + 1;
                    if code < 0 { -d } else { d }
                };
                if dual {
                    dmv[t] = if self.br.bit() {
                        if self.br.bit() { -1 } else { 1 }
                    } else {
                        0
                    };
                }
                if delta < low || delta > high {
                    self.emit(Rule::MvDeltaRange, || {
                        format!("motion vector delta {delta} is outside [{low}:{high}] of f_code {f_code}")
                    });
                }
                let halve = field_format && t == 1 && self.pp.structure == FRAME;
                let prediction = if halve {
                    self.pmv[r][s][t] >> 1
                } else {
                    self.pmv[r][s][t]
                };
                let mut v = prediction + delta;
                if v < low {
                    v += range;
                }
                if v > high {
                    v -= range;
                }
                if halve {
                    let p = v * 2;
                    if p < low || p > high {
                        self.emit(Rule::MvFieldHalfRange, || {
                            format!(
                                "vertical field vector {v} in a frame picture makes the predictor {p}, outside [{low}:{high}] of f_code {f_code}"
                            )
                        });
                    }
                    self.pmv[r][s][t] = p;
                } else {
                    self.pmv[r][s][t] = v;
                }
                vec[r][s][t] = v;
                let st = &mut self.st.stats;
                if t == 0 {
                    st.max_vec_h = st.max_vec_h.max(v.abs());
                } else {
                    st.max_vec_v = st.max_vec_v.max(v.abs());
                    if let Some((lo, hi)) = self.pp.level_vrange
                        && (v < lo || v > hi)
                    {
                        self.emit(Rule::MvLevelVertical, || {
                            format!("vertical vector component {v} half samples is outside the level's range [{lo}:{hi}]")
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// True if the prediction of a `w` x `h` luminance region at (x, y) with vector `v` (half
    /// samples) lies inside a reference of `rw` x `rh` luminance samples, for luminance and
    /// for the scaled chrominance vector of 7.6.3.7.
    #[allow(clippy::too_many_arguments)]
    fn inside(&self, x: u32, y: u32, w: i32, h: i32, v: [i32; 2], rw: i32, rh: i32) -> bool {
        #[inline]
        fn axis(p: i32, size: i32, v: i32, lim: i32) -> bool {
            let start = p + (v >> 1);
            let end = start + size + (v & 1);
            start >= 0 && end <= lim
        }
        let (x, y) = (x as i32, y as i32);
        if !(axis(x, w, v[0], rw) && axis(y, h, v[1], rh)) {
            return false;
        }
        let (sx, sy) = match self.pp.chroma {
            1 => (1, 1),
            2 => (1, 0),
            _ => (0, 0),
        };
        let cvx = if sx == 1 { div2_trunc(v[0]) } else { v[0] };
        let cvy = if sy == 1 { div2_trunc(v[1]) } else { v[1] };
        axis(x >> sx, w >> sx, cvx, rw >> sx) && axis(y >> sy, h >> sy, cvy, rh >> sy)
    }

    /// Checks every prediction of a non-intra macroblock against 7.6.3.8, and the level's
    /// vertical range for the dual-prime arithmetic.
    #[allow(clippy::too_many_arguments)]
    fn predictions(
        &mut self,
        addr: u32,
        col: u32,
        row: u32,
        mode: Mode,
        forward: bool,
        backward: bool,
        vec: &[[[i32; 2]; 2]; 2],
        fsel: &[[bool; 2]; 2],
        dmv: &[i32; 2],
    ) {
        let pp = self.pp;
        let (x, y) = (col * 16, row * 16);
        let (rw, rh) = (self.ref_w, self.ref_h);
        let mut bad: Option<(&'static str, [i32; 2])> = None;
        for s in 0..2usize {
            if !(if s == 0 { forward } else { backward }) {
                continue;
            }
            match mode {
                Mode::Frame | Mode::Field => {
                    if !self.inside(x, y, 16, 16, vec[0][s], rw, rh) {
                        bad = Some((
                            if mode == Mode::Frame {
                                "frame"
                            } else {
                                "field"
                            },
                            vec[0][s],
                        ));
                    }
                }
                Mode::FieldInFrame => {
                    for v in vec.iter().map(|r| r[s]) {
                        if !self.inside(x, y / 2, 16, 8, v, rw, rh / 2) {
                            bad = Some(("field (in a frame picture)", v));
                        }
                    }
                }
                Mode::Mc16x8 => {
                    for (r, v) in vec.iter().map(|r| r[s]).enumerate() {
                        if !self.inside(x, y + 8 * r as u32, 16, 8, v, rw, rh) {
                            bad = Some(("16x8", v));
                        }
                    }
                }
                Mode::DualPrime => {
                    let v = vec[0][0];
                    let frame_pic = pp.structure == FRAME;
                    // (m, e) for each field being predicted from the field of opposite parity
                    let cases: &[(i32, i32)] = if frame_pic {
                        // top field predicted from bottom: m[1][0], e = -1;
                        // bottom field predicted from top: m[0][1], e = +1
                        if pp.tff {
                            &[(1, -1), (3, 1)]
                        } else {
                            &[(3, -1), (1, 1)]
                        }
                    } else if pp.structure == 1 {
                        &[(1, -1)]
                    } else {
                        &[(1, 1)]
                    };
                    let (ry, rhh, hh) = if frame_pic {
                        (y / 2, rh / 2, 8)
                    } else {
                        (y, rh, 16)
                    };
                    if !self.inside(x, ry, 16, hh, v, rw, rhh) {
                        bad = Some(("dual-prime same-parity", v));
                    }
                    for &(m, e) in cases {
                        let scaled = div2_round(v[1] * m);
                        let d = [div2_round(v[0] * m) + dmv[0], scaled + e + dmv[1]];
                        if !self.inside(x, ry, 16, hh, d, rw, rhh) {
                            bad = Some(("dual-prime derived", d));
                        }
                        if let Some((lo, hi)) = pp.level_vrange {
                            for val in [scaled, scaled + e, scaled + dmv[1], scaled + e + dmv[1]] {
                                if val < lo || val > hi {
                                    self.emit(Rule::MvLevelVertical, || {
                                        format!("dual-prime vertical value {val} half samples is outside the level's range [{lo}:{hi}]")
                                    });
                                }
                            }
                        }
                    }
                }
            }
            if pp.second_field_after_i && s == 0 {
                let cur_bottom = pp.structure == 2;
                let same = match mode {
                    Mode::DualPrime => true,
                    Mode::Mc16x8 => fsel[0][0] == cur_bottom || fsel[1][0] == cur_bottom,
                    _ => fsel[0][0] == cur_bottom,
                };
                if same {
                    self.emit(Rule::MbPFieldAfterI, || {
                        "prediction from the same parity field (or dual prime) in a P field that follows the I field of its frame".to_string()
                    });
                }
            }
        }
        if let Some((kind, v)) = bad {
            self.mb = Some(addr);
            self.emit(Rule::MvOutside, || {
                format!(
                    "{kind} prediction of the macroblock at column {col}, row {row} with vector ({}, {}) half samples refers to samples outside the coded picture ({}x{})",
                    v[0], v[1], rw, rh
                )
            });
        }
    }

    /// block(i) of 6.2.6 with 7.2 and the reconstruction of 7.4.
    fn block(&mut self, i: u32, intra: bool) -> Result<(), Abort> {
        let pp = self.pp;
        let cc = if i < 4 { 0usize } else { 1 + (i as usize & 1) };
        let chroma_matrix = cc != 0 && pp.chroma != 1;
        let wq = &pp.wq[usize::from(!intra) + if chroma_matrix { 2 } else { 0 }];
        let lut = &self.t.dct[usize::from(intra && pp.intra_vlc)];
        let qscale = self.qscale;
        let mut n: u32;
        self.st.stats.blocks += 1;
        let mut coefs = 0u64;
        if intra {
            let size_lut = if cc == 0 {
                &self.t.dc_size_luma
            } else {
                &self.t.dc_size_chroma
            };
            let Some((si, slen)) = size_lut.get(self.br.peek(size_lut.bits())) else {
                return Err(self.abort(Rule::BlkCoefVlc, || "no dct_dc_size code".to_string()));
            };
            self.br.skip(slen);
            let table = if cc == 0 {
                tables::DC_SIZE_LUMA
            } else {
                tables::DC_SIZE_CHROMA
            };
            let size = u32::from(table.get(si).map_or(0, |c| c.1));
            let diff = if size == 0 {
                0
            } else {
                let v = self.br.read(size) as i32;
                let half = 1i32 << (size - 1);
                if v >= half { v } else { v + 1 - 2 * half }
            };
            let qfs0 = self.dc_pred[cc] + diff;
            let max = (1i32 << (8 + i32::from(pp.dc_prec))) - 1;
            if qfs0 < 0 || qfs0 > max {
                self.emit(Rule::BlkDcRange, || {
                    format!(
                        "intra DC coefficient {qfs0} of block {i} (differential {diff}, dct_dc_size {size}) is outside 0 to {max}"
                    )
                });
            }
            if qfs0 == 0 || qfs0 == max {
                self.st.stats.full_dc_range += 1;
            }
            self.dc_pred[cc] = qfs0;
            n = 1;
        } else {
            n = 0;
        }
        let mut first = !intra;
        loop {
            let run: u32;
            let level: i32;
            if first && self.br.peek(1) == 1 {
                // Table B.14 note 3: '1 s' is run 0, level 1 for the first coefficient
                let s = self.br.read(2) & 1;
                run = 0;
                level = if s == 1 { -1 } else { 1 };
            } else {
                let (sym, len) = lut.get(self.br.peek(16));
                match sym {
                    DctSym::Invalid => {
                        let w = self.br.peek(16);
                        return Err(self.abort(Rule::BlkCoefVlc, || {
                            format!("no DCT coefficient code starts with {w:016b} (block {i}, coefficient index {n})")
                        }));
                    }
                    DctSym::Eob => {
                        self.br.skip(len);
                        break;
                    }
                    DctSym::Escape => {
                        self.br.skip(len);
                        run = self.br.read(6);
                        let raw = self.br.read(12);
                        level = if raw & 0x800 != 0 {
                            raw as i32 - 4096
                        } else {
                            raw as i32
                        };
                        self.st.stats.escapes += 1;
                        if raw == 0 {
                            self.emit(Rule::BlkEscZero, || {
                                format!("escape with signed_level '0000 0000 0000' (forbidden), block {i}")
                            });
                        } else if raw == 0x800 {
                            self.emit(Rule::BlkEscReserved, || {
                                format!("escape with signed_level '1000 0000 0000' (reserved), block {i}")
                            });
                        } else if lut.has_code(run, level.unsigned_abs()) {
                            self.emit(Rule::BlkEscRedundant, || {
                                format!("escape for run {run}, level {level}, which has a code in the table, block {i}")
                            });
                        }
                    }
                    DctSym::Coef(r, l) => {
                        self.br.skip(len);
                        run = u32::from(r);
                        level = if self.br.bit() {
                            -i32::from(l)
                        } else {
                            i32::from(l)
                        };
                    }
                }
            }
            first = false;
            n += run;
            if n > 63 {
                return Err(self.abort(Rule::BlkOverrun, || {
                    format!(
                        "block {i}: a run of {run} carries the coefficient index to {n}, past 63"
                    )
                }));
            }
            let w = i32::from(wq[n as usize]);
            let f = if intra {
                (2 * level * w * qscale) / 32
            } else {
                ((2 * level + level.signum()) * w * qscale) / 32
            };
            if !(-2048..=2047).contains(&f) {
                self.emit(Rule::BlkSaturation, || {
                    format!(
                        "block {i}, coefficient index {n}: level {level} with weight {w} and quantiser_scale {qscale} reconstructs to {f}, saturated"
                    )
                });
            }
            let a = level.unsigned_abs();
            if a > self.st.stats.max_abs_level {
                self.st.stats.max_abs_level = a;
            }
            coefs += 1;
            n += 1;
        }
        self.st.stats.coefficients += coefs;
        if coefs > 0 {
            self.st.stats.nonzero_residual += 1;
        }
        Ok(())
    }
}
