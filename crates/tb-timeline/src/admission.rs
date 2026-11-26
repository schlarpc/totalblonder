//! Admission control for the card's hardware decoder (NVDEC).
//!
//! notes/cuda-mpeg2-spike.md §C.9: "The decoder is the limit and nothing manages it": a line-up
//! that needs more than the decoder has is accepted and every input degrades. This module is
//! the manager's arithmetic: a [`DecoderBudget`] that knows what each admitted input takes from
//! the decoder and answers, for a new input, [`Admission::Hardware`], [`Admission::Software`]
//! or [`Admission::Refuse`]. The C++ harness has nothing of this.
//!
//! **Cost model** (§C.2, measured on the reference card). The engine has one throughput per
//! kind of stream whatever the number of sessions, so an input costs the fraction
//! `input frames per second / engine frames per second for its kind`. For the kinds in the
//! table of §C.2 ([`MEASURED`]) the engine rate is the best measured total. Any other kind
//! takes the nearest measured kind of the same codec (same bit depth first, then nearest
//! picture size, then scan, then frame rate) and scales its rate by the number of pixels per
//! picture, i.e. it assumes the same pixel rate; that is the notes' "about 1.9 gigapixels per
//! second for H.264 at 1080p, 1.7 at 720p, 1.1 at SD; HEVC 2.2 times as fast"
//! ([`H264_GPX_1080`] and the constants after it, which the table reproduces).
//!
//! **Budget** (§C.4, §C.5). In the pipeline the engine gives about 9 % less than alone, so
//! plan with 90 % of the table ([`SHARE_ALONE`], the default); next to other users of the card
//! with less ([`SHARE_TYPICAL`], [`SHARE_WORST`]).
//!
//! **Software decode** (§C.2 "which inputs to hand to the CPU first", §C.4 "Hybrid"). When the
//! decoder is full, an input that is cheap for the CPU is decoded there instead: MPEG-2 of any
//! size and H.264 up to SD ([`software_ok`]). Its cost is the table's cores per real-time
//! stream, scaled by pixel rate for other kinds, against a budget in cores. Everything else is
//! refused: H.264 at 1080 lines costs 0.19 core for 3.2 % of the decoder, HEVC and 2160p more
//! than they free.
//!
//! Costs are kept as integers (millionths of the decoder, millionths of a core, rounded up),
//! so that releasing every grant returns the budget to exactly zero.
//!
//! Not here: moving an admitted SD input from the card to the CPU to make room for an input
//! that must be on the card, and measuring the engine instead of trusting the table (another
//! card has another table).

/// Codecs the decoder was measured with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    Hevc,
    /// MPEG-2 video.
    Mpeg2,
    /// AV1.
    Av1,
    /// VP9.
    Vp9,
}

/// A kind of input: what its cost depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputKind {
    /// The codec.
    pub codec: Codec,
    /// Picture width.
    pub w: u32,
    /// Picture height (of the frame, also for interlaced streams).
    pub h: u32,
    /// Frames per second (29.97 for 1080i59.94).
    pub fps: f64,
    /// Bits per sample (8 or 10).
    pub bits: u8,
    /// Interlaced coding.
    pub interlaced: bool,
}

impl InputKind {
    /// A progressive 8-bit kind.
    pub const fn new(codec: Codec, w: u32, h: u32, fps: f64) -> Self {
        Self {
            codec,
            w,
            h,
            fps,
            bits: 8,
            interlaced: false,
        }
    }

    /// The same, interlaced.
    pub const fn interlaced(mut self) -> Self {
        self.interlaced = true;
        self
    }

    /// The same with 10 bits per sample.
    pub const fn ten_bit(mut self) -> Self {
        self.bits = 10;
        self
    }

    fn pixels(&self) -> f64 {
        f64::from(self.w) * f64::from(self.h)
    }
}

/// A row of the table in notes §C.2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    /// The kind of input.
    pub kind: InputKind,
    /// Frames per second the engine decodes of this kind in total (the best of the totals at
    /// 1 to 16 streams).
    pub engine_fps: f64,
    /// Software decode: cores per real-time stream (one thread, the reference host).
    pub sw_cores: f64,
}

