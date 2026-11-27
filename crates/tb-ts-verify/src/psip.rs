//! ATSC PSIP tables of A/65 §6 (p.23-82): MGT, TVCT/CVCT, STT, EIT, ETT; their field checks.

use crate::bytes::{Rd, descriptors};
use crate::mss;
use crate::psi::{ac3_desc, caption_desc};
use crate::report::Sink;
use crate::rules::Rule;

/// One table type of the MGT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MgtEntry {
    pub(crate) table_type: u16,
    pub(crate) pid: u16,
    pub(crate) version: u8,
    pub(crate) bytes: u32,
}

/// What a table_type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TableType {
    Tvct { current: bool },
    Cvct { current: bool },
    ChannelEtt,
    Dccsct,
    Eit(u8),
    EventEtt(u8),
    Rrt(u8),
    Private,
    Dcct(u8),
    Reserved,
}

impl TableType {
    pub(crate) fn of(t: u16) -> TableType {
        match t {
            0x0000 => TableType::Tvct { current: true },
            0x0001 => TableType::Tvct { current: false },
            0x0002 => TableType::Cvct { current: true },
            0x0003 => TableType::Cvct { current: false },
            0x0004 => TableType::ChannelEtt,
            0x0005 => TableType::Dccsct,
            0x0100..=0x017F => TableType::Eit((t - 0x0100) as u8),
            0x0200..=0x027F => TableType::EventEtt((t - 0x0200) as u8),
            0x0301..=0x03FF => TableType::Rrt((t - 0x0300) as u8),
            0x0400..=0x0FFF => TableType::Private,
            0x1400..=0x14FF => TableType::Dcct((t - 0x1400) as u8),
            _ => TableType::Reserved,
        }
    }

    pub(crate) fn name(self) -> String {
        match self {
            TableType::Tvct { current } => format!("TVCT{}", if current { "" } else { " next" }),
            TableType::Cvct { current } => format!("CVCT{}", if current { "" } else { " next" }),
            TableType::ChannelEtt => "channel ETT".into(),
            TableType::Dccsct => "DCCSCT".into(),
            TableType::Eit(k) => format!("EIT-{k}"),
            TableType::EventEtt(k) => format!("ETT-{k}"),
            TableType::Rrt(r) => format!("RRT region {r}"),
            TableType::Private => "user private table".into(),
            TableType::Dcct(k) => format!("DCCT {k}"),
            TableType::Reserved => "reserved table_type".into(),
        }
    }
}

/// Parses the MGT and checks its header and table types.
pub(crate) fn parse_mgt(d: &[u8], ext: u16, sink: &mut Sink) -> Option<Vec<MgtEntry>> {
    let p = Some(0x1FFB);
    if ext != 0 {
        sink.emit(Rule::MgtHeader, p, || {
            format!("MGT table_id_extension 0x{ext:04X} (shall be 0x0000)")
        });
    }
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(9..end)?);
    let n = r.u16()?;
    if !(2..=370).contains(&n) {
        sink.emit(Rule::MgtHeader, p, || {
            format!("tables_defined {n} outside 2 to 370 (cable)")
        });
    }
    let mut v = Vec::new();
    for _ in 0..n {
        let (Some(tt), Some(pid), Some(ver), Some(bytes), Some(dl)) =
            (r.u16(), r.u16(), r.u8(), r.u32(), r.u16())
        else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("MGT table loop of {n} entries runs past the section")
            });
            return None;
        };
        let dl = usize::from(dl & 0x0FFF);
        match r.take(dl) {
            Some(ds) => {
                if descriptors(ds).is_err() {
                    sink.emit(Rule::DescLoop, p, || {
                        format!("MGT table_type 0x{tt:04X}: descriptors overrun their loop")
                    });
                }
            }
            None => {
                sink.emit(Rule::SectSyntax, p, || {
                    format!("MGT table_type 0x{tt:04X}: table_type_descriptors_length {dl} runs past the section")
                });
                return None;
            }
        }
        v.push(MgtEntry {
            table_type: tt,
            pid: pid & 0x1FFF,
            version: ver & 0x1F,
            bytes,
        });
    }
    if let Some(dl) = r.u16() {
        let dl = usize::from(dl & 0x0FFF);
        if r.take(dl).is_none() {
            sink.emit(Rule::SectSyntax, p, || {
                format!("MGT descriptors_length {dl} runs past the section")
            });
        }
    }
    // Table types and PIDs.
    let mut seen_types = std::collections::BTreeSet::new();
    for e in &v {
        let tt = TableType::of(e.table_type);
        if tt == TableType::Reserved {
            sink.emit(Rule::MgtTableType, p, || {
                format!("table_type 0x{:04X} is reserved", e.table_type)
            });
        }
        if !seen_types.insert(e.table_type) {
            sink.emit(Rule::MgtTableType, p, || {
                format!("table_type 0x{:04X} listed twice", e.table_type)
            });
        }
        match tt {
            TableType::Eit(_) | TableType::EventEtt(_) | TableType::ChannelEtt => {
                if e.pid == 0x1FFB {
                    sink.emit(Rule::MgtTableType, p, || {
                        format!("{} on PID 0x1FFB", tt.name())
                    });
                }
                if v.iter()
                    .any(|o| o.table_type != e.table_type && o.pid == e.pid)
                {
                    sink.emit(Rule::MgtTableType, p, || {
                        format!(
                            "{} PID 0x{:04X} is also the PID of another table type",
                            tt.name(),
                            e.pid
                        )
                    });
                }
            }
            TableType::Tvct { .. } | TableType::Cvct { .. } | TableType::Rrt(_)
                if e.pid != 0x1FFB =>
            {
                sink.emit(Rule::MgtTableType, p, || {
                    format!("{} on PID 0x{:04X} (shall be 0x1FFB)", tt.name(), e.pid)
                });
            }
            _ => {}
        }
    }
    Some(v)
}

