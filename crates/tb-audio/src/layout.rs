//! Channel layouts and the mapping from a decoder's layout to the layouts we encode.
//!
//! Output layouts are the two of SPEC §8.3: stereo (A/52 `acmod` 2, 2/0) and 5.1 (`acmod` 7,
//! 3/2, with `lfeon`). Output planes are in A/52 channel order, A/52:2012 §5.4.2.3 Table 5.8
//! (p.39): "L, R" for 2/0 and "L, C, R, SL, SR" for 3/2, then the LFE channel, which A/52 codes
//! after the full-bandwidth channels (§5.4.3, the `lfeon` channel follows `nfchans`).
//!
//! The mapping is a constant matrix, built in two steps:
//!
//! 1. **Into a 5.1 bed** (L, C, R, Ls, Rs, LFE): front left/right/center and LFE go to their
//!    own channels. Side and back speakers are both "surround" in a 3/2 layout: when a layout has
//!    both (7.1), each pair enters the surrounds at −3 dB (power-preserving); when it has one
//!    pair, at unity. A back center enters both surrounds at −3 dB, as A/52 §7.8.1 routes a
//!    mono surround "into left and right with −3 dB". Front left/right-of-center enter the
//!    center and the corresponding side at −3 dB each. Mono (a lone center) is the center.
//!    Stereo is L and R only: nothing is synthesized into C, the surrounds or LFE
//!    [choice: no upmix processing; a stereo program stays a stereo image on a 5.1 system,
//!    and the 2/0 output layout is the one to use for stereo sources anyway].
//! 2. **To stereo**, the LoRo downmix of A/52 §7.8.2 (p.95):
//!    `Lo = L + clev·C + slev·Ls`, `Ro = R + clev·C + slev·Rs`, with `clev = slev = 0.707`
//!    (−3 dB, `cmixlev` and `surmixlev` code '00', Tables 5.9 and 5.10). These are also the
//!    ITU-R BS.775 downmix coefficients. The LFE is dropped (A/52 §7.8: "Downmixing of the lfe
//!    channel is optional"; BS.775 omits it). A mono source becomes `0.707·C` in each of L and
//!    R, as the A/52 pseudo-code does ("route center into left and right with −3 dB").
//!
//! A/52 §7.8.2 then scales all coefficients by `1/(1 + 0.707 + 0.707) = 0.414` (−7.65 dB) so a
//! fixed-point decoder cannot overflow. In `f32` nothing overflows, and the normalizer and the
//! true-peak limiter that follow set the level and catch the peaks, so the default is
//! [`DownmixScaling::Unscaled`]; [`DownmixScaling::A52WorstCase`] applies the 0.414 factor.

/// A loudspeaker position (the names of SMPTE ST 2036-2 / the WAVE channel mask).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Speaker {
    /// Front left.
    FrontLeft,
    /// Front right.
    FrontRight,
    /// Front center.
    FrontCenter,
    /// Low-frequency effects.
    Lfe,
    /// Back (rear) left.
    BackLeft,
    /// Back (rear) right.
    BackRight,
    /// Front left of center.
    FrontLeftOfCenter,
    /// Front right of center.
    FrontRightOfCenter,
    /// Back center.
    BackCenter,
    /// Side left.
    SideLeft,
    /// Side right.
    SideRight,
}

/// Most input channels a layout may have.
pub const MAX_INPUT_CHANNELS: usize = 16;

/// The channel layout of decoded input: a list of distinct speakers, one per plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputLayout {
    speakers: [Speaker; MAX_INPUT_CHANNELS],
    n: usize,
}

/// Why a layout was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    /// No channels, or more than [`MAX_INPUT_CHANNELS`].
    #[error("{0} channels: a layout has 1 to 16")]
    Count(usize),
    /// A speaker appears twice.
    #[error("speaker {0:?} appears twice")]
    Duplicate(Speaker),
    /// No default layout for this many channels.
    #[error("no default layout for {0} channels")]
    NoDefault(usize),
}

impl InputLayout {
    /// A layout of `speakers`, one per plane in that order. Each speaker at most once.
    pub fn new(speakers: &[Speaker]) -> Result<Self, LayoutError> {
        let n = speakers.len();
        if n == 0 || n > MAX_INPUT_CHANNELS {
            return Err(LayoutError::Count(n));
        }
        for (i, s) in speakers.iter().enumerate() {
            if speakers[..i].contains(s) {
                return Err(LayoutError::Duplicate(*s));
            }
        }
        let mut a = [Speaker::FrontCenter; MAX_INPUT_CHANNELS];
        a[..n].copy_from_slice(speakers);
        Ok(Self { speakers: a, n })
    }

