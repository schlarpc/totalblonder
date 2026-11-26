//! A builder of small conformant cable transport streams for the rule tests: one or more
//! programs (MPEG-2 video and AC-3 audio with the ATSC descriptors), PAT and PMT, and PSIP
//! (MGT, CVCT, STT, EIT-0, ETT-0), scheduled on a constant-rate slot grid with null packets,
//! PCRs computed from the byte position. Written from the standards for the tests, sharing
//! nothing with the checker. Tests change the [`Spec`] or the built packets.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, VecDeque};

use tb_ts_verify::{Options, Report, Rule, Severity, check, crc32};

/// 27 MHz ticks of the first byte of the stream.
pub const PCR0: u64 = 27_000_000 * 1000;

/// An additional audio (or other) elementary stream of a program.
#[derive(Clone, Debug)]
pub struct Extra {
    pub pid: u16,
    pub stream_type: u8,
    pub desc: Vec<u8>,
    /// None: AC-3 frames like the main audio. Some((slots, secs)): one PES sent from 0.2 s
    /// for that many seconds, its first two packets back to back, then one packet every
    /// `slots` slots, ahead of every other packet.
    pub flood: Option<(usize, f64)>,
}

/// Description of one program.
#[derive(Clone, Debug)]
pub struct Prog {
    pub number: u16,
    pub pmt_pid: u16,
    pub video_pid: u16,
    pub audio_pid: Option<u16>,
    pub pcr_pid: u16,
    pub info_desc: Vec<u8>,
    pub video_desc: Vec<u8>,
    pub audio_desc: Vec<u8>,
    pub video_type: u8,
    pub audio_type: u8,
    pub frame_bytes: usize,
    /// Frames from..to (exclusive) have this many bytes instead.
    pub big_frame: Option<(usize, usize, usize)>,
    /// Video PTS minus the time its first byte is due, s.
    pub video_lead: f64,
    /// All packets of a frame due at once (sent back to back).
    pub video_burst: bool,
    /// Video packets go before every other packet except a flood (so on their due slot).
    pub video_first: bool,
    /// DTS = PTS + this many 90 kHz ticks, when Some.
    pub video_dts: Option<i64>,
    pub vbv: u32,
    pub profile_level: u8,
    pub audio_lead: f64,
    /// Time between the packets of one audio frame, s.
    pub audio_spacing: f64,
    /// A PTS on every n-th audio PES.
    pub audio_pts_every: usize,
    /// Each AC-3 frame in two PES packets, both with a PTS, the second not aligned.
    pub audio_split: bool,
    pub extra: Vec<Extra>,
    pub major: u16,
    pub minor: u16,
    pub source_id: u16,
    pub name: &'static str,
    pub sld: bool,
}

/// An AC-3 descriptor: 48 kHz, bsid 8, 192 kbit/s exact, the given bsmod, acmod 2/0, full
/// service, mainid and priority, and language when given.
pub fn ac3_desc_with(bsmod: u8, mainid: u8, priority: u8, lang: Option<&[u8; 3]>) -> Vec<u8> {
    let mut d = vec![0x81, 0, 0x08, 0x28, (bsmod << 5) | (2 << 1) | 1, 0xFF];
    if bsmod < 2 {
        d.push((mainid << 5) | (priority << 3) | 7);
    } else {
        d.push(0x01);
    }
    d.push(0x01);
    match lang {
        Some(l) => {
            d.push(0xBF);
            d.extend_from_slice(l);
        }
        None => d.push(0x3F),
    }
    d[1] = (d.len() - 2) as u8;
    d
}

/// The AC-3 descriptor of the default audio (main, primary, English).
pub fn ac3_desc() -> Vec<u8> {
    ac3_desc_with(0, 0, 1, Some(b"eng"))
}

pub fn iso639(lang: &[u8; 3]) -> Vec<u8> {
    vec![0x0A, 0x04, lang[0], lang[1], lang[2], 0x00]
}

impl Prog {
    pub fn new(number: u16) -> Self {
        let base = 0x0030 + 0x10 * (number - 1);
        Prog {
            number,
            pmt_pid: base,
            video_pid: base + 1,
            audio_pid: Some(base + 4),
            pcr_pid: base + 1,
            info_desc: vec![0x05, 0x04, b'G', b'A', b'9', b'4'],
            video_desc: vec![0x06, 0x01, 0x02],
            audio_desc: [ac3_desc(), iso639(b"eng")].concat(),
            video_type: 0x02,
            audio_type: 0x81,
            frame_bytes: 6000,
            big_frame: None,
            video_lead: 0.35,
            video_burst: false,
            video_first: false,
            video_dts: None,
            vbv: 112,
            profile_level: 0x48,
            audio_lead: 0.05,
            audio_spacing: 0.001,
            audio_pts_every: 1,
            audio_split: false,
            extra: Vec::new(),
            major: 50,
            minor: number,
            source_id: number,
            name: "TEST",
            sld: true,
        }
    }
}

