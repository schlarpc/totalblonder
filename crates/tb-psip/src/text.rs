//! Text types: ISO 639 language codes, channel short names, and the A/65
//! `multiple_string_structure` (A/65 §6.10 Table 6.39, p.79–82; `notes/standards-digest.md`
//! §3.8).

use crate::Error;

/// An ISO 639-2/B language code: three lower-case ASCII letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lang([u8; 3]);

impl Lang {
    /// English.
    pub const ENG: Lang = Lang(*b"eng");
    /// Spanish.
    pub const SPA: Lang = Lang(*b"spa");

    /// `code` if it is three ASCII letters (lower-cased).
    pub fn new(code: &str) -> Result<Lang, Error> {
        let b = code.as_bytes();
        if b.len() != 3 || !b.iter().all(u8::is_ascii_alphabetic) {
            return Err(Error::Lang(code.to_owned()));
        }
        Ok(Lang([
            b[0].to_ascii_lowercase(),
            b[1].to_ascii_lowercase(),
            b[2].to_ascii_lowercase(),
        ]))
    }

    /// The three bytes.
    pub const fn bytes(self) -> [u8; 3] {
        self.0
    }
}

/// A virtual channel's `short_name`: one to seven UTF-16 code units (A/65 §6.3.1, p.31–32).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShortName(Vec<u16>);

impl ShortName {
    /// `s` if it is 1–7 UTF-16 code units and contains no NUL.
    pub fn new(s: &str) -> Result<ShortName, Error> {
        let u: Vec<u16> = s.encode_utf16().collect();
        if u.is_empty() || u.len() > 7 || u.contains(&0) {
            return Err(Error::ShortName(s.to_owned()));
        }
        Ok(ShortName(u))
    }

    /// The 14 bytes of the field: big-endian code units, padded with 0x0000.
    pub fn field(&self) -> [u8; 14] {
        let mut b = [0u8; 14];
        for (i, u) in self.0.iter().enumerate() {
            b[2 * i..2 * i + 2].copy_from_slice(&u.to_be_bytes());
        }
        b
    }
}

/// One string of a multiple-string structure: a language and its text.
///
/// Encoding (our choice, the digest's recommendation): one segment per at most 255 bytes,
/// `compression_type` 0x00; `mode` 0x00 (Latin-1, U+0000–U+00FF) when every character fits,
/// otherwise `mode` 0x3F (UTF-16, most significant byte first; A/65 Table 6.41, p.82). No
/// Huffman compression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LangString {
    /// The language.
    pub lang: Lang,
    /// The text.
    pub text: String,
}

/// A `multiple_string_structure` value. The empty structure (`number_strings` 0) is the
/// recommended form of a null string (A/65 §6.10, p.80).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Mss(pub Vec<LangString>);

impl Mss {
    /// One English string.
    pub fn eng(text: &str) -> Mss {
        Mss(vec![LangString {
            lang: Lang::ENG,
            text: text.to_owned(),
        }])
    }

    /// Appends the encoding to `out`.
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let n = u8::try_from(self.0.len()).map_err(|_| Error::TooLong("number_strings"))?;
        out.push(n);
        for s in &self.0 {
            out.extend_from_slice(&s.lang.bytes());
            let (mode, bytes, unit) = if s.text.chars().all(|c| u32::from(c) <= 0xFF) {
                (
                    0x00u8,
                    s.text
                        .chars()
                        .map(|c| u32::from(c) as u8)
                        .collect::<Vec<u8>>(),
                    1,
                )
            } else {
                let b: Vec<u8> = s.text.encode_utf16().flat_map(u16::to_be_bytes).collect();
                (0x3Fu8, b, 2)
            };
            // Segments of at most 255 bytes, never splitting a UTF-16 code unit (or, by keeping
            // an even segment length, a surrogate pair's halves across a unit boundary).
            let seg = if unit == 2 { 254 } else { 255 };
            let chunks: Vec<&[u8]> = if bytes.is_empty() {
                vec![]
            } else {
                bytes.chunks(seg).collect()
            };
            let ns = u8::try_from(chunks.len()).map_err(|_| Error::TooLong("number_segments"))?;
            out.push(ns);
            for c in chunks {
                out.push(0x00); // compression_type: none
                out.push(mode);
                out.push(c.len() as u8);
                out.extend_from_slice(c);
            }
        }
        Ok(())
    }

    /// The encoding as bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut v = Vec::new();
        self.write(&mut v)?;
        Ok(v)
    }
}

/// Decodes a multiple-string structure written with modes 0x00 or 0x3F and no compression
/// (what [`Mss::write`] produces); for tests and checkers. Returns the strings and the bytes
/// used.
pub fn parse_mss(b: &[u8]) -> Option<(Vec<(Lang, String)>, usize)> {
    let mut i = 0;
    let n = *b.first()?;
    i += 1;
    let mut out = Vec::new();
    for _ in 0..n {
        let l = b.get(i..i + 3)?;
        let lang = Lang([l[0], l[1], l[2]]);
        i += 3;
        let segs = *b.get(i)?;
        i += 1;
        let mut text = String::new();
        let mut utf16 = Vec::new();
        for _ in 0..segs {
            let comp = *b.get(i)?;
            let mode = *b.get(i + 1)?;
            let len = usize::from(*b.get(i + 2)?);
            let d = b.get(i + 3..i + 3 + len)?;
            i += 3 + len;
            if comp != 0 {
                return None;
            }
            match mode {
                0x00 => text.extend(d.iter().map(|&c| char::from(c))),
                0x3F => utf16.extend(
                    d.chunks(2)
                        .map(|p| u16::from_be_bytes([p[0], *p.get(1).unwrap_or(&0)])),
                ),
                _ => return None,
            }
        }
        if !utf16.is_empty() {
            text.push_str(&String::from_utf16(&utf16).ok()?);
        }
        out.push((lang, text));
    }
    Some((out, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang() {
        assert_eq!(Lang::new("ENG").unwrap(), Lang::ENG);
        assert!(Lang::new("en").is_err());
        assert!(Lang::new("e1g").is_err());
    }

    #[test]
    fn short_name() {
        let n = ShortName::new("WALL").unwrap();
        assert_eq!(&n.field()[..8], &[0, b'W', 0, b'A', 0, b'L', 0, b'L']);
        assert!(n.field()[8..].iter().all(|&b| b == 0));
        assert!(ShortName::new("EIGHTCHR").is_err());
        assert!(ShortName::new("").is_err());
    }

    #[test]
    fn mss_latin1_and_utf16_round_trip() {
        for text in [
            "Live Wall",
            "Caf\u{e9} \u{bf}qu\u{e9}?",
            "\u{65e5}\u{672c}",
            &"x".repeat(600),
            "",
        ] {
            let m = Mss::eng(text);
            let b = m.to_bytes().unwrap();
            let (s, used) = parse_mss(&b).unwrap();
            assert_eq!(used, b.len());
            assert_eq!(s, vec![(Lang::ENG, text.to_owned())]);
        }
        assert_eq!(Mss::default().to_bytes().unwrap(), vec![0]);
    }
}
