//! Writes one carousel cycle of a sample carrier's tables (and an STT) as transport packets to
//! the file named by the first argument, for cross-checking with TSDuck:
//! `tstables --atsc FILE` and `tsanalyze --atsc FILE`.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::io::Write;

use tb_psip::build::{GuideConfig, build};
use tb_psip::descriptor::{Ac3Descriptor, CaptionKind, CaptionService, ServiceNumber};
use tb_psip::gps::{DstRule, GpsSeconds, LeapSeconds};
use tb_psip::guide::{Event, Schedule};
use tb_psip::lineup::*;
use tb_psip::rating::{Dlsv, UsRating};
use tb_psip::tables::{EventId, stt};
use tb_psip::text::{Lang, Mss, ShortName};
use tb_psip::version::Versions;
use tb_ts::Cc;
use tb_ts::section::Cursor;

fn program(n: u16) -> Program {
    let base = 0x30 + 0x10 * (n - 1);
    Program {
        number: ProgramNumber::new(n).unwrap(),
        pmt_pid: EsPid::new(base).unwrap(),
        video: Video {
            pid: EsPid::new(base + 1).unwrap(),
            codec: if n == 2 {
                VideoCodec::H264 {
                    profile_idc: 100,
                    constraint_flags: 0,
                    level_idc: 40,
                }
            } else {
                VideoCodec::Mpeg2
            },
            captions: vec![
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
            ],
        },
        audio: vec![Audio {
            pid: EsPid::new(base + 4).unwrap(),
            ac3: Ac3Descriptor {
                bit_rate_index: 10,
                surround_mode: 0,
                bsmod: 0,
                acmod: 2,
                max_channels: None,
                mainid: 0,
                priority: 1,
                asvcflags: 0,
                lang: Lang::ENG,
            },
            audio_type: 0,
            component_name: None,
        }],
        rating: (n == 1).then_some(
            UsRating::Tv14(Dlsv {
                l: true,
                v: true,
                ..Dlsv::default()
            })
            .into(),
        ),
        channel: Channel {
            number: ChannelNumber::new(50, n).unwrap(),
            short_name: ShortName::new(&format!("TB-{n}")).unwrap(),
            long_name: Some(Mss::eng(&format!("Totalblonder channel {n}"))),
            source_id: SourceId::new(n).unwrap(),
            service_type: ServiceType::DigitalTv,
            hidden: false,
            hide_guide: false,
            etm: EtmLocation::None,
        },
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("output path");
    let leap = LeapSeconds(18);
    let unix = 1_795_660_000i64; // 2026-11-26
    let now = GpsSeconds::from_unix(unix, leap).unwrap();
    let carrier = Carrier::new(0x0101, (1..=3).map(program).collect()).unwrap();
    let sched = Schedule {
        source: SourceId::new(1).unwrap(),
        name: "TB-1".into(),
        events: vec![Event {
            id: EventId::new(42).unwrap(),
            start: GpsSeconds(now.0 - 600),
            length: 3600,
            title: Mss::eng("The test pattern hour"),
            description: Some(Mss::eng("Color bars and a tone, with captions.")),
            descriptors: vec![tb_psip::descriptor::Descriptor::ContentAdvisory(
                UsRating::TvG.into(),
            )],
        }],
    };
    let mut v = Versions::new();
    let set = build(
        &carrier,
        &[sched],
        &GuideConfig {
            windows: 4,
            ..GuideConfig::default()
        },
        now,
        leap,
        &mut v,
    )
    .unwrap();
    let mut f = std::fs::File::create(path).unwrap();
    let mut ccs = std::collections::HashMap::new();
    let mut emit = |pid: tb_ts::Pid, secs: Vec<tb_ts::section::Section>, f: &mut std::fs::File| {
        let cc = ccs.entry(pid).or_insert(Cc::new(0));
        let mut c = Cursor::new(secs);
        while let Some(p) = c.next_packet(pid, cc) {
            f.write_all(&p.0).unwrap();
        }
    };
    for _ in 0..3 {
        for t in &set.tables {
            emit(t.pid, t.sections.clone(), &mut f);
        }
        let s = stt(now, leap.0, DstRule::Us.at(unix)).unwrap();
        emit(tb_ts::Pid::PSIP_BASE, vec![s], &mut f);
    }
}