/// One EIT event: id, start relative to the start of its window (s), length (s), ETM.
#[derive(Clone, Copy, Debug)]
pub struct Ev {
    pub id: u16,
    pub start: i64,
    pub len: u32,
    pub etm: u8,
}

/// Description of a whole stream.
#[derive(Clone, Debug)]
pub struct Spec {
    pub rate: u64,
    pub secs: f64,
    pub tsid: u16,
    pub programs: Vec<Prog>,
    pub pat_period: f64,
    pub pmt_period: f64,
    pub pcr_period: f64,
    pub pcrs: bool,
    /// PCRs off the straight line by a triangle wave of this many seconds (variable rate).
    pub vbr_warp: f64,
    pub psip: bool,
    pub tvct: bool,
    pub mgt_period: f64,
    pub vct_period: f64,
    pub stt_period: f64,
    pub stt_frozen: bool,
    pub eit_period: f64,
    pub ett_period: f64,
    pub ett: bool,
    pub ett_text: String,
    pub eit1: bool,
    pub events0: Vec<Ev>,
    pub events1: Vec<Ev>,
    pub eit_skip_source: Option<u16>,
    pub rrt_once: bool,
    /// GPS seconds at the first byte.
    pub gps0: u32,
    pub gps_utc: u8,
    pub eit_pid: u16,
    pub ett_pid: u16,
    pub version: u8,
    /// Edits a section (from table_id to just before CRC_32) before its CRC is computed.
    pub hook: Option<fn(&mut Vec<u8>)>,
    /// Overrides the MGT's number_bytes when Some.
    pub mgt_bytes: Option<u32>,
}

impl Default for Spec {
    fn default() -> Self {
        // A UTC time 5000 s into a 3-hour window.
        let utc = 140_000 * 10_800 + 5_000;
        Spec {
            rate: 8_000_000,
            secs: 3.0,
            tsid: 0x0101,
            programs: vec![Prog::new(1)],
            pat_period: 0.08,
            pmt_period: 0.08,
            pcr_period: 0.025,
            pcrs: true,
            vbr_warp: 0.0,
            psip: true,
            tvct: false,
            mgt_period: 0.1,
            vct_period: 0.2,
            stt_period: 0.5,
            stt_frozen: false,
            eit_period: 0.4,
            ett_period: 0.5,
            ett: true,
            ett_text: "Details".into(),
            eit1: false,
            events0: vec![Ev {
                id: 1,
                start: 0,
                len: 10_800,
                etm: 1,
            }],
            events1: Vec::new(),
            eit_skip_source: None,
            rrt_once: false,
            gps0: utc + 18,
            gps_utc: 18,
            eit_pid: 0x1D00,
            ett_pid: 0x1E00,
            version: 0,
            hook: None,
            mgt_bytes: None,
        }
    }
}

/// Writes bits most significant first.
#[derive(Default)]
pub struct Bits {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    pub fn put(&mut self, v: u64, bits: u32) -> &mut Self {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
        self
    }
}

/// A long-form section from table_id, extension, version and the body after
/// last_section_number; CRC appended (after the spec's hook).
#[allow(clippy::too_many_arguments)]
pub fn section(
    spec: &Spec,
    table_id: u8,
    private: bool,
    ext: u16,
    version: u8,
    number: u8,
    last: u8,
    body: &[u8],
) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![
        table_id,
        0x80 | if private { 0x40 } else { 0 } | 0x30 | ((len >> 8) as u8 & 0x0F),
        len as u8,
        (ext >> 8) as u8,
        ext as u8,
        0xC0 | (version << 1) | 1,
        number,
        last,
    ];
    s.extend_from_slice(body);
    if let Some(h) = spec.hook {
        h(&mut s);
        let len = s.len() + 4 - 3;
        s[1] = (s[1] & 0xF0) | ((len >> 8) as u8 & 0x0F);
        s[2] = len as u8;
    }
    let c = crc32(&s);
    s.extend_from_slice(&c.to_be_bytes());
    s
}

/// Recomputes the CRC of the section that starts the payload of a packet (pointer 0) and
/// ends in it.
pub fn fix_crc(pkt: &mut [u8; 188]) {
    let off = 5 + usize::from(pkt[4]);
    let len = 3 + usize::from(u16::from_be_bytes([pkt[off + 1], pkt[off + 2]]) & 0x0FFF);
    let c = crc32(&pkt[off..off + len - 4]);
    pkt[off + len - 4..off + len].copy_from_slice(&c.to_be_bytes());
}

