//! Shared fixtures: a lineup, synthetic encoders, and a stream checker written for these tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::collections::HashMap;

use tb_mux::*;
use tb_psip::build::{GuideConfig, TableSet, build};
use tb_psip::descriptor::Ac3Descriptor;
use tb_psip::gps::{DstRule, GpsSeconds, LeapSeconds};
use tb_psip::lineup::*;
use tb_psip::text::{Lang, Mss, ShortName};
use tb_psip::version::Versions;
use tb_ts::section::{Assembler, Parsed};
use tb_ts::{Packet, Pid, STC_HZ, Stc};

pub const MS: u64 = STC_HZ / 1000;
pub const FRAME: u64 = 900_900; // 1001/30000 s
pub const AC3: u64 = 864_000; // 32 ms
pub const D: u64 = STC_HZ / 2;
pub const UNIX0: i64 = 1_795_660_000;

pub fn program(n: u16) -> Program {
    let base = 0x30 + 0x10 * (n - 1);
    Program {
        number: ProgramNumber::new(n).unwrap(),
        pmt_pid: EsPid::new(base).unwrap(),
        video: Video {
            pid: EsPid::new(base + 1).unwrap(),
            codec: VideoCodec::Mpeg2,
            captions: vec![],
        },
        audio: vec![Audio {
            pid: EsPid::new(base + 4).unwrap(),
            ac3: Ac3Descriptor {
                bit_rate_index: 10,
                surround_mode: 0,
                bsmod: 0,
                acmod: 2,
                max_channels: None,
                rate_upper_limit: false,
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
            number: ChannelNumber::new(50, n).unwrap(),
            short_name: ShortName::new(&format!("TB-{n}")).unwrap(),
            long_name: Some(Mss::eng("test")),
            source_id: SourceId::new(n).unwrap(),
            service_type: ServiceType::DigitalTv,
            hidden: false,
            hide_guide: false,
            etm: EtmLocation::None,
        },
    }
}

pub fn tables(n: u16) -> TableSet {
    let carrier = Carrier::new(0x0101, (1..=n).map(program).collect()).unwrap();
    let leap = LeapSeconds(18);
    let now = GpsSeconds::from_unix(UNIX0, leap).unwrap();
    build(
        &carrier,
        &[],
        &GuideConfig::default(),
        now,
        leap,
        &mut Versions::new(),
    )
    .unwrap()
}

pub fn mux(n: u16, es_bps: u64) -> Mux {
    let programs = (1..=n)
        .map(|i| {
            let base = 0x30 + 0x10 * (i - 1);
            ProgramConfig {
                number: i,
                video: VideoConfig {
                    pid: Pid::new(base + 1).unwrap(),
                    rx_bps: 96_000_000,
                    rate_bps: ts_rate_for(es_bps, 30_000, 1001, 30),
                    buffer_delay: D,
                    es_priority_on_key: false,
                },
                audio: vec![AudioConfig::ac3(Pid::new(base + 4).unwrap())],
                pcr_interval: 30 * MS,
            }
        })
        .collect();
    let mut m = Mux::new(MuxConfig {
        rate: TsRate::DEFAULT,
        stc0: Stc(1_000_000_000),
        programs,
        periods: Periods::default(),
        stt: Some(SttConfig {
            leap: LeapSeconds(18),
            dst: DstRule::Us,
        }),
        queue_limit: 64,
        token_depth: 2,
    })
    .unwrap();
    m.set_tables(&tables(n));
    m.set_wall_clock(WallClock {
        stc: 1_000_000_000,
        unix_nanos: i128::from(UNIX0) * 1_000_000_000,
    });
    m
}

/// A synthetic MPEG-2 encoder: closed GOPs of 15 (I P B B ...), sizes cycling so the rate is
/// below `es_bps`, times on the carrier STC from `t0` (DTS of the first picture).
pub struct FakeVideo {
    pub t0: u64,
    pub coded: u64,
    pub es_bps: u64,
}

const ORDER: [u64; 15] = [0, 3, 1, 2, 6, 4, 5, 9, 7, 8, 12, 10, 11, 14, 13];

impl FakeVideo {
    pub fn next_au(&mut self) -> VideoAu {
        let j = self.coded;
        self.coded += 1;
        let gop = j / 15;
        let k = (j % 15) as usize;
        let d = gop * 15 + ORDER[k];
        let dts = self.t0 + j * FRAME;
        let pts = self.t0 + (d + 1) * FRAME;
        let per_frame = self.es_bps / 30 / 8;
        // Anchors are the pictures displayed after every picture coded before them.
        let anchor = ORDER[..k].iter().all(|&o| o < ORDER[k]);
        // I 4, P 1, B 0.6 units: 14.4 units per 15 pictures, plus up to 1 kB of noise.
        let size = match k {
            0 => per_frame * 4,
            _ if anchor => per_frame,
            _ => per_frame * 6 / 10,
        } as usize;
        let size = size * 9 / 10 + (j as usize * 7919) % 997;
        VideoAu {
            data: vec![(j & 0xFF) as u8; size.max(1)],
            pts: Stc(pts),
            dts: Stc(dts),
            keyframe: k == 0,
            reconfig: None,
        }
    }
}

/// What a stream check finds.
#[derive(Debug, Default)]
pub struct Report {
    pub cc_errors: u64,
    pub pcr_wrong: u64,
    pub pcr_max_gap: HashMap<u16, u64>,
    pub pes_late: u64,
    pub pes_early: u64,
    pub pes_count: HashMap<u16, u64>,
    pub table_max_gap: HashMap<(u8, u16), u64>,
    pub nulls: u64,
    pub packets: u64,
    pub crc_errors: u64,
}

/// Re-parses `(slot, packet)` pairs: continuity, PCR value against the slot clock, PES
/// delivery windows, table intervals.
pub fn check(clock: &SlotClock, packets: &[(u64, Packet)], video: &[u16], audio: &[u16]) -> Report {
    let mut r = Report::default();
    let mut last_cc: HashMap<u16, u8> = HashMap::new();
    let mut last_pcr: HashMap<u16, u64> = HashMap::new();
    let mut pes_start: HashMap<u16, (u64, Vec<u8>)> = HashMap::new();
    let mut last_payload: HashMap<u16, u64> = HashMap::new();
    let mut asm: HashMap<u16, Assembler> = HashMap::new();
    let mut table_last: HashMap<(u8, u16), u64> = HashMap::new();
    let finish = |pid: u16, start: u64, end: u64, bytes: &[u8], r: &mut Report| {
        let h = tb_ts::pes::parse(bytes).unwrap();
        *r.pes_count.entry(pid).or_default() += 1;
        let end_t = clock.stc(end + 1).0;
        let start_t = clock.stc(start).0;
        let base = clock.stc(0).0 - clock.stc(0).0 % (1u64 << 33).wrapping_mul(300).max(1);
        let _ = base;
        let ts = |t: tb_ts::Ts90k| t.get() * 300;
        let stc_mod = |t: u64| (t / 300) % (1 << 33) * 300;
        if video.contains(&pid) {
            let dts = ts(h.dts.or(h.pts).unwrap());
            if stc_mod(end_t) > dts {
                eprintln!(
                    "late video PES on {pid:#x}: slots {start}..{end}, end {} dts {dts}",
                    stc_mod(end_t)
                );
                r.pes_late += 1;
            }
            if stc_mod(start_t) + D + 300 < dts {
                r.pes_early += 1;
            }
        } else if audio.contains(&pid) {
            let pts = ts(h.pts.unwrap());
            if stc_mod(end_t) > pts {
                eprintln!(
                    "late audio PES on {pid:#x}: slots {start}..{end}, end {} pts {pts}",
                    stc_mod(end_t)
                );
                r.pes_late += 1;
            }
            if stc_mod(start_t) + 40 * MS + 300 < pts {
                r.pes_early += 1;
            }
        }
    };
    for (slot, p) in packets {
        r.packets += 1;
        let v = p.view().unwrap();
        let pid = v.pid().get();
        if pid == 0x1FFF {
            r.nulls += 1;
            continue;
        }
        if let Some(&c) = last_cc.get(&pid) {
            let want = if v.has_payload() { (c + 1) & 15 } else { c };
            if v.cc().get() != want {
                r.cc_errors += 1;
            }
        }
        last_cc.insert(pid, v.cc().get());
        if let Some(pcr) = v.pcr() {
            if pcr != clock.pcr_stc(*slot).pcr() {
                r.pcr_wrong += 1;
            }
            let t = clock.pcr_stc(*slot).0;
            if let Some(l) = last_pcr.insert(pid, t) {
                let g = r.pcr_max_gap.entry(pid).or_default();
                *g = (*g).max(t - l);
            }
        }
        if video.contains(&pid) || audio.contains(&pid) {
            if v.pusi() {
                if let Some((s, b)) = pes_start.remove(&pid) {
                    finish(pid, s, last_payload[&pid], &b, &mut r);
                }
                pes_start.insert(pid, (*slot, Vec::new()));
            }
            if v.has_payload() {
                last_payload.insert(pid, *slot);
            }
            if let Some((_, b)) = pes_start.get_mut(&pid) {
                b.extend_from_slice(v.payload());
                // Audio PES are bounded: finish when complete.
                if audio.contains(&pid) && b.len() >= 6 {
                    let len = usize::from(u16::from_be_bytes([b[4], b[5]])) + 6;
                    if b.len() >= len {
                        let (s, b) = pes_start.remove(&pid).unwrap();
                        finish(pid, s, *slot, &b[..len], &mut r);
                    }
                }
            }
        } else {
            let mut out = Vec::new();
            asm.entry(pid).or_default().push(&v, &mut out);
            for s in out {
                match Parsed::parse(&s) {
                    Ok(ps) => {
                        let key = (ps.header.table_id.get(), pid);
                        let t = clock.stc(*slot).0;
                        if let Some(l) = table_last.insert((key.0, ps.header.extension ^ pid), t) {
                            let g = r.table_max_gap.entry(key).or_default();
                            *g = (*g).max(t - l);
                        }
                    }
                    Err(_) => r.crc_errors += 1,
                }
            }
        }
    }
    r
}
