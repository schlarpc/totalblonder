//! Active Format Description (ATSC A/53 Part 4 §6.2.4, Tables 6.13 and 6.14): which part of the
//! coded frame holds the picture, for receivers that zoom or crop.
//!
//! The code is chosen by containment: the smallest area of interest Table 6.14 offers for the
//! frame's aspect ratio that still contains the whole picture. A receiver acting on it never
//! crops picture, only bars. Content whose aspect matches no code exactly (wider than 16:9 in a
//! 16:9 frame, narrower than 4:3 in a 4:3 frame, anything in between) gets the next larger
//! area, at worst the full frame. Codes 0000 and 0100 need bar data, whose line numbering
//! (SMPTE 2016-1) we do not hold, so they are never chosen.

/// 1000: the area of interest is the full frame (either frame aspect).
pub const FULL: u8 = 0b1000;
/// 1001 in a 16:9 frame: a 4:3 picture, pillarboxed.
pub const PILLAR_4_3: u8 = 0b1001;
/// 1010 in a 4:3 frame: a 16:9 picture, letterboxed.
pub const LETTER_16_9: u8 = 0b1010;
/// 1011: a 14:9 picture, pillarboxed in a 16:9 frame or letterboxed in a 4:3 frame.
pub const BOX_14_9: u8 = 0b1011;

/// How close two aspect ratios must be to count as the same (the converter's own tolerance,
/// `tb_convert::make_fit`).
const SAME: f64 = 0.01;

/// The active_format for a picture of display aspect `content` shown in a frame of display
/// aspect 16:9 (`wide`) or 4:3. `content` is the aspect of the rectangle the picture fills,
/// width over height, in display units (frame aspect × its share of the width ÷ its share of
/// the height).
pub fn code(wide: bool, content: f64) -> u8 {
    let fits_in = |area: f64| content <= area * (1.0 + SAME);
    let covers = |area: f64| content >= area * (1.0 - SAME);
    if !content.is_finite() || content <= 0.0 {
        return FULL;
    }
    if wide {
        // pillarbox areas, narrowest first: the picture must fit inside
        if fits_in(4.0 / 3.0) {
            PILLAR_4_3
        } else if fits_in(14.0 / 9.0) {
            BOX_14_9
        } else {
            FULL
        }
    } else if covers(16.0 / 9.0) {
        // letterbox areas, shortest first: the picture must be at least as wide
        LETTER_16_9
    } else if covers(14.0 / 9.0) {
        BOX_14_9
    } else {
        FULL
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

/// Whether `af` is a value [`code`] can give (the ones this crate writes).
pub fn is_written(af: u8) -> bool {
    matches!(af, FULL | PILLAR_4_3 | LETTER_16_9 | BOX_14_9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_nine_frame() {
        assert_eq!(code(true, 16.0 / 9.0), FULL);
        assert_eq!(code(true, 4.0 / 3.0), PILLAR_4_3);
        assert_eq!(code(true, 14.0 / 9.0), BOX_14_9);
        // narrower than 4:3 (a portrait phone video): inside the 4:3 area
        assert_eq!(code(true, 9.0 / 16.0), PILLAR_4_3);
        // between 14:9 and 16:9 (1.66): only the full frame contains it
        assert_eq!(code(true, 1.66), FULL);
        // wider than 16:9 (2.39 letterboxed): full frame, no 0100 without bar data
        assert_eq!(code(true, 2.39), FULL);
    }

    #[test]
    fn four_three_frame() {
        assert_eq!(code(false, 4.0 / 3.0), FULL);
        assert_eq!(code(false, 16.0 / 9.0), LETTER_16_9);
        assert_eq!(code(false, 14.0 / 9.0), BOX_14_9);
        // wider than 16:9: the 16:9 band contains it
        assert_eq!(code(false, 2.39), LETTER_16_9);
        // 1.66: wider than 14:9, narrower than 16:9
        assert_eq!(code(false, 1.66), BOX_14_9);
        // narrower than 4:3: pillarboxed, no code for that but the full frame
        assert_eq!(code(false, 1.0), FULL);
    }

    #[test]
    fn rounding_of_the_converter_is_tolerated() {
        // 1440x1080 of 1920x1080 at 16:9 (a 4:3 source), and 1436 after rounding to 8
        assert_eq!(code(true, content_aspect(16.0 / 9.0, 1920, 1080, 1440, 1080)), PILLAR_4_3);
        assert_eq!(code(true, content_aspect(16.0 / 9.0, 1920, 1080, 1436, 1080)), PILLAR_4_3);
        // 704x480 at 4:3 with a 16:9 source: 360 lines, and a line pair either way
        assert_eq!(code(false, content_aspect(4.0 / 3.0, 704, 480, 704, 360)), LETTER_16_9);
        assert_eq!(code(false, content_aspect(4.0 / 3.0, 704, 480, 704, 358)), LETTER_16_9);
        assert_eq!(code(false, content_aspect(4.0 / 3.0, 704, 480, 704, 362)), LETTER_16_9);
    }

    #[test]
    fn nonsense_is_full_frame() {
        assert_eq!(code(true, f64::NAN), FULL);
        assert_eq!(code(false, 0.0), FULL);
        assert_eq!(content_aspect(16.0 / 9.0, 0, 0, 0, 0), 16.0 / 9.0);
    }
}
