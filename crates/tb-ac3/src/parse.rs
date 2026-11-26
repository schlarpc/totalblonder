//! A strict AC-3 syncframe parser: syncinfo, bsi, both CRCs, and (for the subset this encoder
//! writes: no coupling, no delta bit allocation, no 1+1) every audio block down to the
//! mantissas.
//!
//! It is the encoder's first check: every test runs the encoder's frames through it. It also
//! serves the server, which can read a frame's dialnorm, layout and rate from [`parse_header`].
//! The full parse ([`parse_frame`]) runs the bit allocation of [`crate::bitalloc`] as a decoder
//! does, so it is not independent of the encoder on that point; FFmpeg's and liba52's decoders
//! are (`tests/external.rs`).
//!
//! The CRC check ([`check_crcs`]) is the bitwise LFSR of A/52 §7.10.1, independent of the
//! table-driven code the encoder uses.

use crate::bitalloc::{
    self, DbPerBitCode, FastDecayCode, FastGainCode, FloorCode, Params, SlowDecayCode,
    SlowGainCode, SnrOffset,
};
use crate::bits::{BitReader, OutOfData};
use crate::crc;
use crate::exponents::{self, ExpStrategy};
use crate::quantize::mantissa_value;
use crate::tables::{
    BIT_RATES_KBPS, LFE_END_MANT, N_COEFS, SYNCWORD, WORDS_PER_FRAME_48K, words_five_eighths,
};

/// Why a frame was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The data ended early.
    #[error(transparent)]
    OutOfData(#[from] OutOfData),
    /// The first 16 bits are not 0x0B77.
    #[error("no syncword")]
    NoSync,
    /// fscod is not 48 kHz (the only rate this parser knows the frame sizes of).
    #[error("fscod {0} is not 48 kHz")]
    SampleRate(u32),
    /// frmsizecod above 37.
    #[error("frmsizecod {0} is reserved")]
    FrameSizeCode(u32),
    /// The buffer is not exactly one frame long.
    #[error("buffer is {got} bytes, frame is {expected}")]
    Length {
        /// Bytes the frame size code says.
        expected: usize,
        /// Bytes given.
        got: usize,
    },
    /// The register is not zero at the 5/8 point (A/52 §7.10.1).
    #[error("crc1 check failed")]
    Crc1,
    /// The register is not zero at the end of the frame (A/52 §7.10.1).
    #[error("crc2 check failed")]
    Crc2,
    /// A field holds a value the syntax forbids or this parser does not support.
    #[error("block {block}: {what}")]
    Invalid {
        /// Audio block, or 6 for the frame-level fields.
        block: usize,
        /// What was wrong.
        what: &'static str,
    },
}

fn invalid(block: usize, what: &'static str) -> ParseError {
    ParseError::Invalid { block, what }
}

/// syncinfo (A/52 Table 5.1, p.30).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncInfo {
    /// crc1.
    pub crc1: u16,
    /// fscod.
    pub fscod: u32,
    /// frmsizecod.
    pub frmsizecod: u32,
}

impl SyncInfo {
    /// Nominal bit rate, kbit/s (A/52 Table 5.18, p.51).
    pub fn kbps(&self) -> u32 {
        BIT_RATES_KBPS[(self.frmsizecod >> 1) as usize]
    }

    /// Frame length in bytes at 48 kHz.
    pub fn frame_bytes(&self) -> usize {
        WORDS_PER_FRAME_48K[(self.frmsizecod >> 1) as usize] as usize * 2
    }

    /// Bytes in the first 5/8 of the frame (A/52 §7.10.1, p.104).
    pub fn five_eighths_bytes(&self) -> usize {
        words_five_eighths(WORDS_PER_FRAME_48K[(self.frmsizecod >> 1) as usize]) as usize * 2
    }
}

