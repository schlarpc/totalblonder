//! PAT, CAT and PMT (H.222.0 §2.4.4.3 to §2.4.4.9, p.47-52) and the PMT checks of A/53
//! Part 3, A/52 Annex A and SCTE 54.

use std::collections::BTreeMap;

use crate::bytes::{Rd, descriptors, lang};
use crate::report::Sink;
use crate::rules::Rule;

/// The long-form section header (H.222.0 Table 2-30, 2-33; A/65 Table 4.1).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hdr {
    pub(crate) table_id: u8,
    /// The bit after section_syntax_indicator ('0' in PSI, private_indicator in PSIP).
    pub(crate) bit1: bool,
    pub(crate) length: usize,
    pub(crate) ext: u16,
    pub(crate) version: u8,
    pub(crate) current: bool,
    pub(crate) number: u8,
    pub(crate) last: u8,
}

impl Hdr {
    pub(crate) fn parse(d: &[u8]) -> Option<Hdr> {
        if d.len() < 8 {
            return None;
        }
        Some(Hdr {
            table_id: d[0],
            bit1: d[1] & 0x40 != 0,
            length: usize::from(u16::from_be_bytes([d[1], d[2]]) & 0x0FFF),
            ext: u16::from_be_bytes([d[3], d[4]]),
            version: (d[5] >> 1) & 0x1F,
            current: d[5] & 1 != 0,
            number: d[6],
            last: d[7],
        })
    }
}

/// Kind of elementary stream, from its stream_type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// MPEG-1/2 video (0x01, 0x02, 0x80).
    Mpeg2Video,
    /// AVC (0x1B).
    Avc,
    /// Other video (HEVC 0x24 and the like).
    OtherVideo,
    /// AC-3 (0x81).
    Ac3,
    /// E-AC-3 (0x87).
    Eac3,
    /// Other audio.
    OtherAudio,
    /// Sections (private sections, DSM-CC, SCTE 35, A/90 tables).
    Sections,
    /// Anything else: PES or private data.
    Other,
}

impl Kind {
    pub(crate) fn of(stream_type: u8) -> Kind {
        match stream_type {
            0x01 | 0x02 | 0x80 => Kind::Mpeg2Video,
            0x1B => Kind::Avc,
            0x10 | 0x24 | 0x20 | 0x1F => Kind::OtherVideo,
            0x81 => Kind::Ac3,
            0x87 => Kind::Eac3,
            0x03 | 0x04 | 0x0F | 0x11 | 0x1C | 0x2D | 0x2E | 0x88 => Kind::OtherAudio,
            0x05 | 0x0A..=0x0D | 0x14 | 0x86 | 0x95 => Kind::Sections,
            _ => Kind::Other,
        }
    }

    pub(crate) fn is_video(self) -> bool {
        matches!(self, Kind::Mpeg2Video | Kind::Avc | Kind::OtherVideo)
    }

    pub(crate) fn is_audio(self) -> bool {
        matches!(self, Kind::Ac3 | Kind::Eac3 | Kind::OtherAudio)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Mpeg2Video => "MPEG-2 video",
            Kind::Avc => "AVC video",
            Kind::OtherVideo => "video",
            Kind::Ac3 => "AC-3 audio",
            Kind::Eac3 => "E-AC-3 audio",
            Kind::OtherAudio => "audio",
            Kind::Sections => "sections",
            Kind::Other => "data",
        }
    }
}

/// PAT programs of one section: (program_number, PID).
pub(crate) fn parse_pat(d: &[u8]) -> Option<Vec<(u16, u16)>> {
    let body = d.get(8..d.len().checked_sub(4)?)?;
    if body.len() % 4 != 0 {
        return None;
    }
    Some(
        body.as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                (
                    u16::from_be_bytes([c[0], c[1]]),
                    u16::from_be_bytes([c[2], c[3]]) & 0x1FFF,
                )
            })
            .collect(),
    )
}

/// The CA_PIDs of the CA_descriptors (tag 0x09) in a descriptor loop.
pub(crate) fn ca_pids(loop_: &[u8]) -> Vec<u16> {
    descriptors(loop_)
        .unwrap_or_default()
        .into_iter()
        .filter(|(t, b)| *t == 0x09 && b.len() >= 4)
        .map(|(_, b)| u16::from_be_bytes([b[2], b[3]]) & 0x1FFF)
        .collect()
}