/// A service_location_descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sld {
    pub(crate) pcr_pid: u16,
    pub(crate) elems: Vec<(u8, u16, u32)>,
}

/// One virtual channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Channel {
    pub(crate) name: String,
    pub(crate) major: u16,
    pub(crate) minor: u16,
    pub(crate) tsid: u16,
    pub(crate) program: u16,
    pub(crate) etm: u8,
    pub(crate) hidden: bool,
    pub(crate) service_type: u8,
    pub(crate) source_id: u16,
    pub(crate) sld: Option<Sld>,
}

impl Channel {
    pub(crate) fn number(&self) -> String {
        if self.major & 0x3F0 == 0x3F0 {
            format!(
                "{}",
                (u32::from(self.major & 0xF) << 10) | u32::from(self.minor)
            )
        } else {
            format!("{}.{}", self.major, self.minor)
        }
    }
}

fn short_name(b: &[u8], what: &str, sink: &mut Sink) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    let n = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let mut bad = None;
    if n == 0 {
        bad = Some("empty".to_string());
    } else if units[n..].iter().any(|&u| u != 0) {
        bad = Some("characters after the 0x0000 padding".to_string());
    }
    let name = match String::from_utf16(&units[..n]) {
        Ok(s) => s,
        Err(_) => {
            bad = Some("an unpaired UTF-16 surrogate".to_string());
            String::from_utf16_lossy(&units[..n])
        }
    };
    if let Some(why) = bad {
        sink.emit(Rule::VctShortName, Some(0x1FFB), || {
            format!("{what}: short_name has {why}")
        });
    }
    name
}

