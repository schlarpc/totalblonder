//! What a sequence may say: the limits of H.262 Main Profile and of the broadcast standards,
//! checked once when a session opens.
//!
//! [`check`] takes the parameters a caller configures and returns the values the sequence
//! header carries ([`SeqParams`]), or the rule the configuration breaks ([`ProfileError`]).
//! Nothing that passes can be written as an illegal sequence header, sequence extension or GOP
//! time code.
//!
//! Sources (page numbers are the printed ones): ITU-T H.262 (02/2000) §6.3.3, §6.3.8, §8,
//! Tables 6-4, 8-5, 8-8, 8-11 to 8-14; ATSC A/53 Part 4:2009 §6.1, Tables 6.1 to 6.3;
//! ANSI/SCTE 43 2015 §7.1, Tables 2 to 4; SCTE 54 2024 §7.4; ITU-T H.222.0 (06/2012)
//! §2.4.2.6.

/// The set of rules a session's stream must satisfy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    /// H.262 Main Profile at Main, High-1440 or High level and nothing else. For tests: the
    /// small pictures of the sandbox gates are in no broadcast table.
    H262,
    /// ATSC terrestrial: A/53 Part 4 Table 6.2 formats, bit_rate at most 19.4 Mbit/s
    /// (§6.1.1, p.9), vbv_buffer_size_value at most 488.
    A53,
    /// Cable, 64-QAM: the same formats, bit_rate at most 27.0 Mbit/s (SCTE 43 §7.1.1, p.2).
    Scte43Qam64,
    /// Cable, 256-QAM: the same formats, bit_rate at most 38.8 Mbit/s (SCTE 43 §7.1.1, p.2).
    #[default]
    Scte43Qam256,
}

impl Profile {
    /// Every profile, in the order of the documentation.
    pub const ALL: [Self; 4] = [Self::H262, Self::A53, Self::Scte43Qam64, Self::Scte43Qam256];

    /// The profile's name on command lines and in records.
    pub fn name(self) -> &'static str {
        match self {
            Self::H262 => "h262",
            Self::A53 => "a53",
            Self::Scte43Qam64 => "scte43-64qam",
            Self::Scte43Qam256 => "scte43-256qam",
        }
    }

    /// The profile of a name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// A broadcast profile: the format tables, the rate ceiling and the transport limits apply.
    pub fn broadcast(self) -> bool {
        self != Self::H262
    }

    /// The profile's ceiling of bit_rate in bit/s (A/53 Part 4 Table 6.1: bit_rate_value 48500;
    /// SCTE 43 Table 2: 67500 and 97000), if it has one.
    pub fn max_bit_rate(self) -> Option<u64> {
        match self {
            Self::H262 => None,
            Self::A53 => Some(19_400_000),
            Self::Scte43Qam64 => Some(27_000_000),
            Self::Scte43Qam256 => Some(38_800_000),
        }
    }

    /// The profile's ceiling of the buffer in bits (vbv_buffer_size_value 488 in A/53 Part 4
    /// Table 6.1 and SCTE 43 Table 2), if it has one.
    pub fn max_vbv_bits(self) -> Option<u64> {
        self.broadcast().then_some(488 * VBV_UNIT)
    }
}

/// vbv_buffer_size counts units of 16384 bits (H.262 §6.3.3, p.40).
pub const VBV_UNIT: u64 = 16384;

/// A level of Main Profile (H.262 §8.3). Low level is not used: see [`Level::lowest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Main level.
    Main,
    /// High-1440 level.
    High1440,
    /// High level.
    High,
}

impl Level {
    /// The three levels, lowest first.
    pub const ALL: [Self; 3] = [Self::Main, Self::High1440, Self::High];

    /// profile_and_level_indication of Main Profile at this level (§8, Tables 8-2 and 8-3:
    /// profile 100, levels 1000, 0110, 0100).
    pub fn indication(self) -> u8 {
        match self {
            Self::Main => 0x48,
            Self::High1440 => 0x46,
            Self::High => 0x44,
        }
    }

