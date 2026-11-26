//! The CEA-608 character repertoire and its codes.
//!
//! - Standard characters, one byte 0x20 to 0x7F: CTA-608-E §6.4.1 and Table 50 (F.1.1.2,
//!   printed p.97-98; the FCC table of 47 CFR 15.119). Not ASCII everywhere: 0x2A is `á`, 0x5C
//!   `é`, 0x5E `í`, 0x5F `ó`, 0x60 `ú`, 0x7B `ç`, 0x7C `÷`, 0x7D `Ñ`, 0x7E `ñ`, 0x7F the solid
//!   block `■`.
//! - Special characters, two bytes 0x11 (data channel 1) or 0x19 (data channel 2) then 0x30 to
//!   0x3F: Table 49 (F.1.1.1, p.96). 0x39 is the transparent space.
//! - Extended characters, two bytes 0x12 or 0x13 (0x1A or 0x1B on data channel 2) then 0x20 to
//!   0x3F: §6.4.2, Tables 5 to 10 (p.20-25). "Each of the 64 Extended Characters incorporates
//!   an automatic BS": a receiver backs up one column, erasing the character there, before
//!   showing the extended character, so the provider sends a standard character first that a
//!   receiver without extended characters keeps (p.18-19). That character is
//!   [`Char608::fallback`]: the "Alternate" of the tables where they give one, otherwise our
//!   choice of the nearest standard character.
//!
//! The glyphs of Tables 9 and 10 for the vertical bar and the four box corners are drawn, not
//! named; they are mapped to U+2502 and U+250C, U+2510, U+2514, U+2518 (the box-drawing
//! characters CTA-708-E's G2 set uses for the same purpose), and U+23D0 and U+23A1 to U+23A6,
//! the code points of the glyphs in the PDF's text layer, are accepted as well.

/// Standard characters 0x20 to 0x7F (Table 50).
const STANDARD: [char; 96] = [
    ' ', '!', '"', '#', '$', '%', '&', '\'', '(', ')', 'á', '+', ',', '-', '.', '/', //
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?', //
    '@', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', //
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '[', 'é', ']', 'í', 'ó', //
    'ú', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', //
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'ç', '÷', 'Ñ', 'ñ', '■', //
];

/// Special characters 0x30 to 0x3F (Table 49); 0x39, the transparent space, has no character.
const SPECIAL: [char; 16] = [
    '®', '°', '½', '¿', '™', '¢', '£', '♪', 'à', '\0', 'è', 'â', 'ê', 'î', 'ô', 'û',
];

/// Extended characters of first byte 0x12 (Tables 5 to 7) and 0x13 (Tables 8 to 10), second
/// byte 0x20 to 0x3F, with the standard character sent before each.
const EXTENDED: [[(char, char); 32]; 2] = [
    [
        // Table 5, Spanish
        ('Á', 'A'),
        ('É', 'E'),
        ('Ó', 'O'),
        ('Ú', 'U'),
        ('Ü', 'U'),
        ('ü', 'u'),
        ('‘', '\''),
        ('¡', '!'),
        // Table 6, miscellaneous
        ('*', '.'),
        ('\'', '\''),
        ('—', '-'),
        ('©', 'c'),
        ('℠', ' '),
        ('•', '.'),
        ('“', '"'),
        ('”', '"'),
        // Table 7, French
        ('À', 'A'),
        ('Â', 'A'),
        ('Ç', 'C'),
        ('È', 'E'),
        ('Ê', 'E'),
        ('Ë', 'E'),
        ('ë', 'e'),
        ('Î', 'I'),
        ('Ï', 'I'),
        ('ï', 'i'),
        ('Ô', 'O'),
        ('Ù', 'U'),
        ('ù', 'u'),
        ('Û', 'U'),
        ('«', '"'),
        ('»', '"'),
    ],
    [
        // Table 8, Portuguese
        ('Ã', 'A'),
        ('ã', 'a'),
        ('Í', 'I'),
        ('Ì', 'I'),
        ('ì', 'i'),
        ('Ò', 'O'),
        ('ò', 'o'),
        ('Õ', 'O'),
        ('õ', 'o'),
        ('{', '('),
        ('}', ')'),
        ('\\', '/'),
        ('^', '\''),
        ('_', '-'),
        ('|', '!'),
        ('~', '-'),
        // Table 9, German
        ('Ä', 'A'),
        ('ä', 'a'),
        ('Ö', 'O'),
        ('ö', 'o'),
        ('ß', 's'),
        ('¥', 'Y'),
        ('¤', '$'),
        ('│', '!'),
        // Table 10, Danish
        ('Å', 'A'),
        ('å', 'a'),
        ('Ø', 'O'),
        ('ø', 'o'),
        ('┌', '+'),
        ('┐', '+'),
        ('└', '+'),
        ('┘', '+'),
    ],
];

