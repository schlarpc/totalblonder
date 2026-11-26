//! What an encoder is configured with, validated once so that the encoder cannot be asked for
//! something the standard or ATSC does not allow.
//!
//! Choices that only make sense together are one enum: the mix levels and the Dolby Surround
//! flag live inside [`AudioCodingMode`], so a 2/0 stream cannot carry a center mix level
//! (A/52 Table 5.2, p.31). Numeric fields with a legal range are newtypes with validating
//! constructors ([`Dialnorm`], [`MixLevel`], [`BandwidthCode`]).

use crate::tables::{BIT_RATES_KBPS, WORDS_PER_FRAME_48K, words_five_eighths};

/// Sample rate. Only 48 kHz is supported: it is the only rate ATSC allows
/// (A/53 Part 5:2014 §5.4, p.7; fscod `00`, A/52 Table 5.6, p.37).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SampleRate {
    /// 48 kHz, fscod `00`.
    #[default]
    Hz48000,
}

impl SampleRate {
    /// The 2-bit fscod (A/52 Table 5.6, p.37).
    pub fn fscod(self) -> u32 {
        match self {
            SampleRate::Hz48000 => 0,
        }
    }

    /// Samples per second.
    pub fn hz(self) -> u32 {
        match self {
            SampleRate::Hz48000 => 48_000,
        }
    }
}

/// Nominal bit rate: the rows of the frame size code table (A/52 Table 5.18, p.51–52).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(missing_docs)]
pub enum BitRate {
    Kbps32,
    Kbps40,
    Kbps48,
    Kbps56,
    Kbps64,
    Kbps80,
    Kbps96,
    Kbps112,
    Kbps128,
    Kbps160,
    Kbps192,
    Kbps224,
    Kbps256,
    Kbps320,
    Kbps384,
    Kbps448,
    Kbps512,
    Kbps576,
    Kbps640,
}

impl BitRate {
    /// Every rate, in table order.
    pub const ALL: [BitRate; 19] = [
        BitRate::Kbps32,
        BitRate::Kbps40,
        BitRate::Kbps48,
        BitRate::Kbps56,
        BitRate::Kbps64,
        BitRate::Kbps80,
        BitRate::Kbps96,
        BitRate::Kbps112,
        BitRate::Kbps128,
        BitRate::Kbps160,
        BitRate::Kbps192,
        BitRate::Kbps224,
        BitRate::Kbps256,
        BitRate::Kbps320,
        BitRate::Kbps384,
        BitRate::Kbps448,
        BitRate::Kbps512,
        BitRate::Kbps576,
        BitRate::Kbps640,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// The rate with this many kbit/s, if it is in the table.
    pub fn from_kbps(kbps: u32) -> Option<Self> {
        BIT_RATES_KBPS
            .iter()
            .position(|&k| k == kbps)
            .map(|i| Self::ALL[i])
    }

    /// kbit/s.
    pub fn kbps(self) -> u32 {
        BIT_RATES_KBPS[self.index()]
    }

    /// frmsizecod: the even code of the pair; at 48 kHz both codes mean the same size
    /// (A/52 Table 5.18, p.51).
    pub fn frmsizecod(self) -> u32 {
        2 * self.index() as u32
    }

    /// Syncframe length in 16-bit words at 48 kHz (A/52 Table 5.18, p.51–52).
    pub fn frame_words(self) -> u32 {
        WORDS_PER_FRAME_48K[self.index()]
    }

    /// Syncframe length in bytes at 48 kHz.
    pub fn frame_bytes(self) -> usize {
        self.frame_words() as usize * 2
    }

    /// Bytes in the first 5/8 of the syncframe (A/52 §7.10.1, p.104).
    pub fn five_eighths_bytes(self) -> usize {
        words_five_eighths(self.frame_words()) as usize * 2
    }
}

/// Center mix level, cmixlev (A/52 Table 5.9, p.39).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CenterMixLevel {
    /// 0.707 (−3 dB), `00`.
    Minus3Db,
    /// 0.595 (−4.5 dB), `01`.
    #[default]
    Minus4_5Db,
    /// 0.500 (−6 dB), `10`.
    Minus6Db,
}

impl CenterMixLevel {
    /// The 2-bit code.
    pub fn code(self) -> u32 {
        match self {
            CenterMixLevel::Minus3Db => 0,
            CenterMixLevel::Minus4_5Db => 1,
            CenterMixLevel::Minus6Db => 2,
        }
    }
}

