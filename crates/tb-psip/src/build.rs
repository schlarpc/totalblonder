//! The complete table set of one carrier: PAT, PMTs, CVCT, EIT-0…EIT-(n−1), event ETTs and the
//! MGT that describes them (computed last, from the sections actually built, so its PIDs,
//! versions and byte counts are right by construction). The STT is not here: its content
//! depends on the moment it is sent, so the mux makes it ([`crate::tables::stt`]).

use tb_ts::Pid;
use tb_ts::section::{Section, TableId, Version};

use crate::Error;
use crate::descriptor::Descriptor;
use crate::gps::{GpsSeconds, LeapSeconds};
use crate::guide::{Schedule, Window, etms, events_in};
use crate::lineup::{Carrier, Program, ProgramNumber};
use crate::tables::{self, MgtEntry, TableType};
use crate::version::{Key, Versions};

type SectionsFn = Box<dyn Fn(Version) -> Result<Vec<Section>, Error>>;

/// Guide settings (SPEC §7.5, `[psip]` of the configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuideConfig {
    /// EIT windows sent, 1..=128 (the default 8 covers 24 hours). At least EIT-0 is always
    /// sent: an MGT listing the CVCT alone is an open question of the standard
    /// (`docs/notes/standards-digest.md` "Open questions" 2).
    pub windows: u8,
    /// Each ETT instance on a PID gets its own `ETT_table_id_extension` (1, 2, …) as A/65
    /// §6.6 requires; `false` sends 0 everywhere, which some receivers need (footnote 13).
    pub ett_unique_extension: bool,
    /// Windows with no scheduled event get one event titled with the service name.
    pub placeholders: bool,
    /// Send service location descriptors in the CVCT (optional on cable).
    pub service_location: bool,
}

impl Default for GuideConfig {
    fn default() -> Self {
        GuideConfig {
            windows: 8,
            ett_unique_extension: true,
            placeholders: true,
            service_location: true,
        }
    }
}

/// What a table is, for the carousel's periods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableKind {
    /// PAT.
    Pat,
    /// The PMT of a program.
    Pmt(ProgramNumber),
    /// MGT.
    Mgt,
    /// CVCT.
    Cvct,
    /// EIT-k.
    Eit(u8),
    /// Event ETT-k.
    EventEtt(u8),
}

/// One table: its PID, kind, version and sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The PID.
    pub pid: Pid,
    /// What it is.
    pub kind: TableKind,
    /// Its version.
    pub version: Version,
    /// Its sections, in order.
    pub sections: Vec<Section>,
}

impl Table {
    /// Total bytes.
    pub fn bytes(&self) -> u32 {
        self.sections.iter().map(|s| s.len() as u32).sum()
    }
}

/// A carrier's tables.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableSet {
    /// Every table but the STT.
    pub tables: Vec<Table>,
}

/// The descriptors every EIT event of a program carries when the EIT is present on cable
/// (A/65 Table 6.25b, p.67): one AC-3 descriptor per audio service and the caption service
/// descriptor when captions are carried.
pub fn event_defaults(p: &Program) -> Vec<Descriptor> {
    let mut d: Vec<Descriptor> = p
        .audio
        .iter()
        .map(|a| Descriptor::Ac3(a.ac3.clone()))
        .collect();
    if !p.video.captions.is_empty() {
        d.push(Descriptor::CaptionService(p.video.captions.clone()));
    }
    d
}

fn with_defaults(mut own: Vec<Descriptor>, defaults: &[Descriptor]) -> Vec<Descriptor> {
    for d in defaults {
        if !own.iter().any(|o| o.tag() == d.tag()) {
            own.push(d.clone());
        }
    }
    own
}