    /// Its name in messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Main => "Main",
            Self::High1440 => "High-1440",
            Self::High => "High",
        }
    }

    /// Samples per line, lines per frame (Table 8-11, p.114).
    pub fn max_size(self) -> (u32, u32) {
        match self {
            Self::Main => (720, 576),
            Self::High1440 => (1440, 1088),
            Self::High => (1920, 1088),
        }
    }

    /// Highest frame_rate_code (Table 8-8, p.112).
    pub fn max_frame_rate_code(self) -> u8 {
        if self == Self::Main { 5 } else { 8 }
    }

    /// Luminance samples per second (Table 8-12, p.115).
    pub fn max_sample_rate(self) -> u64 {
        match self {
            Self::Main => 10_368_000,
            Self::High1440 => 47_001_600,
            Self::High => 62_668_800,
        }
    }

    /// bit_rate in bit/s (Table 8-13, p.116).
    pub fn max_bit_rate(self) -> u64 {
        match self {
            Self::Main => 15_000_000,
            Self::High1440 => 60_000_000,
            Self::High => 80_000_000,
        }
    }

    /// VBV buffer in bits (Table 8-14, p.116).
    pub fn max_vbv_bits(self) -> u64 {
        match self {
            Self::Main => 1_835_008,
            Self::High1440 => 7_340_032,
            Self::High => 9_781_248,
        }
    }

    /// The lowest level whose limits hold a sequence of this size and rate (Tables 8-8, 8-11,
    /// 8-12; at Main level more than 480 lines go with 25 Hz only, §8.2.1, p.111), or `None`
    /// when not even High level does.
    ///
    /// Low level is left out on purpose. No format of the broadcast tables fits it (the
    /// smallest, 640x480 at 23.976 Hz, has 480 lines against 288), so "the lowest level
    /// consistent with the stream" (A/53 Part 4 Table 6.3 note, SCTE 43 §7.1.3.1) is never Low
    /// there; elsewhere a level is an upper bound and Main is a legal label.
    pub fn lowest(width: u32, height: u32, interlaced: bool, frame_rate_code: u8) -> Option<Self> {
        let rate = sample_rate(width, height, interlaced, frame_rate_code)?;
        Self::ALL.into_iter().find(|l| {
            let (w, h) = l.max_size();
            width <= w
                && height <= h
                && frame_rate_code <= l.max_frame_rate_code()
                && rate <= l.max_sample_rate() as f64
                && !(*l == Self::Main && height > 480 && frame_rate_code != 3)
        })
    }
}

/// Highest f_code of a vertical component in a frame picture, at every level from Main up
/// (Table 8-8, p.112): the vertical vector range is [-128, 127.5] samples.
pub const MAX_F_CODE_VERTICAL: u8 = 5;

/// frame_rate_value of a frame_rate_code as a fraction (Table 6-4, p.39).
pub fn frame_rate_value(code: u8) -> Option<(u32, u32)> {
    Some(match code {
        1 => (24000, 1001),
        2 => (24, 1),
        3 => (25, 1),
        4 => (30000, 1001),
        5 => (30, 1),
        6 => (50, 1),
        7 => (60000, 1001),
        8 => (60, 1),
        _ => return None,
    })
}

/// frame_rate_code of a frame rate given as a fraction: the fraction is reduced and must equal
/// an entry of Table 6-4 exactly (the frame rate extension is always zero here, as A/53 Part 4
/// Table 6.3 and SCTE 43 Table 4 require).
pub fn frame_rate_code(fps_n: u32, fps_d: u32) -> Option<u8> {
    if fps_n == 0 || fps_d == 0 {
        return None;
    }
    let g = gcd(fps_n, fps_d);
    let f = (fps_n / g, fps_d / g);
    (1..=8).find(|c| frame_rate_value(*c) == Some(f))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Whether a sequence at this frame_rate_code may contain pictures with repeat_first_field = 1
/// (§8.2.1, Table 8-7, p.110-111, Simple and Main profile): in an interlaced sequence at 25 Hz
/// and above (codes 3 to 8), in a progressive sequence at 59.94 and 60 Hz only (codes 7, 8);
/// never at 23.976 or 24 Hz.
pub fn repeat_first_field_allowed(frame_rate_code: u8, progressive_sequence: bool) -> bool {
    if progressive_sequence {
        matches!(frame_rate_code, 7 | 8)
    } else {
        matches!(frame_rate_code, 3..=8)
    }
}

/// Whether B-pictures may carry repeat_first_field = 1 at this level and size: "Additional
/// constraints exist for Main profile @ Main level [...]: if (vertical_size > 480 lines) or
/// (frame_rate is 25Hz) then if picture_coding_type == 011 (i.e. B-picture),
/// repeat_first_field shall be 0" (§8.2.1, p.111). 704x480 at 29.97 Hz at Main level is not
/// touched by it, and High-1440 and High level have no such constraint.
pub fn repeat_first_field_in_b_allowed(level: Level, height: u32, frame_rate_code: u8) -> bool {
    !(level == Level::Main && (height > 480 || frame_rate_code == 3))
}

/// Pictures counted per second by the GOP time code at this frame rate: the rate "rounded to
/// the nearest integral number of pictures per second" (§6.3.8, p.46).
pub fn time_code_base(frame_rate_code: u8) -> u64 {
    match frame_rate_code {
        1 | 2 => 24,
        3 => 25,
        6 => 50,
        7 | 8 => 60,
        _ => 30,
    }
}

/// The luminance sample rate P of §8.5 (p.115): the coded size (width up to 16, height up to 16
/// or, interlaced, 32) times the frame rate.
pub fn sample_rate(width: u32, height: u32, interlaced: bool, frame_rate_code: u8) -> Option<f64> {
    let (n, d) = frame_rate_value(frame_rate_code)?;
    let w = 16 * u64::from(width.div_ceil(16));
    let h = if interlaced {
        32 * u64::from(height.div_ceil(32))
    } else {
        16 * u64::from(height.div_ceil(16))
    };
    Some((w * h) as f64 * f64::from(n) / f64::from(d))
}

/// colour_primaries, transfer_characteristics and matrix_coefficients of the sequence display
/// extension (§6.3.6, Tables 6-7 to 6-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    /// colour_primaries.
    pub primaries: u8,
    /// transfer_characteristics.
    pub transfer: u8,
    /// matrix_coefficients.
    pub matrix: u8,
}

