//! The pictures the harness makes itself: the slate (one per format) and the overlay planes,
//! byte for byte as the C++ harness made them.

/// The slate at the coded (padded) size `gw` x `gh` for a picture of `w` x `h`: color bars
/// with a dark box in the middle, as three planes Y, U, V (chroma `gw / 2` x `gh / 2`). The
/// encoder's own padding replaces what lies outside the picture.
pub fn slate(w: u32, h: u32, gw: u32, gh: u32) -> [Vec<u8>; 3] {
    const BARS: [[u8; 3]; 8] = [
        [180, 128, 128],
        [162, 44, 142],
        [131, 156, 44],
        [112, 72, 58],
        [84, 184, 198],
        [65, 100, 212],
        [35, 212, 114],
        [16, 128, 128],
    ];
    let (w, h, gw, gh) = (w as i64, h as i64, gw as usize, gh as usize);
    let mut y = vec![128u8; gw * gh];
    let mut u = vec![128u8; gw * gh / 4];
    let mut v = vec![128u8; gw * gh / 4];
    for row in 0..gh {
        for x in 0..gw {
            let (xi, yi) = (x as i64, row as i64);
            let k = (xi * 8 / w).min(7) as usize;
            let boxed = (xi - w / 2).abs() < w / 6 && (yi - h / 2).abs() < h / 8;
            y[row * gw + x] = if boxed { 40 } else { BARS[k][0] };
            if row % 2 == 0 && x % 2 == 0 {
                let ci = (row / 2) * (gw / 2) + x / 2;
                u[ci] = if boxed { 128 } else { BARS[k][1] };
                v[ci] = if boxed { 128 } else { BARS[k][2] };
            }
        }
    }
    [y, u, v]
}

/// The overlay of a format: a logo box and a banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayPicture {
    /// Luma, `w` x `h`.
    pub y: Vec<u8>,
    /// Alpha per luma sample.
    pub a: Vec<u8>,
    /// Cb, `w / 2` x `h / 2`.
    pub u: Vec<u8>,
    /// Cr.
    pub v: Vec<u8>,
    /// Alpha per chroma sample.
    pub a2: Vec<u8>,
}

/// The overlay planes at picture size.
pub fn overlay(w: u32, h: u32) -> OverlayPicture {
    let (w, h) = (w as usize, h as usize);
    let (ys, cs) = (w * h, w * h / 4);
    let mut o = OverlayPicture {
        y: vec![0; ys],
        a: vec![0; ys],
        u: vec![0; cs],
        v: vec![0; cs],
        a2: vec![0; cs],
    };
    for row in 0..h {
        for x in 0..w {
            let logo = x > w * 7 / 8 && x < w * 31 / 32 && row > h / 16 && row < h / 6;
            let banner = row > h * 13 / 16 && row < h * 15 / 16;
            let a = if logo {
                255
            } else if banner {
                160
            } else {
                0
            };
            o.y[row * w + x] = if logo {
                235
            } else {
                (40 + ((x / 16 + row / 16) & 1) * 30) as u8
            };
            o.a[row * w + x] = a;
            if row % 2 == 0 && x % 2 == 0 {
                let ci = (row / 2) * (w / 2) + x / 2;
                o.u[ci] = if logo { 128 } else { 160 };
                o.v[ci] = if logo { 128 } else { 110 };
                o.a2[ci] = a;
            }
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slate_has_bars_and_a_box() {
        let [y, u, v] = slate(704, 480, 704, 480);
        assert_eq!(y[0], 180);
        assert_eq!(y[703], 16);
        assert_eq!(y[240 * 704 + 352], 40);
        assert_eq!((u[0], v[0]), (128, 128));
        assert_eq!((u[100], v[100]), (156, 44));
        // the padded size: the bars go on to the right, row 1087 exists
        let [y, _, _] = slate(1920, 1080, 1920, 1088);
        assert_eq!(y.len(), 1920 * 1088);
        assert_eq!(y[1087 * 1920], 180);
    }

    #[test]
    fn overlay_alpha() {
        let o = overlay(1280, 720);
        assert_eq!(o.a[0], 0);
        assert_eq!(o.a[620 * 1280 + 10], 160);
        let logo = 80 * 1280 + 1200;
        assert_eq!((o.a[logo], o.y[logo]), (255, 235));
    }
}
