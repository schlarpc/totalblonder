//! One function per table. Each returns complete sections (CRC included) for a given version;
//! [`crate::version`] decides the versions.

use tb_ts::Pid;
use tb_ts::section::{Kind, Long, Section, TableId, Version};

use crate::Error;
use crate::descriptor::{self, Descriptor};
use crate::gps::{DaylightSaving, GpsSeconds};
use crate::lineup::{Carrier, EtmLocation, Program, SourceId, audio_lang};
use crate::text::Mss;

fn pid13(top3: u8, p: Pid) -> [u8; 2] {
    let v = p.get();
    [(top3 << 5) | ((v >> 8) as u8 & 0x1F), v as u8]
}

fn len12(top4: u8, n: usize, what: &'static str) -> Result<[u8; 2], Error> {
    if n > 0x0FFF {
        return Err(Error::TooLong(what));
    }
    Ok([(top4 << 4) | ((n >> 8) as u8 & 0x0F), n as u8])
}

fn len10(n: usize, what: &'static str) -> Result<[u8; 2], Error> {
    if n > 0x03FF {
        return Err(Error::TooLong(what));
    }
    // reserved '111111' above 10 bits of length.
    Ok([0xFC | ((n >> 8) as u8 & 0x03), n as u8])
}

fn one(table_id: TableId, extension: u16, version: Version) -> Long {
    Long {
        table_id,
        extension,
        version,
        current: true,
        number: 0,
        last: 0,
    }
}

/// The PAT (Rec. Table 2-30, p.47): `transport_stream_id` and one entry per program. No
/// network PID entry (A/65 footnote 7).
pub fn pat(c: &Carrier, version: Version) -> Result<Section, Error> {
    let mut b = Vec::new();
    for p in c.programs() {
        b.extend_from_slice(&p.number.get().to_be_bytes());
        b.extend_from_slice(&pid13(0b111, p.pmt_pid.pid()));
    }
    Ok(Section::long(
        Kind::Psi,
        one(TableId::PAT, c.tsid(), version),
        &b,
    )?)
}

/// The PMT of one program (Rec. Table 2-33, p.50) with the descriptors of
/// `notes/standards-digest.md` §4.2: `GA94` and the rating in the program loop; for video the
/// alignment descriptor, the caption service descriptor when captions are carried, and the AVC
/// descriptor for H.264; for each AC-3 stream the AC-3 descriptor, the `AC-3` registration, an
/// ISO 639 descriptor and the component name when one is set. The PCR PID is the video PID.
pub fn pmt(p: &Program, version: Version) -> Result<Section, Error> {
    let mut prog = vec![Descriptor::Registration(*b"GA94")];
    if let Some(r) = p.rating {
        prog.push(Descriptor::ContentAdvisory(r));
    }
    let prog = descriptor::write_all(&prog)?;
    let mut b = Vec::new();
    b.extend_from_slice(&pid13(0b111, p.video.pid.pid()));
    b.extend_from_slice(&len12(0xF, prog.len(), "program_info_length")?);
    b.extend_from_slice(&prog);

    let mut es = |st: u8, pid: Pid, ds: Vec<Descriptor>| -> Result<(), Error> {
        let d = descriptor::write_all(&ds)?;
        b.push(st);
        b.extend_from_slice(&pid13(0b111, pid));
        b.extend_from_slice(&len12(0xF, d.len(), "ES_info_length")?);
        b.extend_from_slice(&d);
        Ok(())
    };
    let mut vd = vec![Descriptor::VideoAlignment];
    if !p.video.captions.is_empty() {
        vd.push(Descriptor::CaptionService(p.video.captions.clone()));
    }
    if let crate::lineup::VideoCodec::H264 {
        profile_idc,
        constraint_flags,
        level_idc,
    } = p.video.codec
    {
        vd.push(Descriptor::AvcVideo {
            profile_idc,
            constraint_flags,
            level_idc,
        });
    }
    es(p.video.codec.stream_type(), p.video.pid.pid(), vd)?;
    for a in &p.audio {
        let mut ad = vec![
            Descriptor::Registration(*b"AC-3"),
            Descriptor::Ac3(a.ac3.clone()),
            Descriptor::Iso639 {
                lang: a.ac3.lang,
                audio_type: a.audio_type,
            },
        ];
        if let Some(n) = &a.component_name {
            ad.push(Descriptor::ComponentName(n.clone()));
        }
        es(0x81, a.pid.pid(), ad)?;
    }
    Ok(Section::long(
        Kind::Psi,
        one(TableId::PMT, p.number.get(), version),
        &b,
    )?)
}

