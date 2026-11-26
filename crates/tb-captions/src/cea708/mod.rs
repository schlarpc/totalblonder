//! CEA-708 (CTA-708-E S-2023): the DTVCC coding layer (commands and character sets) and the
//! packet and service layers that carry it in `cc_data()`.
//!
//! Page numbers are the printed ones of CTA-708-E (PDF page = printed + 9).
//!
//! - Code sets (§7.1, p.27-38): C0 controls (ETX 0x03, BS 0x08, FF 0x0C, CR 0x0D, HCR 0x0E,
//!   EXT1 0x10), C1 caption commands 0x80 to 0x9F, G0 (ASCII with the music note at 0x7F), G1
//!   (ISO 8859-1 at 0xA0 to 0xFF), G2 and G3 after EXT1 ([`Char708`]).
//! - Commands (§8.10.5, p.63-83), each with its bit layout: [`Command`]. The command
//!   examples of the standard are tests below; where the prose of an example disagrees with
//!   its own bytes (SetPenAttributes, p.78: the bytes 0x4A 0xCA give pen size LARGE and font
//!   style 2 by the coding table, the prose says SMALL and 4), the coding table is followed.
//! - Services (§6, p.23-26) and caption channel packets (§5, p.21-22): [`packet`].
//!
//! Typed parameters: a [`ServiceNumber`] is 1 to 63, a [`WindowId`] 0 to 7, colors have two
//! bits per component, and every other field is an enum of its legal values, so a command
//! that this module encodes is well formed by construction.

pub mod decode;
pub mod packet;

/// A caption service number, 1 to 63 (§6.1: "Service #0 shall not be used").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceNumber(u8);

impl ServiceNumber {
    /// Service `n`, 1 to 63.
    pub const fn new(n: u8) -> Option<Self> {
        if n >= 1 && n <= 63 {
            Some(Self(n))
        } else {
            None
        }
    }
    /// Service 1, the primary caption service (§6.1).
    pub const PRIMARY: Self = Self(1);
    /// Service 2, the secondary language service (§6.1).
    pub const SECONDARY: Self = Self(2);
    /// Its number.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A window identifier, 0 to 7 (§8.4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(u8);

impl WindowId {
    /// Window `n`, 0 to 7.
    pub const fn new(n: u8) -> Option<Self> {
        if n <= 7 { Some(Self(n)) } else { None }
    }
    /// Window 0.
    pub const ZERO: Self = Self(0);
    /// Its number.
    pub const fn get(self) -> u8 {
        self.0
    }
    /// The window map with only this window.
    pub const fn map(self) -> WindowMap {
        WindowMap(1 << self.0)
    }
}

/// A set of windows: bit `n` for window `n` (§8.10.5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct WindowMap(pub u8);

/// A color: two bits each of red, green and blue (§8.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(u8);

impl Color {
    /// `(r, g, b)`, each 0 to 3.
    pub const fn new(r: u8, g: u8, b: u8) -> Option<Self> {
        if r > 3 || g > 3 || b > 3 {
            return None;
        }
        Some(Self((r << 4) | (g << 2) | b))
    }
    /// (0, 0, 0).
    pub const BLACK: Self = Self(0);
    /// (2, 2, 2): the white of the predefined pen styles (Table 27).
    pub const WHITE: Self = Self(0x2A);
    /// The six bits.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Opacity of fills and pens (§8.10.5.8, §8.10.5.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Opacity {
    /// Solid.
    Solid = 0,
    /// Flashing.
    Flash = 1,
    /// Translucent.
    Translucent = 2,
    /// Transparent.
    Transparent = 3,
}

/// Anchor point of a window (§8.4.4, Figure 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Anchor {
    /// Upper left.
    TopLeft = 0,
    /// Upper center.
    TopCenter = 1,
    /// Upper right.
    TopRight = 2,
    /// Middle left.
    MiddleLeft = 3,
    /// Center.
    Center = 4,
    /// Middle right.
    MiddleRight = 5,
    /// Lower left.
    BottomLeft = 6,
    /// Lower center.
    BottomCenter = 7,
    /// Lower right.
    BottomRight = 8,
}

/// Text justification (§8.10.5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Justify {
    /// Left.
    Left = 0,
    /// Right.
    Right = 1,
    /// Center.
    Center = 2,
    /// Full.
    Full = 3,
}