/// Surround mix level, surmixlev (A/52 Table 5.10, p.39).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SurroundMixLevel {
    /// 0.707 (−3 dB), `00`.
    Minus3Db,
    /// 0.500 (−6 dB), `01`.
    #[default]
    Minus6Db,
    /// 0, `10`.
    Zero,
}

impl SurroundMixLevel {
    /// The 2-bit code.
    pub fn code(self) -> u32 {
        match self {
            SurroundMixLevel::Minus3Db => 0,
            SurroundMixLevel::Minus6Db => 1,
            SurroundMixLevel::Zero => 2,
        }
    }
}

/// Dolby Surround mode, dsurmod, for 2/0 streams (A/52 Table 5.11, p.40).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DolbySurroundMode {
    /// `00`.
    #[default]
    NotIndicated,
    /// `01`.
    NotDolbySurround,
    /// `10`.
    DolbySurround,
}

impl DolbySurroundMode {
    /// The 2-bit code.
    pub fn code(self) -> u32 {
        match self {
            DolbySurroundMode::NotIndicated => 0,
            DolbySurroundMode::NotDolbySurround => 1,
            DolbySurroundMode::DolbySurround => 2,
        }
    }
}

/// Audio coding mode, acmod, with the BSI fields that only exist for it (A/52 Table 5.8, p.39;
/// Table 5.2, p.31). Dual mono (1+1, acmod 0) is not offered: ATSC prohibits it
/// (A/53 Part 5:2014 §5.4, p.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCodingMode {
    /// 1/0: C.
    Mono,
    /// 2/0: L, R.
    Stereo {
        /// dsurmod.
        surround: DolbySurroundMode,
    },
    /// 3/0: L, C, R.
    ThreeZero {
        /// cmixlev.
        center: CenterMixLevel,
    },
    /// 2/1: L, R, S.
    TwoOne {
        /// surmixlev.
        surround: SurroundMixLevel,
    },
    /// 3/1: L, C, R, S.
    ThreeOne {
        /// cmixlev.
        center: CenterMixLevel,
        /// surmixlev.
        surround: SurroundMixLevel,
    },
    /// 2/2: L, R, SL, SR.
    TwoTwo {
        /// surmixlev.
        surround: SurroundMixLevel,
    },
    /// 3/2: L, C, R, SL, SR.
    ThreeTwo {
        /// cmixlev.
        center: CenterMixLevel,
        /// surmixlev.
        surround: SurroundMixLevel,
    },
}

impl AudioCodingMode {
    /// 2/0 with dsurmod "not indicated".
    pub const STEREO: Self = AudioCodingMode::Stereo {
        surround: DolbySurroundMode::NotIndicated,
    };

    /// 3/2 with the default mix levels (−4.5 dB center, −6 dB surround).
    pub const THREE_TWO: Self = AudioCodingMode::ThreeTwo {
        center: CenterMixLevel::Minus4_5Db,
        surround: SurroundMixLevel::Minus6Db,
    };

    /// The 3-bit acmod (A/52 Table 5.8, p.39).
    pub fn acmod(self) -> u32 {
        match self {
            AudioCodingMode::Mono => 1,
            AudioCodingMode::Stereo { .. } => 2,
            AudioCodingMode::ThreeZero { .. } => 3,
            AudioCodingMode::TwoOne { .. } => 4,
            AudioCodingMode::ThreeOne { .. } => 5,
            AudioCodingMode::TwoTwo { .. } => 6,
            AudioCodingMode::ThreeTwo { .. } => 7,
        }
    }

    /// Number of full-bandwidth channels, nfchans (A/52 Table 5.8, p.39).
    pub fn nfchans(self) -> usize {
        match self {
            AudioCodingMode::Mono => 1,
            AudioCodingMode::Stereo { .. } => 2,
            AudioCodingMode::ThreeZero { .. } | AudioCodingMode::TwoOne { .. } => 3,
            AudioCodingMode::ThreeOne { .. } | AudioCodingMode::TwoTwo { .. } => 4,
            AudioCodingMode::ThreeTwo { .. } => 5,
        }
    }

    /// cmixlev, present when there are three front channels (A/52 Table 5.2, p.31).
    pub fn center_mix_level(self) -> Option<CenterMixLevel> {
        match self {
            AudioCodingMode::ThreeZero { center }
            | AudioCodingMode::ThreeOne { center, .. }
            | AudioCodingMode::ThreeTwo { center, .. } => Some(center),
            AudioCodingMode::Mono
            | AudioCodingMode::Stereo { .. }
            | AudioCodingMode::TwoOne { .. }
            | AudioCodingMode::TwoTwo { .. } => None,
        }
    }