const fn row(kind: InputKind, engine_fps: f64, sw_cores: f64) -> Measured {
    Measured {
        kind,
        engine_fps,
        sw_cores,
    }
}

const F24: f64 = 24000.0 / 1001.0;
const F30: f64 = 30000.0 / 1001.0;
const F60: f64 = 60000.0 / 1001.0;

/// The table of notes/cuda-mpeg2-spike.md §C.2 (columns "frames per second in total" and
/// "software decode: cores per real-time stream"; MPEG-2 480i's 0.011 core is from the text
/// below the table).
pub const MEASURED: [Measured; 18] = [
    row(InputKind::new(Codec::Av1, 1920, 1080, F30), 1816.0, 0.18),
    row(
        InputKind::new(Codec::H264, 1920, 1080, F30).interlaced(),
        840.0,
        0.23,
    ),
    row(InputKind::new(Codec::H264, 1920, 1080, F24), 907.0, 0.18),
    row(InputKind::new(Codec::H264, 1920, 1080, 25.0), 912.0, 0.17),
    row(InputKind::new(Codec::H264, 1920, 1080, F30), 925.0, 0.19),
    row(InputKind::new(Codec::H264, 1920, 1080, F60), 937.0, 0.32),
    row(InputKind::new(Codec::H264, 1366, 768, F30), 1674.0, 0.08),
    row(
        InputKind::new(Codec::H264, 704, 480, F30).interlaced(),
        3302.0,
        0.05,
    ),
    row(InputKind::new(Codec::H264, 1280, 720, 50.0), 1816.0, 0.13),
    row(InputKind::new(Codec::H264, 1280, 720, F60), 1839.0, 0.15),
    row(InputKind::new(Codec::H264, 854, 480, F30), 3179.0, 0.03),
    row(InputKind::new(Codec::Hevc, 1920, 1080, F30), 2077.0, 0.24),
    row(
        InputKind::new(Codec::Hevc, 1920, 1080, F30).ten_bit(),
        1879.0,
        0.30,
    ),
    row(
        InputKind::new(Codec::Hevc, 3840, 2160, F24).ten_bit(),
        471.0,
        1.37,
    ),
    row(InputKind::new(Codec::Hevc, 3840, 2160, F30), 538.0, 1.05),
    row(
        InputKind::new(Codec::Mpeg2, 1920, 1080, F30).interlaced(),
        870.0,
        0.07,
    ),
    row(
        InputKind::new(Codec::Mpeg2, 704, 480, F30).interlaced(),
        3235.0,
        0.011,
    ),
    row(InputKind::new(Codec::Vp9, 1920, 1080, F30), 1678.0, 0.16),
];

/// Engine throughput for H.264 at 1080p, gigapixels per second (§C.2, first note below the
/// table). The table rows carry it; the constants are here to name the notes' figures and to
/// check the rows against them.
pub const H264_GPX_1080: f64 = 1.9;
/// The same at 720p.
pub const H264_GPX_720: f64 = 1.7;
/// The same at SD (more bits per pixel there).
pub const H264_GPX_SD: f64 = 1.1;
/// HEVC against H.264 at 1080p (§C.2).
pub const HEVC_OVER_H264: f64 = 2.2;
/// Engine rate of 10-bit against 8-bit streams, used when a codec has no measured row of the
/// input's bit depth: HEVC 1080p30, 1879 against 2077 frames per second (§C.2).
pub const TEN_BIT_ENGINE: f64 = 1879.0 / 2077.0;
/// Software decode cost of 10-bit against 8-bit streams: HEVC 1080p30, 0.30 against 0.24 core
/// (§C.2).
pub const TEN_BIT_CPU: f64 = 0.30 / 0.24;

/// Share of the measured decoder capacity to plan with when the pipeline has the card to
/// itself (§C.4: "plan with 90 % of the table"; §C.5 planning table).
pub const SHARE_ALONE: f64 = 0.90;
/// Next to up to three real-time transcodes without tone mapping (§C.5).
pub const SHARE_TYPICAL: f64 = 0.75;
/// Next to two 2160p and a tone-mapped 2160p transcode in real time (§C.5).
pub const SHARE_WORST: f64 = 0.65;
/// Default CPU budget for software decode, cores (§C.4: the 8 vCPUs of the reference host "do
/// not carry it far"; the hybrid mixed line-up at 2.5 cores in total was borderline).
pub const DEFAULT_CPU_CORES: f64 = 1.0;