/// Offset of the first section byte in a PUSI section packet without adaptation field.
pub fn sec_off(pkt: &[u8; 188]) -> usize {
    5 + usize::from(pkt[4])
}

pub fn pat(spec: &Spec) -> Vec<u8> {
    let mut b = Vec::new();
    for p in &spec.programs {
        b.extend_from_slice(&p.number.to_be_bytes());
        b.extend_from_slice(&(0xE000 | p.pmt_pid).to_be_bytes());
    }
    section(spec, 0x00, false, spec.tsid, spec.version, 0, 0, &b)
}

fn es_entry(b: &mut Vec<u8>, st: u8, pid: u16, desc: &[u8]) {
    b.push(st);
    b.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    b.extend_from_slice(&(0xF000 | desc.len() as u16).to_be_bytes());
    b.extend_from_slice(desc);
}

pub fn pmt(spec: &Spec, p: &Prog) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&(0xE000 | p.pcr_pid).to_be_bytes());
    b.extend_from_slice(&(0xF000 | p.info_desc.len() as u16).to_be_bytes());
    b.extend_from_slice(&p.info_desc);
    es_entry(&mut b, p.video_type, p.video_pid, &p.video_desc);
    if let Some(a) = p.audio_pid {
        es_entry(&mut b, p.audio_type, a, &p.audio_desc);
    }
    for x in &p.extra {
        es_entry(&mut b, x.stream_type, x.pid, &x.desc);
    }
    section(spec, 0x02, false, p.number, spec.version, 0, 0, &b)
}

/// A one-segment, mode 0 multiple_string_structure in English.
pub fn mss(text: &str) -> Vec<u8> {
    let mut v = vec![1, b'e', b'n', b'g'];
    let chunks: Vec<&[u8]> = text.as_bytes().chunks(255).collect();
    v.push(chunks.len() as u8);
    for c in chunks {
        v.extend_from_slice(&[0, 0, c.len() as u8]);
        v.extend_from_slice(c);
    }
    v
}

fn iso_lang(desc: &[u8]) -> [u8; 3] {
    let mut i = 0;
    while i + 2 <= desc.len() {
        let (t, l) = (desc[i], usize::from(desc[i + 1]));
        if t == 0x0A && l >= 4 {
            return [desc[i + 2], desc[i + 3], desc[i + 4]];
        }
        i += 2 + l;
    }
    [0; 3]
}

pub fn vct(spec: &Spec) -> Vec<u8> {
    let mut b = vec![0x00, spec.programs.len() as u8];
    for p in &spec.programs {
        let mut name = [0u8; 14];
        for (i, ch) in p.name.encode_utf16().take(7).enumerate() {
            name[2 * i..2 * i + 2].copy_from_slice(&ch.to_be_bytes());
        }
        b.extend_from_slice(&name);
        let mut bits = Bits::default();
        bits.put(0xF, 4)
            .put(u64::from(p.major), 10)
            .put(u64::from(p.minor), 10);
        b.extend_from_slice(&bits.out);
        b.push(if spec.tvct { 0x04 } else { 0x03 });
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&spec.tsid.to_be_bytes());
        b.extend_from_slice(&p.number.to_be_bytes());
        // ETM_location 00, access 0, hidden 0, path 0, oob 0, hide_guide 1, reserved 1;
        // reserved 11, service_type 0x02.
        b.push(0x03);
        b.push(0xC2);
        b.extend_from_slice(&p.source_id.to_be_bytes());
        let mut sld = Vec::new();
        if p.sld {
            sld = vec![0xA1, 0, 0xE0 | (p.pcr_pid >> 8) as u8, p.pcr_pid as u8];
            let mut elems = vec![(p.video_type, p.video_pid, [0u8; 3])];
            if let Some(a) = p.audio_pid {
                elems.push((p.audio_type, a, *b"eng"));
            }
            for x in &p.extra {
                elems.push((x.stream_type, x.pid, iso_lang(&x.desc)));
            }
            sld.push(elems.len() as u8);
            for (st, pid, l) in elems {
                sld.push(st);
                sld.extend_from_slice(&(0xE000 | pid).to_be_bytes());
                sld.extend_from_slice(&l);
            }
            sld[1] = (sld.len() - 2) as u8;
        }
        b.extend_from_slice(&(0xFC00 | sld.len() as u16).to_be_bytes());
        b.extend_from_slice(&sld);
    }
    b.extend_from_slice(&0xFC00u16.to_be_bytes());
    let tid = if spec.tvct { 0xC8 } else { 0xC9 };
    section(spec, tid, true, spec.tsid, spec.version, 0, 0, &b)
}

