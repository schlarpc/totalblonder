//! PES packets (H.222.0 §2.4.3.6, §2.4.3.7, p.33-41) and the per-stream buffer model.

use crate::es::{Ac3, Avc, EsEvent, M2v};
use crate::psi::{Ac3Desc, Kind};
use crate::report::Sink;
use crate::rules::Rule;
use crate::time::{Repeat, SYSTEM_CLOCK, TimeBase, pcr_diff, ts_diff};
use crate::tstd::{self, EsModel, EsParams};

/// Limits of the PTS repetition check.
pub(crate) static PTS_LIMITS: [(f64, Rule); 1] = [(0.7, Rule::PtsRepetition)];

/// A parsed PES header.
#[derive(Debug, Clone, Copy, Default)]
struct PesHdr {
    stream_id: u8,
    length: u16,
    align: bool,
    pts: Option<u64>,
    dts: Option<u64>,
}

/// Bytes of the header: 6 for streams without the optional header, else 9 +
/// PES_header_data_length; `None` while fewer bytes than that are known.
fn header_len(h: &[u8]) -> Option<usize> {
    let id = *h.get(3)?;
    if matches!(id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xFF | 0xF2 | 0xF8) {
        return Some(6);
    }
    Some(9 + usize::from(*h.get(8)?))
}

fn timestamp(b: &[u8], prefix: u8, what: &str, pid: u16, sink: &mut Sink) -> u64 {
    if b[0] >> 4 != prefix || b[0] & 1 == 0 || b[2] & 1 == 0 || b[4] & 1 == 0 {
        sink.emit(Rule::PesTsMarker, Some(pid), || {
            format!(
                "{what}: prefix {:04b} (shall be {prefix:04b}) or a marker bit 0",
                b[0] >> 4
            )
        });
    }
    (u64::from((b[0] >> 1) & 7) << 30)
        | (u64::from(b[1]) << 22)
        | (u64::from(b[2] >> 1) << 15)
        | (u64::from(b[3]) << 7)
        | u64::from(b[4] >> 1)
}

