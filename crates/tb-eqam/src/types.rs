//! Value types that can only hold what the device accepts (SPEC §10.1, docs/notes/device.md §3,
//! docs/notes/manual.md §4–§5, docs/notes/corruption-48.md §2 "Triggers and damage").

use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::time::Duration;

/// A value outside the range a type allows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValueError {
    /// A number outside its range.
    #[error("{what} {value} is outside {min}..={max}")]
    OutOfRange {
        /// The kind of value.
        what: &'static str,
        /// The value given.
        value: i64,
        /// Lowest allowed.
        min: i64,
        /// Highest allowed.
        max: i64,
    },
    /// Text that does not have the required form.
    #[error("{what}: {reason}: {text:?}")]
    Invalid {
        /// The kind of value.
        what: &'static str,
        /// Why it was rejected.
        reason: &'static str,
        /// The text given (never a credential).
        text: String,
    },
}

fn range(what: &'static str, value: i64, min: i64, max: i64) -> Result<(), ValueError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ValueError::OutOfRange {
            what,
            value,
            min,
            max,
        })
    }
}

/// An MPTS input slot, 1..=16. Slot *n* feeds QAM carrier *n*; slots 1–8 arrive on GIGE1 and
/// 9–16 on GIGE2 (device §3, "statusedit.cgi").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot(u8);

impl Slot {
    /// Number of slots in MPTS mode.
    pub const COUNT: usize = 16;

    /// Slot `n` (1-based).
    pub fn new(n: u8) -> Result<Self, ValueError> {
        range("input slot", n.into(), 1, 16)?;
        Ok(Slot(n))
    }

    /// All 16 slots in order.
    pub fn all() -> impl Iterator<Item = Slot> {
        (1..=16).map(Slot)
    }

    /// The 1-based number.
    pub fn get(self) -> u8 {
        self.0
    }

    /// Zero-based index.
    pub fn index(self) -> usize {
        usize::from(self.0 - 1)
    }

    /// The RF module whose carrier this slot feeds.
    pub fn module(self) -> RfModule {
        RfModule((self.0 - 1) / 4 + 1)
    }

    /// The carrier position (A–D) within its module.
    pub fn carrier(self) -> CarrierLetter {
        CarrierLetter::ALL[usize::from((self.0 - 1) % 4)]
    }

    /// The GigE port the stream arrives on: 1 for slots 1–8, 2 for 9–16.
    pub fn gige(self) -> u8 {
        if self.0 <= 8 { 1 } else { 2 }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An RF module (one output port with four adjacent carriers), 1..=4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RfModule(u8);

impl RfModule {
    /// Module `n` (1-based).
    pub fn new(n: u8) -> Result<Self, ValueError> {
        range("RF module", n.into(), 1, 4)?;
        Ok(RfModule(n))
    }

    /// All four modules in order — the only order in which the QAM plan is ever saved.
    pub fn all() -> [RfModule; 4] {
        [RfModule(1), RfModule(2), RfModule(3), RfModule(4)]
    }

    /// The 1-based number.
    pub fn get(self) -> u8 {
        self.0
    }

    /// Zero-based index.
    pub fn index(self) -> usize {
        usize::from(self.0 - 1)
    }

    /// The four slots (carriers A–D) of this module.
    pub fn slots(self) -> [Slot; 4] {
        let b = (self.0 - 1) * 4;
        [Slot(b + 1), Slot(b + 2), Slot(b + 3), Slot(b + 4)]
    }
}

impl fmt::Display for RfModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Carrier position within an RF module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarrierLetter {
    /// First carrier (the one whose channel is chosen).
    A,
    /// Second.
    B,
    /// Third.
    C,
    /// Fourth.
    D,
}

impl CarrierLetter {
    /// A–D in order.
    pub const ALL: [CarrierLetter; 4] = [
        CarrierLetter::A,
        CarrierLetter::B,
        CarrierLetter::C,
        CarrierLetter::D,
    ];

    /// The letter as used in form field names.
    pub fn as_str(self) -> &'static str {
        match self {
            CarrierLetter::A => "A",
            CarrierLetter::B => "B",
            CarrierLetter::C => "C",
            CarrierLetter::D => "D",
        }
    }
}

/// A stream name: 1–10 ASCII letters or digits (manual §4.2 "≤10 alphanumeric"; SPEC §10.1).
/// Never empty (defect B turns an empty name into `0`) and never containing `,` `-` `.`
/// (defect B's `strtok` splits on them) — both excluded because only `[A-Za-z0-9]` is accepted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamName(String);

impl StreamName {
    /// Longest name the form accepts (`maxlength="10"`).
    pub const MAX_LEN: usize = 10;

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for StreamName {
    type Err = ValueError;
    fn from_str(s: &str) -> Result<Self, ValueError> {
        let bad = |reason| ValueError::Invalid {
            what: "stream name",
            reason,
            text: s.to_owned(),
        };
        if s.is_empty() {
            return Err(bad("empty"));
        }
        if s.len() > Self::MAX_LEN {
            return Err(bad("longer than 10 characters"));
        }
        if !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(bad("only ASCII letters and digits are allowed"));
        }
        Ok(StreamName(s.to_owned()))
    }
}

impl fmt::Display for StreamName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Transport of an input stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Proto {
    /// `UDP://`
    Udp,
    /// `RTP://`
    Rtp,
}

/// A stream URL as the device prints and parses it: `UDP://a.b.c.d:port` or
/// `RTP://a.b.c.d:port` (manual §4.1; `mux_sync` strings, device §11.4). Address `0.0.0.0` and
/// port 0 are refused: the device uses them for "unset". At most 27 characters (the form's
/// `maxlength`), which every address and port fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamUrl {
    proto: Proto,
    addr: Ipv4Addr,
    port: u16,
}

