//! The descriptors we emit, each from its syntax table (citations per type). Every descriptor
//! is written as tag, length, body; a body over 255 bytes is an error.

use tb_ts::Pid;

use crate::Error;
use crate::rating::UsRating;
use crate::text::{Lang, Mss};

/// A descriptor to write.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Descriptor {
    /// `registration_descriptor` (H.222.0 Table 2-51, p.74): tag 0x05 and a format identifier
    /// such as `GA94` or `AC-3`.
    Registration([u8; 4]),
    /// `data_stream_alignment_descriptor` (tag 0x06) with alignment_type 0x02 (access unit),
    /// required for video on ATSC (A/53-3 §5.4.1, p.12).
    VideoAlignment,
    /// `ISO_639_language_descriptor` (H.222.0 Table 2-64, tag 0x0A) for one language with an
    /// `audio_type` (0x00 undefined, 0x03 visual impaired commentary; Table 2-60, p.78).
    Iso639 {
        /// Language.
        lang: Lang,
        /// audio_type.
        audio_type: u8,
    },
    /// AC-3 audio descriptor (A/52 Annex A Table A4.1, p.116), tag 0x81.
    Ac3(Ac3Descriptor),
    /// `caption_service_descriptor` (A/65 §6.9.2 Table 6.26, p.69), tag 0x86.
    CaptionService(Vec<CaptionService>),
    /// `content_advisory_descriptor` (A/65 §6.9.3 Table 6.27, p.70), tag 0x87, for region 0x01
    /// with no description text (CTA-766-D §4.1, p.6).
    ContentAdvisory(UsRating),
    /// `extended_channel_name_descriptor` (A/65 §6.9.4 Table 6.28, p.71), tag 0xA0.
    ExtendedChannelName(Mss),
    /// `service_location_descriptor` (A/65 §6.9.5 Table 6.29, p.72), tag 0xA1.
    ServiceLocation {
        /// PCR_PID of the program.
        pcr_pid: Pid,
        /// (stream_type, elementary_PID, language or none).
        elements: Vec<(u8, Pid, Option<Lang>)>,
    },
    /// `component_name_descriptor` (A/65 §6.9.7 Table 6.32, p.74), tag 0xA3.
    ComponentName(Mss),
    /// `AVC_video_descriptor` (H.222.0 Table 2-89, p.96), tag 0x28.
    AvcVideo {
        /// profile_idc.
        profile_idc: u8,
        /// constraint_set0..5 flags (6 bits) and AVC_compatible_flags (2 bits), as one byte.
        constraint_flags: u8,
        /// level_idc.
        level_idc: u8,
    },
}

/// The fields of an AC-3 audio descriptor that vary (A/52 Table A4.1; values for us in
/// `notes/standards-digest.md` §4.3–4.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ac3Descriptor {
    /// Index 0..=18 of the exact bit rate (A/52 Table A4.3: 32, 40, 48, 56, 64, 80, 96, 112,
    /// 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640 kbit/s). ATSC allows at most
    /// index 15, 448 kbit/s (A/53-3 §5.8.1.1).
    pub bit_rate_index: u8,
    /// surround_mode (dsurmod), usually 0.
    pub surround_mode: u8,
    /// bsmod (0 complete main, 2 visually impaired, ...).
    pub bsmod: u8,
    /// acmod (1..=7; 0, dual mono, is prohibited on cable).
    pub acmod: u8,
    /// mainid, for bsmod < 2.
    pub mainid: u8,
    /// priority (1 primary, 2 other, 3 unspecified), for bsmod < 2.
    pub priority: u8,
    /// asvcflags, for bsmod >= 2.
    pub asvcflags: u8,
    /// The language, always sent on cable.
    pub lang: Lang,
}

/// One caption service of a caption service descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaptionService {
    /// Language.
    pub lang: Lang,
    /// Which kind of service.
    pub kind: CaptionKind,
    /// easy_reader.
    pub easy_reader: bool,
    /// wide_aspect_ratio: 16:9 display intended.
    pub wide: bool,
}

/// A CEA-608 line-21 service or a CEA-708 service number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptionKind {
    /// digital_cc 0 with line21_field (receivers ignore it; 0 is the usual value).
    Line21 {
        /// line21_field.
        field2: bool,
    },
    /// digital_cc 1 with caption_service_number 1..=63.
    Digital(ServiceNumber),
}

