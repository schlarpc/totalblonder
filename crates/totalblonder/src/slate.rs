//! The slate (SPEC §9.5, level 3): generated in-process, with no dependency on any input or
//! library. 75 % color bars, the channel's number and name in a built-in 5×7 font, and a
//! marker that moves once a second, so a frozen picture is distinguishable from a live slate.

/// 75 % bars, limited range: white, yellow, cyan, green, magenta, red, blue as (Y, Cb, Cr).
/// BT.709 for HD and BT.601 for SD, matching the color description the encoder writes for
/// the picture height.
const BARS_709: [(u8, u8, u8); 7] = [
    (180, 128, 128),
    (168, 44, 136),
    (145, 147, 44),
    (133, 63, 52),
    (63, 193, 204),
    (51, 109, 212),
    (28, 212, 120),
];
const BARS_601: [(u8, u8, u8); 7] = [
    (180, 128, 128),
    (162, 44, 142),
    (131, 156, 44),
    (112, 72, 58),
    (84, 184, 198),
    (65, 100, 212),
    (35, 212, 114),
];

/// 5×7 glyphs, one byte per row, bit 4 the leftmost column.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        _ => [0; 7],
    }
}

/// A slate for one program.
pub struct Slate {
    w: usize,
    h: usize,
    base: Vec<u8>,
    marker: Option<usize>,
}

impl Slate {
    /// The slate of a `w`×`h` program labeled `text` (for example `20.1 WALL`).
    pub fn new(w: u32, h: u32, text: &str) -> Self {
        let (w, h) = (w as usize, h as usize);
        let mut base = vec![0u8; w * h * 3 / 2];
        let bars = if h > 576 { &BARS_709 } else { &BARS_601 };
        let (yp, rest) = base.split_at_mut(w * h);
        let (up, vp) = rest.split_at_mut(w * h / 4);
        let top = h * 2 / 3;
        for y in 0..h {
            for x in 0..w {
                let (yy, cb, cr) = if y < top {
                    bars[x * 7 / w]
                } else {
                    (16, 128, 128)
                };
                yp[y * w + x] = yy;
                if y % 2 == 0 && x % 2 == 0 {
                    up[(y / 2) * (w / 2) + x / 2] = cb;
                    vp[(y / 2) * (w / 2) + x / 2] = cr;
                }
            }
        }
        // Text in the lower third: scale so the line is at most 3/4 of the width.
        let chars = text.chars().count().max(1);
        let scale = ((w * 3 / 4) / (chars * 6)).min((h - top) / 10).max(1);
        let tw = chars * 6 * scale;
        let x0 = (w.saturating_sub(tw)) / 2;
        let y0 = top + (h - top - 7 * scale) / 2;
        for (i, c) in text.chars().enumerate() {
            let g = glyph(c);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = x0 + (i * 6 + col) * scale + dx;
                            let y = y0 + row * scale + dy;
                            if x < w && y < h {
                                yp[y * w + x] = 235;
                            }
                        }
                    }
                }
            }
        }
        Slate {
            w,
            h,
            base,
            marker: None,
        }
    }

    /// Forgets what was last written, so the next [`Slate::render`] writes the whole picture
    /// (the encoder's buffer held something else in between).
    pub fn invalidate(&mut self) {
        self.marker = None;
    }

    /// Writes the picture for second `sec` into `out` (`w·h·3/2` bytes, planar 4:2:0). Returns
    /// `false` when the picture is the same as the last one written (the caller may repeat).
    pub fn render(&mut self, sec: u64, out: &mut [u8]) -> bool {
        let pos = (sec % 8) as usize;
        if self.marker == Some(pos) {
            return false;
        }
        self.marker = Some(pos);
        let n = self.base.len().min(out.len());
        out[..n].copy_from_slice(&self.base[..n]);
        // A white block stepping along the bottom edge.
        let bw = self.w / 8;
        let bh = (self.h / 30).max(2) & !1;
        for y in self.h - bh..self.h {
            for x in pos * bw..(pos + 1) * bw {
                if let Some(p) = out.get_mut(y * self.w + x) {
                    *p = 235;
                }
            }
        }
        true
    }
}

/// A program that shows its slate.
pub struct SlateProvider(pub Slate);

impl crate::video::FrameProvider for SlateProvider {
    fn shot(&mut self, t: u64, len: usize) -> Option<crate::video::Shot> {
        let sec = t / crate::time::HZ;
        let mut buf = vec![0u8; len];
        self.0.invalidate();
        self.0.render(sec, &mut buf);
        Some(crate::video::Shot {
            key: tb_timeline::film::Key::Instant {
                src: u32::MAX,
                idx: sec as i64,
            },
            data: std::sync::Arc::new(buf),
        })
    }

    fn picture(&mut self, t: u64, out: &mut [u8]) -> crate::video::Picture {
        if self.0.render(t / crate::time::HZ, out) {
            crate::video::Picture::Fresh
        } else {
            crate::video::Picture::Repeat
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_repeats_within_a_second() {
        let mut s = Slate::new(704, 480, "20.1 WALL");
        let mut out = vec![0u8; 704 * 480 * 3 / 2];
        assert!(s.render(0, &mut out));
        assert!(!s.render(0, &mut out));
        assert!(s.render(1, &mut out));
        // Bars: the first column is 75 % white.
        assert_eq!(out[0], 180);
        // Every luma value is in the limited range.
        assert!(out[..704 * 480].iter().all(|&y| (16..=235).contains(&y)));
    }
}