/// bsi (A/52 Table 5.2, p.31–32), main-service fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bsi {
    /// bsid.
    pub bsid: u32,
    /// bsmod.
    pub bsmod: u32,
    /// acmod.
    pub acmod: u32,
    /// cmixlev, when present.
    pub cmixlev: Option<u32>,
    /// surmixlev, when present.
    pub surmixlev: Option<u32>,
    /// dsurmod, when present.
    pub dsurmod: Option<u32>,
    /// lfeon.
    pub lfeon: bool,
    /// dialnorm.
    pub dialnorm: u32,
    /// compr, when present.
    pub compr: Option<u32>,
    /// langcod, when present.
    pub langcod: Option<u32>,
    /// (mixlevel, roomtyp), when present.
    pub audio_production: Option<(u32, u32)>,
    /// copyrightb.
    pub copyright: bool,
    /// origbs.
    pub original: bool,
    /// timecod1, when present.
    pub timecod1: Option<u32>,
    /// timecod2, when present.
    pub timecod2: Option<u32>,
    /// Bytes of addbsi.
    pub addbsi_bytes: usize,
}

impl Bsi {
    /// nfchans (A/52 Table 5.8, p.39).
    pub fn nfchans(&self) -> usize {
        [2, 1, 2, 3, 3, 4, 4, 5][self.acmod as usize & 7]
    }

    /// nfchans plus LFE.
    pub fn channels(&self) -> usize {
        self.nfchans() + usize::from(self.lfeon)
    }
}

/// syncinfo and bsi of a frame, and where the bsi ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// syncinfo.
    pub syncinfo: SyncInfo,
    /// bsi.
    pub bsi: Bsi,
    /// Bit position of the first audio block.
    pub bits: usize,
}

fn parse_header_from(r: &mut BitReader<'_>) -> Result<Header, ParseError> {
    if r.get(16)? != u32::from(SYNCWORD) {
        return Err(ParseError::NoSync);
    }
    let crc1 = r.get(16)? as u16;
    let fscod = r.get(2)?;
    let frmsizecod = r.get(6)?;
    if fscod != 0 {
        return Err(ParseError::SampleRate(fscod));
    }
    if frmsizecod > 37 {
        return Err(ParseError::FrameSizeCode(frmsizecod));
    }
    let bsid = r.get(5)?;
    let bsmod = r.get(3)?;
    let acmod = r.get(3)?;
    let cmixlev = if (acmod & 1) != 0 && acmod != 1 {
        Some(r.get(2)?)
    } else {
        None
    };
    let surmixlev = if (acmod & 4) != 0 {
        Some(r.get(2)?)
    } else {
        None
    };
    let dsurmod = if acmod == 2 { Some(r.get(2)?) } else { None };
    let lfeon = r.flag()?;
    let dialnorm = r.get(5)?;
    let compr = if r.flag()? { Some(r.get(8)?) } else { None };
    let langcod = if r.flag()? { Some(r.get(8)?) } else { None };
    let audio_production = if r.flag()? {
        Some((r.get(5)?, r.get(2)?))
    } else {
        None
    };
    if acmod == 0 {
        // 1+1: dialnorm2, compr2, langcod2, audprodi2 (Table 5.2). Parsed and dropped.
        r.get(5)?;
        if r.flag()? {
            r.get(8)?;
        }
        if r.flag()? {
            r.get(8)?;
        }
        if r.flag()? {
            r.get(7)?;
        }
    }
    let copyright = r.flag()?;
    let original = r.flag()?;
    let timecod1 = if r.flag()? { Some(r.get(14)?) } else { None };
    let timecod2 = if r.flag()? { Some(r.get(14)?) } else { None };
    let addbsi_bytes = if r.flag()? {
        let n = r.get(6)? as usize + 1;
        r.skip(n * 8)?;
        n
    } else {
        0
    };
    Ok(Header {
        syncinfo: SyncInfo {
            crc1,
            fscod,
            frmsizecod,
        },
        bsi: Bsi {
            bsid,
            bsmod,
            acmod,
            cmixlev,
            surmixlev,
            dsurmod,
            lfeon,
            dialnorm,
            compr,
            langcod,
            audio_production,
            copyright,
            original,
            timecod1,
            timecod2,
            addbsi_bytes,
        },
        bits: r.position(),
    })
}

/// Parses syncinfo and bsi. Does not check the CRCs or the length.
pub fn parse_header(frame: &[u8]) -> Result<Header, ParseError> {
    parse_header_from(&mut BitReader::new(frame))
}

