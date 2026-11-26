//! Reader (and writer, [`write`]) of the golden vectors.
//!
//! `crates/tb-golden/vectors/*.jsonl` at the repository root holds what the encoder decided and what crossed its
//! host/device boundary, one JSON object per line, as `tb-m2venc --golden` records it on the
//! reference adapter (`crates/tb-golden/vectors/generate.sh`; the first five cases and the tables were recorded by
//! the C++ encoder of the CUDA spike first, docs/ARCHITECTURE.md "History", and the Rust recorder
//! writes the same records, `crates/tb-golden/vectors/README.md`).
//! `crates/tb-golden/vectors/README.md` describes every record. This crate embeds the files, so tests need no
//! file access (they run in the Nix sandbox), and gives typed access to the fields.
//!
//! A test of a ported function typically walks the records of one kind, feeds the recorded
//! inputs to the Rust function and compares the outputs exactly:
//!
//! ```
//! for rec in tb_golden::case("cbr").unwrap().iter().filter(|r| r.kind() == "hdr") {
//!     let bytes = rec.bytes("bytes").unwrap();
//!     assert!(bytes.ends_with(&[0]) || !bytes.is_empty());
//! }
//! ```
//!
//! Numbers: integers are exact; doubles were printed with 17 significant digits and parse back
//! to the same IEEE value, so `assert_eq!` on `f64` is the right comparison.
#![forbid(unsafe_code)]

use serde_json::Value;

pub mod write;

/// The embedded cases: name and text.
const CASES: &[(&str, &str)] = &[
    ("tables", include_str!("../vectors/tables.jsonl")),
    ("cbr", include_str!("../vectors/cbr.jsonl")),
    ("ladder", include_str!("../vectors/ladder.jsonl")),
    ("fixedq", include_str!("../vectors/fixedq.jsonl")),
    ("interlaced", include_str!("../vectors/interlaced.jsonl")),
    ("shed", include_str!("../vectors/shed.jsonl")),
    ("skip", include_str!("../vectors/skip.jsonl")),
    ("deadline", include_str!("../vectors/deadline.jsonl")),
    ("churn", include_str!("../vectors/churn.jsonl")),
    ("devin", include_str!("../vectors/devin.jsonl")),
    ("twofmt", include_str!("../vectors/twofmt.jsonl")),
    ("film", include_str!("../vectors/film.jsonl")),
    ("film60", include_str!("../vectors/film60.jsonl")),
    ("taps", include_str!("../vectors/taps.jsonl")),
];

/// `crates/tb-golden/vectors/<case>.sha256` of the cases whose streams the tests compare.
const DIGESTS: &[(&str, &str)] = &[
    ("cbr", include_str!("../vectors/cbr.sha256")),
    ("ladder", include_str!("../vectors/ladder.sha256")),
    ("fixedq", include_str!("../vectors/fixedq.sha256")),
    ("interlaced", include_str!("../vectors/interlaced.sha256")),
    ("shed", include_str!("../vectors/shed.sha256")),
    ("skip", include_str!("../vectors/skip.sha256")),
    ("deadline", include_str!("../vectors/deadline.sha256")),
    ("churn", include_str!("../vectors/churn.sha256")),
    ("devin", include_str!("../vectors/devin.sha256")),
    ("twofmt", include_str!("../vectors/twofmt.sha256")),
    ("film", include_str!("../vectors/film.sha256")),
    ("film60", include_str!("../vectors/film60.sha256")),
];

/// The SHA-256 digests (lowercase hex) of a case's coded streams, in the order of
/// `crates/tb-golden/vectors/<case>.sha256` (by file name: the sessions in order). Empty for an unknown case.
pub fn digests(name: &str) -> Vec<&'static str> {
    DIGESTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| {
            t.lines()
                .filter_map(|l| l.split_whitespace().next())
                .collect()
        })
        .unwrap_or_default()
}

/// The text of an embedded case, as committed.
pub fn text(name: &str) -> Option<&'static str> {
    CASES.iter().find_map(|(n, t)| (*n == name).then_some(*t))
}

