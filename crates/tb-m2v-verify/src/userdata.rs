//! Picture user data of ATSC A/53 Part 4 (captions, AFD, bar data) and CTA-708 cc_data().

use crate::rules::Rule;

/// What a user_data() structure is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UdKind {
    /// GA94 with user_data_type_code 0x03.
    Captions(CcInfo),
    /// DTG1; the active_format if the flag is set.
    Afd(Option<u8>),
    /// GA94 with user_data_type_code 0x06.
    Bar,
    /// SCTE 20 (starts with user_data_type_code 0x03 and no identifier).
    Scte20,
    /// GA94 with another type code.
    OtherAtsc(u8),
    /// Anything else.
    Other,
}

impl UdKind {
    /// Position in the order of SCTE 43 clause 7.3, if it has one.
    pub(crate) fn order(&self) -> Option<u8> {
        match self {
            UdKind::Scte20 => Some(0),
            UdKind::Captions(_) => Some(1),
            UdKind::Afd(_) => Some(2),
            UdKind::Bar => Some(3),
            _ => None,
        }
    }

    /// The user_data_type_code under GA94, if any.
    pub(crate) fn atsc_type(&self) -> Option<u8> {
        match self {
            UdKind::Captions(_) => Some(0x03),
            UdKind::Bar => Some(0x06),
            UdKind::OtherAtsc(t) => Some(*t),
            _ => None,
        }
    }
}

/// The content of one cc_data().
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CcInfo {
    pub(crate) cc_count: u8,
    pub(crate) process: bool,
    /// cc_type of the leading CEA-608 constructs (before the first DTVCC construct).
    pub(crate) lead_608: Vec<u8>,
    /// Valid constructs of cc_type 00 and 01.
    pub(crate) valid_608: [u8; 2],
    /// Valid DTVCC constructs.
    pub(crate) valid_dtvcc: u8,
}

/// A problem found in a user data structure: the rule, the byte offset in the payload, the
/// message.
pub(crate) type UdIssue = (Rule, usize, String);

/// Length of the payload without trailing zero bytes.
fn trimmed(p: &[u8]) -> &[u8] {
    let n = p.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    p.get(..n).unwrap_or(p)
}

/// The longest run of zero bits inside the payload.
pub(crate) fn longest_zero_run(p: &[u8]) -> (u32, usize) {
    let mut best = 0u32;
    let mut best_at = 0usize;
    let mut run = 0u32;
    for (i, &b) in p.iter().enumerate() {
        if b == 0 {
            run += 8;
        } else {
            run += b.leading_zeros();
            if run > best {
                best = run;
                best_at = i;
            }
            run = b.trailing_zeros();
        }
    }
    if run > best {
        best = run;
        best_at = p.len();
    }
    (best, best_at)
}

/// Classifies a user data payload (the bytes after the start code) and lists what is wrong
/// with it under A/53 Part 4 and CTA-708.
pub(crate) fn parse(payload: &[u8]) -> (UdKind, Vec<UdIssue>) {
    let mut issues = Vec::new();
    let p = trimmed(payload);
    let kind = match p {
        [0x47, 0x41, 0x39, 0x34, 0x03, rest @ ..] => UdKind::Captions(parse_cc(rest, &mut issues)),
        [0x47, 0x41, 0x39, 0x34, 0x06, rest @ ..] => {
            parse_bar(rest, &mut issues);
            UdKind::Bar
        }
        [0x47, 0x41, 0x39, 0x34, t, ..] => {
            issues.push((
                Rule::UdTypeReserved,
                4,
                format!("ATSC_user_data() with reserved user_data_type_code 0x{t:02X}"),
            ));
            UdKind::OtherAtsc(*t)
        }
        [0x47, 0x41, 0x39, 0x34] => {
            issues.push((
                Rule::UdTypeReserved,
                4,
                "ATSC_user_data() ends before user_data_type_code".to_string(),
            ));
            UdKind::Other
        }
        [0x44, 0x54, 0x47, 0x31, rest @ ..] => UdKind::Afd(parse_afd(rest, &mut issues)),
        [0x03, b, ..] if b & 0xFE == 0x80 => UdKind::Scte20,
        _ => {
            let id: Vec<String> = p.iter().take(4).map(|b| format!("{b:02X}")).collect();
            issues.push((
                Rule::UdIdentifier,
                0,
                format!(
                    "user data with an identifier this profile does not define ({}), {} bytes",
                    id.join(" "),
                    p.len()
                ),
            ));
            UdKind::Other
        }
    };
    (kind, issues)
}

