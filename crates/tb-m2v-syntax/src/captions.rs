//! Caption user data: what a picture's `cc_data()` must look like (CTA-708-E §4.3, Table 2;
//! A/53 Part 4 §6.2.3.1).

/// cc_count of a frame picture shown for one frame period at this frame_rate_code: the caption
/// channel carries 9600 bit/s, two bytes per construct, so `cc_count = 9600 x t / 16` with `t`
/// the presentation time, "rounded to the nearest whole value" (CTA-708-E §4.3.6, p.15-16;
/// Table 4: 25 at 24 Hz, 20 at 30 Hz, 10 at 60 Hz).
pub fn cc_count(frame_rate_code: u8) -> usize {
    match frame_rate_code {
        1 | 2 => 25,
        3 => 24,
        6 => 12,
        7 | 8 => 10,
        _ => 20,
    }
}

/// CEA-608 constructs a frame picture may start its `cc_data()` with: "CEA-608 data bytes shall
/// only occur in the first one, two or three byte-pair positions, depending on frame rate"
/// (CTA-708-E §4.3.1 rule 2, p.11): one per field period of 1/59.94 s that the picture is
/// shown for (one at 59.94 and 60 Hz, two at 29.97 and 30 Hz, three at 23.976 to 25 Hz, where
/// a frame lasts two and a half field periods; two at 50 Hz).
pub fn cea608_positions(frame_rate_code: u8) -> usize {
    match frame_rate_code {
        1..=3 => 3,
        7 | 8 => 1,
        _ => 2,
    }
}

/// Why the caption bytes of a picture are refused. Nothing is repaired: the caller built a
/// `cc_data()` that a receiver would misread, and should know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptionError {
    /// Not a whole number of 3-byte constructs.
    #[error(
        "caption data of {0} bytes: cc_data() is made of 3-byte constructs (CTA-708-E §4.3, \
         Table 2, p.10)"
    )]
    Length(usize),
    /// Not the cc_count of the frame rate and the picture's display time.
    #[error(
        "{got} caption constructs in a picture: for the time this picture is shown cc_count is \
         {want} (9600 bit/s over the time the picture is shown; CTA-708-E §4.3.6, p.15-16, \
         Table 4)"
    )]
    Count {
        /// Constructs given.
        got: usize,
        /// cc_count of the session's frame rate.
        want: usize,
    },
    /// one_bit or the reserved bits of a construct are not '11111'.
    #[error(
        "caption construct {index} starts with byte {byte:#04x}: one_bit and the four reserved \
         bits shall be '1' (CTA-708-E §4.3, Table 2, p.10); with them no user data can hold 23 \
         zero bits in a row (H.262 §6.3.4.1, p.40)"
    )]
    MarkerBits {
        /// Index of the construct.
        index: usize,
        /// Its first byte.
        byte: u8,
    },
    /// A valid CEA-608 construct after a DTVCC construct.
    #[error(
        "caption construct {0} is valid CEA-608 data (cc_type 00 or 01) after a DTVCC construct \
         (cc_type 10 or 11): not a transition of CTA-708-E §4.3.1 Figure 2 (rule 6, p.11)"
    )]
    Cea608AfterDtvcc(usize),
    /// More leading CEA-608 constructs than the picture has positions for.
    #[error(
        "{got} constructs of cc_type 00 or 01 before the first DTVCC construct: at this frame \
         rate CEA-608 data occupies at most the first {max} positions, at most two per field, \
         and a construct of cc_type 10 or 11 ends it (CTA-708-E §4.3.1 rule 2, p.11; Annex C.2)"
    )]
    Cea608Positions {
        /// Leading constructs of cc_type 00 or 01.
        got: usize,
        /// Positions of the frame rate.
        max: usize,
    },
    /// CEA-608 data of one field and no construct of the other.
    #[error(
        "CEA-608 data is present but there is no construct of cc_type {0:02b}: encoding \
         equipment shall always include a construct for each field, with cc_valid 0 if it has \
         no data (CTA-708-E §4.3.2.2.1, p.14)"
    )]
    Cea608FieldMissing(u8),
}

