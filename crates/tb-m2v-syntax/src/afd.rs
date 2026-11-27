//! Active Format Description and bar data (ATSC A/53 Part 4 §6.2.3.2 and §6.2.4, Tables 6.11,
//! 6.13 and 6.14; SMPTE ST 2016-1:2009): which part of the coded frame holds the picture, for
//! receivers that zoom or crop.
//!
//! A picture whose aspect is one Table 6.14 names (4:3 or 14:9 pillarboxed in a 16:9 frame,
//! 16:9 or 14:9 letterboxed in a 4:3 frame) gets that code; any other boxed picture gets 0100
//! (wider than 16:9) or 0000 (anything else) with bar data, which gives the bars' exact extent
//! and which receivers use with those two codes (A/53-4 §6.2.4.2). Bar data is sent with every
//! boxed picture. Its line and pixel numbers are those of the format's production standard,
//! SMPTE ST 2016-1 Table 2:
//!
//! | Raster | Pixels | First field | Second field | Frame |
//! |---|---|---|---|---|
//! | 480i (SMPTE 125M) | 0–719 | 23–262 | 286–525 | |
//! | 720p (SMPTE 296M) | 0–1279 | | | 26–745 |
//! | 1080i (SMPTE 274M) | 0–1919 | 21–560 | 584–1123 | |
//! | 1080p (SMPTE 274M) | 0–1919 | | | 42–1121 |
//!
//! A 1080i frame's rows alternate first field (row 0 is line 21) and second field (row 1 is
//! line 584), as ST 2016-1 Annex B, Table B.2 shows. For 480i the standard does not say which
//! field holds the top row, and lets an interlaced bar's line come from either field (§6.6):
//! our bars are a whole number of row pairs, and each edge is signaled as the first-field line
//! of its pair, right whichever field is on top. 480-line pixels are numbered over 720 even
//! when 704 are coded (Table 2 note): coded pixel x is pixel 8 + x (SMPTE RP 202's alignment).

/// 1000: the area of interest is the full frame (either frame aspect).
pub const FULL: u8 = 0b1000;
/// 1001 in a 16:9 frame: a 4:3 picture, pillarboxed.
pub const PILLAR_4_3: u8 = 0b1001;
/// 1010 in a 4:3 frame: a 16:9 picture, letterboxed.
pub const LETTER_16_9: u8 = 0b1010;
/// 1011: a 14:9 picture, pillarboxed in a 16:9 frame or letterboxed in a 4:3 frame.
pub const BOX_14_9: u8 = 0b1011;
/// 0100: a picture wider than 16:9; bar data gives its height.
pub const WIDER: u8 = 0b0100;
/// 0000: a picture of no aspect the table names; bar data gives its extent.
pub const OTHER: u8 = 0b0000;

/// How close two aspect ratios must be to count as the same (the converter's own tolerance,
/// `tb_convert::make_fit`).
const SAME: f64 = 0.01;

/// A coded raster and its production standard's line and pixel numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Raster {
    /// 704 or 720 × 480 interlaced (SMPTE 125M).
    I480 {
        /// Coded pixels per line (704 or 720).
        width: u16,
    },
    /// 1280 × 720 progressive (SMPTE 296M).
    P720,
    /// 1920 × 1080 interlaced (SMPTE 274M).
    I1080,
    /// 1920 × 1080 progressive (SMPTE 274M).
    P1080,
}

impl Raster {
    /// The raster of a `w` × `h` coded frame, interlaced or not, if ST 2016-1 numbers it.
    pub fn of(w: u32, h: u32, interlaced: bool) -> Option<Raster> {
        match (w, h, interlaced) {
            (704 | 720, 480, true) => Some(Raster::I480 { width: w as u16 }),
            (1280, 720, false) => Some(Raster::P720),
            (1920, 1080, true) => Some(Raster::I1080),
            (1920, 1080, false) => Some(Raster::P1080),
            _ => None,
        }
    }

    /// The line number of coded row `r` (0 at the top).
    pub fn line(self, r: u32) -> u16 {
        let n = match self {
            // the first-field line of the row pair (§6.6: either field's line)
            Raster::I480 { .. } => 23 + r / 2,
            Raster::P720 => 26 + r,
            Raster::I1080 if r.is_multiple_of(2) => 21 + r / 2,
            Raster::I1080 => 584 + (r - 1) / 2,
            Raster::P1080 => 42 + r,
        };
        n as u16
    }

