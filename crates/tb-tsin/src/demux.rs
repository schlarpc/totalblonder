//! The input demuxer: transport packets of one program into [`InputEvent`]s.
//!
//! - **Program.** The PAT (PID 0) names each program's PMT PID; the demuxer follows the
//!   program [`ProgramSelector`] picks (the first listed, or a number). A PAT version
//!   change that moves the PMT is followed.
//! - **Streams.** Each elementary stream of the PMT that [`crate::codec::classify`] maps to a
//!   codec becomes a [`StreamInfo`] whose index is its PID (13 bits fit `StreamIndex`, and a
//!   PID keeps its index across PMT versions). Time base 90 kHz; no extradata (Annex B,
//!   ADTS, sync frames) except Opus's `OpusHead`; language from the ISO 639 descriptor.
//!   `Streams` is emitted when the first PMT with a usable stream arrives, and again when a
//!   new PMT version changes that set (an identical set is not re-announced).
//! - **PES.** Payloads are reassembled per PID from payload_unit_start to the next one (or to
//!   PES_packet_length when it is non-zero, so audio is not held for a packet). One PES is
//!   one [`Packet`] (Opus: one per access unit), with PTS/DTS unwrapped from 33 bits (one
//!   unwrapper per program), `dts = pts` when only PTS is sent, and `keyframe` from the
//!   random_access_indicator or the bitstream ([`crate::codec::is_key`]).
//! - **Loss.** The continuity counter (§2.4.3.3) is checked per PID: a duplicate packet is
//!   dropped; a gap drops the partial PES and flags the next packet of that PID
//!   ([`TsEvent::after_loss`]); a discontinuity_indicator allows a jump. A PES larger than
//!   [`DemuxConfig::max_pes`], or one that would take the partial PES packets of all streams
//!   over [`DemuxConfig::max_buffered`], is dropped the same way.
//!
//! `End` is not the demuxer's business (a TS has no end of its own): the transport layer
//! ([`crate::input`]) emits it.

use std::collections::BTreeMap;

use tb_media::{CodecId, InputEvent, Packet, Rational, StreamIndex, StreamInfo};

use crate::codec::{self, StreamKind};
use crate::packet::{Framer, PACKET_SIZE, Pid, TsPacket};
use crate::pes::PesHeader;
use crate::psi::{self, PatEntry, SectionAssembler};
use crate::unwrap::Unwrap33;

/// Which program to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgramSelector {
    /// The first program the PAT lists.
    #[default]
    First,
    /// The program with this program_number.
    Number(u16),
}

/// Demuxer settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxConfig {
    /// The program.
    pub program: ProgramSelector,
    /// Largest PES reassembled (bytes).
    pub max_pes: usize,
    /// Most bytes held in partial PES packets across all streams.
    pub max_buffered: usize,
    /// Most elementary streams followed.
    pub max_streams: usize,
}

impl Default for DemuxConfig {
    fn default() -> Self {
        Self {
            program: ProgramSelector::First,
            max_pes: 8 << 20,
            max_buffered: 16 << 20,
            max_streams: 16,
        }
    }
}

/// An input event, with the loss flag a transport stream can give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsEvent {
    /// The event.
    pub event: InputEvent,
    /// For a packet: data of its PID was lost just before it (a continuity gap, a dropped or
    /// oversized PES). The decoder should expect damage until the next key frame.
    pub after_loss: bool,
}

/// Counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DemuxStats {
    /// Transport packets read.
    pub packets: u64,
    /// Packets with transport_error_indicator set (dropped).
    pub transport_errors: u64,
    /// Scrambled packets (dropped).
    pub scrambled: u64,
    /// Packets that could not be parsed (no sync byte, bad adaptation field).
    pub malformed: u64,
    /// Continuity counter gaps.
    pub cc_errors: u64,
    /// Duplicate packets dropped.
    pub duplicates: u64,
    /// PES packets dropped (loss, oversize, bad header).
    pub pes_dropped: u64,
    /// PSI sections with a bad CRC or length.
    pub bad_sections: u64,
    /// PMT entries not carried.
    pub skipped_streams: u64,
    /// Bytes skipped by the byte-stream framer, and sync losses.
    pub framer_skipped: u64,
    /// Sync losses of the byte-stream framer.
    pub sync_losses: u64,
}