/// Names of the embedded cases.
pub fn case_names() -> impl Iterator<Item = &'static str> {
    CASES.iter().map(|(n, _)| *n)
}

/// What is wrong with a golden file or with a test's use of it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GoldenError {
    /// No embedded case has this name.
    #[error("no golden case named {0}")]
    NoCase(String),
    /// A line is not a JSON object with a string `k`.
    #[error("golden case {case}, line {line}: {what}")]
    Parse {
        /// The case.
        case: String,
        /// 1-based line number.
        line: usize,
        /// What is wrong.
        what: String,
    },
    /// A record lacks a field, or the field has another type.
    #[error("record `{kind}` has no {ty} field `{field}`")]
    Field {
        /// The record's kind.
        kind: String,
        /// The field asked for.
        field: String,
        /// The type asked for.
        ty: &'static str,
    },
}

/// One record: a JSON object whose `k` is its kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Record(Value);

/// The records of a case, in the order they were written.
pub fn case(name: &str) -> Result<Vec<Record>, GoldenError> {
    let text = CASES
        .iter()
        .find_map(|(n, t)| (*n == name).then_some(*t))
        .ok_or_else(|| GoldenError::NoCase(name.to_owned()))?;
    parse(name, text)
}

/// Parse the text of a golden file.
pub fn parse(name: &str, text: &str) -> Result<Vec<Record>, GoldenError> {
    let err = |line: usize, what: String| GoldenError::Parse {
        case: name.to_owned(),
        line,
        what,
    };
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        let v: Value = serde_json::from_str(l).map_err(|e| err(i + 1, e.to_string()))?;
        if v.get("k").and_then(Value::as_str).is_none() {
            return Err(err(i + 1, "no string field `k`".to_owned()));
        }
        out.push(Record(v));
    }
    Ok(out)
}

impl Record {
    fn field(&self, field: &str, ty: &'static str) -> GoldenError {
        GoldenError::Field {
            kind: self.kind().to_owned(),
            field: field.to_owned(),
            ty,
        }
    }

    /// The record's kind (`k`).
    pub fn kind(&self) -> &str {
        self.0.get("k").and_then(Value::as_str).unwrap_or("")
    }

    /// Whether the record has a field.
    pub fn has(&self, field: &str) -> bool {
        self.0.get(field).is_some()
    }

    /// An integer field.
    pub fn int(&self, field: &str) -> Result<i64, GoldenError> {
        self.0
            .get(field)
            .and_then(Value::as_i64)
            .ok_or_else(|| self.field(field, "integer"))
    }

    /// An integer field that is 0 or 1.
    pub fn flag(&self, field: &str) -> Result<bool, GoldenError> {
        match self.int(field)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.field(field, "0/1")),
        }
    }

    /// A number field as `f64` (integers convert).
    pub fn num(&self, field: &str) -> Result<f64, GoldenError> {
        self.0
            .get(field)
            .and_then(Value::as_f64)
            .ok_or_else(|| self.field(field, "number"))
    }

    /// A string field.
    pub fn text(&self, field: &str) -> Result<&str, GoldenError> {
        self.0
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| self.field(field, "string"))
    }

    /// A hex field as bytes.
    pub fn bytes(&self, field: &str) -> Result<Vec<u8>, GoldenError> {
        let s = self.text(field)?;
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        if s.len() % 2 != 0 {
            return Err(self.field(field, "hex"));
        }
        s.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| self.field(field, "hex"))
    }

    /// A hex field as an array of plain-data records (for example `rowstat.rows` as
    /// `tb_kernel_abi::RowStat`). The copy is aligned whatever the bytes' alignment.
    pub fn pods<T: bytemuck::Pod>(&self, field: &str) -> Result<Vec<T>, GoldenError> {
        let b = self.bytes(field)?;
        if size_of::<T>() == 0 || b.len() % size_of::<T>() != 0 {
            return Err(self.field(field, "record array"));
        }
        Ok(b.chunks_exact(size_of::<T>())
            .map(bytemuck::pod_read_unaligned)
            .collect())
    }

    /// An 8-byte hex field as a little-endian `u64` (`au.fnv`, `tick.rng`).
    pub fn u64_le(&self, field: &str) -> Result<u64, GoldenError> {
        let b = self.bytes(field)?;
        let a: [u8; 8] = b
            .as_slice()
            .try_into()
            .map_err(|_| self.field(field, "8 hex bytes"))?;
        Ok(u64::from_le_bytes(a))
    }

    /// An array of integers.
    pub fn ints(&self, field: &str) -> Result<Vec<i64>, GoldenError> {
        self.0
            .get(field)
            .and_then(Value::as_array)
            .and_then(|a| a.iter().map(Value::as_i64).collect())
            .ok_or_else(|| self.field(field, "integer array"))
    }

    /// An array of numbers.
    pub fn nums(&self, field: &str) -> Result<Vec<f64>, GoldenError> {
        self.0
            .get(field)
            .and_then(Value::as_array)
            .and_then(|a| a.iter().map(Value::as_f64).collect())
            .ok_or_else(|| self.field(field, "number array"))
    }
}