/// Parses a TVCT (0xC8) or CVCT (0xC9) section and checks its channel fields.
pub(crate) fn parse_vct(d: &[u8], sink: &mut Sink) -> Option<Vec<Channel>> {
    let p = Some(0x1FFB);
    let cable = d[0] == 0xC9;
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(9..end)?);
    let n = r.u8()?;
    let mut v = Vec::new();
    for k in 0..n {
        let Some(c) = r.take(32) else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("VCT channel {k} of {n} runs past the section")
            });
            return None;
        };
        let major = (u16::from(c[14] & 0x0F) << 6) | u16::from(c[15] >> 2);
        let minor = (u16::from(c[15] & 3) << 8) | u16::from(c[16]);
        let what = format!("channel {major}.{minor}");
        let name = short_name(&c[..14], &what, sink);
        let modulation = c[17];
        let carrier = u32::from_be_bytes([c[18], c[19], c[20], c[21]]);
        let tsid = u16::from_be_bytes([c[22], c[23]]);
        let program = u16::from_be_bytes([c[24], c[25]]);
        let etm = c[26] >> 6;
        let hidden = c[26] & 0x10 != 0;
        let service_type = c[27] & 0x3F;
        let source_id = u16::from_be_bytes([c[28], c[29]]);
        let dl = usize::from(u16::from_be_bytes([c[30], c[31]]) & 0x03FF);
        let Some(ds) = r.take(dl) else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("{what}: descriptors_length {dl} runs past the section")
            });
            return None;
        };
        let mut sld = None;
        match descriptors(ds) {
            Err(at) => sink.emit(Rule::DescLoop, p, || {
                format!("{what}: descriptor at offset {at} overruns the loop")
            }),
            Ok(list) => {
                for (t, b) in list {
                    match t {
                        0xA0 => {
                            let _ = mss::decode(
                                b,
                                &format!("{what} extended_channel_name"),
                                0x1FFB,
                                sink,
                            );
                        }
                        0xA1 => {
                            let mut rr = Rd::new(b);
                            let parsed = (|| {
                                let pcr = rr.u16()? & 0x1FFF;
                                let ne = rr.u8()?;
                                let mut elems = Vec::new();
                                for _ in 0..ne {
                                    let st = rr.u8()?;
                                    let pid = rr.u16()? & 0x1FFF;
                                    let l = rr.u24()?;
                                    elems.push((st, pid, l));
                                }
                                Some(Sld {
                                    pcr_pid: pcr,
                                    elems,
                                })
                            })();
                            match parsed {
                                Some(s) => sld = Some(s),
                                None => sink.emit(Rule::DescLoop, p, || {
                                    format!("{what}: service_location_descriptor shorter than its elements")
                                }),
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Field checks.
        let mut bad: Vec<String> = Vec::new();
        let active = program != 0;
        if active && (modulation == 0 || (0x06..=0x7F).contains(&modulation)) {
            bad.push(format!("modulation_mode 0x{modulation:02X} is reserved"));
        }
        if etm == 3 {
            bad.push("ETM_location 11 is reserved".into());
        }
        if service_type == 0 {
            bad.push("service_type 0x00 is reserved".into());
        }
        if !cable && source_id == 0 {
            bad.push("source_id 0 is reserved in a TVCT".into());
        }
        if !active && !hidden {
            bad.push("inactive channel (program_number 0) is not hidden".into());
        }
        for m in bad {
            sink.emit(Rule::VctFields, p, || format!("{what}: {m}"));
        }
        if carrier != 0 {
            sink.emit(Rule::VctCarrier, p, || {
                format!("{what}: carrier_frequency {carrier} (shall be 0)")
            });
        }
        let range_ok = if cable {
            (major < 1000 && minor < 1000) || major & 0x3F0 == 0x3F0
        } else {
            (1..=99).contains(&major)
                && minor <= 999
                && (!matches!(service_type, 0x02 | 0x03) || (1..=99).contains(&minor))
                && (service_type != 0x01 || minor == 0)
        };
        if !range_ok {
            sink.emit(Rule::VctNumber, p, || {
                format!(
                    "{} channel number {major}/{minor} (service_type 0x{service_type:02X}) outside the allowed ranges",
                    if cable { "CVCT" } else { "TVCT" }
                )
            });
        }
        if !cable && active && service_type != 0x01 && sld.is_none() {
            sink.emit(Rule::VctSldMissing, p, || {
                format!("{what}: active TVCT channel without service_location_descriptor")
            });
        }
        v.push(Channel {
            name,
            major,
            minor,
            tsid,
            program,
            etm,
            hidden,
            service_type,
            source_id,
            sld,
        });
    }
    if let Some(dl) = r.u16() {
        let dl = usize::from(dl & 0x03FF);
        if r.take(dl).is_none() {
            sink.emit(Rule::SectSyntax, p, || {
                format!("VCT additional_descriptors_length {dl} runs past the section")
            });
        }
    }
    Some(v)
}

/// The STT's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stt {
    pub(crate) system_time: u32,
    pub(crate) gps_utc_offset: u8,
}

/// Parses and checks an STT.
pub(crate) fn parse_stt(d: &[u8], h_ext: u16, h_version: u8, sink: &mut Sink) -> Option<Stt> {
    let p = Some(0x1FFB);
    if h_ext != 0 || h_version != 0 {
        sink.emit(Rule::SttHeader, p, || {
            format!("STT table_id_extension 0x{h_ext:04X}, version_number {h_version} (shall be 0 and 0)")
        });
    }
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(9..end)?);
    let st = r.u32()?;
    let off = r.u8()?;
    let ds = r.u16()?;
    let day = (ds >> 8) & 0x1F;
    let hour = ds & 0xFF;
    if hour > 18 || (day == 0 && hour != 0) {
        sink.emit(Rule::SttDs, p, || {
            format!("daylight_saving: DS_day_of_month {day}, DS_hour {hour}")
        });
    }
    if off != 18 {
        sink.emit(Rule::SttGpsUtc, p, || format!("GPS_UTC_offset {off} s"));
    }
    if descriptors(r.rest()).is_err() {
        sink.emit(Rule::DescLoop, p, || {
            "STT descriptors overrun the section".into()
        });
    }
    Some(Stt {
        system_time: st,
        gps_utc_offset: off,
    })
}

/// One event of an EIT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) id: u16,
    pub(crate) start: u32,
    pub(crate) len: u32,
    pub(crate) etm: u8,
}

/// Parses an EIT section; checks titles, descriptors and the order within the section.
pub(crate) fn parse_eit(
    d: &[u8],
    pid: u16,
    source: u16,
    sink: &mut Sink,
    advisories: &mut Vec<RegionRating>,
) -> Option<Vec<Event>> {
    let p = Some(pid);
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(9..end)?);
    let n = r.u8()?;
    let mut v: Vec<Event> = Vec::new();
    for k in 0..n {
        let (Some(id), Some(start), Some(x), Some(tl)) = (r.u16(), r.u32(), r.u24(), r.u8()) else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("EIT source {source}: event {k} of {n} runs past the section")
            });
            return None;
        };
        let what = format!("EIT source {source} event {}", id & 0x3FFF);
        let Some(title) = r.take(usize::from(tl)) else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("{what}: title runs past the section")
            });
            return None;
        };
        let _ = mss::decode(title, &format!("{what} title"), pid, sink);
        let Some(dl) = r.u16() else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("{what}: event runs past the section")
            });
            return None;
        };
        let dl = usize::from(dl & 0x0FFF);
        let Some(ds) = r.take(dl) else {
            sink.emit(Rule::SectSyntax, p, || {
                format!("{what}: descriptors_length {dl} runs past the section")
            });
            return None;
        };
        match descriptors(ds) {
            Err(at) => sink.emit(Rule::DescLoop, p, || {
                format!("{what}: descriptor at offset {at} overruns the loop")
            }),
            Ok(list) => {
                for (t, b) in list {
                    match t {
                        0x86 => caption_desc(b, pid, sink),
                        0x81 => {
                            let _ = ac3_desc(b, pid, sink);
                        }
                        0x87 => advisories.extend(content_advisory(b, &what, pid, sink)),
                        _ => {}
                    }
                }
            }
        }
        let ev = Event {
            id: id & 0x3FFF,
            start,
            len: x & 0x0F_FFFF,
            etm: ((x >> 20) & 3) as u8,
        };
        if let Some(prev) = v.last()
            && u64::from(ev.start) < u64::from(prev.start) + u64::from(prev.len)
        {
            let (pe, ps, pl) = (prev.id, prev.start, prev.len);
            sink.emit(Rule::EitOrder, p, || {
                format!(
                    "{what} starts at {} before the end of event {pe} ({ps} + {pl} s)",
                    ev.start
                )
            });
        }
        v.push(ev);
    }
    Some(v)
}

