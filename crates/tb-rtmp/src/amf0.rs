//! AMF0 (Adobe "Action Message Format -- AMF 0", 2007), encode and decode.
//!
//! Section numbers below are the AMF0 specification's. Decoding is bounded: nesting depth and
//! the total number of values are limited, and no length read from the input is used to
//! allocate before the bytes it describes are known to be there. Invalid UTF-8 in strings is
//! replaced (U+FFFD) rather than refused: names and keys are only compared, never trusted.
//!
//! The AMF3 switch marker (`avmplus-object-marker`, §3.1, 0x11) is refused with
//! [`AmfError::Amf3`]: RTMP publishers (ffmpeg, OBS, librtmp, GStreamer) send AMF0 commands.

/// An AMF0 value.
#[derive(Debug, Clone, PartialEq)]
pub enum Amf0Value {
    /// number-type (§2.2): an IEEE 754 double.
    Number(f64),
    /// boolean-type (§2.3).
    Boolean(bool),
    /// string-type (§2.4) or long-string-type (§2.14).
    String(String),
    /// anonymous object (§2.5): key/value pairs in order.
    Object(Vec<(String, Amf0Value)>),
    /// null-type (§2.7).
    Null,
    /// undefined-type (§2.8).
    Undefined,
    /// reference-type (§2.9): an index into the objects already read. Not resolved.
    Reference(u16),
    /// ecma-array-type (§2.10): an associative array.
    EcmaArray(Vec<(String, Amf0Value)>),
    /// strict-array-type (§2.12).
    StrictArray(Vec<Amf0Value>),
    /// date-type (§2.13): milliseconds since the epoch, and a time zone that should be 0.
    Date {
        /// Milliseconds since 1970-01-01 UTC.
        millis: f64,
        /// Reserved time zone field.
        time_zone: i16,
    },
    /// unsupported-type (§2.15).
    Unsupported,
    /// xml-document-type (§2.17).
    XmlDocument(String),
    /// typed-object-type (§2.18): a class name and properties.
    TypedObject {
        /// The class name.
        class: String,
        /// Properties.
        properties: Vec<(String, Amf0Value)>,
    },
}

impl Amf0Value {
    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
    /// The number, if this is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }
    /// The properties of an object, ECMA array or typed object.
    pub fn properties(&self) -> Option<&[(String, Amf0Value)]> {
        match self {
            Self::Object(p) | Self::EcmaArray(p) | Self::TypedObject { properties: p, .. } => {
                Some(p)
            }
            _ => None,
        }
    }
    /// A property of an object, ECMA array or typed object, by key (the first match).
    pub fn get(&self, key: &str) -> Option<&Amf0Value> {
        self.properties()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
    /// Shorthand for an object built from string keys.
    pub fn object<const N: usize>(props: [(&str, Amf0Value); N]) -> Self {
        Self::Object(props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }
    /// Shorthand for a string value.
    pub fn string(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

/// A decode failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AmfError {
    /// The data ended inside a value.
    #[error("AMF0 data truncated")]
    Truncated,
    /// A marker this decoder does not know or that is reserved (movieclip, recordset).
    #[error("unknown or reserved AMF0 marker {0:#04x}")]
    Marker(u8),
    /// The AMF3 switch marker (0x11).
    #[error("AMF3 values are not supported")]
    Amf3,
    /// Nesting deeper than [`Limits::max_depth`].
    #[error("AMF0 nesting deeper than {0}")]
    TooDeep(usize),
    /// More values than [`Limits::max_values`].
    #[error("more than {0} AMF0 values")]
    TooMany(usize),
}

/// Bounds on decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Deepest nesting of objects and arrays.
    pub max_depth: usize,
    /// Most values (at any depth) in one decode.
    pub max_values: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_values: 4096,
        }
    }
}

const NUMBER: u8 = 0x00;
const BOOLEAN: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const NULL: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const REFERENCE: u8 = 0x07;
const ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const STRICT_ARRAY: u8 = 0x0A;
const DATE: u8 = 0x0B;
const LONG_STRING: u8 = 0x0C;
const UNSUPPORTED: u8 = 0x0D;
const XML_DOCUMENT: u8 = 0x0F;
const TYPED_OBJECT: u8 = 0x10;
const AVMPLUS: u8 = 0x11;

/// Decode every value in `data` (a command or data message body is a sequence of values).
pub fn decode_all(data: &[u8], limits: Limits) -> Result<Vec<Amf0Value>, AmfError> {
    let mut d = Decoder {
        data,
        pos: 0,
        limits,
        values: 0,
    };
    let mut out = Vec::new();
    while d.pos < data.len() {
        out.push(d.value(0)?);
    }
    Ok(out)
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    limits: Limits,
    values: usize,
}