/// The service location descriptor of a program, made from the same structure as its PMT
/// so that the two always agree (A/65 §6.9.5, p.72–73).
pub fn service_location(p: &Program) -> Descriptor {
    let mut elements = vec![(p.video.codec.stream_type(), p.video.pid.pid(), None)];
    elements.extend(
        p.audio
            .iter()
            .map(|a| (0x81, a.pid.pid(), Some(audio_lang(a)))),
    );
    Descriptor::ServiceLocation {
        pcr_pid: p.video.pid.pid(),
        elements,
    }
}

fn cvct_channel(c: &Carrier, p: &Program, with_location: bool) -> Result<Vec<u8>, Error> {
    let ch = &p.channel;
    let mut b = Vec::with_capacity(64);
    b.extend_from_slice(&ch.short_name.field());
    let (maj, min) = (u32::from(ch.number.major()), u32::from(ch.number.minor()));
    // reserved '1111', major (10), minor (10), modulation_mode (8): 0x03, SCTE mode 2, 256-QAM.
    let w = (0xFu32 << 28) | (maj << 18) | (min << 8) | 0x03;
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes()); // carrier_frequency: zero (A/65 §6.3.1)
    b.extend_from_slice(&c.tsid().to_be_bytes()); // channel_TSID
    b.extend_from_slice(&p.number.get().to_be_bytes());
    // ETM_location (2), access_controlled 0, hidden, path_select 0, out_of_band 0, hide_guide,
    // reserved '111', service_type (6).
    let flags: u16 = (u16::from(ch.etm.code()) << 14)
        | (u16::from(ch.hidden) << 12)
        | (u16::from(ch.hide_guide) << 9)
        | (0b111 << 6)
        | u16::from(ch.service_type.code());
    b.extend_from_slice(&flags.to_be_bytes());
    b.extend_from_slice(&ch.source_id.get().to_be_bytes());
    let mut ds = Vec::new();
    if let Some(n) = &ch.long_name {
        ds.push(Descriptor::ExtendedChannelName(n.clone()));
    }
    if with_location {
        ds.push(service_location(p));
    }
    let d = descriptor::write_all(&ds)?;
    b.extend_from_slice(&len10(d.len(), "descriptors_length")?);
    b.extend_from_slice(&d);
    Ok(b)
}

/// The CVCT (A/65 §6.3.2 Table 6.8, p.36) of the carrier's own programs (SPEC §7.1), in as
/// many sections as needed (at most 1,024 bytes each; a channel never spans sections).
/// `with_location` adds service location descriptors, optional on cable.
pub fn cvct(c: &Carrier, version: Version, with_location: bool) -> Result<Vec<Section>, Error> {
    // Header 8 + protocol_version 1 + num_channels 1, trailer 2 + CRC 4.
    const FIXED: usize = 8 + 2 + 2 + 4;
    let mut groups: Vec<Vec<Vec<u8>>> = vec![Vec::new()];
    let mut used = FIXED;
    for p in c.programs() {
        let ch = cvct_channel(c, p, with_location)?;
        if ch.len() + FIXED > 1024 {
            return Err(Error::TooLong("CVCT channel record"));
        }
        let fits = used + ch.len() <= 1024 && groups.last().is_some_and(|g| g.len() < 255);
        if !fits {
            groups.push(Vec::new());
            used = FIXED;
        }
        used += ch.len();
        if let Some(g) = groups.last_mut() {
            g.push(ch);
        }
    }
    let last = u8::try_from(groups.len() - 1).map_err(|_| Error::TooLong("CVCT sections"))?;
    let mut out = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let mut b = vec![0u8, g.len() as u8]; // protocol_version, num_channels_in_section
        for ch in g {
            b.extend_from_slice(ch);
        }
        b.extend_from_slice(&len10(0, "additional_descriptors_length")?);
        let h = Long {
            number: i as u8,
            last,
            ..one(TableId::CVCT, c.tsid(), version)
        };
        out.push(Section::long(Kind::Private1024, h, &b)?);
    }
    Ok(out)
}