/// Parses a complete PES header and makes its checks.
fn parse_header(h: &[u8], kind: Kind, stream_type: u8, pid: u16, sink: &mut Sink) -> PesHdr {
    let p = Some(pid);
    let mut r = PesHdr {
        stream_id: h[3],
        length: u16::from_be_bytes([h[4], h[5]]),
        ..PesHdr::default()
    };
    let id = r.stream_id;
    let ok_id = match kind {
        Kind::Mpeg2Video | Kind::Avc => (0xE0..=0xEF).contains(&id),
        Kind::Ac3 | Kind::Eac3 => id == 0xBD,
        _ => true,
    };
    if !ok_id {
        sink.emit(Rule::PesStreamId, p, || {
            format!("stream_id 0x{id:02X} for stream_type 0x{stream_type:02X}")
        });
    }
    if r.length == 0 && !kind.is_video() {
        sink.emit(Rule::PesLengthZero, p, || {
            format!("PES_packet_length 0 on a {} stream", kind.name())
        });
    }
    if r.length != 0 && stream_type == 0x02 {
        sink.emit(Rule::PesLengthVideo, p, || {
            format!("MPEG-2 video PES_packet_length {} (shall be 0)", r.length)
        });
    }
    if h.len() < 9 {
        return r;
    }
    if h[6] >> 6 != 2 {
        sink.emit(Rule::PesHeader, p, || {
            format!(
                "PES header byte 6 starts with {:02b} (shall be 10)",
                h[6] >> 6
            )
        });
        return r;
    }
    let scrambling = (h[6] >> 4) & 3;
    r.align = h[6] & 0x04 != 0;
    let flags = h[7];
    let pts_dts = flags >> 6;
    let escr = flags & 0x20 != 0;
    let es_rate = flags & 0x10 != 0;
    let trick = flags & 0x08 != 0;
    let add_copy = flags & 0x04 != 0;
    let crc = flags & 0x02 != 0;
    let ext = flags & 0x01 != 0;
    let hdl = usize::from(h[8]);
    let opt = &h[9..];
    if pts_dts == 1 {
        sink.emit(Rule::PesHeader, p, || {
            "PTS_DTS_flags 01 is forbidden".into()
        });
    }
    let mut need = match pts_dts {
        2 => 5,
        3 => 10,
        _ => 0,
    };
    need += usize::from(escr) * 6
        + usize::from(es_rate) * 3
        + usize::from(trick)
        + usize::from(add_copy)
        + usize::from(crc) * 2;
    let mut ext_flags = 0u8;
    if ext {
        if let Some(&ef) = opt.get(need) {
            ext_flags = ef;
            need += 1;
            if ef & 0x80 != 0 {
                need += 16;
            }
            if ef & 0x40 != 0 {
                need += 1 + usize::from(opt.get(need).copied().unwrap_or(0));
            }
            if ef & 0x20 != 0 {
                need += 2;
            }
            if ef & 0x10 != 0 {
                need += 2;
            }
            if ef & 0x01 != 0 {
                need += 1 + usize::from(opt.get(need).copied().unwrap_or(0) & 0x7F);
            }
        } else {
            need += 1;
        }
    }
    if need > hdl {
        sink.emit(Rule::PesHeader, p, || {
            format!("PES_header_data_length {hdl} is shorter than the {need} bytes of the flagged fields")
        });
        return r;
    }
    if pts_dts >= 2 {
        let pts = timestamp(
            &opt[0..5],
            if pts_dts == 3 { 3 } else { 2 },
            "PTS",
            pid,
            sink,
        );
        r.pts = Some(pts);
        if pts_dts == 3 {
            let dts = timestamp(&opt[5..10], 1, "DTS", pid, sink);
            r.dts = Some(dts);
            let d = ts_diff(pts, dts);
            if d == 0 {
                sink.emit(Rule::PesDtsRedundant, p, || {
                    format!("DTS equals PTS ({pts})")
                });
            } else if d < 0 {
                sink.emit(Rule::PesDtsOrder, p, || {
                    format!("DTS {dts} is {} ticks after PTS {pts}", -d)
                });
            }
        }
    }
    if kind.is_video() || kind.is_audio() {
        let mut bad = Vec::new();
        if scrambling != 0 {
            bad.push("PES_scrambling_control");
        }
        if escr {
            bad.push("ESCR_flag");
        }
        if es_rate {
            bad.push("ES_rate_flag");
        }
        if crc {
            bad.push("PES_CRC_flag");
        }
        if ext_flags & 0x80 != 0 {
            bad.push("PES_private_data_flag");
        }
        if ext_flags & 0x40 != 0 {
            bad.push("pack_header_field_flag");
        }
        if ext_flags & 0x20 != 0 {
            bad.push("program_packet_sequence_counter_flag");
        }
        if ext_flags & 0x10 != 0 {
            bad.push("P-STD_buffer_flag");
        }
        if !bad.is_empty() {
            sink.emit(Rule::PesFlags, p, || format!("set: {}", bad.join(", ")));
        }
    }
    if stream_type == 0x02 {
        if r.pts.is_none() {
            sink.emit(Rule::PesVideoPts, p, || {
                "MPEG-2 video PES header without PTS".into()
            });
        }
        if !r.align {
            sink.emit(Rule::PesVideoAlign, p, || {
                "MPEG-2 video PES with data_alignment_indicator 0".into()
            });
        }
    }
    r
}

/// Everything the PES layer needs from the packet and the program.
pub(crate) struct PktCtx<'a> {
    pub(crate) sink: &'a mut Sink,
    pub(crate) tb: &'a TimeBase,
    /// The program's clock: stream time of its latest PCR and that PCR.
    pub(crate) clock: Option<(f64, u64)>,
    /// Arrival of the packet's first and last byte, when the time base is known.
    pub(crate) arrival: Option<(f64, f64)>,
    pub(crate) packet_pos: u64,
    pub(crate) model: bool,
}