/// Largest picture of an H.264 input that may be decoded in software: SD (720 x 576).
pub const SOFTWARE_H264_MAX_PIXELS: u32 = 720 * 576;

/// The row of [`MEASURED`] for exactly this kind (frame rate within half a frame per second).
pub fn measured(kind: &InputKind) -> Option<&'static Measured> {
    MEASURED.iter().find(|m| {
        let k = &m.kind;
        k.codec == kind.codec
            && (k.w, k.h, k.bits, k.interlaced) == (kind.w, kind.h, kind.bits, kind.interlaced)
            && (k.fps - kind.fps).abs() < 0.5
    })
}

/// The measured row to estimate `kind` from: same codec; same bit depth first, then nearest
/// picture size (as a ratio), then same scan, then nearest frame rate.
fn nearest(kind: &InputKind) -> Option<&'static Measured> {
    let key = |m: &Measured| {
        let k = &m.kind;
        (
            k.bits != kind.bits,
            (k.pixels() / kind.pixels()).ln().abs(),
            k.interlaced != kind.interlaced,
            (k.fps - kind.fps).abs(),
        )
    };
    let mut best: Option<&'static Measured> = None;
    for m in MEASURED.iter().filter(|m| m.kind.codec == kind.codec) {
        if best.is_none_or(|b| key(m) < key(b)) {
            best = Some(m);
        }
    }
    best
}

/// The row to work from and the factors for engine rate and CPU cost when the bit depth
/// differs from the row's.
fn model(kind: &InputKind) -> Option<(&'static Measured, f64, f64)> {
    let sane = kind.w > 0 && kind.h > 0 && kind.fps.is_finite() && kind.fps > 0.0;
    if !sane {
        return None;
    }
    let m = measured(kind).or_else(|| nearest(kind))?;
    Some(match (m.kind.bits > 8, kind.bits > 8) {
        (false, true) => (m, TEN_BIT_ENGINE, TEN_BIT_CPU),
        (true, false) => (m, 1.0 / TEN_BIT_ENGINE, 1.0 / TEN_BIT_CPU),
        _ => (m, 1.0, 1.0),
    })
}

/// Frames per second the engine decodes of this kind (measured, or scaled by picture size from
/// the nearest measured kind). `None` for a kind without size or rate.
pub fn engine_fps(kind: &InputKind) -> Option<f64> {
    let (m, engine, _) = model(kind)?;
    Some(m.engine_fps * engine * (m.kind.pixels() / kind.pixels()))
}

/// Fraction of the decoder one real-time input of this kind takes (1.0 = all of it).
pub fn hardware_cost(kind: &InputKind) -> Option<f64> {
    engine_fps(kind).map(|e| kind.fps / e)
}

/// Cores one real-time input of this kind takes when decoded in software (measured, or scaled
/// by pixel rate from the nearest measured kind).
pub fn software_cost(kind: &InputKind) -> Option<f64> {
    let (m, _, cpu) = model(kind)?;
    let rate = (kind.pixels() * kind.fps) / (m.kind.pixels() * m.kind.fps);
    Some(m.sw_cores * cpu * rate)
}

/// Whether an input of this kind may be handed to the CPU when the decoder is full (§C.2):
/// MPEG-2, and H.264 up to SD.
pub fn software_ok(kind: &InputKind) -> bool {
    match kind.codec {
        Codec::Mpeg2 => true,
        Codec::H264 => u64::from(kind.w) * u64::from(kind.h) <= u64::from(SOFTWARE_H264_MAX_PIXELS),
        Codec::Hevc | Codec::Av1 | Codec::Vp9 => false,
    }
}

/// Where an admitted input is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    /// On the card's decoder.
    Hardware,
    /// In a software decode thread.
    Software,
}

/// What an admitted input holds of the budget; give it back with [`DecoderBudget::release`].
/// (Deliberately not `Clone`: one grant, one release.)
#[derive(Debug, PartialEq, Eq)]
#[must_use = "an admitted input holds budget until its grant is released"]
pub struct Grant {
    route: Route,
    cost: u64,
}