/// The start of the 3-hour window containing the first byte, GPS seconds.
pub fn window0(spec: &Spec) -> u32 {
    let utc = spec.gps0 - u32::from(spec.gps_utc);
    utc / 10_800 * 10_800 + u32::from(spec.gps_utc)
}

pub fn eit(spec: &Spec, p: &Prog, k: u32) -> Vec<u8> {
    let events = if k == 0 { &spec.events0 } else { &spec.events1 };
    let mut b = vec![0x00, events.len() as u8];
    let w = i64::from(window0(spec)) + 10_800 * i64::from(k);
    for e in events {
        b.extend_from_slice(&(0xC000u16 | e.id).to_be_bytes());
        b.extend_from_slice(&((w + e.start) as u32).to_be_bytes());
        let x: u32 = (0b11 << 22) | (u32::from(e.etm) << 20) | e.len;
        b.extend_from_slice(&x.to_be_bytes()[1..]);
        let t = mss("News");
        b.push(t.len() as u8);
        b.extend_from_slice(&t);
        b.extend_from_slice(&0xF000u16.to_be_bytes());
    }
    section(spec, 0xCB, true, p.source_id, spec.version, 0, 0, &b)
}

pub fn ett(spec: &Spec, p: &Prog) -> Vec<u8> {
    let mut b = vec![0x00];
    let id: u32 = (u32::from(p.source_id) << 16) | (1 << 2) | 2;
    b.extend_from_slice(&id.to_be_bytes());
    b.extend_from_slice(&mss(&spec.ett_text));
    section(spec, 0xCC, true, p.source_id, spec.version, 0, 0, &b)
}

fn eit_progs(spec: &Spec) -> Vec<&Prog> {
    spec.programs
        .iter()
        .filter(|p| Some(p.source_id) != spec.eit_skip_source)
        .collect()
}

pub fn mgt(spec: &Spec) -> Vec<u8> {
    let cv = vct(spec).len() as u32;
    let ei: u32 = eit_progs(spec)
        .iter()
        .map(|p| eit(spec, p, 0).len() as u32)
        .sum();
    let ei1: u32 = eit_progs(spec)
        .iter()
        .map(|p| eit(spec, p, 1).len() as u32)
        .sum();
    let et: u32 = spec
        .programs
        .iter()
        .map(|p| ett(spec, p).len() as u32)
        .sum();
    let mut tables = vec![(if spec.tvct { 0x0000u16 } else { 0x0002 }, 0x1FFBu16, cv)];
    tables.push((0x0100, spec.eit_pid, ei));
    if spec.eit1 {
        tables.push((0x0101, spec.eit_pid + 1, ei1));
    }
    if spec.ett {
        tables.push((0x0200, spec.ett_pid, et));
    }
    let mut b = vec![0x00];
    b.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    for (tt, pid, n) in tables {
        b.extend_from_slice(&tt.to_be_bytes());
        b.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        b.push(0xE0 | spec.version);
        b.extend_from_slice(&spec.mgt_bytes.unwrap_or(n).to_be_bytes());
        b.extend_from_slice(&0xF000u16.to_be_bytes());
    }
    b.extend_from_slice(&0xF000u16.to_be_bytes());
    section(spec, 0xC7, true, 0, spec.version, 0, 0, &b)
}

pub fn stt(spec: &Spec, gps: u32) -> Vec<u8> {
    let mut b = vec![0x00];
    b.extend_from_slice(&gps.to_be_bytes());
    b.push(spec.gps_utc);
    b.extend_from_slice(&0x6000u16.to_be_bytes());
    section(spec, 0xCD, true, 0, 0, 0, 0, &b)
}

/// A minimal RRT for region 5: one dimension with one value.
pub fn rrt(spec: &Spec) -> Vec<u8> {
    let mut b = vec![0x00];
    let name = mss("Test");
    b.push(name.len() as u8);
    b.extend_from_slice(&name);
    b.push(1);
    let dim = mss("D");
    b.push(dim.len() as u8);
    b.extend_from_slice(&dim);
    b.push(0xE1);
    for t in ["", ""] {
        let m = mss(t);
        b.push(m.len() as u8);
        b.extend_from_slice(&m);
    }
    b.extend_from_slice(&0xFC00u16.to_be_bytes());
    section(spec, 0xCA, true, 0xFF05, 0, 0, 0, &b)
}