    /// The pixel number of coded pixel `x` (0 at the left).
    pub fn pixel(self, x: u32) -> u16 {
        let n = match self {
            Raster::I480 { width } => x + (720 - u32::from(width)) / 2,
            _ => x,
        };
        n as u16
    }
}

/// The extent of a boxed picture as bar data carries it (A/53-4 Table 6.11): one pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bars {
    /// Letterbox: the last line of the top bar and the first line of the bottom bar.
    TopBottom {
        /// line_number_end_of_top_bar.
        top_end: u16,
        /// line_number_start_of_bottom_bar.
        bottom_start: u16,
    },
    /// Pillarbox: the last pixel of the left bar and the first pixel of the right bar.
    LeftRight {
        /// pixel_number_end_of_left_bar.
        left_end: u16,
        /// pixel_number_start_of_right_bar.
        right_start: u16,
    },
}

/// What a picture's user data says about where the picture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActiveFormat {
    /// active_format.
    pub afd: u8,
    /// The bars, when the picture is boxed.
    pub bars: Option<Bars>,
}

impl ActiveFormat {
    /// The full frame, no bars.
    pub const FULL: ActiveFormat = ActiveFormat {
        afd: FULL,
        bars: None,
    };

    /// Whether this crate writes it: one of its codes, with bars where the code needs them
    /// (0000 and 0100) and the values inside 14 bits.
    pub fn is_written(&self) -> bool {
        let codes = matches!(
            self.afd,
            FULL | PILLAR_4_3 | LETTER_16_9 | BOX_14_9 | WIDER | OTHER
        );
        let needs = matches!(self.afd, WIDER | OTHER);
        let small = match self.bars {
            Some(Bars::TopBottom {
                top_end: a,
                bottom_start: b,
            })
            | Some(Bars::LeftRight {
                left_end: a,
                right_start: b,
            }) => a < 1 << 14 && b < 1 << 14,
            None => true,
        };
        codes && small && (!needs || self.bars.is_some())
    }
}

/// The active_format Table 6.14 names for a picture of display aspect `content` in a frame of
/// display aspect 16:9 (`wide`) or 4:3, if it names one: the full frame, 4:3 and 14:9 in a 16:9
/// frame, 16:9 and 14:9 in a 4:3 frame.
pub fn code(wide: bool, content: f64) -> Option<u8> {
    let near = |a: f64| (content / a - 1.0).abs() < SAME;
    if !content.is_finite() || content <= 0.0 {
        return None;
    }
    if near(if wide { 16.0 / 9.0 } else { 4.0 / 3.0 }) {
        Some(FULL)
    } else if near(14.0 / 9.0) {
        Some(BOX_14_9)
    } else if wide && near(4.0 / 3.0) {
        Some(PILLAR_4_3)
    } else if !wide && near(16.0 / 9.0) {
        Some(LETTER_16_9)
    } else {
        None
    }
}

/// The aspect of the rectangle `aw` × `ah` inside a `w` × `h` frame of display aspect
/// `frame` (width over height).
pub fn content_aspect(frame: f64, w: u32, h: u32, aw: u32, ah: u32) -> f64 {
    if w == 0 || h == 0 || ah == 0 {
        return frame;
    }
    frame * (f64::from(aw) / f64::from(w)) / (f64::from(ah) / f64::from(h))
}