impl Color {
    /// ITU-R BT.709 (1, 1, 1).
    pub const BT709: Self = Self {
        primaries: 1,
        transfer: 1,
        matrix: 1,
    };
    /// SMPTE 170M (6, 6, 6).
    pub const SMPTE170M: Self = Self {
        primaries: 6,
        transfer: 6,
        matrix: 6,
    };

    /// What a picture of this height is taken to be when nobody says: BT.709 above 576 lines,
    /// SMPTE 170M otherwise.
    pub fn for_height(height: u32) -> Self {
        if height > 576 {
            Self::BT709
        } else {
            Self::SMPTE170M
        }
    }

    /// No field is 0 (forbidden) and each is a defined entry of its table: primaries 1, 4 to
    /// 7; transfer 1, 4 to 8; matrix 1, 4 to 7 (2 is "unspecified", 3 reserved; a description
    /// that says "unspecified" is not explicit, which A/53 Part 4 §6.1.4 asks for).
    pub fn defined(self) -> bool {
        matches!(self.primaries, 1 | 4..=7)
            && matches!(self.transfer, 1 | 4..=8)
            && matches!(self.matrix, 1 | 4..=7)
    }
}

/// What a caller configures of a sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqConfig {
    /// horizontal_size.
    pub width: u32,
    /// vertical_size (1080 for the 1088 coded lines).
    pub height: u32,
    /// Frame rate numerator.
    pub fps_n: u32,
    /// Frame rate denominator.
    pub fps_d: u32,
    /// progressive_sequence = 0.
    pub interlaced: bool,
    /// aspect_ratio_information, or 0: the format's default.
    pub aspect: u8,
    /// bit_rate in bit/s (written rounded up to 400 bit/s).
    pub max_bitrate: u64,
    /// vbv_buffer_size in bits.
    pub vbv_bits: u64,
    /// The color description, or `None`: by the height.
    pub color: Option<Color>,
}

/// What the sequence header, sequence extension and sequence display extension carry, as
/// [`check`] derived it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqParams {
    /// frame_rate_code (Table 6-4).
    pub frame_rate_code: u8,
    /// aspect_ratio_information (Table 6-3): 1, 2 or 3.
    pub aspect: u8,
    /// The level of profile_and_level_indication.
    pub level: Level,
    /// bit_rate in units of 400 bit/s (18 bits; the extension is zero).
    pub bit_rate_value: u32,
    /// vbv_buffer_size in units of 16384 bits (10 bits; the extension is zero).
    pub vbv_buffer_size: u16,
    /// The color description.
    pub color: Color,
}