/// One elementary stream of a PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EsInfo {
    pub(crate) stream_type: u8,
    pub(crate) pid: u16,
    pub(crate) desc: Vec<u8>,
}

/// A program definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pmt {
    pub(crate) program: u16,
    pub(crate) pcr_pid: u16,
    pub(crate) info: Vec<u8>,
    pub(crate) streams: Vec<EsInfo>,
}

/// Parses a PMT section (CRC already checked). Reports loop overruns.
pub(crate) fn parse_pmt(d: &[u8], pid: u16, sink: &mut Sink) -> Option<Pmt> {
    let h = Hdr::parse(d)?;
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(8..end)?);
    let pcr_pid = r.u16()? & 0x1FFF;
    let info_len = usize::from(r.u16()? & 0x0FFF);
    let Some(info) = r.take(info_len) else {
        sink.emit(Rule::SectSyntax, Some(pid), || {
            format!("PMT program_info_length {info_len} runs past the section")
        });
        return None;
    };
    let mut streams = Vec::new();
    while r.left() > 0 {
        let (Some(st), Some(epid), Some(l)) = (r.u8(), r.u16(), r.u16()) else {
            sink.emit(Rule::SectSyntax, Some(pid), || {
                "PMT elementary stream loop ends inside an entry".into()
            });
            return None;
        };
        let l = usize::from(l & 0x0FFF);
        let Some(desc) = r.take(l) else {
            sink.emit(Rule::SectSyntax, Some(pid), || {
                format!("PMT ES_info_length {l} runs past the section")
            });
            return None;
        };
        streams.push(EsInfo {
            stream_type: st,
            pid: epid & 0x1FFF,
            desc: desc.to_vec(),
        });
    }
    Some(Pmt {
        program: h.ext,
        pcr_pid,
        info: info.to_vec(),
        streams,
    })
}

/// The fields of an AC-3_audio_stream_descriptor (A/52 Table A4.1) the checks need.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Ac3Desc {
    pub(crate) sample_rate_code: u8,
    pub(crate) bsid: u8,
    pub(crate) bit_rate_code: u8,
    pub(crate) bsmod: u8,
    pub(crate) num_channels: u8,
    pub(crate) full_svc: bool,
    pub(crate) mainid: Option<u8>,
    pub(crate) priority: Option<u8>,
    pub(crate) language: Option<u32>,
}

/// Parses and checks an AC-3_audio_stream_descriptor body.
pub(crate) fn ac3_desc(b: &[u8], pid: u16, sink: &mut Sink) -> Option<Ac3Desc> {
    let p = Some(pid);
    if b.len() < 3 {
        sink.emit(Rule::DescAc3, p, || {
            format!(
                "AC-3 descriptor of {} bytes: the first three are required",
                b.len()
            )
        });
        return None;
    }
    let mut d = Ac3Desc {
        sample_rate_code: b[0] >> 5,
        bsid: b[0] & 0x1F,
        bit_rate_code: b[1] >> 2,
        bsmod: (b[2] >> 5) & 7,
        num_channels: (b[2] >> 1) & 0x0F,
        full_svc: b[2] & 1 != 0,
        ..Ac3Desc::default()
    };
    let surround = b[1] & 3;
    let mut bad: Vec<String> = Vec::new();
    if d.sample_rate_code == 3 {
        bad.push("sample_rate_code 011 is reserved".into());
    }
    if surround == 3 {
        bad.push("surround_mode 11 is reserved".into());
    }
    if d.num_channels >= 14 {
        bad.push(format!("num_channels {:04b} is reserved", d.num_channels));
    }
    let mut r = Rd::new(&b[3..]);
    // Optional tail: langcod, [langcod2], mainid/priority or asvcflags, textlen, text,
    // language flags, languages.
    if let Some(langcod) = r.u8() {
        if langcod != 0xFF {
            bad.push(format!("langcod 0x{langcod:02X} (shall be 0xFF)"));
        }
        if d.num_channels == 0 {
            match r.u8() {
                Some(0xFF) => {}
                Some(v) => bad.push(format!("langcod2 0x{v:02X} (shall be 0xFF)")),
                None => bad.push("langcod2 missing for num_channels 0000".into()),
            }
        }
        if let Some(x) = r.u8() {
            if d.bsmod < 2 {
                d.mainid = Some(x >> 5);
                let prio = (x >> 3) & 3;
                d.priority = Some(prio);
                if prio == 0 {
                    bad.push("priority 00 is reserved".into());
                }
            }
            if let Some(t) = r.u8() {
                let textlen = usize::from(t >> 1);
                if r.take(textlen).is_none() {
                    bad.push(format!("textlen {textlen} runs past the descriptor"));
                } else if let Some(f) = r.u8() {
                    let lf = f & 0x80 != 0;
                    let lf2 = f & 0x40 != 0;
                    if lf2 && d.num_channels != 0 {
                        bad.push("language_flag_2 set with num_channels other than 0000".into());
                    }
                    if lf {
                        match r.u24() {
                            Some(l) => d.language = Some(l),
                            None => {
                                bad.push("language_flag set but the language is missing".into())
                            }
                        }
                    }
                    if lf2 && r.u24().is_none() {
                        bad.push("language_flag_2 set but language_2 is missing".into());
                    }
                }
            }
        }
    }
    for m in bad {
        sink.emit(Rule::DescAc3, p, || format!("AC-3 descriptor: {m}"));
    }
    let mut atsc: Vec<String> = Vec::new();
    if !(d.bit_rate_code <= 15 || (32..=47).contains(&d.bit_rate_code)) {
        atsc.push(format!(
            "bit_rate_code {} signals more than 448 kbit/s",
            d.bit_rate_code
        ));
    }
    if !(1..=13).contains(&d.num_channels) {
        atsc.push(format!("num_channels {} outside 1 to 13", d.num_channels));
    }
    for m in atsc {
        sink.emit(Rule::DescAc3Atsc, p, || format!("AC-3 descriptor: {m}"));
    }
    Some(d)
}

