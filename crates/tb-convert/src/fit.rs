//! Where a source goes in a destination area (the C++ `Fit`, `make_fit`).

/// How a source of another aspect ratio fills its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FitMode {
    /// All of the source visible, bars around it (`fit=box`, 0).
    Box,
    /// The area filled, the source cropped (`fit=crop`, 1).
    Crop,
    /// The source stretched to the area (`fit=stretch`, 2).
    Stretch,
}

/// Source crop and destination rectangle of one source in one area (luma samples).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fit {
    /// Crop origin, x.
    pub cx: i32,
    /// Crop origin, y.
    pub cy: i32,
    /// Crop width.
    pub cw: i32,
    /// Crop height.
    pub ch: i32,
    /// Rectangle origin, x.
    pub rx: i32,
    /// Rectangle origin, y.
    pub ry: i32,
    /// Rectangle width.
    pub rw: i32,
    /// Rectangle height.
    pub rh: i32,
}

/// The crop and the rectangle for a source of `sw` x `sh` samples of aspect `sar` in the area
/// (`ax`, `ay`, `aw`, `ah`) of a destination whose samples have aspect `dpar` (`make_fit`). `il`:
/// the destination is interlaced (rectangle heights in multiples of 4).
#[allow(clippy::too_many_arguments, reason = "the C++ signature")]
pub fn make_fit(
    sw: i32,
    sh: i32,
    sar: f64,
    ax: i32,
    ay: i32,
    aw: i32,
    ah: i32,
    dpar: f64,
    mode: FitMode,
    il: bool,
) -> Fit {
    let lround = |x: f64| x.round() as i64 as i32;
    let mut f = Fit {
        cx: 0,
        cy: 0,
        cw: sw,
        ch: sh,
        rx: ax,
        ry: ay,
        rw: aw,
        rh: ah,
    };
    if mode == FitMode::Stretch {
        return f;
    }
    let as_ = f64::from(sw) * sar / f64::from(sh);
    let ad = f64::from(aw) * dpar / f64::from(ah);
    let vq = if il { 4 } else { 2 };
    if (as_ / ad - 1.0).abs() < 0.01 {
        return f;
    }
    if mode == FitMode::Box {
        if as_ > ad {
            f.rh = ah.min(lround(f64::from(ah) * ad / as_ / f64::from(vq)) * vq);
            f.ry = ay + ((ah - f.rh) / 2 / vq) * vq;
        } else {
            f.rw = aw.min(lround(f64::from(aw) * as_ / ad / 8.0) * 8);
            f.rx = ax + ((aw - f.rw) / 2 / 8) * 8;
        }
    } else if as_ > ad {
        f.cw = sw.min(lround(f64::from(sw) * ad / as_ / 2.0) * 2);
        f.cx = ((sw - f.cw) / 4) * 2;
    } else {
        f.ch = sh.min(lround(f64::from(sh) * as_ / ad / 4.0) * 4);
        f.cy = ((sh - f.ch) / 8) * 4;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_takes_everything() {
        let f = make_fit(
            1920,
            1080,
            1.0,
            0,
            0,
            704,
            480,
            10.0 / 11.0,
            FitMode::Stretch,
            true,
        );
        assert_eq!(
            f,
            Fit {
                cx: 0,
                cy: 0,
                cw: 1920,
                ch: 1080,
                rx: 0,
                ry: 0,
                rw: 704,
                rh: 480
            }
        );
    }

    #[test]
    fn hd_into_sd_letterbox_and_crop() {
        // 16:9 into 4:3 SD (pipe-1080p-480i-letterbox): as = 16/9, ad = 704 * 10/11 / 480 = 4/3;
        // rh = lround(480 * 0.75 / 4) * 4 = 360, ry = (120 / 2 / 4) * 4 = 60.
        let f = make_fit(
            1920,
            1080,
            1.0,
            0,
            0,
            704,
            480,
            10.0 / 11.0,
            FitMode::Box,
            true,
        );
        assert_eq!((f.rx, f.ry, f.rw, f.rh), (0, 60, 704, 360));
        assert_eq!((f.cx, f.cy, f.cw, f.ch), (0, 0, 1920, 1080));
        // crop (pipe-1080p-480i-crop): cw = lround(1920 * 0.75 / 2) * 2 = 1440, cx = 240.
        let f = make_fit(
            1920,
            1080,
            1.0,
            0,
            0,
            704,
            480,
            10.0 / 11.0,
            FitMode::Crop,
            true,
        );
        assert_eq!((f.cx, f.cy, f.cw, f.ch), (240, 0, 1440, 1080));
        assert_eq!((f.rx, f.ry, f.rw, f.rh), (0, 0, 704, 480));
    }

    #[test]
    fn narrow_into_wide_pillarbox_and_crop() {
        // 4:3 SD (sar 10/11) into 1080i: as = 704 * 10/11 / 480 = 4/3, ad = 16/9;
        // rw = lround(1920 * 0.75 / 8) * 8 = 1440, rx = (480 / 2 / 8) * 8 = 240.
        let f = make_fit(
            704,
            480,
            10.0 / 11.0,
            0,
            0,
            1920,
            1080,
            1.0,
            FitMode::Box,
            true,
        );
        assert_eq!((f.rx, f.ry, f.rw, f.rh), (240, 0, 1440, 1080));
        // ch = lround(480 * 0.75 / 4) * 4 = 360, cy = (120 / 8) * 4 = 60.
        let f = make_fit(
            704,
            480,
            10.0 / 11.0,
            0,
            0,
            1920,
            1080,
            1.0,
            FitMode::Crop,
            true,
        );
        assert_eq!((f.cx, f.cy, f.cw, f.ch), (0, 60, 704, 360));
        // same aspect within 1 %: untouched
        let f = make_fit(1280, 720, 1.0, 960, 540, 960, 540, 1.0, FitMode::Box, false);
        assert_eq!(
            (f.rx, f.ry, f.rw, f.rh, f.cw, f.ch),
            (960, 540, 960, 540, 1280, 720)
        );
    }
}