/// Why a sequence configuration is refused. Each names the rule it would break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// Zero, odd, or a multiple of 4096.
    #[error(
        "picture size {width}x{height}: both must be even (4:2:0) and not zero, and \
         horizontal_size_value and vertical_size_value shall not be zero (H.262 §6.3.3, p.38)"
    )]
    Size {
        #[allow(missing_docs, reason = "the size")]
        width: u32,
        #[allow(missing_docs, reason = "the size")]
        height: u32,
    },
    /// Not an entry of Table 6-4.
    #[error(
        "frame rate {fps_n}/{fps_d} is not a frame_rate_value of H.262 Table 6-4 (p.39): \
         24000/1001, 24, 25, 30000/1001, 30, 50, 60000/1001, 60"
    )]
    FrameRate {
        #[allow(missing_docs, reason = "the rate")]
        fps_n: u32,
        #[allow(missing_docs, reason = "the rate")]
        fps_d: u32,
    },
    /// Beyond Main Profile at High level.
    #[error(
        "{width}x{height} at frame_rate_code {frame_rate_code} exceeds Main Profile at High \
         level: 1920 samples per line, 1088 lines, 62 668 800 luminance samples per second \
         (H.262 Tables 8-11 and 8-12, p.114-115)"
    )]
    AboveHighLevel {
        #[allow(missing_docs, reason = "the size")]
        width: u32,
        #[allow(missing_docs, reason = "the size")]
        height: u32,
        #[allow(missing_docs, reason = "the rate")]
        frame_rate_code: u8,
    },
    /// aspect_ratio_information outside 1..=3.
    #[error(
        "aspect_ratio_information {0}: Main Profile allows 1, 2 and 3 (H.262 Table 8-5, p.110)"
    )]
    Aspect(u8),
    /// Not a row of the format table.
    #[error(
        "{width}x{height} {scan} is not a compression format of A/53 Part 4 Table 6.2 (p.9): \
         1920x1080, 1280x720 progressive, 704x480, 640x480"
    )]
    Format {
        #[allow(missing_docs, reason = "the size")]
        width: u32,
        #[allow(missing_docs, reason = "the size")]
        height: u32,
        /// "interlaced" or "progressive".
        scan: &'static str,
    },
    /// A row of SCTE 43 Table 3 only.
    #[error(
        "{width}x{height} is a format of SCTE 43 Table 3 (p.3) but not of A/53 Part 4 Table 6.2 \
         (p.9), with which SCTE 54-2024 §7.4 (p.14) requires MPEG-2 video on cable to comply"
    )]
    CableOnlyFormat {
        #[allow(missing_docs, reason = "the size")]
        width: u32,
        #[allow(missing_docs, reason = "the size")]
        height: u32,
    },
    /// The format's row does not have this frame rate with this scan.
    #[error(
        "{width}x{height} {scan} at frame_rate_code {frame_rate_code}: A/53 Part 4 Table 6.2 \
         (p.9) and SCTE 43 Table 3 (p.3) allow {allowed:?} for this format and scan"
    )]
    FormatFrameRate {
        #[allow(missing_docs, reason = "the size")]
        width: u32,
        #[allow(missing_docs, reason = "the size")]
        height: u32,
        /// "interlaced" or "progressive".
        scan: &'static str,
        #[allow(missing_docs, reason = "the rate")]
        frame_rate_code: u8,
        /// The codes the row has.
        allowed: &'static [u8],
    },
    /// The format's row does not have this aspect ratio.
    #[error(
        "{width}x{height} with aspect_ratio_information {aspect}: A/53 Part 4 Table 6.2 (p.9) \
         and SCTE 43 Table 3 (p.3) allow {allowed:?} for this format"
    )]
    FormatAspect {
        #[allow(missing_docs, reason = "the size")]
        width: u32,
        #[allow(missing_docs, reason = "the size")]
        height: u32,
        #[allow(missing_docs, reason = "the code")]
        aspect: u8,
        /// The codes the row has.
        allowed: &'static [u8],
    },
    /// bit_rate of zero.
    #[error("bit_rate shall not be zero (H.262 §6.3.3, p.39): no ceiling of the rate was given")]
    NoBitRate,
    /// bit_rate above the level.
    #[error(
        "bit_rate {bit_rate} bit/s exceeds Main Profile at {level} level: {max} bit/s (H.262 \
         Table 8-13, p.116)"
    )]
    LevelBitRate {
        #[allow(missing_docs, reason = "the rate")]
        bit_rate: u64,
        /// The level's name.
        level: &'static str,
        #[allow(missing_docs, reason = "the limit")]
        max: u64,
    },
    /// bit_rate above the profile.
    #[error(
        "bit_rate {bit_rate} bit/s exceeds the {profile} ceiling of {max} bit/s (A/53 Part 4 \
         §6.1.1 Table 6.1, p.9; SCTE 43 §7.1.1 Table 2, p.2)"
    )]
    ProfileBitRate {
        #[allow(missing_docs, reason = "the rate")]
        bit_rate: u64,
        /// The profile's name.
        profile: &'static str,
        #[allow(missing_docs, reason = "the limit")]
        max: u64,
    },
    /// Buffer below one unit or not a whole number of units.
    #[error(
        "vbv_buffer_size {0} bits: it is coded in units of 16384 bits and shall be at least one \
         (H.262 §6.3.3, p.40)"
    )]
    VbvUnits(u64),
    /// Buffer above the level.
    #[error(
        "vbv_buffer_size {vbv_bits} bits exceeds Main Profile at {level} level: {max} bits \
         (H.262 Table 8-14, p.116)"
    )]
    LevelVbv {
        #[allow(missing_docs, reason = "the size")]
        vbv_bits: u64,
        /// The level's name.
        level: &'static str,
        #[allow(missing_docs, reason = "the limit")]
        max: u64,
    },
    /// Buffer above 488 units.
    #[error(
        "vbv_buffer_size {0} bits exceeds vbv_buffer_size_value 488 (7 995 392 bits; A/53 Part 4 \
         Table 6.1, p.9; SCTE 43 Table 2, p.2)"
    )]
    ProfileVbv(u64),
    /// More than one second of buffer at the ceiling rate.
    #[error(
        "buffer delay vbv_buffer_size / bit_rate = {vbv_bits} / {bit_rate} exceeds the one \
         second that data may stay in the T-STD buffers (H.222.0 §2.4.2.6, p.19)"
    )]
    BufferDelay {
        #[allow(missing_docs, reason = "the size")]
        vbv_bits: u64,
        #[allow(missing_docs, reason = "the rate")]
        bit_rate: u64,
    },
    /// A color description field that is forbidden, reserved or "unspecified".
    #[error(
        "color description ({0}, {1}, {2}): each of colour_primaries, transfer_characteristics \
         and matrix_coefficients must be a defined value of H.262 Tables 6-7 to 6-9 (p.43-44), \
         explicit as A/53 Part 4 §6.1.4 (p.10) requires"
    )]
    Color(u8, u8, u8),
}

/// A row of the format tables: A/53 Part 4 Table 6.2 (p.9), and the rows SCTE 43 Table 3 (p.3)
/// adds (`cable_only`).
struct Row {
    height: u32,
    width: u32,
    aspects: &'static [u8],
    progressive: &'static [u8],
    interlaced: &'static [u8],
    cable_only: bool,
}

const ALL_RATES: &[u8] = &[1, 2, 4, 5, 7, 8];
const FILM_TO_30: &[u8] = &[1, 2, 4, 5];
const VIDEO_30: &[u8] = &[4, 5];

