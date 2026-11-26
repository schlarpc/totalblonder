//! The RF probe's pure half (SPEC §11.1): what an HDHomeRun says about a carrier, checked
//! against what we send, and the rotation of one tuner over the carriers.
//!
//! The adapter runs `hdhomerun_config <id> set /tunerN/channel auto:<Hz>`, then `get
//! /tunerN/status` and `get /tunerN/streaminfo`, and optionally `save` for a capture that goes
//! through `tb-ts-verify`. This crate parses the two text replies
//! (`docs/notes/software-survey.md` §7.1) and turns them into a verdict per carrier.
//!
//! Status line: `ch=qam256:57000000 lock=qam256 ss=83 snq=90 seq=100 bps=38807712 pps=0`
//! (`seq` 100 means no uncorrectable errors). Stream info: one line per program,
//! `1: 50.1 TB1`, possibly with `(encrypted)` or `(no data)`, then `tsid=0x0101`.
#![forbid(unsafe_code)]

use std::time::Duration;

/// A tuner's status reply.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// `ch=`.
    pub channel: String,
    /// `lock=`, `none` when unlocked.
    pub lock: String,
    /// Signal strength, 0–100.
    pub ss: u8,
    /// Signal-to-noise quality, 0–100.
    pub snq: u8,
    /// Symbol error quality, 0–100 (100: no uncorrectable errors).
    pub seq: u8,
    /// Raw channel bit rate.
    pub bps: u64,
}

/// A reply that does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unexpected probe reply: {0:?}")]
pub struct ParseError(pub String);

/// Parses `get /tunerN/status`.
pub fn parse_status(s: &str) -> Result<Status, ParseError> {
    let mut st = Status::default();
    let mut seen = 0;
    for kv in s.split_whitespace() {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let n = || v.parse::<u64>().map_err(|_| ParseError(s.to_owned()));
        match k {
            "ch" => st.channel = v.to_owned(),
            "lock" => {
                st.lock = v.to_owned();
                seen += 1;
            }
            "ss" => st.ss = n()?.min(100) as u8,
            "snq" => st.snq = n()?.min(100) as u8,
            "seq" => st.seq = n()?.min(100) as u8,
            "bps" => st.bps = n()?,
            _ => {}
        }
    }
    if seen == 0 {
        return Err(ParseError(s.to_owned()));
    }
    Ok(st)
}

/// One program as the tuner saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenProgram {
    /// Program number.
    pub number: u16,
    /// `major.minor`, if the VCT gave one.
    pub channel: Option<(u16, u16)>,
    /// Name.
    pub name: String,
    /// Flagged `(encrypted)`, `(no data)` or `(control)`.
    pub flags: Vec<String>,
}

/// Parses `get /tunerN/streaminfo`: programs and the TSID.
pub fn parse_streaminfo(s: &str) -> (Vec<SeenProgram>, Option<u16>) {
    let mut progs = Vec::new();
    let mut tsid = None;
    for line in s.lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("tsid=") {
            tsid = u16::from_str_radix(v.trim_start_matches("0x"), 16).ok();
            continue;
        }
        let Some((num, rest)) = line.split_once(':') else {
            continue;
        };
        let Ok(number) = num.trim().parse::<u16>() else {
            continue;
        };
        let mut rest = rest.trim().to_owned();
        let mut flags = Vec::new();
        while let Some(open) = rest.rfind('(') {
            if !rest.ends_with(')') {
                break;
            }
            flags.insert(0, rest[open + 1..rest.len() - 1].to_owned());
            rest = rest[..open].trim_end().to_owned();
        }
        let (channel, name) = match rest.split_once(' ') {
            Some((ch, name)) => match ch.split_once('.') {
                Some((a, b)) => match (a.parse(), b.parse()) {
                    (Ok(a), Ok(b)) => (Some((a, b)), name.trim().to_owned()),
                    _ => (None, rest.clone()),
                },
                None => (None, rest.clone()),
            },
            None => (None, rest.clone()),
        };
        progs.push(SeenProgram {
            number,
            channel,
            name,
            flags,
        });
    }
    (progs, tsid)
}

/// What we send on a carrier: TSID and programs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// transport_stream_id.
    pub tsid: u16,
    /// (program number, major, minor, short name).
    pub programs: Vec<(u16, u16, u16, String)>,
}

/// The verdict on one carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Locked at 256-QAM.
    pub locked: bool,
    /// No uncorrectable errors (`seq` 100).
    pub clean: bool,
    /// Every expected program seen with its number and name, and the TSID matches.
    pub lineup_ok: bool,
    /// What did not match, for the log.
    pub problems: Vec<String>,
}

/// Judges a carrier from its status and stream info against what we send.
pub fn judge(
    status: &Status,
    programs: &[SeenProgram],
    tsid: Option<u16>,
    want: &Expected,
) -> Verdict {
    let mut problems = Vec::new();
    let locked = status.lock.starts_with("qam256");
    if !locked {
        problems.push(format!("lock={}", status.lock));
    }
    let clean = status.seq >= 100;
    if !clean {
        problems.push(format!("seq={}", status.seq));
    }
    if tsid != Some(want.tsid) {
        problems.push(format!("tsid {tsid:?}, want {:#06x}", want.tsid));
    }
    for (n, maj, min, name) in &want.programs {
        match programs.iter().find(|p| p.number == *n) {
            None => problems.push(format!("program {n} missing")),
            Some(p) => {
                if p.channel != Some((*maj, *min)) {
                    problems.push(format!(
                        "program {n}: channel {:?}, want {maj}.{min}",
                        p.channel
                    ));
                }
                if &p.name != name {
                    problems.push(format!("program {n}: name {:?}, want {name:?}", p.name));
                }
                if !p.flags.is_empty() {
                    problems.push(format!("program {n}: {:?}", p.flags));
                }
            }
        }
    }
    let lineup_ok = !problems
        .iter()
        .any(|p| p.starts_with("program") || p.starts_with("tsid"));
    Verdict {
        locked,
        clean,
        lineup_ok,
        problems,
    }
}

