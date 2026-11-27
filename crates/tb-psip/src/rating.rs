//! US content advisories (rating region 0x01), as CTA-766-D §4.1 Table 3 (p.7–8) allows them
//! (`docs/notes/standards-digest.md` §3.4).
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

/// A rating system of our own: what one Rating Region Table (RRT) instance defines (A/65 §6.4,
/// Table 6.10). Regions 1 (US) and 2 (Canada) are fully defined by CTA-766 and never sent;
/// any other region is ours to define (A/65 asks for an ATSC-assigned value; on a private
/// plant an unassigned one only has to be consistent).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RatingSystem {
    region: u8,
    name: String,
    dimensions: Vec<RatingDimension>,
}

/// One dimension of a [`RatingSystem`]: its name and the values 1 and up (value 0, "not
/// rated in this dimension", is implicit and has null strings).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RatingDimension {
    /// Its name, at most 20 characters.
    pub name: String,
    /// Higher values mean more of what the dimension rates (A/65 graduated_scale).
    pub graduated: bool,
    /// Values 1, 2, …: at most 14 (values_defined is 4 bits and counts value 0).
    pub values: Vec<RatingValue>,
}

/// One value of a dimension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RatingValue {
    /// The abbreviation receivers show, at most 8 characters, not empty.
    pub abbrev: String,
    /// The full text, at most 150 characters.
    pub text: String,
}

/// A rating in one region of our own: (dimension, value) pairs, dimensions increasing, and the
/// short text receivers show for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegionRating {
    /// The rating region.
    pub region: u8,
    /// (rating_dimension, rating_value), dimensions strictly increasing, values 1 and up.
    pub values: Vec<(u8, u8)>,
    /// rating_description_text: at most 16 characters (empty: none).
    pub description: String,
}

/// What a content_advisory_descriptor says: a US rating and ratings in regions of our own,
/// at most 8 regions in all (A/65 §6.9.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Advisory {
    /// The US rating (region 0x01).
    pub us: Option<UsRating>,
    /// Ratings in our own regions, regions distinct.
    pub regions: Vec<RegionRating>,
}

impl From<UsRating> for Advisory {
    fn from(r: UsRating) -> Self {
        Advisory {
            us: Some(r),
            regions: Vec::new(),
        }
    }
}

/// The A/65 display limits, in characters.
const REGION_NAME_MAX: usize = 32;
const DIMENSION_NAME_MAX: usize = 20;
const ABBREV_MAX: usize = 8;
const VALUE_TEXT_MAX: usize = 150;
/// rating_description_text (A/65 §6.9.3).
pub const DESCRIPTION_MAX: usize = 16;

fn chars(s: &str) -> usize {
    s.chars().count()
}