/// Print and scroll directions (§8.10.5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Left to right.
    LeftToRight = 0,
    /// Right to left.
    RightToLeft = 1,
    /// Top to bottom.
    TopToBottom = 2,
    /// Bottom to top.
    BottomToTop = 3,
}

/// Display effect (§8.10.5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effect {
    /// Snap on and off.
    Snap = 0,
    /// Fade.
    Fade = 1,
    /// Wipe.
    Wipe = 2,
}

/// Border type (§8.10.5.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Border {
    /// None.
    None = 0,
    /// Raised.
    Raised = 1,
    /// Depressed.
    Depressed = 2,
    /// Uniform.
    Uniform = 3,
    /// Shadow left.
    ShadowLeft = 4,
    /// Shadow right.
    ShadowRight = 5,
}

/// Pen size (§8.10.5.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PenSize {
    /// Small.
    Small = 0,
    /// Standard.
    Standard = 1,
    /// Large.
    Large = 2,
}

/// Character offset (§8.10.5.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Offset {
    /// Subscript.
    Subscript = 0,
    /// Normal.
    Normal = 1,
    /// Superscript.
    Superscript = 2,
}

/// Character edge type (§8.10.5.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    /// None.
    None = 0,
    /// Raised.
    Raised = 1,
    /// Depressed.
    Depressed = 2,
    /// Uniform.
    Uniform = 3,
    /// Left drop shadow.
    LeftDropShadow = 4,
    /// Right drop shadow.
    RightDropShadow = 5,
}

/// The parameters of DefineWindow (§8.10.5.2, p.65-67).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowDef {
    /// The window.
    pub id: WindowId,
    /// Display priority, 0 (highest) to 7 (masked to 3 bits).
    pub priority: u8,
    /// Anchor point.
    pub anchor: Anchor,
    /// The anchor coordinates are percentages (0 to 99) rather than grid cells.
    pub relative: bool,
    /// Vertical anchor: 0 to 74 on the grid (masked to 7 bits).
    pub av: u8,
    /// Horizontal anchor: 0 to 209 (16:9) or 0 to 159 (4:3) on the grid.
    pub ah: u8,
    /// Rows, 1 to 12 (coded as rows - 1; §9.5: legacy decoders ignore more than 4).
    pub rows: u8,
    /// Columns, 1 to 42 (coded as columns - 1; 32 for captions meant for 4:3 too, §9.4).
    pub cols: u8,
    /// Row lock (§9.7: "should always set").
    pub row_lock: bool,
    /// Column lock.
    pub col_lock: bool,
    /// Visible when created.
    pub visible: bool,
    /// Predefined window style, 0 to 7 (0: style 1 on create, unchanged on update).
    pub window_style: u8,
    /// Predefined pen style, 0 to 7.
    pub pen_style: u8,
}

/// The parameters of SetWindowAttributes (§8.10.5.8, p.75-76). Word wrap is always off
/// ("wordwrap shall always be disabled", p.75; §9.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowAttrs {
    /// Justification.
    pub justify: Justify,
    /// Print direction.
    pub print: Direction,
    /// Scroll direction.
    pub scroll: Direction,
    /// Display effect.
    pub effect: Effect,
    /// Effect direction.
    pub effect_dir: Direction,
    /// Effect speed in half seconds, 0 to 15.
    pub effect_speed: u8,
    /// Fill color.
    pub fill: Color,
    /// Fill opacity.
    pub fill_opacity: Opacity,
    /// Border type.
    pub border: Border,
    /// Border color.
    pub border_color: Color,
}

impl WindowAttrs {
    /// Predefined window style 2, "PopUp Captions w/o Black Background" (Table 26): left
    /// justified, left to right, scrolling bottom to top, snap, transparent fill, no border;
    /// "Translated captions from CEA-608 sources may look best" with it (p.67).
    pub const POP_ON: Self = Self {
        justify: Justify::Left,
        print: Direction::LeftToRight,
        scroll: Direction::BottomToTop,
        effect: Effect::Snap,
        effect_dir: Direction::LeftToRight,
        effect_speed: 0,
        fill: Color::BLACK,
        fill_opacity: Opacity::Transparent,
        border: Border::None,
        border_color: Color::BLACK,
    };
}

/// The parameters of SetPenAttributes (§8.10.5.9, p.77-78).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PenAttrs {
    /// Pen size.
    pub size: PenSize,
    /// Font style 0 to 7.
    pub font: u8,
    /// Text tag 0 to 15.
    pub tag: u8,
    /// Offset.
    pub offset: Offset,
    /// Italics.
    pub italics: bool,
    /// Underline.
    pub underline: bool,
    /// Edge type.
    pub edge: Edge,
}