fn parse_cc(d: &[u8], issues: &mut Vec<UdIssue>) -> CcInfo {
    let mut info = CcInfo::default();
    let (Some(&b0), Some(&b1)) = (d.first(), d.get(1)) else {
        issues.push((
            Rule::CcLength,
            5,
            format!(
                "cc_data() is cut short: {} bytes after user_data_type_code",
                d.len()
            ),
        ));
        return info;
    };
    info.cc_count = b0 & 0x1F;
    info.process = b0 & 0x40 != 0;
    if b0 & 0x80 == 0 {
        issues.push((
            Rule::CcFixedBits,
            5,
            format!("first bit of cc_data() is '0' (reserved '1'; process_em_data_flag in CEA-708-D), byte 0x{b0:02X}"),
        ));
    }
    if b0 & 0x20 != 0 {
        issues.push((
            Rule::CcFixedBits,
            5,
            format!(
                "zero_bit of cc_data() is '1' (additional_data_flag in CEA-708-D), byte 0x{b0:02X}"
            ),
        ));
    }
    if b1 != 0xFF {
        issues.push((
            Rule::CcFixedBits,
            6,
            format!("reserved byte of cc_data() is 0x{b1:02X}, not 0xFF (em_data in CEA-708-D)"),
        ));
    }
    if !info.process {
        issues.push((
            Rule::CcProcessFlag,
            5,
            "process_cc_data_flag is '0': decoders discard this cc_data()".to_string(),
        ));
    }
    let need = 2 + 3 * usize::from(info.cc_count);
    let mut dtvcc_seen = false;
    let mut state_reported = false;
    for i in 0..usize::from(info.cc_count) {
        let Some(c) = d.get(2 + 3 * i..5 + 3 * i) else {
            break;
        };
        let head = c.first().copied().unwrap_or(0);
        if head & 0xF8 != 0xF8 {
            issues.push((
                Rule::CcMarker,
                5 + 2 + 3 * i,
                format!("caption construct {i}: one_bit and reserved bits are not '11111' (byte 0x{head:02X})"),
            ));
        }
        let valid = head & 0x04 != 0;
        let ty = head & 0x03;
        if ty >= 2 {
            dtvcc_seen = true;
            if valid {
                info.valid_dtvcc = info.valid_dtvcc.saturating_add(1);
            }
        } else {
            if !dtvcc_seen {
                info.lead_608.push(ty);
            } else if valid && !state_reported {
                state_reported = true;
                issues.push((
                    Rule::CcState,
                    5 + 2 + 3 * i,
                    format!("caption construct {i}: valid CEA-608 construct (cc_type {ty:02b}) after a DTVCC construct"),
                ));
            }
            if valid && let Some(n) = info.valid_608.get_mut(usize::from(ty)) {
                *n = n.saturating_add(1);
            }
        }
    }
    match d.len().cmp(&(need + 1)) {
        std::cmp::Ordering::Less => {
            if d.len() == need {
                issues.push((
                    Rule::CcTrailer,
                    5 + need,
                    "MPEG_cc_data() has no marker_bits byte after cc_data()".to_string(),
                ));
            } else {
                issues.push((
                    Rule::CcLength,
                    5,
                    format!(
                        "cc_count {} needs {} bytes of cc_data(), the user data has {}",
                        info.cc_count,
                        need,
                        d.len()
                    ),
                ));
            }
        }
        std::cmp::Ordering::Equal | std::cmp::Ordering::Greater => {
            let m = d.get(need).copied().unwrap_or(0);
            if m != 0xFF {
                issues.push((
                    Rule::CcTrailer,
                    5 + need,
                    format!("marker_bits after cc_data() is 0x{m:02X}, not 0xFF"),
                ));
            }
            if d.len() > need + 1 {
                issues.push((
                    Rule::CcLength,
                    5 + need + 1,
                    format!(
                        "{} bytes follow MPEG_cc_data() in the same user data",
                        d.len() - need - 1
                    ),
                ));
            }
        }
    }
    info
}