    /// The layout a decoder means by a bare channel count, in the WAVE / SMPTE channel order
    /// that libav also uses for decoded audio: 1 mono (C); 2 stereo (L R); 3 (L R C);
    /// 4 quad (L R BL BR); 5 (L R C BL BR); 6 5.1 (L R C LFE BL BR); 7 6.1 (L R C LFE BC SL SR);
    /// 8 7.1 (L R C LFE BL BR SL SR). [choice: the counts of 3, 4 and 5 are ambiguous (2.1 and
    /// 3.0, quad and 4.0); these are the WAVE_FORMAT_EXTENSIBLE defaults. The decoder adapter
    /// should pass the layout it was given whenever it has one.]
    pub fn default_for(channels: usize) -> Result<Self, LayoutError> {
        use Speaker::*;
        let s: &[Speaker] = match channels {
            1 => &[FrontCenter],
            2 => &[FrontLeft, FrontRight],
            3 => &[FrontLeft, FrontRight, FrontCenter],
            4 => &[FrontLeft, FrontRight, BackLeft, BackRight],
            5 => &[FrontLeft, FrontRight, FrontCenter, BackLeft, BackRight],
            6 => &[FrontLeft, FrontRight, FrontCenter, Lfe, BackLeft, BackRight],
            7 => &[
                FrontLeft,
                FrontRight,
                FrontCenter,
                Lfe,
                BackCenter,
                SideLeft,
                SideRight,
            ],
            8 => &[
                FrontLeft,
                FrontRight,
                FrontCenter,
                Lfe,
                BackLeft,
                BackRight,
                SideLeft,
                SideRight,
            ],
            n => return Err(LayoutError::NoDefault(n)),
        };
        Self::new(s)
    }

    /// The speakers, one per plane.
    pub fn speakers(&self) -> &[Speaker] {
        &self.speakers[..self.n]
    }

    /// The channel count.
    pub fn channels(&self) -> usize {
        self.n
    }
}

/// An output channel, named as in A/52.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutputChannel {
    /// Left.
    L,
    /// Center.
    C,
    /// Right.
    R,
    /// Left surround (A/52 "SL").
    Ls,
    /// Right surround (A/52 "SR").
    Rs,
    /// Low-frequency effects.
    Lfe,
}

/// The layouts we encode (SPEC §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutputLayout {
    /// 2/0, `acmod` 2, no LFE.
    Stereo,
    /// 3/2 with LFE, `acmod` 7, `lfeon` 1.
    FivePointOne,
}

impl OutputLayout {
    /// The output channels in A/52 order (Table 5.8, then LFE).
    pub fn channels(self) -> &'static [OutputChannel] {
        use OutputChannel::*;
        match self {
            Self::Stereo => &[L, R],
            Self::FivePointOne => &[L, C, R, Ls, Rs, Lfe],
        }
    }
    /// The channel count, LFE included.
    pub fn count(self) -> usize {
        self.channels().len()
    }
    /// A/52 `acmod` (Table 5.8).
    pub fn acmod(self) -> u8 {
        match self {
            Self::Stereo => 0b010,
            Self::FivePointOne => 0b111,
        }
    }
    /// A/52 `lfeon`.
    pub fn lfe(self) -> bool {
        matches!(self, Self::FivePointOne)
    }
}

/// Whether a downmix to stereo is scaled against overload (module documentation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DownmixScaling {
    /// The A/52 / BS.775 coefficients as they are (the level is the normalizer's business).
    #[default]
    Unscaled,
    /// All coefficients times `1/(1 + clev + slev)`, A/52 §7.8.2 "worst case" scaling.
    A52WorstCase,
}

/// −3 dB: `clev` and `slev` (A/52 Tables 5.9, 5.10 code '00'), and the power-preserving pan.
const M3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// A constant mixing matrix from an [`InputLayout`] to an [`OutputLayout`].
#[derive(Clone, Debug, PartialEq)]
pub struct Mixer {
    input: InputLayout,
    output: OutputLayout,
    /// `m[o][i]`: gain from input plane `i` to output plane `o`.
    m: [[f32; MAX_INPUT_CHANNELS]; 6],
    /// Per output plane, when it is a plain copy: `Some(None)` silent, `Some(Some(i))` a copy
    /// of input plane `i`; `None` a mix.
    routes: [Option<Option<usize>>; 6],
}

/// Why [`Mixer::process`] refused its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MixError {
    /// Wrong number of planes.
    #[error("expected {expected} planes, got {got}")]
    Planes {
        /// Planes expected.
        expected: usize,
        /// Planes given.
        got: usize,
    },
    /// The input planes differ in length.
    #[error("input planes differ in length")]
    Ragged,
}

