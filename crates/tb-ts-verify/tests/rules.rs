//! One mutated stream per rule: a conformant synthetic stream (tests/common) in which one
//! thing is changed so that it violates the rule. The checker has to report exactly the
//! listed rules: the one under test and, where one fault necessarily shows as several,
//! those too (each listed consequence is explained). `coverage` lists the checked rules of
//! the catalog without a detecting case.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tb_ts_verify::rules::{ALL_RULES, Check};
use tb_ts_verify::{Options, check};

struct Case {
    name: &'static str,
    rules: &'static [&'static str],
    make: fn() -> (Stream, Options),
}

fn std(spec: &Spec) -> (Stream, Options) {
    (build(spec), opts(spec))
}

fn with(spec: Spec, f: impl FnOnce(&Spec, &mut Stream)) -> (Stream, Options) {
    let mut s = build(&spec);
    f(&spec, &mut s);
    (s, opts(&spec))
}

/// Packets of `b` replace those of `a` from time `t` on (both built from specs with the same
/// schedule, so continuity counters agree).
fn splice(spec_a: &Spec, spec_b: &Spec, t: f64) -> Stream {
    let a = build(spec_a);
    let b = build(spec_b);
    let k = a.at(spec_a, t);
    let mut packets = a.packets[..k].to_vec();
    packets.extend_from_slice(&b.packets[k..]);
    Stream { packets }
}

/// A hook body: appends descriptor `d` to the program_info loop of every PMT section, fixing
/// program_info_length and section_length.
fn pmt_info(s: &mut Vec<u8>, d: &[u8]) {
    if s[0] != 0x02 {
        return;
    }
    let pil = (usize::from(s[10] & 0x0F) << 8) | usize::from(s[11]);
    let at = 12 + pil;
    s.splice(at..at, d.iter().copied());
    let pil = pil + d.len();
    s[10] = (s[10] & 0xF0) | (pil >> 8) as u8;
    s[11] = pil as u8;
    let sl = ((usize::from(s[1] & 0x0F) << 8) | usize::from(s[2])) + d.len();
    s[1] = (s[1] & 0xF0) | (sl >> 8) as u8;
    s[2] = sl as u8;
}

fn first_of(s: &Stream, pid: u16, from: usize) -> usize {
    s.of_pid(pid).into_iter().find(|&i| i >= from).unwrap()
}

/// Rebuilds packet `i` with an adaptation field of `af_len` bytes (flags 0) in front of
/// its payload, which loses its last bytes.
fn add_af(s: &mut Stream, i: usize, af_len: usize) {
    let p = s.packets[i];
    let mut q = [0xFFu8; 188];
    q[..4].copy_from_slice(&p[..4]);
    q[3] = (q[3] & 0xCF) | 0x30;
    q[4] = af_len as u8;
    if af_len > 0 {
        q[5] = 0;
    }
    let off = 5 + af_len;
    q[off..].copy_from_slice(&p[4..4 + 188 - off]);
    s.packets[i] = q;
}

/// Byte offset of the PES header in a PUSI packet.
fn pes_off(p: &[u8; 188]) -> usize {
    if p[3] & 0x20 != 0 {
        5 + usize::from(p[4])
    } else {
        4
    }
}

/// The number of bytes of a section that starts in packet `i` (pointer 0) left for the
/// next packet.
fn spill(s: &Stream, i: usize) -> usize {
    let p = &s.packets[i];
    let len = 3 + usize::from(u16::from_be_bytes([p[6], p[7]]) & 0x0FFF);
    len - 183
}

/// An ETT whose text needs two packets.
fn two_packet_ett() -> Spec {
    Spec {
        ett_text: "x".repeat(250),
        ..Spec::default()
    }
}

fn extra_audio(desc: Vec<u8>) -> Extra {
    Extra {
        pid: 0x0035,
        stream_type: 0x81,
        desc,
        flood: None,
    }
}