/// What the program checks found about one audio stream.
#[derive(Debug, Clone)]
struct AudioInfo {
    pid: u16,
    ac3: Option<Ac3Desc>,
    iso639: Option<(u32, u8)>,
    component_name: bool,
}

/// Checks a PMT's contents (run when a PMT is new or changed).
pub(crate) fn check_pmt(pmt: &Pmt, pmt_pid: u16, sink: &mut Sink) -> BTreeMap<u16, Ac3Desc> {
    let mut ac3_map = BTreeMap::new();
    let pp = Some(pmt_pid);
    let prog = pmt.program;
    // Program loop.
    match descriptors(&pmt.info) {
        Err(at) => sink.emit(Rule::DescLoop, pp, || {
            format!("program {prog}: program_info descriptor at offset {at} overruns the loop")
        }),
        Ok(ds) => {
            loop_dups(&ds, &format!("program {prog} program_info"), pmt_pid, sink);
            let ga94 = ds
                .iter()
                .any(|(t, b)| *t == 0x05 && b.len() >= 4 && &b[..4] == b"GA94");
            if !ga94 {
                sink.emit(Rule::DescGa94, pp, || {
                    format!("program {prog}: no registration descriptor GA94 in program_info")
                });
            }
            for (t, b) in &ds {
                if *t == 0x86 {
                    caption_desc(b, pmt_pid, sink);
                }
            }
        }
    }
    let mut video = 0;
    let mut audios: Vec<AudioInfo> = Vec::new();
    for es in &pmt.streams {
        let kind = Kind::of(es.stream_type);
        let what = format!(
            "program {prog} PID 0x{:04X} (stream_type 0x{:02X})",
            es.pid, es.stream_type
        );
        if kind.is_video() {
            video += 1;
        }
        if !matches!(
            es.stream_type,
            0x01 | 0x02
                | 0x05
                | 0x06
                | 0x0F
                | 0x11
                | 0x1B
                | 0x24
                | 0x2D
                | 0x2E
                | 0x80
                | 0x81
                | 0x87
                | 0x88
                | 0xC4..=0xFF
        ) {
            sink.emit(Rule::PmtStreamType, pp, || {
                format!("{what}: stream_type not among those SCTE 54 Table 3 lists")
            });
        }
        let ds = match descriptors(&es.desc) {
            Ok(ds) => ds,
            Err(at) => {
                sink.emit(Rule::DescLoop, pp, || {
                    format!("{what}: ES_info descriptor at offset {at} overruns the loop")
                });
                continue;
            }
        };
        loop_dups(&ds, &what, pmt_pid, sink);
        let has = |tag: u8| ds.iter().any(|(t, _)| *t == tag);
        if es.stream_type >= 0xC4 && !has(0x05) {
            sink.emit(Rule::PmtPrivateReg, pp, || {
                format!("{what}: user-private stream_type without a registration descriptor")
            });
        }
        if matches!(es.stream_type, 0x02 | 0x1B) {
            let dsa = ds.iter().find(|(t, _)| *t == 0x06);
            match dsa {
                None => sink.emit(Rule::PmtDsa, pp, || {
                    format!("{what}: no data_stream_alignment_descriptor")
                }),
                Some((_, b)) if b.len() != 1 || b[0] != 0x02 => sink.emit(Rule::PmtDsa, pp, || {
                    format!(
                        "{what}: data_stream_alignment_descriptor of length {} with alignment_type {:?} (shall be length 1, 0x02)",
                        b.len(),
                        b.first()
                    )
                }),
                Some(_) => {}
            }
        }
        if es.stream_type == 0x81 && !has(0x81) {
            sink.emit(Rule::PmtAudioDesc, pp, || {
                format!("{what}: no AC-3_audio_stream_descriptor")
            });
        }
        if es.stream_type == 0x87 && !has(0xCC) {
            sink.emit(Rule::PmtAudioDesc, pp, || {
                format!("{what}: no E-AC-3_audio_descriptor (tag 0xCC)")
            });
        }
        let mut info = AudioInfo {
            pid: es.pid,
            ac3: None,
            iso639: None,
            component_name: has(0xA3),
        };
        for (t, b) in &ds {
            match t {
                0x81 if es.stream_type == 0x81 => {
                    info.ac3 = ac3_desc(b, pmt_pid, sink);
                }
                0x0A => {
                    if b.len() % 4 != 0 || b.is_empty() {
                        sink.emit(Rule::DescIso639, pp, || {
                            format!("{what}: ISO_639_language_descriptor of length {}", b.len())
                        });
                    } else {
                        info.iso639 = Some((u32::from_be_bytes([0, b[0], b[1], b[2]]), b[3]));
                    }
                }
                0x86 => caption_desc(b, pmt_pid, sink),
                _ => {}
            }
        }
        if es.stream_type == 0x81 {
            if let Some(a) = info.ac3 {
                ac3_map.insert(es.pid, a);
            }
            audios.push(info);
        }
    }
    if video > 1 {
        sink.emit(Rule::PmtVideoCount, pp, || {
            format!("program {prog}: {video} video elementary streams")
        });
    }
    program_audio_checks(prog, &audios, pmt_pid, sink);
    ac3_map
}