/// FNV-1a (64 bit), the digest of coded pictures in `au` records.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb_kernel_abi::{Geo, RowStat, SessPar, Tables};

    #[test]
    fn every_case_parses_and_starts_with_its_header() {
        for name in case_names() {
            let recs = case(name).unwrap();
            assert_eq!(recs[0].kind(), "golden", "{name}");
            assert_eq!(recs[0].int("version").unwrap(), 1);
        }
        assert!(matches!(case("nope"), Err(GoldenError::NoCase(_))));
    }

    #[test]
    fn records_carry_the_abi_records() {
        let tables = case("tables").unwrap();
        assert_eq!(tables[1].pods::<Tables>("bytes").unwrap().len(), 1);
        for name in ["cbr", "ladder", "fixedq", "interlaced", "shed"] {
            let recs = case(name).unwrap();
            let batch = recs.iter().find(|r| r.kind() == "batch").unwrap();
            let geo = batch.pods::<Geo>("geo").unwrap()[0];
            assert_eq!(geo.nmb, geo.mbw * geo.mbh);
            for r in recs.iter().filter(|r| r.kind() == "begin") {
                let par = r.pods::<SessPar>("par").unwrap();
                assert_eq!(i64::from(par[0].pic_type), r.int("type").unwrap());
            }
            for r in recs.iter().filter(|r| r.kind() == "rowstat") {
                assert_eq!(r.pods::<RowStat>("rows").unwrap().len(), geo.mbh as usize);
            }
        }
    }

    /// A repeated picture carries its bytes; its digest and length are those of the bytes.
    #[test]
    fn repeat_pictures_carry_their_bytes() {
        let recs = case("shed").unwrap();
        let mut n = 0;
        for r in recs
            .iter()
            .filter(|r| r.kind() == "au" && r.flag("repeat").unwrap())
        {
            let data = r.bytes("data").unwrap();
            assert_eq!(data.len() as i64, r.int("len").unwrap());
            assert_eq!(fnv1a(&data), r.u64_le("fnv").unwrap());
            n += 1;
        }
        assert!(n > 10);
    }

    /// Doubles survive the text form exactly: the buffer's deliverable bits of the first
    /// picture are the rate times D to the last bit.
    #[test]
    fn doubles_are_exact() {
        let recs = case("cbr").unwrap();
        let open = recs.iter().find(|r| r.kind() == "open").unwrap();
        let begin = recs.iter().find(|r| r.kind() == "begin").unwrap();
        let d = open.num("D").unwrap();
        assert_eq!(
            d,
            open.num("vbv_bits").unwrap() / open.num("max_bitrate").unwrap()
        );
        assert_eq!(begin.num("cap").unwrap(), open.num("bitrate").unwrap() * d);
    }
}