static CASES: &[Case] = &[
    // ------------------------------------------------------------------ packet layer
    Case {
        name: "sync byte corrupted once",
        rules: &["TS-SYNC-BYTE"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.null_after(1000);
                s.packets[i][0] = 0x46;
            })
        },
    },
    Case {
        name: "two sync bytes corrupted: sync lost",
        rules: &["TS-SYNC-BYTE", "TS-SYNC-LOSS"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = (1000..)
                    .find(|&i| s.pid(i) == 0x1FFF && s.pid(i + 1) == 0x1FFF)
                    .unwrap();
                s.packets[i][0] = 0x00;
                s.packets[i + 1][0] = 0x00;
            })
        },
    },
    Case {
        name: "transport_error_indicator",
        rules: &["TS-TEI"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.null_after(1000);
                s.packets[i][1] |= 0x80;
            })
        },
    },
    Case {
        // The PCR in that packet is lost: two PCR intervals become 50 ms.
        name: "adaptation_field_control 00",
        rules: &["TS-AFC-RESERVED", "PCR-INTERVAL-40", "PCR-INTERVAL-TARGET"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.pcr_packets(0x31)[40];
                s.packets[i][3] &= 0xCF;
            })
        },
    },
    Case {
        name: "adaptation_field_length 182 in an adaptation-only packet",
        rules: &["TS-AF-LENGTH"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.pcr_packets(0x31)[40];
                s.packets[i][4] = 182;
            })
        },
    },
    Case {
        name: "OPCR without PCR",
        rules: &["TS-OPCR"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s
                    .of_pid(0x31)
                    .into_iter()
                    .find(|&i| s.packets[i][3] & 0x30 == 0x30 && s.packets[i][4] >= 7)
                    .unwrap();
                s.packets[i][5] = 0x08;
            })
        },
    },
    Case {
        name: "null packet with PUSI",
        rules: &["TS-NULL"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.null_after(1000);
                s.packets[i][1] |= 0x40;
            })
        },
    },
    Case {
        name: "reserved PID 0x0005",
        rules: &["TS-PID-RESERVED"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.null_after(1000);
                s.packets[i][1] = 0x00;
                s.packets[i][2] = 0x05;
            })
        },
    },
    Case {
        name: "PCR on reserved PID 0x0005",
        rules: &["TS-PID-RESERVED", "TS-PCR-PID"],
        make: || {
            with(Spec::default(), |spec, s| {
                let i = s.null_after(1000);
                s.packets[i][1] = 0x00;
                s.packets[i][2] = 0x05;
                s.packets[i][3] = 0x20;
                s.packets[i][4] = 183;
                s.packets[i][5] = 0x10;
                put_pcr(&mut s.packets[i][6..12], pcr_at(spec, i));
            })
        },
    },
    Case {
        name: "discontinuity_indicator on the PCR PID",
        rules: &["TS-DISCONTINUITY"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.pcr_packets(0x31)[40];
                s.packets[i][5] |= 0x80;
            })
        },
    },
    Case {
        name: "packets on DVB SDT PID 0x0011",
        rules: &["TS-DVB-PID"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.null_after(1000);
                s.packets[i][1] = 0x00;
                s.packets[i][2] = 0x11;
            })
        },
    },
    Case {
        name: "audio packet lost",
        rules: &["CC-ERROR"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x34)[202];
                s.null(i);
            })
        },
    },
    Case {
        name: "video packet sent three times",
        rules: &["CC-DUPLICATE"],
        make: || {
            with(Spec::default(), |_, s| {
                let v = s.of_pid(0x31);
                let i = (500..)
                    .map(|k| v[k])
                    .find(|&i| s.packets[i][3] & 0x30 == 0x10)
                    .unwrap();
                let a = s.null_after(i);
                let b = s.null_after(a + 1);
                assert!(first_of(s, 0x31, i + 1) > b);
                s.packets[a] = s.packets[i];
                s.packets[b] = s.packets[i];
            })
        },
    },
    Case {
        name: "duplicate packet that differs",
        rules: &["CC-DUP-CONTENT"],
        make: || {
            with(Spec::default(), |_, s| {
                let v = s.of_pid(0x31);
                let i = (500..)
                    .map(|k| v[k])
                    .find(|&i| s.packets[i][3] & 0x30 == 0x10)
                    .unwrap();
                let a = s.null_after(i);
                assert!(first_of(s, 0x31, i + 1) > a);
                s.packets[a] = s.packets[i];
                s.packets[a][100] ^= 1;
            })
        },
    },
    Case {
        name: "audio PID 0x1FF0 outside the SCTE range",
        rules: &["PID-RANGE"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_pid = Some(0x1FF0);
            std(&spec)
        },
    },
    // ------------------------------------------------------------------ sections
    Case {
        name: "CRC error in an ETT",
        rules: &["SECT-CRC"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x1E00)[2];
                s.packets[i][20] ^= 0x10;
            })
        },
    },
    Case {
        name: "pointer_field beyond the payload",
        rules: &["SECT-POINTER"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x1E00)[2];
                s.packets[i][4] = 200;
            })
        },
    },
    Case {
        name: "section starts in a packet without PUSI",
        rules: &["SECT-PUSI"],
        make: || {
            with(two_packet_ett(), |_, s| {
                let first = s.of_pid(0x1E00)[2];
                let i = s.of_pid(0x1E00)[3];
                assert!(s.packets[i][1] & 0x40 == 0);
                let k = 4 + spill(s, first);
                s.packets[i][k] = 0xCC;
            })
        },
    },
    Case {
        name: "stuffing then data",
        rules: &["SECT-STUFFING"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x1E00)[2];
                s.packets[i][187] = 0x12;
            })
        },
    },
    Case {
        name: "section cut short by a new start",
        rules: &["SECT-INCOMPLETE"],
        make: || {
            with(two_packet_ett(), |_, s| {
                let first = s.of_pid(0x1E00)[2];
                let i = s.of_pid(0x1E00)[3];
                let p = s.packets[i];
                let tail = spill(s, first);
                let mut q = [0xFFu8; 188];
                q[..4].copy_from_slice(&p[..4]);
                q[1] |= 0x40;
                q[4] = (tail - 1) as u8;
                q[5..5 + tail - 1].copy_from_slice(&p[4..4 + tail - 1]);
                s.packets[i] = q;
            })
        },
    },
    Case {
        name: "section_length 4095",
        rules: &["SECT-LENGTH"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x1E00)[2];
                s.packets[i][6] |= 0x0F;
                s.packets[i][7] = 0xFF;
            })
        },
    },
    Case {
        name: "PMT with the bit after section_syntax_indicator set",
        rules: &["SECT-SYNTAX"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s[1] |= 0x40;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "PMT with last_section_number 1",
        rules: &["SECT-SINGLE"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s[7] = 1;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "PAT last_section_number changes within a version",
        rules: &["SECT-LAST-SECTION"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0)[10];
                let o = sec_off(&s.packets[i]);
                s.packets[i][o + 7] = 1;
                fix_crc(&mut s.packets[i]);
            })
        },
    },
    Case {
        // The content is the same, so the change is also a no-op.
        name: "PMT version 0 then 2",
        rules: &["SECT-VERSION-STEP", "SECT-VERSION-NOOP"],
        make: || {
            let a = Spec::default();
            let b = Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s[5] = 0xC1 | (2 << 1);
                    }
                }),
                ..Spec::default()
            };
            (splice(&a, &b, 1.5), opts(&a))
        },
    },
    Case {
        name: "PMT content changes without a new version",
        rules: &["SECT-VERSION-STALE"],
        make: || {
            let a = Spec::default();
            let mut b = Spec::default();
            b.programs[0].info_desc.extend_from_slice(&[0x80, 0x00]);
            (splice(&a, &b, 1.5), opts(&a))
        },
    },
    Case {
        name: "PMT version changes, content identical",
        rules: &["SECT-VERSION-NOOP"],
        make: || {
            let a = Spec::default();
            let b = Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s[5] = 0xC1 | (1 << 1);
                    }
                }),
                ..Spec::default()
            };
            (splice(&a, &b, 1.5), opts(&a))
        },
    },
    Case {
        name: "STT with current_next_indicator 0",
        rules: &["SECT-CURRENT-NEXT"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCD {
                        s[5] &= 0xFE;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "a PMT on PID 0x1FFB",
        rules: &["SECT-TABLE-PID"],
        make: || {
            with(
                Spec {
                    stt_period: 0.25,
                    ..Spec::default()
                },
                |_, s| {
                    let i = s
                        .of_pid(0x1FFB)
                        .into_iter()
                        .find(|&i| s.packets[i][5] == 0xCD && i > 3000)
                        .unwrap();
                    s.packets[i][5] = 0x02;
                    fix_crc(&mut s.packets[i]);
                },
            )
        },
    },
    // ------------------------------------------------------------------ PSI
    Case {
        // The PAT packets are removed, so their continuity counter jumps.
        name: "no PAT for 0.6 s",
        rules: &["PAT-INTERVAL-TR", "PAT-INTERVAL", "CC-ERROR"],
        make: || {
            with(Spec::default(), |spec, s| {
                let (a, b) = (s.at(spec, 1.0), s.at(spec, 1.6));
                for i in s.of_pid(0) {
                    if i >= a && i < b {
                        s.null(i);
                    }
                }
            })
        },
    },
    Case {
        name: "PAT every 120 ms",
        rules: &["PAT-INTERVAL"],
        make: || {
            std(&Spec {
                pat_period: 0.12,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "table_id 0x02 on PID 0",
        rules: &["PAT-TABLE-ID"],
        make: || {
            with(
                Spec {
                    pat_period: 0.04,
                    ..Spec::default()
                },
                |_, s| {
                    let i = s.of_pid(0)[20];
                    s.packets[i][5] = 0x02;
                    fix_crc(&mut s.packets[i]);
                },
            )
        },
    },
    Case {
        name: "scrambled PAT packet",
        rules: &["PAT-SCRAMBLED"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0)[10];
                s.packets[i][3] |= 0x80;
            })
        },
    },
    Case {
        name: "program listed twice in the PAT",
        rules: &["PAT-PROGRAM-DUP"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x00 {
                        s.extend_from_slice(&[0x00, 0x01, 0xE0, 0x30]);
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "network PID in the PAT",
        rules: &["PAT-NIT"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x00 {
                        s.extend_from_slice(&[0x00, 0x00, 0xE0, 0x10]);
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "two programs on one PMT PID",
        rules: &["PAT-PMT-PID-SHARED"],
        make: || {
            let mut spec = Spec::default();
            let mut p2 = Prog::new(2);
            p2.pmt_pid = 0x30;
            spec.programs.push(p2);
            std(&spec)
        },
    },
    Case {
        name: "TSID other than configured",
        rules: &["PAT-TSID"],
        make: || {
            let spec = Spec::default();
            let mut o = opts(&spec);
            o.tsid = Some(0x0202);
            (build(&spec), o)
        },
    },
    Case {
        name: "adaptation field on the PAT PID",
        rules: &["PSI-AF"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0)[10];
                add_af(s, i, 1);
            })
        },
    },
    Case {
        name: "no PMT for 0.6 s",
        rules: &["PMT-INTERVAL-TR", "PMT-INTERVAL", "CC-ERROR"],
        make: || {
            with(Spec::default(), |spec, s| {
                let (a, b) = (s.at(spec, 1.0), s.at(spec, 1.6));
                for i in s.of_pid(0x30) {
                    if i >= a && i < b {
                        s.null(i);
                    }
                }
            })
        },
    },
    Case {
        name: "PMT every 450 ms",
        rules: &["PMT-INTERVAL"],
        make: || {
            std(&Spec {
                pmt_period: 0.45,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "scrambled PMT packet",
        rules: &["PMT-SCRAMBLED"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x30)[10];
                s.packets[i][3] |= 0xC0;
            })
        },
    },
    Case {
        // The program's own PMT never arrives: its elementary PIDs are unreferenced, its
        // PCR and buffers unknown, and the VCT's program has no PMT to compare with.
        name: "PMT PID carries the PMT of another program",
        rules: &[
            "PMT-PROGRAM",
            "PMT-INTERVAL-TR",
            "PMT-INTERVAL",
            "PID-UNREFERENCED",
        ],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s[4] = 7;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "user-private stream_type without registration",
        rules: &["PMT-PRIVATE-REG"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_type = 0xC5;
            std(&spec)
        },
    },
    Case {
        name: "stream_type 0x03 (MPEG-1 audio)",
        rules: &["PMT-STREAM-TYPE"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_type = 0x03;
            std(&spec)
        },
    },
    Case {
        // The second video stream has no packets (so no PTS either) and no alignment
        // descriptor, and the VCT's service location does not list it.
        name: "two video streams",
        rules: &[
            "PMT-VIDEO-COUNT",
            "PMT-DSA",
            "PID-MISSING",
            "PTS-REPETITION",
            "VCT-SLD",
        ],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s.extend_from_slice(&[0x02, 0xE0, 0x39, 0xF0, 0x00]);
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        // bsmod 2 in the descriptor differs from the stream's 0.
        name: "no complete main audio",
        rules: &["PMT-AUDIO-CM", "DESC-AC3-STREAM"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc = ac3_desc_with(2, 0, 0, Some(b"eng"));
            std(&spec)
        },
    },
    Case {
        name: "no data_stream_alignment_descriptor",
        rules: &["PMT-DSA"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].video_desc.clear();
            std(&spec)
        },
    },
    Case {
        name: "no AC-3 descriptor",
        rules: &["PMT-AUDIO-DESC"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc = iso639(b"eng");
            std(&spec)
        },
    },
    Case {
        name: "PCR_PID without PCRs",
        rules: &["PCR-MISSING"],
        make: || {
            std(&Spec {
                pcrs: false,
                ..Spec::default()
            })
        },
    },
    // ------------------------------------------------------------------ descriptors
    Case {
        // The loop is not interpreted further.
        name: "descriptor overruns program_info",
        rules: &["DESC-LOOP"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].info_desc = vec![0x05, 0x08, b'G', b'A', b'9', b'4'];
            std(&spec)
        },
    },
    Case {
        name: "two alignment descriptors",
        rules: &["DESC-DUP"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].video_desc = vec![0x06, 0x01, 0x02, 0x06, 0x01, 0x02];
            std(&spec)
        },
    },
    Case {
        name: "two registration descriptors",
        rules: &["DESC-REG-DUP"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].info_desc = [b"\x05\x04GA94".as_slice(), b"\x05\x04SCTE"].concat();
            std(&spec)
        },
    },
    Case {
        name: "no GA94",
        rules: &["DESC-GA94"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].info_desc.clear();
            std(&spec)
        },
    },
    Case {
        name: "AC-3 descriptor langcod 0x00",
        rules: &["DESC-AC3"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc[5] = 0x00;
            std(&spec)
        },
    },
    Case {
        // 640 kbit/s is also not the stream's 192.
        name: "AC-3 descriptor bit_rate_code 640 kbit/s",
        rules: &["DESC-AC3-ATSC", "DESC-AC3-STREAM"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc[3] = 18 << 2;
            std(&spec)
        },
    },
    Case {
        name: "AC-3 descriptor bsid 6",
        rules: &["DESC-AC3-STREAM"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc[2] = 0x06;
            std(&spec)
        },
    },
    Case {
        name: "two main services, one without language",
        rules: &["DESC-AC3-LANGUAGE"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].extra.push(extra_audio(
                [ac3_desc_with(0, 1, 2, None), iso639(b"spa")].concat(),
            ));
            std(&spec)
        },
    },
    Case {
        name: "two primary audio streams",
        rules: &["DESC-AC3-PRIMARY"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].extra.push(extra_audio(
                [ac3_desc_with(0, 1, 1, Some(b"spa")), iso639(b"spa")].concat(),
            ));
            std(&spec)
        },
    },
    Case {
        name: "two main services with mainid 0",
        rules: &["DESC-AC3-MAINID"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].extra.push(extra_audio(
                [ac3_desc_with(0, 0, 2, Some(b"spa")), iso639(b"spa")].concat(),
            ));
            std(&spec)
        },
    },
    Case {
        name: "ISO 639 descriptor of length 3",
        rules: &["DESC-ISO639"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc = [ac3_desc(), vec![0x0A, 3, b'e', b'n', b'g']].concat();
            std(&spec)
        },
    },
    Case {
        name: "ISO 639 language differs from the AC-3 descriptor's",
        rules: &["DESC-ISO639-ATSC"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_desc = [ac3_desc(), iso639(b"spa")].concat();
            std(&spec)
        },
    },
    Case {
        name: "two English main services without component names",
        rules: &["DESC-COMPONENT-NAME"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].extra.push(extra_audio(
                [ac3_desc_with(0, 1, 2, Some(b"eng")), iso639(b"eng")].concat(),
            ));
            std(&spec)
        },
    },
    Case {
        name: "caption_service_descriptor of the wrong length",
        rules: &["DESC-CAPTION"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0]
                .video_desc
                .extend_from_slice(&[0x86, 0x04, 0xE1, b'e', b'n', b'g']);
            std(&spec)
        },
    },
    // ------------------------------------------------------------------ PES
    Case {
        name: "audio PES without start code",
        rules: &["PES-START"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 2] = 0x02;
            })
        },
    },
    Case {
        // Without PTS_DTS_flags the video PES has no PTS.
        name: "PTS_DTS_flags 01",
        rules: &["PES-HEADER", "PES-VIDEO-PTS"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x31)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 7] = 0x40;
            })
        },
    },
    Case {
        name: "PTS marker bit 0",
        rules: &["PES-TS-MARKER"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 13] &= 0xFE;
            })
        },
    },
    Case {
        name: "audio PES_packet_length one too many",
        rules: &["PES-LENGTH"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 5] += 1;
            })
        },
    },
    Case {
        name: "audio PES_packet_length 0",
        rules: &["PES-LENGTH-ZERO"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 4] = 0;
                s.packets[i][o + 5] = 0;
            })
        },
    },
    Case {
        name: "video PES_packet_length not 0",
        rules: &["PES-LENGTH-VIDEO"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x31)[30];
                let o = pes_off(&s.packets[i]);
                let len = 6000u16 + 8;
                s.packets[i][o + 4..o + 6].copy_from_slice(&len.to_be_bytes());
            })
        },
    },
    Case {
        name: "AC-3 in stream_id 0xC0",
        rules: &["PES-STREAM-ID"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 3] = 0xC0;
            })
        },
    },
    Case {
        name: "PES_scrambling_control 01",
        rules: &["PES-FLAGS"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 6] |= 0x10;
            })
        },
    },
    Case {
        name: "video PES without PTS",
        rules: &["PES-VIDEO-PTS"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x31)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 7] = 0x00;
            })
        },
    },
    Case {
        name: "video PES with data_alignment_indicator 0",
        rules: &["PES-VIDEO-ALIGN"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x31)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 6] &= !0x04;
            })
        },
    },
    Case {
        name: "DTS equal to PTS",
        rules: &["PES-DTS-REDUNDANT"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].video_dts = Some(0);
            std(&spec)
        },
    },
    Case {
        name: "DTS after PTS",
        rules: &["PES-DTS-ORDER"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].video_dts = Some(1500);
            std(&spec)
        },
    },
    Case {
        name: "audio PES with a PTS but no syncframe start",
        rules: &["PES-PTS-NO-AU"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_split = true;
            std(&spec)
        },
    },
    Case {
        name: "audio PTS every 960 ms",
        rules: &["PTS-REPETITION"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_pts_every = 30;
            std(&spec)
        },
    },
    // ------------------------------------------------------------------ PCR and rate
    Case {
        // 110 ms is also outside the 0 to 100 ms of TR 101 290 2.3b.
        name: "PCR every 110 ms",
        rules: &[
            "PCR-REPETITION",
            "PCR-INTERVAL-40",
            "PCR-INTERVAL-TARGET",
            "PCR-DISCONTINUITY",
        ],
        make: || {
            std(&Spec {
                pcr_period: 0.11,
                ..Spec::default()
            })
        },
    },
    Case {
        // Reported twice (the jump and the jump back); the accuracy is measured on each
        // side of a discontinuity, so the lone PCR between is not.
        name: "one PCR 200 ms ahead",
        rules: &["PCR-DISCONTINUITY"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.pcr_packets(0x31)[60];
                let pcr = get_pcr(&s.packets[i][6..12]) + 27_000_000 / 5;
                put_pcr(&mut s.packets[i][6..12], pcr);
            })
        },
    },
    Case {
        name: "one PCR 1 us off",
        rules: &["PCR-ACCURACY", "PCR-ACCURACY-NOMINAL"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.pcr_packets(0x31)[60];
                let pcr = get_pcr(&s.packets[i][6..12]) + 27;
                put_pcr(&mut s.packets[i][6..12], pcr);
            })
        },
    },
    Case {
        name: "nominal rate 50 ppm off the PCRs",
        rules: &["PCR-ACCURACY-NOMINAL", "TS-RATE"],
        make: || {
            let spec = Spec::default();
            let mut o = opts(&spec);
            o.rate = Some(spec.rate as f64 * (1.0 + 50e-6));
            (build(&spec), o)
        },
    },
    Case {
        name: "PCR every 45 ms",
        rules: &["PCR-INTERVAL-40", "PCR-INTERVAL-TARGET"],
        make: || {
            std(&Spec {
                pcr_period: 0.045,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "PCR target 20 ms",
        rules: &["PCR-INTERVAL-TARGET"],
        make: || {
            let spec = Spec::default();
            let mut o = opts(&spec);
            o.pcr_target_ms = 20.0;
            (build(&spec), o)
        },
    },
    Case {
        name: "variable rate",
        rules: &["TS-VBR"],
        make: || {
            let spec = Spec {
                vbr_warp: 0.001,
                ..Spec::default()
            };
            let mut o = opts(&spec);
            o.rate = None;
            (build(&spec), o)
        },
    },
    Case {
        name: "no PCR, no rate",
        rules: &["TIME-BASE"],
        make: || {
            let spec = Spec {
                pcrs: false,
                ..Spec::default()
            };
            let mut o = opts(&spec);
            o.rate = None;
            (build(&spec), o)
        },
    },
    // ------------------------------------------------------------------ PSIP
    Case {
        name: "adaptation field on an ETT packet",
        rules: &["PSIP-AF"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x1E00)[2];
                add_af(s, i, 0);
            })
        },
    },
    Case {
        name: "scrambled ETT packet",
        rules: &["PSIP-SCRAMBLED"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x1E00)[2];
                s.packets[i][3] |= 0x80;
            })
        },
    },
    Case {
        name: "MGT after a pointer_field of 1",
        rules: &["MGT-PACKET-START"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s
                    .of_pid(0x1FFB)
                    .into_iter()
                    .find(|&i| s.packets[i][5] == 0xC7 && i > 3000)
                    .unwrap();
                let p = s.packets[i];
                let mut q = p;
                q[4] = 1;
                q[5] = 0x00;
                q[6..].copy_from_slice(&p[5..187]);
                s.packets[i] = q;
            })
        },
    },
    Case {
        name: "ETT protocol_version 1",
        rules: &["PSIP-PROTOCOL-VERSION"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCC {
                        s[8] = 1;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "no PSIP",
        rules: &["PSIP-REQUIRED"],
        make: || {
            std(&Spec {
                psip: false,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "MGT every 200 ms",
        rules: &["MGT-INTERVAL"],
        make: || {
            std(&Spec {
                mgt_period: 0.2,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "CVCT every 450 ms",
        rules: &["VCT-INTERVAL"],
        make: || {
            std(&Spec {
                vct_period: 0.45,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "STT every 11 s",
        rules: &["STT-INTERVAL", "STT-INTERVAL-1S"],
        make: || {
            std(&Spec {
                stt_period: 11.0,
                secs: 11.5,
                rate: 3_000_000,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "STT every 1.5 s",
        rules: &["STT-INTERVAL-1S"],
        make: || {
            std(&Spec {
                stt_period: 1.5,
                ..Spec::default()
            })
        },
    },
    Case {
        // The RRT is not listed in the MGT either, and the MGT's EIT-0 PID carries nothing
        // (there is no program).
        name: "RRT once in 61 s",
        rules: &["RRT-INTERVAL", "MGT-UNLISTED", "PID-MISSING"],
        make: || {
            std(&Spec {
                rrt_once: true,
                programs: Vec::new(),
                secs: 61.0,
                rate: 400_000,
                ett: false,
                events0: Vec::new(),
                ..Spec::default()
            })
        },
    },
    Case {
        // The RRT of the synthetic stream is not listed in its MGT.
        name: "RRT for rating_region 0",
        rules: &["RRT-SYNTAX", "MGT-UNLISTED"],
        make: || {
            std(&Spec {
                rrt_once: true,
                hook: Some(|s| {
                    if s[0] == 0xCA {
                        s[4] = 0;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "content advisory with no region",
        rules: &["ADVISORY-SYNTAX"],
        make: || {
            std(&Spec {
                hook: Some(|s| pmt_info(s, &[0x87, 1, 0xC0])),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "content advisory in region 16 without its RRT",
        rules: &["ADVISORY-NO-RRT"],
        make: || {
            std(&Spec {
                // region 16, no dimension, no description
                hook: Some(|s| pmt_info(s, &[0x87, 4, 0xC1, 0x10, 0, 0])),
                ..Spec::default()
            })
        },
    },
    Case {
        // The synthetic RRT (region 5) defines value 0 only; the advisory uses value 1. The
        // RRT is not listed in the MGT.
        name: "content advisory with a value its RRT does not define",
        rules: &["ADVISORY-UNDEFINED", "MGT-UNLISTED"],
        make: || {
            std(&Spec {
                rrt_once: true,
                hook: Some(|s| pmt_info(s, &[0x87, 6, 0xC1, 0x05, 1, 0, 0xF1, 0])),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "EIT-0 every 600 ms",
        rules: &["EIT0-INTERVAL"],
        make: || {
            std(&Spec {
                eit_period: 0.6,
                ..Spec::default()
            })
        },
    },
    Case {
        // 300 kbit/s also overflows the smoothing buffer that leaks 250 kbit/s.
        name: "ETT PID at 300 kbit/s",
        rules: &["PSIP-RATE", "PSIP-SB-OVERFLOW"],
        make: || {
            std(&Spec {
                ett_period: 0.005,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "STT version 1",
        rules: &["STT-HEADER"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCD {
                        s[5] = 0xC3;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "STT not advancing",
        rules: &["STT-TIME"],
        make: || {
            std(&Spec {
                stt_frozen: true,
                secs: 4.0,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "GPS_UTC_offset 17",
        rules: &["STT-GPS-UTC"],
        make: || {
            std(&Spec {
                gps_utc: 17,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "DS_hour 20",
        rules: &["STT-DS"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCD {
                        s[14] = 0x65;
                        s[15] = 20;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "MGT table_id_extension 1",
        rules: &["MGT-HEADER"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC7 {
                        s[4] = 1;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        // The ETT is no longer an ETT of the MGT: its PID is unreferenced and the event's
        // ETM has no ETT to be in.
        name: "MGT table_type 0x0180 (reserved)",
        rules: &["MGT-TABLE-TYPE", "PID-UNREFERENCED", "ETM-MISSING"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC7 {
                        s[11 + 22] = 0x01;
                        s[12 + 22] = 0x80;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "MGT says CVCT version 1",
        rules: &["MGT-VERSION"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC7 {
                        s[15] = 0xE1;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "MGT number_bytes 999",
        rules: &["MGT-NUMBER-BYTES"],
        make: || {
            std(&Spec {
                mgt_bytes: Some(999),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "CVCT not in the MGT",
        rules: &["MGT-UNLISTED"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC7 {
                        s[10] = 2;
                        s.drain(11..22);
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "CVCT transport_stream_id other than the PAT's",
        rules: &["VCT-TSID"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC9 {
                        s[3] = 0x02;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "channel 1005.1",
        rules: &["VCT-NUMBER"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].major = 1005;
            std(&spec)
        },
    },
    Case {
        name: "two channels 50.1",
        rules: &["VCT-NUMBER-DUP"],
        make: || {
            let mut spec = Spec::default();
            let mut p2 = Prog::new(2);
            p2.minor = 1;
            spec.programs.push(p2);
            std(&spec)
        },
    },
    Case {
        name: "empty short_name",
        rules: &["VCT-SHORT-NAME"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].name = "";
            std(&spec)
        },
    },
    Case {
        name: "carrier_frequency not 0",
        rules: &["VCT-CARRIER-FREQ"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC9 {
                        s[28] = 1;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "service_type 0",
        rules: &["VCT-FIELDS"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC9 {
                        s[37] = 0xC0;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        // The two programs' EIT and ETT become one instance each, so the MGT's byte counts
        // (made for two) are wrong.
        name: "two programs with source_id 1",
        rules: &["VCT-SOURCE-ID-DUP", "MGT-NUMBER-BYTES"],
        make: || {
            let mut spec = Spec::default();
            let mut p2 = Prog::new(2);
            p2.source_id = 1;
            spec.programs.push(p2);
            spec.eit_skip_source = None;
            let mut s = build(&spec);
            // Both programs' EIT and ETT instances are now one instance each: keep the
            // MGT's byte counts right by sending each table once per cycle.
            let _ = &mut s;
            (s, opts(&spec))
        },
    },
    Case {
        name: "event in EIT-0 three windows ahead",
        rules: &["EIT-WINDOW"],
        make: || {
            let mut spec = Spec::default();
            spec.events0[0].start += 3 * 10_800;
            std(&spec)
        },
    },
    Case {
        // Reported once by the section parser and once by the instance check.
        name: "overlapping events",
        rules: &["EIT-ORDER"],
        make: || {
            let mut spec = Spec::default();
            spec.events0[0].len = 6000;
            spec.events0.push(Ev {
                id: 2,
                start: 5000,
                len: 1000,
                etm: 0,
            });
            std(&spec)
        },
    },
    Case {
        // The event's ETM is then missing.
        name: "event ETM_id ending in 01",
        rules: &["ETT-ETM-ID", "ETM-MISSING"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCC {
                        s[12] = (s[12] & 0xFC) | 1;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "two ETMs with ETT_table_id_extension 0",
        rules: &["ETT-TID-EXT"],
        make: || {
            let mut spec = Spec::default();
            spec.programs.push(Prog::new(2));
            spec.hook = Some(|s| {
                if s[0] == 0xCC {
                    s[3] = 0;
                    s[4] = 0;
                }
            });
            std(&spec)
        },
    },
    Case {
        name: "string segment in reserved mode 0x07",
        rules: &["MSS-SYNTAX"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCC {
                        s[19] = 0x07;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "Huffman-compressed string",
        rules: &["MSS-HUFFMAN"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCC {
                        s[18] = 0x01;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    // ------------------------------------------------------------------ cross-checks
    Case {
        // Audio frames stop: no PTS for longer than 700 ms, and the frame cut in the
        // middle never completes in the buffer.
        name: "audio PID silent from 0.988 s, in a frame",
        rules: &["PID-MISSING", "PTS-REPETITION", "TSTD-B-UNDERFLOW"],
        make: || {
            with(Spec::default(), |spec, s| {
                let a = s.at(spec, 0.988);
                for i in s.of_pid(0x34) {
                    if i >= a {
                        s.null(i);
                    }
                }
            })
        },
    },
    Case {
        name: "packets on PID 0x0100",
        rules: &["PID-UNREFERENCED"],
        make: || {
            with(Spec::default(), |_, s| {
                let mut i = 1000;
                for k in 0..20u8 {
                    i = s.null_after(i + 100);
                    s.packets[i][1] = 0x01;
                    s.packets[i][2] = 0x00;
                    s.packets[i][3] = 0x10 | (k & 0x0F);
                }
            })
        },
    },
    Case {
        // The added stream has no AC-3 descriptor, is not in the service location, and
        // carries no PES with a PTS.
        name: "PMT PID listed as an elementary PID",
        rules: &[
            "PID-CONFLICT",
            "PMT-AUDIO-DESC",
            "VCT-SLD",
            "PTS-REPETITION",
        ],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0x02 {
                        s.extend_from_slice(&[0x81, 0xE0, 0x30, 0xF0, 0x00]);
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "scrambled video without CAT",
        rules: &["CAT-ERROR"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.of_pid(0x31)[400];
                s.packets[i][3] |= 0x80;
            })
        },
    },
    Case {
        name: "ETT on the EIT PID",
        rules: &["MGT-PID-TABLE"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCB {
                        s[0] = 0xCD;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        // Program 1 is then described by no channel.
        name: "channel names program 5",
        rules: &["VCT-PROGRAM", "VCT-COVERAGE"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC9 {
                        s[35] = 5;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        // The second program's EIT instance names a source no channel has.
        name: "program 2 without a channel",
        rules: &["VCT-COVERAGE", "EIT-SOURCE"],
        make: || {
            let mut spec = Spec::default();
            spec.programs.push(Prog::new(2));
            spec.hook = Some(|s| {
                if s[0] == 0xC9 {
                    s[9] = 1;
                    let ch1 = 10 + 32 + usize::from(u16::from_be_bytes([s[40], s[41]]) & 0x3FF);
                    let ch2 = ch1
                        + 32
                        + usize::from(u16::from_be_bytes([s[ch1 + 30], s[ch1 + 31]]) & 0x3FF);
                    s.drain(ch1..ch2);
                }
            });
            std(&spec)
        },
    },
    Case {
        // Program 1 then has no channel with this TSID.
        name: "channel names TSID 0x0202",
        rules: &["VCT-CHANNEL-TSID", "VCT-COVERAGE"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC9 {
                        s[32] = 0x02;
                        s[33] = 0x02;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "service location PCR_PID 0x0034",
        rules: &["VCT-SLD"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xC9 {
                        s[45] = 0x34;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    Case {
        name: "TVCT channel without service location",
        rules: &["VCT-SLD-MISSING"],
        make: || {
            let mut spec = Spec {
                tvct: true,
                ..Spec::default()
            };
            spec.programs[0].sld = false;
            std(&spec)
        },
    },
    Case {
        name: "no EIT-0 instance for program 2",
        rules: &["EIT-INSTANCE"],
        make: || {
            let mut spec = Spec::default();
            spec.programs.push(Prog::new(2));
            spec.eit_skip_source = Some(2);
            std(&spec)
        },
    },
    Case {
        name: "event spans EIT-0 and EIT-1 with two event_ids",
        rules: &["EIT-SPAN-ID"],
        make: || {
            let mut spec = Spec {
                eit1: true,
                ..Spec::default()
            };
            spec.events0 = vec![Ev {
                id: 1,
                start: 6000,
                len: 6000,
                etm: 1,
            }];
            spec.events1 = vec![Ev {
                id: 2,
                start: 6000 - 10_800,
                len: 6000,
                etm: 0,
            }];
            std(&spec)
        },
    },
    Case {
        name: "event with ETM_location 01 but no ETT",
        rules: &["ETM-MISSING"],
        make: || {
            std(&Spec {
                ett: false,
                ..Spec::default()
            })
        },
    },
    Case {
        name: "MGT lists EIT-0 for a source the VCT lacks",
        rules: &["EIT-SOURCE", "EIT-INSTANCE", "ETM-MISSING"],
        make: || {
            std(&Spec {
                hook: Some(|s| {
                    if s[0] == 0xCB {
                        s[4] = 9;
                    }
                }),
                ..Spec::default()
            })
        },
    },
    // ------------------------------------------------------------------ buffer model
    Case {
        name: "audio frame packets back to back",
        rules: &["TSTD-TB-OVERFLOW"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_spacing = 0.0;
            std(&spec)
        },
    },
    Case {
        name: "PMT of five packets every 400 ms",
        rules: &["TSTD-TBSYS-OVERFLOW"],
        make: || {
            let mut spec = Spec {
                pmt_period: 0.38,
                ..Spec::default()
            };
            for t in 0x90u8..0x94 {
                spec.programs[0].info_desc.push(t);
                spec.programs[0].info_desc.push(200);
                spec.programs[0].info_desc.extend_from_slice(&[0u8; 200]);
            }
            std(&spec)
        },
    },
    Case {
        // Five back-to-back packets also overflow TBsys.
        name: "PMT of five packets every 50 ms",
        rules: &["TSTD-BSYS-OVERFLOW", "TSTD-TBSYS-OVERFLOW"],
        make: || {
            let mut spec = Spec {
                pmt_period: 0.05,
                ..Spec::default()
            };
            for t in 0x90u8..0x94 {
                spec.programs[0].info_desc.push(t);
                spec.programs[0].info_desc.push(200);
                spec.programs[0].info_desc.extend_from_slice(&[0u8; 200]);
            }
            std(&spec)
        },
    },
    Case {
        // Two packets back to back, then one every 752 us (2 Mbit/s at 8 Mbit/s, exactly
        // Rx) for 1.1 s: TBn never empties and holds at most 282 bytes. The stream has one
        // PES (so one PTS) and no E-AC-3 descriptor.
        name: "E-AC-3 PID at Rx for 1.1 s",
        rules: &["TSTD-TB-EMPTY", "PTS-REPETITION", "PMT-AUDIO-DESC"],
        make: || {
            let mut spec = Spec {
                secs: 2.0,
                ..Spec::default()
            };
            spec.programs[0].extra.push(Extra {
                pid: 0x0036,
                stream_type: 0x87,
                desc: Vec::new(),
                flood: Some((4, 1.1)),
            });
            let mut s = build(&spec);
            let _ = &mut s;
            (s, opts(&spec))
        },
    },
    Case {
        // Two frames of 72 kB, each delivered over one frame period: 17.3 Mbit/s of payload
        // (17.9 Mbit/s of packets, which TBn with Rx 18 Mbit/s passes) into MBn leaking at
        // 15 Mbit/s: 19 kB stay, MBn is 10 kB.
        name: "two 72 kB frames at 17.3 Mbit/s",
        rules: &["TSTD-MB-OVERFLOW"],
        make: || {
            let mut spec = Spec {
                rate: 38_800_000,
                secs: 1.5,
                ..Spec::default()
            };
            spec.programs[0].big_frame = Some((16, 18, 72_000));
            spec.programs[0].video_first = true;
            spec.programs[0].video_lead = 0.2;
            std(&spec)
        },
    },
    Case {
        name: "EB full for longer than a second",
        rules: &["TSTD-MB-EMPTY"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].vbv = 20;
            std(&spec)
        },
    },
    Case {
        name: "video arriving late",
        rules: &["TSTD-EB-UNDERFLOW"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].video_lead = 0.02;
            std(&spec)
        },
    },
    Case {
        name: "audio 200 ms early",
        rules: &["TSTD-B-OVERFLOW"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_lead = 0.2;
            std(&spec)
        },
    },
    Case {
        name: "audio arriving late",
        rules: &["TSTD-B-UNDERFLOW"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].audio_lead = 0.002;
            std(&spec)
        },
    },
    Case {
        name: "video 1.1 s early",
        rules: &["TSTD-DELAY"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].video_lead = 1.1;
            std(&spec)
        },
    },
    Case {
        name: "ETT of seven packets back to back",
        rules: &["PSIP-SB-OVERFLOW"],
        make: || {
            std(&Spec {
                ett_text: "y".repeat(1200),
                ett_period: 1.0,
                ..Spec::default()
            })
        },
    },
    // ------------------------------------------------------------------ elementary streams
    Case {
        // The PES whose frame lost its syncword has a PTS but no syncframe begins in it.
        name: "AC-3 syncword corrupted",
        rules: &["ES-AC3-SYNC", "PES-PTS-NO-AU"],
        make: || {
            with(Spec::default(), |_, s| {
                let i = s.starts(0x34)[30];
                let o = pes_off(&s.packets[i]);
                s.packets[i][o + 14] = 0x0C;
            })
        },
    },
    Case {
        name: "video at High profile",
        rules: &["ES-VIDEO-SEQ"],
        make: || {
            let mut spec = Spec::default();
            spec.programs[0].profile_level = 0x14;
            std(&spec)
        },
    },
];

#[test]
fn clean_stream_is_clean() {
    let spec = Spec::default();
    let s = build(&spec);
    let r = run(&spec, &s);
    expect(&r, &[]);
    // Every model ran.
    let names: Vec<String> = r
        .stats
        .buffers
        .iter()
        .map(|b| format!("{} {:04X}", b.buffer, b.pid))
        .collect();
    for want in [
        "TB 0031",
        "MB 0031",
        "EB 0031",
        "TB 0034",
        "B 0034",
        "TBsys 0030",
        "Bsys 0030",
        "SB 1FFB",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} missing from {names:?}"
        );
    }
}

#[test]
fn two_program_stream_is_clean() {
    let mut spec = Spec::default();
    spec.programs.push(Prog::new(2));
    spec.eit1 = true;
    let s = build(&spec);
    let r = run(&spec, &s);
    expect(&r, &[]);
}

#[test]
fn cases() {
    let mut failures = Vec::new();
    for c in CASES {
        let (s, o) = (c.make)();
        let r = check(&s.bytes(), &o);
        // TB_CASE=<part of a case name> prints that case's report (for debugging).
        if std::env::var("TB_CASE").is_ok_and(|n| c.name.contains(&n)) {
            eprintln!("{}\n{}\n{}", c.name, r.summary(), r.tables_text());
            eprintln!("{}", r.details());
        }
        let mut got: Vec<&str> = r.counts.keys().map(|r| r.info().id).collect();
        got.sort_unstable();
        let mut want = c.rules.to_vec();
        want.sort_unstable();
        if got != want {
            failures.push(format!(
                "{}: want {want:?}, got {got:?}\n{}",
                c.name,
                r.details()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failing cases:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn coverage() {
    let mut missing = Vec::new();
    for r in ALL_RULES {
        let i = r.info();
        if i.check != Check::Yes {
            continue;
        }
        if !CASES.iter().any(|c| c.rules.contains(&i.id)) {
            missing.push(i.id);
        }
    }
    assert!(
        missing.is_empty(),
        "rules without a detecting case: {missing:?}"
    );
}