const fn row(
    height: u32,
    width: u32,
    aspects: &'static [u8],
    progressive: &'static [u8],
    interlaced: &'static [u8],
    cable_only: bool,
) -> Row {
    Row {
        height,
        width,
        aspects,
        progressive,
        interlaced,
        cable_only,
    }
}

const FORMATS: [Row; 9] = [
    row(1080, 1920, &[1, 3], FILM_TO_30, VIDEO_30, false),
    row(720, 1280, &[1, 3], ALL_RATES, &[], false),
    row(480, 704, &[2, 3], ALL_RATES, VIDEO_30, false),
    row(480, 640, &[1, 2], ALL_RATES, VIDEO_30, false),
    row(1080, 1440, &[3], FILM_TO_30, VIDEO_30, true),
    row(480, 720, &[2, 3], ALL_RATES, VIDEO_30, true),
    row(480, 544, &[2], &[1], &[4], true),
    row(480, 528, &[2], &[1], &[4], true),
    row(480, 352, &[2], &[1], &[4], true),
];
// 1088 lines are coded for 1080, but vertical_size_value is 1080 (Table 6.2 note a): a
// configured height of 1088 is not a format.

/// Check a sequence configuration against a profile and derive what its headers carry.
///
/// Under every profile: the size is even, not zero and not a multiple of 4096 in either
/// direction; the frame rate is a frame_rate_value of Table 6-4; size and rate fit Main
/// Profile at High level, and the lowest level that fits is signaled; the aspect code is 1, 2
/// or 3; bit_rate is not zero and within the level; the buffer is a whole number of units of
/// 16384 bits within the level (so neither extension field is ever non-zero).
///
/// Under a broadcast profile also: (size, scan, frame rate, aspect) is a row of A/53 Part 4
/// Table 6.2; bit_rate and the buffer are within the profile's ceilings; the buffer delay
/// `vbv_bits / max_bitrate` is at most one second.
pub fn check(c: &SeqConfig, profile: Profile) -> Result<SeqParams, ProfileError> {
    let (width, height) = (c.width, c.height);
    if width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || width.is_multiple_of(4096)
        || height.is_multiple_of(4096)
    {
        return Err(ProfileError::Size { width, height });
    }
    let code = frame_rate_code(c.fps_n, c.fps_d).ok_or(ProfileError::FrameRate {
        fps_n: c.fps_n,
        fps_d: c.fps_d,
    })?;
    let level =
        Level::lowest(width, height, c.interlaced, code).ok_or(ProfileError::AboveHighLevel {
            width,
            height,
            frame_rate_code: code,
        })?;
    if c.aspect > 3 {
        return Err(ProfileError::Aspect(c.aspect));
    }
    let scan = if c.interlaced {
        "interlaced"
    } else {
        "progressive"
    };
    let aspect = if profile.broadcast() {
        let row = FORMATS
            .iter()
            .find(|r| r.width == width && r.height == height)
            .ok_or(ProfileError::Format {
                width,
                height,
                scan,
            })?;
        if row.cable_only {
            return Err(ProfileError::CableOnlyFormat { width, height });
        }
        let rates = if c.interlaced {
            row.interlaced
        } else {
            row.progressive
        };
        if rates.is_empty() {
            return Err(ProfileError::Format {
                width,
                height,
                scan,
            });
        }
        if !rates.contains(&code) {
            return Err(ProfileError::FormatFrameRate {
                width,
                height,
                scan,
                frame_rate_code: code,
                allowed: rates,
            });
        }
        // The default: 16:9 where the row has it and the samples are not 4:3 shaped, 4:3 for
        // the 480-line formats (as before for 704x480; 640x480 is 4:3 with square samples).
        let aspect = match c.aspect {
            0 if height == 480 => 2,
            0 => 3,
            a => a,
        };
        if !row.aspects.contains(&aspect) {
            return Err(ProfileError::FormatAspect {
                width,
                height,
                aspect,
                allowed: row.aspects,
            });
        }
        aspect
    } else {
        match c.aspect {
            0 if u64::from(width) * 3 == u64::from(height) * 4
                || ((width == 720 || width == 704) && height == 480) =>
            {
                2
            }
            0 => 3,
            a => a,
        }
    };
    if c.max_bitrate == 0 {
        return Err(ProfileError::NoBitRate);
    }
    if c.max_bitrate > level.max_bit_rate() {
        return Err(ProfileError::LevelBitRate {
            bit_rate: c.max_bitrate,
            level: level.name(),
            max: level.max_bit_rate(),
        });
    }
    if let Some(max) = profile.max_bit_rate()
        && c.max_bitrate > max
    {
        return Err(ProfileError::ProfileBitRate {
            bit_rate: c.max_bitrate,
            profile: profile.name(),
            max,
        });
    }
    if c.vbv_bits < VBV_UNIT || !c.vbv_bits.is_multiple_of(VBV_UNIT) {
        return Err(ProfileError::VbvUnits(c.vbv_bits));
    }
    if c.vbv_bits > level.max_vbv_bits() {
        return Err(ProfileError::LevelVbv {
            vbv_bits: c.vbv_bits,
            level: level.name(),
            max: level.max_vbv_bits(),
        });
    }
    if let Some(max) = profile.max_vbv_bits()
        && c.vbv_bits > max
    {
        return Err(ProfileError::ProfileVbv(c.vbv_bits));
    }
    if profile.broadcast() && c.vbv_bits > c.max_bitrate {
        return Err(ProfileError::BufferDelay {
            vbv_bits: c.vbv_bits,
            bit_rate: c.max_bitrate,
        });
    }
    let color = c.color.unwrap_or(Color::for_height(height));
    if !color.defined() {
        return Err(ProfileError::Color(
            color.primaries,
            color.transfer,
            color.matrix,
        ));
    }
    // Both fit their fields: the level limits are below 2^18 x 400 bit/s and 2^10 units.
    let bit_rate_value = u32::try_from(c.max_bitrate.div_ceil(400)).unwrap_or(u32::MAX);
    let vbv_buffer_size = u16::try_from(c.vbv_bits / VBV_UNIT).unwrap_or(u16::MAX);
    Ok(SeqParams {
        frame_rate_code: code,
        aspect,
        level,
        bit_rate_value,
        vbv_buffer_size,
        color,
    })
}