impl PenAttrs {
    /// Predefined pen style 1 (Table 27) with italics and underline as given.
    pub const fn plain(italics: bool, underline: bool) -> Self {
        Self {
            size: PenSize::Standard,
            font: 0,
            tag: 0,
            offset: Offset::Normal,
            italics,
            underline,
            edge: Edge::None,
        }
    }
}

/// The parameters of SetPenColor (§8.10.5.10, p.79).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PenColor {
    /// Foreground color.
    pub fg: Color,
    /// Foreground opacity.
    pub fg_opacity: Opacity,
    /// Background color.
    pub bg: Color,
    /// Background opacity.
    pub bg_opacity: Opacity,
    /// Edge color.
    pub edge: Color,
}

impl PenColor {
    /// Predefined pen style 1's colors: white on solid black (Table 27, "NTSC Style").
    pub const NTSC: Self = Self {
        fg: Color::WHITE,
        fg_opacity: Opacity::Solid,
        bg: Color::BLACK,
        bg_opacity: Opacity::Solid,
        edge: Color::BLACK,
    };
}

/// A character of the DTVCC code sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Char708(CharCode);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CharCode {
    /// One byte of G0 (0x20 to 0x7F) or G1 (0xA0 to 0xFF).
    One(u8),
    /// EXT1 then a G2 (0x20 to 0x7F) or G3 (0xA0 to 0xFF) code.
    Ext(u8),
}

/// G2 (Table 17, p.35): the code and the character. The table's drawn glyphs (box drawing,
/// bullet) are in a symbol font in the PDF's text layer; they are the Unicode box-drawing
/// characters and U+2022.
const G2: [(u8, char); 26] = [
    (0x25, '…'),
    (0x2A, 'Š'),
    (0x2C, 'Œ'),
    (0x30, '█'),
    (0x31, '‘'),
    (0x32, '’'),
    (0x33, '“'),
    (0x34, '”'),
    (0x35, '•'),
    (0x39, '™'),
    (0x3A, 'š'),
    (0x3C, 'œ'),
    (0x3D, '℠'),
    (0x3F, 'Ÿ'),
    (0x76, '⅛'),
    (0x77, '⅜'),
    (0x78, '⅝'),
    (0x79, '⅞'),
    (0x7A, '│'),
    (0x7B, '┐'),
    (0x7C, '└'),
    (0x7D, '─'),
    (0x7E, '┘'),
    (0x7F, '┌'),
    // transparent spaces: no Unicode character; decoded as spaces
    (0x20, '\u{0}'),
    (0x21, '\u{0}'),
];

impl Char708 {
    /// The character for `c`, if the DTVCC sets have it (G0, G1, G2; the caption icon of G3
    /// has no Unicode character).
    pub fn from_char(c: char) -> Option<Self> {
        let u = u32::from(c);
        match u {
            0x20..=0x7E => Some(Self(CharCode::One(u as u8))),
            0xA0..=0xFF => Some(Self(CharCode::One(u as u8))),
            0x266A => Some(Self(CharCode::One(0x7F))),
            0 => None,
            _ => G2
                .iter()
                .find(|&&(_, g)| g == c)
                .map(|&(code, _)| Self(CharCode::Ext(code))),
        }
    }

    /// A near character for one the DTVCC sets lack (dashes and low quotes; the em dash of
    /// CEA-608's extended set is not in G2), or `None`.
    pub fn substitute(c: char) -> Option<Self> {
        let s = match c {
            '—' | '–' | '‐' | '‑' | '−' => '-',
            '‚' => ',',
            '„' | '″' => '"',
            '′' => '\'',
            '♫' | '♬' => '♪',
            '⏐' => '│',
            '■' => '█',
            '⎡' => '┌',
            '⎤' => '┐',
            '⎣' => '└',
            '⎦' => '┘',
            _ => return None,
        };
        Self::from_char(s)
    }

    /// The bytes of the character.
    pub fn push(self, out: &mut Vec<u8>) {
        match self.0 {
            CharCode::One(b) => out.push(b),
            CharCode::Ext(b) => out.extend([0x10, b]),
        }
    }