#[derive(Debug, Clone)]
struct PesState {
    codec: CodecId,
    buf: Vec<u8>,
    active: bool,
    random_access: bool,
    after_loss: bool,
    last_cc: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Continuity {
    Ok,
    Duplicate,
    Loss,
}

fn continuity(last: &mut Option<u8>, p: &TsPacket<'_>) -> Continuity {
    if !p.has_payload {
        return Continuity::Ok;
    }
    let disc = p.adaptation.is_some_and(|a| a.discontinuity);
    let r = match *last {
        None => Continuity::Ok,
        Some(_) if disc => Continuity::Ok,
        Some(l) if p.cc == l => return Continuity::Duplicate,
        Some(l) if p.cc == (l + 1) & 0x0F => Continuity::Ok,
        Some(_) => Continuity::Loss,
    };
    *last = Some(p.cc);
    r
}

/// The transport stream demuxer.
#[derive(Debug, Clone)]
pub struct Demuxer {
    config: DemuxConfig,
    framer: Framer,
    pat: SectionAssembler,
    pat_cc: Option<u8>,
    pat_version: Option<u8>,
    pat_sections: BTreeMap<u8, Vec<PatEntry>>,
    selected: Option<PatEntry>,
    pmt: SectionAssembler,
    pmt_cc: Option<u8>,
    pmt_version: Option<u8>,
    streams: BTreeMap<Pid, PesState>,
    announced: Option<Vec<StreamInfo>>,
    unwrap: Unwrap33,
    events: Vec<TsEvent>,
    stats: DemuxStats,
}

impl Demuxer {
    /// A demuxer with nothing known.
    pub fn new(config: DemuxConfig) -> Self {
        Self {
            config,
            framer: Framer::new(),
            pat: SectionAssembler::default(),
            pat_cc: None,
            pat_version: None,
            pat_sections: BTreeMap::new(),
            selected: None,
            pmt: SectionAssembler::default(),
            pmt_cc: None,
            pmt_version: None,
            streams: BTreeMap::new(),
            announced: None,
            unwrap: Unwrap33::new(),
            events: Vec::new(),
            stats: DemuxStats::default(),
        }
    }

    /// Forget everything (a new session); counters are kept.
    pub fn reset(&mut self) {
        let stats = self.stats;
        *self = Self::new(self.config);
        self.stats = stats;
    }

    /// Counters.
    pub fn stats(&self) -> DemuxStats {
        let mut s = self.stats;
        s.bad_sections = self.pat.bad + self.pmt.bad;
        s.framer_skipped = self.framer.skipped;
        s.sync_losses = self.framer.sync_losses;
        s
    }

    /// `Streams` has been emitted since the last reset.
    pub fn started(&self) -> bool {
        self.announced.is_some()
    }

    /// The program followed, once the PAT has named it.
    pub fn program(&self) -> Option<u16> {
        self.selected.map(|s| s.program)
    }

    /// Events produced so far.
    pub fn take_events(&mut self) -> Vec<TsEvent> {
        std::mem::take(&mut self.events)
    }

    /// Feed packet-aligned data (a UDP datagram, an RTP payload): whole 188-byte packets; a
    /// trailing partial packet is counted as malformed.
    pub fn push_packets(&mut self, data: &[u8]) {
        let (packets, rest) = data.as_chunks::<PACKET_SIZE>();
        for p in packets {
            self.packet(p);
        }
        if !rest.is_empty() {
            self.stats.malformed += 1;
        }
    }

    /// Feed a byte stream split anywhere (a file, a TCP or SRT stream): sync is acquired and
    /// kept by the framer.
    pub fn push_bytes(&mut self, data: &[u8]) {
        let mut framer = std::mem::take(&mut self.framer);
        framer.push(data, |p| self.packet(p));
        self.framer = framer;
    }