/// The largest buffer a sequence at this level may declare under a profile with this ceiling
/// rate, in bits: the level's and the profile's limits, at most one second of the rate under a
/// broadcast profile, in whole units. What a driver uses when nobody gives a buffer.
pub fn max_vbv_bits(level: Level, profile: Profile, max_bitrate: u64) -> u64 {
    let mut v = level.max_vbv_bits();
    if let Some(p) = profile.max_vbv_bits() {
        v = v.min(p);
    }
    if profile.broadcast() {
        v = v.min(max_bitrate);
    }
    v / VBV_UNIT * VBV_UNIT
}

/// The highest ceiling rate a sequence at this level may declare under a profile, bit/s.
pub fn max_bit_rate(level: Level, profile: Profile) -> u64 {
    profile
        .max_bit_rate()
        .map_or(level.max_bit_rate(), |p| p.min(level.max_bit_rate()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(width: u32, height: u32, fps: (u32, u32), interlaced: bool) -> SeqConfig {
        SeqConfig {
            width,
            height,
            fps_n: fps.0,
            fps_d: fps.1,
            interlaced,
            aspect: 0,
            max_bitrate: 4_000_000,
            vbv_bits: 1_835_008,
            color: None,
        }
    }

    const NTSC: (u32, u32) = (30000, 1001);

    /// Table 6-4, reduced fractions, and everything else refused.
    #[test]
    fn frame_rate_codes() {
        for (n, d, code) in [
            (24000, 1001, 1),
            (24, 1, 2),
            (25, 1, 3),
            (30000, 1001, 4),
            (30, 1, 5),
            (50, 1, 6),
            (60000, 1001, 7),
            (60, 1, 8),
            // not reduced
            (48000, 2002, 1),
            (24000, 1000, 2),
            (25000, 1000, 3),
            (50000, 1000, 6),
            (60000, 1000, 8),
            (2997 * 1000, 100 * 1001, 0), // 29.94...: not 30000/1001
            (120, 2, 8),
        ] {
            let want = (code != 0).then_some(code);
            assert_eq!(frame_rate_code(n, d), want, "{n}/{d}");
        }
        for (n, d) in [
            (0, 1),
            (1, 0),
            (2997, 100),
            (15, 1),
            (120, 1),
            (30000, 1000 + 2),
        ] {
            assert_eq!(frame_rate_code(n, d), None, "{n}/{d}");
        }
    }

    /// The levels of `docs/notes/format-conformance.md` §2.1, computed from Tables 8-8, 8-11, 8-12.
    #[test]
    fn lowest_levels() {
        use Level::{High, High1440, Main};
        for (w, h, il, code, want) in [
            (1920, 1080, true, 4, Some(High)),
            (1920, 1080, false, 5, Some(High)), // 62 668 800: equal to the limit
            (1920, 1080, false, 1, Some(High)),
            (1440, 1080, true, 4, Some(High1440)),
            (1440, 1080, false, 5, Some(High1440)), // 47 001 600: equal to the limit
            (1280, 720, false, 7, Some(High)),
            (1280, 720, false, 8, Some(High)),
            (1280, 720, false, 5, Some(High1440)),
            (1280, 720, false, 1, Some(High1440)),
            (720, 480, true, 5, Some(Main)), // 10 368 000: equal to the limit
            (704, 480, true, 4, Some(Main)),
            (704, 480, false, 7, Some(High1440)),
            (640, 480, false, 8, Some(High1440)),
            (720, 576, true, 3, Some(Main)),
            (720, 576, true, 4, Some(High1440)), // above 480 lines Main level is 25 Hz only
            (176, 144, false, 4, Some(Main)),
            (176, 144, false, 6, Some(High1440)),
            (1920, 1080, false, 7, None), // 125.2 Msample/s
            (1920, 1090, false, 4, None),
            (1922, 1080, false, 4, None),
        ] {
            assert_eq!(Level::lowest(w, h, il, code), want, "{w}x{h} {il} {code}");
        }
        assert_eq!(Level::Main.indication(), 0x48);
        assert_eq!(Level::High1440.indication(), 0x46);
        assert_eq!(Level::High.indication(), 0x44);
    }

    /// Every row of A/53 Part 4 Table 6.2 passes under every profile with every aspect and
    /// rate it lists, and nothing else of the grid does under a broadcast profile.
    #[test]
    fn format_table() {
        type TableRow = (u32, u32, &'static [u8], &'static [u8], &'static [u8]);
        let rows: [TableRow; 4] = [
            (1920, 1080, &[1, 3], &[1, 2, 4, 5], &[4, 5]),
            (1280, 720, &[1, 3], &[1, 2, 4, 5, 7, 8], &[]),
            (704, 480, &[2, 3], &[1, 2, 4, 5, 7, 8], &[4, 5]),
            (640, 480, &[1, 2], &[1, 2, 4, 5, 7, 8], &[4, 5]),
        ];
        let sizes = [
            (1920, 1080),
            (1280, 720),
            (704, 480),
            (640, 480),
            (1440, 1080),
            (720, 480),
            (544, 480),
            (528, 480),
            (352, 480),
            (1920, 1088),
            (1280, 1080),
            (720, 576),
            (696, 470),
            (176, 144),
        ];
        for profile in [Profile::A53, Profile::Scte43Qam64, Profile::Scte43Qam256] {
            for (w, h) in sizes {
                for il in [false, true] {
                    for code in 1..=8u8 {
                        for aspect in 1..=3u8 {
                            let (n, d) = frame_rate_value(code).unwrap();
                            let mut c = cfg(w, h, (n, d), il);
                            c.aspect = aspect;
                            c.vbv_bits = 1_835_008;
                            c.max_bitrate = 4_000_000;
                            let legal = rows.iter().any(|r| {
                                r.0 == w
                                    && r.1 == h
                                    && r.2.contains(&aspect)
                                    && if il { r.4 } else { r.3 }.contains(&code)
                            });
                            let got = check(&c, profile);
                            assert_eq!(
                                got.is_ok(),
                                legal,
                                "{w}x{h} il {il} code {code} aspect {aspect}: {got:?}"
                            );
                        }
                    }
                }
            }
        }
        // the cable-only sizes say why
        for (w, h) in [(1440, 1080), (720, 480), (544, 480), (528, 480), (352, 480)] {
            assert_eq!(
                check(&cfg(w, h, NTSC, true), Profile::Scte43Qam256),
                Err(ProfileError::CableOnlyFormat {
                    width: w,
                    height: h
                })
            );
        }
        assert!(matches!(
            check(&cfg(1280, 720, NTSC, true), Profile::A53),
            Err(ProfileError::Format { .. })
        ));
        assert!(matches!(
            check(&cfg(704, 480, (60000, 1001), true), Profile::A53),
            Err(ProfileError::FormatFrameRate {
                allowed: &[4, 5],
                ..
            })
        ));
        assert!(matches!(
            check(&cfg(704, 480, (25, 1), false), Profile::A53),
            Err(ProfileError::FormatFrameRate { .. })
        ));
    }

    #[test]
    fn default_aspects() {
        let a = |w, h, il, p| check(&cfg(w, h, NTSC, il), p).unwrap().aspect;
        assert_eq!(a(1920, 1080, true, Profile::Scte43Qam256), 3);
        assert_eq!(a(1280, 720, false, Profile::A53), 3);
        assert_eq!(a(704, 480, true, Profile::Scte43Qam64), 2);
        assert_eq!(a(640, 480, true, Profile::A53), 2);
        assert_eq!(a(176, 144, false, Profile::H262), 3);
        assert_eq!(a(720, 480, true, Profile::H262), 2);
        assert_eq!(a(640, 480, true, Profile::H262), 2);
        let mut c = cfg(640, 480, NTSC, true);
        c.aspect = 3;
        assert!(matches!(
            check(&c, Profile::A53),
            Err(ProfileError::FormatAspect { aspect: 3, .. })
        ));
        c.aspect = 4;
        assert_eq!(check(&c, Profile::H262), Err(ProfileError::Aspect(4)));
    }

    #[test]
    fn sizes_refused_under_every_profile() {
        for p in Profile::ALL {
            for (w, h) in [(0, 480), (704, 0), (703, 480), (704, 479), (4096, 32)] {
                assert!(
                    matches!(
                        check(&cfg(w, h, NTSC, false), p),
                        Err(ProfileError::Size { .. })
                    ),
                    "{w}x{h}"
                );
            }
            for (w, h) in [(1922, 1080), (1920, 1090), (2048, 2048), (176, 2800 + 16)] {
                assert!(
                    matches!(
                        check(&cfg(w, h, NTSC, false), p),
                        Err(ProfileError::AboveHighLevel { .. })
                    ),
                    "{w}x{h}"
                );
            }
            assert!(matches!(
                check(&cfg(704, 480, (2997, 100), true), p),
                Err(ProfileError::FrameRate { .. })
            ));
        }
        assert!(check(&cfg(1920, 1088, NTSC, false), Profile::H262).is_ok());
    }

    #[test]
    fn rates_and_buffers() {
        let ok = cfg(704, 480, NTSC, true);
        let p = check(&ok, Profile::Scte43Qam256).unwrap();
        assert_eq!(
            (p.bit_rate_value, p.vbv_buffer_size, p.level),
            (10_000, 112, Level::Main)
        );
        assert_eq!(p.color, Color::SMPTE170M);
        let with = |f: &dyn Fn(&mut SeqConfig), p| {
            let mut c = ok;
            f(&mut c);
            check(&c, p)
        };
        // the rate is written rounded up
        let r = with(&|c| c.max_bitrate = 4_000_001, Profile::H262).unwrap();
        assert_eq!(r.bit_rate_value, 10_001);
        assert_eq!(
            with(&|c| c.max_bitrate = 0, Profile::H262),
            Err(ProfileError::NoBitRate)
        );
        // Main level: 15 Mbit/s and 1 835 008 bits
        assert!(with(&|c| c.max_bitrate = 15_000_000, Profile::H262).is_ok());
        assert!(matches!(
            with(&|c| c.max_bitrate = 15_000_001, Profile::H262),
            Err(ProfileError::LevelBitRate { .. })
        ));
        assert!(matches!(
            with(&|c| c.vbv_bits = 1_835_008 + 16384, Profile::H262),
            Err(ProfileError::LevelVbv { .. })
        ));
        for v in [0, 16383, 16385, 1_835_000] {
            assert_eq!(
                with(&|c| c.vbv_bits = v, Profile::H262),
                Err(ProfileError::VbvUnits(v))
            );
        }
        // one second of buffer at most, under a broadcast profile
        let slow = |c: &mut SeqConfig| {
            c.max_bitrate = 1_500_000;
            c.vbv_bits = 1_835_008;
        };
        assert!(with(&slow, Profile::H262).is_ok());
        assert!(matches!(
            with(&slow, Profile::Scte43Qam256),
            Err(ProfileError::BufferDelay { .. })
        ));
        // High level under the profiles: 488 units; 19.4, 27.0 and 38.8 Mbit/s
        let hd = cfg(1920, 1080, NTSC, true);
        for (p, max) in [
            (Profile::A53, 19_400_000),
            (Profile::Scte43Qam64, 27_000_000),
            (Profile::Scte43Qam256, 38_800_000),
        ] {
            let mut c = hd;
            c.vbv_bits = 488 * 16384;
            c.max_bitrate = max;
            let r = check(&c, p).unwrap();
            assert_eq!(u64::from(r.bit_rate_value) * 400, max);
            assert_eq!(r.vbv_buffer_size, 488);
            c.max_bitrate = max + 1;
            assert!(matches!(
                check(&c, p),
                Err(ProfileError::ProfileBitRate { .. })
            ));
            c.max_bitrate = max;
            c.vbv_bits = 489 * 16384;
            assert_eq!(check(&c, p), Err(ProfileError::ProfileVbv(489 * 16384)));
            assert_eq!(max_bit_rate(Level::High, p), max);
            assert_eq!(max_vbv_bits(Level::High, p, max), 488 * 16384);
        }
        let mut c = hd;
        c.max_bitrate = 80_000_000;
        c.vbv_bits = 9_781_248;
        assert!(check(&c, Profile::H262).is_ok());
        c.vbv_bits += 16384;
        assert!(matches!(
            check(&c, Profile::H262),
            Err(ProfileError::LevelVbv { .. })
        ));
        // High-1440: 448 units bind before the profile's 488
        let mut c = cfg(1280, 720, NTSC, false);
        c.max_bitrate = 12_000_000;
        c.vbv_bits = 449 * 16384;
        assert!(matches!(
            check(&c, Profile::Scte43Qam256),
            Err(ProfileError::LevelVbv { .. })
        ));
        assert_eq!(
            max_vbv_bits(Level::High1440, Profile::Scte43Qam256, 12_000_000),
            448 * 16384
        );
        assert_eq!(
            max_vbv_bits(Level::Main, Profile::Scte43Qam256, 1_500_000),
            1_500_000 / 16384 * 16384
        );
        assert_eq!(max_bit_rate(Level::Main, Profile::Scte43Qam256), 15_000_000);
    }

    #[test]
    fn color_descriptions() {
        let mut c = cfg(704, 480, NTSC, true);
        c.color = Some(Color::BT709);
        assert_eq!(check(&c, Profile::A53).unwrap().color, Color::BT709);
        assert_eq!(
            check(&cfg(1920, 1080, NTSC, true), Profile::A53)
                .unwrap()
                .color,
            Color::BT709
        );
        for bad in [(0, 1, 1), (1, 2, 1), (1, 1, 3), (9, 1, 1), (1, 1, 0)] {
            c.color = Some(Color {
                primaries: bad.0,
                transfer: bad.1,
                matrix: bad.2,
            });
            assert_eq!(
                check(&c, Profile::H262),
                Err(ProfileError::Color(bad.0, bad.1, bad.2))
            );
        }
    }

    #[test]
    fn profile_names() {
        for p in Profile::ALL {
            assert_eq!(Profile::from_name(p.name()), Some(p));
        }
        assert_eq!(Profile::from_name("pal"), None);
        assert_eq!(Profile::default(), Profile::Scte43Qam256);
    }
}