/// State of one PES elementary stream.
#[derive(Debug, Clone)]
pub(crate) struct EsState {
    pub(crate) program: u16,
    pub(crate) stream_type: u8,
    pub(crate) kind: Kind,
    pid: u16,
    in_pes: bool,
    hdr: Vec<u8>,
    hdr_done: bool,
    declared: Option<usize>,
    received: usize,
    cur: PesHdr,
    au_in_pes: u32,
    payload_seen: usize,
    first4: Vec<u8>,
    m2v: M2v,
    avc: Avc,
    pub(crate) ac3: Ac3,
    es_off: u64,
    pub(crate) pts_rep: Repeat,
    model: Option<EsModel>,
    params: Option<EsParams>,
    lead_pending: bool,
    last_au_time: Option<f64>,
    last_field: bool,
    pub(crate) ac3_desc: Option<Ac3Desc>,
    ac3_compared: Option<crate::es::Ac3Header>,
    ev: Vec<EsEvent>,
    seq_noted: bool,
    pub(crate) pes_count: u64,
}

impl EsState {
    pub(crate) fn new(program: u16, stream_type: u8, pid: u16) -> Self {
        let kind = Kind::of(stream_type);
        Self {
            program,
            stream_type,
            kind,
            pid,
            in_pes: false,
            hdr: Vec::new(),
            hdr_done: false,
            declared: None,
            received: 0,
            cur: PesHdr::default(),
            au_in_pes: 0,
            payload_seen: 0,
            first4: Vec::new(),
            m2v: M2v::default(),
            avc: Avc::default(),
            ac3: Ac3::default(),
            es_off: 0,
            pts_rep: Repeat::new(format!("PTS on PID 0x{pid:04X}"), pid, &PTS_LIMITS),
            model: None,
            params: tb_only_params(kind),
            lead_pending: false,
            last_au_time: None,
            last_field: false,
            ac3_desc: None,
            ac3_compared: None,
            ev: Vec::new(),
            seq_noted: false,
            pes_count: 0,
        }
    }

    /// After lost packets or a loss of sync: drop the PES in progress and the buffer model.
    pub(crate) fn reset(&mut self) {
        self.in_pes = false;
        self.model = None;
        self.last_au_time = None;
        self.ac3.resync();
        self.m2v.reset_scan();
    }

    pub(crate) fn tstd_stats(&self, out: &mut Vec<crate::report::BufferStats>) {
        if let Some(m) = &self.model {
            m.stats(out);
        }
    }

    /// Runs the model to the end of the stream.
    pub(crate) fn finish(&mut self, t: f64, sink: &mut Sink) {
        if let Some(m) = &mut self.model {
            m.finish(t, sink);
        }
    }

    /// One packet of the PID. `payload` is `None` for packets that carry no payload or
    /// are duplicates (they still enter the transport buffer).
    pub(crate) fn packet(&mut self, payload: Option<&[u8]>, pusi: bool, ctx: &mut PktCtx<'_>) {
        let mut units = 0.0;
        if let Some(data) = payload {
            units = self.payload(data, pusi, ctx);
        }
        if let (Some(m), Some((t0, t1))) = (&mut self.model, ctx.arrival) {
            m.packet(t0, t1, units, ctx.sink);
        }
    }

    fn end_pes(&mut self, sink: &mut Sink) {
        if !self.in_pes {
            return;
        }
        self.in_pes = false;
        let p = Some(self.pid);
        if let Some(d) = self.declared
            && self.received != d
        {
            let got = self.received;
            sink.emit(Rule::PesLength, p, || {
                format!("PES_packet_length {d} but {got} bytes follow it before the next PES")
            });
        }
        if self.cur.pts.is_some()
            && self.au_in_pes == 0
            && matches!(self.kind, Kind::Mpeg2Video | Kind::Ac3)
            && self.hdr_done
        {
            sink.emit(Rule::PesPtsNoAu, p, || {
                format!(
                    "PES with a PTS in which no {} begins",
                    if self.kind == Kind::Ac3 {
                        "syncframe"
                    } else {
                        "picture"
                    }
                )
            });
        }
    }

