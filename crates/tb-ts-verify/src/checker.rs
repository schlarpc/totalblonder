//! The checker: transport-stream bytes in, findings and statistics out.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::bytes::{FNV0, fnv, lang};
use crate::crc::crc32;
use crate::pes::{EsState, PktCtx};
use crate::psi::{self, Ac3Desc, Hdr, Kind, Pmt};
use crate::psip::{self, Channel, Event, MgtEntry, TableType};
use crate::report::{Finding, PidStats, Position, Report, Sink, Stats};
use crate::rules::{Rule, Severity};
use crate::section::{Assembler, SectionOut};
use crate::time::{PcrParams, PcrTrack, Repeat, TimeBase};
use crate::tstd::{Leaky, SB_LEAK, SB_SIZE, SysModel};

/// What to check and how.
#[derive(Debug, Clone)]
pub struct Options {
    /// The nominal transport rate in bit/s (for this project 38,800,000). Without it the rate
    /// is estimated from the PCRs.
    pub rate: Option<f64>,
    /// The transport_stream_id the PAT must carry.
    pub tsid: Option<u16>,
    /// Findings kept with position and message, per rule.
    pub max_examples: usize,
    /// The PCR interval this project targets, ms (rule PCR-INTERVAL-TARGET).
    pub pcr_target_ms: f64,
    /// How long an elementary or PCR PID named in a PMT may go without packets, s.
    pub pid_timeout_s: f64,
    /// How long an EIT or ETT PID named in the MGT may go without packets, s.
    pub psip_pid_timeout_s: f64,
    /// Run the buffer model (T-STD and PSIP smoothing buffer).
    pub buffer_model: bool,
    /// Keep up to this many recent findings for [`Checker::drain_events`] (0: none).
    pub events_capacity: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rate: None,
            tsid: None,
            max_examples: 8,
            pcr_target_ms: 30.0,
            pid_timeout_s: 1.0,
            psip_pid_timeout_s: 60.0,
            buffer_model: true,
            events_capacity: 0,
        }
    }
}

static PAT_LIMITS: [(f64, Rule); 2] = [(0.5, Rule::PatIntervalTr), (0.1, Rule::PatInterval)];
static PMT_LIMITS: [(f64, Rule); 2] = [(0.5, Rule::PmtIntervalTr), (0.4, Rule::PmtInterval)];
static MGT_LIMITS: [(f64, Rule); 1] = [(0.15, Rule::MgtInterval)];
static VCT_LIMITS: [(f64, Rule); 1] = [(0.4, Rule::VctInterval)];
static STT_LIMITS: [(f64, Rule); 2] = [(10.0, Rule::SttInterval), (1.0, Rule::SttInterval1s)];
static RRT_LIMITS: [(f64, Rule); 1] = [(60.0, Rule::RrtInterval)];
static EIT0_LIMITS: [(f64, Rule); 1] = [(0.5, Rule::Eit0Interval)];

/// What a PID carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Unknown,
    Pat,
    Cat,
    Tsdt,
    Pmt,
    Es,
    PcrOnly,
    Psip,
    Eit(u8),
    Ett(Option<u8>),
    Ca,
    Null,
}

impl Role {
    fn sections(self) -> bool {
        matches!(
            self,
            Role::Pat | Role::Cat | Role::Pmt | Role::Psip | Role::Eit(_) | Role::Ett(_)
        )
    }

    fn psip(self) -> bool {
        matches!(self, Role::Psip | Role::Eit(_) | Role::Ett(_))
    }
}

/// PSIP rate in 100 ms slices.
#[derive(Debug, Clone, Default)]
struct RateSlices {
    cur: u64,
    ring: [u32; 10],
    over: bool,
}

#[derive(Debug, Clone)]
struct PidState {
    pid: u16,
    role: Role,
    packets: u64,
    scrambled: u64,
    cc_errors: u64,
    first_pos: u64,
    last_pos: Option<u64>,
    cc: Option<u8>,
    last_had_payload: bool,
    dup_count: u8,
    last_pkt: Option<Box<[u8; 188]>>,
    di_pending: bool,
    asm: Option<Assembler>,
    es: Option<Box<EsState>>,
    sb: Option<Leaky>,
    rate: Option<RateSlices>,
    unref_reported: bool,
    missing_latched: bool,
    cat_reported: bool,
    psi_af_reported: bool,
    cycled: bool,
    seen_keys: HashSet<u64>,
}

impl PidState {
    fn new(pid: u16, pos: u64) -> Self {
        Self {
            pid,
            role: Role::Unknown,
            packets: 0,
            scrambled: 0,
            cc_errors: 0,
            first_pos: pos,
            last_pos: None,
            cc: None,
            last_had_payload: false,
            dup_count: 0,
            last_pkt: None,
            di_pending: false,
            asm: None,
            es: None,
            sb: None,
            rate: None,
            unref_reported: false,
            missing_latched: false,
            cat_reported: false,
            psi_af_reported: false,
            cycled: false,
            seen_keys: HashSet::new(),
        }
    }

