//! Color matrices between BT.601 and BT.709 (the C++ `make_csc`).

/// Color matrix in Q12 on (Y, Cb - 128, Cr - 128) with offsets (`CscPar`, `make_csc`).
///
/// The C++ record also carries the `interlaced` flag of the frame it is applied to; here that
/// flag is an argument of the launch ([`crate::ConvLaunch::Csc`]), since it does not depend on
/// the matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csc {
    /// Row-major 3 x 3 matrix.
    pub m: [i32; 9],
    /// Offsets.
    pub off: [i32; 3],
}

/// YCbCr to RGB of the color description (Kr, Kb), on normalized values.
fn ycc2rgb(kr: f64, kb: f64) -> [[f64; 3]; 3] {
    let kg = 1.0 - kr - kb;
    [
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [
            1.0,
            -2.0 * kb * (1.0 - kb) / kg,
            -2.0 * kr * (1.0 - kr) / kg,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ]
}

/// RGB to YCbCr of the color description (Kr, Kb), on normalized values.
fn rgb2ycc(kr: f64, kb: f64) -> [[f64; 3]; 3] {
    let kg = 1.0 - kr - kb;
    [
        [kr, kg, kb],
        [-0.5 * kr / (1.0 - kb), -0.5 * kg / (1.0 - kb), 0.5],
        [0.5, -0.5 * kg / (1.0 - kr), -0.5 * kb / (1.0 - kr)],
    ]
}

/// `(int)std::lround(x)`.
fn lround(x: f64) -> i32 {
    x.round() as i64 as i32
}

/// The matrix between BT.601 and BT.709, limited or full range source (`make_csc`).
/// `from_full`: the source is full range (0..255); the destination is always limited range.
#[allow(clippy::needless_range_loop, reason = "the C++ loops, index for index")]
pub fn make_csc(from_709: bool, to_709: bool, from_full: bool) -> Csc {
    let a = if from_709 {
        ycc2rgb(0.2126, 0.0722)
    } else {
        ycc2rgb(0.299, 0.114)
    };
    let b = if to_709 {
        rgb2ycc(0.2126, 0.0722)
    } else {
        rgb2ycc(0.299, 0.114)
    };
    let mut m = [[0.0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = 0.0;
            for k in 0..3 {
                m[i][j] += b[i][k] * a[k][j];
            }
        }
    }
    // m acts on normalized (Y in 0..1, C in -0.5..0.5). Source code values: limited Y = 16 + 219 y,
    // C = 128 + 224 c; full Y = 255 y, C = 128 + 255 c.
    let sy = if from_full { 255.0 } else { 219.0 };
    let sc = if from_full { 255.0 } else { 224.0 };
    let oy = if from_full { 0.0 } else { 16.0 };
    let mut mm = [[0.0f64; 3]; 3];
    let mut off = [0.0f64; 3];
    for i in 0..3 {
        let so = if i == 0 { 219.0 } else { 224.0 };
        mm[i][0] = m[i][0] * so / sy;
        mm[i][1] = m[i][1] * so / sc;
        mm[i][2] = m[i][2] * so / sc;
        off[i] = (if i == 0 { 16.0 } else { 128.0 }) - mm[i][0] * oy;
    }
    let mut p = Csc {
        m: [0; 9],
        off: [0; 3],
    };
    for i in 0..3 {
        for j in 0..3 {
            p.m[3 * i + j] = lround(mm[i][j] * 4096.0);
        }
        p.off[i] = lround(off[i] * 4096.0);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_match_the_golden_records() {
        let mut n = 0;
        for r in tb_golden::case("taps").unwrap().iter() {
            if r.kind() != "csc" {
                continue;
            }
            let p = make_csc(
                r.flag("from709").unwrap(),
                r.flag("to709").unwrap(),
                r.flag("from_full").unwrap(),
            );
            let m: Vec<i64> = p.m.iter().map(|&v| i64::from(v)).collect();
            let off: Vec<i64> = p.off.iter().map(|&v| i64::from(v)).collect();
            assert_eq!(m, r.ints("m").unwrap());
            assert_eq!(off, r.ints("off").unwrap());
            assert_eq!(
                r.int("interlaced").unwrap(),
                r.int("interlaced_out").unwrap()
            );
            n += 1;
        }
        assert_eq!(n, 16);
    }
}