fn loop_dups(ds: &[(u8, &[u8])], what: &str, pid: u16, sink: &mut Sink) {
    let mut seen = [0u8; 256];
    for (t, _) in ds {
        seen[usize::from(*t)] = seen[usize::from(*t)].saturating_add(1);
    }
    for (t, &n) in seen.iter().enumerate() {
        if n < 2 {
            continue;
        }
        if t == 0x05 {
            sink.emit(Rule::DescRegDup, Some(pid), || {
                format!("{what}: {n} registration descriptors in one loop")
            });
        } else if t != 0x09 {
            // CA descriptors may occur once per CA system.
            sink.emit(Rule::DescDup, Some(pid), || {
                format!("{what}: descriptor tag 0x{t:02X} occurs {n} times in one loop")
            });
        }
    }
}

/// Checks a caption_service_descriptor body (A/65 Table 6.26).
pub(crate) fn caption_desc(b: &[u8], pid: u16, sink: &mut Sink) {
    let p = Some(pid);
    let Some(&first) = b.first() else {
        sink.emit(Rule::DescCaption, p, || {
            "caption_service_descriptor is empty".into()
        });
        return;
    };
    let n = usize::from(first & 0x1F);
    if !(1..=16).contains(&n) {
        sink.emit(Rule::DescCaption, p, || {
            format!("caption_service_descriptor: number_of_services {n} outside 1 to 16")
        });
    }
    if b.len() != 1 + 6 * n {
        sink.emit(Rule::DescCaption, p, || {
            format!(
                "caption_service_descriptor: length {} for {n} services (shall be {})",
                b.len(),
                1 + 6 * n
            )
        });
        return;
    }
    for k in 0..n {
        let s = &b[1 + 6 * k..1 + 6 * (k + 1)];
        let digital = s[3] & 0x80 != 0;
        if digital {
            let num = s[3] & 0x3F;
            if num == 0 {
                sink.emit(Rule::DescCaption, p, || {
                    format!("caption_service_descriptor: service {k} is digital_cc with caption_service_number 0")
                });
            }
        }
    }
}