    /// Release every partial PES that is complete enough to read (at the end of input).
    pub fn flush(&mut self) {
        let pids: Vec<Pid> = self.streams.keys().copied().collect();
        for pid in pids {
            self.flush_pes(pid);
        }
    }

    /// One transport packet.
    pub fn packet(&mut self, raw: &[u8; PACKET_SIZE]) {
        self.stats.packets += 1;
        let p = match TsPacket::parse(raw) {
            Ok(p) => p,
            Err(_) => {
                self.stats.malformed += 1;
                return;
            }
        };
        if p.error {
            self.stats.transport_errors += 1;
            return;
        }
        if p.pid == Pid::NULL {
            return;
        }
        if p.scrambled {
            self.stats.scrambled += 1;
            return;
        }
        if p.pid == Pid::PAT {
            let c = continuity(&mut self.pat_cc, &p);
            if c == Continuity::Duplicate {
                return;
            }
            let mut sections = Vec::new();
            self.pat
                .push(p.payload, p.unit_start, c == Continuity::Ok, |s| {
                    sections.push(s.to_vec());
                });
            for s in sections {
                self.on_pat(&s);
            }
        } else if self.selected.is_some_and(|s| s.pmt_pid == p.pid) {
            let c = continuity(&mut self.pmt_cc, &p);
            if c == Continuity::Duplicate {
                return;
            }
            let mut sections = Vec::new();
            self.pmt
                .push(p.payload, p.unit_start, c == Continuity::Ok, |s| {
                    sections.push(s.to_vec());
                });
            for s in sections {
                self.on_pmt(&s);
            }
        } else if self.streams.contains_key(&p.pid) {
            self.on_pes_packet(&p);
        }
    }

    fn on_pat(&mut self, s: &[u8]) {
        let Some((h, body)) = psi::split_section(s) else {
            return;
        };
        if h.table_id != 0 || !h.current {
            return;
        }
        if self.pat_version != Some(h.version) {
            self.pat_version = Some(h.version);
            self.pat_sections.clear();
        }
        self.pat_sections.insert(h.number, psi::parse_pat(body));
        if (0..=h.last).any(|n| !self.pat_sections.contains_key(&n)) {
            return;
        }
        let mut all = self.pat_sections.values().flatten();
        let pick = match self.config.program {
            ProgramSelector::First => all.next().copied(),
            ProgramSelector::Number(n) => all.find(|e| e.program == n).copied(),
        };
        if pick != self.selected {
            self.selected = pick;
            self.pmt = SectionAssembler::default();
            self.pmt_cc = None;
            self.pmt_version = None;
        }
    }