/// Builds every table of `carrier` at time `now`, with versions from `versions`.
pub fn build(
    carrier: &Carrier,
    schedules: &[Schedule],
    guide: &GuideConfig,
    now: GpsSeconds,
    leap: LeapSeconds,
    versions: &mut Versions,
) -> Result<TableSet, Error> {
    if guide.windows == 0 || guide.windows > 128 {
        return Err(Error::Invalid("guide windows must be 1..=128"));
    }
    let mut out = Vec::new();
    let tsid = carrier.tsid();
    let one = |s: Section| vec![s];

    let (v, s) = versions.stamp(
        Key {
            pid: 0,
            table_id: 0x00,
            extension: Some(tsid),
        },
        |v| tables::pat(carrier, v).map(one),
    )?;
    out.push(Table {
        pid: Pid::PAT,
        kind: TableKind::Pat,
        version: v,
        sections: s,
    });

    for p in carrier.programs() {
        let pid = p.pmt_pid.pid();
        let key = Key {
            pid: pid.get(),
            table_id: 0x02,
            extension: Some(p.number.get()),
        };
        let (v, s) = versions.stamp(key, |v| tables::pmt(p, v).map(one))?;
        out.push(Table {
            pid,
            kind: TableKind::Pmt(p.number),
            version: v,
            sections: s,
        });
    }

    let mut mgt = Vec::new();
    let key = Key {
        pid: Pid::PSIP_BASE.get(),
        table_id: TableId::CVCT.get(),
        extension: Some(tsid),
    };
    let (v, s) = versions.stamp(key, |v| tables::cvct(carrier, v, guide.service_location))?;
    let cvct = Table {
        pid: Pid::PSIP_BASE,
        kind: TableKind::Cvct,
        version: v,
        sections: s,
    };
    mgt.push(MgtEntry {
        table_type: TableType::CvctCurrent,
        pid: cvct.pid,
        version: v,
        number_bytes: cvct.bytes(),
    });
    let mut psip = vec![cvct];

    let w0 = Window::of(now, leap);
    for k in 0..guide.windows {
        let w = w0.plus(u32::from(k));
        let mut eit_sections: Vec<SectionsFn> = Vec::new();
        let mut all_etms = Vec::new();
        for p in carrier.programs() {
            let src = p.channel.source_id;
            let empty;
            let sched = match schedules.iter().find(|s| s.source == src) {
                Some(s) => s,
                None => {
                    empty = Schedule {
                        source: src,
                        name: short_name_text(p),
                        events: vec![],
                    };
                    &empty
                }
            };
            let defaults = event_defaults(p);
            let mut evs = events_in(sched, w, leap, guide.placeholders, &defaults)?;
            for (e, _) in &mut evs {
                e.descriptors = with_defaults(std::mem::take(&mut e.descriptors), &defaults);
            }
            all_etms.extend(etms(src, &evs));
            let evs: Vec<_> = evs.into_iter().map(|(e, _)| e).collect();
            eit_sections.push(Box::new(move |v| tables::eit(src, v, &evs)));
        }
        let pid = w.eit_pid();
        let key = Key {
            pid: pid.get(),
            table_id: TableId::EIT.get(),
            extension: None,
        };
        let (v, s) = versions.stamp(key, |v| {
            let mut all = Vec::new();
            for f in &eit_sections {
                all.extend(f(v)?);
            }
            Ok(all)
        })?;
        let t = Table {
            pid,
            kind: TableKind::Eit(k),
            version: v,
            sections: s,
        };
        mgt.push(MgtEntry {
            table_type: TableType::Eit(k),
            pid,
            version: v,
            number_bytes: t.bytes(),
        });
        psip.push(t);

        if !all_etms.is_empty() {
            let pid = w.ett_pid();
            let key = Key {
                pid: pid.get(),
                table_id: TableId::ETT.get(),
                extension: None,
            };
            let unique = guide.ett_unique_extension;
            let (v, s) = versions.stamp(key, |v| {
                all_etms
                    .iter()
                    .enumerate()
                    .map(|(i, (id, text))| {
                        let ext = if unique { (i + 1) as u16 } else { 0 };
                        tables::ett(ext, v, *id, text)
                    })
                    .collect()
            })?;
            let t = Table {
                pid,
                kind: TableKind::EventEtt(k),
                version: v,
                sections: s,
            };
            mgt.push(MgtEntry {
                table_type: TableType::EventEtt(k),
                pid,
                version: v,
                number_bytes: t.bytes(),
            });
            psip.push(t);
        }
    }

    let key = Key {
        pid: Pid::PSIP_BASE.get(),
        table_id: TableId::MGT.get(),
        extension: Some(0),
    };
    let (v, s) = versions.stamp(key, |v| tables::mgt(&mgt, v).map(one))?;
    out.push(Table {
        pid: Pid::PSIP_BASE,
        kind: TableKind::Mgt,
        version: v,
        sections: s,
    });
    out.extend(psip);
    Ok(TableSet { tables: out })
}