/// A carrier to visit: its id and center frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop {
    /// Carrier id.
    pub carrier: u8,
    /// Center frequency, Hz.
    pub hz: u32,
}

/// One tuner rotating over the carriers: each visit tunes, waits `settle`, reads, and moves on;
/// a full rotation then waits `pause`.
#[derive(Debug, Clone)]
pub struct Rotation {
    stops: Vec<Stop>,
    next: usize,
    /// Wait after tuning before reading.
    pub settle: Duration,
    /// Wait after a full rotation.
    pub pause: Duration,
}

impl Rotation {
    /// A rotation over `stops` in order.
    pub fn new(stops: Vec<Stop>, settle: Duration, pause: Duration) -> Self {
        Rotation {
            stops,
            next: 0,
            settle,
            pause,
        }
    }

    /// The next carrier, and how long to wait before tuning it (the pause after a rotation).
    pub fn next_stop(&mut self) -> Option<(Stop, Duration)> {
        let s = *self.stops.get(self.next)?;
        let wait = if self.next == 0 {
            self.pause
        } else {
            Duration::ZERO
        };
        self.next = (self.next + 1) % self.stops.len();
        Some((s, wait))
    }
}

/// US cable (STD plan) center frequency of channel `ch`, Hz: CTA-542-D Table 1 (checked against
/// the PDF in `docs/external/standards/2026-11-24-archive-org/`: ch 100 at 651, 140 at 891, 158 at 999 MHz)
/// and, where they overlap,
/// the frequencies `docs/notes/device.md` §4 records for the box's carriers.
pub fn std_frequency(ch: u16) -> Option<u32> {
    let mhz: f64 = match ch {
        2..=4 => 57.0 + 6.0 * f64::from(ch - 2),
        5..=6 => 79.0 + 6.0 * f64::from(ch - 5),
        7..=13 => 177.0 + 6.0 * f64::from(ch - 7),
        14..=22 => 123.0 + 6.0 * f64::from(ch - 14),
        23..=94 => 219.0 + 6.0 * f64::from(ch - 23),
        95..=99 => 93.0 + 6.0 * f64::from(ch - 95),
        100..=158 => 651.0 + 6.0 * f64::from(ch - 100),
        _ => return None,
    };
    Some((mhz * 1e6) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line() {
        let s =
            parse_status("ch=qam256:57000000 lock=qam256 ss=83 snq=90 seq=100 bps=38807712 pps=0")
                .unwrap();
        assert_eq!(
            (s.lock.as_str(), s.ss, s.snq, s.seq, s.bps),
            ("qam256", 83, 90, 100, 38_807_712)
        );
        let s = parse_status("ch=none lock=none ss=0 snq=0 seq=0 bps=0 pps=0").unwrap();
        assert_eq!(s.lock, "none");
        assert!(parse_status("ERROR: unknown getset variable").is_err());
    }

    #[test]
    fn streaminfo_and_judgment() {
        let text = "1: 50.1 TB1\n2: 50.2 TB2 (no data)\ntsid=0x0101\n";
        let (p, tsid) = parse_streaminfo(text);
        assert_eq!(tsid, Some(0x0101));
        assert_eq!(
            p[0],
            SeenProgram {
                number: 1,
                channel: Some((50, 1)),
                name: "TB1".into(),
                flags: vec![]
            }
        );
        assert_eq!(p[1].flags, vec!["no data".to_owned()]);
        let want = Expected {
            tsid: 0x0101,
            programs: vec![(1, 50, 1, "TB1".into()), (2, 50, 2, "TB2".into())],
        };
        let st =
            parse_status("ch=qam256:57000000 lock=qam256 ss=83 snq=90 seq=100 bps=38807712 pps=0")
                .unwrap();
        let v = judge(&st, &p, tsid, &want);
        assert!(v.locked && v.clean && !v.lineup_ok, "{v:?}");
        let (p, tsid) = parse_streaminfo("1: 50.1 TB1\n2: 50.2 TB2\ntsid=0x0101\n");
        assert!(judge(&st, &p, tsid, &want).lineup_ok);
    }

    #[test]
    fn frequencies_and_rotation() {
        // The box's carriers today: channels 2–5, 10–13, 6/95/96/97, 140–143 (device §4).
        assert_eq!(std_frequency(2), Some(57_000_000));
        assert_eq!(std_frequency(5), Some(79_000_000));
        assert_eq!(std_frequency(6), Some(85_000_000));
        assert_eq!(std_frequency(95), Some(93_000_000));
        assert_eq!(std_frequency(13), Some(213_000_000));
        assert_eq!(std_frequency(140), Some(891_000_000));
        let mut r = Rotation::new(
            vec![
                Stop {
                    carrier: 1,
                    hz: 57_000_000,
                },
                Stop {
                    carrier: 2,
                    hz: 63_000_000,
                },
            ],
            Duration::from_secs(2),
            Duration::from_secs(60),
        );
        assert_eq!(
            r.next_stop().map(|(s, w)| (s.carrier, w.as_secs())),
            Some((1, 60))
        );
        assert_eq!(
            r.next_stop().map(|(s, w)| (s.carrier, w.as_secs())),
            Some((2, 0))
        );
        assert_eq!(r.next_stop().map(|(s, _)| s.carrier), Some(1));
    }
}