/// A `table_type` of the MGT (A/65 Table 6.3, p.27).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TableType {
    /// 0x0002: the current CVCT.
    CvctCurrent,
    /// 0x0004: the channel ETT.
    ChannelEtt,
    /// 0x0100 + k: EIT-k, k < 128.
    Eit(u8),
    /// 0x0200 + k: event ETT-k, k < 128.
    EventEtt(u8),
}

impl TableType {
    /// The 16-bit code, or `None` for k ≥ 128.
    pub fn code(self) -> Option<u16> {
        match self {
            TableType::CvctCurrent => Some(0x0002),
            TableType::ChannelEtt => Some(0x0004),
            TableType::Eit(k) if k < 128 => Some(0x0100 + u16::from(k)),
            TableType::EventEtt(k) if k < 128 => Some(0x0200 + u16::from(k)),
            _ => None,
        }
    }
}

/// One MGT entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MgtEntry {
    /// What the table is.
    pub table_type: TableType,
    /// Where it is.
    pub pid: Pid,
    /// Its version (equal to the version in its sections).
    pub version: Version,
    /// Total bytes of all its sections (A/65 §6.2: number_bytes).
    pub number_bytes: u32,
}

/// The MGT (A/65 §6.2 Table 6.2, p.26), one section. The STT is never listed.
pub fn mgt(entries: &[MgtEntry], version: Version) -> Result<Section, Error> {
    let n = u16::try_from(entries.len()).map_err(|_| Error::TooLong("tables_defined"))?;
    let mut b = vec![0u8]; // protocol_version
    b.extend_from_slice(&n.to_be_bytes());
    for e in entries {
        let tt = e
            .table_type
            .code()
            .ok_or(Error::Invalid("table type index"))?;
        b.extend_from_slice(&tt.to_be_bytes());
        b.extend_from_slice(&pid13(0b111, e.pid));
        b.push(0xE0 | e.version.get());
        b.extend_from_slice(&e.number_bytes.to_be_bytes());
        b.extend_from_slice(&len12(0xF, 0, "table_type_descriptors_length")?);
    }
    b.extend_from_slice(&len12(0xF, 0, "descriptors_length")?);
    Ok(Section::long(
        Kind::Private,
        one(TableId::MGT, 0, version),
        &b,
    )?)
}

/// The STT (A/65 §6.1 Table 6.1, p.24): extension 0, version 0, no descriptors.
pub fn stt(time: GpsSeconds, gps_utc_offset: u8, ds: DaylightSaving) -> Result<Section, Error> {
    let mut b = vec![0u8]; // protocol_version
    b.extend_from_slice(&time.0.to_be_bytes());
    b.push(gps_utc_offset);
    b.extend_from_slice(&ds.bytes());
    Ok(Section::long(
        Kind::Private1024,
        one(TableId::STT, 0, Version::new(0)),
        &b,
    )?)
}

/// A 14-bit `event_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(u16);