    /// surmixlev, present when a surround channel exists (A/52 Table 5.2, p.31).
    pub fn surround_mix_level(self) -> Option<SurroundMixLevel> {
        match self {
            AudioCodingMode::TwoOne { surround }
            | AudioCodingMode::ThreeOne { surround, .. }
            | AudioCodingMode::TwoTwo { surround }
            | AudioCodingMode::ThreeTwo { surround, .. } => Some(surround),
            AudioCodingMode::Mono
            | AudioCodingMode::Stereo { .. }
            | AudioCodingMode::ThreeZero { .. } => None,
        }
    }

    /// dsurmod, present in 2/0 (A/52 Table 5.2, p.31).
    pub fn dolby_surround(self) -> Option<DolbySurroundMode> {
        match self {
            AudioCodingMode::Stereo { surround } => Some(surround),
            _ => None,
        }
    }

    /// Channel names in A/52 order, the order of the encoder's input (A/52 Table 5.8, p.39).
    pub fn channel_names(self) -> &'static [&'static str] {
        match self {
            AudioCodingMode::Mono => &["C"],
            AudioCodingMode::Stereo { .. } => &["L", "R"],
            AudioCodingMode::ThreeZero { .. } => &["L", "C", "R"],
            AudioCodingMode::TwoOne { .. } => &["L", "R", "S"],
            AudioCodingMode::ThreeOne { .. } => &["L", "C", "R", "S"],
            AudioCodingMode::TwoTwo { .. } => &["L", "R", "SL", "SR"],
            AudioCodingMode::ThreeTwo { .. } => &["L", "C", "R", "SL", "SR"],
        }
    }
}

/// Bit stream mode, bsmod (A/52 Table 5.7, p.38).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BitstreamMode {
    /// Main audio service: complete main (CM), `000`.
    #[default]
    CompleteMain,
    /// Main audio service: music and effects (ME), `001`.
    MusicAndEffects,
    /// Associated service: visually impaired (VI), `010`.
    VisuallyImpaired,
    /// Associated service: hearing impaired (HI), `011`.
    HearingImpaired,
    /// Associated service: dialogue (D), `100`.
    Dialogue,
    /// Associated service: commentary (C), `101`.
    Commentary,
    /// Associated service: emergency (E), `110`.
    Emergency,
    /// Associated service: voice over (VO), `111` with acmod 1/0 only.
    VoiceOver,
    /// Main audio service: karaoke, `111` with acmod 2/0 or more.
    Karaoke,
}

impl BitstreamMode {
    /// The 3-bit code.
    pub fn code(self) -> u32 {
        match self {
            BitstreamMode::CompleteMain => 0,
            BitstreamMode::MusicAndEffects => 1,
            BitstreamMode::VisuallyImpaired => 2,
            BitstreamMode::HearingImpaired => 3,
            BitstreamMode::Dialogue => 4,
            BitstreamMode::Commentary => 5,
            BitstreamMode::Emergency => 6,
            BitstreamMode::VoiceOver | BitstreamMode::Karaoke => 7,
        }
    }
}

/// Dialogue level below digital full scale, dialnorm: 1..=31 meaning −1..−31 dB; 0 is reserved
/// (A/52 §5.4.2.8, p.40).
///
/// The default is 24 (−24 dBFS): every track is normalized to −24 LKFS and must say so
/// (A/85:2013 §4 p.14, §7.3.5 p.21; docs/SPEC.md §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dialnorm(u8);

impl Dialnorm {
    /// −24 dB, the A/85 target.
    pub const ATSC_TARGET: Self = Dialnorm(24);

    /// `db` dB below full scale, 1..=31.
    pub fn new(db: u8) -> Result<Self, ConfigError> {
        if (1..=31).contains(&db) {
            Ok(Dialnorm(db))
        } else {
            Err(ConfigError::DialnormOutOfRange(db))
        }
    }

    /// The 5-bit code (equal to the dB value).
    pub fn code(self) -> u32 {
        u32::from(self.0)
    }
}

impl Default for Dialnorm {
    fn default() -> Self {
        Self::ATSC_TARGET
    }
}

/// Peak mixing level, mixlevel: 80 + n dB SPL, n in 0..=31 (A/52 §5.4.2.14, p.40–41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MixLevel(u8);