impl StreamUrl {
    /// Build from parts.
    pub fn new(proto: Proto, addr: Ipv4Addr, port: u16) -> Result<Self, ValueError> {
        if addr.is_unspecified() {
            return Err(ValueError::Invalid {
                what: "stream URL",
                reason: "address 0.0.0.0 means unset",
                text: addr.to_string(),
            });
        }
        range("stream port", port.into(), 1, 65535)?;
        Ok(StreamUrl { proto, addr, port })
    }

    /// Transport.
    pub fn proto(&self) -> Proto {
        self.proto
    }

    /// Destination (group or unicast) address.
    pub fn addr(&self) -> Ipv4Addr {
        self.addr
    }

    /// UDP port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl FromStr for StreamUrl {
    type Err = ValueError;
    /// Parses exactly the device's form: upper-case scheme, dotted quad, decimal port.
    fn from_str(s: &str) -> Result<Self, ValueError> {
        let bad = |reason| ValueError::Invalid {
            what: "stream URL",
            reason,
            text: s.to_owned(),
        };
        let (proto, rest) = if let Some(r) = s.strip_prefix("UDP://") {
            (Proto::Udp, r)
        } else if let Some(r) = s.strip_prefix("RTP://") {
            (Proto::Rtp, r)
        } else {
            return Err(bad("must start with UDP:// or RTP://"));
        };
        let (a, p) = rest.rsplit_once(':').ok_or_else(|| bad("missing :port"))?;
        if a.split('.').count() != 4
            || !a
                .split('.')
                .all(|o| !o.is_empty() && o.len() <= 3 && o.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(bad("address must be a dotted quad"));
        }
        let addr: Ipv4Addr = a
            .parse()
            .map_err(|_| bad("address must be a dotted quad"))?;
        if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("port must be decimal"));
        }
        let port: u32 = p.parse().map_err(|_| bad("port must be decimal"))?;
        let port = u16::try_from(port).map_err(|_| bad("port above 65535"))?;
        StreamUrl::new(proto, addr, port)
    }
}

impl fmt::Display for StreamUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = match self.proto {
            Proto::Udp => "UDP",
            Proto::Rtp => "RTP",
        };
        write!(f, "{scheme}://{}:{}", self.addr, self.port)
    }
}

/// A cable channel number on the device's list, 2..=158 (device §3, "qamconfig.cgi").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u8);

impl Channel {
    /// Channel `n`.
    pub fn new(n: u8) -> Result<Self, ValueError> {
        range("cable channel", n.into(), 2, 158)?;
        Ok(Channel(n))
    }

    /// The number.
    pub fn get(self) -> u8 {
        self.0
    }