impl EventId {
    /// `v` if below 2^14.
    pub const fn new(v: u16) -> Option<Self> {
        if v < 0x4000 { Some(Self(v)) } else { None }
    }
    /// The low 14 bits of `v`.
    pub const fn wrapping(v: u32) -> Self {
        Self((v & 0x3FFF) as u16)
    }
    /// The value.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// One event of an EIT instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EitEvent {
    /// event_id, stable for the event's life.
    pub id: EventId,
    /// start_time.
    pub start: GpsSeconds,
    /// length_in_seconds (below 2^20).
    pub length: u32,
    /// Where its ETM is.
    pub etm: EtmLocation,
    /// title_text.
    pub title: Mss,
    /// Event descriptors: AC-3, caption service, content advisory (A/65 Table 6.25b).
    pub descriptors: Vec<Descriptor>,
}

fn eit_event(e: &EitEvent) -> Result<Vec<u8>, Error> {
    if e.length >= 1 << 20 {
        return Err(Error::Invalid("length_in_seconds"));
    }
    let mut b = Vec::new();
    b.extend_from_slice(&(0xC000 | e.id.get()).to_be_bytes());
    b.extend_from_slice(&e.start.0.to_be_bytes());
    // reserved '11', ETM_location (2), length_in_seconds (20).
    let w = (0b11u32 << 22) | (u32::from(e.etm.code()) << 20) | e.length;
    b.extend_from_slice(&w.to_be_bytes()[1..]);
    let t = e.title.to_bytes()?;
    b.push(u8::try_from(t.len()).map_err(|_| Error::TooLong("title_length"))?);
    b.extend_from_slice(&t);
    let d = descriptor::write_all(&e.descriptors)?;
    b.extend_from_slice(&len12(0xF, d.len(), "descriptors_length")?);
    b.extend_from_slice(&d);
    Ok(b)
}

/// The EIT instance of one source in one window (A/65 §6.5 Table 6.11, p.40–43): events in
/// start order, not overlapping, never split across sections; an instance with no events is
/// one section with `num_events_in_section` 0 (p.40).
pub fn eit(source: SourceId, version: Version, events: &[EitEvent]) -> Result<Vec<Section>, Error> {
    for w in events.windows(2) {
        if u64::from(w[1].start.0) < u64::from(w[0].start.0) + u64::from(w[0].length) {
            return Err(Error::Invalid("EIT events overlap or are out of order"));
        }
    }
    // Header 8 + protocol_version + num_events + CRC 4.
    const FIXED: usize = 8 + 2 + 4;
    let mut groups: Vec<Vec<Vec<u8>>> = vec![Vec::new()];
    let mut used = FIXED;
    for e in events {
        let r = eit_event(e)?;
        if r.len() + FIXED > 4096 {
            return Err(Error::TooLong("EIT event"));
        }
        if used + r.len() > 4096 || groups.last().is_some_and(|g| g.len() == 255) {
            groups.push(Vec::new());
            used = FIXED;
        }
        used += r.len();
        if let Some(g) = groups.last_mut() {
            g.push(r);
        }
    }
    let last = u8::try_from(groups.len() - 1).map_err(|_| Error::TooLong("EIT sections"))?;
    let mut out = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let mut b = vec![0u8, g.len() as u8];
        for r in g {
            b.extend_from_slice(r);
        }
        let h = Long {
            number: i as u8,
            last,
            ..one(TableId::EIT, source.get(), version)
        };
        out.push(Section::long(Kind::Private, h, &b)?);
    }
    Ok(out)
}

/// An `ETM_id` (A/65 Table 6.14, p.45).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EtmId {
    /// A channel's ETM: `source_id << 16`.
    Channel(SourceId),
    /// An event's ETM: `source_id << 16 | event_id << 2 | 0b10`.
    Event(SourceId, EventId),
}

impl EtmId {
    /// The 32-bit value.
    pub fn value(self) -> u32 {
        match self {
            EtmId::Channel(s) => u32::from(s.get()) << 16,
            EtmId::Event(s, e) => (u32::from(s.get()) << 16) | (u32::from(e.get()) << 2) | 0b10,
        }
    }
}

