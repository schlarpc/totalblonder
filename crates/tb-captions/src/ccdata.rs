//! `cc_data()` constructs (CTA-708-E §4.3, Table 2, p.10): building them, reading a source's
//! back, and merging a source's captions with ours.
//!
//! A construct is three bytes: one_bit and four reserved bits '11111', `cc_valid`, `cc_type`
//! (00 / 01: CEA-608 field 1 / 2; 11: start of a caption channel packet; 10: its
//! continuation), then two data bytes. The bytes this crate writes for a picture are exactly
//! the constructs, as `tb_timeline::captions::CaptionCarrier` writes them and the encoder
//! takes them (`tb_m2v_syntax::captions::validate_shown`): the CEA-608 constructs first, one
//! per field period in display order, then the DTVCC ones. Unused CEA-608 positions are
//! `cc_valid` 0 with the null pair ([`PAD_608`]: no waveform for that field, §4.3.5); unused
//! DTVCC positions are `FA 00 00` ([`PAD_DTVCC`], "recommended ... cc_data_1 = 0x00,
//! cc_data_2 = 0x00", §4.3.5).
//!
//! **Pass-through.** [`Parser`] reads the constructs of a source's pictures (as
//! `tb_media::VideoFrameDesc::cc_data` has them, in display order) into CEA-608 pairs per field
//! and whole caption channel packets.
//!
//! **Merging** ([`Merger`]). The rule: *a source's captions win where it has any, ours fill
//! the rest*, decided per service so that a source with only CC1 does not silence our CEA-708
//! service, and the other way round:
//! - CEA-608, per field: the source's constructs are used while the source has sent a valid,
//!   non-null pair on that field within [`MergeCfg::hold`] (default 30 s, longer than the
//!   16 s after which CTA-608-E D.4 lets an encoder blank a silent caption, so the source's
//!   pauses between captions do not let ours in);
//! - DTVCC, as a whole (packets carry several services and are not split): the source's
//!   constructs while it has sent a valid DTVCC construct within the hold time.
//!
//! When ours take over, our constructs that continue a packet begun while the source had the
//! channel are replaced by padding until our next packet starts, so no receiver sees half a
//! packet. When the source takes over, a packet of ours may be cut; a receiver discards a
//! partial element (§4.3.3) and our next screens are self-contained.

use crate::cea608::{Field, Pair};
use crate::cea708::packet::Reassembler;
use crate::cue::{FieldRate, Millis};

/// An unused DTVCC position: `cc_valid` 0, `cc_type` 10.
pub const PAD_DTVCC: [u8; 3] = [0xFA, 0x00, 0x00];

/// An unused CEA-608 position of field 1 and of field 2: `cc_valid` 0, the null pair.
pub const PAD_608: [[u8; 3]; 2] = [[0xF8, 0x80, 0x80], [0xF9, 0x80, 0x80]];

/// Constructs per field period (CTA-708-E §4.3.6: 9600 bit/s at 59.94 field periods per
/// second, rounded).
pub const PER_FIELD_PERIOD: usize = 10;

/// The CEA-608 construct of field `f`: valid with `p`, or the unused position.
pub fn construct_608(f: Field, p: Option<Pair>) -> [u8; 3] {
    match p {
        Some(p) => {
            let [a, b] = p.bytes();
            [0xFC | f.index() as u8, a, b]
        }
        None => PAD_608[f.index()],
    }
}

/// A valid DTVCC construct: the start of a packet (`cc_type` 11) or a continuation (10).
pub fn construct_dtvcc(start: bool, data: [u8; 2]) -> [u8; 3] {
    [if start { 0xFF } else { 0xFE }, data[0], data[1]]
}

/// The field whose CEA-608 construct goes with field period `k` of a picture whose first
/// field period is the stream's `abs`-th: on an interlaced output (`top_first` given) field 2
/// with a top field and field 1 with a bottom field; on a progressive one the fields in turn,
/// field 1 on even field periods. The rule of `tb_timeline::captions::CaptionCarrier::take_shown`.
pub fn field_of(abs: u64, k: u64, top_first: Option<bool>) -> Field {
    let two = match top_first {
        Some(top) => top == k.is_multiple_of(2),
        None => abs % 2 == 1,
    };
    if two { Field::Two } else { Field::One }
}