    fn on_pmt(&mut self, s: &[u8]) {
        let Some((h, body)) = psi::split_section(s) else {
            return;
        };
        let Some(sel) = self.selected else { return };
        if h.table_id != 2 || !h.current || h.id != sel.program {
            return;
        }
        if self.pmt_version == Some(h.version) {
            return;
        }
        let Some(pmt) = psi::parse_pmt(body) else {
            self.stats.bad_sections += 1;
            return;
        };
        self.pmt_version = Some(h.version);
        let mut infos = Vec::new();
        for es in &pmt.streams {
            if infos.len() >= self.config.max_streams {
                self.stats.skipped_streams += 1;
                continue;
            }
            match codec::classify(es, &pmt.descriptors) {
                StreamKind::Codec(c) => infos.push(StreamInfo {
                    index: StreamIndex(es.pid.get()),
                    codec: c,
                    time_base: Rational::MPEG_90K,
                    extradata: if c == CodecId::Opus {
                        codec::opus_head(&es.descriptors).unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    video: Default::default(),
                    audio: Default::default(),
                    language: codec::language(&es.descriptors),
                }),
                StreamKind::Skipped(_) => self.stats.skipped_streams += 1,
            }
        }
        if self.announced.as_ref() == Some(&infos) || (self.announced.is_none() && infos.is_empty())
        {
            return;
        }
        // Keep the reassembly state of streams that keep their PID and codec.
        let mut old = std::mem::take(&mut self.streams);
        for i in &infos {
            let Ok(pid) = Pid::new(i.index.0) else {
                continue;
            };
            let st = match old.remove(&pid) {
                Some(st) if st.codec == i.codec => st,
                _ => PesState {
                    codec: i.codec,
                    buf: Vec::new(),
                    active: false,
                    random_access: false,
                    after_loss: false,
                    last_cc: None,
                },
            };
            self.streams.insert(pid, st);
        }
        self.events.push(TsEvent {
            event: InputEvent::Streams(infos.clone()),
            after_loss: false,
        });
        self.announced = Some(infos);
    }

    fn on_pes_packet(&mut self, p: &TsPacket<'_>) {
        let max = self.config.max_pes;
        // At most max_streams buffers: summing them is cheap.
        let total: usize = self.streams.values().map(|s| s.buf.len()).sum();
        let over_total = total + p.payload.len() > self.config.max_buffered;
        let Some(st) = self.streams.get_mut(&p.pid) else {
            return;
        };
        match continuity(&mut st.last_cc, p) {
            Continuity::Duplicate => {
                self.stats.duplicates += 1;
                return;
            }
            Continuity::Loss => {
                self.stats.cc_errors += 1;
                if st.active {
                    self.stats.pes_dropped += 1;
                }
                st.buf.clear();
                st.active = false;
                st.after_loss = true;
            }
            Continuity::Ok => {}
        }
        if p.unit_start {
            self.flush_pes(p.pid);
            let Some(st) = self.streams.get_mut(&p.pid) else {
                return;
            };
            st.buf.clear();
            st.buf.extend_from_slice(p.payload);
            st.active = true;
            st.random_access = p.adaptation.is_some_and(|a| a.random_access);
        } else if st.active {
            if st.buf.len() + p.payload.len() > max || over_total {
                self.stats.pes_dropped += 1;
                st.buf = Vec::new();
                st.active = false;
                st.after_loss = true;
                return;
            }
            st.buf.extend_from_slice(p.payload);
        } else {
            return;
        }
        // A bounded PES is complete as soon as its bytes are in.
        let Some(st) = self.streams.get(&p.pid) else {
            return;
        };
        if st.buf.len() >= 6 {
            let len = usize::from(u16::from_be_bytes([st.buf[4], st.buf[5]]));
            if len > 0 && st.buf.len() >= 6 + len {
                self.flush_pes(p.pid);
            }
        }
    }

    /// Emit the PES being reassembled on `pid`, if any.
    fn flush_pes(&mut self, pid: Pid) {
        let Some(st) = self.streams.get_mut(&pid) else {
            return;
        };
        if !st.active {
            return;
        }
        st.active = false;
        let buf = std::mem::take(&mut st.buf);
        let h = match PesHeader::parse(&buf) {
            Ok(h) => h,
            Err(_) => {
                self.stats.pes_dropped += 1;
                st.after_loss = true;
                return;
            }
        };
        let end = if h.packet_length > 0 {
            (6 + usize::from(h.packet_length)).min(buf.len())
        } else {
            buf.len()
        };
        let payload = buf.get(h.payload_offset..end).unwrap_or_default();
        let codec = st.codec;
        let mut after_loss = std::mem::take(&mut st.after_loss);
        let key = st.random_access || codec::is_key(codec, payload);
        let pts = h.pts.map(|t| self.unwrap.unwrap(t));
        let dts = h.dts.map(|t| self.unwrap.unwrap(t)).or(pts);
        let stream = StreamIndex(pid.get());
        if codec == CodecId::Opus {
            let mut t = pts;
            for au in codec::opus_units(payload) {
                self.events.push(TsEvent {
                    event: InputEvent::Packet(Packet {
                        stream,
                        pts: t,
                        dts: t,
                        duration: au.duration,
                        keyframe: true,
                        data: au.data.to_vec(),
                    }),
                    after_loss,
                });
                after_loss = false;
                t = t.zip(au.duration).map(|(t, d)| t + d);
            }
            return;
        }
        self.events.push(TsEvent {
            event: InputEvent::Packet(Packet {
                stream,
                pts,
                dts,
                duration: None,
                keyframe: key,
                data: payload.to_vec(),
            }),
            after_loss,
        });
    }
}
