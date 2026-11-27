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

/// A program stepped to another format: one GOP under the High-level parameters, a gap of the
/// new buffer delay and more, then a GOP whose first access unit carries the Main-level ones.
/// Everything is delivered, nothing is dropped, the new parameters apply once, and no access
/// unit of the new stream enters the buffer before its `DTS − D` of the new delay, nor before
/// the old stream's last picture is decoded.
#[test]
fn a_stream_in_another_format_starts_under_its_own_parameters() {
    use tb_mux::VideoReconfig;
    let mut m = mux(1, 8_000_000);
    let now = m.clock().stc(0).0;
    let mut old = FakeVideo {
        t0: now + D,
        coded: 0,
        es_bps: 8_000_000,
    };
    let mut last_old_dts = 0;
    for _ in 0..15 {
        let au = old.next_au();
        last_old_dts = au.dts.0;
        m.push_video(0, au).unwrap();
    }
    let d_new = 12_420_000; // 0.46 s
    let gap = d_new + 3 * FRAME;
    let mut new = FakeVideo {
        t0: last_old_dts + gap,
        coded: 0,
        es_bps: 4_000_000,
    };
    let first_new_dts = new.t0;
    for k in 0..15 {
        let mut au = new.next_au();
        if k == 0 {
            au.reconfig = Some(VideoReconfig {
                rx_bps: 18_000_000,
                buffer_delay: d_new,
                rate_bps: tb_mux::ts_rate_for(4_000_000, 30_000, 1001, 30),
            });
        }
        m.push_video(0, au).unwrap();
    }
    // Run until both GOPs are out, noting when each PES starts on the video PID.
    let mut starts: Vec<(u64, u64)> = Vec::new(); // (slot STC, DTS)
    for _ in 0..(38_800_000 / 1504 * 3) {
        let (n, p) = m.next_slot();
        let b = &p.0;
        let pid = (u16::from(b[1] & 0x1F) << 8) | u16::from(b[2]);
        if pid == 0x31 && b[1] & 0x40 != 0 {
            let afc = (b[3] >> 4) & 3;
            let off = if afc == 3 { 5 + usize::from(b[4]) } else { 4 };
            let h = &b[off..];
            if h[..3] == [0, 0, 1] && h[7] & 0x80 != 0 {
                // the DTS follows the PTS; without one it equals the PTS
                let t = if h[7] & 0x40 != 0 {
                    &h[14..19]
                } else {
                    &h[9..14]
                };
                let dts = (u64::from(t[0] >> 1 & 7) << 30)
                    | (u64::from(t[1]) << 22)
                    | (u64::from(t[2] >> 1) << 15)
                    | (u64::from(t[3]) << 7)
                    | u64::from(t[4] >> 1);
                starts.push((m.clock().stc(n).0, dts * 300));
            }
        }
    }
    let s = m.program_stats(0).unwrap();
    assert_eq!(
        (s.video_aus, s.video_dropped, s.reconfigs),
        (30, 0, 1),
        "{s:?}"
    );
    // (the STC here is far below the 33-bit wrap of the 90 kHz clock)
    // (a DTS read back is in 90 kHz units: up to 299 ticks below the 27 MHz value)
    let news: Vec<_> = starts
        .iter()
        .filter(|(_, dts)| *dts + 300 > first_new_dts)
        .collect();
    assert_eq!(news.len(), 15);
    for &&(at, dts) in &news {
        assert!(
            at + d_new >= dts,
            "a new AU (DTS {dts}) entered {} ticks early",
            dts - d_new - at
        );
        assert!(
            at >= last_old_dts,
            "a new AU entered before the old stream's last DTS"
        );
    }
}
