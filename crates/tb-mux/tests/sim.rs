//! The mux fed by synthetic encoders for tens of seconds of stream time, its output re-parsed.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;
use common::*;

use std::io::Write;
use tb_mux::{AudioAu, PushError};
use tb_ts::Stc;

fn run(programs: u16, es_bps: u64, seconds: u64, dump: Option<&str>) -> (tb_mux::Mux, Report) {
    let mut m = mux(programs, es_bps);
    let t0 = m.clock().stc(0).0 + 600 * MS;
    let mut vids: Vec<FakeVideo> = (0..programs)
        .map(|_| FakeVideo {
            t0,
            coded: 0,
            es_bps,
        })
        .collect();
    let mut pending: Vec<Option<tb_mux::VideoAu>> = vec![None; programs as usize];
    let mut audio_next: Vec<u64> = vec![t0; programs as usize];
    let slots = seconds * 38_800_000 / 1504;
    let mut out = Vec::with_capacity(slots as usize);
    let mut file = dump.map(|p| std::fs::File::create(p).unwrap());
    for _ in 0..slots {
        let now = m.clock().stc(m.slot()).0;
        // Encoders deliver about one frame period ahead of D (their output is "on time").
        for (i, v) in vids.iter_mut().enumerate() {
            loop {
                let au = pending[i].take().unwrap_or_else(|| v.next_au());
                if au.dts.0 > now + D + 2 * FRAME {
                    pending[i] = Some(au);
                    break;
                }
                match m.push_video(i, au) {
                    Ok(()) => {}
                    Err(PushError::Full) => panic!("queue full"),
                    Err(e) => panic!("{e}"),
                }
            }
            while audio_next[i] < now + 100 * MS {
                m.push_audio(
                    i,
                    0,
                    AudioAu {
                        data: vec![0x0B; 768],
                        pts: Stc(audio_next[i]),
                    },
                )
                .unwrap();
                audio_next[i] += AC3;
            }
        }
        let (n, p) = m.next_slot();
        if let Some(f) = &mut file {
            f.write_all(&p.0).unwrap();
        }
        out.push((n, p));
    }
    let video: Vec<u16> = (1..=programs).map(|i| 0x30 + 0x10 * (i - 1) + 1).collect();
    let audio: Vec<u16> = (1..=programs).map(|i| 0x30 + 0x10 * (i - 1) + 4).collect();
    let r = check(m.clock(), &out, &video, &audio);
    (m, r)
}

#[test]
fn two_programs_twenty_seconds() {
    let dump = std::env::var("TB_MUX_DUMP").ok();
    let (m, r) = run(2, 8_000_000, 20, dump.as_deref());
    assert_eq!(r.cc_errors, 0);
    assert_eq!(r.pcr_wrong, 0);
    assert_eq!(r.crc_errors, 0);
    assert_eq!((r.pes_late, r.pes_early), (0, 0), "{r:?}");
    for (pid, g) in &r.pcr_max_gap {
        assert!(*g <= 31 * MS, "PCR gap {} ms on {pid:#x}", g / MS);
    }
    for ((tid, pid), g) in &r.table_max_gap {
        let max = match tid {
            0x00 => 100,
            0x02 => 400,
            0xC7 => 150,
            0xC9 => 400,
            0xCD => 1100,
            0xCB => 3100,
            _ => 100_000,
        };
        assert!(*g <= max * MS, "table {tid:#x} on {pid:#x}: {} ms", g / MS);
    }
    for i in 0..2 {
        let s = m.program_stats(i).unwrap();
        assert_eq!((s.video_dropped, s.audio_dropped), (0, 0));
        // 20 s minus the start-up delay.
        assert!(s.video_aus > 500, "{s:?}");
        assert!(s.audio_aus > 550, "{s:?}");
    }
    assert_eq!(m.table_overruns(), 0);
    // Constant rate: every slot was produced; nulls fill the rest.
    assert!(r.nulls > 0);
}

#[test]
fn six_programs_near_capacity() {
    // 6 × 5.4 Mbit/s of video plus audio and tables: close to the 38.8 Mbit/s carrier.
    let (m, r) = run(6, 5_400_000, 6, None);
    assert_eq!(
        (r.cc_errors, r.pcr_wrong, r.pes_late, r.pes_early),
        (0, 0, 0, 0),
        "{r:?}"
    );
    for i in 0..6 {
        let s = m.program_stats(i).unwrap();
        assert_eq!((s.video_dropped, s.audio_dropped), (0, 0), "{s:?}");
    }
}

#[test]
fn starved_encoder_keeps_pcr_and_tables() {
    // No access units at all: PCR-only packets, tables, nulls; nothing else.
    let mut m = mux(1, 4_000_000);
    let mut out = Vec::new();
    for _ in 0..(38_800_000 / 1504 * 2) {
        out.push(m.next_slot());
    }
    let r = check(m.clock(), &out, &[0x31], &[0x34]);
    assert_eq!((r.cc_errors, r.pcr_wrong), (0, 0));
    assert!(r.pcr_max_gap[&0x31] <= 31 * MS);
    assert!(m.program_stats(0).unwrap().pcr_only_packets > 60);
}

#[test]
fn late_access_units_drop_to_the_next_key_frame() {
    let mut m = mux(1, 4_000_000);
    let now = m.clock().stc(0).0;
    let mut v = FakeVideo {
        t0: now + FRAME,
        coded: 0,
        es_bps: 4_000_000,
    };
    // The first GOP is due almost at once: too late at 4 Mbit/s for the I picture.
    for _ in 0..30 {
        m.push_video(0, v.next_au()).unwrap();
    }
    for _ in 0..(38_800_000 / 1504 * 2) {
        m.next_slot();
    }
    let s = m.program_stats(0).unwrap();
    assert!(s.video_dropped >= 1);
    // Dropping stops at the second GOP's key frame, which arrives in time.
    assert!(s.video_aus >= 1, "{s:?}");
}

#[test]
fn skip_keeps_pcr_exact() {
    let mut m = mux(1, 4_000_000);
    let mut out = Vec::new();
    for _ in 0..1000 {
        out.push(m.next_slot());
    }
    m.skip_to(5000);
    for _ in 0..1000 {
        out.push(m.next_slot());
    }
    let r = check(m.clock(), &out, &[0x31], &[0x34]);
    assert_eq!((r.cc_errors, r.pcr_wrong), (0, 0));
    assert_eq!(m.stats().skipped, 4000);
}