/// One construct, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Construct {
    /// `cc_valid`.
    pub valid: bool,
    /// `cc_type`, 0 to 3.
    pub ty: u8,
    /// The two data bytes.
    pub data: [u8; 2],
}

/// The constructs of a `cc_data()` (a trailing partial construct is ignored).
pub fn constructs(cc: &[u8]) -> impl Iterator<Item = Construct> + '_ {
    cc.as_chunks::<3>().0.iter().map(|c| Construct {
        valid: c[0] & 0x04 != 0,
        ty: c[0] & 0x03,
        data: [c[1], c[2]],
    })
}

/// What a source's pictures carried.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// Valid CEA-608 pairs, in order, with their field.
    pub cea608: Vec<(Field, [u8; 2])>,
    /// Caption channel packets completed by this picture (header first).
    pub dtvcc: Vec<Vec<u8>>,
}

/// Reads the `cc_data()` of a source's pictures, in display order.
#[derive(Clone, Debug, Default)]
pub struct Parser {
    reasm: Reassembler,
}

impl Parser {
    /// A parser with nothing open.
    pub fn new() -> Self {
        Self::default()
    }

    /// The reassembler's counters.
    pub fn reassembler(&self) -> &Reassembler {
        &self.reasm
    }

    /// One picture's `cc_data()`.
    pub fn picture(&mut self, cc: &[u8]) -> Parsed {
        let mut out = Parsed::default();
        for c in constructs(cc) {
            match c.ty {
                0 | 1 if c.valid => out
                    .cea608
                    .push((if c.ty == 0 { Field::One } else { Field::Two }, c.data)),
                0 | 1 => {}
                _ => self.reasm.feed(c.valid, c.ty, c.data, &mut out.dtvcc),
            }
        }
        out
    }
}

/// Configuration of a [`Merger`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeCfg {
    /// How long a service stays the source's after its last data.
    pub hold: Millis,
    /// The output's field rate.
    pub rate: FieldRate,
}

impl Default for MergeCfg {
    fn default() -> Self {
        Self {
            hold: Millis(30_000),
            rate: FieldRate::NTSC,
        }
    }
}

/// Why a merge was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    /// Our constructs are not those of the picture.
    #[error("our cc_data has {got} bytes, a picture of {fields} field periods has {want}")]
    Ours {
        /// Bytes given.
        got: usize,
        /// Bytes wanted.
        want: usize,
        /// Field periods.
        fields: u8,
    },
    /// The source's constructs (not empty) are not those of the picture.
    #[error("the source's cc_data has {got} bytes, a picture of {fields} field periods has {want}")]
    Source {
        /// Bytes given.
        got: usize,
        /// Bytes wanted.
        want: usize,
        /// Field periods.
        fields: u8,
    },
}

/// Who has a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Owner {
    /// Our generated captions.
    #[default]
    Ours,
    /// The source's.
    Source,
}

/// Merges a source's `cc_data()` and ours, picture by picture (see the module text).
#[derive(Clone, Debug)]
pub struct Merger {
    cfg: MergeCfg,
    hold: u64,
    fields: u64,
    last_608: [Option<u64>; 2],
    last_dtvcc: Option<u64>,
    dtvcc_owner: Owner,
    skip_ours: bool,
}

impl Merger {
    /// A merger where everything is ours until the source sends something.
    pub fn new(cfg: MergeCfg) -> Self {
        Self {
            cfg,
            hold: cfg.hold.to_field_periods(cfg.rate).max(0) as u64,
            fields: 0,
            last_608: [None, None],
            last_dtvcc: None,
            dtvcc_owner: Owner::Ours,
            skip_ours: false,
        }
    }

    /// The configuration.
    pub fn cfg(&self) -> &MergeCfg {
        &self.cfg
    }

    fn held(&self, last: Option<u64>) -> bool {
        last.is_some_and(|t| self.fields.saturating_sub(t) <= self.hold)
    }

