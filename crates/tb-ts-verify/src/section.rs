//! Reassembly of sections from transport packets (H.222.0 §2.4.4.1, §2.4.4.2, p.46).

use crate::report::Sink;
use crate::rules::Rule;

/// A complete section.
#[derive(Debug, Clone)]
pub(crate) struct SectionOut {
    /// The section, from table_id to its last byte.
    pub(crate) data: Vec<u8>,
    /// The section began right after a pointer_field of 0 in a packet with PUSI 1.
    pub(crate) at_payload_start: bool,
    /// Stream offset of the section's last byte.
    pub(crate) end_pos: u64,
}

/// Reassembly state of one section PID.
#[derive(Debug, Clone, Default)]
pub(crate) struct Assembler {
    buf: Vec<u8>,
    need: Option<usize>,
    active: bool,
    at_payload_start: bool,
}

/// Maximum section_length of any section (private sections, H.222.0 §2.4.4.11).
const MAX_SECTION_LENGTH: usize = 4093;

impl Assembler {
    /// Drops the section in progress (after a continuity error or a loss of sync).
    pub(crate) fn reset(&mut self) {
        self.buf.clear();
        self.need = None;
        self.active = false;
    }

    /// Feeds the payload of one packet. `payload_pos` is the stream offset of the payload's
    /// first byte.
    pub(crate) fn push(
        &mut self,
        payload: &[u8],
        pusi: bool,
        payload_pos: u64,
        pid: u16,
        sink: &mut Sink,
        out: &mut Vec<SectionOut>,
    ) {
        let pid_o = Some(pid);
        if pusi {
            let Some((&ptr, rest)) = payload.split_first() else {
                sink.emit(Rule::SectPointer, pid_o, || {
                    "payload_unit_start_indicator set but the payload is empty".into()
                });
                self.reset();
                return;
            };
            let ptr = usize::from(ptr);
            if ptr > rest.len() {
                sink.emit(Rule::SectPointer, pid_o, || {
                    format!(
                        "pointer_field {ptr} points beyond the {} payload bytes",
                        rest.len()
                    )
                });
                self.reset();
                return;
            }
            let tail = &rest[..ptr];
            if self.active {
                let used = self.feed(tail, payload_pos + 1, out);
                if self.active {
                    sink.emit(Rule::SectIncomplete, pid_o, || {
                        "a new section starts before the one in progress is complete".into()
                    });
                    self.reset();
                } else if used < tail.len() {
                    let left = tail.len() - used;
                    sink.emit(Rule::SectPointer, pid_o, || {
                        format!(
                            "pointer_field {ptr} points {left} bytes after the end of the section in progress"
                        )
                    });
                }
            }
            self.parse_from(
                &rest[ptr..],
                payload_pos + 1 + ptr as u64,
                ptr == 0,
                true,
                pid,
                sink,
                out,
            );
        } else if self.active {
            let used = self.feed(payload, payload_pos, out);
            if !self.active && used < payload.len() {
                self.parse_from(
                    &payload[used..],
                    payload_pos + used as u64,
                    false,
                    false,
                    pid,
                    sink,
                    out,
                );
            }
        }
    }

    /// Appends bytes to the section in progress; returns how many were used. Completes the
    /// section into `out` when it is whole.
    fn feed(&mut self, data: &[u8], data_pos: u64, out: &mut Vec<SectionOut>) -> usize {
        let mut used = 0;
        if self.need.is_none() {
            let take = (3 - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            used = take;
            if self.buf.len() < 3 {
                return used;
            }
            let len = usize::from(u16::from_be_bytes([self.buf[1], self.buf[2]]) & 0x0FFF);
            if len > MAX_SECTION_LENGTH {
                // Garbage: dropped here, reported by the length check when it starts in a
                // packet.
                self.reset();
                return data.len();
            }
            self.need = Some(3 + len);
        }
        let need = self.need.unwrap_or(3);
        let take = (need - self.buf.len()).min(data.len() - used);
        self.buf.extend_from_slice(&data[used..used + take]);
        used += take;
        if self.buf.len() == need {
            out.push(SectionOut {
                data: std::mem::take(&mut self.buf),
                at_payload_start: self.at_payload_start,
                end_pos: data_pos + used as u64 - 1,
            });
            self.reset();
        }
        used
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_from(
        &mut self,
        data: &[u8],
        data_pos: u64,
        first_at_start: bool,
        allow_new: bool,
        pid: u16,
        sink: &mut Sink,
        out: &mut Vec<SectionOut>,
    ) {
        let mut i = 0;
        let mut first = true;
        while i < data.len() {
            if data[i] == 0xFF {
                if let Some(k) = data[i..].iter().position(|&b| b != 0xFF) {
                    let at = i + k;
                    sink.emit(Rule::SectStuffing, Some(pid), || {
                        format!(
                            "byte 0x{:02X} at payload offset {} after stuffing bytes",
                            data[at], at
                        )
                    });
                }
                return;
            }
            if !allow_new {
                sink.emit(Rule::SectPusi, Some(pid), || {
                    format!(
                        "a section (table_id 0x{:02X}) starts in a packet with payload_unit_start_indicator 0",
                        data[i]
                    )
                });
                return;
            }
            self.reset();
            self.active = true;
            self.at_payload_start = first && first_at_start;
            first = false;
            if data.len() - i >= 3 {
                let len = usize::from(u16::from_be_bytes([data[i + 1], data[i + 2]]) & 0x0FFF);
                if len > MAX_SECTION_LENGTH {
                    sink.emit(Rule::SectLength, Some(pid), || {
                        format!(
                            "section_length {len} (table_id 0x{:02X}) exceeds 4093",
                            data[i]
                        )
                    });
                    self.reset();
                    return;
                }
            }
            let used = self.feed(&data[i..], data_pos + i as u64, out);
            i += used;
            if self.active {
                return;
            }
        }
    }
}