impl Grant {
    /// Where the input is decoded.
    pub fn route(&self) -> Route {
        self.route
    }

    /// Its cost: the fraction of the decoder for [`Route::Hardware`], cores for
    /// [`Route::Software`].
    pub fn cost(&self) -> f64 {
        self.cost as f64 / UNIT
    }
}

/// The answer to [`DecoderBudget::admit`].
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub enum Admission {
    /// Decode on the card.
    Hardware(Grant),
    /// The decoder is full and the input is cheap for the CPU: decode in software.
    Software(Grant),
    /// Neither budget has room (or the kind has no size or rate).
    Refuse,
}

/// Integer cost units per decoder and per core.
const UNIT: f64 = 1e6;

/// A cost in millionths, rounded up; `None` for a cost that is not a finite positive number.
fn units(cost: f64) -> Option<u64> {
    // `as` saturates; a cost anywhere near that is refused by every budget
    (cost.is_finite() && cost > 0.0).then(|| (cost * UNIT).ceil() as u64)
}

/// A budget in millionths: negative and non-finite values count as 0.
fn budget_units(v: f64) -> u64 {
    if v.is_finite() && v > 0.0 {
        (v * UNIT).floor() as u64
    } else {
        0
    }
}

/// The decoder's budget and the CPU's budget for software decode, and what is admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecoderBudget {
    share: u64,
    cores: u64,
    hw_used: u64,
    sw_used: u64,
    hw_inputs: usize,
    sw_inputs: usize,
}

impl Default for DecoderBudget {
    /// [`SHARE_ALONE`] of the decoder and [`DEFAULT_CPU_CORES`].
    fn default() -> Self {
        Self::new(SHARE_ALONE, DEFAULT_CPU_CORES)
    }
}

impl DecoderBudget {
    /// `share`: fraction of the measured decoder capacity to use (0 to 1). `cpu_cores`: cores
    /// that software decode may use (0: never decode in software).
    pub fn new(share: f64, cpu_cores: f64) -> Self {
        Self {
            share: budget_units(share),
            cores: budget_units(cpu_cores),
            hw_used: 0,
            sw_used: 0,
            hw_inputs: 0,
            sw_inputs: 0,
        }
    }

    /// Change the decoder share (another user of the card came or went, §C.5). Admitted inputs
    /// stay; while they exceed the new share ([`DecoderBudget::over`]) nothing more is admitted
    /// to the card.
    pub fn set_share(&mut self, share: f64) {
        self.share = budget_units(share);
    }

    /// Change the CPU budget; admitted inputs stay.
    pub fn set_cpu_cores(&mut self, cpu_cores: f64) {
        self.cores = budget_units(cpu_cores);
    }

    /// A new input: on the card while the share holds; else in software when the kind is cheap
    /// for the CPU ([`software_ok`]) and the CPU budget holds; else refused.
    pub fn admit(&mut self, kind: &InputKind) -> Admission {
        if let Some(cost) = hardware_cost(kind).and_then(units)
            && self.hw_used.saturating_add(cost) <= self.share
        {
            self.hw_used += cost;
            self.hw_inputs += 1;
            return Admission::Hardware(Grant {
                route: Route::Hardware,
                cost,
            });
        }
        if software_ok(kind)
            && let Some(cost) = software_cost(kind).and_then(units)
            && self.sw_used.saturating_add(cost) <= self.cores
        {
            self.sw_used += cost;
            self.sw_inputs += 1;
            return Admission::Software(Grant {
                route: Route::Software,
                cost,
            });
        }
        Admission::Refuse
    }

    /// An admitted input ended: its share is free again.
    pub fn release(&mut self, grant: Grant) {
        match grant.route {
            Route::Hardware => {
                self.hw_used = self.hw_used.saturating_sub(grant.cost);
                self.hw_inputs = self.hw_inputs.saturating_sub(1);
            }
            Route::Software => {
                self.sw_used = self.sw_used.saturating_sub(grant.cost);
                self.sw_inputs = self.sw_inputs.saturating_sub(1);
            }
        }
    }