    /// The next channel up in frequency, from the device's `cable_channel_table_after`
    /// (6 → 95, 94 → 100, 13 → 23, 22 → 7, 99 → 14); `None` after 158.
    pub fn next_up(self) -> Option<Channel> {
        let n = crate::channels::CABLE_CHANNEL_TABLE_AFTER[usize::from(self.0 - 2)];
        if n == 0 { None } else { Some(Channel(n)) }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The channel of carrier A of a module: one of the values the `outputChannelA` select offers,
/// 2..=155, so that carriers B–D (the next three channels up) exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FirstChannel(Channel);

impl FirstChannel {
    /// Carrier A on channel `n`.
    pub fn new(n: u8) -> Result<Self, ValueError> {
        range("first channel", n.into(), 2, 155)?;
        Ok(FirstChannel(Channel(n)))
    }

    /// The channel.
    pub fn channel(self) -> Channel {
        self.0
    }

    /// The four channels of carriers A–D. Every first channel in range has three successors.
    pub fn carriers(self) -> [Channel; 4] {
        let a = self.0;
        let b = a.next_up().unwrap_or(a);
        let c = b.next_up().unwrap_or(b);
        let d = c.next_up().unwrap_or(c);
        [a, b, c, d]
    }
}

/// QAM mode (`QAMModeA` option values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QamMode {
    /// 64-QAM, J.83 Annex B (value 0).
    Qam64B,
    /// 256-QAM, J.83 Annex B (value 1).
    Qam256B,
    /// 16-QAM Annex A (value 2).
    Qam16A,
    /// 32-QAM Annex A (value 3).
    Qam32A,
    /// 64-QAM Annex A (value 4).
    Qam64A,
    /// 128-QAM Annex A (value 5).
    Qam128A,
    /// 256-QAM Annex A (value 6).
    Qam256A,
}

impl QamMode {
    /// The form value.
    pub fn form_value(self) -> u8 {
        match self {
            QamMode::Qam64B => 0,
            QamMode::Qam256B => 1,
            QamMode::Qam16A => 2,
            QamMode::Qam32A => 3,
            QamMode::Qam64A => 4,
            QamMode::Qam128A => 5,
            QamMode::Qam256A => 6,
        }
    }

    /// From the form value.
    pub fn from_form_value(v: u8) -> Result<Self, ValueError> {
        Ok(match v {
            0 => QamMode::Qam64B,
            1 => QamMode::Qam256B,
            2 => QamMode::Qam16A,
            3 => QamMode::Qam32A,
            4 => QamMode::Qam64A,
            5 => QamMode::Qam128A,
            6 => QamMode::Qam256A,
            _ => {
                return Err(ValueError::OutOfRange {
                    what: "QAM mode",
                    value: v.into(),
                    min: 0,
                    max: 6,
                });
            }
        })
    }
}

impl FromStr for QamMode {
    type Err = ValueError;
    /// The labels the page shows and the configuration uses: `64B`, `256B`, `16A` … `256A`.
    fn from_str(s: &str) -> Result<Self, ValueError> {
        Ok(match s {
            "64B" => QamMode::Qam64B,
            "256B" => QamMode::Qam256B,
            "16A" => QamMode::Qam16A,
            "32A" => QamMode::Qam32A,
            "64A" => QamMode::Qam64A,
            "128A" => QamMode::Qam128A,
            "256A" => QamMode::Qam256A,
            _ => {
                return Err(ValueError::Invalid {
                    what: "QAM mode",
                    reason: "expected 64B, 256B, 16A, 32A, 64A, 128A or 256A",
                    text: s.to_owned(),
                });
            }
        })
    }
}

/// Channel plan (`QAMMap` option values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QamMap {
    /// Standard (value 10).
    Std,
    /// Incrementally related carriers (value 11).
    Irc,
    /// Harmonically related carriers (value 12).
    Hrc,
}

impl QamMap {
    /// The form value.
    pub fn form_value(self) -> u8 {
        match self {
            QamMap::Std => 10,
            QamMap::Irc => 11,
            QamMap::Hrc => 12,
        }
    }

    /// From the form value.
    pub fn from_form_value(v: u8) -> Result<Self, ValueError> {
        Ok(match v {
            10 => QamMap::Std,
            11 => QamMap::Irc,
            12 => QamMap::Hrc,
            _ => {
                return Err(ValueError::OutOfRange {
                    what: "QAM map",
                    value: v.into(),
                    min: 10,
                    max: 12,
                });
            }
        })
    }
}

impl FromStr for QamMap {
    type Err = ValueError;
    /// `STD`, `IRC` or `HRC`.
    fn from_str(s: &str) -> Result<Self, ValueError> {
        Ok(match s {
            "STD" => QamMap::Std,
            "IRC" => QamMap::Irc,
            "HRC" => QamMap::Hrc,
            _ => {
                return Err(ValueError::Invalid {
                    what: "QAM map",
                    reason: "expected STD, IRC or HRC",
                    text: s.to_owned(),
                });
            }
        })
    }
}