impl MixLevel {
    /// `n` = level − 80 dB SPL, 0..=31.
    pub fn new(n: u8) -> Result<Self, ConfigError> {
        if n <= 31 {
            Ok(MixLevel(n))
        } else {
            Err(ConfigError::MixLevelOutOfRange(n))
        }
    }

    /// The 5-bit code.
    pub fn code(self) -> u32 {
        u32::from(self.0)
    }
}

/// Mixing room type, roomtyp (A/52 Table 5.12, p.41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RoomType {
    /// `00`.
    #[default]
    NotIndicated,
    /// Large room, X curve monitor, `01`.
    LargeRoom,
    /// Small room, flat monitor, `10`.
    SmallRoom,
}

impl RoomType {
    /// The 2-bit code.
    pub fn code(self) -> u32 {
        match self {
            RoomType::NotIndicated => 0,
            RoomType::LargeRoom => 1,
            RoomType::SmallRoom => 2,
        }
    }
}

/// Audio production information: mixlevel and roomtyp, sent together (A/52 Table 5.2, p.31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioProduction {
    /// mixlevel.
    pub mix_level: MixLevel,
    /// roomtyp.
    pub room_type: RoomType,
}

/// Channel bandwidth code, chbwcod, 0..=60: the last coded bin is
/// `endmant = (chbwcod + 12)·3 + 37` (A/52 §5.4.3.24, p.46; §7.1.3, p.60).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BandwidthCode(u8);

impl BandwidthCode {
    /// chbwcod, 0..=60.
    pub fn new(code: u8) -> Result<Self, ConfigError> {
        if code <= 60 {
            Ok(BandwidthCode(code))
        } else {
            Err(ConfigError::BandwidthOutOfRange(code))
        }
    }

    /// The code whose bandwidth is nearest `hz`, clamped to the legal range.
    pub fn nearest_hz(hz: u32) -> Self {
        // A bin is 48000 / 512 = 93.75 Hz wide.
        let bins = f64::from(hz) / 93.75;
        let code = ((bins - 73.0) / 3.0).round().clamp(0.0, 60.0);
        BandwidthCode(code as u8)
    }

    /// The 6-bit code.
    pub fn code(self) -> u32 {
        u32::from(self.0)
    }

    /// endmant: one past the last coded bin (A/52 §7.1.3, p.60).
    pub fn end_mant(self) -> usize {
        (usize::from(self.0) + 12) * 3 + 37
    }

    /// Upper band edge in Hz at 48 kHz.
    pub fn hz(self) -> u32 {
        (self.end_mant() as f64 * 93.75) as u32
    }
}

/// How the full-bandwidth channels' bandwidth is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Bandwidth {
    /// From the bit rate per channel (see [`Config::bandwidth_code`]).
    #[default]
    Auto,
    /// This code for every full-bandwidth channel.
    Fixed(BandwidthCode),
}

/// A configuration that failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// dialnorm outside 1..=31 (A/52 §5.4.2.8, p.40).
    #[error("dialnorm {0} is outside 1..=31 dB (A/52 §5.4.2.8)")]
    DialnormOutOfRange(u8),
    /// mixlevel outside 0..=31 (A/52 §5.4.2.14).
    #[error("mixlevel {0} is outside 0..=31 (A/52 §5.4.2.14)")]
    MixLevelOutOfRange(u8),
    /// chbwcod above 60 (A/52 §5.4.3.24).
    #[error("chbwcod {0} is above 60 (A/52 §5.4.3.24)")]
    BandwidthOutOfRange(u8),
    /// A rate above what ATSC allows for one service.
    #[error("{kbps} kbit/s is above the 448 kbit/s ATSC limit (A/53 Part 5:2014 §5.3)")]
    AboveAtscLimit {
        /// The rate asked for.
        kbps: u32,
    },
    /// bsmod 7 means voice-over only in 1/0 and karaoke only in 2/0 and up (A/52 Table 5.7).
    #[error("bsmod {mode:?} is not defined for acmod {acmod} (A/52 Table 5.7)")]
    BsmodForAcmod {
        /// The mode asked for.
        mode: BitstreamMode,
        /// The audio coding mode.
        acmod: u32,
    },
    /// The rate cannot carry even the smallest legal frame for this layout and bandwidth.
    #[error(
        "{kbps} kbit/s is too low for {channels} channels at bandwidth code {bandwidth}: the \
         smallest frame needs {needed} bits of {available}"
    )]
    RateTooLow {
        /// The rate asked for.
        kbps: u32,
        /// Channels including LFE.
        channels: usize,
        /// The bandwidth code in use.
        bandwidth: u32,
        /// Bits the smallest frame needs (or bits blocks 0–1 need before the 5/8 point).
        needed: usize,
        /// Bits available.
        available: usize,
    },
}