/// A character CEA-608 can show. Only encodable characters can be made: through
/// [`Char608::from_char`] or the constants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Char608(Kind);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    /// One byte, 0x20 to 0x7F.
    Standard(u8),
    /// Second byte of 0x11 / 0x19, 0x30 to 0x3F.
    Special(u8),
    /// First byte 0x12 + `set` (0 or 1), second byte 0x20 to 0x3F.
    Extended { set: u8, code: u8 },
}

/// How a character is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coding {
    /// One byte (0x20 to 0x7F, before parity) of a character pair.
    Byte(u8),
    /// A two-byte code of data channel 1 (add 0x08 to the first byte for data channel 2).
    Code([u8; 2]),
}

impl Char608 {
    /// The transparent space (special character 0x39): moves the cursor without covering the
    /// picture.
    pub const TRANSPARENT_SPACE: Self = Self(Kind::Special(0x39));
    /// The standard space.
    pub const SPACE: Self = Self(Kind::Standard(0x20));

    /// The character for `c`, if CEA-608 has it.
    pub fn from_char(c: char) -> Option<Self> {
        let c = match c {
            // drawn glyphs of Tables 9 and 10 as the PDF's text layer has them
            '\u{23d0}' => '│',
            '\u{23a1}' => '┌',
            '\u{23a4}' => '┐',
            '\u{23a3}' => '└',
            '\u{23a6}' => '┘',
            c => c,
        };
        if c == '\'' {
            // the standard apostrophe; extended 0x29 is the same character, uncurled
            return Some(Self(Kind::Standard(0x27)));
        }
        if let Some(i) = STANDARD.iter().position(|&s| s == c) {
            return Some(Self(Kind::Standard(0x20 + i as u8)));
        }
        if c != '\0'
            && let Some(i) = SPECIAL.iter().position(|&s| s == c)
        {
            return Some(Self(Kind::Special(0x30 + i as u8)));
        }
        for (set, table) in EXTENDED.iter().enumerate() {
            if let Some(i) = table.iter().position(|&(e, _)| e == c) {
                return Some(Self(Kind::Extended {
                    set: set as u8,
                    code: 0x20 + i as u8,
                }));
            }
        }
        None
    }

    /// The character from its codes: a standard byte (`[b, 0]`) or a two-byte code of either
    /// data channel. `None` for anything else (including the transparent space, which is
    /// [`Char608::TRANSPARENT_SPACE`]).
    pub fn from_code(c: [u8; 2]) -> Option<Self> {
        let [a, b] = [c[0] & 0x7F, c[1] & 0x7F];
        match a {
            0x20..=0x7F => Some(Self(Kind::Standard(a))),
            0x11 | 0x19 if (0x30..=0x3F).contains(&b) => Some(Self(Kind::Special(b))),
            0x12 | 0x13 | 0x1A | 0x1B if (0x20..=0x3F).contains(&b) => Some(Self(Kind::Extended {
                set: a & 1,
                code: b,
            })),
            _ => None,
        }
    }

    /// The Unicode character (a space for the transparent space).
    pub fn to_char(self) -> char {
        match self.0 {
            Kind::Standard(b) => STANDARD[usize::from(b - 0x20)],
            Kind::Special(0x39) => ' ',
            Kind::Special(b) => SPECIAL[usize::from(b - 0x30)],
            Kind::Extended { set, code } => EXTENDED[usize::from(set)][usize::from(code - 0x20)].0,
        }
    }

    /// How it is sent (data channel 1).
    pub fn coding(self) -> Coding {
        match self.0 {
            Kind::Standard(b) => Coding::Byte(b),
            Kind::Special(b) => Coding::Code([0x11, b]),
            Kind::Extended { set, code } => Coding::Code([0x12 + set, code]),
        }
    }

    /// For an extended character, the standard character sent before it (which the
    /// extended character's automatic backspace replaces).
    pub fn fallback(self) -> Option<Self> {
        match self.0 {
            Kind::Extended { set, code } => {
                let f = EXTENDED[usize::from(set)][usize::from(code - 0x20)].1;
                Self::from_char(f)
            }
            _ => None,
        }
    }

    /// An extended character (with an automatic backspace).
    pub fn is_extended(self) -> bool {
        matches!(self.0, Kind::Extended { .. })
    }

    /// Every character of the repertoire (standard, special without the transparent space,
    /// extended), for tests and tables.
    pub fn all() -> impl Iterator<Item = Self> {
        let std = (0x20..=0x7Fu8).map(|b| Self(Kind::Standard(b)));
        let sp = (0x30..=0x3Fu8)
            .filter(|&b| b != 0x39)
            .map(|b| Self(Kind::Special(b)));
        let ext = (0..2u8)
            .flat_map(|set| (0x20..=0x3Fu8).map(move |code| Self(Kind::Extended { set, code })));
        std.chain(sp).chain(ext)
    }
}