    /// The Unicode character of a G0 / G1 byte, or of EXT1 + a G2 / G3 byte (`ext`): `None`
    /// for codes without a character (G2 transparent spaces read as a space; undefined G2
    /// and G3 codes as `_`, §7.1.8.1 and §7.1.9).
    pub fn decode(b: u8, ext: bool) -> Option<char> {
        if !ext {
            return match b {
                0x20..=0x7E | 0xA0..=0xFF => char::from_u32(u32::from(b)),
                0x7F => Some('♪'),
                _ => None,
            };
        }
        match b {
            0x20 | 0x21 => Some(' '),
            0x20..=0x7F => Some(G2.iter().find(|g| g.0 == b).map_or('_', |g| g.1)),
            0xA0..=0xFF => Some('_'),
            _ => None,
        }
    }

    /// Every character with a Unicode form, for tests.
    pub fn all() -> impl Iterator<Item = char> {
        (0x20u8..=0x7E)
            .chain(0xA0..=0xFF)
            .map(char::from)
            .chain(std::iter::once('♪'))
            .chain(G2.iter().map(|g| g.1).filter(|&c| c != '\0'))
    }
}

/// A DTVCC command or character: one syntactic element (§7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// SetCurrentWindow, CW0 to CW7 (0x80 + id).
    SetCurrentWindow(WindowId),
    /// DefineWindow, DF0 to DF7 (0x98 + id), 7 bytes.
    DefineWindow(WindowDef),
    /// ClearWindows, CLW 0x88.
    ClearWindows(WindowMap),
    /// DisplayWindows, DSW 0x89.
    DisplayWindows(WindowMap),
    /// HideWindows, HDW 0x8A.
    HideWindows(WindowMap),
    /// ToggleWindows, TGW 0x8B.
    ToggleWindows(WindowMap),
    /// DeleteWindows, DLW 0x8C.
    DeleteWindows(WindowMap),
    /// Delay, DLY 0x8D, in tenths of a second.
    Delay(u8),
    /// DelayCancel, DLC 0x8E.
    DelayCancel,
    /// Reset, RST 0x8F.
    Reset,
    /// SetWindowAttributes, SWA 0x97, 5 bytes.
    SetWindowAttributes(WindowAttrs),
    /// SetPenAttributes, SPA 0x90, 3 bytes.
    SetPenAttributes(PenAttrs),
    /// SetPenColor, SPC 0x91, 4 bytes.
    SetPenColor(PenColor),
    /// SetPenLocation, SPL 0x92: row 0 to 14, column 0 to 41 (masked to 4 and 6 bits).
    SetPenLocation {
        /// Row.
        row: u8,
        /// Column.
        col: u8,
    },
    /// End of text, ETX 0x03.
    Etx,
    /// Carriage return, CR 0x0D.
    Cr,
    /// Horizontal carriage return, HCR 0x0E.
    Hcr,
    /// Form feed, FF 0x0C.
    Ff,
    /// Backspace, BS 0x08.
    Bs,
    /// A character.
    Char(Char708),
}

impl Command {
    /// Appends the bytes of the command.
    pub fn push(&self, out: &mut Vec<u8>) {
        match *self {
            Self::SetCurrentWindow(w) => out.push(0x80 | w.0),
            Self::DefineWindow(d) => out.extend([
                0x98 | d.id.0,
                (u8::from(d.visible) << 5)
                    | (u8::from(d.row_lock) << 4)
                    | (u8::from(d.col_lock) << 3)
                    | (d.priority & 7),
                (u8::from(d.relative) << 7) | (d.av & 0x7F),
                d.ah,
                ((d.anchor as u8) << 4) | (d.rows.clamp(1, 16) - 1),
                d.cols.clamp(1, 64) - 1,
                ((d.window_style & 7) << 3) | (d.pen_style & 7),
            ]),
            Self::ClearWindows(m) => out.extend([0x88, m.0]),
            Self::DisplayWindows(m) => out.extend([0x89, m.0]),
            Self::HideWindows(m) => out.extend([0x8A, m.0]),
            Self::ToggleWindows(m) => out.extend([0x8B, m.0]),
            Self::DeleteWindows(m) => out.extend([0x8C, m.0]),
            Self::Delay(t) => out.extend([0x8D, t]),
            Self::DelayCancel => out.push(0x8E),
            Self::Reset => out.push(0x8F),
            Self::SetWindowAttributes(a) => {
                let bt = a.border as u8;
                out.extend([
                    0x97,
                    ((a.fill_opacity as u8) << 6) | a.fill.0,
                    ((bt & 3) << 6) | a.border_color.0,
                    ((bt >> 2) << 7)
                        | ((a.print as u8) << 4)
                        | ((a.scroll as u8) << 2)
                        | (a.justify as u8),
                    ((a.effect_speed & 0xF) << 4) | ((a.effect_dir as u8) << 2) | (a.effect as u8),
                ]);
            }
            Self::SetPenAttributes(p) => out.extend([
                0x90,
                ((p.tag & 0xF) << 4) | ((p.offset as u8) << 2) | (p.size as u8),
                (u8::from(p.italics) << 7)
                    | (u8::from(p.underline) << 6)
                    | ((p.edge as u8) << 3)
                    | (p.font & 7),
            ]),
            Self::SetPenColor(c) => out.extend([
                0x91,
                ((c.fg_opacity as u8) << 6) | c.fg.0,
                ((c.bg_opacity as u8) << 6) | c.bg.0,
                c.edge.0,
            ]),
            Self::SetPenLocation { row, col } => out.extend([0x92, row & 0x0F, col & 0x3F]),
            Self::Etx => out.push(0x03),
            Self::Cr => out.push(0x0D),
            Self::Hcr => out.push(0x0E),
            Self::Ff => out.push(0x0C),
            Self::Bs => out.push(0x08),
            Self::Char(c) => c.push(out),
        }
    }