/// Parses an ETT: its ETM_id (the text is decoded for the syntax checks).
pub(crate) fn parse_ett(d: &[u8], pid: u16, sink: &mut Sink) -> Option<u32> {
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(9..end)?);
    let id = r.u32()?;
    let _ = mss::decode(r.rest(), &format!("ETM 0x{id:08X}"), pid, sink);
    Some(id)
}

/// What an RRT defines: its region and the values_defined of each dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rrt {
    pub(crate) region: u8,
    pub(crate) values: Vec<u8>,
}

/// Parses an RRT (A/65 Table 6.10) and checks its syntax.
pub(crate) fn parse_rrt(d: &[u8], ext: u16, pid: u16, sink: &mut Sink) -> Option<Rrt> {
    let p = Some(pid);
    let region = (ext & 0xFF) as u8;
    let what = format!("RRT region {region}");
    let bad = |sink: &mut Sink, m: String| sink.emit(Rule::RrtSyntax, p, || m);
    if ext >> 8 != 0xFF {
        bad(
            sink,
            format!(
                "{what}: the reserved byte of table_id_extension is 0x{:02X}, not 0xFF",
                ext >> 8
            ),
        );
    }
    if region == 0 {
        bad(sink, format!("{what}: rating_region 0 shall not be used"));
    }
    let end = d.len().checked_sub(4)?;
    let mut r = Rd::new(d.get(8..end)?);
    let short = |sink: &mut Sink| {
        sink.emit(Rule::RrtSyntax, p, || {
            format!("RRT region {region}: runs past the section")
        })
    };
    // protocol_version: PSIP-PROTOCOL-VERSION checks it for every PSIP table
    if r.u8().is_none() {
        short(sink);
        return None;
    }
    let text = |r: &mut Rd<'_>, name: &str, sink: &mut Sink| -> Option<String> {
        let n = usize::from(r.u8()?);
        let t = r.take(n)?;
        Some(mss::decode(t, &format!("{what} {name}"), pid, sink))
    };
    if text(&mut r, "rating_region_name_text", sink).is_none() {
        short(sink);
        return None;
    }
    let Some(nd) = r.u8() else {
        short(sink);
        return None;
    };
    if nd == 0 {
        bad(sink, format!("{what}: dimensions_defined 0"));
    }
    let mut values = Vec::new();
    for k in 0..nd {
        if text(&mut r, "dimension_name_text", sink).is_none() {
            short(sink);
            return None;
        }
        let Some(b) = r.u8() else {
            short(sink);
            return None;
        };
        let nv = b & 0x0F;
        if nv == 0 {
            bad(sink, format!("{what}: dimension {k}: values_defined 0"));
        }
        for j in 0..nv {
            let (Some(a), Some(t)) = (
                text(&mut r, "abbrev_rating_value_text", sink),
                text(&mut r, "rating_value_text", sink),
            ) else {
                short(sink);
                return None;
            };
            if j == 0 && !(a.is_empty() && t.is_empty()) {
                bad(
                    sink,
                    format!(
                        "{what}: dimension {k}: value 0 must have null strings, has {a:?} / {t:?}"
                    ),
                );
            }
        }
        values.push(nv);
    }
    let Some(dl) = r.u16() else {
        short(sink);
        return None;
    };
    let dl = usize::from(dl & 0x03FF);
    if r.take(dl).is_none() {
        short(sink);
        return None;
    }
    if !r.rest().is_empty() {
        bad(sink, format!("{what}: bytes after the descriptors"));
    }
    Some(Rrt { region, values })
}