/// The active format of a picture filling the rectangle at (`x`, `y`) of `rw` × `rh` in a
/// `w` × `h` frame of `raster`, display aspect 16:9 (`wide`) or 4:3. A picture filling the
/// frame is [`ActiveFormat::FULL`]; a boxed one carries its bars, and its code is the one
/// Table 6.14 names for its aspect, else 0100 (wider than 16:9) or 0000.
#[allow(clippy::too_many_arguments, reason = "a rectangle in a frame")]
pub fn describe(
    raster: Raster,
    wide: bool,
    w: u32,
    h: u32,
    x: u32,
    y: u32,
    rw: u32,
    rh: u32,
) -> ActiveFormat {
    let frame = if wide { 16.0 / 9.0 } else { 4.0 / 3.0 };
    let (rw, rh) = (rw.min(w), rh.min(h));
    let letter = rh < h && y > 0 && y + rh < h;
    let pillar = rw < w && x > 0 && x + rw < w;
    let bars = if letter {
        Some(Bars::TopBottom {
            top_end: raster.line(y - 1),
            bottom_start: raster.line(y + rh),
        })
    } else if pillar {
        Some(Bars::LeftRight {
            left_end: raster.pixel(x - 1),
            right_start: raster.pixel(x + rw),
        })
    } else {
        None
    };
    let Some(bars) = bars else {
        return ActiveFormat::FULL;
    };
    let content = content_aspect(frame, w, h, rw, rh);
    let afd = match code(wide, content) {
        Some(FULL) | None if content > 16.0 / 9.0 * (1.0 + SAME) => WIDER,
        Some(FULL) | None => OTHER,
        Some(c) => c,
    };
    ActiveFormat {
        afd,
        bars: Some(bars),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_aspects() {
        assert_eq!(code(true, 16.0 / 9.0), Some(FULL));
        assert_eq!(code(true, 4.0 / 3.0), Some(PILLAR_4_3));
        assert_eq!(code(true, 14.0 / 9.0), Some(BOX_14_9));
        assert_eq!(code(false, 16.0 / 9.0), Some(LETTER_16_9));
        assert_eq!(code(false, 4.0 / 3.0), Some(FULL));
        assert_eq!(code(true, 2.39), None);
        assert_eq!(code(true, 9.0 / 16.0), None);
        assert_eq!(code(false, f64::NAN), None);
    }

    /// The examples of SMPTE ST 2016-1 Annex B.
    #[test]
    fn annex_b() {
        // B.1: 2.4:1 in 1280x720: 533 lines, bars of 93 and 94
        let a = describe(Raster::P720, true, 1280, 720, 0, 93, 1280, 533);
        assert_eq!(
            a,
            ActiveFormat {
                afd: WIDER,
                bars: Some(Bars::TopBottom {
                    top_end: 118,
                    bottom_start: 652
                })
            }
        );
        // B.2: 2.4:1 in 1920x1080i: 800 lines, bars of 140
        let a = describe(Raster::I1080, true, 1920, 1080, 0, 140, 1920, 800);
        assert_eq!(
            a.bars,
            Some(Bars::TopBottom {
                top_end: 653,
                bottom_start: 491
            })
        );
        assert_eq!(a.afd, WIDER);
        // Table B.2's rows: top bar of 1, 2, 139 lines; bottom bar of 143, 6 lines
        assert_eq!(Raster::I1080.line(0), 21);
        assert_eq!(Raster::I1080.line(1), 584);
        assert_eq!(Raster::I1080.line(138), 90);
        assert_eq!(Raster::I1080.line(1080 - 143), 1052);
        assert_eq!(Raster::I1080.line(1080 - 6), 558);
        assert_eq!(Raster::I1080.line(1079), 1123);
        assert_eq!(Raster::P1080.line(1079), 1121);
    }

    #[test]
    fn our_converter_s_boxes() {
        // 4:3 in 1080i: pillarbox of 1440, bars of 240 pixels
        let a = describe(Raster::I1080, true, 1920, 1080, 240, 0, 1440, 1080);
        assert_eq!(
            a,
            ActiveFormat {
                afd: PILLAR_4_3,
                bars: Some(Bars::LeftRight {
                    left_end: 239,
                    right_start: 1680
                })
            }
        );
        // 16:9 in 480i (704 coded): 360 lines, bars of 60; numbering over 720 pixels
        let r = Raster::I480 { width: 704 };
        let a = describe(r, false, 704, 480, 0, 60, 704, 360);
        assert_eq!(a.afd, LETTER_16_9);
        // the first-field line of each edge's row pair: rows 58/59 and 420/421
        assert_eq!(
            a.bars,
            Some(Bars::TopBottom {
                top_end: 23 + 29,
                bottom_start: 23 + 210
            })
        );
        assert_eq!(r.pixel(0), 8);
        assert_eq!(r.pixel(703), 711);
        // a portrait picture in a 16:9 frame: 0000 with its pillars
        let a = describe(Raster::P720, true, 1280, 720, 438, 0, 404, 720);
        assert_eq!(a.afd, OTHER);
        assert!(matches!(a.bars, Some(Bars::LeftRight { .. })));
        // the full frame
        assert_eq!(
            describe(Raster::P1080, true, 1920, 1080, 0, 0, 1920, 1080),
            ActiveFormat::FULL
        );
        assert!(a.is_written() && ActiveFormat::FULL.is_written());
        assert!(
            !ActiveFormat {
                afd: WIDER,
                bars: None
            }
            .is_written()
        );
        assert!(
            !ActiveFormat {
                afd: 0b0001,
                bars: None
            }
            .is_written()
        );
    }
}