/// A CEA-708 caption service number, 1..=63 (0 is prohibited; A/65 §6.9.2, p.69).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceNumber(u8);

impl ServiceNumber {
    /// `n` if in 1..=63.
    pub const fn new(n: u8) -> Option<Self> {
        if n >= 1 && n <= 63 {
            Some(Self(n))
        } else {
            None
        }
    }
    /// The number.
    pub const fn get(self) -> u8 {
        self.0
    }
}

fn pid13(top3: u8, p: Pid) -> [u8; 2] {
    let v = p.get();
    [(top3 << 5) | ((v >> 8) as u8 & 0x1F), v as u8]
}

impl Descriptor {
    /// The descriptor's tag.
    pub fn tag(&self) -> u8 {
        match self {
            Descriptor::Registration(_) => 0x05,
            Descriptor::VideoAlignment => 0x06,
            Descriptor::Iso639 { .. } => 0x0A,
            Descriptor::AvcVideo { .. } => 0x28,
            Descriptor::Ac3(_) => 0x81,
            Descriptor::CaptionService(_) => 0x86,
            Descriptor::ContentAdvisory(_) => 0x87,
            Descriptor::ExtendedChannelName(_) => 0xA0,
            Descriptor::ServiceLocation { .. } => 0xA1,
            Descriptor::ComponentName(_) => 0xA3,
        }
    }

    /// Appends tag, length and body to `out`.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let mut b: Vec<u8> = Vec::new();
        match self {
            Descriptor::Registration(id) => b.extend_from_slice(id),
            Descriptor::VideoAlignment => b.push(0x02),
            Descriptor::Iso639 { lang, audio_type } => {
                b.extend_from_slice(&lang.bytes());
                b.push(*audio_type);
            }
            Descriptor::AvcVideo {
                profile_idc,
                constraint_flags,
                level_idc,
            } => {
                // AVC_still_present 0, AVC_24_hour_picture_flag 0,
                // Frame_Packing_SEI_not_present_flag 1, reserved '11111'.
                b.extend_from_slice(&[*profile_idc, *constraint_flags, *level_idc, 0x3F]);
            }
            Descriptor::Ac3(a) => {
                if a.bit_rate_index > 18 || a.acmod == 0 || a.acmod > 7 || a.bsmod > 7 {
                    return Err(Error::Invalid("AC-3 descriptor field out of range"));
                }
                // sample_rate_code '000' (48 kHz), bsid 8 (A/52 Table A4.2; digest §4.4).
                b.push(0x08);
                // bit_rate_code: MSB 0 = exact rate (Table A4.3); surround_mode.
                b.push((a.bit_rate_index << 2) | (a.surround_mode & 3));
                // bsmod, num_channels (MSB 0, acmod), full_svc 1.
                b.push((a.bsmod << 5) | ((a.acmod & 7) << 1) | 1);
                // langcod: reserved, 0xFF on cable (SCTE 54 §7.9.3.1 item 3).
                b.push(0xFF);
                if a.bsmod < 2 {
                    b.push(((a.mainid & 7) << 5) | ((a.priority & 3) << 3) | 0x07);
                } else {
                    b.push(a.asvcflags);
                }
                // textlen 0, text_code 1 (Latin-1).
                b.push(0x01);
                // language_flag 1, language_flag_2 0, reserved '111111'.
                b.push(0xBF);
                b.extend_from_slice(&a.lang.bytes());
            }
            Descriptor::CaptionService(svcs) => {
                if svcs.is_empty() || svcs.len() > 16 {
                    return Err(Error::Invalid("number_of_services must be 1..=16"));
                }
                b.push(0xE0 | svcs.len() as u8);
                for s in svcs {
                    b.extend_from_slice(&s.lang.bytes());
                    let (digital, low6) = match s.kind {
                        CaptionKind::Line21 { field2 } => (0u8, 0x3E | u8::from(field2)),
                        CaptionKind::Digital(n) => (1u8, n.get()),
                    };
                    // digital_cc, reserved '1', then line21 bits or caption_service_number.
                    b.push((digital << 7) | 0x40 | low6);
                    // easy_reader, wide_aspect_ratio, 14 reserved bits of 1.
                    b.push((u8::from(s.easy_reader) << 7) | (u8::from(s.wide) << 6) | 0x3F);
                    b.push(0xFF);
                }
            }
            Descriptor::ContentAdvisory(r) => {
                let dims = r.dimensions();
                // reserved '11', rating_region_count 1.
                b.push(0xC1);
                b.push(0x01); // rating_region: US
                b.push(dims.len() as u8);
                for (d, v) in dims {
                    b.push(d);
                    b.push(0xF0 | (v & 0x0F));
                }
                b.push(0); // rating_description_length 0 (CTA-766-D §4.1)
            }
            Descriptor::ExtendedChannelName(m) | Descriptor::ComponentName(m) => m.write(&mut b)?,
            Descriptor::ServiceLocation { pcr_pid, elements } => {
                b.extend_from_slice(&pid13(0b111, *pcr_pid));
                b.push(
                    u8::try_from(elements.len()).map_err(|_| Error::TooLong("number_elements"))?,
                );
                for (st, pid, lang) in elements {
                    b.push(*st);
                    b.extend_from_slice(&pid13(0b111, *pid));
                    b.extend_from_slice(&lang.map_or([0; 3], Lang::bytes));
                }
            }
        }
        let len = u8::try_from(b.len()).map_err(|_| Error::TooLong("descriptor"))?;
        out.push(self.tag());
        out.push(len);
        out.extend_from_slice(&b);
        Ok(())
    }
}