/// One region's rating in a content advisory: (dimension, value) pairs.
pub(crate) type RegionRating = (u8, Vec<(u8, u8)>);

/// Parses a content_advisory_descriptor's payload (A/65 Table 6.27) and checks its syntax.
pub(crate) fn content_advisory(
    b: &[u8],
    what: &str,
    pid: u16,
    sink: &mut Sink,
) -> Vec<RegionRating> {
    let p = Some(pid);
    let mut out: Vec<RegionRating> = Vec::new();
    let mut r = Rd::new(b);
    let short = |sink: &mut Sink| {
        sink.emit(Rule::AdvisorySyntax, p, || {
            format!("{what}: content_advisory_descriptor runs past its length")
        })
    };
    let Some(h) = r.u8() else {
        short(sink);
        return out;
    };
    let n = h & 0x3F;
    if !(1..=8).contains(&n) {
        sink.emit(Rule::AdvisorySyntax, p, || {
            format!("{what}: rating_region_count {n}")
        });
    }
    for _ in 0..n {
        let (Some(region), Some(nd)) = (r.u8(), r.u8()) else {
            short(sink);
            return out;
        };
        let mut dims = Vec::new();
        for _ in 0..nd {
            let (Some(dim), Some(v)) = (r.u8(), r.u8()) else {
                short(sink);
                return out;
            };
            dims.push((dim, v & 0x0F));
        }
        if !dims.windows(2).all(|w| w[0].0 < w[1].0) {
            sink.emit(Rule::AdvisorySyntax, p, || {
                format!("{what}: region {region}: rating dimensions not in increasing order")
            });
        }
        let Some(dl) = r.u8() else {
            short(sink);
            return out;
        };
        if dl > 80 {
            sink.emit(Rule::AdvisorySyntax, p, || {
                format!("{what}: region {region}: rating_description_length {dl}")
            });
        }
        let Some(t) = r.take(usize::from(dl)) else {
            short(sink);
            return out;
        };
        if !t.is_empty() {
            let _ = mss::decode(
                t,
                &format!("{what} region {region} rating_description_text"),
                pid,
                sink,
            );
        }
        if out.iter().any(|(g, _)| *g == region) {
            sink.emit(Rule::AdvisorySyntax, p, || {
                format!("{what}: region {region} twice")
            });
        }
        out.push((region, dims));
    }
    if !r.rest().is_empty() {
        sink.emit(Rule::AdvisorySyntax, p, || {
            format!("{what}: bytes after the last region")
        });
    }
    out
}