/// The payloads of the packets carrying one section, each starting a packet.
pub fn packetize_section(s: &[u8]) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    let mut first = true;
    let mut rest = s;
    while first || !rest.is_empty() {
        let mut p = Vec::with_capacity(184);
        if first {
            p.push(0);
        }
        let take = (184 - p.len()).min(rest.len());
        p.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
        p.resize(184, 0xFF);
        v.push(p);
        first = false;
    }
    v
}

/// The elementary stream of one video frame.
pub fn video_frame(p: &Prog, k: usize) -> Vec<u8> {
    let mut v = Vec::new();
    if k.is_multiple_of(15) {
        v.extend_from_slice(&[0, 0, 1, 0xB3, 0x2D, 0x01, 0xE0, 0x24]);
        let x: u32 = (3600 << 14) | (1 << 13) | ((p.vbv & 0x3FF) << 3);
        v.extend_from_slice(&x.to_be_bytes());
        let mut b = Bits::default();
        b.put(1, 4)
            .put(u64::from(p.profile_level), 8)
            .put(0, 1)
            .put(1, 2)
            .put(0, 2)
            .put(0, 2)
            .put(0, 12);
        b.put(1, 1).put(0, 8).put(0, 1).put(0, 2).put(0, 5);
        v.extend_from_slice(&[0, 0, 1, 0xB5]);
        v.extend_from_slice(&b.out);
        v.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x00]);
    }
    v.extend_from_slice(&[0, 0, 1, 0x00, 0x00, 0x0F, 0xFF, 0xF8]);
    v.extend_from_slice(&[0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x80, 0x80]);
    v.extend_from_slice(&[0, 0, 1, 0x01]);
    let size = match p.big_frame {
        Some((a, b, n)) if (a..b).contains(&k) => n,
        _ => p.frame_bytes,
    };
    v.resize(size, 0x55);
    v
}

/// One AC-3 syncframe: 48 kHz, 192 kbit/s (768 bytes), bsid 8, bsmod 0, acmod 2.
pub fn ac3_frame() -> Vec<u8> {
    let mut f = vec![0u8; 768];
    f[0] = 0x0B;
    f[1] = 0x77;
    f[4] = 0x14;
    f[5] = 0x40;
    f[6] = 0x40;
    f
}

pub fn ts5(prefix: u8, ts: u64) -> [u8; 5] {
    [
        (prefix << 4) | (((ts >> 30) as u8 & 7) << 1) | 1,
        (ts >> 22) as u8,
        (((ts >> 15) as u8) << 1) | 1,
        (ts >> 7) as u8,
        ((ts as u8) << 1) | 1,
    ]
}

/// A PES packet: stream_id, PES_packet_length (0 or computed), PTS and DTS, alignment.
pub fn pes(
    stream_id: u8,
    unbounded: bool,
    pts: Option<u64>,
    dts: Option<u64>,
    align: bool,
    es: &[u8],
) -> Vec<u8> {
    let flags = match (pts, dts) {
        (Some(_), Some(_)) => 0xC0,
        (Some(_), None) => 0x80,
        _ => 0x00,
    };
    let hdl = match (pts, dts) {
        (Some(_), Some(_)) => 10,
        (Some(_), None) => 5,
        _ => 0,
    };
    let mut v = vec![
        0,
        0,
        1,
        stream_id,
        0,
        0,
        if align { 0x84 } else { 0x80 },
        flags,
        hdl,
    ];
    if let Some(p) = pts {
        v.extend_from_slice(&ts5(if dts.is_some() { 3 } else { 2 }, p));
    }
    if let Some(d) = dts {
        v.extend_from_slice(&ts5(1, d));
    }
    v.extend_from_slice(es);
    if !unbounded {
        let l = (v.len() - 6) as u16;
        v[4..6].copy_from_slice(&l.to_be_bytes());
    }
    v
}

/// Packet payloads of a PES: 184-byte chunks; the last is short (stuffed with an
/// adaptation field when the packet is built).
pub fn packetize_pes(p: &[u8]) -> Vec<Vec<u8>> {
    p.chunks(184).map(|c| c.to_vec()).collect()
}

/// One packet to send: PID, PUSI, payload (`None`: a PCR-only packet), not before `due`.
#[derive(Clone, Debug)]
struct Item {
    due: f64,
    pid: u16,
    pusi: bool,
    payload: Option<Vec<u8>>,
}

/// The built stream.
#[derive(Clone, Debug)]
pub struct Stream {
    pub packets: Vec<[u8; 188]>,
}

impl Stream {
    pub fn bytes(&self) -> Vec<u8> {
        self.packets.iter().flatten().copied().collect()
    }

    pub fn pid(&self, i: usize) -> u16 {
        (u16::from(self.packets[i][1] & 0x1F) << 8) | u16::from(self.packets[i][2])
    }

