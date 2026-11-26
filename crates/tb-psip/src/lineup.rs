//! A carrier's programs, validated as a whole.
//!
//! The rules checked when a [`Carrier`] is made:
//! - PMT and elementary PIDs in 0x0030–0x1FEF (SCTE 54-2024 §7.9.4, p.23), all distinct;
//! - program numbers non-zero and distinct (Rec. §2.4.4.3; A/65 footnote 7);
//! - virtual channel numbers two-part, 1–999 each, distinct (A/65 §6.3.2, p.35; SCTE 54
//!   §7.8.1.1, p.17);
//! - source ids 0x0001–0x0FFF and distinct (A/65 §6.3.2, p.37);
//! - at most three audio streams per program (the modulator's MPTS limit, manual §4.2) and at
//!   most one stream labeled primary; audio streams that share language and bsmod carry a
//!   component name (A/65 §6.9.7, p.74).

use std::collections::HashSet;

use tb_ts::Pid;

use crate::Error;
use crate::descriptor::{Ac3Descriptor, CaptionService};
use crate::rating::UsRating;
use crate::text::{Lang, Mss, ShortName};

/// A `program_number`, never 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProgramNumber(u16);

impl ProgramNumber {
    /// `n` if non-zero.
    pub const fn new(n: u16) -> Option<Self> {
        if n == 0 { None } else { Some(Self(n)) }
    }
    /// The number.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A PID allowed for PMTs and elementary streams on cable: 0x0030–0x1FEF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EsPid(Pid);

impl EsPid {
    /// `v` if in range.
    pub fn new(v: u16) -> Result<Self, Error> {
        if (0x0030..=0x1FEF).contains(&v) {
            Pid::new(v).map(EsPid).map_err(|_| Error::Invalid("PID"))
        } else {
            Err(Error::Lineup(format!("PID {v:#x} outside 0x0030-0x1FEF")))
        }
    }
    /// The PID.
    pub const fn pid(self) -> Pid {
        self.0
    }
}

/// A `source_id` we allocate: 0x0001–0x0FFF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(u16);

impl SourceId {
    /// `v` if in range.
    pub const fn new(v: u16) -> Option<Self> {
        if v >= 1 && v <= 0x0FFF {
            Some(Self(v))
        } else {
            None
        }
    }
    /// The value.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// A two-part virtual channel number, `major.minor`, each 1–999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelNumber {
    major: u16,
    minor: u16,
}

impl ChannelNumber {
    /// `major.minor` if both are in 1..=999.
    pub const fn new(major: u16, minor: u16) -> Option<Self> {
        if major >= 1 && major <= 999 && minor >= 1 && minor <= 999 {
            Some(Self { major, minor })
        } else {
            None
        }
    }
    /// Parses `"20.1"` or `"20-1"`.
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once(['.', '-'])?;
        Self::new(a.trim().parse().ok()?, b.trim().parse().ok()?)
    }
    /// major_channel_number.
    pub const fn major(self) -> u16 {
        self.major
    }
    /// minor_channel_number.
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

impl std::fmt::Display for ChannelNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// `service_type` (A/53-3 §5.1.3, `notes/standards-digest.md` §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceType {
    /// 0x02: ATSC digital television (MPEG-2 or AVC video with AC-3).
    DigitalTv,
    /// 0x03: ATSC audio only.
    AudioOnly,
}

impl ServiceType {
    /// The 6-bit code.
    pub const fn code(self) -> u8 {
        match self {
            ServiceType::DigitalTv => 0x02,
            ServiceType::AudioOnly => 0x03,
        }
    }
}

/// The video codec and its PMT signaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    /// MPEG-2 video, stream_type 0x02.
    Mpeg2,
    /// H.264, stream_type 0x1B, with the AVC video descriptor's fields.
    H264 {
        /// profile_idc (100 High, 77 Main).
        profile_idc: u8,
        /// constraint_set flags and AVC_compatible_flags as in the SPS byte.
        constraint_flags: u8,
        /// level_idc (30, 40, 42).
        level_idc: u8,
    },
}

impl VideoCodec {
    /// The PMT stream_type.
    pub const fn stream_type(self) -> u8 {
        match self {
            VideoCodec::Mpeg2 => 0x02,
            VideoCodec::H264 { .. } => 0x1B,
        }
    }
}

/// The video elementary stream of a program. It is also the PCR PID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Video {
    /// The PID.
    pub pid: EsPid,
    /// The codec.
    pub codec: VideoCodec,
    /// Caption services in the picture user data (empty: no caption descriptor).
    pub captions: Vec<CaptionService>,
}