/// Settings with defaults, turned into a [`Config`] by [`ConfigBuilder::build`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigBuilder {
    /// Sample rate.
    pub sample_rate: SampleRate,
    /// acmod and its mix levels.
    pub coding_mode: AudioCodingMode,
    /// lfeon.
    pub lfe: bool,
    /// Nominal bit rate.
    pub bit_rate: BitRate,
    /// dialnorm, default 24.
    pub dialnorm: Dialnorm,
    /// bsmod, default complete main.
    pub bitstream_mode: BitstreamMode,
    /// copyrightb, default false.
    pub copyright: bool,
    /// origbs, default true.
    pub original: bool,
    /// mixlevel and roomtyp, default absent.
    pub audio_production: Option<AudioProduction>,
    /// Full-bandwidth channel bandwidth, default automatic.
    pub bandwidth: Bandwidth,
    /// Short transforms on transients (A/52 §8.2.2), default on.
    pub block_switching: bool,
    /// Rematrixing in 2/0 (A/52 §7.5), default on.
    pub rematrixing: bool,
}

impl ConfigBuilder {
    /// Defaults for a layout and rate.
    pub fn new(coding_mode: AudioCodingMode, lfe: bool, bit_rate: BitRate) -> Self {
        Self {
            sample_rate: SampleRate::Hz48000,
            coding_mode,
            lfe,
            bit_rate,
            dialnorm: Dialnorm::ATSC_TARGET,
            bitstream_mode: BitstreamMode::CompleteMain,
            copyright: false,
            original: true,
            audio_production: None,
            bandwidth: Bandwidth::Auto,
            block_switching: true,
            rematrixing: true,
        }
    }

    /// Validates the settings.
    pub fn build(self) -> Result<Config, ConfigError> {
        if self.bit_rate > BitRate::Kbps448 {
            return Err(ConfigError::AboveAtscLimit {
                kbps: self.bit_rate.kbps(),
            });
        }
        let acmod = self.coding_mode.acmod();
        match self.bitstream_mode {
            BitstreamMode::VoiceOver if acmod != 1 => {
                return Err(ConfigError::BsmodForAcmod {
                    mode: self.bitstream_mode,
                    acmod,
                });
            }
            BitstreamMode::Karaoke if acmod < 2 => {
                return Err(ConfigError::BsmodForAcmod {
                    mode: self.bitstream_mode,
                    acmod,
                });
            }
            _ => {}
        }
        let config = Config { b: self };
        crate::encoder::check_rate_feasible(&config)?;
        Ok(config)
    }
}

/// A validated encoder configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    b: ConfigBuilder,
}

impl Config {
    /// Defaults for a layout and rate, validated.
    pub fn new(
        coding_mode: AudioCodingMode,
        lfe: bool,
        bit_rate: BitRate,
    ) -> Result<Self, ConfigError> {
        ConfigBuilder::new(coding_mode, lfe, bit_rate).build()
    }

    /// The settings this was built from.
    pub fn settings(&self) -> &ConfigBuilder {
        &self.b
    }

    /// Sample rate.
    pub fn sample_rate(&self) -> SampleRate {
        self.b.sample_rate
    }

    /// acmod with its mix levels.
    pub fn coding_mode(&self) -> AudioCodingMode {
        self.b.coding_mode
    }

    /// lfeon.
    pub fn lfe(&self) -> bool {
        self.b.lfe
    }

    /// Bit rate.
    pub fn bit_rate(&self) -> BitRate {
        self.b.bit_rate
    }

    /// dialnorm.
    pub fn dialnorm(&self) -> Dialnorm {
        self.b.dialnorm
    }

    /// bsmod.
    pub fn bitstream_mode(&self) -> BitstreamMode {
        self.b.bitstream_mode
    }

    /// Full-bandwidth channels.
    pub fn nfchans(&self) -> usize {
        self.b.coding_mode.nfchans()
    }

    /// All channels including LFE: the number of input slices `encode_frame` takes.
    pub fn channels(&self) -> usize {
        self.nfchans() + usize::from(self.b.lfe)
    }

    /// Syncframe length in bytes.
    pub fn frame_bytes(&self) -> usize {
        self.b.bit_rate.frame_bytes()
    }