    /// Indexes of the packets of a PID.
    pub fn of_pid(&self, pid: u16) -> Vec<usize> {
        (0..self.packets.len())
            .filter(|&i| self.pid(i) == pid)
            .collect()
    }

    /// Indexes of the packets of a PID with PUSI set.
    pub fn starts(&self, pid: u16) -> Vec<usize> {
        self.of_pid(pid)
            .into_iter()
            .filter(|&i| self.packets[i][1] & 0x40 != 0)
            .collect()
    }

    /// Indexes of the PCR-only packets of a PID.
    pub fn pcr_packets(&self, pid: u16) -> Vec<usize> {
        self.of_pid(pid)
            .into_iter()
            .filter(|&i| self.packets[i][3] & 0x30 == 0x20 && self.packets[i][5] & 0x10 != 0)
            .collect()
    }

    /// Index of the first packet of `pid` with PUSI at or after `from`.
    pub fn start_after(&self, pid: u16, from: usize) -> usize {
        self.starts(pid).into_iter().find(|&i| i >= from).unwrap()
    }

    /// Index of the first null packet at or after `from`.
    pub fn null_after(&self, from: usize) -> usize {
        (from..self.packets.len())
            .find(|&i| self.pid(i) == 0x1FFF)
            .unwrap()
    }

    /// Replaces packet `i` by a null packet.
    pub fn null(&mut self, i: usize) {
        let mut p = [0xFFu8; 188];
        p[..4].copy_from_slice(&[0x47, 0x1F, 0xFF, 0x10]);
        self.packets[i] = p;
    }

    /// The packet index at a stream time.
    pub fn at(&self, spec: &Spec, t: f64) -> usize {
        (t * spec.rate as f64 / 1504.0) as usize
    }
}

