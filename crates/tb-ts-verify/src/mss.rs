//! multiple_string_structure() of A/65 §6.10 (Tables 6.39 to 6.41, p.79-82).

use crate::bytes::{Rd, lang};
use crate::report::Sink;
use crate::rules::Rule;

fn reserved_mode(m: u8) -> bool {
    matches!(m, 0x07..=0x08 | 0x11..=0x1F | 0x28..=0x2F | 0x34..=0x3D | 0x42..=0x47 | 0x49..=0xDF)
}

/// Decodes a multiple_string_structure, reporting syntax errors as `what` at `pid`.
/// Returns the first string's text (empty when there is none), with characters the checker
/// cannot decode (Huffman, SCSU, other systems) shown as '?'.
pub(crate) fn decode(data: &[u8], what: &str, pid: u16, sink: &mut Sink) -> String {
    let mut r = Rd::new(data);
    let pid_o = Some(pid);
    let mut first: Option<String> = None;
    let Some(n) = r.u8() else {
        if !data.is_empty() {
            sink.emit(Rule::MssSyntax, pid_o, || {
                format!("{what}: empty string structure")
            });
        }
        return String::new();
    };
    for _ in 0..n {
        let (Some(code), Some(segs)) = (r.u24(), r.u8()) else {
            sink.emit(Rule::MssSyntax, pid_o, || {
                format!(
                    "{what}: string header runs past the {} bytes of the field",
                    data.len()
                )
            });
            return first.unwrap_or_default();
        };
        let mut text = String::new();
        for _ in 0..segs {
            let (Some(comp), Some(mode), Some(nb)) = (r.u8(), r.u8(), r.u8()) else {
                sink.emit(Rule::MssSyntax, pid_o, || {
                    format!("{what}: segment header runs past the end of the field")
                });
                return first.unwrap_or(text);
            };
            let Some(bytes) = r.take(usize::from(nb)) else {
                sink.emit(Rule::MssSyntax, pid_o, || {
                    format!("{what}: segment of {nb} bytes runs past the end of the field")
                });
                return first.unwrap_or(text);
            };
            if nb == 0 {
                continue;
            }
            match comp {
                0x00 => {}
                0x01 | 0x02 => {
                    sink.emit(Rule::MssHuffman, pid_o, || {
                        format!("{what}: Huffman compression type {comp} (not decoded)")
                    });
                    if mode != 0 {
                        sink.emit(Rule::MssSyntax, pid_o, || {
                            format!("{what}: Huffman compression with mode 0x{mode:02X} (only mode 0x00)")
                        });
                    }
                    text.push('?');
                    continue;
                }
                0x03..=0xAF => {
                    sink.emit(Rule::MssSyntax, pid_o, || {
                        format!("{what}: reserved compression_type 0x{comp:02X}")
                    });
                    continue;
                }
                _ => {
                    text.push('?');
                    continue;
                }
            }
            if reserved_mode(mode) {
                sink.emit(Rule::MssSyntax, pid_o, || {
                    format!(
                        "{what}: reserved mode 0x{mode:02X} (language {})",
                        lang(code)
                    )
                });
                continue;
            }
            match mode {
                0x00..=0x33 => {
                    for &b in bytes {
                        let cp = (u32::from(mode) << 8) | u32::from(b);
                        text.push(char::from_u32(cp).unwrap_or('?'));
                    }
                }
                0x3F => {
                    if bytes.len() % 2 != 0 {
                        sink.emit(Rule::MssSyntax, pid_o, || {
                            format!("{what}: UTF-16 segment of odd length {}", bytes.len())
                        });
                    }
                    let units: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    match String::from_utf16(&units) {
                        Ok(s) => text.push_str(&s),
                        Err(_) => {
                            sink.emit(Rule::MssSyntax, pid_o, || {
                                format!("{what}: UTF-16 segment with an unpaired surrogate")
                            });
                            text.push_str(&String::from_utf16_lossy(&units));
                        }
                    }
                }
                _ => text.push('?'),
            }
        }
        if first.is_none() {
            first = Some(text);
        }
    }
    if r.left() > 0 {
        let left = r.left();
        sink.emit(Rule::MssSyntax, pid_o, || {
            format!("{what}: {left} bytes after the last string")
        });
    }
    first.unwrap_or_default()
}