impl Mixer {
    /// The matrix for `input` to `output` (module documentation).
    pub fn new(input: InputLayout, output: OutputLayout, scaling: DownmixScaling) -> Self {
        // Step 1: into the bed L, C, R, Ls, Rs, LFE.
        let has = |s| input.speakers().contains(&s);
        let side_and_back_l = has(Speaker::SideLeft) && has(Speaker::BackLeft);
        let side_and_back_r = has(Speaker::SideRight) && has(Speaker::BackRight);
        let mut bed = [[0.0f32; MAX_INPUT_CHANNELS]; 6];
        for (i, &s) in input.speakers().iter().enumerate() {
            let sl = if side_and_back_l { M3DB } else { 1.0 };
            let sr = if side_and_back_r { M3DB } else { 1.0 };
            let route: &[(usize, f32)] = match s {
                Speaker::FrontLeft => &[(0, 1.0)],
                Speaker::FrontCenter => &[(1, 1.0)],
                Speaker::FrontRight => &[(2, 1.0)],
                Speaker::Lfe => &[(5, 1.0)],
                Speaker::SideLeft | Speaker::BackLeft => &[(3, sl)],
                Speaker::SideRight | Speaker::BackRight => &[(4, sr)],
                Speaker::BackCenter => &[(3, M3DB), (4, M3DB)],
                Speaker::FrontLeftOfCenter => &[(0, M3DB), (1, M3DB)],
                Speaker::FrontRightOfCenter => &[(2, M3DB), (1, M3DB)],
            };
            for &(o, g) in route {
                bed[o][i] += g;
            }
        }
        // Step 2: the bed to the output.
        let mut m = [[0.0f32; MAX_INPUT_CHANNELS]; 6];
        match output {
            OutputLayout::FivePointOne => m = bed,
            OutputLayout::Stereo => {
                let k = match scaling {
                    DownmixScaling::Unscaled => 1.0,
                    DownmixScaling::A52WorstCase => 1.0 / (1.0 + 2.0 * M3DB),
                };
                for i in 0..input.channels() {
                    m[0][i] = k * (bed[0][i] + M3DB * bed[1][i] + M3DB * bed[3][i]);
                    m[1][i] = k * (bed[2][i] + M3DB * bed[1][i] + M3DB * bed[4][i]);
                }
            }
        }
        let mut routes = [None; 6];
        for (o, r) in routes.iter_mut().enumerate() {
            let row = &m[o][..input.channels()];
            let mut nz = row.iter().enumerate().filter(|(_, g)| **g != 0.0);
            *r = match (nz.next(), nz.next()) {
                (None, _) => Some(None),
                (Some((i, &1.0)), None) => Some(Some(i)),
                _ => None,
            };
        }
        Self {
            input,
            output,
            m,
            routes,
        }
    }

    /// The gain from input plane `i` to output plane `o`.
    pub fn gain(&self, o: usize, i: usize) -> f32 {
        self.m.get(o).and_then(|r| r.get(i)).copied().unwrap_or(0.0)
    }

    /// The input layout.
    pub fn input(&self) -> InputLayout {
        self.input
    }

    /// The output layout.
    pub fn output(&self) -> OutputLayout {
        self.output
    }