/// Checks the length and both CRCs of a frame with the decoder's LFSR (A/52 §7.10.1,
/// p.103–104): the register over the frame after the syncword is zero at the 5/8 point and at
/// the end.
pub fn check_crcs(frame: &[u8]) -> Result<(), ParseError> {
    let h = parse_header(frame)?;
    let expected = h.syncinfo.frame_bytes();
    if frame.len() != expected {
        return Err(ParseError::Length {
            expected,
            got: frame.len(),
        });
    }
    let p58 = h.syncinfo.five_eighths_bytes();
    if crc::lfsr_register(&frame[2..p58]) != 0 {
        return Err(ParseError::Crc1);
    }
    if crc::lfsr_register(&frame[2..]) != 0 {
        return Err(ParseError::Crc2);
    }
    Ok(())
}

/// What one audio block contained.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// Bit position where the block starts.
    pub start_bit: usize,
    /// Bit position of the first mantissa.
    pub mantissa_bit: usize,
    /// Bit position after the last mantissa.
    pub end_bit: usize,
    /// blksw per full-bandwidth channel.
    pub blksw: Vec<bool>,
    /// dithflag per full-bandwidth channel.
    pub dithflag: Vec<bool>,
    /// chexpstr per full-bandwidth channel, then lfeexpstr (0 or 1).
    pub expstr: Vec<u32>,
    /// rematflg in effect (2/0).
    pub rematflg: [bool; 4],
    /// csnroffst and channel 0's fsnroffst, if sent in this block.
    pub snr: Option<SnrOffset>,
    /// skipl, if present.
    pub skip: Option<u32>,
    /// Dequantized coefficients per channel (dither and rematrixing not applied).
    pub coefs: Vec<[f32; N_COEFS]>,
    /// Exponents in effect per channel.
    pub exps: Vec<[u8; N_COEFS]>,
    /// bap in effect per channel.
    pub bap: Vec<[u8; N_COEFS]>,
    /// One past the last coded bin per channel.
    pub end: Vec<usize>,
}

/// A frame, parsed to the end.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// syncinfo and bsi.
    pub header: Header,
    /// The six audio blocks.
    pub blocks: Vec<Block>,
    /// nauxbits (A/52 §5.4.4.1, p.51).
    pub aux_bits: usize,
    /// auxdatal if auxdatae is set.
    pub auxdatal: Option<u32>,
    /// crcrsv.
    pub crcrsv: bool,
    /// The SNR offset of block 0 (csnroffst, and fsnroffst of channel 0).
    pub snr: SnrOffset,
}

/// Mantissa ungrouping state, shared across the channels of a block (A/52 §7.3.5, p.79–80).
#[derive(Debug, Default)]
struct Ungroup {
    buf: [[u32; 3]; 3],
    left: [usize; 3],
}

impl Ungroup {
    fn next(&mut self, r: &mut BitReader<'_>, bap: u8, block: usize) -> Result<u32, ParseError> {
        let (gi, n, levels, width, max) = match bap {
            1 => (0, 3, 3, 5, 26),
            2 => (1, 3, 5, 7, 124),
            _ => (2, 2, 11, 7, 120),
        };
        if self.left[gi] == 0 {
            let g = r.get(width)?;
            // A/52 §7.10.2 conditions 18–20 (p.107).
            if g > max {
                return Err(invalid(block, "grouped mantissa out of range"));
            }
            if n == 3 {
                self.buf[gi] = [
                    g / (levels * levels),
                    (g % (levels * levels)) / levels,
                    g % levels,
                ];
            } else {
                self.buf[gi] = [g / levels, g % levels, 0];
            }
            self.left[gi] = n;
        }
        let v = self.buf[gi][n - self.left[gi]];
        self.left[gi] -= 1;
        Ok(v)
    }
}