    /// Who has CEA-608 field `f` now.
    pub fn owner_608(&self, f: Field) -> Owner {
        if self.held(self.last_608[f.index()]) {
            Owner::Source
        } else {
            Owner::Ours
        }
    }

    /// Who has the DTVCC channel now.
    pub fn owner_dtvcc(&self) -> Owner {
        self.dtvcc_owner
    }

    /// The constructs of one picture of `fields` field periods: `source` (the source's, laid
    /// out for this picture as `tb_timeline::captions::CaptionCarrier::take_shown` does, or
    /// empty when the source has no captions) merged with `ours` (from
    /// [`crate::Captioner::picture`] for the same picture). `out` is cleared and filled.
    pub fn merge(
        &mut self,
        source: &[u8],
        ours: &[u8],
        fields: u8,
        out: &mut Vec<u8>,
    ) -> Result<(), MergeError> {
        out.clear();
        let n = usize::from(fields.clamp(1, 3));
        let want = 3 * PER_FIELD_PERIOD * n;
        if ours.len() != want {
            return Err(MergeError::Ours {
                got: ours.len(),
                want,
                fields,
            });
        }
        if !source.is_empty() && source.len() != want {
            return Err(MergeError::Source {
                got: source.len(),
                want,
                fields,
            });
        }
        let src: Vec<Construct> = constructs(source).collect();
        let our: Vec<Construct> = constructs(ours).collect();
        // what the source sends in this picture
        for (k, c) in src.iter().enumerate() {
            let at = self.fields + (k / PER_FIELD_PERIOD) as u64;
            if k < n {
                let p = Pair::check(c.data);
                if c.valid && c.ty < 2 && p.is_ok_and(|p| !p.is_null()) {
                    self.last_608[usize::from(c.ty)] = Some(at);
                }
            } else if c.valid && c.ty >= 2 {
                self.last_dtvcc = Some(at);
            }
        }
        for k in 0..n {
            let f = if our[k].ty == 1 {
                Field::Two
            } else {
                Field::One
            };
            let take_src = !src.is_empty() && self.owner_608(f) == Owner::Source;
            let c = if take_src {
                &source[3 * k..3 * k + 3]
            } else {
                &ours[3 * k..3 * k + 3]
            };
            out.extend_from_slice(c);
        }
        let owner = if self.held(self.last_dtvcc) && !src.is_empty() {
            Owner::Source
        } else {
            Owner::Ours
        };
        if owner == Owner::Ours && self.dtvcc_owner == Owner::Source {
            self.skip_ours = true;
        }
        self.dtvcc_owner = owner;
        for k in n..our.len() {
            if owner == Owner::Source {
                out.extend_from_slice(&source[3 * k..3 * k + 3]);
                continue;
            }
            let c = our[k];
            if self.skip_ours && c.valid && c.ty == 3 {
                self.skip_ours = false;
            }
            if self.skip_ours && c.valid && c.ty == 2 {
                out.extend_from_slice(&PAD_DTVCC);
            } else {
                out.extend_from_slice(&ours[3 * k..3 * k + 3]);
            }
        }
        self.fields += n as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours(fields: usize, tag: u8) -> Vec<u8> {
        let mut v = Vec::new();
        for k in 0..fields {
            v.extend(construct_608(
                if k % 2 == 0 { Field::One } else { Field::Two },
                Some(Pair::new(tag, tag)),
            ));
        }
        v.extend(construct_dtvcc(true, [0x02, tag]));
        v.extend(construct_dtvcc(false, [tag, tag]));
        while v.len() < 30 * fields {
            v.extend(PAD_DTVCC);
        }
        v
    }

    #[test]
    fn fields_follow_the_carrier() {
        // interlaced, top first: 01, 00, 01; bottom first: 00, 01
        let f: Vec<Field> = (0..3).map(|k| field_of(0, k, Some(true))).collect();
        assert_eq!(f, [Field::Two, Field::One, Field::Two]);
        let f: Vec<Field> = (0..2).map(|k| field_of(7, k, Some(false))).collect();
        assert_eq!(f, [Field::One, Field::Two]);
        // progressive: by the absolute field period
        assert_eq!(field_of(4, 0, None), Field::One);
        assert_eq!(field_of(5, 0, None), Field::Two);
    }

    #[test]
    fn parse_a_source() {
        let mut p = Parser::new();
        let v = ours(2, 0x41);
        let got = p.picture(&v);
        assert_eq!(got.cea608.len(), 2);
        assert_eq!(got.dtvcc, [vec![0x02, 0x41, 0x41, 0x41]]);
    }

    #[test]
    fn the_source_wins_where_it_has_data() {
        let mut m = Merger::new(MergeCfg {
            hold: Millis(1001),
            rate: FieldRate::NTSC,
        });
        let mut out = Vec::new();
        // no source: ours
        m.merge(&[], &ours(2, 0x41), 2, &mut out).unwrap();
        assert_eq!(out, ours(2, 0x41));
        // a source with field 1 only and DTVCC padding: field 1 and nothing else is the source's
        let mut src = vec![];
        src.extend(construct_608(Field::One, Some(Pair::new(0x61, 0x62))));
        src.extend(PAD_608[1]);
        while src.len() < 60 {
            src.extend(PAD_DTVCC);
        }
        m.merge(&src, &ours(2, 0x41), 2, &mut out).unwrap();
        assert_eq!(out[..3], src[..3]);
        assert_eq!(out[3..], ours(2, 0x41)[3..]);
        assert_eq!(m.owner_608(Field::One), Owner::Source);
        assert_eq!(m.owner_608(Field::Two), Owner::Ours);
        // the source goes quiet: field 1 stays the source's for the hold time (60 field
        // periods), then ours again
        let quiet: Vec<u8> = [PAD_608[0], PAD_608[1]]
            .concat()
            .into_iter()
            .chain(std::iter::repeat_n(PAD_DTVCC, 18).flatten())
            .collect();
        for _ in 0..30 {
            m.merge(&quiet, &ours(2, 0x41), 2, &mut out).unwrap();
            assert_eq!(out[..3], PAD_608[0]);
        }
        m.merge(&quiet, &ours(2, 0x41), 2, &mut out).unwrap();
        assert_eq!(out[..3], ours(2, 0x41)[..3]);
        // a source with DTVCC takes the DTVCC channel
        let mut src = vec![];
        src.extend(PAD_608[0]);
        src.extend(PAD_608[1]);
        src.extend(construct_dtvcc(true, [0x41, 0x99]));
        while src.len() < 60 {
            src.extend(PAD_DTVCC);
        }
        m.merge(&src, &ours(2, 0x41), 2, &mut out).unwrap();
        assert_eq!(out[6..], src[6..]);
        assert_eq!(m.owner_dtvcc(), Owner::Source);
        // after the hold: ours, without the continuation of a packet begun meanwhile
        let mut cont = ours(2, 0x41);
        cont[6..9].copy_from_slice(&construct_dtvcc(false, [1, 2]));
        cont[9..12].copy_from_slice(&construct_dtvcc(false, [3, 4]));
        let mut flipped = 0;
        for i in 0..40 {
            m.merge(&quiet, &cont, 2, &mut out).unwrap();
            if m.owner_dtvcc() == Owner::Ours {
                flipped = i;
                break;
            }
            assert_eq!(out[6..], quiet[6..]);
        }
        assert_eq!(flipped, 30);
        assert_eq!(out[6..12], [PAD_DTVCC, PAD_DTVCC].concat());
        m.merge(&quiet, &cont, 2, &mut out).unwrap();
        assert_eq!(
            out[6..12],
            [PAD_DTVCC, PAD_DTVCC].concat(),
            "still no packet of ours"
        );
        m.merge(&quiet, &ours(2, 0x42), 2, &mut out).unwrap();
        assert_eq!(out, ours(2, 0x42));
        // wrong sizes are refused
        assert_eq!(
            m.merge(&[], &ours(2, 0), 3, &mut out),
            Err(MergeError::Ours {
                got: 60,
                want: 90,
                fields: 3
            })
        );
    }
}
