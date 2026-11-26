//! The `caption_service_descriptor()` that announces a program's caption services in its
//! PMT (video elementary stream loop) and in the EIT events that have captions (ATSC A/65:2013
//! §6.9.2, Table 6.26, p.68-70; `notes/standards-digest.md` §3.7). It is the CAPTION CHANNEL
//! SERVICE DIRECTORY of CTA-708-E §4.5 (p.19-20). This crate does not build tables; the bytes
//! are here because the services they describe are made here.
//!
//! One entry per CEA-608 field used and per CEA-708 service, at most 16 (A/65:
//! `number_of_services` "in the range 1 to 16"; CTA-708-E §4.5: "a maximum of 16 separate
//! services"). The language is written for CEA-608 entries too, although "when the digital_cc
//! flag is set to '0', this field has no meaning" (the digest's recommendation: "eng" with
//! CC1).

use crate::cea608::Field;
use crate::cea708::ServiceNumber;
use crate::schedule::{Aspect, Track};

/// What a service is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    /// A CEA-608 data stream on line 21 field 1 or 2 (`digital_cc` 0; `line21_field` "is not
    /// expected to be used by receivers").
    Line21(Field),
    /// A CEA-708 service (`digital_cc` 1).
    Digital {
        /// Its number.
        service: ServiceNumber,
        /// Text for beginning readers.
        easy_reader: bool,
        /// Authored for 16:9.
        wide: bool,
    },
}

/// One entry of the descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionService {
    /// ISO 639-2/B language code, three Latin-1 letters (e.g. `*b"eng"`).
    pub language: [u8; 3],
    /// The service.
    pub kind: ServiceKind,
}

/// Why a descriptor cannot be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// None or more than 16 services.
    #[error("a caption_service_descriptor lists 1 to 16 services, not {0}")]
    Count(usize),
}

/// The descriptor: tag 0x86, length, the entries.
pub fn caption_service_descriptor(services: &[CaptionService]) -> Result<Vec<u8>, DescriptorError> {
    let n = services.len();
    if !(1..=16).contains(&n) {
        return Err(DescriptorError::Count(n));
    }
    let mut v = vec![0x86, (1 + 6 * n) as u8, 0xE0 | n as u8];
    for s in services {
        v.extend_from_slice(&s.language);
        match s.kind {
            ServiceKind::Line21(f) => {
                // digital_cc 0, reserved '1', reserved '11111', line21_field
                v.push(0x7E | u8::from(f == Field::Two));
                // easy_reader and wide_aspect_ratio have no meaning here; reserved '1's
                v.extend([0x3F, 0xFF]);
            }
            ServiceKind::Digital {
                service,
                easy_reader,
                wide,
            } => {
                v.push(0xC0 | service.get());
                v.push((u8::from(easy_reader) << 7) | (u8::from(wide) << 6) | 0x3F);
                v.push(0xFF);
            }
        }
    }
    Ok(v)
}

/// The entries of a captioner's tracks, track `i` in language `languages[i]` (`eng` when
/// none is given).
pub fn services_of(tracks: &[Track], languages: &[[u8; 3]]) -> Vec<CaptionService> {
    let mut out = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        let language = languages.get(i).copied().unwrap_or(*b"eng");
        if let Some(c) = t.cea608 {
            out.push(CaptionService {
                language,
                kind: ServiceKind::Line21(c.channel.field()),
            });
        }
        if let Some(c) = t.cea708 {
            out.push(CaptionService {
                language,
                kind: ServiceKind::Digital {
                    service: c.service,
                    easy_reader: false,
                    wide: c.aspect == Aspect::Wide,
                },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Presentation;

    #[test]
    fn english_and_spanish() {
        let tracks = [
            Track::primary(Presentation::PopOn),
            Track::secondary(Presentation::PopOn),
        ];
        let s = services_of(&tracks, &[*b"eng", *b"spa"]);
        let d = caption_service_descriptor(&s).unwrap();
        assert_eq!(
            d,
            [
                0x86, 25, 0xE4, //
                b'e', b'n', b'g', 0x7E, 0x3F, 0xFF, // CC1: field 1
                b'e', b'n', b'g', 0xC1, 0x7F, 0xFF, // service 1, 16:9
                b's', b'p', b'a', 0x7F, 0x3F, 0xFF, // CC3: field 2
                b's', b'p', b'a', 0xC2, 0x7F, 0xFF, // service 2, 16:9
            ]
        );
        assert_eq!(usize::from(d[1]), d.len() - 2);
        assert_eq!(
            caption_service_descriptor(&[]),
            Err(DescriptorError::Count(0))
        );
        assert_eq!(
            caption_service_descriptor(&[s[0]; 17]),
            Err(DescriptorError::Count(17))
        );
    }
}