    /// Fraction of the decoder the admitted inputs take.
    pub fn hardware_used(&self) -> f64 {
        self.hw_used as f64 / UNIT
    }

    /// Cores the inputs admitted to software decode take.
    pub fn software_used(&self) -> f64 {
        self.sw_used as f64 / UNIT
    }

    /// Inputs admitted to the card and to software decode.
    pub fn inputs(&self) -> (usize, usize) {
        (self.hw_inputs, self.sw_inputs)
    }

    /// The admitted inputs take more of the decoder than the share (after
    /// [`DecoderBudget::set_share`] lowered it): the caller should move or end inputs.
    pub fn over(&self) -> bool {
        self.hw_used > self.share
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_1080P30: InputKind = InputKind::new(Codec::H264, 1920, 1080, F30);
    const H264_1080I: InputKind = InputKind::new(Codec::H264, 1920, 1080, F30).interlaced();
    const H264_720P60: InputKind = InputKind::new(Codec::H264, 1280, 720, F60);
    const H264_480I: InputKind = InputKind::new(Codec::H264, 704, 480, F30).interlaced();
    const H264_854: InputKind = InputKind::new(Codec::H264, 854, 480, F30);
    const H264_768: InputKind = InputKind::new(Codec::H264, 1366, 768, F30);
    const HEVC_1080P30: InputKind = InputKind::new(Codec::Hevc, 1920, 1080, F30);
    const HEVC_2160P30: InputKind = InputKind::new(Codec::Hevc, 3840, 2160, F30);
    const MPEG2_1080I: InputKind = InputKind::new(Codec::Mpeg2, 1920, 1080, F30).interlaced();
    const MPEG2_480I: InputKind = InputKind::new(Codec::Mpeg2, 704, 480, F30).interlaced();

    fn pct(kind: &InputKind) -> f64 {
        100.0 * hardware_cost(kind).unwrap()
    }
    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }
    fn hw(a: Admission) -> Grant {
        match a {
            Admission::Hardware(g) => g,
            a => panic!("not on the card: {a:?}"),
        }
    }

    /// Notes §C.4: "What a channel takes from the decoder follows from §C.2".
    #[test]
    fn measured_kinds_cost_what_the_notes_say() {
        assert!(near(pct(&H264_1080P30), 3.24, 0.01));
        assert!(near(
            pct(&InputKind::new(Codec::H264, 1920, 1080, F60)),
            6.4,
            0.05
        ));
        assert!(near(pct(&H264_720P60), 3.3, 0.05));
        assert!(near(pct(&H264_480I), 0.9, 0.05));
        assert!(near(pct(&H264_854), 0.9, 0.05));
        assert!(near(pct(&HEVC_1080P30), 1.5, 0.06));
        assert!(near(pct(&HEVC_2160P30), 5.6, 0.05));
        assert!(near(
            pct(&InputKind::new(Codec::Hevc, 3840, 2160, F24).ten_bit()),
            5.1,
            0.05
        ));
        // §C.2: MPEG-2 1080i 3.4 % of the decoder for 0.07 core, 480i 0.9 % for 0.01 core
        assert!(near(pct(&MPEG2_1080I), 3.4, 0.05));
        assert!(near(pct(&MPEG2_480I), 0.9, 0.05));
        assert_eq!(software_cost(&MPEG2_1080I), Some(0.07));
        assert_eq!(software_cost(&MPEG2_480I), Some(0.011));
        assert_eq!(software_cost(&H264_1080P30), Some(0.19));
        // every row is found as itself, and a nominal rate finds its row (30 for 29.97)
        for m in &MEASURED {
            assert_eq!(measured(&m.kind), Some(m));
            assert_eq!(engine_fps(&m.kind), Some(m.engine_fps));
        }
        assert_eq!(
            engine_fps(&InputKind::new(Codec::H264, 1920, 1080, 30.0)),
            Some(925.0)
        );
        assert_eq!(
            engine_fps(&InputKind::new(Codec::H264, 1920, 1080, 24.0)),
            Some(907.0)
        );
    }