    /// The bandwidth code of the full-bandwidth channels. Automatic choice, by bit rate per
    /// full-bandwidth channel `r`: ≥ 96 kbit/s → 20 kHz, ≥ 80 → 19 kHz, ≥ 64 → 18 kHz,
    /// ≥ 48 → 13 kHz, ≥ 32 → 10 kHz, below → 7 kHz. Without coupling, bandwidth is the main
    /// lever against bit starvation at low rates. The steps were set against FFmpeg's choices
    /// with coupling off (notes/ac3-encoder-log.md): the same at 2/0 192 and 3/2 384, wider
    /// below 64 kbit/s per channel, where FFmpeg drops to 6.8 kHz.
    pub fn bandwidth_code(&self) -> BandwidthCode {
        match self.b.bandwidth {
            Bandwidth::Fixed(code) => code,
            Bandwidth::Auto => {
                let r = self.b.bit_rate.kbps() / self.nfchans() as u32;
                let hz = match r {
                    96.. => 20_000,
                    80..=95 => 19_000,
                    64..=79 => 18_000,
                    48..=63 => 13_000,
                    32..=47 => 10_000,
                    _ => 7_000,
                };
                BandwidthCode::nearest_hz(hz)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialnorm_range() {
        assert!(Dialnorm::new(0).is_err());
        assert!(Dialnorm::new(32).is_err());
        assert_eq!(Dialnorm::new(31).unwrap().code(), 31);
        assert_eq!(Dialnorm::default().code(), 24);
    }

    #[test]
    fn bandwidth_codes() {
        assert!(BandwidthCode::new(61).is_err());
        assert_eq!(BandwidthCode::new(60).unwrap().end_mant(), 253);
        assert_eq!(BandwidthCode::new(0).unwrap().end_mant(), 73);
        assert_eq!(BandwidthCode::nearest_hz(20_000).end_mant(), 214);
        assert_eq!(BandwidthCode::nearest_hz(1_000).code(), 0);
        assert_eq!(BandwidthCode::nearest_hz(30_000).code(), 60);
    }

    #[test]
    fn bit_rates() {
        assert_eq!(BitRate::from_kbps(192), Some(BitRate::Kbps192));
        assert_eq!(BitRate::from_kbps(193), None);
        assert_eq!(BitRate::Kbps192.frame_bytes(), 768);
        assert_eq!(BitRate::Kbps192.frmsizecod(), 20);
        assert_eq!(BitRate::Kbps448.five_eighths_bytes(), 1120);
        for r in BitRate::ALL {
            assert_eq!(BitRate::from_kbps(r.kbps()), Some(r));
        }
    }

    #[test]
    fn bsmod_seven_depends_on_acmod() {
        let vo = ConfigBuilder {
            bitstream_mode: BitstreamMode::VoiceOver,
            ..ConfigBuilder::new(AudioCodingMode::STEREO, false, BitRate::Kbps192)
        };
        assert!(matches!(vo.build(), Err(ConfigError::BsmodForAcmod { .. })));
        let vo_mono = ConfigBuilder {
            coding_mode: AudioCodingMode::Mono,
            ..vo
        };
        assert!(vo_mono.build().is_ok());
        let karaoke_mono = ConfigBuilder {
            bitstream_mode: BitstreamMode::Karaoke,
            ..vo_mono
        };
        assert!(karaoke_mono.build().is_err());
    }

    #[test]
    fn rate_limits() {
        assert!(matches!(
            Config::new(AudioCodingMode::STEREO, false, BitRate::Kbps512),
            Err(ConfigError::AboveAtscLimit { kbps: 512 })
        ));
        // 5.1 at 32 kbit/s with full bandwidth cannot fit block 0's exponents before 5/8.
        let starved = ConfigBuilder {
            bandwidth: Bandwidth::Fixed(BandwidthCode::new(60).unwrap()),
            ..ConfigBuilder::new(AudioCodingMode::THREE_TWO, true, BitRate::Kbps32)
        };
        assert!(matches!(
            starved.build(),
            Err(ConfigError::RateTooLow { .. })
        ));
        // With the automatic bandwidth every rate works for every layout.
        for r in BitRate::ALL.into_iter().filter(|r| *r <= BitRate::Kbps448) {
            for m in [
                AudioCodingMode::Mono,
                AudioCodingMode::STEREO,
                AudioCodingMode::THREE_TWO,
            ] {
                for lfe in [false, true] {
                    Config::new(m, lfe, r).unwrap_or_else(|e| panic!("{m:?} {lfe} {r:?}: {e}"));
                }
            }
        }
    }
}