/// Builds the stream a spec describes.
pub fn build(spec: &Spec) -> Stream {
    let slot = 1504.0 / spec.rate as f64;
    let n_slots = (spec.secs / slot) as usize;
    let periodic = |q: &mut Vec<Item>, period: f64, pid: u16, sec: &[u8]| {
        let mut t = 0.0;
        while t < spec.secs {
            for (i, p) in packetize_section(sec).into_iter().enumerate() {
                q.push(Item {
                    due: t,
                    pid,
                    pusi: i == 0,
                    payload: Some(p),
                });
            }
            t += period;
        }
    };
    let mut psi = Vec::new();
    periodic(&mut psi, spec.pat_period, 0, &pat(spec));
    for p in &spec.programs {
        periodic(&mut psi, spec.pmt_period, p.pmt_pid, &pmt(spec, p));
    }
    let mut psip = Vec::new();
    if spec.psip {
        periodic(&mut psip, spec.mgt_period, 0x1FFB, &mgt(spec));
        periodic(&mut psip, spec.vct_period, 0x1FFB, &vct(spec));
        let mut t = 0.0;
        while t < spec.secs {
            let gps = if spec.stt_frozen {
                spec.gps0
            } else {
                spec.gps0 + t as u32
            };
            for (i, p) in packetize_section(&stt(spec, gps)).into_iter().enumerate() {
                psip.push(Item {
                    due: t,
                    pid: 0x1FFB,
                    pusi: i == 0,
                    payload: Some(p),
                });
            }
            t += spec.stt_period;
        }
        if spec.rrt_once {
            for (i, p) in packetize_section(&rrt(spec)).into_iter().enumerate() {
                psip.push(Item {
                    due: 0.05,
                    pid: 0x1FFB,
                    pusi: i == 0,
                    payload: Some(p),
                });
            }
        }
        for p in eit_progs(spec) {
            periodic(&mut psip, spec.eit_period, spec.eit_pid, &eit(spec, p, 0));
            if spec.eit1 {
                periodic(
                    &mut psip,
                    spec.eit_period,
                    spec.eit_pid + 1,
                    &eit(spec, p, 1),
                );
            }
        }
        if spec.ett {
            for p in &spec.programs {
                periodic(&mut psip, spec.ett_period, spec.ett_pid, &ett(spec, p));
            }
        }
    }
    let psi: VecDeque<Item> = merge(psi);
    let psip: VecDeque<Item> = merge(psip);
    let mut audio: Vec<Item> = Vec::new();
    let mut video: Vec<Item> = Vec::new();
    let mut flood: Vec<Item> = Vec::new();
    let video_first = spec.programs.iter().any(|p| p.video_first);
    let stc90 = |t: f64| (PCR0 as f64 / 300.0 + t * 90_000.0).round() as u64 % (1 << 33);
    for p in &spec.programs {
        // Video: 29.97 frames/s, PTS = 0.5 s + k / fps, bytes spread over one frame period
        // starting video_lead before the PTS.
        let fps = 30000.0 / 1001.0;
        let t_first = (p.video_lead + 0.05).max(0.5);
        let mut k = 0;
        loop {
            let pts_t = t_first + k as f64 / fps;
            let start = pts_t - p.video_lead;
            if start >= spec.secs {
                break;
            }
            let pts = stc90(pts_t);
            let dts = p.video_dts.map(|d| (pts as i64 + d) as u64 % (1 << 33));
            let pes_bytes = pes(0xE0, true, Some(pts), dts, true, &video_frame(p, k));
            let chunks = packetize_pes(&pes_bytes);
            let n = chunks.len();
            for (i, c) in chunks.into_iter().enumerate() {
                let off = if p.video_burst {
                    0.0
                } else {
                    i as f64 / n as f64 / fps
                };
                video.push(Item {
                    due: start.max(0.0) + off,
                    pid: p.video_pid,
                    pusi: i == 0,
                    payload: Some(c),
                });
            }
            k += 1;
        }
        let mut audio_pids: Vec<u16> = p.audio_pid.into_iter().collect();
        audio_pids.extend(p.extra.iter().filter(|x| x.flood.is_none()).map(|x| x.pid));
        for a in audio_pids {
            let mut j = 0;
            loop {
                let pts_t = 0.3 + j as f64 * 0.032;
                let start = pts_t - p.audio_lead;
                if start >= spec.secs {
                    break;
                }
                let pts = (j % p.audio_pts_every == 0).then(|| stc90(pts_t));
                let frame = ac3_frame();
                let mut pes_list = Vec::new();
                if p.audio_split {
                    pes_list.push(pes(0xBD, false, pts, None, true, &frame[..400]));
                    pes_list.push(pes(0xBD, false, pts, None, false, &frame[400..]));
                } else {
                    pes_list.push(pes(0xBD, false, pts, None, true, &frame));
                }
                let mut i = 0;
                for pb in pes_list {
                    for (c_i, c) in packetize_pes(&pb).into_iter().enumerate() {
                        audio.push(Item {
                            due: start + i as f64 * p.audio_spacing,
                            pid: a,
                            pusi: c_i == 0,
                            payload: Some(c),
                        });
                        i += 1;
                    }
                }
                j += 1;
            }
        }
        for x in p.extra.iter().filter(|x| x.flood.is_some()) {
            let Some((every, secs)) = x.flood else {
                continue;
            };
            let n = (secs / (every as f64 * slot)) as usize + 1;
            let body = vec![0u8; n * 184 - 14];
            let pb = pes(
                0xBD,
                false,
                Some(stc90(0.2 + secs + 0.1)),
                None,
                false,
                &body,
            );
            let s0 = (0.2 / slot).ceil();
            for (i, c) in packetize_pes(&pb).into_iter().enumerate() {
                let k = if i == 0 { 0 } else { 1 + every * (i - 1) };
                flood.push(Item {
                    due: (s0 + k as f64) * slot,
                    pid: x.pid,
                    pusi: i == 0,
                    payload: Some(c),
                });
            }
        }
    }
    let mut audio: VecDeque<Item> = merge(audio);
    let mut video: VecDeque<Item> = merge(video);
    let mut flood: VecDeque<Item> = merge(flood);
    let mut psi = psi;
    let mut psip = psip;
    let mut cc: BTreeMap<u16, u8> = BTreeMap::new();
    let mut next_pcr: Vec<f64> = vec![0.0; spec.programs.len()];
    let mut packets = Vec::with_capacity(n_slots);
    for n in 0..n_slots {
        let t = n as f64 * slot;
        let mut item: Option<Item> = None;
        // Due times are slot boundaries computed in floating point: allow for rounding.
        let t = t + slot * 1e-6;
        if flood.front().is_some_and(|i| i.due <= t) {
            item = flood.pop_front();
        }
        if item.is_none() && video_first && video.front().is_some_and(|i| i.due <= t) {
            item = video.pop_front();
        }
        if item.is_none() && psi.front().is_some_and(|i| i.due <= t) {
            item = psi.pop_front();
        }
        if item.is_none() && spec.pcrs {
            for (k, nt) in next_pcr.iter_mut().enumerate() {
                if *nt <= t {
                    *nt += spec.pcr_period;
                    item = Some(Item {
                        due: t,
                        pid: spec.programs[k].pcr_pid,
                        pusi: false,
                        payload: None,
                    });
                    break;
                }
            }
        }
        if item.is_none() && psip.front().is_some_and(|i| i.due <= t) {
            item = psip.pop_front();
        }
        if item.is_none() && audio.front().is_some_and(|i| i.due <= t) {
            item = audio.pop_front();
        }
        if item.is_none() && video.front().is_some_and(|i| i.due <= t) {
            item = video.pop_front();
        }
        let mut pkt = [0xFFu8; 188];
        match item {
            None => pkt[..4].copy_from_slice(&[0x47, 0x1F, 0xFF, 0x10]),
            Some(it) => {
                pkt[0] = 0x47;
                pkt[1] = (if it.pusi { 0x40 } else { 0 }) | (it.pid >> 8) as u8;
                pkt[2] = it.pid as u8;
                let c = cc.entry(it.pid).or_insert(15);
                match it.payload {
                    None => {
                        // Adaptation-only packet with a PCR; CC not incremented.
                        pkt[3] = 0x20 | *c;
                        pkt[4] = 183;
                        pkt[5] = 0x10;
                        put_pcr(&mut pkt[6..12], pcr_at(spec, n));
                    }
                    Some(pl) => {
                        *c = (*c + 1) & 0x0F;
                        if pl.len() == 184 {
                            pkt[3] = 0x10 | *c;
                            pkt[4..].copy_from_slice(&pl);
                        } else {
                            pkt[3] = 0x30 | *c;
                            let af = 183 - pl.len();
                            pkt[4] = af as u8;
                            if af > 0 {
                                pkt[5] = 0x00;
                            }
                            pkt[188 - pl.len()..].copy_from_slice(&pl);
                        }
                    }
                }
            }
        }
        packets.push(pkt);
    }
    Stream { packets }
}