    #[test]
    fn the_table_reproduces_the_named_pixel_rates() {
        let gpx = |k: &InputKind| engine_fps(k).unwrap() * k.pixels() / 1e9;
        assert!(near(gpx(&H264_1080P30), H264_GPX_1080, 0.05));
        assert!(near(gpx(&H264_720P60), H264_GPX_720, 0.05));
        assert!(near(gpx(&H264_480I), H264_GPX_SD, 0.05));
        assert!(near(gpx(&H264_854), 1.3, 0.05));
        let ratio = engine_fps(&HEVC_1080P30).unwrap() / engine_fps(&H264_1080P30).unwrap();
        assert!(near(ratio, HEVC_OVER_H264, 0.05));
        // "a 2160p HEVC stream costs as much as four 1080p HEVC ones or two 1080p H.264 ones"
        assert!(near(pct(&HEVC_2160P30) / pct(&HEVC_1080P30), 4.0, 0.2));
        assert!(near(pct(&HEVC_2160P30) / pct(&H264_1080P30), 2.0, 0.3));
    }

    #[test]
    fn other_kinds_scale_by_pixels_from_the_nearest_measured_kind() {
        // 1080p50: no row; the 1080p row of the nearest rate (59.94) as it is
        let k = InputKind::new(Codec::H264, 1920, 1080, 50.0);
        assert_eq!(measured(&k), None);
        assert_eq!(engine_fps(&k), Some(937.0));
        assert!(near(software_cost(&k).unwrap(), 0.32 * 50.0 / F60, 1e-12));
        // 720p30: the 720p50 row
        let k = InputKind::new(Codec::H264, 1280, 720, F30);
        assert_eq!(engine_fps(&k), Some(1816.0));
        assert!(near(software_cost(&k).unwrap(), 0.13 * F30 / 50.0, 1e-12));
        // 1440x1080 (HDV): the 1080p30 row at the same pixel rate
        let k = InputKind::new(Codec::H264, 1440, 1080, F30);
        assert!(near(engine_fps(&k).unwrap(), 925.0 * 1920.0 / 1440.0, 1e-9));
        assert!(near(software_cost(&k).unwrap(), 0.19 * 0.75, 1e-12));
        // PAL SD interlaced: the nearest size is 854x480 (size ranks before scan)
        let k = InputKind::new(Codec::H264, 720, 576, 25.0).interlaced();
        assert!(near(
            engine_fps(&k).unwrap(),
            3179.0 * (854.0 * 480.0) / (720.0 * 576.0),
            1e-9
        ));
        assert!(near(pct(&k), 0.80, 0.01));
        // 640x480 interlaced: 704x480 is nearest
        let k = InputKind::new(Codec::H264, 640, 480, F30).interlaced();
        assert!(near(engine_fps(&k).unwrap(), 3302.0 * 704.0 / 640.0, 1e-9));
        // HEVC 720p60: from HEVC 1080p30 (8 bit), not from the 10-bit or 2160p rows
        let k = InputKind::new(Codec::Hevc, 1280, 720, F60);
        assert!(near(engine_fps(&k).unwrap(), 2077.0 * 2.25, 1e-9));
        // H.264 2160p30: four times a 1080p30 input
        let k = InputKind::new(Codec::H264, 3840, 2160, F30);
        assert!(near(pct(&k), 4.0 * pct(&H264_1080P30), 1e-9));
        // 10-bit H.264 has no row: the 8-bit row and the HEVC ratio
        let k = H264_1080P30.ten_bit();
        assert!(near(engine_fps(&k).unwrap(), 925.0 * TEN_BIT_ENGINE, 1e-9));
        assert!(near(software_cost(&k).unwrap(), 0.19 * TEN_BIT_CPU, 1e-12));
        // MPEG-2 720p60: nearest size is 1080i
        let k = InputKind::new(Codec::Mpeg2, 1280, 720, F60);
        assert!(near(engine_fps(&k).unwrap(), 870.0 * 2.25, 1e-9));
        // no size or rate: no cost, and no admission
        for k in [
            InputKind::new(Codec::H264, 0, 1080, F30),
            InputKind::new(Codec::H264, 1920, 1080, 0.0),
            InputKind::new(Codec::H264, 1920, 1080, f64::NAN),
        ] {
            assert_eq!(hardware_cost(&k), None);
            assert_eq!(software_cost(&k), None);
            assert_eq!(DecoderBudget::default().admit(&k), Admission::Refuse);
        }
    }