impl RatingSystem {
    /// A system for `region` (3..=255) named `name`, with `dimensions` (1..=255). Every limit of
    /// A/65 Table 6.10 is checked, and the RRT must fit its one 1024-byte section
    /// ([`crate::tables::rrt`] checks that).
    pub fn new(
        region: u8,
        name: &str,
        dimensions: Vec<RatingDimension>,
    ) -> Result<RatingSystem, crate::Error> {
        use crate::Error::Lineup;
        if region < 3 {
            return Err(Lineup(format!(
                "rating region {region}: 0 is not used, 1 (US) and 2 (Canada) are defined by CTA-766"
            )));
        }
        if name.is_empty() || chars(name) > REGION_NAME_MAX {
            return Err(Lineup(format!(
                "rating region {region}: the name must be 1 to {REGION_NAME_MAX} characters"
            )));
        }
        if dimensions.is_empty() || dimensions.len() > 255 {
            return Err(Lineup(format!(
                "rating region {region}: 1 to 255 dimensions"
            )));
        }
        for d in &dimensions {
            if d.name.is_empty() || chars(&d.name) > DIMENSION_NAME_MAX {
                return Err(Lineup(format!(
                    "rating region {region}: dimension {:?}: the name must be 1 to {DIMENSION_NAME_MAX} characters",
                    d.name
                )));
            }
            if dimensions.iter().filter(|o| o.name == d.name).count() > 1 {
                return Err(Lineup(format!(
                    "rating region {region}: dimension {:?} twice",
                    d.name
                )));
            }
            if d.values.is_empty() || d.values.len() > 14 {
                return Err(Lineup(format!(
                    "rating region {region}: dimension {:?}: 1 to 14 values",
                    d.name
                )));
            }
            for v in &d.values {
                if v.abbrev.is_empty() || chars(&v.abbrev) > ABBREV_MAX {
                    return Err(Lineup(format!(
                        "rating region {region}: value {:?}: the abbreviation must be 1 to {ABBREV_MAX} characters",
                        v.abbrev
                    )));
                }
                if chars(&v.text) > VALUE_TEXT_MAX {
                    return Err(Lineup(format!(
                        "rating region {region}: value {:?}: the text is longer than {VALUE_TEXT_MAX} characters",
                        v.abbrev
                    )));
                }
                if d.values.iter().filter(|o| o.abbrev == v.abbrev).count() > 1 {
                    return Err(Lineup(format!(
                        "rating region {region}: dimension {:?}: value {:?} twice",
                        d.name, v.abbrev
                    )));
                }
            }
        }
        Ok(RatingSystem {
            region,
            name: name.to_owned(),
            dimensions,
        })
    }

    /// The region.
    pub fn region(&self) -> u8 {
        self.region
    }
    /// The region's name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The dimensions, in order (index = rating_dimension).
    pub fn dimensions(&self) -> &[RatingDimension] {
        &self.dimensions
    }

    /// The (rating_dimension, rating_value) of the value abbreviated `abbrev` in the dimension
    /// named `dimension`.
    pub fn lookup(&self, dimension: &str, abbrev: &str) -> Option<(u8, u8)> {
        let (di, d) = self
            .dimensions
            .iter()
            .enumerate()
            .find(|(_, d)| d.name == dimension)?;
        let vi = d.values.iter().position(|v| v.abbrev == abbrev)?;
        Some((di as u8, vi as u8 + 1))
    }

    /// Whether `r` is a rating this system defines: its region, dimensions increasing and
    /// defined, values defined and not 0, the description short enough.
    pub fn check(&self, r: &RegionRating) -> Result<(), crate::Error> {
        use crate::Error::Lineup;
        let region = self.region;
        if r.region != region {
            return Err(Lineup(format!(
                "rating for region {} checked against region {region}",
                r.region
            )));
        }
        if r.values.is_empty() && r.description.is_empty() {
            return Err(Lineup(format!(
                "rating region {region}: neither values nor a description"
            )));
        }
        if !r.values.windows(2).all(|w| w[0].0 < w[1].0) {
            return Err(Lineup(format!(
                "rating region {region}: dimensions not increasing"
            )));
        }
        for &(d, v) in &r.values {
            let ok = self
                .dimensions
                .get(usize::from(d))
                .is_some_and(|dim| v >= 1 && usize::from(v) <= dim.values.len());
            if !ok {
                return Err(Lineup(format!(
                    "rating region {region}: dimension {d} value {v} is not defined"
                )));
            }
        }
        if chars(&r.description) > DESCRIPTION_MAX {
            return Err(Lineup(format!(
                "rating region {region}: description {:?} is longer than {DESCRIPTION_MAX} characters",
                r.description
            )));
        }
        Ok(())
    }
}