    fn payload(&mut self, data: &[u8], pusi: bool, ctx: &mut PktCtx<'_>) -> f64 {
        let pid = self.pid;
        let mut rest = data;
        let mut header_units = 0usize;
        if pusi {
            self.end_pes(ctx.sink);
            if rest.len() < 3 || rest[..3] != [0, 0, 1] {
                ctx.sink.emit(Rule::PesStart, Some(pid), || {
                    format!(
                        "payload starts {:02X?} instead of a packet start code",
                        &rest[..rest.len().min(4)]
                    )
                });
                return 0.0;
            }
            self.in_pes = true;
            self.hdr.clear();
            self.hdr_done = false;
            self.declared = None;
            self.received = 0;
            self.cur = PesHdr::default();
            self.au_in_pes = 0;
            self.payload_seen = 0;
            self.first4.clear();
            self.pes_count += 1;
        } else if !self.in_pes {
            return 0.0;
        }
        if !self.hdr_done {
            // Collect the header (usually all in the first packet).
            loop {
                let want = match header_len(&self.hdr) {
                    Some(n) => n,
                    None if self.hdr.len() < 6 => 6,
                    None => 9,
                };
                if self.hdr.len() >= want {
                    break;
                }
                let take = (want - self.hdr.len()).min(rest.len());
                if take == 0 {
                    break;
                }
                self.hdr.extend_from_slice(&rest[..take]);
                header_units += take;
                rest = &rest[take..];
            }
            let complete = header_len(&self.hdr).is_some_and(|n| self.hdr.len() >= n);
            if !complete {
                return if self.model.is_some() && self.kind == Kind::Ac3 {
                    header_units as f64
                } else {
                    0.0
                };
            }
            self.hdr_done = true;
            let hdr = std::mem::take(&mut self.hdr);
            self.cur = parse_header(&hdr, self.kind, self.stream_type, pid, ctx.sink);
            self.received = hdr.len() - 6;
            if self.cur.length != 0 {
                self.declared = Some(usize::from(self.cur.length));
            }
            if self.cur.pts.is_some() && (self.kind.is_video() || self.kind.is_audio()) {
                self.pts_rep.occur(ctx.packet_pos, ctx.tb, ctx.sink);
            }
            if self.kind == Kind::Ac3
                && self.cur.align
                && self.ac3.frames > 0
                && self.ac3.misaligned()
            {
                ctx.sink.emit(Rule::EsAc3Sync, Some(pid), || {
                    "PES with data_alignment_indicator 1 does not begin with an AC-3 syncframe"
                        .into()
                });
            }
            self.maybe_start_model(ctx);
            self.hdr = hdr;
        }
        // Payload bytes of this PES in this packet.
        let allowed = match self.declared {
            Some(d) => d.saturating_sub(self.received).min(rest.len()),
            None => rest.len(),
        };
        self.received += rest.len();
        let body = &rest[..allowed];
        if self.first4.len() < 4 {
            let take = (4 - self.first4.len()).min(body.len());
            self.first4.extend_from_slice(&body[..take]);
            if self.first4.len() == 4
                && self.stream_type == 0x02
                && !(self.first4[..3] == [0, 0, 1] && matches!(self.first4[3], 0xB3 | 0xB8 | 0x00))
            {
                let f = self.first4.clone();
                ctx.sink.emit(Rule::PesVideoAlign, Some(pid), || {
                    format!("MPEG-2 video PES payload begins {f:02X?}, not with an access unit")
                });
            }
        }
        let fed_before = self.model.as_ref().map(|m| m.fed()).unwrap_or(0.0);
        let audio_units = self.kind == Kind::Ac3;
        let chunk_unit_base = fed_before
            + if audio_units {
                header_units as f64
            } else {
                0.0
            };
        let es_base = self.es_off;
        let mut ev = std::mem::take(&mut self.ev);
        ev.clear();
        match self.kind {
            Kind::Mpeg2Video => self.m2v.push(body, es_base, &mut ev),
            Kind::Avc => self.avc.push(body, es_base, &mut ev),
            Kind::Ac3 => self.ac3.push(body, es_base, &mut ev),
            _ => {}
        }
        self.es_off += body.len() as u64;
        for e in &ev {
            self.event(*e, es_base, chunk_unit_base, ctx);
        }
        self.ev = ev;
        self.payload_seen += body.len();
        if self.model.is_some() {
            if audio_units {
                (header_units + body.len()) as f64
            } else {
                body.len() as f64
            }
        } else {
            0.0
        }
    }