/// Final output level in dBmV, 28..=41. One value for the whole unit; only module 1's page
/// can change it (device §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputLevel(u8);

impl OutputLevel {
    /// Level `dbmv`.
    pub fn new(dbmv: u8) -> Result<Self, ValueError> {
        range("output level", dbmv.into(), 28, 41)?;
        Ok(OutputLevel(dbmv))
    }

    /// dBmV.
    pub fn get(self) -> u8 {
        self.0
    }
}

/// Transport-stream mode from the page header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TsMode {
    /// Single Program Transport Stream mode (64 inputs). Never written by this crate.
    Spts,
    /// Multiple Program Transport Stream mode (16 inputs): the only mode this crate writes in.
    Mpts,
}

/// A web session id: a decimal number from `login.cgi` (a random 31-bit value; device §2).
/// Its `Debug` output is redacted because it authenticates as Admin.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SessionId(u32);

impl SessionId {
    /// Parse the decimal digits the device prints.
    pub fn parse(s: &str) -> Result<Self, ValueError> {
        let s = s.trim();
        if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ValueError::Invalid {
                what: "session id",
                reason: "expected decimal digits",
                text: String::from("<redacted>"),
            });
        }
        let v: u64 = s.parse().map_err(|_| ValueError::Invalid {
            what: "session id",
            reason: "expected decimal digits",
            text: String::from("<redacted>"),
        })?;
        let v = u32::try_from(v).map_err(|_| ValueError::Invalid {
            what: "session id",
            reason: "above 2^32",
            text: String::from("<redacted>"),
        })?;
        if v == 0 {
            // `0` is the "unused" marker in /tmp/user_info.dat.
            return Err(ValueError::Invalid {
                what: "session id",
                reason: "0 is the unused marker",
                text: String::from("0"),
            });
        }
        Ok(SessionId(v))
    }

    /// The decimal text sent to the device.
    pub(crate) fn wire(&self) -> String {
        self.0.to_string()
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionId(<redacted>)")
    }
}

/// Web credentials, passed in at run time (never from the configuration file; SPEC §10.1).
/// Both parts are 1–10 printable ASCII characters (`maxlength="10"` on the login form).
/// `Debug` redacts the password.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    username: String,
    password: String,
}

impl Credentials {
    /// Validate and hold a user name and password.
    pub fn new(username: &str, password: &str) -> Result<Self, ValueError> {
        let check = |what: &'static str, s: &str, show: bool| {
            if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                Err(ValueError::Invalid {
                    what,
                    reason: "1-10 printable ASCII characters without spaces",
                    text: if show {
                        s.to_owned()
                    } else {
                        String::from("<redacted>")
                    },
                })
            } else {
                Ok(())
            }
        };
        check("user name", username, true)?;
        check("password", password, false)?;
        Ok(Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
        })
    }

    /// The user name.
    pub fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Device uptime as printed in page headers: `6d 17h 50m 6s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Uptime(pub Duration);

impl FromStr for Uptime {
    type Err = ValueError;
    fn from_str(s: &str) -> Result<Self, ValueError> {
        let bad = || ValueError::Invalid {
            what: "uptime",
            reason: "expected `<d>d <h>h <m>m <s>s`",
            text: s.to_owned(),
        };
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.len() != 4 {
            return Err(bad());
        }
        let mut total: u64 = 0;
        for (p, (unit, mult)) in
            parts
                .iter()
                .zip([('d', 86_400u64), ('h', 3600), ('m', 60), ('s', 1)])
        {
            let n = p.strip_suffix(unit).ok_or_else(bad)?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            let n: u64 = n.parse().map_err(|_| bad())?;
            total = total
                .checked_add(n.checked_mul(mult).ok_or_else(bad)?)
                .ok_or_else(bad)?;
        }
        Ok(Uptime(Duration::from_secs(total)))
    }
}

/// Chassis temperature in tenths of a degree Fahrenheit (printed as `%4.1f` then `ºF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TemperatureF {
    /// Tenths of a degree Fahrenheit.
    pub tenths: i32,
}

/// A bit rate in bits per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Bitrate(pub u64);

/// Parse a decimal with an optional fraction (`8.4`) into an integer number of `unit`s,
/// exactly (no floating point): `parse_decimal("8.4", 1_000_000) == Some(8_400_000)`.
pub(crate) fn parse_decimal(s: &str, unit: u64) -> Option<u64> {
    let s = s.trim();
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v = int.parse::<u64>().ok()?.checked_mul(unit)?;
    let mut scale = unit;
    for d in frac.bytes() {
        scale /= 10;
        if scale == 0 {
            break;
        }
        v = v.checked_add(u64::from(d - b'0') * scale)?;
    }
    Some(v)
}