/// One ETT instance (A/65 §6.6 Table 6.13, p.44): one section.
pub fn ett(extension: u16, version: Version, id: EtmId, text: &Mss) -> Result<Section, Error> {
    let mut b = vec![0u8];
    b.extend_from_slice(&id.value().to_be_bytes());
    text.write(&mut b)?;
    Ok(Section::long(
        Kind::Private,
        one(TableId::ETT, extension, version),
        &b,
    )?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::descriptor::{Ac3Descriptor, CaptionKind, CaptionService, ServiceNumber};
    use crate::lineup::*;
    use crate::text::{Lang, ShortName};
    use tb_ts::section::Parsed;

    pub(crate) fn program(n: u16) -> Program {
        let base = 0x30 + 0x10 * (n - 1);
        Program {
            number: ProgramNumber::new(n).unwrap(),
            pmt_pid: EsPid::new(base).unwrap(),
            video: Video {
                pid: EsPid::new(base + 1).unwrap(),
                codec: VideoCodec::Mpeg2,
                captions: vec![CaptionService {
                    lang: Lang::ENG,
                    kind: CaptionKind::Digital(ServiceNumber::new(1).unwrap()),
                    easy_reader: false,
                    wide: true,
                }],
            },
            audio: vec![Audio {
                pid: EsPid::new(base + 4).unwrap(),
                ac3: Ac3Descriptor {
                    bit_rate_index: 10,
                    surround_mode: 0,
                    bsmod: 0,
                    acmod: 2,
                    mainid: 0,
                    priority: 1,
                    asvcflags: 0,
                    lang: Lang::ENG,
                },
                audio_type: 0,
                component_name: None,
            }],
            rating: None,
            channel: Channel {
                number: ChannelNumber::new(20, n).unwrap(),
                short_name: ShortName::new(&format!("TB{n}")).unwrap(),
                long_name: Some(Mss::eng("Totalblonder")),
                source_id: SourceId::new(n).unwrap(),
                service_type: ServiceType::DigitalTv,
                hidden: false,
                hide_guide: false,
                etm: EtmLocation::None,
            },
        }
    }

    pub(crate) fn carrier() -> Carrier {
        Carrier::new(0x0101, (1..=6).map(program).collect()).unwrap()
    }

    #[test]
    fn pat_lists_programs() {
        let s = pat(&carrier(), Version::new(1)).unwrap();
        let p = Parsed::parse(s.bytes()).unwrap();
        assert_eq!(p.header.extension, 0x0101);
        assert!(!p.private);
        assert_eq!(p.body.len(), 24);
        assert_eq!(&p.body[..4], &[0x00, 0x01, 0xE0, 0x30]);
    }

    #[test]
    fn pmt_fields() {
        let s = pmt(&program(2), Version::new(0)).unwrap();
        let p = Parsed::parse(s.bytes()).unwrap();
        assert_eq!(p.header.extension, 2);
        // PCR_PID = video 0x41.
        assert_eq!(&p.body[..2], &[0xE0, 0x41]);
        let pil = usize::from(u16::from_be_bytes([p.body[2] & 0x0F, p.body[3]]));
        assert_eq!(&p.body[4..4 + pil], &[0x05, 4, b'G', b'A', b'9', b'4']);
        let es = &p.body[4 + pil..];
        assert_eq!(es[0], 0x02);
        assert_eq!(&es[1..3], &[0xE0, 0x41]);
    }

    #[test]
    fn cvct_layout() {
        let secs = cvct(&carrier(), Version::new(4), true).unwrap();
        assert_eq!(secs.len(), 1);
        let p = Parsed::parse(secs[0].bytes()).unwrap();
        assert_eq!(p.header.extension, 0x0101);
        assert_eq!((p.body[0], p.body[1]), (0, 6));
        let ch = &p.body[2..];
        assert_eq!(&ch[..6], &[0, b'T', 0, b'B', 0, b'1']);
        // major 20, minor 1, modulation 0x03.
        let w = u32::from_be_bytes([ch[14], ch[15], ch[16], ch[17]]);
        assert_eq!(
            (w >> 28, (w >> 18) & 0x3FF, (w >> 8) & 0x3FF, w & 0xFF),
            (0xF, 20, 1, 3)
        );
        assert_eq!(u16::from_be_bytes([ch[22], ch[23]]), 0x0101); // channel_TSID
        assert_eq!(u16::from_be_bytes([ch[24], ch[25]]), 1); // program_number
        assert_eq!(ch[27] & 0x3F, 0x02); // service_type
        assert_eq!(u16::from_be_bytes([ch[28], ch[29]]), 1); // source_id
    }

    #[test]
    fn lineup_rules() {
        let mut ps: Vec<Program> = (1..=2).map(program).collect();
        ps[1].channel.number = ps[0].channel.number;
        assert!(Carrier::new(1, ps).is_err());
        let mut ps: Vec<Program> = (1..=2).map(program).collect();
        ps[1].video.pid = ps[0].audio[0].pid;
        assert!(Carrier::new(1, ps).is_err());
        assert!(EsPid::new(0x20).is_err());
        assert!(EsPid::new(0x1FF0).is_err());
        let mut p = program(1);
        p.audio.push(p.audio[0].clone());
        p.audio[1].pid = EsPid::new(0x35).unwrap();
        p.audio[1].ac3.priority = 2;
        assert!(
            Carrier::new(1, vec![p.clone()]).is_err(),
            "same language and bsmod need names"
        );
        p.audio[1].component_name = Some(Mss::eng("Commentary"));
        p.audio[0].component_name = Some(Mss::eng("Main"));
        assert!(Carrier::new(1, vec![p]).is_ok());
    }

    #[test]
    fn eit_rules() {
        let src = SourceId::new(1).unwrap();
        let ev = |id: u16, start: u32, len: u32| EitEvent {
            id: EventId::new(id).unwrap(),
            start: GpsSeconds(start),
            length: len,
            etm: EtmLocation::None,
            title: Mss::eng("Show"),
            descriptors: vec![],
        };
        let s = eit(src, Version::new(0), &[]).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(Parsed::parse(s[0].bytes()).unwrap().body, &[0, 0]);
        assert!(eit(src, Version::new(0), &[ev(1, 100, 50), ev(2, 120, 10)]).is_err());
        let s = eit(src, Version::new(0), &[ev(1, 100, 50), ev(2, 150, 10)]).unwrap();
        let p = Parsed::parse(s[0].bytes()).unwrap();
        assert_eq!(p.header.extension, 1);
        assert_eq!(p.body[1], 2);
        // Many events split into sections of at most 4096 bytes.
        let many: Vec<EitEvent> = (0..400).map(|i| ev(i, u32::from(i) * 60, 60)).collect();
        let s = eit(src, Version::new(0), &many).unwrap();
        assert!(s.len() > 1);
        assert!(s.iter().all(|x| x.len() <= 4096));
    }

    #[test]
    fn etm_ids() {
        let s = SourceId::new(3).unwrap();
        assert_eq!(EtmId::Channel(s).value(), 0x0003_0000);
        assert_eq!(
            EtmId::Event(s, EventId::new(5).unwrap()).value(),
            0x0003_0016
        );
    }

    #[test]
    fn stt_bytes() {
        let s = stt(GpsSeconds(0x5000_0000), 18, DaylightSaving::default()).unwrap();
        let p = Parsed::parse(s.bytes()).unwrap();
        assert_eq!(p.body, &[0, 0x50, 0, 0, 0, 18, 0x60, 0]);
        assert_eq!(s.table_id(), 0xCD);
    }
}