    fn maybe_start_model(&mut self, ctx: &mut PktCtx<'_>) {
        if self.model.is_some() || !ctx.model {
            return;
        }
        let Some((t0, _)) = ctx.arrival else {
            return;
        };
        let params = match self.kind {
            Kind::Mpeg2Video => {
                let s = self.m2v.seq;
                match s.profile_level {
                    Some(pl) => {
                        let p = tstd::m2v_params(pl, s.vbv_bits, s.bit_rate, s.low_delay);
                        if p.is_none() && !self.seq_noted {
                            self.seq_noted = true;
                            ctx.sink.emit(Rule::EsVideoSeq, Some(self.pid), || {
                                format!("profile_and_level_indication 0x{pl:02X}: MBn and EBn not modeled")
                            });
                        }
                        p
                    }
                    None => None,
                }
            }
            Kind::Avc => match (self.avc.profile_idc, self.avc.level_idc) {
                (Some(pr), Some(l)) if self.avc.auds > 0 => {
                    tstd::avc_params(pr, l, self.avc.constraint3)
                }
                _ => None,
            },
            Kind::Ac3 => self.ac3.last.map(|_| tstd::ac3_params()),
            _ => self.params,
        };
        let Some(params) = params else {
            return;
        };
        let timed = matches!(self.kind, Kind::Mpeg2Video | Kind::Avc | Kind::Ac3);
        if timed && (self.cur.pts.is_none() || ctx.clock.is_none()) {
            return;
        }
        self.params = Some(params);
        self.model = Some(EsModel::new(params, self.pid, t0));
        self.lead_pending = timed;
        self.last_au_time = None;
    }

    fn stc_to_stream(&self, ts90: u64, clock: Option<(f64, u64)>) -> Option<f64> {
        let (t, pcr) = clock?;
        Some(t + pcr_diff(ts90 * 300, pcr) as f64 / SYSTEM_CLOCK)
    }

    fn event(&mut self, e: EsEvent, es_base: u64, unit_base: f64, ctx: &mut PktCtx<'_>) {
        let unit = |off: u64| unit_base + (off as f64 - es_base as f64);
        match e {
            EsEvent::AuStart { off } => {
                let first_in_pes = self.au_in_pes == 0;
                self.au_in_pes += 1;
                let Some(model) = &self.model else {
                    return;
                };
                if model.p.mb.is_none() && !model.p.audio {
                    return;
                }
                let ts = if first_in_pes {
                    self.cur.dts.or(self.cur.pts)
                } else {
                    None
                };
                let time = match ts {
                    Some(ts) => self.stc_to_stream(ts, ctx.clock),
                    None => self.last_au_time.and_then(|t| {
                        let period = match self.kind {
                            Kind::Mpeg2Video if self.m2v.seq.frame_rate > 0.0 => {
                                let f = 1.0 / self.m2v.seq.frame_rate;
                                Some(if self.last_field { f / 2.0 } else { f })
                            }
                            Kind::Ac3 => self.ac3.last.map(|h| match h.fscod {
                                1 => 1536.0 / 44100.0,
                                2 => 1536.0 / 32000.0,
                                _ => 1536.0 / 48000.0,
                            }),
                            _ => None,
                        };
                        period.map(|p| t + p)
                    }),
                };
                self.last_field = false;
                let Some(time) = time else {
                    // Cannot time this access unit: stop modeling until the next timestamp.
                    self.model = None;
                    return;
                };
                self.last_au_time = Some(time);
                let arrival = ctx.arrival.map(|a| a.0).unwrap_or(time);
                let u = unit(off);
                let sink = &mut *ctx.sink;
                if let Some(m) = &mut self.model {
                    if self.lead_pending {
                        self.lead_pending = false;
                        if u > 0.5 {
                            m.au_start(0.0, time, arrival, sink);
                        }
                    }
                    m.au_start(u.max(0.0), time, arrival, sink);
                }
            }
            EsEvent::AuEnd { off } => {
                if let Some(m) = &mut self.model
                    && m.p.audio
                {
                    m.set_end(unit(off), ctx.sink);
                }
            }
            EsEvent::FieldPicture => self.last_field = true,
            EsEvent::Sequence => {
                // New buffer parameters: restart the model at the next timestamped PES.
                if self.model.is_some() {
                    let s = self.m2v.seq;
                    let new = s
                        .profile_level
                        .and_then(|pl| tstd::m2v_params(pl, s.vbv_bits, s.bit_rate, s.low_delay));
                    if new != self.params {
                        self.model = None;
                    }
                }
            }
            EsEvent::AvcSps => {}
            EsEvent::Ac3SyncLost { off: _, why } => {
                ctx.sink
                    .emit(Rule::EsAc3Sync, Some(self.pid), || format!("AC-3: {why}"));
                self.model = None;
            }
        }
        if self.kind == Kind::Ac3
            && let (Some(h), Some(d)) = (self.ac3.last, self.ac3_desc)
            && self.ac3_compared != Some(h)
        {
            self.ac3_compared = Some(h);
            compare_ac3(h, d, self.pid, ctx.sink);
        }
    }
}