    /// Mix `input` (one plane per input channel, equal lengths) into `output` (one vector per
    /// output channel, A/52 order), replacing their contents.
    pub fn process(&self, input: &[&[f32]], output: &mut [Vec<f32>]) -> Result<(), MixError> {
        let (ni, no) = (self.input.channels(), self.output.count());
        if input.len() != ni {
            return Err(MixError::Planes {
                expected: ni,
                got: input.len(),
            });
        }
        if output.len() != no {
            return Err(MixError::Planes {
                expected: no,
                got: output.len(),
            });
        }
        let n = input[0].len();
        if input.iter().any(|p| p.len() != n) {
            return Err(MixError::Ragged);
        }
        for (o, out) in output.iter_mut().enumerate() {
            out.clear();
            match self.routes[o] {
                Some(None) => out.resize(n, 0.0),
                Some(Some(i)) => out.extend_from_slice(input[i]),
                None => {
                    out.resize(n, 0.0);
                    for (i, p) in input.iter().enumerate() {
                        let g = self.m[o][i];
                        if g != 0.0 {
                            for (d, &s) in out.iter_mut().zip(p.iter()) {
                                *d += g * s;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OutputChannel::*;
    use Speaker::*;

    fn mix(l: InputLayout, out: OutputLayout, x: &[f32]) -> Vec<f32> {
        let m = Mixer::new(l, out, DownmixScaling::Unscaled);
        let planes: Vec<Vec<f32>> = x.iter().map(|&v| vec![v]).collect();
        let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
        let mut o = vec![Vec::new(); out.count()];
        m.process(&refs, &mut o).unwrap();
        o.iter().map(|p| p[0]).collect()
    }

    /// A/52 §5.4.2.3 Table 5.8: 2/0 is "L, R", 3/2 is "L, C, R, SL, SR"; LFE after them.
    #[test]
    fn output_order_is_a52_table_5_8() {
        assert_eq!(OutputLayout::Stereo.channels(), &[L, R]);
        assert_eq!(OutputLayout::Stereo.acmod(), 2);
        assert_eq!(
            OutputLayout::FivePointOne.channels(),
            &[L, C, R, Ls, Rs, OutputChannel::Lfe]
        );
        assert_eq!(OutputLayout::FivePointOne.acmod(), 7);
        assert!(OutputLayout::FivePointOne.lfe());
        assert!(!OutputLayout::Stereo.lfe());
    }

    #[test]
    fn layouts_are_validated() {
        assert_eq!(InputLayout::new(&[]), Err(LayoutError::Count(0)));
        assert_eq!(
            InputLayout::new(&[FrontLeft, FrontLeft]),
            Err(LayoutError::Duplicate(FrontLeft))
        );
        assert!(InputLayout::default_for(9).is_err());
        for n in 1..=8 {
            assert_eq!(InputLayout::default_for(n).unwrap().channels(), n);
        }
    }

    /// 5.1 in decoder order (L R C LFE Ls Rs) is reordered to A/52 order (L C R Ls Rs LFE).
    #[test]
    fn five_one_is_reordered_to_a52() {
        let l = InputLayout::default_for(6).unwrap();
        let y = mix(
            l,
            OutputLayout::FivePointOne,
            &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        );
        assert_eq!(y, vec![1.0, 3.0, 2.0, 5.0, 6.0, 4.0]);
    }

    /// A/52 §7.8.2 LoRo with clev = slev = 0.707; LFE dropped.
    #[test]
    fn five_one_to_stereo_is_a52_loro() {
        let l = InputLayout::default_for(6).unwrap();
        let y = mix(l, OutputLayout::Stereo, &[1.0, 2.0, 3.0, 100.0, 5.0, 6.0]);
        let lo = 1.0 + 0.70710677 * 3.0 + 0.70710677 * 5.0;
        let ro = 2.0 + 0.70710677 * 3.0 + 0.70710677 * 6.0;
        assert!(
            (y[0] - lo).abs() < 1e-5 && (y[1] - ro).abs() < 1e-5,
            "{y:?}"
        );
        let m = Mixer::new(l, OutputLayout::Stereo, DownmixScaling::A52WorstCase);
        // A/52 Table 7.31: unscaled 1.0 -> scaled 0.414, 0.707 -> 0.293.
        assert!((m.gain(0, 0) - 0.4142).abs() < 1e-4);
        assert!((m.gain(0, 2) - 0.2929).abs() < 1e-4);
    }

    #[test]
    fn mono_and_stereo_mappings() {
        let mono = InputLayout::default_for(1).unwrap();
        let y = mix(mono, OutputLayout::Stereo, &[1.0]);
        assert!((y[0] - M3DB).abs() < 1e-7 && (y[1] - M3DB).abs() < 1e-7);
        assert_eq!(
            mix(mono, OutputLayout::FivePointOne, &[1.0]),
            vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0]
        );
        let st = InputLayout::default_for(2).unwrap();
        assert_eq!(
            mix(st, OutputLayout::Stereo, &[0.25, -0.5]),
            vec![0.25, -0.5]
        );
        assert_eq!(
            mix(st, OutputLayout::FivePointOne, &[0.25, -0.5]),
            vec![0.25, 0.0, -0.5, 0.0, 0.0, 0.0]
        );
    }

    /// 7.1: side and back pairs share the surrounds at −3 dB each; 6.1's back center is split.
    #[test]
    fn seven_one_and_six_one_into_surrounds() {
        let l = InputLayout::default_for(8).unwrap();
        let y = mix(
            l,
            OutputLayout::FivePointOne,
            &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
        );
        assert!((y[3] - 2.0 * M3DB).abs() < 1e-6 && y[4] == 0.0, "{y:?}");
        let l = InputLayout::default_for(7).unwrap();
        let y = mix(
            l,
            OutputLayout::FivePointOne,
            &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        );
        assert!((y[3] - M3DB).abs() < 1e-7 && (y[4] - M3DB).abs() < 1e-7);
    }

    #[test]
    fn process_checks_shapes() {
        let m = Mixer::new(
            InputLayout::default_for(2).unwrap(),
            OutputLayout::Stereo,
            DownmixScaling::Unscaled,
        );
        let mut o = vec![Vec::new(); 2];
        assert!(m.process(&[&[1.0]], &mut o).is_err());
        assert_eq!(
            m.process(&[&[1.0], &[1.0, 2.0]], &mut o),
            Err(MixError::Ragged)
        );
        let mut o1 = vec![Vec::new()];
        assert!(m.process(&[&[1.0], &[2.0]], &mut o1).is_err());
    }
}
