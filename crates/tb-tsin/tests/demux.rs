//! The demuxer and the UDP input on hand-built streams: streams and packets, program
//! selection, PMT changes, loss and duplicates, split invariance, RTP reordering and
//! sessions, and random corruption.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use common::*;
use tb_media::{CodecId, EndReason, InputEvent, Packet, StreamIndex};
use tb_tsin::{DemuxConfig, Demuxer, ProgramSelector, Transport, TsEvent, TsInput, TsInputConfig};

fn demux_all(bytes: &[u8], config: DemuxConfig) -> (Vec<TsEvent>, Demuxer) {
    let mut d = Demuxer::new(config);
    d.push_bytes(bytes);
    d.flush();
    (d.take_events(), d)
}

fn packets(ev: &[TsEvent], pid: u16) -> Vec<(Packet, bool)> {
    ev.iter()
        .filter_map(|e| match &e.event {
            InputEvent::Packet(p) if p.stream == StreamIndex(pid) => {
                Some((p.clone(), e.after_loss))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn streams_and_packets() {
    let (ev, d) = demux_all(&simple_stream(30, 900_000), DemuxConfig::default());
    let InputEvent::Streams(s) = &ev[0].event else {
        panic!()
    };
    assert_eq!(s.len(), 2);
    assert_eq!(
        (s[0].index, s[0].codec),
        (StreamIndex(0x100), CodecId::H264)
    );
    assert_eq!((s[1].index, s[1].codec), (StreamIndex(0x101), CodecId::Ac3));
    assert_eq!(s[1].language, Some(*b"eng"));
    assert_eq!(s[0].time_base, tb_media::Rational::MPEG_90K);
    let v = packets(&ev, 0x100);
    assert_eq!(v.len(), 30);
    for (i, (p, loss)) in v.iter().enumerate() {
        let t = 900_000 + i as i64 * 3000;
        assert_eq!((p.dts, p.pts), (Some(t), Some(t + 6000)));
        assert_eq!(p.keyframe, i % 15 == 0);
        assert!(!loss);
        assert_eq!(p.data, h264_au(i % 15 == 0, 100 + (i * 97) % 900, i as u8));
    }
    let a = packets(&ev, 0x101);
    assert_eq!(a.len(), 30);
    assert!(a.iter().all(|(p, _)| p.keyframe && p.dts == p.pts));
    assert_eq!(d.stats().cc_errors, 0);
    assert_eq!(d.program(), Some(1));
}

#[test]
fn every_split_point_gives_the_same_events() {
    let bytes = simple_stream(6, 0);
    let (whole, _) = demux_all(&bytes, DemuxConfig::default());
    for k in 0..=bytes.len() {
        let mut d = Demuxer::new(DemuxConfig::default());
        d.push_bytes(&bytes[..k]);
        d.push_bytes(&bytes[k..]);
        d.flush();
        assert_eq!(d.take_events(), whole, "split at {k}");
    }
}

#[test]
fn timestamps_unwrap_across_2_pow_33() {
    let base = (1u64 << 33) - 30_000;
    let (ev, _) = demux_all(&simple_stream(30, base), DemuxConfig::default());
    let v = packets(&ev, 0x100);
    for (i, (p, _)) in v.iter().enumerate() {
        assert_eq!(p.dts, Some(base as i64 + i as i64 * 3000));
    }
    assert!(v[29].0.dts.unwrap() > 1 << 33);
}

#[test]
fn loss_drops_the_partial_pes_and_flags_the_next() {
    let bytes = simple_stream(10, 0);
    // Find the packets of PID 0x100 and drop the second packet of frame 3 (frames are > 1
    // packet: 100 + 3*97 = 391 bytes).
    let pkts: Vec<&[u8]> = bytes.chunks(188).collect();
    let mut frame = -1i32;
    let mut drop_at = None;
    for (i, p) in pkts.iter().enumerate() {
        let pid = u16::from(p[1] & 0x1F) << 8 | u16::from(p[2]);
        if pid == 0x100 {
            if p[1] & 0x40 != 0 {
                frame += 1;
            } else if frame == 3 && drop_at.is_none() {
                drop_at = Some(i);
            }
        }
    }
    let mut damaged = Vec::new();
    for (i, p) in pkts.iter().enumerate() {
        if Some(i) != drop_at {
            damaged.extend_from_slice(p);
        }
    }
    let (ev, d) = demux_all(&damaged, DemuxConfig::default());
    let v = packets(&ev, 0x100);
    assert_eq!(v.len(), 9, "frame 3 is dropped");
    let flagged: Vec<i64> = v
        .iter()
        .filter(|(_, l)| *l)
        .map(|(p, _)| p.dts.unwrap())
        .collect();
    assert_eq!(flagged, [4 * 3000], "frame 4 is flagged");
    assert_eq!(d.stats().cc_errors, 1);
    assert_eq!(packets(&ev, 0x101).len(), 10, "audio untouched");

    // A duplicated packet is dropped silently.
    let mut dup = Vec::new();
    for (i, p) in pkts.iter().enumerate() {
        dup.extend_from_slice(p);
        if Some(i) == drop_at {
            dup.extend_from_slice(p);
        }
    }
    let (ev, d) = demux_all(&dup, DemuxConfig::default());
    assert_eq!(packets(&ev, 0x100).len(), 10);
    assert_eq!(d.stats().duplicates, 1);
    assert_eq!(d.stats().cc_errors, 0);
}

#[test]
fn program_selection_and_pmt_changes() {
    let mut w = Writer::new();
    w.pat(&[(5, 0x1000), (7, 0x1010)], 0);
    let h264 = |pid| Es {
        stream_type: 0x1B,
        pid,
        descriptors: vec![],
    };
    w.pmt(0x1000, 5, 0, 0x100, &[h264(0x100)]);
    w.pmt(
        0x1010,
        7,
        0,
        0x200,
        &[
            h264(0x200),
            Es {
                stream_type: 0x0F,
                pid: 0x201,
                descriptors: vec![],
            },
        ],
    );
    w.pes(0x100, 0xE0, 0, None, &h264_au(true, 10, 1), true);
    w.pes(0x200, 0xE0, 0, None, &h264_au(true, 10, 2), true);
    // Program 7's PMT: same version again (ignored), then version 1 adds AC-3 via the DVB
    // descriptor and drops AAC.
    w.pmt(0x1010, 7, 0, 0x200, &[h264(0x200)]);
    w.pmt(
        0x1010,
        7,
        1,
        0x200,
        &[
            h264(0x200),
            Es {
                stream_type: 0x06,
                pid: 0x202,
                descriptors: vec![0x6A, 1, 0],
            },
            Es {
                stream_type: 0x11,
                pid: 0x203,
                descriptors: vec![],
            },
        ],
    );
    w.pes(0x200, 0xE0, 3000, None, &h264_au(false, 10, 3), false);
    w.pes(0x202, 0xBD, 3000, None, &[0x0B, 0x77], false);
    let bytes = w.out;

    let (ev, _) = demux_all(&bytes, DemuxConfig::default());
    let InputEvent::Streams(s) = &ev[0].event else {
        panic!()
    };
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].index, StreamIndex(0x100));
    assert_eq!(packets(&ev, 0x100).len(), 1);
    assert!(packets(&ev, 0x200).is_empty());

    let cfg = DemuxConfig {
        program: ProgramSelector::Number(7),
        ..DemuxConfig::default()
    };
    let (ev, d) = demux_all(&bytes, cfg);
    let announced: Vec<Vec<(u16, CodecId)>> = ev
        .iter()
        .filter_map(|e| match &e.event {
            InputEvent::Streams(s) => Some(s.iter().map(|s| (s.index.0, s.codec)).collect()),
            _ => None,
        })
        .collect();
    assert_eq!(
        announced,
        [
            vec![(0x200, CodecId::H264), (0x201, CodecId::Aac)],
            vec![(0x200, CodecId::H264), (0x202, CodecId::Ac3)],
        ]
    );
    assert_eq!(packets(&ev, 0x200).len(), 2);
    assert_eq!(packets(&ev, 0x202).len(), 1);
    assert!(d.stats().skipped_streams >= 1, "LATM is skipped");
}

/// Datagrams of 7 packets, as RTP with shuffled order inside a small window, through TsInput:
/// the same events as the byte stream, then End after the idle timeout.
#[test]
fn rtp_input_reorders_and_ends() {
    let bytes = simple_stream(20, 0);
    let (whole, _) = demux_all(&bytes, DemuxConfig::default());
    let dgrams: Vec<Vec<u8>> = bytes
        .chunks(7 * 188)
        .enumerate()
        .map(|(i, c)| rtp(65_530u16.wrapping_add(i as u16), 0xABCD, c))
        .collect();
    // Swap neighbors pairwise (after the first: the first packet received starts the
    // session, so an earlier one arriving after it would be late).
    let mut order: Vec<usize> = (0..dgrams.len()).collect();
    for k in (1..order.len() - 1).step_by(2) {
        order.swap(k, k + 1);
    }
    let mut input = TsInput::new(TsInputConfig::default());
    let mut ev = Vec::new();
    for (n, &i) in order.iter().enumerate() {
        ev.extend(input.on_datagram(Duration::from_millis(n as u64), &dgrams[i]));
    }
    assert_eq!(input.transport(), Transport::Rtp);
    assert_eq!(input.stats().rtp.reordered, ((dgrams.len() - 1) / 2) as u64);
    ev.extend(input.on_tick(Duration::from_secs(10)));
    let end = ev.pop().unwrap();
    assert!(matches!(
        end.event,
        InputEvent::End(EndReason::Transport(_))
    ));
    let flushed = {
        let mut d = Demuxer::new(DemuxConfig::default());
        d.push_bytes(&bytes);
        d.flush();
        d.take_events()
    };
    assert_eq!(ev, flushed);
    assert_eq!(whole, flushed);

    // A new SSRC ends the session and starts another.
    let mut input = TsInput::new(TsInputConfig::default());
    let mut ev = Vec::new();
    for (i, c) in bytes.chunks(7 * 188).enumerate() {
        ev.extend(input.on_datagram(Duration::ZERO, &rtp(i as u16, 1, c)));
    }
    for (i, c) in bytes.chunks(7 * 188).enumerate() {
        ev.extend(input.on_datagram(Duration::ZERO, &rtp(1000 + i as u16, 2, c)));
    }
    ev.extend(input.finish(Duration::ZERO));
    let kinds: Vec<&str> = ev
        .iter()
        .filter_map(|e| match &e.event {
            InputEvent::Streams(_) => Some("S"),
            InputEvent::End(EndReason::Transport(_)) => Some("T"),
            InputEvent::End(EndReason::Closed) => Some("C"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["S", "T", "S", "C"]);
}

#[test]
fn raw_udp_input_and_rejects() {
    let bytes = simple_stream(5, 0);
    let mut input = TsInput::new(TsInputConfig::default());
    let mut ev = Vec::new();
    for c in bytes.chunks(7 * 188) {
        ev.extend(input.on_datagram(Duration::ZERO, c));
    }
    ev.extend(input.on_datagram(Duration::ZERO, &rtp(0, 1, &bytes[..188])));
    ev.extend(input.finish(Duration::ZERO));
    assert_eq!(input.transport(), Transport::RawUdp);
    assert_eq!(input.stats().rejected, 1);
    assert_eq!(packets(&ev, 0x100).len(), 5);
    assert!(matches!(
        ev.last().unwrap().event,
        InputEvent::End(EndReason::Closed)
    ));
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Random bytes, random corruption and random datagrams: no panic, packets only of announced
/// streams, at most one End per session.
#[test]
fn corruption_never_panics() {
    let good = simple_stream(40, 0);
    let mut rng = Rng(0x5EED);
    for round in 0..400 {
        let mut b = if round % 4 == 0 {
            (0..rng.below(30_000)).map(|_| rng.next() as u8).collect()
        } else {
            good.clone()
        };
        if round % 4 != 0 {
            for _ in 0..1 + rng.below(50) {
                let i = rng.below(b.len());
                b[i] = rng.next() as u8;
            }
        }
        // As a byte stream.
        let mut d = Demuxer::new(DemuxConfig::default());
        let mut at = 0;
        while at < b.len() {
            let n = (1 + rng.below(2000)).min(b.len() - at);
            d.push_bytes(&b[at..at + n]);
            at += n;
        }
        d.flush();
        check_events(&d.take_events());
        // As datagrams (raw and RTP with random sequence numbers).
        let mut input = TsInput::new(TsInputConfig::default());
        let mut ev = Vec::new();
        for (i, c) in b.chunks(1 + rng.below(1500)).enumerate() {
            let dg = if round % 2 == 0 {
                c.to_vec()
            } else {
                rtp((i as u16).wrapping_mul(1 + rng.below(3) as u16), 7, c)
            };
            ev.extend(input.on_datagram(Duration::from_millis(i as u64), &dg));
        }
        ev.extend(input.finish(Duration::from_secs(10)));
        check_events(&ev);
    }
}

#[test]
fn oversized_pes_is_dropped_and_flagged() {
    // Video PES are 19 header bytes + a 10-byte prefix + 100 + (i * 97) % 900 bytes.
    let bytes = simple_stream(20, 0);
    let size = |i: usize| 19 + 10 + 100 + (i * 97) % 900;
    let cfg = DemuxConfig {
        max_pes: 600,
        ..DemuxConfig::default()
    };
    let (ev, d) = demux_all(&bytes, cfg);
    let v = packets(&ev, 0x100);
    let expected: Vec<usize> = (0..20).filter(|&i| size(i) <= 600).collect();
    let got: Vec<usize> = v
        .iter()
        .map(|(p, _)| (p.dts.unwrap() / 3000) as usize)
        .collect();
    assert_eq!(got, expected);
    assert!(d.stats().pes_dropped > 0);
    // The packet after a dropped one is flagged.
    for (p, loss) in &v {
        let i = (p.dts.unwrap() / 3000) as usize;
        assert_eq!(*loss, i > 0 && !expected.contains(&(i - 1)), "frame {i}");
    }
    // A total bound below two packets' payload: only PES that fit one packet survive.
    let cfg = DemuxConfig {
        max_buffered: 150,
        ..DemuxConfig::default()
    };
    let (ev, _) = demux_all(&bytes, cfg);
    let v = packets(&ev, 0x100);
    assert!(!v.is_empty());
    assert!(v.iter().all(|(p, _)| p.data.len() + 19 <= 184));
    assert_eq!(
        packets(&ev, 0x101).len(),
        20,
        "single-packet audio PES pass"
    );
}

fn check_events(ev: &[TsEvent]) {
    let mut announced: Option<Vec<StreamIndex>> = None;
    for e in ev {
        match &e.event {
            InputEvent::Streams(s) => announced = Some(s.iter().map(|s| s.index).collect()),
            InputEvent::Packet(p) => {
                assert!(announced.as_ref().unwrap().contains(&p.stream));
            }
            InputEvent::End(_) => {
                assert!(announced.take().is_some(), "End without a session");
            }
        }
    }
}