fn tb_only_params(kind: Kind) -> Option<EsParams> {
    match kind {
        Kind::Eac3 => Some(EsParams {
            rx: tstd::RX_AUDIO,
            mb: None,
            eb: f64::INFINITY,
            max_delay: 1.0,
            low_delay: false,
            audio: false,
        }),
        _ => None,
    }
}

/// Compares an AC-3 descriptor with the stream (A/53-3 §5.8.1.1; A/52 §A4.3).
fn compare_ac3(h: crate::es::Ac3Header, d: Ac3Desc, pid: u16, sink: &mut Sink) {
    let mut bad: Vec<String> = Vec::new();
    if h.bsmod != d.bsmod {
        bad.push(format!(
            "bsmod {} in the descriptor, {} in the stream",
            d.bsmod, h.bsmod
        ));
    }
    if h.bsid != d.bsid {
        bad.push(format!(
            "bsid {} in the descriptor, {} in the stream",
            d.bsid, h.bsid
        ));
    }
    let src_ok = match d.sample_rate_code {
        0 => h.fscod == 0,
        1 => h.fscod == 1,
        2 => h.fscod == 2,
        4 => h.fscod <= 1,
        5 => h.fscod == 0 || h.fscod == 2,
        6 => h.fscod >= 1,
        7 => true,
        _ => true,
    };
    if !src_ok {
        bad.push(format!(
            "sample_rate_code {:03b} does not include the stream's fscod {}",
            d.sample_rate_code, h.fscod
        ));
    }
    if d.num_channels < 8 && d.num_channels != 0 && h.acmod != d.num_channels {
        bad.push(format!(
            "num_channels {} but the stream's acmod is {}",
            d.num_channels, h.acmod
        ));
    }
    let kbps = crate::es::AC3_KBPS[usize::from(h.frmsizecod / 2)];
    if d.bit_rate_code < 32 {
        let want = crate::es::AC3_KBPS
            .get(usize::from(d.bit_rate_code))
            .copied();
        if want != Some(kbps) {
            bad.push(format!(
                "bit_rate_code {} ({want:?} kbit/s exact) but the stream is {kbps} kbit/s",
                d.bit_rate_code
            ));
        }
    } else if let Some(max) = crate::es::AC3_KBPS.get(usize::from(d.bit_rate_code - 32))
        && kbps > *max
    {
        bad.push(format!(
            "bit_rate_code {} (at most {max} kbit/s) but the stream is {kbps} kbit/s",
            d.bit_rate_code
        ));
    }
    for m in bad {
        sink.emit(Rule::DescAc3Stream, Some(pid), || {
            format!("AC-3 descriptor: {m}")
        });
    }
}

/// The stream time at which `ts` (90 kHz) falls, given a program clock (used by tests).
#[allow(dead_code)]
pub(crate) fn ts_time(ts90: u64, clock: (f64, u64)) -> f64 {
    clock.0 + pcr_diff(ts90 * 300, clock.1) as f64 / SYSTEM_CLOCK
}
