//! US content advisories (rating region 0x01), as CTA-766-D §4.1 Table 3 (p.7–8) allows them
//! (`notes/standards-digest.md` §3.4).
//!
//! [`UsRating`] can only hold one of the permitted messages: TV-Y7 may carry FV, TV-PG and
//! TV-14 any subset of D, L, S, V, TV-MA any subset of L, S, V (no D), and an MPAA rating is
//! sent alone.

/// The D/L/S/V content flags of TV-PG and TV-14.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Dlsv {
    /// Suggestive dialogue.
    pub d: bool,
    /// Coarse language.
    pub l: bool,
    /// Sexual situations.
    pub s: bool,
    /// Violence.
    pub v: bool,
}

/// The L/S/V content flags of TV-MA (D is not allowed with TV-MA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Lsv {
    /// Coarse language.
    pub l: bool,
    /// Sexual situations.
    pub s: bool,
    /// Violence.
    pub v: bool,
}

/// MPAA ratings, dimension 7 (CTA-766-D Table 1, p.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mpaa {
    /// "MPAA Rating Not Applicable".
    NotApplicable = 1,
    /// G.
    G = 2,
    /// PG.
    Pg = 3,
    /// PG-13.
    Pg13 = 4,
    /// R.
    R = 5,
    /// NC-17.
    Nc17 = 6,
    /// X.
    X = 7,
    /// "Not Rated by MPAA".
    NotRated = 8,
}

/// A permitted US rating message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsRating {
    /// TV-None: "no blocking is intended".
    TvNone,
    /// TV-Y.
    TvY,
    /// TV-Y7, optionally with fantasy violence.
    TvY7 {
        /// FV.
        fv: bool,
    },
    /// TV-G (no flags).
    TvG,
    /// TV-PG with flags.
    TvPg(Dlsv),
    /// TV-14 with flags.
    Tv14(Dlsv),
    /// TV-MA with flags.
    TvMa(Lsv),
    /// An MPAA rating.
    Mpaa(Mpaa),
}

impl UsRating {
    /// The (rating_dimension, rating_value) pairs, dimensions strictly increasing.
    pub fn dimensions(self) -> Vec<(u8, u8)> {
        let flags = |d: bool, l: bool, s: bool, v: bool| {
            [(1u8, d), (2, l), (3, s), (4, v)]
                .into_iter()
                .filter(|&(_, on)| on)
                .map(|(dim, _)| (dim, 1u8))
                .collect::<Vec<_>>()
        };
        match self {
            UsRating::TvNone => vec![(0, 1)],
            UsRating::TvY => vec![(5, 1)],
            UsRating::TvY7 { fv: false } => vec![(5, 2)],
            UsRating::TvY7 { fv: true } => vec![(5, 2), (6, 1)],
            UsRating::TvG => vec![(0, 2)],
            UsRating::TvPg(f) => [vec![(0, 3)], flags(f.d, f.l, f.s, f.v)].concat(),
            UsRating::Tv14(f) => [vec![(0, 4)], flags(f.d, f.l, f.s, f.v)].concat(),
            UsRating::TvMa(f) => [vec![(0, 5)], flags(false, f.l, f.s, f.v)].concat(),
            UsRating::Mpaa(m) => vec![(7, m as u8)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_from_the_standard() {
        // TV-14-L-V = rated_dimensions 3: (0,4) (2,1) (4,1) (digest §3.4).
        let r = UsRating::Tv14(Dlsv {
            l: true,
            v: true,
            ..Dlsv::default()
        });
        assert_eq!(r.dimensions(), vec![(0, 4), (2, 1), (4, 1)]);
        assert_eq!(
            UsRating::TvY7 { fv: true }.dimensions(),
            vec![(5, 2), (6, 1)]
        );
        assert_eq!(UsRating::Mpaa(Mpaa::NotRated).dimensions(), vec![(7, 8)]);
    }

    #[test]
    fn dimensions_increase() {
        let all = Lsv {
            l: true,
            s: true,
            v: true,
        };
        for r in [
            UsRating::TvMa(all),
            UsRating::TvPg(Dlsv {
                d: true,
                l: true,
                s: true,
                v: true,
            }),
        ] {
            let d = r.dimensions();
            assert!(d.windows(2).all(|w| w[0].0 < w[1].0));
        }
    }
}