    /// Line-up (a) of notes §C.4: 27 x H.264 1080p30 hold, 28 do not.
    #[test]
    fn lineup_a_admits_27_at_90_per_cent() {
        let mut b = DecoderBudget::default();
        let grants: Vec<Grant> = (0..27).map(|_| hw(b.admit(&H264_1080P30))).collect();
        assert_eq!(b.inputs(), (27, 0));
        assert!(near(b.hardware_used(), 0.875, 0.001));
        assert!(near(grants[0].cost(), 0.0324, 0.0001));
        assert_eq!(grants[0].route(), Route::Hardware);
        // the 28th: the decoder is full and 1080p H.264 is not for the CPU
        assert_eq!(b.admit(&H264_1080P30), Admission::Refuse);
        assert_eq!(b.inputs(), (27, 0));
        // the planning table of §C.5: 22 at 75 %, 19 to 20 at 65 %
        for (share, n) in [(SHARE_TYPICAL, 23), (SHARE_WORST, 20)] {
            let mut b = DecoderBudget::new(share, 0.0);
            let got = (0..40)
                .filter(|_| b.admit(&H264_1080P30) != Admission::Refuse)
                .count();
            assert_eq!(got, n, "share {share}");
        }
        // line-up (b): 27 x 720p60
        let mut b = DecoderBudget::default();
        let got = (0..40)
            .filter(|_| b.admit(&H264_720P60) != Admission::Refuse)
            .count();
        assert_eq!(got, 27);
    }

    /// The inputs of unit `u` of line-up (c) (`gate/pipe_lineup.py`): one of four kinds of
    /// 1080 source, a 720p60 and four small ones; nine kinds in all.
    fn unit(u: usize) -> [InputKind; 6] {
        let hd = [H264_1080P30, H264_1080I, HEVC_1080P30, MPEG2_1080I][u % 4];
        [hd, H264_720P60, H264_480I, MPEG2_480I, H264_854, H264_768]
    }

    /// Line-up (c) of notes §C.4: 8 mixed units (48 inputs) hold at 87 % of the decoder, 9 do
    /// not.
    #[test]
    fn lineup_c_admits_eight_mixed_units() {
        let mut b = DecoderBudget::new(SHARE_ALONE, 0.0);
        for u in 0..8 {
            for k in unit(u) {
                assert!(
                    matches!(b.admit(&k), Admission::Hardware(_)),
                    "unit {u}: {k:?}"
                );
            }
        }
        assert_eq!(b.inputs(), (48, 0));
        assert!(
            near(b.hardware_used(), 0.87, 0.015),
            "{}",
            b.hardware_used()
        );
        // the ninth unit does not fit
        let refused = unit(8)
            .iter()
            .filter(|k| b.admit(k) == Admission::Refuse)
            .count();
        assert!(refused >= 2, "{refused}");
        assert!(!b.over());

        // with a core for software decode, what the ninth unit lacks on the card goes to the
        // CPU where the kind allows it (§C.4 "Hybrid")
        let mut b = DecoderBudget::default();
        for u in 0..8 {
            for k in unit(u) {
                let _ = hw(b.admit(&k));
            }
        }
        let mut refused = Vec::new();
        for k in unit(8) {
            match b.admit(&k) {
                Admission::Hardware(_) => {}
                Admission::Software(g) => {
                    assert!(software_ok(&k));
                    assert_eq!(g.route(), Route::Software);
                }
                Admission::Refuse => refused.push(k),
            }
        }
        assert!(b.inputs().1 >= 1);
        assert!(refused.iter().all(|k| !software_ok(k)), "{refused:?}");
        assert!(b.software_used() <= 0.1);
    }