fn parse_afd(d: &[u8], issues: &mut Vec<UdIssue>) -> Option<u8> {
    let Some(&b0) = d.first() else {
        issues.push((Rule::AfdSyntax, 4, "afd_data() is empty".to_string()));
        return None;
    };
    if b0 & 0x80 != 0 || b0 & 0x3F != 0x01 {
        issues.push((
            Rule::AfdSyntax,
            4,
            format!("afd_data() first byte 0x{b0:02X}: the zero bit or the reserved bits '00 0001' are wrong"),
        ));
    }
    let flag = b0 & 0x40 != 0;
    let expect = if flag { 2 } else { 1 };
    if d.len() != expect {
        issues.push((
            Rule::AfdSyntax,
            4,
            format!(
                "afd_data() is {} bytes, active_format_flag {} needs {}",
                d.len(),
                u8::from(flag),
                expect
            ),
        ));
    }
    if !flag {
        return None;
    }
    let b1 = d.get(1).copied()?;
    if b1 & 0xF0 != 0xF0 {
        issues.push((
            Rule::AfdSyntax,
            5,
            format!("afd_data() second byte 0x{b1:02X}: reserved bits are not '1111'"),
        ));
    }
    let af = b1 & 0x0F;
    match af {
        0b0001 | 0b0101..=0b0111 | 0b1100 => issues.push((
            Rule::AfdReserved,
            5,
            format!("active_format {af:04b} is reserved"),
        )),
        0b0010 | 0b0011 => issues.push((
            Rule::AfdNotRecommended,
            5,
            format!("active_format {af:04b} is not recommended"),
        )),
        _ => {}
    }
    Some(af)
}

fn parse_bar(d: &[u8], issues: &mut Vec<UdIssue>) {
    let Some(&b0) = d.first() else {
        issues.push((Rule::BarSyntax, 5, "bar_data() is empty".to_string()));
        return;
    };
    let (top, bottom, left, right) = (
        b0 & 0x80 != 0,
        b0 & 0x40 != 0,
        b0 & 0x20 != 0,
        b0 & 0x10 != 0,
    );
    if b0 & 0x0F != 0x0F {
        issues.push((
            Rule::BarSyntax,
            5,
            format!("bar_data() reserved bits are not '1111' (byte 0x{b0:02X})"),
        ));
    }
    if top != bottom || left != right || (top && left) {
        issues.push((
            Rule::BarFlags,
            5,
            format!(
                "bar_data() flags top {} bottom {} left {} right {}: bars are signaled in pairs, and only one pair",
                u8::from(top),
                u8::from(bottom),
                u8::from(left),
                u8::from(right)
            ),
        ));
    }
    let n = [top, bottom, left, right].iter().filter(|f| **f).count();
    if d.len() != 1 + 2 * n {
        issues.push((
            Rule::BarSyntax,
            5,
            format!(
                "bar_data() is {} bytes, its flags need {}",
                d.len(),
                1 + 2 * n
            ),
        ));
    }
    for i in 0..n {
        if let Some(&b) = d.get(1 + 2 * i)
            && b & 0xC0 != 0xC0
        {
            issues.push((
                Rule::BarSyntax,
                6 + 2 * i,
                format!("bar_data() value {i}: one_bits are not '11' (byte 0x{b:02X})"),
            ));
        }
    }
}