/// The bytes of a descriptor loop.
pub fn write_all(ds: &[Descriptor]) -> Result<Vec<u8>, Error> {
    let mut v = Vec::new();
    for d in ds {
        d.write(&mut v)?;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rating::{Dlsv, UsRating};

    fn bytes(d: Descriptor) -> Vec<u8> {
        let mut v = Vec::new();
        d.write(&mut v).unwrap();
        v
    }

    #[test]
    fn ac3_full_form() {
        let d = Descriptor::Ac3(Ac3Descriptor {
            bit_rate_index: 10, // 192 kbit/s
            surround_mode: 0,
            bsmod: 0,
            acmod: 2,
            mainid: 0,
            priority: 1,
            asvcflags: 0,
            lang: Lang::ENG,
        });
        assert_eq!(
            bytes(d),
            vec![
                0x81, 10, 0x08, 0x28, 0x05, 0xFF, 0x0F, 0x01, 0xBF, b'e', b'n', b'g'
            ]
        );
    }

    #[test]
    fn caption_service_two_entries() {
        let d = Descriptor::CaptionService(vec![
            CaptionService {
                lang: Lang::ENG,
                kind: CaptionKind::Line21 { field2: false },
                easy_reader: false,
                wide: true,
            },
            CaptionService {
                lang: Lang::ENG,
                kind: CaptionKind::Digital(ServiceNumber::new(1).unwrap()),
                easy_reader: false,
                wide: true,
            },
        ]);
        assert_eq!(
            bytes(d),
            vec![
                0x86, 13, 0xE2, b'e', b'n', b'g', 0x7E, 0x7F, 0xFF, b'e', b'n', b'g', 0xC1, 0x7F,
                0xFF
            ]
        );
        assert!(ServiceNumber::new(0).is_none());
        assert!(ServiceNumber::new(64).is_none());
    }

    #[test]
    fn content_advisory_tv14_lv() {
        let r = UsRating::Tv14(Dlsv {
            l: true,
            v: true,
            ..Dlsv::default()
        });
        assert_eq!(
            bytes(Descriptor::ContentAdvisory(r)),
            vec![0x87, 10, 0xC1, 0x01, 3, 0, 0xF4, 2, 0xF1, 4, 0xF1, 0]
        );
    }

    #[test]
    fn service_location() {
        let d = Descriptor::ServiceLocation {
            pcr_pid: Pid::new(0x31).unwrap(),
            elements: vec![
                (0x02, Pid::new(0x31).unwrap(), None),
                (0x81, Pid::new(0x34).unwrap(), Some(Lang::ENG)),
            ],
        };
        assert_eq!(
            bytes(d),
            vec![
                0xA1, 15, 0xE0, 0x31, 2, 0x02, 0xE0, 0x31, 0, 0, 0, 0x81, 0xE0, 0x34, b'e', b'n',
                b'g'
            ]
        );
    }
}