/// Check the caption bytes of one frame picture: the `cc_data()` constructs (3 bytes each:
/// marker bits with cc_valid and cc_type, cc_data_1, cc_data_2) that [`crate::headers`] wraps
/// in picture user data. Empty: the picture carries no captions, which is always accepted.
///
/// The rules, all from CTA-708-E §4.3 (the transport A/53 Part 4 §6.2.3.1 and SCTE 43 §7.2.1
/// prescribe): whole constructs; exactly [`cc_count`] of them; one_bit and the reserved bits
/// '11111' in each; no valid CEA-608 construct after a DTVCC construct; the leading CEA-608
/// constructs within [`cea608_positions`], at most two per field; where CEA-608 data is
/// present, a construct for each field (with cc_valid 0 for a field without data).
///
/// Start code emulation (H.262 §6.3.4.1, p.40: user data shall not contain 23 or more
/// consecutive zero bits) needs no escaping once the marker bits hold: the user data is
/// 'GA94', 0x03, a byte with its two top bits set, 0xFF, the constructs, 0xFF. The longest run
/// of zero bits is the three low bits of a construct's first byte and its two data bytes: 19.
pub fn validate(cc: &[u8], frame_rate_code: u8) -> Result<(), CaptionError> {
    validate_shown(cc, frame_rate_code, 1, 1)
}

/// cc_count and CEA-608 positions of a picture shown for `units / per_frame` frame periods at
/// this frame_rate_code: `units` fields of an interlaced sequence (`per_frame` 2; three with
/// repeat_first_field) or `units` frame periods of a progressive one (`per_frame` 1; two or
/// three with repeat_first_field). The count is "set based on the difference between the
/// presentation time of the picture" and its neighbor's (CTA-708-E §4.3.6, p.15-16): 9600
/// bit/s over the time the picture is shown, so 30 for three fields at 29.97 Hz (Table 4,
/// "30i with 3:2 pull-down: alternating between 20 and 30"; Annex C.1: "30 data pairs in this
/// cc_data() structure, as required for this picture's header values") and 20 or 30 for a
/// frame shown twice or three times at 59.94 Hz. CEA-608 pairs: one per field period shown,
/// three at most (§4.3.1 rule 2, p.11: "the first one, two or three byte-pair positions").
pub fn shown_counts(frame_rate_code: u8, units: u8, per_frame: u8) -> (usize, usize) {
    let (u, f) = (usize::from(units.max(1)), usize::from(per_frame.max(1)));
    (
        (cc_count(frame_rate_code) * u / f).min(31),
        (cea608_positions(frame_rate_code) * u / f).clamp(1, 3),
    )
}