/// Parses a whole frame: header, CRCs, all six audio blocks with their mantissas, aux and
/// errorcheck, and checks the §5.5 limits and the §7.10.2 consistency conditions that apply.
pub fn parse_frame(frame: &[u8]) -> Result<Frame, ParseError> {
    check_crcs(frame)?;
    let mut r = BitReader::new(frame);
    let header = parse_header_from(&mut r)?;
    let bsi = header.bsi;
    if bsi.bsid != 8 {
        return Err(invalid(6, "bsid is not 8"));
    }
    if bsi.acmod == 0 {
        return Err(invalid(6, "1+1 mode is not supported"));
    }
    let nf = bsi.nfchans();
    let nch = bsi.channels();
    let frame_bits = frame.len() * 8;
    let p58 = header.syncinfo.five_eighths_bytes() * 8;

    let mut exps = vec![[0u8; N_COEFS]; nch];
    let mut bap = vec![[0u8; N_COEFS]; nch];
    let mut end = vec![0usize; nch];
    if bsi.lfeon {
        end[nf] = LFE_END_MANT;
    }
    let mut params: Option<Params> = None;
    let mut csnr = 0u8;
    let mut fsnr = vec![0u8; nch];
    let mut fgain = vec![FastGainCode::new(0); nch];
    let mut rematflg = [false; 4];
    let mut blocks = Vec::with_capacity(6);
    let mut first_snr = SnrOffset::default();

    for blk in 0..6 {
        let start_bit = r.position();
        let blksw: Vec<bool> = (0..nf).map(|_| r.flag()).collect::<Result<_, _>>()?;
        let dithflag: Vec<bool> = (0..nf).map(|_| r.flag()).collect::<Result<_, _>>()?;
        if r.flag()? {
            r.get(8)?; // dynrng
        }
        let cplstre = r.flag()?;
        if blk == 0 && !cplstre {
            return Err(invalid(blk, "cplstre = 0 in block 0"));
        }
        if cplstre && r.flag()? {
            return Err(invalid(blk, "coupling is not supported"));
        }
        let mut new_alloc = false;
        if bsi.acmod == 2 {
            let rematstr = r.flag()?;
            if blk == 0 && !rematstr {
                return Err(invalid(blk, "rematstr = 0 in block 0"));
            }
            if rematstr {
                for f in &mut rematflg {
                    *f = r.flag()?;
                }
            }
        }
        let mut expstr: Vec<u32> = (0..nf).map(|_| r.get(2)).collect::<Result<_, _>>()?;
        if bsi.lfeon {
            expstr.push(r.get(1)?);
        }
        if blk == 0 && expstr.contains(&0) {
            return Err(invalid(blk, "exponent reuse in block 0"));
        }
        for ch in 0..nf {
            if expstr[ch] != 0 {
                let bw = r.get(6)?;
                if bw > 60 {
                    return Err(invalid(blk, "chbwcod above 60"));
                }
                // chbwcod comes only with new exponents, so the bandwidth cannot change under
                // reused exponents (§7.10.2 condition 9).
                end[ch] = (bw as usize + 12) * 3 + 37;
            }
        }
        let mut groups = [0u8; 84];
        for ch in 0..nch {
            let is_lfe = ch == nf;
            let st = if is_lfe {
                if expstr[ch] == 0 {
                    None
                } else {
                    Some(ExpStrategy::D15)
                }
            } else {
                ExpStrategy::from_code(expstr[ch])
            };
            if let Some(st) = st {
                new_alloc = true;
                let abs = r.get(4)? as u8;
                let n = exponents::n_groups(end[ch], st);
                for g in &mut groups[..n] {
                    *g = r.get(7)? as u8;
                }
                if exponents::decode(abs, &groups[..n], st, &mut exps[ch]).is_none() {
                    return Err(invalid(blk, "exponent out of range"));
                }
                if !is_lfe {
                    r.get(2)?; // gainrng
                }
            }
        }
        let baie = r.flag()?;
        if blk == 0 && !baie {
            return Err(invalid(blk, "baie = 0 in block 0"));
        }
        if baie {
            new_alloc = true;
            // The field widths make every code valid; `new` cannot fail here.
            let bad = || invalid(blk, "bit allocation code");
            let sdcycod = SlowDecayCode::new(r.get(2)? as u8).ok_or_else(bad)?;
            let fdcycod = FastDecayCode::new(r.get(2)? as u8).ok_or_else(bad)?;
            let sgaincod = SlowGainCode::new(r.get(2)? as u8).ok_or_else(bad)?;
            let dbpbcod = DbPerBitCode::new(r.get(2)? as u8).ok_or_else(bad)?;
            let floorcod = FloorCode::new(r.get(3)? as u8).ok_or_else(bad)?;
            params = Some(Params {
                sdcycod,
                fdcycod,
                sgaincod,
                dbpbcod,
                floorcod,
            });
        }
        let snroffste = r.flag()?;
        if blk == 0 && !snroffste {
            return Err(invalid(blk, "snroffste = 0 in block 0"));
        }
        let mut block_snr = None;
        if snroffste {
            new_alloc = true;
            csnr = r.get(6)? as u8;
            for ch in 0..nch {
                fsnr[ch] = r.get(4)? as u8;
                fgain[ch] = FastGainCode::new(r.get(3)? as u8);
            }
            let snr = SnrOffset {
                coarse: csnr,
                fine: fsnr[0],
            };
            if blk == 0 {
                first_snr = snr;
            }
            block_snr = Some(snr);
        }
        if r.flag()? {
            return Err(invalid(blk, "delta bit allocation is not supported"));
        }
        let skip = if r.flag()? {
            let n = r.get(9)?;
            r.skip(n as usize * 8)?;
            Some(n)
        } else {
            None
        };
        if new_alloc {
            let Some(p) = params else {
                return Err(invalid(blk, "bit allocation parameters missing"));
            };
            let zero = csnr == 0 && fsnr.iter().all(|&f| f == 0);
            for ch in 0..nch {
                let Some(fg) = fgain[ch] else {
                    return Err(invalid(blk, "fast gain code missing"));
                };
                let snr = SnrOffset {
                    coarse: csnr,
                    fine: fsnr[ch],
                };
                bitalloc::decoder_bap(&exps[ch], end[ch], &p, fg, snr, zero, &mut bap[ch]);
            }
        }
        // Mantissas.
        let mantissa_bit = r.position();
        let mut ungroup = Ungroup::default();
        let mut coefs = vec![[0.0f32; N_COEFS]; nch];
        for ch in 0..nch {
            for bin in 0..end[ch] {
                let b = bap[ch][bin];
                let code = match b {
                    0 => continue,
                    1 | 2 | 4 => ungroup.next(&mut r, b, blk)?,
                    3 => r.get(3)?,
                    5 => r.get(4)?,
                    6..=13 => r.get(u32::from(b) - 1)?,
                    14 => r.get(14)?,
                    _ => r.get(16)?,
                };
                if (b == 3 && code > 6) || (b == 5 && code > 14) {
                    return Err(invalid(blk, "mantissa code out of range"));
                }
                let scale = (-(i32::from(exps[ch][bin]))) as f32;
                coefs[ch][bin] = mantissa_value(b, code) * scale.exp2();
            }
        }
        let end_bit = r.position();
        blocks.push(Block {
            start_bit,
            mantissa_bit,
            end_bit,
            blksw,
            dithflag,
            expstr,
            rematflg,
            skip,
            snr: block_snr,
            coefs,
            exps: exps.clone(),
            bap: bap.clone(),
            end: end.clone(),
        });
    }

    // §5.5 (p.52): blocks 0 and 1 within the first 5/8; block 5's mantissas within the last 3/8.
    if blocks[1].end_bit > p58 {
        return Err(invalid(1, "blocks 0 and 1 extend past the 5/8 point"));
    }
    if blocks[5].mantissa_bit < p58 {
        return Err(invalid(5, "block 5 mantissas start before the 5/8 point"));
    }
    let used = r.position();
    if used + 18 > frame_bits {
        return Err(invalid(6, "audio blocks run into the errorcheck"));
    }
    let mut tail = BitReader::new(frame);
    tail.skip(frame_bits - 18)?;
    let auxdatae = tail.flag()?;
    let crcrsv = tail.flag()?;
    let auxdatal = if auxdatae {
        let mut l = BitReader::new(frame);
        l.skip(frame_bits - 18 - 14)?;
        Some(l.get(14)?)
    } else {
        None
    };
    let aux_bits = (frame_bits - 18 - auxdatal.map_or(0, |_| 14)).saturating_sub(used);
    Ok(Frame {
        header,
        blocks,
        aux_bits,
        auxdatal,
        crcrsv,
        snr: first_snr,
    })
}