    fn reset_data(&mut self) {
        if let Some(a) = &mut self.asm {
            a.reset();
        }
        if let Some(e) = &mut self.es {
            e.reset();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TKey {
    pid: u16,
    table_id: u8,
    inst: u32,
    current: bool,
}

#[derive(Debug, Clone, Copy)]
struct SecInfo {
    hash: u64,
    len: usize,
    seen: u32,
}

#[derive(Debug, Clone)]
struct TState {
    version: u8,
    last: u8,
    sections: BTreeMap<u8, SecInfo>,
    last_pos: u64,
}

#[derive(Debug, Clone)]
struct PmtInfo {
    pmt: Pmt,
    ac3: BTreeMap<u16, Ac3Desc>,
}

#[derive(Debug, Clone, Default)]
struct PatState {
    tsid: u16,
    version: u8,
    last: u8,
    sections: BTreeMap<u8, Vec<(u16, u16)>>,
    programs: BTreeMap<u16, u16>,
    complete: bool,
}

#[derive(Debug, Clone, Default)]
struct VctState {
    table_id: u8,
    tsid: u16,
    version: u8,
    sections: BTreeMap<u8, Vec<Channel>>,
}

#[derive(Debug, Clone, Default)]
struct EitInst {
    version: u8,
    last: u8,
    sections: BTreeMap<u8, Vec<Event>>,
    last_pos: u64,
}

#[derive(Debug, Clone, Copy)]
struct SttState {
    pos: u64,
    system_time: u32,
    offset: u8,
}

#[derive(Debug, Clone, Copy)]
struct RefInfo {
    since: u64,
    timeout: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Af {
    di: bool,
    pcr: Option<u64>,
}

/// A streaming checker of one transport stream.
///
/// Feed it bytes with [`push`](Self::push) in any chunking; read the state with
/// [`report`](Self::report) at any time, or end the stream with [`finish`](Self::finish).
/// Memory is bounded: per-PID state, the tables on air, and the kept findings.
#[derive(Debug)]
pub struct Checker {
    opts: Options,
    sink: Sink,
    tb: TimeBase,
    carry: Vec<u8>,
    base: u64,
    in_sync: bool,
    ever_synced: bool,
    bad_syncs: u32,
    packets: u64,
    nulls: u64,
    out_of_sync: u64,
    last_pos: u64,
    pid_slot: Vec<u16>,
    pids: Vec<PidState>,
    tables: HashMap<TKey, TState>,
    repeats: BTreeMap<(u16, u8, u32, u8), Repeat>,
    pat: PatState,
    pat_seen: bool,
    pmts: BTreeMap<u16, PmtInfo>,
    cat_seen: bool,
    ca_pids: BTreeSet<u16>,
    mgt: Option<(u8, Vec<MgtEntry>)>,
    mgt_seen_pos: Option<u64>,
    vct: Option<VctState>,
    vct_seen: bool,
    stt: Option<SttState>,
    stt_seen: bool,
    stt_body: u64,
    eit: BTreeMap<(u16, u16), EitInst>,
    ett: BTreeMap<u16, BTreeMap<u32, u64>>,
    ett_ext: BTreeMap<u16, BTreeMap<u16, u32>>,
    ett_ext_reported: BTreeSet<u16>,
    clocks: BTreeMap<u16, (f64, u64)>,
    clock_rejects: BTreeMap<u16, u32>,
    sys: BTreeMap<u16, SysModel>,
    pcr: BTreeMap<u16, PcrTrack>,
    refs: BTreeMap<u16, RefInfo>,
    xcheck_due: Option<u64>,
    reported: HashSet<u64>,
    mgt_mismatch: BTreeMap<u16, u64>,
    next_tick: u64,
    next_slow: f64,
    psip_required_reported: u8,
    sections: Vec<SectionOut>,
    unlisted_reported: BTreeSet<(u8, u32)>,
}

const TICK_PACKETS: u64 = 1024;

impl Checker {
    /// A checker with these options.
    pub fn new(opts: Options) -> Self {
        let sink = Sink::new(opts.max_examples, opts.events_capacity);
        let tb = TimeBase::new(opts.rate);
        let mut c = Self {
            opts,
            sink,
            tb,
            carry: Vec::new(),
            base: 0,
            in_sync: false,
            ever_synced: false,
            bad_syncs: 0,
            packets: 0,
            nulls: 0,
            out_of_sync: 0,
            last_pos: 0,
            pid_slot: vec![u16::MAX; 8192],
            pids: Vec::new(),
            tables: HashMap::new(),
            repeats: BTreeMap::new(),
            pat: PatState::default(),
            pat_seen: false,
            pmts: BTreeMap::new(),
            cat_seen: false,
            ca_pids: BTreeSet::new(),
            mgt: None,
            mgt_seen_pos: None,
            vct: None,
            vct_seen: false,
            stt: None,
            stt_seen: false,
            stt_body: 0,
            eit: BTreeMap::new(),
            ett: BTreeMap::new(),
            ett_ext: BTreeMap::new(),
            ett_ext_reported: BTreeSet::new(),
            clocks: BTreeMap::new(),
            clock_rejects: BTreeMap::new(),
            sys: BTreeMap::new(),
            pcr: BTreeMap::new(),
            refs: BTreeMap::new(),
            xcheck_due: None,
            reported: HashSet::new(),
            mgt_mismatch: BTreeMap::new(),
            next_tick: TICK_PACKETS,
            next_slow: 1.0,
            psip_required_reported: 0,
            sections: Vec::new(),
            unlisted_reported: BTreeSet::new(),
        };
        let mut pat = Repeat::new("PAT".into(), 0, &PAT_LIMITS);
        pat.expect_from(0);
        c.repeats.insert((0, 0, 0, 0), pat);
        c
    }

    /// Feeds bytes of the stream.
    pub fn push(&mut self, data: &[u8]) {
        let mut owned = std::mem::take(&mut self.carry);
        let consumed;
        let rest: Vec<u8>;
        if owned.is_empty() {
            consumed = self.process(data);
            rest = data[consumed..].to_vec();
        } else {
            owned.extend_from_slice(data);
            consumed = self.process(&owned);
            rest = owned[consumed..].to_vec();
            owned.clear();
        }
        self.base += consumed as u64;
        owned.extend_from_slice(&rest);
        self.carry = owned;
    }

    /// Takes the findings recorded since the last call (only with `events_capacity` > 0).
    pub fn drain_events(&mut self) -> Vec<Finding> {
        self.sink.drain_events()
    }

    /// The number of violations of each rule so far.
    pub fn counts(&self) -> &BTreeMap<Rule, u64> {
        &self.sink.counts
    }

    fn process(&mut self, input: &[u8]) -> usize {
        let mut i = 0;
        loop {
            if self.in_sync {
                if input.len() - i < 188 {
                    return i;
                }
                let pos = self.base + i as u64;
                if input[i] != 0x47 {
                    self.sink.pos = Position {
                        byte: pos,
                        packet: pos / 188,
                        pid: None,
                    };
                    self.sink.time = self.tb.dur(pos);
                    let b = input[i];
                    self.sink.emit(Rule::TsSyncByte, None, || {
                        format!("byte 0x{b:02X} where the sync byte was due")
                    });
                    self.bad_syncs += 1;
                    if self.bad_syncs >= 2 {
                        self.sink.emit(Rule::TsSyncLoss, None, || {
                            "two consecutive corrupted sync bytes: synchronization lost".into()
                        });
                        self.in_sync = false;
                        continue;
                    }
                    i += 188;
                    self.packets += 1;
                    continue;
                }
                self.bad_syncs = 0;
                let mut pkt = [0u8; 188];
                pkt.copy_from_slice(&input[i..i + 188]);
                self.packet(&pkt, pos);
                i += 188;
            } else {
                // Hunt for five consecutive sync bytes.
                let need = 4 * 188 + 1;
                let mut found = None;
                let mut o = i;
                while input.len() - o >= need {
                    if (0..5).all(|k| input[o + k * 188] == 0x47) {
                        found = Some(o);
                        break;
                    }
                    o += 1;
                }
                match found {
                    Some(o) => {
                        self.out_of_sync += (o - i) as u64;
                        self.in_sync = true;
                        let pos = self.base + o as u64;
                        if self.ever_synced {
                            self.resync(pos);
                        }
                        self.ever_synced = true;
                        i = o;
                    }
                    None => {
                        let keep = (input.len() - i).min(need - 1);
                        let skip = input.len() - i - keep;
                        self.out_of_sync += skip as u64;
                        return i + skip;
                    }
                }
            }
        }
    }

    /// After a loss of sync: other indicators are invalid across it, so start afresh.
    fn resync(&mut self, pos: u64) {
        for st in &mut self.pids {
            st.cc = None;
            st.dup_count = 0;
            st.reset_data();
        }
        for r in self.repeats.values_mut() {
            r.rebase(pos);
        }
        for p in self.pcr.values_mut() {
            p.rebase(pos);
        }
        for (pid, r) in &mut self.refs {
            let _ = pid;
            r.since = r.since.max(pos);
        }
        for st in &mut self.pids {
            if let Some(es) = &mut st.es {
                es.pts_rep.rebase(pos);
            }
        }
    }

    fn slot(&mut self, pid: u16, pos: u64) -> usize {
        let s = self.pid_slot[usize::from(pid)];
        if s != u16::MAX {
            return usize::from(s);
        }
        let idx = self.pids.len();
        let mut st = PidState::new(pid, pos);
        st.role = match pid {
            0 => Role::Pat,
            1 => Role::Cat,
            2 => Role::Tsdt,
            0x1FFB => Role::Psip,
            0x1FFF => Role::Null,
            _ => Role::Unknown,
        };
        self.pids.push(st);
        self.pid_slot[usize::from(pid)] = idx as u16;
        idx
    }

    fn packet(&mut self, pkt: &[u8; 188], pos: u64) {
        self.packets += 1;
        self.last_pos = pos + 188;
        let tei = pkt[1] & 0x80 != 0;
        let pusi = pkt[1] & 0x40 != 0;
        let pid = (u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]);
        let tsc = pkt[3] >> 6;
        let afc = (pkt[3] >> 4) & 3;
        let cc = pkt[3] & 0x0F;
        self.sink.pos = Position {
            byte: pos,
            packet: pos / 188,
            pid: Some(pid),
        };
        self.sink.time = self.tb.dur(pos);
        let idx = self.slot(pid, pos);
        {
            let st = &mut self.pids[idx];
            st.packets += 1;
            st.last_pos = Some(pos);
            st.missing_latched = false;
        }
        if self.packets >= self.next_tick {
            self.next_tick = self.packets + TICK_PACKETS;
            self.tick(pos, false);
        }
        if tei {
            self.sink.emit(Rule::TsTei, Some(pid), || {
                "transport_error_indicator set".into()
            });
            let st = &mut self.pids[idx];
            st.cc = None;
            st.reset_data();
            return;
        }
        if pid == 0x1FFF {
            self.nulls += 1;
            if pusi || tsc != 0 || afc != 1 {
                self.sink.emit(Rule::TsNull, Some(pid), || {
                    format!("null packet with PUSI {}, scrambling {tsc:02b}, adaptation_field_control {afc:02b}", u8::from(pusi))
                });
            }
            return;
        }
        if afc == 0 {
            self.sink.emit(Rule::TsAfcReserved, Some(pid), || {
                "adaptation_field_control 00".into()
            });
            return;
        }
        if (0x0004..=0x000F).contains(&pid) && self.pids[idx].packets == 1 {
            self.sink.emit(Rule::TsPidReserved, Some(pid), || {
                "reserved PID in use".into()
            });
        }
        if (0x0010..=0x0014).contains(&pid) && self.pids[idx].packets == 1 {
            self.sink.emit(Rule::TsDvbPid, Some(pid), || {
                "a DVB SI PID carries packets".into()
            });
        }
        if pid == 0x1FFE && self.pids[idx].packets == 1 {
            self.sink.emit(Rule::PidReservedScte, Some(pid), || {
                "PID 0x1FFE (reserved for DOCSIS) carries packets".into()
            });
        }
        // Adaptation field.
        let mut af = Af::default();
        let mut payload_off = 4usize;
        if afc & 2 != 0 {
            let len = usize::from(pkt[4]);
            if (afc == 2 && len != 183) || (afc == 3 && len > 182) {
                self.sink.emit(Rule::TsAfLength, Some(pid), || {
                    format!("adaptation_field_length {len} with adaptation_field_control {afc:02b}")
                });
            }
            if len > 183 {
                return;
            }
            payload_off = 5 + len;
            if len > 0 {
                let a = &pkt[5..5 + len];
                let flags = a[0];
                af.di = flags & 0x80 != 0;
                let has_pcr = flags & 0x10 != 0;
                let has_opcr = flags & 0x08 != 0;
                let mut need = 1 + 6 * usize::from(has_pcr) + 6 * usize::from(has_opcr);
                need += usize::from(flags & 0x04 != 0);
                if flags & 0x02 != 0 {
                    need += 1 + usize::from(a.get(need).copied().unwrap_or(0));
                }
                if flags & 0x01 != 0 {
                    need += 1 + usize::from(a.get(need).copied().unwrap_or(0));
                }
                if need > len {
                    self.sink.emit(Rule::TsAfLength, Some(pid), || {
                        format!("adaptation field flags need {need} bytes, adaptation_field_length is {len}")
                    });
                }
                if has_pcr && len >= 7 {
                    let b = &a[1..7];
                    let base = (u64::from(b[0]) << 25)
                        | (u64::from(b[1]) << 17)
                        | (u64::from(b[2]) << 9)
                        | (u64::from(b[3]) << 1)
                        | u64::from(b[4] >> 7);
                    let ext = (u64::from(b[4] & 1) << 8) | u64::from(b[5]);
                    af.pcr = Some(base * 300 + ext);
                }
                if has_opcr && !has_pcr {
                    self.sink.emit(Rule::TsOpcr, Some(pid), || {
                        "OPCR without PCR in the same packet".into()
                    });
                }
            }
        }
        let payload: &[u8] = if afc & 1 != 0 {
            &pkt[payload_off.min(188)..]
        } else {
            &[]
        };
        if af.di {
            self.sink.emit(Rule::TsDiscontinuity, Some(pid), || {
                "discontinuity_indicator set".into()
            });
            self.pids[idx].di_pending = true;
        }
        // Continuity.
        let dup = self.continuity(idx, pkt, afc, cc, af.di);
        let role = self.pids[idx].role;
        // Scrambling and adaptation fields by role.
        if tsc != 0 {
            self.pids[idx].scrambled += 1;
            match role {
                Role::Pat => self.sink.emit(Rule::PatScrambled, Some(pid), || {
                    format!("transport_scrambling_control {tsc:02b} on the PAT PID")
                }),
                Role::Pmt => self.sink.emit(Rule::PmtScrambled, Some(pid), || {
                    format!("transport_scrambling_control {tsc:02b} on a PMT PID")
                }),
                r if r.psip() => self.sink.emit(Rule::PsipScrambled, Some(pid), || {
                    format!("transport_scrambling_control {tsc:02b} on a PSIP PID")
                }),
                _ => {
                    if !self.cat_seen && !self.pids[idx].cat_reported {
                        self.pids[idx].cat_reported = true;
                        self.sink.emit(Rule::CatError, Some(pid), || {
                            "scrambled packets but no CAT".into()
                        });
                    }
                }
            }
        }
        if afc & 2 != 0 {
            match role {
                Role::Pat | Role::Pmt => {
                    let only_di = pkt[4] >= 1 && pkt[5] & 0x7F == 0 && af.di;
                    if !only_di && !self.pids[idx].psi_af_reported {
                        self.pids[idx].psi_af_reported = true;
                        self.sink.emit(Rule::PsiAdaptation, Some(pid), || {
                            "adaptation field on a PAT or PMT PID for other than discontinuity_indicator".into()
                        });
                    }
                }
                r if r.psip() => self.sink.emit(Rule::PsipAf, Some(pid), || {
                    format!("adaptation_field_control {afc:02b} on a PSIP PID (shall be 01)")
                }),
                _ => {}
            }
        }
        if let Some(pcr) = af.pcr {
            self.on_pcr(pid, pos, pcr, af.di);
        }
        let arrival = self.tb.dur(pos).zip(self.tb.dur(pos + 188));
        // System data buffers.
        if self.opts.buffer_model
            && let Some((t0, t1)) = arrival
        {
            let rsys = self
                .tb
                .rate()
                .map(|r| (r / 500.0).max(80_000.0))
                .unwrap_or(80_000.0);
            let pl = if dup { 0.0 } else { payload.len() as f64 };
            if pid <= 3 {
                for m in self.sys.values_mut() {
                    m.packet(t0, t1, pl, rsys, pid, &mut self.sink);
                }
            } else if role == Role::Pmt {
                for m in self.sys.values_mut().filter(|m| m.pmt_pid == pid) {
                    m.packet(t0, t1, pl, rsys, pid, &mut self.sink);
                }
            }
            if role.psip() {
                let st = &mut self.pids[idx];
                let sb = st
                    .sb
                    .get_or_insert_with(|| Leaky::new(SB_LEAK, SB_SIZE, t0));
                let s = sb.packet(t0, t1, 188.0);
                if sb.overflowed(&s) {
                    let fill = s.fill;
                    self.sink.emit(Rule::PsipSbOverflow, Some(pid), || {
                        format!(
                            "smoothing buffer holds {fill:.0} bytes (size 1024, leak 250 kbit/s)"
                        )
                    });
                }
            }
        }
        if role.psip()
            && let Some((t0, _)) = arrival
        {
            self.psip_rate(idx, t0);
        }
        // Payload.
        if role.sections() && !dup && !payload.is_empty() {
            let mut out = std::mem::take(&mut self.sections);
            out.clear();
            let st = &mut self.pids[idx];
            let asm = st.asm.get_or_insert_with(Assembler::default);
            asm.push(
                payload,
                pusi,
                pos + payload_off as u64,
                pid,
                &mut self.sink,
                &mut out,
            );
            for s in &out {
                self.on_section(s, pid, role);
            }
            self.sections = out;
        } else if role == Role::Es || self.pids[idx].es.is_some() {
            let st = &mut self.pids[idx];
            if let Some(es) = st.es.as_deref_mut() {
                let clock = self.clocks.get(&es.program).copied();
                let mut ctx = PktCtx {
                    sink: &mut self.sink,
                    tb: &self.tb,
                    clock,
                    arrival,
                    packet_pos: pos,
                    model: self.opts.buffer_model,
                };
                let pl = if dup || payload.is_empty() {
                    None
                } else {
                    Some(payload)
                };
                es.packet(pl, pusi, &mut ctx);
            }
        }
    }

    /// Checks the continuity counter; returns true for a duplicate packet.
    fn continuity(&mut self, idx: usize, pkt: &[u8; 188], afc: u8, cc: u8, di: bool) -> bool {
        let has_payload = afc & 1 != 0;
        let st = &mut self.pids[idx];
        let pid = st.pid;
        let Some(last) = st.cc else {
            st.cc = Some(cc);
            st.last_had_payload = has_payload;
            if has_payload {
                st.last_pkt
                    .get_or_insert_with(|| Box::new([0; 188]))
                    .copy_from_slice(pkt);
            }
            return false;
        };
        if !has_payload {
            if cc != last && !di {
                st.cc_errors += 1;
                st.cc = Some(cc);
                self.sink.emit(Rule::CcError, Some(pid), || {
                    format!("continuity_counter {cc} in an adaptation-only packet after {last} (it shall not increment)")
                });
            }
            st.last_had_payload = false;
            st.dup_count = 0;
            return false;
        }
        if cc == last && st.last_had_payload && !di {
            st.dup_count += 1;
            if st.dup_count >= 2 {
                st.cc_errors += 1;
                let n = st.dup_count + 1;
                self.sink.emit(Rule::CcDuplicate, Some(pid), || {
                    format!("packet with continuity_counter {cc} sent {n} times")
                });
            } else if let Some(prev) = &st.last_pkt {
                let pcr_both = pkt[3] & 0x20 != 0
                    && prev[3] & 0x20 != 0
                    && pkt[4] >= 7
                    && prev[4] >= 7
                    && pkt[5] & 0x10 != 0
                    && prev[5] & 0x10 != 0;
                let same = if pcr_both {
                    pkt[..6] == prev[..6] && pkt[12..] == prev[12..]
                } else {
                    pkt[..] == prev[..]
                };
                if !same {
                    self.sink.emit(Rule::CcDupContent, Some(pid), || {
                        format!("two packets with continuity_counter {cc} differ: a duplicate shall repeat every byte")
                    });
                }
            }
            return true;
        }
        st.dup_count = 0;
        let expected = (last + 1) & 0x0F;
        if cc != expected && !di {
            st.cc_errors += 1;
            let lost = (cc + 16 - expected) & 0x0F;
            self.sink.emit(Rule::CcError, Some(pid), || {
                format!(
                    "continuity_counter {cc} after {last}: {lost} packet(s) missing or out of order"
                )
            });
            st.reset_data();
        }
        st.cc = Some(cc);
        st.last_had_payload = true;
        st.last_pkt
            .get_or_insert_with(|| Box::new([0; 188]))
            .copy_from_slice(pkt);
        false
    }

    fn on_pcr(&mut self, pid: u16, pos: u64, pcr: u64, di: bool) {
        if (0x0002..=0x000F).contains(&pid) {
            self.sink.emit(Rule::TsPcrPid, Some(pid), || {
                "PCR on a PID that may not carry one".into()
            });
        }
        let at = pos + 10;
        self.tb.pcr(pid, at, pcr, di);
        let params = PcrParams {
            target_ms: self.opts.pcr_target_ms,
            nominal: self.opts.rate,
        };
        let track = self.pcr.entry(pid).or_insert_with(|| PcrTrack::new(pid));
        track.pcr(at, pos, pcr, di, &self.tb, params, &mut self.sink, true);
        if let Some(t) = self.tb.dur(at) {
            for (prog, info) in &self.pmts {
                if info.pmt.pcr_pid != pid {
                    continue;
                }
                // A PCR more than 1 ms off the clock's extrapolation, without a
                // discontinuity, is taken as a bad value, not as the program's time.
                let keep = di
                    || self.clocks.get(prog).is_none_or(|&(t0, p0)| {
                        let want = (t - t0) * crate::time::SYSTEM_CLOCK;
                        (crate::time::pcr_diff(pcr, p0) as f64 - want).abs()
                            < 0.001 * crate::time::SYSTEM_CLOCK
                    });
                let rejects = self.clock_rejects.entry(*prog).or_insert(0);
                if keep || *rejects >= 2 {
                    *rejects = 0;
                    self.clocks.insert(*prog, (t, pcr));
                } else {
                    *rejects += 1;
                }
            }
        }
    }

    fn psip_rate(&mut self, idx: usize, t: f64) {
        let pid = self.pids[idx].pid;
        let slice = (t / 0.1) as u64;
        let rs = self.pids[idx].rate.get_or_insert_with(|| RateSlices {
            cur: slice,
            ..RateSlices::default()
        });
        // (Time can step back while an estimated rate settles: then stay in the slice.)
        if slice > rs.cur {
            // Close the slices from cur to slice - 1.
            let steps = (slice - rs.cur).min(10);
            let mut emit = None;
            for k in 0..steps {
                if k == 0 {
                    let bits: u64 = rs.ring.iter().map(|&b| u64::from(b)).sum::<u64>() * 8;
                    if bits > 250_000 {
                        if !rs.over {
                            emit = Some(bits);
                        }
                        rs.over = true;
                    } else {
                        rs.over = false;
                    }
                }
                rs.ring.rotate_left(1);
                rs.ring[9] = 0;
            }
            if slice - rs.cur > 10 {
                rs.ring = [0; 10];
            }
            rs.cur = slice;
            if let Some(bits) = emit {
                self.sink.emit(Rule::PsipRate, Some(pid), || {
                    format!("{bits} bit in the last second (limit 250,000)")
                });
            }
        }
        if let Some(rs) = &mut self.pids[idx].rate {
            rs.ring[9] += 188;
        }
    }

    // ------------------------------------------------------------------ sections

    fn on_section(&mut self, sec: &SectionOut, pid: u16, role: Role) {
        let d = &sec.data;
        let tid = d[0];
        let p = Some(pid);
        match role {
            Role::Pat if tid != 0x00 => {
                self.sink.emit(Rule::PatTableId, p, || {
                    format!("table_id 0x{tid:02X} on PID 0x0000")
                });
                return;
            }
            Role::Cat if tid != 0x01 => {
                self.sink.emit(Rule::CatError, p, || {
                    format!("table_id 0x{tid:02X} on PID 0x0001")
                });
                return;
            }
            Role::Psip if !matches!(tid, 0xC7..=0xCA | 0xCD | 0xD3 | 0xD4 | 0xD8) => {
                self.sink.emit(Rule::SectTablePid, p, || {
                    format!("table_id 0x{tid:02X} on PID 0x1FFB")
                });
                return;
            }
            Role::Eit(_) if tid != 0xCB => {
                self.sink.emit(Rule::MgtPidTable, p, || {
                    format!("table_id 0x{tid:02X} on an EIT PID of the MGT")
                });
                return;
            }
            Role::Ett(_) if tid != 0xCC => {
                self.sink.emit(Rule::MgtPidTable, p, || {
                    format!("table_id 0x{tid:02X} on an ETT PID of the MGT")
                });
                return;
            }
            Role::Pmt if tid != 0x02 => {
                if matches!(tid, 0x00 | 0x01 | 0xC7..=0xCD) {
                    self.sink.emit(Rule::SectTablePid, p, || {
                        format!("table_id 0x{tid:02X} on a PMT PID")
                    });
                }
                return;
            }
            _ => {}
        }
        let known_long = matches!(tid, 0x00 | 0x01 | 0x02 | 0xC7..=0xCD);
        let ssi = d[1] & 0x80 != 0;
        if !ssi {
            if known_long {
                self.sink.emit(Rule::SectSyntax, p, || {
                    format!("table_id 0x{tid:02X} with section_syntax_indicator 0")
                });
            }
            return;
        }
        if d.len() < 12 {
            self.sink.emit(Rule::SectLength, p, || {
                format!(
                    "table_id 0x{tid:02X}: section of {} bytes is too short",
                    d.len()
                )
            });
            return;
        }
        if crc32(d) != 0 {
            self.sink.emit(Rule::SectCrc, p, || {
                format!("CRC_32 error in a section with table_id 0x{tid:02X}")
            });
            return;
        }
        let Some(h) = Hdr::parse(d) else {
            return;
        };
        let max = match tid {
            0x00 | 0x01 | 0x02 | 0xC8 | 0xC9 | 0xCA | 0xCD => 1021,
            _ => 4093,
        };
        if h.length > max {
            let l = h.length;
            self.sink.emit(Rule::SectLength, p, || {
                format!("table_id 0x{tid:02X}: section_length {l} exceeds {max}")
            });
        }
        let psip = tid >= 0xC7;
        if known_long && psip != h.bit1 {
            self.sink.emit(Rule::SectSyntax, p, || {
                if psip {
                    format!("table_id 0x{tid:02X}: private_indicator 0 (shall be 1)")
                } else {
                    format!("table_id 0x{tid:02X}: the bit after section_syntax_indicator is 1 (shall be 0)")
                }
            });
        }
        if h.number > h.last {
            self.sink.emit(Rule::SectSyntax, p, || {
                format!(
                    "table_id 0x{tid:02X}: section_number {} above last_section_number {}",
                    h.number, h.last
                )
            });
        }
        if matches!(tid, 0x02 | 0xC7 | 0xCA | 0xCC | 0xCD) && (h.number != 0 || h.last != 0) {
            self.sink.emit(Rule::SectSingle, p, || {
                format!(
                    "table_id 0x{tid:02X}: section_number {} / last_section_number {} in a one-section table",
                    h.number, h.last
                )
            });
        }
        if psip {
            if d[8] != 0 {
                let v = d[8];
                self.sink.emit(Rule::PsipProtocol, p, || {
                    format!("table_id 0x{tid:02X}: protocol_version {v}")
                });
            }
            if matches!(tid, 0xC7 | 0xCA | 0xCB | 0xCC | 0xCD) && !h.current {
                self.sink.emit(Rule::SectCurrentNext, p, || {
                    format!("table_id 0x{tid:02X} with current_next_indicator 0")
                });
            }
        }
        self.sink.feature(match tid {
            0x00 => "section PAT",
            0x01 => "section CAT",
            0x02 => "section PMT",
            0xC7 => "section MGT",
            0xC8 => "section TVCT",
            0xC9 => "section CVCT",
            0xCA => "section RRT",
            0xCB => "section EIT",
            0xCC => "section ETT",
            0xCD => "section STT",
            _ => "section other",
        });
        match tid {
            0x00 => self.on_pat(sec, &h),
            0x01 => self.on_cat(sec, &h),
            0x02 => self.on_pmt(sec, &h, pid),
            0xC7 => self.on_mgt(sec, &h),
            0xC8 | 0xC9 => self.on_vct(sec, &h),
            0xCA => self.on_rrt(sec, &h),
            0xCB => self.on_eit(sec, &h, pid, role),
            0xCC => self.on_ett(sec, &h, pid, role),
            0xCD => self.on_stt(sec, &h),
            _ => {}
        }
    }

    /// Records a section of a table; returns true when its content is new.
    fn track(&mut self, pid: u16, inst: u32, h: &Hdr, d: &[u8], end_pos: u64) -> bool {
        let tid = h.table_id;
        let mut hash = fnv(&d[..5], FNV0);
        hash = fnv(&[d[5] & 0xC1], hash);
        hash = fnv(&d[6..7], hash);
        hash = fnv(&d[8..d.len() - 4], hash);
        let key = TKey {
            pid,
            table_id: tid,
            inst,
            current: h.current,
        };
        let idx = self.pid_slot[usize::from(pid)];
        let p = Some(pid);
        let mut changed = false;
        let st = match self.tables.get_mut(&key) {
            Some(st) => st,
            None => {
                self.tables.insert(
                    key,
                    TState {
                        version: h.version,
                        last: h.last,
                        sections: BTreeMap::new(),
                        last_pos: end_pos,
                    },
                );
                changed = true;
                match self.tables.get_mut(&key) {
                    Some(st) => st,
                    None => return true,
                }
            }
        };
        if st.version != h.version {
            let expected = (st.version + 1) & 0x1F;
            let di = idx != u16::MAX && {
                let ps = &mut self.pids[usize::from(idx)];
                std::mem::replace(&mut ps.di_pending, false)
            };
            if h.version != expected && !di {
                let (old, new) = (st.version, h.version);
                self.sink.emit(Rule::SectVersionStep, p, || {
                    format!("table_id 0x{tid:02X} instance {inst}: version_number {old} -> {new}")
                });
            }
            if h.last == 0 && st.last == 0 && st.sections.get(&0).is_some_and(|s| s.hash == hash) {
                self.sink.emit(Rule::SectVersionNoop, p, || {
                    format!(
                        "table_id 0x{tid:02X} instance {inst}: version_number {} -> {} with identical content",
                        st.version, h.version
                    )
                });
            }
            st.version = h.version;
            st.sections.clear();
            st.last = h.last;
            changed = true;
        } else if h.last != st.last {
            let (a, b) = (st.last, h.last);
            self.sink.emit(Rule::SectLastSection, p, || {
                format!("table_id 0x{tid:02X} instance {inst}: last_section_number {b}, earlier {a} in the same version")
            });
            st.last = h.last;
        }
        st.last_pos = end_pos;
        match st.sections.get_mut(&h.number) {
            Some(s) => {
                s.seen += 1;
                if s.hash != hash {
                    if tid != 0xCD {
                        let n = h.number;
                        self.sink.emit(Rule::SectVersionStale, p, || {
                            format!("table_id 0x{tid:02X} instance {inst} section {n}: content changed without a new version_number")
                        });
                    }
                    s.hash = hash;
                    s.len = d.len();
                    changed = true;
                }
                if s.seen >= 2 && idx != u16::MAX {
                    self.pids[usize::from(idx)].cycled = true;
                }
            }
            None => {
                st.sections.insert(
                    h.number,
                    SecInfo {
                        hash,
                        len: d.len(),
                        seen: 1,
                    },
                );
                changed = true;
            }
        }
        if idx != u16::MAX {
            let ps = &mut self.pids[usize::from(idx)];
            let k = fnv(
                &inst.to_be_bytes(),
                u64::from(tid) << 8 | u64::from(h.number),
            );
            if !ps.seen_keys.insert(k) {
                ps.cycled = true;
            }
            if ps.seen_keys.len() > 4096 {
                ps.seen_keys.clear();
            }
        }
        changed
    }

    fn repeat(
        &mut self,
        key: (u16, u8, u32, u8),
        name: impl FnOnce() -> String,
        limits: &'static [(f64, Rule)],
        end_pos: u64,
    ) {
        let r = self
            .repeats
            .entry(key)
            .or_insert_with(|| Repeat::new(name(), key.0, limits));
        r.occur(end_pos, &self.tb, &mut self.sink);
    }

    fn on_pat(&mut self, sec: &SectionOut, h: &Hdr) {
        let p = Some(0);
        let Some(programs) = psi::parse_pat(&sec.data) else {
            self.sink.emit(Rule::SectSyntax, p, || {
                "PAT program loop is not a whole number of 4-byte entries".into()
            });
            return;
        };
        self.pat_seen = true;
        let n = h.number;
        self.repeat(
            (0, 0, 0, n),
            || format!("PAT section {n}"),
            &PAT_LIMITS,
            sec.end_pos,
        );
        if !h.current {
            let _ = self.track(0, u32::from(h.ext), h, &sec.data, sec.end_pos);
            return;
        }
        let changed = self.track(0, 0, h, &sec.data, sec.end_pos);
        if !changed && self.pat.complete {
            return;
        }
        if self.pat.version != h.version || self.pat.tsid != h.ext {
            self.pat.sections.clear();
        }
        if let Some(want) = self.opts.tsid
            && want != h.ext
        {
            self.sink.emit(Rule::PatTsid, p, || {
                format!(
                    "transport_stream_id 0x{:04X}, configured 0x{want:04X}",
                    h.ext
                )
            });
        }
        if self.pat.complete && self.pat.tsid != h.ext {
            let old = self.pat.tsid;
            self.sink.emit(Rule::PatTsid, p, || {
                format!(
                    "transport_stream_id changed from 0x{old:04X} to 0x{:04X}",
                    h.ext
                )
            });
        }
        self.pat.tsid = h.ext;
        self.pat.version = h.version;
        self.pat.last = h.last;
        self.pat.sections.insert(h.number, programs);
        if (0..=self.pat.last).all(|k| self.pat.sections.contains_key(&k)) {
            let mut map = BTreeMap::new();
            let mut pids: BTreeMap<u16, u16> = BTreeMap::new();
            for (prog, pid) in self.pat.sections.values().flatten() {
                if let Some(old) = map.insert(*prog, *pid) {
                    let (pr, pd) = (*prog, *pid);
                    self.sink.emit(Rule::PatProgramDup, p, || {
                        format!(
                            "program_number {pr} listed twice (PIDs 0x{old:04X} and 0x{pd:04X})"
                        )
                    });
                }
                if *prog == 0 {
                    self.sink.emit(Rule::PatNit, p, || {
                        format!("program_number 0 (network_PID 0x{pid:04X})")
                    });
                    continue;
                }
                if let Some(other) = pids.insert(*pid, *prog)
                    && other != *prog
                {
                    let (pr, pd) = (*prog, *pid);
                    self.sink.emit(Rule::PatPmtPidShared, p, || {
                        format!("programs {other} and {pr} share PMT PID 0x{pd:04X}")
                    });
                }
                if !(0x0030..=0x1FEF).contains(pid) {
                    let (pr, pd) = (*prog, *pid);
                    self.sink.emit(Rule::PidReservedScte, Some(pd), || {
                        format!("PMT PID 0x{pd:04X} of program {pr} outside 0x0030 to 0x1FEF")
                    });
                }
            }
            map.remove(&0);
            self.pat.programs = map;
            self.pat.complete = true;
            self.refresh_roles();
            self.schedule_crosscheck();
        }
    }

    fn on_cat(&mut self, sec: &SectionOut, h: &Hdr) {
        self.cat_seen = true;
        let changed = self.track(1, 0, h, &sec.data, sec.end_pos);
        if changed {
            let body = &sec.data[8..sec.data.len() - 4];
            self.ca_pids.extend(psi::ca_pids(body));
            self.refresh_roles();
        }
    }

    fn on_pmt(&mut self, sec: &SectionOut, h: &Hdr, pid: u16) {
        let prog = h.ext;
        let p = Some(pid);
        match self.pat.programs.get(&prog) {
            Some(&pp) if pp == pid => {}
            other => {
                let other = other.copied();
                self.sink.emit(Rule::PmtProgram, p, || match other {
                    Some(pp) => format!(
                        "PMT of program {prog} on PID 0x{pid:04X}, but the PAT gives PID 0x{pp:04X}"
                    ),
                    None => format!("PMT of program {prog}, which is not in the PAT"),
                });
                return;
            }
        }
        self.repeat(
            (pid, 2, u32::from(prog), 0),
            || format!("PMT program {prog}"),
            &PMT_LIMITS,
            sec.end_pos,
        );
        if !h.current {
            let _ = self.track(pid, u32::from(prog) | 0x1_0000, h, &sec.data, sec.end_pos);
            return;
        }
        let changed = self.track(pid, u32::from(prog), h, &sec.data, sec.end_pos);
        if !changed && self.pmts.contains_key(&prog) {
            return;
        }
        let Some(pmt) = psi::parse_pmt(&sec.data, pid, &mut self.sink) else {
            return;
        };
        let ac3 = psi::check_pmt(&pmt, pid, &mut self.sink);
        for es in &pmt.streams {
            if !(0x0030..=0x1FEF).contains(&es.pid) {
                let e = es.pid;
                self.sink.emit(Rule::PidReservedScte, Some(e), || {
                    format!("elementary PID 0x{e:04X} of program {prog} outside 0x0030 to 0x1FEF")
                });
            }
        }
        self.ca_pids.extend(psi::ca_pids(&pmt.info));
        for es in &pmt.streams {
            self.ca_pids.extend(psi::ca_pids(&es.desc));
        }
        self.pmts.insert(prog, PmtInfo { pmt, ac3 });
        self.refresh_roles();
        self.schedule_crosscheck();
    }

    fn on_mgt(&mut self, sec: &SectionOut, h: &Hdr) {
        if !sec.at_payload_start {
            self.sink.emit(Rule::MgtPacketStart, Some(0x1FFB), || {
                "the MGT does not start right after a pointer_field of 0 in a packet with payload_unit_start_indicator 1".into()
            });
        }
        self.repeat(
            (0x1FFB, 0xC7, 0, 0),
            || "MGT".into(),
            &MGT_LIMITS,
            sec.end_pos,
        );
        self.mgt_seen_pos.get_or_insert(sec.end_pos);
        let changed = self.track(0x1FFB, 0, h, &sec.data, sec.end_pos);
        if !changed && self.mgt.is_some() {
            return;
        }
        if let Some(entries) = psip::parse_mgt(&sec.data, h.ext, &mut self.sink) {
            self.mgt = Some((h.version, entries));
            self.mgt_mismatch.clear();
            self.refresh_roles();
            self.schedule_crosscheck();
        }
    }

    fn on_vct(&mut self, sec: &SectionOut, h: &Hdr) {
        let tid = h.table_id;
        let n = h.number;
        let name = if tid == 0xC9 { "CVCT" } else { "TVCT" };
        self.repeat(
            (0x1FFB, tid, u32::from(h.ext), n),
            || format!("{name} section {n}"),
            &VCT_LIMITS,
            sec.end_pos,
        );
        self.vct_seen = true;
        let changed = self.track(0x1FFB, u32::from(h.ext), h, &sec.data, sec.end_pos);
        if !h.current || !changed {
            return;
        }
        let Some(channels) = psip::parse_vct(&sec.data, &mut self.sink) else {
            return;
        };
        let vct = self.vct.get_or_insert_with(VctState::default);
        if vct.table_id != tid || vct.tsid != h.ext || vct.version != h.version {
            vct.sections.clear();
        }
        vct.table_id = tid;
        vct.tsid = h.ext;
        vct.version = h.version;
        vct.sections.insert(h.number, channels);
        self.schedule_crosscheck();
    }

    fn on_rrt(&mut self, sec: &SectionOut, h: &Hdr) {
        let r = h.ext & 0xFF;
        self.repeat(
            (0x1FFB, 0xCA, u32::from(h.ext), 0),
            || format!("RRT region {r}"),
            &RRT_LIMITS,
            sec.end_pos,
        );
        let _ = self.track(0x1FFB, u32::from(h.ext), h, &sec.data, sec.end_pos);
    }

    fn on_stt(&mut self, sec: &SectionOut, h: &Hdr) {
        self.repeat(
            (0x1FFB, 0xCD, 0, 0),
            || "STT".into(),
            &STT_LIMITS,
            sec.end_pos,
        );
        self.stt_seen = true;
        let _ = self.track(0x1FFB, 0, h, &sec.data, sec.end_pos);
        let d = &sec.data;
        let body = d.get(13..d.len() - 4).unwrap_or(&[]);
        let body_hash = fnv(body, fnv(&d[..8], FNV0));
        let st = if body_hash != self.stt_body {
            self.stt_body = body_hash;
            psip::parse_stt(d, h.ext, h.version, &mut self.sink)
        } else {
            (d.len() >= 17).then(|| psip::Stt {
                system_time: u32::from_be_bytes([d[9], d[10], d[11], d[12]]),
                gps_utc_offset: d[13],
            })
        };
        let Some(st) = st else {
            return;
        };
        if let Some(prev) = self.stt
            && let Some(dt) = self.tb.dur(sec.end_pos - prev.pos)
        {
            let expected = f64::from(prev.system_time) + dt;
            let err = f64::from(st.system_time) - expected;
            if err.abs() > 2.0 {
                self.sink.emit(Rule::SttTime, Some(0x1FFB), || {
                    format!(
                        "system_time {} but {:.1} s after {} it should be about {:.0} ({err:+.1} s)",
                        st.system_time, dt, prev.system_time, expected
                    )
                });
                self.stt = Some(SttState {
                    pos: sec.end_pos,
                    system_time: st.system_time,
                    offset: st.gps_utc_offset,
                });
            } else if self.stt.is_some_and(|s| s.offset != st.gps_utc_offset) {
                self.stt = Some(SttState {
                    offset: st.gps_utc_offset,
                    ..prev
                });
            }
        } else {
            self.stt = Some(SttState {
                pos: sec.end_pos,
                system_time: st.system_time,
                offset: st.gps_utc_offset,
            });
        }
    }

    fn on_eit(&mut self, sec: &SectionOut, h: &Hdr, pid: u16, role: Role) {
        let source = h.ext;
        let n = h.number;
        if role == Role::Eit(0) {
            self.repeat(
                (pid, 0xCB, u32::from(source), n),
                || format!("EIT-0 source {source} section {n}"),
                &EIT0_LIMITS,
                sec.end_pos,
            );
        }
        let changed = self.track(pid, u32::from(source), h, &sec.data, sec.end_pos);
        let inst = self.eit.entry((pid, source)).or_default();
        inst.last_pos = sec.end_pos;
        if !changed {
            return;
        }
        let Some(events) = psip::parse_eit(&sec.data, pid, source, &mut self.sink) else {
            return;
        };
        let inst = self.eit.entry((pid, source)).or_default();
        if inst.version != h.version {
            inst.sections.clear();
        }
        inst.version = h.version;
        inst.last = h.last;
        inst.sections.insert(n, events);
        self.schedule_crosscheck();
    }

    fn on_ett(&mut self, sec: &SectionOut, h: &Hdr, pid: u16, role: Role) {
        let Some(id) = psip::parse_ett(&sec.data, pid, &mut self.sink) else {
            return;
        };
        let changed = self.track(pid, id, h, &sec.data, sec.end_pos);
        let _ = changed;
        let p = Some(pid);
        match role {
            Role::Ett(None) if id & 0xFFFF != 0 => {
                self.sink.emit(Rule::EttEtmId, p, || {
                    format!("ETM_id 0x{id:08X} on the channel ETT PID is not a channel ETM (bits 15 to 0 not zero)")
                });
            }
            Role::Ett(Some(_)) if id & 3 != 2 => {
                self.sink.emit(Rule::EttEtmId, p, || {
                    format!("ETM_id 0x{id:08X} on an event ETT PID is not an event ETM (bits 1 to 0 not '10')")
                });
            }
            _ => {}
        }
        let ids = self.ett.entry(pid).or_default();
        ids.insert(id, sec.end_pos);
        let exts = self.ett_ext.entry(pid).or_default();
        match exts.get(&h.ext) {
            Some(&other) if other != id => {
                if self.ett_ext_reported.insert(pid) {
                    let e = h.ext;
                    self.sink.emit(Rule::EttTidExt, p, || {
                        format!("ETMs 0x{other:08X} and 0x{id:08X} share ETT_table_id_extension 0x{e:04X}")
                    });
                }
            }
            Some(_) => {}
            None => {
                exts.insert(h.ext, id);
            }
        }
    }

    fn schedule_crosscheck(&mut self) {
        let delay = self.tb.bytes_for(1.0).unwrap_or(0);
        let due = self.last_pos + delay;
        if self.xcheck_due.is_none_or(|d| d > due) {
            self.xcheck_due = Some(due);
        }
    }

    /// Recomputes what every PID carries from the PAT, PMTs, CAT and MGT.
    fn refresh_roles(&mut self) {
        let now = self.last_pos;
        let mut roles: BTreeMap<u16, Role> = BTreeMap::new();
        roles.insert(0, Role::Pat);
        roles.insert(1, Role::Cat);
        roles.insert(2, Role::Tsdt);
        roles.insert(0x1FFB, Role::Psip);
        roles.insert(0x1FFF, Role::Null);
        for &pid in &self.ca_pids {
            roles.insert(pid, Role::Ca);
        }
        if let Some((_, entries)) = &self.mgt {
            for e in entries {
                match TableType::of(e.table_type) {
                    TableType::Eit(k) => {
                        roles.insert(e.pid, Role::Eit(k));
                    }
                    TableType::EventEtt(k) => {
                        roles.insert(e.pid, Role::Ett(Some(k)));
                    }
                    TableType::ChannelEtt => {
                        roles.insert(e.pid, Role::Ett(None));
                    }
                    _ => {}
                }
            }
        }
        for (prog, info) in &self.pmts {
            if !self.pat.programs.contains_key(prog) {
                continue;
            }
            if info.pmt.pcr_pid != 0x1FFF {
                roles.entry(info.pmt.pcr_pid).or_insert(Role::PcrOnly);
            }
            for es in &info.pmt.streams {
                roles.insert(es.pid, Role::Es);
            }
        }
        for &pid in self.pat.programs.values() {
            roles.insert(pid, Role::Pmt);
        }
        // Drop the PMTs of programs no longer in the PAT.
        let progs: BTreeSet<u16> = self.pat.programs.keys().copied().collect();
        self.pmts.retain(|p, _| progs.contains(p));
        self.clocks.retain(|p, _| progs.contains(p));
        self.sys.retain(|p, _| progs.contains(p));
        self.repeats
            .retain(|k, _| k.1 != 2 || progs.contains(&(k.2 as u16)));
        // PMT expectations and system buffers.
        let t = self.tb.dur(now).unwrap_or(0.0);
        for (&prog, &pid) in &self.pat.programs {
            let r = self
                .repeats
                .entry((pid, 2, u32::from(prog), 0))
                .or_insert_with(|| Repeat::new(format!("PMT program {prog}"), pid, &PMT_LIMITS));
            r.expect_from(now);
            self.sys
                .entry(prog)
                .or_insert_with(|| SysModel::new(pid, t));
        }
        // EIT-0 repetition only on the current EIT-0 PID.
        let eit0 = roles
            .iter()
            .find(|(_, r)| **r == Role::Eit(0))
            .map(|(p, _)| *p);
        self.repeats.retain(|k, _| k.1 != 0xCB || Some(k.0) == eit0);
        // References for PID-MISSING.
        let mut refs: BTreeMap<u16, RefInfo> = BTreeMap::new();
        for info in self.pmts.values() {
            if info.pmt.pcr_pid != 0x1FFF {
                refs.insert(
                    info.pmt.pcr_pid,
                    RefInfo {
                        since: now,
                        timeout: self.opts.pid_timeout_s,
                    },
                );
            }
            for es in &info.pmt.streams {
                refs.insert(
                    es.pid,
                    RefInfo {
                        since: now,
                        timeout: self.opts.pid_timeout_s,
                    },
                );
            }
        }
        if let Some((_, entries)) = &self.mgt {
            for e in entries {
                if e.pid != 0x1FFB {
                    refs.entry(e.pid).or_insert(RefInfo {
                        since: now,
                        timeout: self.opts.psip_pid_timeout_s,
                    });
                }
            }
        }
        for (pid, r) in &mut refs {
            if let Some(old) = self.refs.get(pid) {
                r.since = old.since;
            }
        }
        self.refs = refs;
        // Apply roles.
        for (&pid, &role) in &roles {
            if pid == 2 || pid == 0x1FFF || pid == 1 {
                continue;
            }
            let idx = self.slot(pid, now);
            self.pids[idx].role = role;
        }
        for st in &mut self.pids {
            let role = roles.get(&st.pid).copied().unwrap_or(Role::Unknown);
            if st.role != role {
                st.role = role;
            }
            if !role.sections() {
                st.asm = None;
            }
            if role != Role::Es {
                st.es = None;
            }
        }
        // Elementary streams.
        let mut es_list: Vec<(u16, u16, u8, Option<Ac3Desc>)> = Vec::new();
        for (&prog, info) in &self.pmts {
            for es in &info.pmt.streams {
                es_list.push((prog, es.pid, es.stream_type, info.ac3.get(&es.pid).copied()));
            }
        }
        for (prog, pid, st_type, ac3) in es_list {
            let idx = self.slot(pid, now);
            let st = &mut self.pids[idx];
            let fresh = st
                .es
                .as_ref()
                .is_none_or(|e| e.stream_type != st_type || e.program != prog);
            if fresh {
                let mut e = EsState::new(prog, st_type, pid);
                if Kind::of(st_type).is_video() || Kind::of(st_type).is_audio() {
                    e.pts_rep.expect_from(now);
                }
                st.es = Some(Box::new(e));
            }
            if let Some(e) = st.es.as_deref_mut() {
                e.ac3_desc = ac3;
            }
        }
        // PCR tracks.
        for info in self.pmts.values() {
            let pid = info.pmt.pcr_pid;
            if pid == 0x1FFF {
                continue;
            }
            let t = self.pcr.entry(pid).or_insert_with(|| PcrTrack::new(pid));
            t.referenced = true;
            t.expected_since.get_or_insert(now);
        }
    }

    // ------------------------------------------------------------------ periodic checks

    fn tick(&mut self, now: u64, final_: bool) {
        if self.tb.rate().is_none() {
            return;
        }
        for r in self.repeats.values_mut() {
            r.tick(now, &self.tb, &mut self.sink);
        }
        for t in self.pcr.values_mut() {
            t.tick(now, &self.tb, &mut self.sink);
        }
        for st in &mut self.pids {
            if let Some(es) = st.es.as_deref_mut()
                && (es.kind.is_video() || es.kind.is_audio())
            {
                es.pts_rep.tick(now, &self.tb, &mut self.sink);
            }
        }
        // PID-MISSING.
        let refs: Vec<(u16, RefInfo)> = self.refs.iter().map(|(p, r)| (*p, *r)).collect();
        for (pid, r) in refs {
            let idx = self.pid_slot[usize::from(pid)];
            let (last, latched) = if idx == u16::MAX {
                (None, false)
            } else {
                let st = &self.pids[usize::from(idx)];
                (st.last_pos, st.missing_latched)
            };
            if latched {
                continue;
            }
            let since = last.map_or(r.since, |l| l.max(r.since));
            if now > since
                && let Some(gap) = self.tb.dur(now - since)
                && gap > r.timeout
            {
                if idx != u16::MAX {
                    self.pids[usize::from(idx)].missing_latched = true;
                } else {
                    let i = self.slot(pid, now);
                    self.pids[i].missing_latched = true;
                    self.pids[i].packets = 0;
                }
                let to = r.timeout;
                self.sink.emit(Rule::PidMissing, Some(pid), || {
                    format!("no packet for {gap:.2} s on a PID the tables reference (limit {to} s)")
                });
            }
        }
        // PID-UNREFERENCED.
        for st in &mut self.pids {
            if st.role != Role::Unknown
                || st.pid < 0x20
                || st.pid >= 0x1FFB
                || st.unref_reported
                || st.packets == 0
            {
                continue;
            }
            if now > st.first_pos
                && let Some(gap) = self.tb.dur(now - st.first_pos)
                && gap > 0.5
            {
                st.unref_reported = true;
                self.sink.emit(Rule::PidUnreferenced, Some(st.pid), || {
                    format!("PID carries packets but no table has referenced it for {gap:.2} s")
                });
            }
        }
        if let Some(due) = self.xcheck_due
            && (due <= now || final_)
        {
            self.xcheck_due = None;
            self.crosscheck(final_);
        }
        let t = self.tb.dur(now).unwrap_or(0.0);
        if t >= self.next_slow || final_ {
            self.next_slow = t + 1.0;
            self.mgt_checks(now, final_);
            self.evict(now);
            self.psip_required(t, final_);
        }
    }

    fn psip_required(&mut self, t: f64, _final: bool) {
        let checks: [(bool, f64, u8, &str); 3] = [
            (self.mgt_seen_pos.is_some(), 0.15, 1, "MGT"),
            (self.vct_seen, 0.4, 2, "CVCT or TVCT"),
            (self.stt_seen, 10.0, 4, "STT"),
        ];
        for (seen, limit, bit, name) in checks {
            if !seen && t > limit && self.psip_required_reported & bit == 0 {
                self.psip_required_reported |= bit;
                self.sink.emit(Rule::PsipRequired, Some(0x1FFB), || {
                    format!("no {name} in the first {limit} s (maximum cycle)")
                });
            }
        }
    }

    fn report_once(&mut self, rule: Rule, pid: Option<u16>, msg: String) {
        let h = fnv(msg.as_bytes(), fnv(rule.info().id.as_bytes(), FNV0));
        if self.reported.insert(h) {
            self.sink.emit(rule, pid, || msg);
        }
    }

    /// The MGT against the tables on air: versions and sizes.
    fn mgt_checks(&mut self, now: u64, final_: bool) {
        let Some((_, entries)) = self.mgt.clone() else {
            return;
        };
        for e in &entries {
            let tt = TableType::of(e.table_type);
            let (pid, tid, inst_filter): (u16, u8, Option<u32>) = match tt {
                TableType::Cvct { .. } => (0x1FFB, 0xC9, None),
                TableType::Tvct { .. } => (0x1FFB, 0xC8, None),
                TableType::Eit(_) => (e.pid, 0xCB, None),
                TableType::EventEtt(_) | TableType::ChannelEtt => (e.pid, 0xCC, None),
                TableType::Rrt(r) => (0x1FFB, 0xCA, Some(0xFF00 | u32::from(r))),
                _ => continue,
            };
            let current = match tt {
                TableType::Cvct { current } | TableType::Tvct { current } => current,
                _ => true,
            };
            let matching: Vec<&TState> = self
                .tables
                .iter()
                .filter(|(k, _)| {
                    k.pid == pid
                        && k.table_id == tid
                        && k.current == current
                        && inst_filter.is_none_or(|i| k.inst == i)
                })
                .map(|(_, s)| s)
                .collect();
            if matching.is_empty() {
                continue;
            }
            let name = tt.name();
            let versions: BTreeSet<u8> = matching.iter().map(|s| s.version).collect();
            if versions.iter().any(|&v| v != e.version) {
                let since = *self.mgt_mismatch.entry(e.table_type).or_insert(now);
                if self.tb.dur(now - since).is_some_and(|d| d >= 2.0) || final_ {
                    let msg = format!(
                        "{name} on PID 0x{pid:04X}: MGT says version {}, the table has {:?}",
                        e.version, versions
                    );
                    self.report_once(Rule::MgtVersion, Some(0x1FFB), msg);
                }
            } else {
                self.mgt_mismatch.remove(&e.table_type);
                let settled = matching.iter().all(|s| {
                    s.sections.values().all(|x| x.seen >= 2)
                        && s.sections.len() == usize::from(s.last) + 1
                });
                if settled || final_ {
                    let total: usize = matching
                        .iter()
                        .flat_map(|s| s.sections.values())
                        .map(|x| x.len)
                        .sum();
                    if total as u64 != u64::from(e.bytes) {
                        let msg = format!(
                            "{name} on PID 0x{pid:04X}: number_bytes {} but its sections total {total} bytes",
                            e.bytes
                        );
                        self.report_once(Rule::MgtNumberBytes, Some(0x1FFB), msg);
                    }
                }
            }
        }
        // Tables present but not listed.
        let mut present: BTreeSet<(u8, u32)> = BTreeSet::new();
        for k in self.tables.keys() {
            if k.pid == 0x1FFB && matches!(k.table_id, 0xC8..=0xCA) {
                present.insert((
                    k.table_id,
                    if k.table_id == 0xCA {
                        k.inst
                    } else {
                        u32::from(k.current)
                    },
                ));
            }
        }
        for (tid, inst) in present {
            let listed = entries.iter().any(|e| match TableType::of(e.table_type) {
                TableType::Tvct { current } => tid == 0xC8 && u32::from(current) == inst,
                TableType::Cvct { current } => tid == 0xC9 && u32::from(current) == inst,
                TableType::Rrt(r) => tid == 0xCA && inst & 0xFF == u32::from(r),
                _ => false,
            });
            if !listed && self.unlisted_reported.insert((tid, inst)) {
                self.sink.emit(Rule::MgtUnlisted, Some(0x1FFB), || {
                    format!("table_id 0x{tid:02X} (instance {inst}) is on air but not in the MGT")
                });
            }
        }
    }

    /// Forgets table instances not seen for ten minutes, so that memory stays bounded on a
    /// stream that runs for months.
    fn evict(&mut self, now: u64) {
        if let Some(ten) = self.tb.bytes_for(600.0) {
            let cut = now.saturating_sub(ten);
            self.tables.retain(|_, s| s.last_pos >= cut);
            self.eit.retain(|_, i| i.last_pos >= cut);
            for ids in self.ett.values_mut() {
                ids.retain(|_, p| *p >= cut);
            }
        }
        if self.reported.len() > 10_000 {
            self.reported.clear();
        }
    }

    fn channels(&self) -> Vec<Channel> {
        self.vct
            .as_ref()
            .map(|v| v.sections.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    fn pid_cycled(&self, pid: u16) -> bool {
        let idx = self.pid_slot[usize::from(pid)];
        idx != u16::MAX && self.pids[usize::from(idx)].cycled
    }

    /// Relations between tables: PAT, PMT, VCT, MGT, EIT, ETT.
    fn crosscheck(&mut self, final_: bool) {
        let tsid = self.pat.tsid;
        let have_pat = self.pat.complete;
        let channels = self.channels();
        let p = Some(0x1FFB);
        let mut out: Vec<(Rule, Option<u16>, String)> = Vec::new();
        if let Some(v) = &self.vct
            && have_pat
        {
            if v.tsid != tsid {
                out.push((
                    Rule::VctTsid,
                    p,
                    format!("VCT transport_stream_id 0x{:04X}, PAT 0x{tsid:04X}", v.tsid),
                ));
            }
            for ch in &channels {
                let num = ch.number();
                if ch.tsid != tsid {
                    out.push((
                        Rule::VctChannelTsid,
                        p,
                        format!(
                            "channel {num} names TSID 0x{:04X} (this TS is 0x{tsid:04X})",
                            ch.tsid
                        ),
                    ));
                } else if ch.program != 0
                    && ch.program != 0xFFFF
                    && !self.pat.programs.contains_key(&ch.program)
                {
                    out.push((
                        Rule::VctProgram,
                        p,
                        format!(
                            "channel {num} names program {} which is not in the PAT",
                            ch.program
                        ),
                    ));
                }
            }
            for &prog in self.pat.programs.keys() {
                if !channels.iter().any(|c| c.tsid == tsid && c.program == prog) {
                    out.push((
                        Rule::VctCoverage,
                        p,
                        format!("program {prog} of the PAT has no virtual channel"),
                    ));
                }
            }
            let mut nums: BTreeMap<(u16, u16), usize> = BTreeMap::new();
            for ch in &channels {
                *nums.entry((ch.major, ch.minor)).or_default() += 1;
            }
            for ((ma, mi), n) in nums {
                if n > 1 {
                    out.push((
                        Rule::VctNumberDup,
                        p,
                        format!("channel number {ma}/{mi} occurs {n} times"),
                    ));
                }
            }
            let mut sources: BTreeMap<u16, BTreeSet<u16>> = BTreeMap::new();
            for ch in &channels {
                if (1..=0x0FFF).contains(&ch.source_id) {
                    sources.entry(ch.source_id).or_default().insert(ch.program);
                }
            }
            for (s, progs) in sources {
                if progs.len() > 1 {
                    out.push((
                        Rule::VctSourceDup,
                        p,
                        format!("source_id {s} is shared by programs {progs:?}"),
                    ));
                }
            }
            for ch in &channels {
                let (Some(sld), Some(info)) = (&ch.sld, self.pmts.get(&ch.program)) else {
                    continue;
                };
                let num = ch.number();
                let pmt = &info.pmt;
                if sld.pcr_pid != pmt.pcr_pid {
                    out.push((
                        Rule::VctSld,
                        p,
                        format!(
                            "channel {num}: service_location PCR_PID 0x{:04X}, PMT 0x{:04X}",
                            sld.pcr_pid, pmt.pcr_pid
                        ),
                    ));
                }
                for es in &pmt.streams {
                    let k = Kind::of(es.stream_type);
                    if !(k.is_video() || k.is_audio()) {
                        continue;
                    }
                    let lang_pmt = pmt_language(es, info.ac3.get(&es.pid));
                    match sld.elems.iter().find(|e| e.1 == es.pid) {
                        None => out.push((
                            Rule::VctSld,
                            p,
                            format!("channel {num}: service_location lacks PID 0x{:04X}", es.pid),
                        )),
                        Some(&(st, _, l)) => {
                            if st != es.stream_type || l != lang_pmt {
                                out.push((
                                    Rule::VctSld,
                                    p,
                                    format!(
                                        "channel {num} PID 0x{:04X}: service_location stream_type 0x{st:02X} language {}, PMT 0x{:02X} {}",
                                        es.pid,
                                        lang(l),
                                        es.stream_type,
                                        lang(lang_pmt)
                                    ),
                                ));
                            }
                        }
                    }
                }
                for &(_, pid, _) in &sld.elems {
                    if !pmt.streams.iter().any(|s| s.pid == pid) {
                        out.push((
                            Rule::VctSld,
                            p,
                            format!("channel {num}: service_location names PID 0x{pid:04X}, absent from the PMT"),
                        ));
                    }
                }
            }
        }
        // EIT and ETT against the VCT and the MGT.
        let eit_pids: BTreeMap<u8, u16> = self
            .mgt
            .as_ref()
            .map(|(_, es)| {
                es.iter()
                    .filter_map(|e| match TableType::of(e.table_type) {
                        TableType::Eit(k) => Some((k, e.pid)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ett_pids: BTreeMap<Option<u8>, u16> = self
            .mgt
            .as_ref()
            .map(|(_, es)| {
                es.iter()
                    .filter_map(|e| match TableType::of(e.table_type) {
                        TableType::EventEtt(k) => Some((Some(k), e.pid)),
                        TableType::ChannelEtt => Some((None, e.pid)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.vct.is_some() {
            let sources: BTreeSet<u16> = channels
                .iter()
                .filter(|c| c.service_type != 0x04)
                .map(|c| c.source_id)
                .collect();
            for (&k, &pid) in &eit_pids {
                let seen_any = self.eit.keys().any(|(p, _)| *p == pid);
                if !(self.pid_cycled(pid) || (final_ && seen_any)) {
                    continue;
                }
                for &s in &sources {
                    if !self.eit.contains_key(&(pid, s)) {
                        out.push((
                            Rule::EitInstance,
                            Some(pid),
                            format!("EIT-{k} has no instance for source_id {s}"),
                        ));
                    }
                }
                for &(ep, s) in self.eit.keys() {
                    if ep == pid && !channels.iter().any(|c| c.source_id == s) {
                        out.push((
                            Rule::EitSource,
                            Some(pid),
                            format!(
                                "EIT-{k} instance for source_id {s}, which no virtual channel has"
                            ),
                        ));
                    }
                }
            }
            // Channel ETMs.
            if let Some(&cpid) = ett_pids.get(&None) {
                let ok = self.pid_cycled(cpid) || (final_ && self.ett.contains_key(&cpid));
                if ok {
                    for ch in channels.iter().filter(|c| c.etm == 1) {
                        let id = u32::from(ch.source_id) << 16;
                        if !self.ett.get(&cpid).is_some_and(|m| m.contains_key(&id)) {
                            out.push((
                                Rule::EtmMissing,
                                Some(cpid),
                                format!(
                                    "channel {} has ETM_location 01 but no channel ETM 0x{id:08X}",
                                    ch.number()
                                ),
                            ));
                        }
                    }
                }
            } else {
                for ch in channels.iter().filter(|c| c.etm == 1) {
                    out.push((
                        Rule::EtmMissing,
                        p,
                        format!(
                            "channel {} has ETM_location 01 but the MGT lists no channel ETT",
                            ch.number()
                        ),
                    ));
                }
            }
        }
        // Event ETMs, order across sections, windows, spanning events.
        let window = self.window_now();
        for (&k, &pid) in &eit_pids {
            let insts: Vec<(u16, Vec<Event>)> = self
                .eit
                .iter()
                .filter(|((p, _), i)| *p == pid && i.sections.len() == usize::from(i.last) + 1)
                .map(|((_, s), i)| (*s, i.sections.values().flatten().copied().collect()))
                .collect();
            for (s, events) in &insts {
                for w in events.windows(2) {
                    if u64::from(w[1].start) < u64::from(w[0].start) + u64::from(w[0].len) {
                        out.push((
                            Rule::EitOrder,
                            Some(pid),
                            format!(
                                "EIT-{k} source {s}: event {} at {} overlaps or precedes event {} ({} + {} s)",
                                w[1].id, w[1].start, w[0].id, w[0].start, w[0].len
                            ),
                        ));
                    }
                }
                if let Some((w0_gps, ok)) = window
                    && ok
                {
                    let ws = w0_gps + 10_800 * i64::from(k);
                    let we = ws + 10_800;
                    for e in events {
                        let s0 = i64::from(e.start);
                        let s1 = s0 + i64::from(e.len);
                        let inside = if e.len == 0 {
                            s0 >= ws && s0 < we
                        } else {
                            s0 < we && s1 > ws
                        };
                        if !inside {
                            out.push((
                                Rule::EitWindow,
                                Some(pid),
                                format!(
                                    "EIT-{k} source {s}: event {} ({s0} to {s1} GPS s) is outside the window {ws} to {we}",
                                    e.id
                                ),
                            ));
                        }
                    }
                }
                if let Some(&epid) = ett_pids.get(&Some(k)) {
                    let ok = self.pid_cycled(epid) || (final_ && self.ett.contains_key(&epid));
                    if ok {
                        for e in events.iter().filter(|e| e.etm == 1) {
                            let id = (u32::from(*s) << 16) | (u32::from(e.id) << 2) | 2;
                            if !self.ett.get(&epid).is_some_and(|m| m.contains_key(&id)) {
                                out.push((
                                    Rule::EtmMissing,
                                    Some(epid),
                                    format!("EIT-{k} source {s} event {} has ETM_location 01 but ETT-{k} has no ETM 0x{id:08X}", e.id),
                                ));
                            }
                        }
                    }
                } else if events.iter().any(|e| e.etm == 1) {
                    out.push((
                        Rule::EtmMissing,
                        Some(pid),
                        format!("EIT-{k} source {s} has events with ETM_location 01 but the MGT lists no ETT-{k}"),
                    ));
                }
                // Spanning events: the same event in EIT-k+1 keeps its event_id.
                if let Some(&npid) = eit_pids.get(&(k + 1))
                    && let Some(next) = self.eit.get(&(npid, *s))
                {
                    for e in events {
                        for n in next.sections.values().flatten() {
                            if n.start == e.start && n.len == e.len && n.id != e.id {
                                out.push((
                                    Rule::EitSpanId,
                                    Some(npid),
                                    format!(
                                        "source {s}: the event at {} is event_id {} in EIT-{k} but {} in EIT-{}",
                                        e.start,
                                        e.id,
                                        n.id,
                                        k + 1
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
        // PID roles.
        let mut es_types: BTreeMap<u16, BTreeSet<u8>> = BTreeMap::new();
        for info in self.pmts.values() {
            for es in &info.pmt.streams {
                es_types.entry(es.pid).or_default().insert(es.stream_type);
            }
        }
        let pmt_pids: BTreeSet<u16> = self.pat.programs.values().copied().collect();
        let psip_pids: BTreeSet<u16> = eit_pids
            .values()
            .chain(ett_pids.values())
            .copied()
            .collect();
        for (pid, types) in &es_types {
            if types.len() > 1 {
                out.push((
                    Rule::PidConflict,
                    Some(*pid),
                    format!("PID 0x{pid:04X} is an elementary PID with stream types {types:02X?}"),
                ));
            }
            if pmt_pids.contains(pid) || psip_pids.contains(pid) || matches!(*pid, 0 | 1 | 0x1FFB) {
                out.push((
                    Rule::PidConflict,
                    Some(*pid),
                    format!("PID 0x{pid:04X} is an elementary PID and also carries PSI or PSIP"),
                ));
            }
        }
        for (rule, pid, msg) in out {
            self.report_once(rule, pid, msg);
        }
    }

    /// The start of the current 3-hour window in GPS seconds, and whether now is far
    /// enough (30 s) from a window boundary to check against it.
    fn window_now(&self) -> Option<(i64, bool)> {
        let s = self.stt?;
        let dt = self.tb.dur(self.last_pos.saturating_sub(s.pos))?;
        let gps = f64::from(s.system_time) + dt;
        let utc = gps - f64::from(s.offset);
        let w0 = (utc / 10_800.0).floor() * 10_800.0;
        let far = utc - w0 > 30.0 && w0 + 10_800.0 - utc > 30.0;
        Some((w0 as i64 + i64::from(s.offset), far))
    }

    // ------------------------------------------------------------------ results

    /// The report so far, without the end-of-stream checks.
    pub fn report(&self) -> Report {
        let mut stats = Stats {
            bytes: self.base + self.carry.len() as u64,
            packets: self.packets,
            bytes_out_of_sync: self.out_of_sync,
            duration_s: self.tb.dur(self.last_pos),
            rate_estimated: self.tb.estimated,
            rate_nominal: self.opts.rate,
            rate_used: self.tb.rate(),
            cbr: !self.tb.vbr(),
            null_share: if self.packets > 0 {
                self.nulls as f64 / self.packets as f64
            } else {
                0.0
            },
            ..Stats::default()
        };
        if let (Some(e), Some(n)) = (stats.rate_estimated, stats.rate_nominal) {
            stats.rate_deviation_ppm = Some((e / n - 1.0) * 1e6);
        }
        let dur = stats.duration_s;
        for st in &self.pids {
            if st.packets == 0 && st.role == Role::Unknown {
                continue;
            }
            let role = match st.role {
                Role::Unknown => "unreferenced".to_string(),
                Role::Pat => "PAT".into(),
                Role::Cat => "CAT".into(),
                Role::Tsdt => "TSDT".into(),
                Role::Pmt => {
                    let progs: Vec<String> = self
                        .pat
                        .programs
                        .iter()
                        .filter(|(_, p)| **p == st.pid)
                        .map(|(k, _)| k.to_string())
                        .collect();
                    format!("PMT program {}", progs.join(","))
                }
                Role::Es => match &st.es {
                    Some(e) => format!(
                        "{} (0x{:02X}) program {}",
                        e.kind.name(),
                        e.stream_type,
                        e.program
                    ),
                    None => "elementary stream".into(),
                },
                Role::PcrOnly => "PCR".into(),
                Role::Psip => "PSIP base".into(),
                Role::Eit(k) => format!("EIT-{k}"),
                Role::Ett(Some(k)) => format!("ETT-{k}"),
                Role::Ett(None) => "channel ETT".into(),
                Role::Ca => "CA (EMM/ECM)".into(),
                Role::Null => "null".into(),
            };
            stats.pids.insert(
                st.pid,
                PidStats {
                    role,
                    packets: st.packets,
                    scrambled: st.scrambled,
                    cc_errors: st.cc_errors,
                    bitrate: dur
                        .filter(|d| *d > 0.0)
                        .map(|d| st.packets as f64 * 1504.0 / d),
                },
            );
        }
        if self.nulls > 0 {
            stats.pids.insert(
                0x1FFF,
                PidStats {
                    role: "null".into(),
                    packets: self.nulls,
                    bitrate: dur
                        .filter(|d| *d > 0.0)
                        .map(|d| self.nulls as f64 * 1504.0 / d),
                    ..PidStats::default()
                },
            );
        }
        for (pid, t) in &self.pcr {
            if t.stats.count > 0 {
                stats.pcr.insert(*pid, t.stats.clone());
            }
        }
        stats.tables = self
            .repeats
            .values()
            .filter(|r| r.count > 0)
            .map(|r| r.stats())
            .collect();
        for st in &self.pids {
            if let Some(es) = &st.es {
                es.tstd_stats(&mut stats.buffers);
            }
            if let Some(sb) = &st.sb {
                stats.buffers.push(sb.stats("SB", st.pid));
            }
        }
        for m in self.sys.values() {
            m.stats(&mut stats.buffers);
        }
        stats.programs = self.pat.programs.clone();
        stats.tsid = self.pat_seen.then_some(self.pat.tsid);
        stats.channels = self
            .channels()
            .iter()
            .map(|c| format!("{} {} (program {})", c.number(), c.name, c.program))
            .collect();
        stats.features = self.sink.features.clone();
        Report {
            findings: self.sink.findings.clone(),
            counts: self.sink.counts.clone(),
            stats,
        }
    }

    /// Ends the stream: runs the checks that need its end, and returns the report.
    pub fn finish(mut self) -> Report {
        let end = self.last_pos;
        self.sink.pos = Position {
            byte: end,
            packet: end / 188,
            pid: None,
        };
        self.sink.time = self.tb.dur(end);
        if !self.ever_synced && self.base + self.carry.len() as u64 > 0 {
            self.sink.emit(Rule::TsSyncLoss, None, || {
                "no synchronization: never five consecutive sync bytes 188 bytes apart".into()
            });
        }
        if self.tb.rate().is_none() && self.packets > 0 {
            self.sink.emit(Rule::TimeBase, None, || {
                "no PCR and no --rate: no time base, so no timing check was made".into()
            });
        }
        self.tick(end, true);
        let params = PcrParams {
            target_ms: self.opts.pcr_target_ms,
            nominal: self.opts.rate,
        };
        for t in self.pcr.values_mut() {
            t.flush(&self.tb, params, &mut self.sink, true);
        }
        if let Some(t) = self.tb.dur(end) {
            for st in &mut self.pids {
                if let Some(es) = st.es.as_deref_mut() {
                    es.finish(t, &mut self.sink);
                }
            }
        }
        if self.tb.vbr() {
            let (n, k) = (self.tb.intervals, self.tb.deviating);
            self.sink.emit(Rule::TsVbr, None, || {
                format!(
                    "{k} of {n} intervals between PCRs differ from the mean rate by more than 0.5 %"
                )
            });
        }
        if let (Some(e), Some(n)) = (self.tb.estimated, self.opts.rate) {
            let ppm = (e / n - 1.0) * 1e6;
            if ppm.abs() > 30.0 {
                self.sink.emit(Rule::TsRate, None, || {
                    format!("rate from the PCRs {e:.0} bit/s is {ppm:+.1} ppm from the nominal {n:.0} bit/s")
                });
            }
        }
        self.report()
    }
}

fn pmt_language(es: &psi::EsInfo, ac3: Option<&Ac3Desc>) -> u32 {
    if let Some(l) = ac3.and_then(|a| a.language) {
        return l;
    }
    if let Ok(ds) = crate::bytes::descriptors(&es.desc) {
        for (t, b) in ds {
            if t == 0x0A && b.len() >= 4 {
                return u32::from_be_bytes([0, b[0], b[1], b[2]]);
            }
        }
    }
    0
}

/// Checks a whole stream held in memory.
pub fn check(data: &[u8], opts: &Options) -> Report {
    let mut c = Checker::new(opts.clone());
    c.push(data);
    c.finish()
}

/// True if the report has no finding at `worst` severity or worse, ignoring `allowed` rules.
pub fn passes(report: &Report, worst: Option<Severity>, allowed: &BTreeSet<Rule>) -> bool {
    match worst {
        None => true,
        Some(w) => !report
            .counts
            .keys()
            .any(|r| !allowed.contains(r) && r.info().severity <= w),
    }
}