/// Parse a signed decimal with one fraction digit at most into tenths.
pub(crate) fn parse_tenths(s: &str) -> Option<i32> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let v = i32::try_from(parse_decimal(body, 10)?).ok()?;
    Some(if neg { -v } else { v })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_name_rules() {
        assert!("ESPN2".parse::<StreamName>().is_ok());
        assert!("A123456789".parse::<StreamName>().is_ok());
        for bad in [
            "",
            "A1234567890",
            "ESPN-HD",
            "a,b",
            "a.b",
            "a b",
            "caf\u{e9}",
            "x_y",
        ] {
            assert!(bad.parse::<StreamName>().is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn stream_url_rules() {
        let u: StreamUrl = "UDP://239.10.0.1:5000".parse().unwrap();
        assert_eq!(u.to_string(), "UDP://239.10.0.1:5000");
        assert_eq!(u.proto(), Proto::Udp);
        let r: StreamUrl = "RTP://10.42.0.183:65535".parse().unwrap();
        assert_eq!(r.to_string().len(), 23);
        assert!("UDP://255.255.255.255:65535".parse::<StreamUrl>().is_ok());
        assert_eq!("UDP://255.255.255.255:65535".len(), 27);
        for bad in [
            "udp://1.2.3.4:5",
            "UDP://0.0.0.0:0",
            "UDP://0.0.0.0:5000",
            "UDP://1.2.3.4:0",
            "UDP://1.2.3.4:65536",
            "UDP://1.2.3:5",
            "UDP://1.2.3.4",
            "UDP://1.2.3.4:+5",
            "UDP://01234.2.3.4:5",
            "HTTP://1.2.3.4:5",
            "UDP://1.2.3.4:5 ",
            "",
        ] {
            assert!(bad.parse::<StreamUrl>().is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn slot_mapping() {
        let s = Slot::new(6).unwrap();
        assert_eq!(s.module().get(), 2);
        assert_eq!(s.carrier(), CarrierLetter::B);
        assert_eq!(s.gige(), 1);
        assert_eq!(Slot::new(9).unwrap().gige(), 2);
        assert!(Slot::new(0).is_err());
        assert!(Slot::new(17).is_err());
        assert_eq!(RfModule::new(4).unwrap().slots()[3].get(), 16);
        assert!(RfModule::new(5).is_err());
    }

    #[test]
    fn channels() {
        assert_eq!(
            FirstChannel::new(6).unwrap().carriers().map(Channel::get),
            [6, 95, 96, 97]
        );
        assert_eq!(
            FirstChannel::new(10).unwrap().carriers().map(Channel::get),
            [10, 11, 12, 13]
        );
        assert_eq!(
            FirstChannel::new(155).unwrap().carriers().map(Channel::get),
            [155, 156, 157, 158]
        );
        assert!(FirstChannel::new(156).is_err());
        assert!(FirstChannel::new(1).is_err());
        assert_eq!(Channel::new(158).unwrap().next_up(), None);
    }

    #[test]
    fn uptime_and_decimals() {
        let u: Uptime = "6d 17h 50m 6s".parse().unwrap();
        assert_eq!(u.0.as_secs(), 6 * 86400 + 17 * 3600 + 50 * 60 + 6);
        assert!("6d 17h 50m".parse::<Uptime>().is_err());
        assert!("6d 17h 50m 6x".parse::<Uptime>().is_err());
        assert_eq!(parse_decimal("8.4", 1_000_000), Some(8_400_000));
        assert_eq!(parse_decimal("12", 1_000_000), Some(12_000_000));
        assert_eq!(parse_decimal("-", 1), None);
        assert_eq!(parse_tenths("80.8"), Some(808));
        assert_eq!(parse_tenths("-3.5"), Some(-35));
    }

    #[test]
    fn secrets_are_redacted() {
        let c = Credentials::new("Admin", "hunter2").unwrap();
        assert!(!format!("{c:?}").contains("hunter2"));
        let s = SessionId::parse("987654321").unwrap();
        assert!(!format!("{s:?}").contains("987654321"));
        assert!(SessionId::parse("0").is_err());
        assert!(SessionId::parse("12a").is_err());
        assert!(Credentials::new("Admin", "has space").is_err());
        assert!(Credentials::new("Admin", "01234567890").is_err());
    }
}