/// The PCR of a PCR carried in slot `n`.
pub fn pcr_at(spec: &Spec, n: usize) -> u64 {
    let byte = n as u128 * 188 + 10;
    let lin = PCR0 + (byte * 8 * 27_000_000 / u128::from(spec.rate)) as u64;
    if spec.vbr_warp > 0.0 {
        // Triangle wave of period 0.2 s and peak vbr_warp: the local rate alternates
        // between 1 + 10 vbr_warp and 1 - 10 vbr_warp of the mean.
        let t = byte as f64 * 8.0 / spec.rate as f64;
        let ph = (t / 0.2).fract();
        let tri = if ph < 0.5 { ph * 2.0 } else { 2.0 - ph * 2.0 };
        lin + (tri * spec.vbr_warp * 27e6) as u64
    } else {
        lin
    }
}

pub fn put_pcr(b: &mut [u8], pcr: u64) {
    let base = pcr / 300;
    let ext = pcr % 300;
    b[0] = (base >> 25) as u8;
    b[1] = (base >> 17) as u8;
    b[2] = (base >> 9) as u8;
    b[3] = (base >> 1) as u8;
    b[4] = (((base & 1) as u8) << 7) | 0x7E | ((ext >> 8) as u8 & 1);
    b[5] = ext as u8;
}

pub fn get_pcr(b: &[u8]) -> u64 {
    let base = (u64::from(b[0]) << 25)
        | (u64::from(b[1]) << 17)
        | (u64::from(b[2]) << 9)
        | (u64::from(b[3]) << 1)
        | u64::from(b[4] >> 7);
    base * 300 + ((u64::from(b[4] & 1) << 8) | u64::from(b[5]))
}

fn merge(mut all: Vec<Item>) -> VecDeque<Item> {
    // A stable sort keeps the packets of each section and PES together and in order.
    all.sort_by(|a, b| a.due.total_cmp(&b.due));
    all.into()
}

/// Options matching a built stream.
pub fn opts(spec: &Spec) -> Options {
    Options {
        rate: Some(spec.rate as f64),
        tsid: Some(spec.tsid),
        max_examples: 4,
        ..Options::default()
    }
}

/// Checks a stream.
pub fn run(spec: &Spec, s: &Stream) -> Report {
    check(&s.bytes(), &opts(spec))
}

/// The rules reported, by identifier.
pub fn ids(r: &Report) -> Vec<&'static str> {
    r.counts.keys().map(|r| r.info().id).collect()
}

/// Asserts that exactly these rules (by identifier) were reported.
#[track_caller]
pub fn expect(r: &Report, want: &[&str]) {
    let got = ids(r);
    let mut w: Vec<&str> = want.to_vec();
    w.sort_unstable();
    let mut g = got.clone();
    g.sort_unstable();
    assert_eq!(g, w, "\n{}\n{}", r.summary(), r.details());
}

/// Rules of severity S1 to S3 reported.
pub fn serious(r: &Report) -> Vec<Rule> {
    r.counts
        .keys()
        .copied()
        .filter(|k| k.info().severity <= Severity::S3)
        .collect()
}