/// An AC-3 audio elementary stream (stream_type 0x81).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Audio {
    /// The PID.
    pub pid: EsPid,
    /// The AC-3 descriptor's contents; its language is also sent in an ISO 639 descriptor.
    pub ac3: Ac3Descriptor,
    /// ISO 639 audio_type (0x00, or 0x03 for described video).
    pub audio_type: u8,
    /// A component name, required when two tracks share language and bsmod.
    pub component_name: Option<Mss>,
}

/// Where the extended text of a channel or event is (A/65 Table 6.6, p.33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EtmLocation {
    /// No ETM.
    #[default]
    None,
    /// In this transport stream.
    Here,
}

impl EtmLocation {
    /// The 2-bit code.
    pub const fn code(self) -> u8 {
        match self {
            EtmLocation::None => 0,
            EtmLocation::Here => 1,
        }
    }
}

/// How a program appears in the channel map.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Channel {
    /// The number the viewer tunes.
    pub number: ChannelNumber,
    /// short_name.
    pub short_name: ShortName,
    /// extended_channel_name, if any.
    pub long_name: Option<Mss>,
    /// The service's stable identity.
    pub source_id: SourceId,
    /// service_type.
    pub service_type: ServiceType,
    /// Hidden from surfing and direct entry.
    pub hidden: bool,
    /// Hidden from the guide too (meaningful only when hidden).
    pub hide_guide: bool,
    /// Where the channel's own ETM is.
    pub etm: EtmLocation,
}

/// One program.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Program {
    /// program_number.
    pub number: ProgramNumber,
    /// The PMT's PID.
    pub pmt_pid: EsPid,
    /// Video, also the PCR PID.
    pub video: Video,
    /// Audio streams, at most three.
    pub audio: Vec<Audio>,
    /// The current event's rating, if rated (PMT program loop, SCTE 54 §7.9.3.11).
    pub rating: Option<UsRating>,
    /// The channel map entry.
    pub channel: Channel,
}

/// A carrier's transport stream: its id and programs. Valid by construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Carrier {
    tsid: u16,
    programs: Vec<Program>,
}

impl Carrier {
    /// Validates and builds the lineup.
    pub fn new(tsid: u16, programs: Vec<Program>) -> Result<Carrier, Error> {
        let mut pids = HashSet::new();
        let mut numbers = HashSet::new();
        let mut channels = HashSet::new();
        let mut sources = HashSet::new();
        let dup = |what: &str, v: String| Error::Lineup(format!("duplicate {what} {v}"));
        for p in &programs {
            if !numbers.insert(p.number) {
                return Err(dup("program_number", p.number.get().to_string()));
            }
            if !channels.insert(p.channel.number) {
                return Err(dup("channel", p.channel.number.to_string()));
            }
            if !sources.insert(p.channel.source_id) {
                return Err(dup("source_id", p.channel.source_id.get().to_string()));
            }
            for pid in std::iter::once(p.pmt_pid)
                .chain(std::iter::once(p.video.pid))
                .chain(p.audio.iter().map(|a| a.pid))
            {
                if !pids.insert(pid) {
                    return Err(dup("PID", format!("{:#x}", pid.pid().get())));
                }
            }
            if p.audio.len() > 3 {
                return Err(Error::Lineup(format!(
                    "program {} has more than 3 audio streams",
                    p.number.get()
                )));
            }
            if p.audio
                .iter()
                .filter(|a| a.ac3.bsmod < 2 && a.ac3.priority == 1)
                .count()
                > 1
            {
                return Err(Error::Lineup(format!(
                    "program {} has two primary audio streams",
                    p.number.get()
                )));
            }
            for (i, a) in p.audio.iter().enumerate() {
                if a.ac3.bit_rate_index > 15 {
                    return Err(Error::Lineup(
                        "AC-3 above 448 kbit/s (A/53-3 §5.8.1.1)".into(),
                    ));
                }
                let shared =
                    p.audio.iter().enumerate().any(|(j, b)| {
                        j != i && b.ac3.lang == a.ac3.lang && b.ac3.bsmod == a.ac3.bsmod
                    });
                if shared && a.component_name.is_none() {
                    return Err(Error::Lineup(format!(
                        "program {}: audio streams share language and bsmod without a component name",
                        p.number.get()
                    )));
                }
            }
        }
        Ok(Carrier { tsid, programs })
    }

    /// transport_stream_id.
    pub fn tsid(&self) -> u16 {
        self.tsid
    }

    /// The programs, in the order given.
    pub fn programs(&self) -> &[Program] {
        &self.programs
    }
}

/// Languages of the audio streams of a program, for the service location descriptor.
pub(crate) fn audio_lang(a: &Audio) -> Lang {
    a.ac3.lang
}