fn short_name_text(p: &Program) -> String {
    let f = p.channel.short_name.field();
    let units: Vec<u16> = f
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::tests::carrier;
    use tb_ts::section::Parsed;

    const LEAP: LeapSeconds = LeapSeconds(18);

    fn now() -> GpsSeconds {
        GpsSeconds::from_unix(1_795_660_000, LEAP).unwrap()
    }

    #[test]
    fn mgt_describes_what_was_built() {
        let mut v = Versions::new();
        let set = build(
            &carrier(),
            &[],
            &GuideConfig::default(),
            now(),
            LEAP,
            &mut v,
        )
        .unwrap();
        let mgt = set
            .tables
            .iter()
            .find(|t| t.kind == TableKind::Mgt)
            .unwrap();
        let p = Parsed::parse(mgt.sections[0].bytes()).unwrap();
        let n = usize::from(u16::from_be_bytes([p.body[1], p.body[2]]));
        assert_eq!(n, 1 + 8); // CVCT + EIT-0..7 (placeholders carry no ETM)
        let mut i = 3;
        for _ in 0..n {
            let e = &p.body[i..i + 11];
            let tt = u16::from_be_bytes([e[0], e[1]]);
            let pid = u16::from_be_bytes([e[2] & 0x1F, e[3]]);
            let ver = e[4] & 0x1F;
            let nb = u32::from_be_bytes([e[5], e[6], e[7], e[8]]);
            let t = set
                .tables
                .iter()
                .find(|t| t.pid.get() == pid && t.kind != TableKind::Mgt)
                .unwrap();
            assert_eq!(ver, t.version.get());
            assert_eq!(nb, t.bytes(), "number_bytes of table type {tt:#x}");
            i += 11;
        }
        // Every service has an instance in every EIT (A/65 §6.5).
        for t in set
            .tables
            .iter()
            .filter(|t| matches!(t.kind, TableKind::Eit(_)))
        {
            assert_eq!(t.sections.len(), 6);
        }
    }

    #[test]
    fn versions_stable_then_bump_on_change() {
        let mut v = Versions::new();
        let c = carrier();
        let a = build(&c, &[], &GuideConfig::default(), now(), LEAP, &mut v).unwrap();
        let b = build(&c, &[], &GuideConfig::default(), now(), LEAP, &mut v).unwrap();
        assert_eq!(a, b);
        let mut ps = c.programs().to_vec();
        ps[0].channel.short_name = crate::text::ShortName::new("NEW").unwrap();
        let c2 = Carrier::new(c.tsid(), ps).unwrap();
        let d = build(&c2, &[], &GuideConfig::default(), now(), LEAP, &mut v).unwrap();
        let ver = |s: &TableSet, k: TableKind| {
            s.tables.iter().find(|t| t.kind == k).unwrap().version.get()
        };
        assert_eq!(ver(&d, TableKind::Cvct), ver(&a, TableKind::Cvct) + 1);
        assert_eq!(ver(&d, TableKind::Mgt), ver(&a, TableKind::Mgt) + 1);
        assert_eq!(ver(&d, TableKind::Pat), ver(&a, TableKind::Pat));
    }
}