impl Advisory {
    /// Whether the descriptor can be written: at most 8 regions, each once, none of them 1
    /// in `regions` (the US rating has its own field).
    pub fn check(&self) -> Result<(), crate::Error> {
        let n = usize::from(self.us.is_some()) + self.regions.len();
        if n > 8 {
            return Err(crate::Error::Invalid(
                "content advisory: at most 8 rating regions",
            ));
        }
        for (i, r) in self.regions.iter().enumerate() {
            if r.region < 3 || self.regions[..i].iter().any(|o| o.region == r.region) {
                return Err(crate::Error::Invalid(
                    "content advisory: regions of our own are 3..=255, each once",
                ));
            }
        }
        Ok(())
    }

    /// Nothing to say.
    pub fn is_empty(&self) -> bool {
        self.us.is_none() && self.regions.is_empty()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn cursed() -> RatingSystem {
        RatingSystem::new(
            0x10,
            "Schlarp Broadcasting Standards",
            vec![
                RatingDimension {
                    name: "Cursed".into(),
                    graduated: true,
                    values: vec![
                        RatingValue {
                            abbrev: "C1".into(),
                            text: "Mildly cursed".into(),
                        },
                        RatingValue {
                            abbrev: "C3".into(),
                            text: "Moderately cursed: contains Garfield".into(),
                        },
                    ],
                },
                RatingDimension {
                    name: "Eirin".into(),
                    graduated: true,
                    values: ["G", "PG12", "R15+", "R18+"]
                        .iter()
                        .map(|a| RatingValue {
                            abbrev: (*a).into(),
                            text: format!("Eirin {a}"),
                        })
                        .collect(),
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn systems_and_their_limits() {
        let s = cursed();
        assert_eq!(s.lookup("Eirin", "R15+"), Some((1, 3)));
        assert_eq!(s.lookup("Cursed", "C3"), Some((0, 2)));
        assert_eq!(s.lookup("Cursed", "C9"), None);
        let dims = s.dimensions().to_vec();
        assert!(RatingSystem::new(1, "US", dims.clone()).is_err());
        assert!(RatingSystem::new(2, "Canada", dims.clone()).is_err());
        assert!(RatingSystem::new(3, &"x".repeat(33), dims.clone()).is_err());
        let mut long = dims.clone();
        long[0].values[0].abbrev = "ABCDEFGHI".into();
        assert!(RatingSystem::new(3, "R", long).is_err());
        let mut many = dims.clone();
        many[0].values = (0..15)
            .map(|i| RatingValue {
                abbrev: format!("V{i}"),
                text: String::new(),
            })
            .collect();
        assert!(RatingSystem::new(3, "R", many).is_err());
        let mut twice = dims;
        twice[1].name = "Cursed".into();
        assert!(RatingSystem::new(3, "R", twice).is_err());
    }

    #[test]
    fn ratings_are_checked_against_their_system() {
        let s = cursed();
        let r = |values: Vec<(u8, u8)>, description: &str| RegionRating {
            region: 0x10,
            values,
            description: description.into(),
        };
        assert!(s.check(&r(vec![(0, 2), (1, 3)], "R15+ cursed")).is_ok());
        assert!(s.check(&r(vec![], "just a vibe")).is_ok());
        assert!(s.check(&r(vec![], "")).is_err());
        assert!(s.check(&r(vec![(1, 3), (0, 2)], "")).is_err());
        assert!(s.check(&r(vec![(0, 3)], "")).is_err());
        assert!(s.check(&r(vec![(0, 0)], "")).is_err());
        assert!(s.check(&r(vec![(2, 1)], "")).is_err());
        assert!(s.check(&r(vec![(0, 1)], "seventeen chars!!")).is_err());
        let a = Advisory {
            us: Some(UsRating::TvG),
            regions: (3..10)
                .map(|g| RegionRating {
                    region: g,
                    ..r(vec![], "x")
                })
                .collect(),
        };
        assert!(a.check().is_ok());
        let mut nine = a.clone();
        nine.regions.push(r(vec![], "x"));
        assert!(nine.check().is_err());
        let mut dup = a;
        dup.regions[1].region = 3;
        assert!(dup.check().is_err());
    }

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