    #[test]
    fn sd_inputs_go_to_software_when_the_decoder_is_full() {
        let mut b = DecoderBudget::new(SHARE_ALONE, 0.1);
        let _held: Vec<Grant> = (0..27).map(|_| hw(b.admit(&H264_1080P30))).collect();
        // 2.5 % of the decoder are left: two SD inputs, then the CPU
        assert!(matches!(b.admit(&H264_480I), Admission::Hardware(_)));
        assert!(matches!(b.admit(&MPEG2_480I), Admission::Hardware(_)));
        let mut soft = Vec::new();
        for k in [H264_480I, MPEG2_480I, H264_854] {
            match b.admit(&k) {
                Admission::Software(g) => soft.push(g),
                a => panic!("{k:?}: {a:?}"),
            }
        }
        // 0.05 + 0.011 + 0.03 core
        assert!(near(b.software_used(), 0.091, 1e-6));
        assert!(near(soft[1].cost(), 0.011, 1e-6));
        assert_eq!(b.inputs(), (29, 3));
        // the CPU budget is used up too
        assert_eq!(b.admit(&H264_854), Admission::Refuse);
        // MPEG-2 1080i is cheap for the CPU (0.07 core), but not within what is left
        assert_eq!(b.admit(&MPEG2_1080I), Admission::Refuse);
        b.set_cpu_cores(0.5);
        assert!(matches!(b.admit(&MPEG2_1080I), Admission::Software(_)));
        // kinds that must stay on the card are refused whatever the CPU has
        b.set_cpu_cores(8.0);
        for k in [
            H264_1080P30,
            H264_720P60,
            H264_768,
            HEVC_1080P30,
            HEVC_2160P30,
        ] {
            assert!(!software_ok(&k));
            assert_eq!(b.admit(&k), Admission::Refuse, "{k:?}");
        }
        assert!(software_ok(&InputKind::new(Codec::H264, 720, 576, 25.0)));
        assert!(software_ok(&InputKind::new(Codec::Mpeg2, 1920, 1080, 25.0)));
        assert!(!software_ok(&InputKind::new(Codec::Av1, 640, 360, 30.0)));
    }

    #[test]
    fn release_gives_the_share_back_exactly() {
        let mut b = DecoderBudget::new(SHARE_ALONE, 0.1);
        let mut grants: Vec<Grant> = (0..27).map(|_| hw(b.admit(&H264_1080P30))).collect();
        assert_eq!(b.admit(&H264_1080P30), Admission::Refuse);
        b.release(grants.pop().unwrap());
        assert_eq!(b.inputs(), (26, 0));
        // room for one 1080p30 again, or for a 2160p HEVC input instead of two of them
        grants.push(hw(b.admit(&H264_1080P30)));
        assert_eq!(b.admit(&HEVC_2160P30), Admission::Refuse);
        b.release(grants.pop().unwrap());
        b.release(grants.pop().unwrap());
        grants.push(hw(b.admit(&HEVC_2160P30)));
        // software grants go back to the CPU budget
        let soft = loop {
            match b.admit(&H264_480I) {
                Admission::Hardware(g) => grants.push(g),
                Admission::Software(s) => break s,
                Admission::Refuse => panic!("refused"),
            }
        };
        assert!(near(b.software_used(), 0.05, 1e-9));
        b.release(soft);
        assert_eq!(b.software_used(), 0.0);
        for g in grants {
            b.release(g);
        }
        assert_eq!(b, DecoderBudget::new(SHARE_ALONE, 0.1));
        assert_eq!((b.hardware_used(), b.inputs()), (0.0, (0, 0)));
    }

    #[test]
    fn a_lowered_share_stops_admission_until_inputs_leave() {
        let mut b = DecoderBudget::new(SHARE_ALONE, 0.0);
        let mut grants: Vec<Grant> = (0..27).map(|_| hw(b.admit(&H264_1080P30))).collect();
        // a transcode started on the card
        b.set_share(SHARE_TYPICAL);
        assert!(b.over());
        assert_eq!(b.admit(&MPEG2_480I), Admission::Refuse);
        for _ in 0..5 {
            b.release(grants.pop().unwrap());
        }
        assert!(!b.over());
        assert!(matches!(b.admit(&MPEG2_480I), Admission::Hardware(_)));
        // nonsense budgets admit nothing
        for share in [0.0, -1.0, f64::NAN] {
            assert_eq!(
                DecoderBudget::new(share, share).admit(&MPEG2_480I),
                Admission::Refuse
            );
        }
    }
}