/// [`validate`] for a picture shown for `units / per_frame` frame periods ([`shown_counts`]).
pub fn validate_shown(
    cc: &[u8],
    frame_rate_code: u8,
    units: u8,
    per_frame: u8,
) -> Result<(), CaptionError> {
    if cc.is_empty() {
        return Ok(());
    }
    if !cc.len().is_multiple_of(3) {
        return Err(CaptionError::Length(cc.len()));
    }
    let (want, max) = shown_counts(frame_rate_code, units, per_frame);
    let got = cc.len() / 3;
    if got != want {
        return Err(CaptionError::Count { got, want });
    }
    let (mut dtvcc, mut lead, mut per_field, mut valid) = (false, 0usize, [0usize; 2], [false; 2]);
    let (constructs, _) = cc.as_chunks::<3>();
    for (index, c) in constructs.iter().enumerate() {
        let byte = c[0];
        if byte & 0xF8 != 0xF8 {
            return Err(CaptionError::MarkerBits { index, byte });
        }
        let (is_valid, ty) = (byte & 0x04 != 0, usize::from(byte & 0x03));
        if ty >= 2 {
            dtvcc = true;
        } else if dtvcc {
            if is_valid {
                return Err(CaptionError::Cea608AfterDtvcc(index));
            }
        } else {
            lead += 1;
            per_field[ty] += 1;
            valid[ty] |= is_valid;
        }
    }
    // Every construct of cc_type 00 or 01 before the first DTVCC construct is a CEA-608
    // position, valid or not: the CEA-608 part of cc_data() ends with a construct of cc_type 10
    // or 11 (CTA-708-E Annex C.2: "at least one data pair with cc_type = 10 or 11 is required
    // to end the CEA-608 datastream"), and padding comes after it.
    if lead > max || per_field[0] > 2 || per_field[1] > 2 {
        return Err(CaptionError::Cea608Positions { got: lead, max });
    }
    // "When CEA-608 data is present in a stream, encoding equipment shall always include a
    // cc_data() structure for each field, even if a decoder is being instructed not to generate
    // the run-in clock and start bit on one field" (4.3.2.2.1). Frames faster than 30/1.001 Hz
    // may carry fewer (same clause); a 60 Hz frame has one position.
    if max >= 2 && frame_rate_code != 5 && (valid[0] || valid[1]) {
        for ty in 0..2u8 {
            if per_field[usize::from(ty)] == 0 {
                return Err(CaptionError::Cea608FieldMissing(ty));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::frame_rate_value;

    /// `n` constructs: the given leading ones, an end-of-packet construct, padding.
    fn cc(lead: &[[u8; 3]], n: usize) -> Vec<u8> {
        let mut v: Vec<u8> = lead.iter().flatten().copied().collect();
        while v.len() < 3 * n {
            v.extend([0xFA, 0, 0]);
        }
        v
    }

    const F1: [u8; 3] = [0xFC, 0x80, 0x80];
    const F2: [u8; 3] = [0xFD, 0x80, 0x80];

    #[test]
    fn accepted_shapes() {
        assert_eq!(validate(&[], 4), Ok(()));
        // three fields at 29.97 Hz: 30 constructs, three CEA-608 positions (Annex C.1, C.3)
        assert_eq!(validate_shown(&cc(&[F2, F1, F2], 30), 4, 3, 2), Ok(()));
        assert_eq!(validate_shown(&cc(&[F1, F2, F1], 30), 4, 3, 2), Ok(()));
        assert_eq!(
            validate_shown(&cc(&[F1, F2], 20), 4, 3, 2),
            Err(CaptionError::Count { got: 20, want: 30 })
        );
        assert_eq!(
            validate_shown(&cc(&[F1, F2, F1], 20), 4, 2, 2),
            Err(CaptionError::Cea608Positions { got: 3, max: 2 })
        );
        // a frame shown two and three times at 59.94 Hz
        assert_eq!(validate_shown(&cc(&[F1, F2], 20), 7, 2, 1), Ok(()));
        assert_eq!(validate_shown(&cc(&[F2, F1, F2], 30), 7, 3, 1), Ok(()));
        assert_eq!(shown_counts(7, 1, 1), (10, 1));
        assert_eq!(shown_counts(4, 2, 2), (20, 2));
        assert_eq!(shown_counts(1, 1, 1), (25, 3));
        // CTA-708-E Annex C: both orders of the fields, three pairs at 24 Hz, one at 60 Hz
        assert_eq!(validate(&cc(&[F1, F2], 20), 4), Ok(()));
        assert_eq!(validate(&cc(&[F2, F1], 20), 5), Ok(()));
        assert_eq!(validate(&cc(&[F2, F1, F2], 25), 1), Ok(()));
        assert_eq!(validate(&cc(&[F1], 10), 7), Ok(()));
        assert_eq!(validate(&cc(&[F2], 10), 8), Ok(()));
        assert_eq!(validate(&cc(&[], 20), 4), Ok(()));
        // a caption channel packet, then padding of cc_type 00 (Annex C.1)
        let mut v = cc(&[F1, F2, [0xFF, 0x05, 0x27], [0xFE, 0x8A, 0xFF]], 20);
        for c in v.as_chunks_mut::<3>().0.iter_mut().skip(4) {
            c.copy_from_slice(&[0xF8, 0, 0]);
        }
        assert_eq!(validate(&v, 4), Ok(()));
    }

    #[test]
    fn refusals() {
        assert_eq!(validate(&[0xFC, 0x80], 4), Err(CaptionError::Length(2)));
        assert_eq!(
            validate(&cc(&[F1, F2], 20)[..59], 4),
            Err(CaptionError::Length(59))
        );
        for (n, code, want) in [
            (1, 4, 20),
            (10, 4, 20),
            (20, 7, 10),
            (20, 1, 25),
            (31, 4, 20),
        ] {
            assert_eq!(
                validate(&cc(&[], n), code),
                Err(CaptionError::Count { got: n, want }),
                "{n} at {code}"
            );
        }
        // marker bits: all-zero constructs would be 24 zero bits each
        let mut v = cc(&[F1, F2], 20);
        v[9] = 0x02;
        assert_eq!(
            validate(&v, 4),
            Err(CaptionError::MarkerBits {
                index: 3,
                byte: 0x02
            })
        );
        assert_eq!(
            validate(&[0u8; 60], 4),
            Err(CaptionError::MarkerBits { index: 0, byte: 0 })
        );
        // a valid CEA-608 construct after DTVCC data
        let v = cc(&[F1, [0xFF, 1, 2], F2], 20);
        assert_eq!(validate(&v, 4), Err(CaptionError::Cea608AfterDtvcc(2)));
        // three CEA-608 constructs in a 30 Hz frame; two in a 60 Hz frame; three of one field
        assert_eq!(
            validate(&cc(&[F1, F2, F1], 20), 4),
            Err(CaptionError::Cea608Positions { got: 3, max: 2 })
        );
        assert_eq!(
            validate(&cc(&[F1, F2], 10), 7),
            Err(CaptionError::Cea608Positions { got: 2, max: 1 })
        );
        // constructs of cc_type 00 as padding with nothing of cc_type 10 or 11 before them
        let mut v = cc(&[F1, F2], 20);
        for c in v.as_chunks_mut::<3>().0.iter_mut().skip(2) {
            c.copy_from_slice(&[0xF8, 0, 0]);
        }
        assert_eq!(
            validate(&v, 4),
            Err(CaptionError::Cea608Positions { got: 20, max: 2 })
        );
        // data of one field and no construct of the other (4.3.2.2.1); with a construct of
        // cc_valid 0 for the other field it is the prescribed form
        assert_eq!(
            validate(&cc(&[F2], 20), 4),
            Err(CaptionError::Cea608FieldMissing(0))
        );
        assert_eq!(
            validate(&cc(&[F1], 20), 4),
            Err(CaptionError::Cea608FieldMissing(1))
        );
        assert_eq!(validate(&cc(&[[0xF8, 0x80, 0x80], F2], 20), 4), Ok(()));
        assert_eq!(validate(&cc(&[F1, [0xF9, 0x80, 0x80]], 20), 4), Ok(()));
        // exactly 30 Hz: fewer constructs may occur
        assert_eq!(validate(&cc(&[F1], 20), 5), Ok(()));
    }

    /// H.262 6.3.4.1: no 23 zero bits in a row in the user data, whatever the data bytes of an
    /// accepted cc_data() are. The worst case: every construct with cc_valid 0, cc_type 00 where
    /// that is allowed, and zero data bytes.
    #[test]
    fn accepted_captions_cannot_emulate_a_start_code() {
        use crate::PicType;
        use crate::headers::{PicInfo, SeqInfo, write_headers};
        use crate::profile::{Profile, SeqConfig, check};
        for (code, fps) in [(1u8, (24000, 1001)), (4, (30000, 1001)), (7, (60000, 1001))] {
            let n = cc_count(code);
            let lead = cea608_positions(code).min(2);
            let mut v = Vec::new();
            for i in 0..n {
                // CEA-608 positions not valid, then the end construct, then padding of
                // cc_type 00 with zero data
                let first = if i < lead {
                    0xF8 | (i as u8 & 1)
                } else if i == lead {
                    0xFA
                } else {
                    0xF8
                };
                v.extend([first, 0, 0]);
            }
            assert_eq!(validate(&v, code), Ok(()), "{code}");
            let cfg = SeqConfig {
                width: 704,
                height: 480,
                fps_n: fps.0,
                fps_d: fps.1,
                interlaced: false,
                aspect: 0,
                max_bitrate: 4_000_000,
                vbv_bits: 1_835_008,
                color: None,
            };
            let seq = SeqInfo {
                width: 704,
                height: 480,
                interlaced: false,
                par: check(&cfg, Profile::H262).unwrap(),
            };
            let pic = PicInfo {
                pic_type: PicType::P,
                disp: 1,
                time: 1,
                tref_base: 0,
                f_code: 3,
                tff: false,
                rff: false,
                frame_pred_frame_dct: true,
                intra_vlc_format: true,
                alt_scan: false,
                progressive_frame: true,
            };
            let mut out = Vec::new();
            write_headers(&mut out, &seq, &pic, false, false, &v);
            let at = out.windows(4).position(|w| w == [0, 0, 1, 0xB2]).unwrap();
            let ud = &out[at + 4..];
            let (mut run, mut worst) = (0, 0);
            for byte in ud {
                for bit in (0..8).rev() {
                    if byte >> bit & 1 == 0 {
                        run += 1;
                        worst = worst.max(run);
                    } else {
                        run = 0;
                    }
                }
            }
            assert_eq!(worst, 19, "frame_rate_code {code}");
            assert_eq!(run, 0, "the user data ends with a one bit");
        }
    }

    /// 600 / frame rate, rounded.
    #[test]
    fn counts_follow_the_frame_period() {
        for code in 1..=8u8 {
            let (n, d) = frame_rate_value(code).unwrap();
            let want = (600.0 * f64::from(d) / f64::from(n)).round() as usize;
            assert_eq!(cc_count(code), want, "frame_rate_code {code}");
        }
        assert_eq!([1, 2, 4, 5, 7, 8].map(cc_count), [25, 25, 20, 20, 10, 10]);
    }
}