impl Decoder<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], AmfError> {
        let end = self.pos.checked_add(n).ok_or(AmfError::Truncated)?;
        let s = self.data.get(self.pos..end).ok_or(AmfError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, AmfError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, AmfError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, AmfError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f64(&mut self) -> Result<f64, AmfError> {
        let b = self.take(8)?;
        let mut a = [0; 8];
        a.copy_from_slice(b);
        Ok(f64::from_be_bytes(a))
    }
    fn utf8(&mut self, n: usize) -> Result<String, AmfError> {
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn short_string(&mut self) -> Result<String, AmfError> {
        let n = self.u16()?;
        self.utf8(usize::from(n))
    }
    fn long_string(&mut self) -> Result<String, AmfError> {
        let n = self.u32()?;
        self.utf8(usize::try_from(n).map_err(|_| AmfError::Truncated)?)
    }

    fn value(&mut self, depth: usize) -> Result<Amf0Value, AmfError> {
        self.values += 1;
        if self.values > self.limits.max_values {
            return Err(AmfError::TooMany(self.limits.max_values));
        }
        let m = self.u8()?;
        Ok(match m {
            NUMBER => Amf0Value::Number(self.f64()?),
            BOOLEAN => Amf0Value::Boolean(self.u8()? != 0),
            STRING => Amf0Value::String(self.short_string()?),
            OBJECT => Amf0Value::Object(self.properties(depth, false)?),
            NULL => Amf0Value::Null,
            UNDEFINED => Amf0Value::Undefined,
            REFERENCE => Amf0Value::Reference(self.u16()?),
            ECMA_ARRAY => {
                // The count is advisory (§2.10); the end marker terminates.
                let _count = self.u32()?;
                Amf0Value::EcmaArray(self.properties(depth, true)?)
            }
            STRICT_ARRAY => {
                let n = self.u32()?;
                self.enter(depth)?;
                let mut v = Vec::new();
                for _ in 0..n {
                    // Each value is at least one byte, so truncation stops a huge count.
                    v.push(self.value(depth + 1)?);
                }
                Amf0Value::StrictArray(v)
            }
            DATE => {
                let millis = self.f64()?;
                let time_zone = self.u16()?.cast_signed();
                Amf0Value::Date { millis, time_zone }
            }
            LONG_STRING => Amf0Value::String(self.long_string()?),
            UNSUPPORTED => Amf0Value::Unsupported,
            XML_DOCUMENT => Amf0Value::XmlDocument(self.long_string()?),
            TYPED_OBJECT => {
                let class = self.short_string()?;
                Amf0Value::TypedObject {
                    class,
                    properties: self.properties(depth, false)?,
                }
            }
            AVMPLUS => return Err(AmfError::Amf3),
            other => return Err(AmfError::Marker(other)),
        })
    }

    fn enter(&self, depth: usize) -> Result<(), AmfError> {
        if depth + 1 > self.limits.max_depth {
            return Err(AmfError::TooDeep(self.limits.max_depth));
        }
        Ok(())
    }

    /// Key/value pairs up to the empty key and object-end marker (§2.5, §2.11).
    fn properties(
        &mut self,
        depth: usize,
        lenient_end: bool,
    ) -> Result<Vec<(String, Amf0Value)>, AmfError> {
        self.enter(depth)?;
        let mut v = Vec::new();
        loop {
            // Some writers end an ECMA array with the data instead of the end marker.
            if lenient_end && self.pos == self.data.len() {
                return Ok(v);
            }
            let key = self.short_string()?;
            if key.is_empty() && self.data.get(self.pos) == Some(&OBJECT_END) {
                self.pos += 1;
                return Ok(v);
            }
            let val = self.value(depth + 1)?;
            v.push((key, val));
        }
    }
}

/// Append the AMF0 encoding of `v` to `out`. Strings longer than 65535 bytes are written as
/// long strings.
pub fn encode(v: &Amf0Value, out: &mut Vec<u8>) {
    match v {
        Amf0Value::Number(n) => {
            out.push(NUMBER);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Amf0Value::Boolean(b) => {
            out.push(BOOLEAN);
            out.push(u8::from(*b));
        }
        Amf0Value::String(s) => match u16::try_from(s.len()) {
            Ok(n) => {
                out.push(STRING);
                out.extend_from_slice(&n.to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            Err(_) => {
                out.push(LONG_STRING);
                long_bytes(s.as_bytes(), out);
            }
        },
        Amf0Value::Object(p) => {
            out.push(OBJECT);
            encode_properties(p, out);
        }
        Amf0Value::Null => out.push(NULL),
        Amf0Value::Undefined => out.push(UNDEFINED),
        Amf0Value::Reference(r) => {
            out.push(REFERENCE);
            out.extend_from_slice(&r.to_be_bytes());
        }
        Amf0Value::EcmaArray(p) => {
            out.push(ECMA_ARRAY);
            out.extend_from_slice(&u32::try_from(p.len()).unwrap_or(u32::MAX).to_be_bytes());
            encode_properties(p, out);
        }
        Amf0Value::StrictArray(a) => {
            out.push(STRICT_ARRAY);
            out.extend_from_slice(&u32::try_from(a.len()).unwrap_or(u32::MAX).to_be_bytes());
            for x in a {
                encode(x, out);
            }
        }
        Amf0Value::Date { millis, time_zone } => {
            out.push(DATE);
            out.extend_from_slice(&millis.to_be_bytes());
            out.extend_from_slice(&time_zone.to_be_bytes());
        }
        Amf0Value::Unsupported => out.push(UNSUPPORTED),
        Amf0Value::XmlDocument(s) => {
            out.push(XML_DOCUMENT);
            long_bytes(s.as_bytes(), out);
        }
        Amf0Value::TypedObject { class, properties } => {
            out.push(TYPED_OBJECT);
            short_bytes(class.as_bytes(), out);
            encode_properties(properties, out);
        }
    }
}

fn short_bytes(b: &[u8], out: &mut Vec<u8>) {
    let b = &b[..b.len().min(usize::from(u16::MAX))];
    out.extend_from_slice(&(b.len() as u16).to_be_bytes());
    out.extend_from_slice(b);
}

fn long_bytes(b: &[u8], out: &mut Vec<u8>) {
    let b = &b[..b.len().min(u32::MAX as usize)];
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

fn encode_properties(p: &[(String, Amf0Value)], out: &mut Vec<u8>) {
    for (k, v) in p {
        short_bytes(k.as_bytes(), out);
        encode(v, out);
    }
    out.extend_from_slice(&[0, 0, OBJECT_END]);
}

/// Encode a sequence of values.
pub fn encode_all(values: &[Amf0Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        encode(v, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let v = vec![
            Amf0Value::string("connect"),
            Amf0Value::Number(1.0),
            Amf0Value::object([
                ("app", Amf0Value::string("live")),
                ("fpad", Amf0Value::Boolean(false)),
                ("n", Amf0Value::Null),
                (
                    "arr",
                    Amf0Value::StrictArray(vec![Amf0Value::Undefined, Amf0Value::Number(2.5)]),
                ),
            ]),
            Amf0Value::EcmaArray(vec![("width".into(), Amf0Value::Number(640.0))]),
            Amf0Value::Date {
                millis: 1.0e12,
                time_zone: 0,
            },
            Amf0Value::String("x".repeat(70_000)),
            Amf0Value::TypedObject {
                class: "C".into(),
                properties: vec![],
            },
            Amf0Value::Reference(3),
            Amf0Value::XmlDocument("<a/>".into()),
            Amf0Value::Unsupported,
        ];
        let b = encode_all(&v);
        assert_eq!(decode_all(&b, Limits::default()).unwrap(), v);
        assert_eq!(v[2].get("app").and_then(Amf0Value::as_str), Some("live"));
    }

    /// The AMF0 specification's own example bytes for a string (§2.4) and a number (§2.2).
    #[test]
    fn known_bytes() {
        let b = encode_all(&[Amf0Value::string("_result"), Amf0Value::Number(1.0)]);
        assert_eq!(
            b,
            [
                2, 0, 7, b'_', b'r', b'e', b's', b'u', b'l', b't', 0, 0x3F, 0xF0, 0, 0, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn bounded() {
        // Deep nesting.
        let mut b = Vec::new();
        for _ in 0..100 {
            b.extend_from_slice(&[OBJECT, 0, 1, b'a']);
        }
        assert_eq!(
            decode_all(&b, Limits::default()),
            Err(AmfError::TooDeep(16))
        );
        // A strict array claiming 4 billion elements, with none present.
        assert_eq!(
            decode_all(&[STRICT_ARRAY, 0xFF, 0xFF, 0xFF, 0xFF], Limits::default()),
            Err(AmfError::Truncated)
        );
        // A long string claiming 4 GiB.
        assert_eq!(
            decode_all(
                &[LONG_STRING, 0xFF, 0xFF, 0xFF, 0xFF, b'a'],
                Limits::default()
            ),
            Err(AmfError::Truncated)
        );
        // Many values.
        let b = vec![NULL; 5000];
        assert_eq!(
            decode_all(&b, Limits::default()),
            Err(AmfError::TooMany(4096))
        );
        assert_eq!(
            decode_all(&[AVMPLUS, 0], Limits::default()),
            Err(AmfError::Amf3)
        );
        assert_eq!(
            decode_all(&[0x04], Limits::default()),
            Err(AmfError::Marker(4))
        );
        // ECMA array with no end marker at the end of the data.
        let b = [ECMA_ARRAY, 0, 0, 0, 1, 0, 1, b'k', NULL];
        assert_eq!(
            decode_all(&b, Limits::default()).unwrap(),
            vec![Amf0Value::EcmaArray(vec![("k".into(), Amf0Value::Null)])]
        );
    }
}