/// What a character of the text becomes in CEA-608: itself, or a short replacement for the
/// common characters CEA-608 lacks. `None`: nothing close; the caller shows `?`
/// and counts it.
pub fn substitute(c: char) -> Option<&'static str> {
    if Char608::from_char(c).is_some() {
        return None;
    }
    Some(match c {
        '’' | '`' | '´' | '′' => "'",
        '‚' => ",",
        '„' | '″' => "\"",
        '–' | '‐' | '‑' | '−' => "-",
        '…' => "...",
        '\u{a0}' | '\u{2007}' | '\u{202f}' | '\u{2009}' | '\u{200a}' => " ",
        'ā' => "a",
        'æ' => "ae",
        'Æ' => "AE",
        'œ' => "oe",
        'Œ' => "OE",
        'ÿ' => "y",
        'ý' => "y",
        'Ý' => "Y",
        'ð' => "d",
        'Ð' => "D",
        'þ' => "th",
        'Þ' => "Th",
        '×' => "x",
        '±' => "+-",
        '·' => ".",
        '♫' | '♬' => "♪",
        '€' => "EUR",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repertoire_round_trips() {
        let all: Vec<Char608> = Char608::all().collect();
        assert_eq!(all.len(), 96 + 15 + 64);
        for c in &all {
            let u = c.to_char();
            let back = Char608::from_char(u).unwrap();
            // the uncurled single quote of Table 6 reads back as the standard apostrophe
            if *c != Char608(Kind::Extended { set: 0, code: 0x29 }) {
                assert_eq!(back, *c, "{u:?}");
            }
            let code = match c.coding() {
                Coding::Byte(b) => [b, 0],
                Coding::Code(x) => x,
            };
            assert_eq!(Char608::from_code(code), Some(*c));
            // data channel 2 and the parity bit
            if let Coding::Code([a, b]) = c.coding() {
                assert_eq!(Char608::from_code([a | 0x88, b | 0x80]), Some(*c));
            }
            if c.is_extended() {
                let f = c.fallback().unwrap();
                assert!(
                    matches!(f.coding(), Coding::Byte(_)),
                    "{u:?} falls back to {f:?}"
                );
            } else {
                assert_eq!(c.fallback(), None);
            }
        }
        // distinct characters
        let mut chars: Vec<char> = all.iter().map(|c| c.to_char()).collect();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), all.len() - 1);
    }

    #[test]
    fn table_spot_checks() {
        // CTA-608-E Table 50: the non-ASCII positions
        for (b, c) in [
            (0x2A, 'á'),
            (0x5C, 'é'),
            (0x5E, 'í'),
            (0x5F, 'ó'),
            (0x60, 'ú'),
            (0x7B, 'ç'),
            (0x7C, '÷'),
            (0x7D, 'Ñ'),
            (0x7E, 'ñ'),
            (0x7F, '■'),
        ] {
            assert_eq!(Char608::from_char(c).unwrap().coding(), Coding::Byte(b));
        }
        // Table 49: music note 0x11 0x37; F.1.1.1 example "0x19 0x37" is the same on channel 2
        assert_eq!(
            Char608::from_char('♪').unwrap().coding(),
            Coding::Code([0x11, 0x37])
        );
        // Tables 5 and 10
        assert_eq!(
            Char608::from_char('Á').unwrap().coding(),
            Coding::Code([0x12, 0x20])
        );
        assert_eq!(
            Char608::from_char('ü').unwrap().coding(),
            Coding::Code([0x12, 0x25])
        );
        assert_eq!(
            Char608::from_char('Ø').unwrap().coding(),
            Coding::Code([0x13, 0x3A])
        );
        assert_eq!(
            Char608::from_char('⎦').unwrap().coding(),
            Coding::Code([0x13, 0x3F])
        );
        // ASCII characters missing from Table 50 are extended
        for (c, code) in [
            ('*', [0x12, 0x28]),
            ('{', [0x13, 0x29]),
            ('~', [0x13, 0x2F]),
        ] {
            assert_eq!(Char608::from_char(c).unwrap().coding(), Coding::Code(code));
        }
        assert_eq!(Char608::from_char('`'), None);
        assert_eq!(Char608::from_char('€'), None);
        assert_eq!(
            Char608::TRANSPARENT_SPACE.coding(),
            Coding::Code([0x11, 0x39])
        );
    }

    #[test]
    fn substitutes_are_encodable() {
        for c in "’`‚„–…\u{a0}æÆœŒÿ×±·♫€ð".chars() {
            let s = substitute(c).unwrap();
            assert!(s.chars().all(|c| Char608::from_char(c).is_some()), "{c}");
        }
        assert_eq!(substitute('a'), None);
        assert_eq!(substitute('漢'), None);
    }
}