    /// The bytes of the command.
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(7);
        self.push(&mut v);
        v
    }
}

/// Bytes of the syntactic element that starts with `b` (§7.1.4 to §7.1.11), or `None` when
/// it depends on the next byte (EXT1) and that is not given. `next` is the byte after `b`.
pub fn element_len(b: u8, next: Option<u8>) -> Option<usize> {
    Some(match b {
        0x00..=0x0F => 1,
        0x10 => {
            let n = next?;
            match n {
                // C2: 0x00-0x07 one byte more, 0x08-0x0F two, 0x10-0x17 three, 0x18-0x1F four
                0x00..=0x07 => 2,
                0x08..=0x0F => 3,
                0x10..=0x17 => 4,
                0x18..=0x1F => 5,
                // C3: 0x80-0x87 five more bytes, 0x88-0x8F six; 0x90-0x9F variable (not
                // supported: treated as the two bytes)
                0x80..=0x87 => 6,
                0x88..=0x8F => 7,
                _ => 2,
            }
        }
        0x11..=0x17 => 2,
        0x18..=0x1F => 3,
        0x20..=0x7F | 0xA0..=0xFF => 1,
        0x80..=0x87 | 0x8E | 0x8F | 0x93..=0x96 => 1,
        0x88..=0x8D => 2,
        0x90 | 0x92 => 3,
        0x91 => 4,
        0x97 => 5,
        0x98..=0x9F => 7,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The examples of §8.10.5 (p.64-81).
    #[test]
    fn examples_of_the_standard() {
        // SetCurrentWindow: "0x84 sets the window ID to window number four"
        assert_eq!(Command::SetCurrentWindow(WindowId(4)).bytes(), [0x84]);
        // DefineWindow: "0x9A, 0x38, 0x4A, 0xD1, 0x8B, 0x0F, 0x11"
        let d = WindowDef {
            id: WindowId(2),
            priority: 0,
            anchor: Anchor::BottomRight,
            relative: false,
            av: 74,
            ah: 209,
            rows: 12,
            cols: 16,
            row_lock: true,
            col_lock: true,
            visible: true,
            window_style: 2,
            pen_style: 1,
        };
        assert_eq!(
            Command::DefineWindow(d).bytes(),
            [0x9A, 0x38, 0x4A, 0xD1, 0x8B, 0x0F, 0x11]
        );
        // ClearWindows "0x88, 0x13" (windows 0, 1, 4); DeleteWindows "0x8C, 0xFE";
        // DisplayWindows "0x89, 0x80"; HideWindows "0x8A, 0x7E"; ToggleWindows "0x8B, 0xF6"
        assert_eq!(Command::ClearWindows(WindowMap(0x13)).bytes(), [0x88, 0x13]);
        assert_eq!(
            Command::DeleteWindows(WindowMap(0xFE)).bytes(),
            [0x8C, 0xFE]
        );
        assert_eq!(
            Command::DisplayWindows(WindowId(7).map()).bytes(),
            [0x89, 0x80]
        );
        assert_eq!(Command::HideWindows(WindowMap(0x7E)).bytes(), [0x8A, 0x7E]);
        assert_eq!(
            Command::ToggleWindows(WindowMap(0xF6)).bytes(),
            [0x8B, 0xF6]
        );
        // SetWindowAttributes "0x97, 0x64, 0x53, 0x88, 0x22"
        let a = WindowAttrs {
            justify: Justify::Left,
            print: Direction::LeftToRight,
            scroll: Direction::TopToBottom,
            effect: Effect::Wipe,
            effect_dir: Direction::LeftToRight,
            effect_speed: 2,
            fill: Color::new(2, 1, 0).unwrap(),
            fill_opacity: Opacity::Flash,
            border: Border::ShadowRight,
            border_color: Color::new(1, 0, 3).unwrap(),
        };
        assert_eq!(
            Command::SetWindowAttributes(a).bytes(),
            [0x97, 0x64, 0x53, 0x88, 0x22]
        );
        // SetPenAttributes "0x90, 0x4A, 0xCA" by the coding table: tag 4, offset superscript,
        // size large; italics, underline, edge raised, font 2
        let p = PenAttrs {
            size: PenSize::Large,
            font: 2,
            tag: 4,
            offset: Offset::Superscript,
            italics: true,
            underline: true,
            edge: Edge::Raised,
        };
        assert_eq!(Command::SetPenAttributes(p).bytes(), [0x90, 0x4A, 0xCA]);
        // SetPenColor "0x91,0x3F,0xC0,0x15"
        let c = PenColor {
            fg: Color::new(3, 3, 3).unwrap(),
            fg_opacity: Opacity::Solid,
            bg: Color::BLACK,
            bg_opacity: Opacity::Transparent,
            edge: Color::new(1, 1, 1).unwrap(),
        };
        assert_eq!(Command::SetPenColor(c).bytes(), [0x91, 0x3F, 0xC0, 0x15]);
        // SetPenLocation "0x92, 0x05, 0x08"; Delay "0x8D, 0x0B"; DLC 0x8E; RST 0x8F
        assert_eq!(
            Command::SetPenLocation { row: 5, col: 8 }.bytes(),
            [0x92, 0x05, 0x08]
        );
        assert_eq!(Command::Delay(11).bytes(), [0x8D, 0x0B]);
        assert_eq!(Command::DelayCancel.bytes(), [0x8E]);
        assert_eq!(Command::Reset.bytes(), [0x8F]);
        // every command's length is what the parser expects
        for cmd in [
            Command::DefineWindow(d),
            Command::SetWindowAttributes(a),
            Command::SetPenAttributes(p),
            Command::SetPenColor(c),
            Command::SetPenLocation { row: 1, col: 2 },
            Command::ClearWindows(WindowMap(1)),
            Command::Etx,
            Command::Char(Char708::from_char('Š').unwrap()),
            Command::Char(Char708::from_char('é').unwrap()),
        ] {
            let b = cmd.bytes();
            assert_eq!(
                element_len(b[0], b.get(1).copied()),
                Some(b.len()),
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn characters() {
        let mut n = 0;
        for c in Char708::all() {
            let ch = Char708::from_char(c).unwrap();
            let mut v = Vec::new();
            ch.push(&mut v);
            let back = match v.as_slice() {
                [b] => Char708::decode(*b, false),
                [0x10, b] => Char708::decode(*b, true),
                _ => None,
            };
            assert_eq!(back, Some(c), "{c:?} {v:02x?}");
            n += 1;
        }
        // 95 of G0, the note, 96 of G1, 24 of G2
        assert_eq!(n, 95 + 1 + 96 + 24);
        assert_eq!(
            Char708::from_char('♪').map(|c| Command::Char(c).bytes()),
            Some(vec![0x7F])
        );
        assert_eq!(
            Char708::from_char('é').map(|c| Command::Char(c).bytes()),
            Some(vec![0xE9])
        );
        assert_eq!(
            Char708::from_char('…').map(|c| Command::Char(c).bytes()),
            Some(vec![0x10, 0x25])
        );
        assert_eq!(Char708::from_char('漢'), None);
        assert_eq!(Char708::from_char('\u{1}'), None);
        assert_eq!(Char708::decode(0x22, true), Some('_'));
        assert_eq!(Char708::decode(0xA0, true), Some('_'));
        assert_eq!(ServiceNumber::new(0), None);
        assert_eq!(ServiceNumber::new(64), None);
        assert_eq!(WindowId::new(8), None);
        assert_eq!(Color::new(4, 0, 0), None);
    }
}
