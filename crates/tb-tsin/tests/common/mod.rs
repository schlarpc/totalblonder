//! A minimal TS writer for tests (PAT, PMT, PES with stuffing and continuity counters), and
//! RTP wrapping. Deliberately naive: it is a test fixture, not the workspace's mux.
#![allow(dead_code, reason = "each test binary uses a subset")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test support")]

use tb_tsin::psi::crc32_mpeg2;

pub struct Writer {
    pub cc: std::collections::HashMap<u16, u8>,
    pub out: Vec<u8>,
}

pub fn section(table_id: u8, id: u16, version: u8, body: &[u8]) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![table_id, 0xB0 | (len >> 8) as u8, len as u8];
    s.extend_from_slice(&id.to_be_bytes());
    s.push(0xC1 | (version << 1));
    s.extend_from_slice(&[0, 0]);
    s.extend_from_slice(body);
    let crc = crc32_mpeg2(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

fn ts_bytes(prefix: u8, t: u64) -> [u8; 5] {
    [
        prefix << 4 | ((t >> 29) as u8 & 0x0E) | 1,
        (t >> 22) as u8,
        ((t >> 14) as u8 & 0xFE) | 1,
        (t >> 7) as u8,
        ((t << 1) as u8) | 1,
    ]
}

/// One elementary stream of a PMT: type, PID, descriptor bytes.
pub struct Es {
    pub stream_type: u8,
    pub pid: u16,
    pub descriptors: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self {
            cc: Default::default(),
            out: Vec::new(),
        }
    }

    fn next_cc(&mut self, pid: u16) -> u8 {
        let c = self.cc.entry(pid).or_insert(15);
        *c = (*c + 1) & 15;
        *c
    }

    /// Payload bytes on `pid` as packets; the first has PUSI (and RAI if `rai`); the last is
    /// padded with adaptation-field stuffing.
    pub fn payload(&mut self, pid: u16, data: &[u8], pusi: bool, rai: bool) {
        let mut rest = data;
        let mut first = true;
        while !rest.is_empty() || first {
            let mut p = vec![0x47, (pid >> 8) as u8, pid as u8, 0];
            if first && pusi {
                p[1] |= 0x40;
            }
            let want_af = first && rai;
            let room = 184 - if want_af { 2 } else { 0 };
            let n = rest.len().min(room);
            let cc = self.next_cc(pid);
            if want_af || n < 184 {
                // Adaptation field: flags + stuffing to fill.
                let af_len = 183 - n;
                p[3] = 0x30 | cc;
                p.push(af_len as u8);
                if af_len > 0 {
                    p.push(if want_af { 0x40 } else { 0 });
                    p.extend(std::iter::repeat_n(0xFF, af_len - 1));
                }
            } else {
                p[3] = 0x10 | cc;
            }
            p.extend_from_slice(&rest[..n]);
            assert_eq!(p.len(), 188);
            self.out.extend_from_slice(&p);
            rest = &rest[n..];
            first = false;
        }
    }

    pub fn psi(&mut self, pid: u16, section: &[u8]) {
        let mut d = vec![0];
        d.extend_from_slice(section);
        self.payload(pid, &d, true, false);
    }

    pub fn pat(&mut self, programs: &[(u16, u16)], version: u8) {
        let mut body = Vec::new();
        for &(n, pid) in programs {
            body.extend_from_slice(&n.to_be_bytes());
            body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        }
        let s = section(0, 1, version, &body);
        self.psi(0, &s);
    }

    pub fn pmt(&mut self, pmt_pid: u16, program: u16, version: u8, pcr: u16, es: &[Es]) {
        let mut body = (0xE000 | pcr).to_be_bytes().to_vec();
        body.extend_from_slice(&[0xF0, 0]);
        for e in es {
            body.push(e.stream_type);
            body.extend_from_slice(&(0xE000 | e.pid).to_be_bytes());
            body.extend_from_slice(&(0xF000 | e.descriptors.len() as u16).to_be_bytes());
            body.extend_from_slice(&e.descriptors);
        }
        let s = section(2, program, version, &body);
        self.psi(pmt_pid, &s);
    }

    /// A PES with PTS (and DTS) on `pid`.
    pub fn pes(
        &mut self,
        pid: u16,
        stream_id: u8,
        pts: u64,
        dts: Option<u64>,
        data: &[u8],
        rai: bool,
    ) {
        let mut opt = Vec::new();
        let flags = if let Some(d) = dts {
            opt.extend_from_slice(&ts_bytes(3, pts));
            opt.extend_from_slice(&ts_bytes(1, d));
            0xC0
        } else {
            opt.extend_from_slice(&ts_bytes(2, pts));
            0x80
        };
        let len = 3 + opt.len() + data.len();
        let len = if (0xE0..=0xEF).contains(&stream_id) || len > 0xFFFF {
            0
        } else {
            len
        };
        let mut v = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8, 0x80, flags];
        v.push(opt.len() as u8);
        v.extend_from_slice(&opt);
        v.extend_from_slice(data);
        self.payload(pid, &v, true, rai);
    }
}

/// An H.264 access unit (Annex B) of `size` bytes: AUD, then an IDR or non-IDR slice.
pub fn h264_au(key: bool, size: usize, tag: u8) -> Vec<u8> {
    let mut v = vec![
        0,
        0,
        0,
        1,
        0x09,
        0xF0,
        0,
        0,
        1,
        if key { 0x65 } else { 0x41 },
    ];
    v.extend(std::iter::repeat_n(tag, size));
    v
}

/// A standard single-program stream: PAT, PMT (H.264 on 0x100, AC-3 "eng" on 0x101), then
/// `frames` video frames at 30 fps (key every 15) and an audio frame per video frame.
pub fn simple_stream(frames: u32, base: u64) -> Vec<u8> {
    let mut w = Writer::new();
    w.pat(&[(1, 0x1000)], 0);
    w.pmt(
        0x1000,
        1,
        0,
        0x100,
        &[
            Es {
                stream_type: 0x1B,
                pid: 0x100,
                descriptors: vec![],
            },
            Es {
                stream_type: 0x81,
                pid: 0x101,
                descriptors: vec![0x0A, 4, b'e', b'n', b'g', 0],
            },
        ],
    );
    for i in 0..frames {
        let t = base + u64::from(i) * 3000;
        let key = i % 15 == 0;
        if i % 10 == 0 {
            w.pat(&[(1, 0x1000)], 0);
        }
        w.pes(
            0x100,
            0xE0,
            (t + 6000) & ((1 << 33) - 1),
            Some(t & ((1 << 33) - 1)),
            &h264_au(key, 100 + (i as usize * 97) % 900, i as u8),
            key,
        );
        w.pes(
            0x101,
            0xBD,
            (t + 3000) & ((1 << 33) - 1),
            None,
            &[0x0B, 0x77, i as u8, 0, 0, 0],
            false,
        );
    }
    w.out
}

pub fn rtp(seq: u16, ssrc: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x80, 33];
    v.extend_from_slice(&seq.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&ssrc.to_be_bytes());
    v.extend_from_slice(payload);
    v
}