fn program_audio_checks(prog: u16, audios: &[AudioInfo], pid: u16, sink: &mut Sink) {
    let p = Some(pid);
    let with: Vec<(&AudioInfo, Ac3Desc)> = audios
        .iter()
        .filter_map(|a| a.ac3.map(|d| (a, d)))
        .collect();
    if !with.is_empty() && !with.iter().any(|(_, d)| d.bsmod == 0) {
        sink.emit(Rule::PmtAudioCm, p, || {
            format!("program {prog}: no AC-3 stream with bsmod 0 (complete main)")
        });
    }
    let primaries = with.iter().filter(|(_, d)| d.priority == Some(1)).count();
    if primaries > 1 {
        sink.emit(Rule::DescAc3Primary, p, || {
            format!("program {prog}: {primaries} AC-3 streams labeled primary")
        });
    }
    let mut mainids: BTreeMap<u8, u16> = BTreeMap::new();
    for (a, d) in &with {
        if d.bsmod < 2
            && let Some(m) = d.mainid
            && let Some(other) = mainids.insert(m, a.pid)
        {
            let pid_a = a.pid;
            sink.emit(Rule::DescAc3Mainid, p, || {
                format!(
                    "program {prog}: PIDs 0x{other:04X} and 0x{pid_a:04X} are main services with mainid {m}"
                )
            });
        }
    }
    // Streams sharing a bsmod.
    let mut by_bsmod: BTreeMap<u8, Vec<&(&AudioInfo, Ac3Desc)>> = BTreeMap::new();
    for x in &with {
        by_bsmod.entry(x.1.bsmod).or_default().push(x);
    }
    let any_iso = audios.iter().any(|a| a.iso639.is_some());
    for (bsmod, group) in &by_bsmod {
        if group.len() < 2 {
            continue;
        }
        for (a, d) in group {
            if d.language.is_none() {
                let pid_a = a.pid;
                sink.emit(Rule::DescAc3Language, p, || {
                    format!(
                        "program {prog}: PID 0x{pid_a:04X} shares bsmod {bsmod} with another AC-3 stream but its AC-3 descriptor has no language"
                    )
                });
            }
            if any_iso && a.iso639.is_none() {
                let pid_a = a.pid;
                sink.emit(Rule::DescIso639Atsc, p, || {
                    format!(
                        "program {prog}: PID 0x{pid_a:04X} shares bsmod {bsmod} with another AC-3 stream and has no ISO_639_language_descriptor although others do"
                    )
                });
            }
        }
        // Same language too: component names required.
        let mut by_lang: BTreeMap<Option<u32>, Vec<u16>> = BTreeMap::new();
        for (a, d) in group {
            let l = d.language.or(a.iso639.map(|x| x.0));
            by_lang.entry(l).or_default().push(a.pid);
        }
        for (l, pids) in by_lang {
            if pids.len() < 2 {
                continue;
            }
            for pid_a in pids {
                let has = audios.iter().any(|a| a.pid == pid_a && a.component_name);
                if !has {
                    let ls = l.map(lang).unwrap_or_else(|| "(none)".into());
                    sink.emit(Rule::DescComponentName, p, || {
                        format!(
                            "program {prog}: PID 0x{pid_a:04X} shares language {ls} and bsmod {bsmod} with another audio stream but has no component_name_descriptor"
                        )
                    });
                }
            }
        }
    }
    for (a, d) in &with {
        if let Some((code, audio_type)) = a.iso639 {
            if let Some(l) = d.language
                && l != code
            {
                let pid_a = a.pid;
                sink.emit(Rule::DescIso639Atsc, p, || {
                    format!(
                        "program {prog}: PID 0x{pid_a:04X}: ISO 639 language {} differs from the AC-3 descriptor's {}",
                        lang(code),
                        lang(l)
                    )
                });
            }
            if audio_type != 0 {
                let pid_a = a.pid;
                sink.emit(Rule::DescIso639Atsc, p, || {
                    format!(
                        "program {prog}: PID 0x{pid_a:04X}: ISO 639 audio_type 0x{audio_type:02X} (A/53 Part 3 requires 0x00)"
                    )
                });
            }
        }
    }
}
