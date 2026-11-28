//! The program guide's input (SPEC §7.5, §13.1): a JSON file of events per service, read
//! again at every table rebuild, so editing the file (or having a producer write it) changes
//! the guide within one period.
//!
//! ```json
//! { "wall": [ { "start": "2026-11-26T20:00:00Z", "minutes": 60,
//!               "title": "Evening", "description": "…", "rating": "TV-14-LV" } ] }
//! ```
//!
//! - `start` is RFC 3339 in UTC (`Z`) or with an offset; `minutes` or `seconds` gives the length.
//! - `rating` is a US rating (`TV-G`, `TV-14-DLSV`, `TV-MA-LS`, `TV-Y7-FV`, `MPAA-PG-13`, …) and
//!   must be one CTA-766 permits; anything else is refused with the event.
//! - `ratings` rates the event in rating systems of our own (`[[rating_system]]` of the
//!   configuration), at most seven besides the US rating:
//!   `[ { "system": 16, "values": { "Eirin": "R15+" }, "description": "R15+ cursed" } ]`.
//!   `system` is the region number or the system's name; `values` maps dimension names to value
//!   abbreviations; `description` (at most 16 characters) is the text receivers show for it.
//! - `event_id` is derived from the start minute (`(start_gps / 60) mod 2^14`): stable for the
//!   event's life and unique within a channel's window, since events do not overlap.
//!
//! A malformed file is reported and the last good one is kept.

use std::collections::HashMap;
use std::path::PathBuf;

use tb_psip::descriptor::Descriptor;
use tb_psip::gps::{GpsSeconds, LeapSeconds};
use tb_psip::guide::{Event, Schedule};
use tb_psip::lineup::SourceId;
use tb_psip::rating::{Advisory, Dlsv, Lsv, Mpaa, RatingSystem, RegionRating, UsRating};
use tb_psip::tables::EventId;
use tb_psip::text::Mss;

/// Parses a US rating label into a permitted message (CTA-766-D Table 3).
pub fn parse_rating(s: &str) -> Option<UsRating> {
    let u = s.trim().to_ascii_uppercase();
    if let Some(m) = u.strip_prefix("MPAA-") {
        return Some(UsRating::Mpaa(match m {
            "NA" | "N/A" => Mpaa::NotApplicable,
            "G" => Mpaa::G,
            "PG" => Mpaa::Pg,
            "PG-13" => Mpaa::Pg13,
            "R" => Mpaa::R,
            "NC-17" => Mpaa::Nc17,
            "X" => Mpaa::X,
            "NR" => Mpaa::NotRated,
            _ => return None,
        }));
    }
    let rest = u.strip_prefix("TV-")?;
    let (level, flags) = match rest.split_once('-') {
        Some((l, f)) => (l, f),
        None => (rest, ""),
    };
    let has = |c: char| flags.contains(c);
    if flags.chars().any(|c| !"DLSVF".contains(c)) {
        return None;
    }
    Some(match level {
        "NONE" if flags.is_empty() => UsRating::TvNone,
        "Y" if flags.is_empty() => UsRating::TvY,
        "Y7" if flags.is_empty() => UsRating::TvY7 { fv: false },
        "Y7" if flags == "FV" => UsRating::TvY7 { fv: true },
        "G" if flags.is_empty() => UsRating::TvG,
        "PG" if !has('F') => UsRating::TvPg(Dlsv {
            d: has('D'),
            l: has('L'),
            s: has('S'),
            v: has('V'),
        }),
        "14" if !has('F') => UsRating::Tv14(Dlsv {
            d: has('D'),
            l: has('L'),
            s: has('S'),
            v: has('V'),
        }),
        // TV-MA carries no D (CTA-766-D Table 3).
        "MA" if !has('F') && !has('D') => UsRating::TvMa(Lsv {
            l: has('L'),
            s: has('S'),
            v: has('V'),
        }),
        _ => return None,
    })
}

/// One parsed event with its rating (for the PMT of the current event).
#[derive(Debug, Clone)]
pub struct Entry {
    /// The event.
    pub event: Event,
    /// Its rating, if rated.
    pub rating: Option<Advisory>,
}

/// Guide events that inputs publish for the services they feed (an HLS input's
/// `EXT-X-DATERANGE`s): by service id. A service with events here takes them instead of the
/// schedule file's.
pub type Feeds = std::sync::Arc<std::sync::Mutex<HashMap<String, Vec<Entry>>>>;

/// Reads and keeps the schedule file, and consults the inputs' [`Feeds`].
pub struct ScheduleFile {
    path: Option<PathBuf>,
    last: HashMap<String, Vec<Entry>>,
    ratings: Vec<RatingSystem>,
    feeds: Feeds,
}

/// One event from an input (the schedule file's US ratings only): `start` and `seconds` on
/// the Unix clock, a title, perhaps a description and a rating such as `TV-14-LV`.
pub fn entry(
    start_unix: i64,
    seconds: u32,
    title: &str,
    description: Option<&str>,
    rating: Option<&str>,
    leap: LeapSeconds,
) -> Result<Entry, String> {
    let gps = GpsSeconds::from_unix(start_unix, leap).ok_or("start out of range")?;
    if seconds == 0 || seconds >= 1 << 20 {
        return Err("length must be 1 s to 12 days".into());
    }
    let us = match rating {
        Some(r) => Some(
            parse_rating(r).ok_or_else(|| format!("rating {r:?} is not a permitted US rating"))?,
        ),
        None => None,
    };
    let advisory = Advisory {
        us,
        regions: Vec::new(),
    };
    advisory.check().map_err(|e| e.to_string())?;
    let rating = (!advisory.is_empty()).then_some(advisory);
    let mut descriptors = Vec::new();
    if let Some(r) = &rating {
        descriptors.push(Descriptor::ContentAdvisory(r.clone()));
    }
    Ok(Entry {
        event: Event {
            id: EventId::wrapping(gps.0 / 60),
            start: gps,
            length: seconds,
            title: Mss::eng(title),
            description: description.map(Mss::eng),
            descriptors,
        },
        rating,
    })
}

fn parse_time(s: &str) -> Option<i64> {
    // RFC 3339: YYYY-MM-DDTHH:MM:SS(.frac)?(Z|±HH:MM)
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let num = |a: usize, n: usize| -> Option<i64> { s.get(a..a + n)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, se) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let off = match b.get(i)? {
        b'Z' | b'z' => 0,
        b'+' | b'-' => {
            let sign = if b[i] == b'-' { -1 } else { 1 };
            sign * (num(i + 1, 2)? * 3600 + num(i + 4, 2)? * 60)
        }
        _ => return None,
    };
    // days from civil (proleptic Gregorian)
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - off)
}

impl ScheduleFile {
    /// The file at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self::with_ratings(path, Vec::new())
    }

    /// The file at `path`, whose events may rate in `ratings`.
    pub fn with_ratings(path: PathBuf, ratings: Vec<RatingSystem>) -> Self {
        ScheduleFile {
            path: Some(path),
            last: HashMap::new(),
            ratings,
            feeds: Feeds::default(),
        }
    }

    /// No file: the inputs' events only.
    pub fn feeds_only(feeds: Feeds) -> Self {
        ScheduleFile {
            path: None,
            last: HashMap::new(),
            ratings: Vec::new(),
            feeds,
        }
    }

    /// Consults `feeds` too.
    pub fn with_feeds(mut self, feeds: Feeds) -> Self {
        self.feeds = feeds;
        self
    }

    /// The events of `service`: the inputs' if they have any, else the file's.
    fn events(&self, service: &str) -> Vec<Entry> {
        let fed = self
            .feeds
            .lock()
            .ok()
            .and_then(|f| f.get(service).filter(|v| !v.is_empty()).cloned());
        fed.or_else(|| self.last.get(service).cloned())
            .unwrap_or_default()
    }

    /// Reads the file again; on any error keeps the previous content, reports why and returns
    /// false.
    pub fn reload(&mut self, leap: LeapSeconds) -> bool {
        let Some(path) = &self.path else {
            return true;
        };
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse_with(&t, leap, &self.ratings))
        {
            Ok(m) => {
                self.last = m;
                true
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), "schedule not reloaded: {e}");
                false
            }
        }
    }

    /// How many services the file and the inputs have events for.
    pub fn services(&self) -> usize {
        let fed = self.feeds.lock().map_or(0, |f| {
            f.keys().filter(|k| !self.last.contains_key(*k)).count()
        });
        self.last.len() + fed
    }

    /// The schedules of `services` (service id, source id, name).
    pub fn schedules(&self, services: &[(String, SourceId, String)]) -> Vec<Schedule> {
        services
            .iter()
            .map(|(id, src, name)| Schedule {
                source: *src,
                name: name.clone(),
                events: self.events(id).into_iter().map(|e| e.event).collect(),
            })
            .collect()
    }

    /// The rating of the event of `service` airing at `now`, if any.
    pub fn current_rating(&self, service: &str, now: GpsSeconds) -> Option<Advisory> {
        self.events(service).iter().find_map(|e| {
            let s = e.event.start.0;
            (s <= now.0 && now.0 < s.saturating_add(e.event.length))
                .then(|| e.rating.clone())
                .flatten()
        })
    }
}

/// Parses the JSON text of a schedule file whose events use no rating system of our own.
pub fn parse(text: &str, leap: LeapSeconds) -> Result<HashMap<String, Vec<Entry>>, String> {
    parse_with(text, leap, &[])
}

/// An event's `ratings`: each entry resolved against the system it names.
fn region_ratings(
    v: &serde_json::Value,
    systems: &[RatingSystem],
) -> Result<Vec<RegionRating>, String> {
    let arr = v.as_array().ok_or("ratings must be a list")?;
    let mut out = Vec::new();
    for r in arr {
        let sys = match r.get("system") {
            Some(serde_json::Value::Number(n)) => {
                let k = n.as_u64().unwrap_or(0);
                systems.iter().find(|s| u64::from(s.region()) == k)
            }
            Some(serde_json::Value::String(name)) => systems.iter().find(|s| s.name() == name),
            _ => return Err("each rating needs a system (region number or name)".into()),
        }
        .ok_or_else(|| {
            format!(
                "no rating system {}",
                r.get("system").map_or(String::new(), ToString::to_string)
            )
        })?;
        let mut values = Vec::new();
        if let Some(m) = r.get("values") {
            let m = m
                .as_object()
                .ok_or("values must map dimension names to values")?;
            for (dim, val) in m {
                let a = val
                    .as_str()
                    .ok_or("a value is its abbreviation (a string)")?;
                values.push(sys.lookup(dim, a).ok_or_else(|| {
                    format!(
                        "rating system {:?} has no value {a:?} in dimension {dim:?}",
                        sys.name()
                    )
                })?);
            }
        }
        values.sort_unstable();
        let rr = RegionRating {
            region: sys.region(),
            values,
            description: r
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_owned(),
        };
        sys.check(&rr).map_err(|e| e.to_string())?;
        out.push(rr);
    }
    Ok(out)
}

/// Parses the JSON text of a schedule file whose events may rate in `systems`.
pub fn parse_with(
    text: &str,
    leap: LeapSeconds,
    systems: &[RatingSystem],
) -> Result<HashMap<String, Vec<Entry>>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let obj = v
        .as_object()
        .ok_or("the top level must be an object of services")?;
    let mut out = HashMap::new();
    for (service, events) in obj {
        let arr = events
            .as_array()
            .ok_or_else(|| format!("{service}: a list of events"))?;
        let mut list = Vec::new();
        for (i, e) in arr.iter().enumerate() {
            let at = |what: &str| format!("{service} event {i}: {what}");
            let start = e
                .get("start")
                .and_then(|x| x.as_str())
                .ok_or_else(|| at("start"))?;
            let unix = parse_time(start).ok_or_else(|| at("start is not RFC 3339"))?;
            let gps = GpsSeconds::from_unix(unix, leap).ok_or_else(|| at("start out of range"))?;
            let secs = e
                .get("seconds")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    e.get("minutes")
                        .and_then(serde_json::Value::as_u64)
                        .map(|m| m * 60)
                })
                .ok_or_else(|| at("minutes or seconds"))?;
            if secs == 0 || secs >= 1 << 20 {
                return Err(at("length must be 1 s to 12 days"));
            }
            let title = e
                .get("title")
                .and_then(|x| x.as_str())
                .ok_or_else(|| at("title"))?;
            let us = match e.get("rating").and_then(|x| x.as_str()) {
                Some(r) => Some(
                    parse_rating(r)
                        .ok_or_else(|| at(&format!("rating {r:?} is not a permitted US rating")))?,
                ),
                None => None,
            };
            let regions = match e.get("ratings") {
                Some(v) => region_ratings(v, systems).map_err(|m| at(&m))?,
                None => Vec::new(),
            };
            let advisory = Advisory { us, regions };
            advisory.check().map_err(|m| at(&m.to_string()))?;
            let rating = (!advisory.is_empty()).then_some(advisory);
            let mut descriptors = Vec::new();
            if let Some(r) = &rating {
                descriptors.push(Descriptor::ContentAdvisory(r.clone()));
            }
            list.push(Entry {
                event: Event {
                    id: EventId::wrapping(gps.0 / 60),
                    start: gps,
                    length: secs as u32,
                    title: Mss::eng(title),
                    description: e.get("description").and_then(|x| x.as_str()).map(Mss::eng),
                    descriptors,
                },
                rating,
            });
        }
        list.sort_by_key(|e| e.event.start);
        for w in list.windows(2) {
            if w[1].event.start.0 < w[0].event.start.0 + w[0].event.length {
                return Err(format!(
                    "{service}: events overlap at {}",
                    w[1].event.start.0
                ));
            }
        }
        out.insert(service.clone(), list);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratings() {
        assert_eq!(parse_rating("TV-G"), Some(UsRating::TvG));
        assert_eq!(
            parse_rating("tv-14-lv"),
            Some(UsRating::Tv14(Dlsv {
                l: true,
                v: true,
                ..Dlsv::default()
            }))
        );
        assert_eq!(parse_rating("TV-Y7-FV"), Some(UsRating::TvY7 { fv: true }));
        assert_eq!(parse_rating("MPAA-PG-13"), Some(UsRating::Mpaa(Mpaa::Pg13)));
        // Not permitted: D with TV-MA, flags on TV-G, FV outside TV-Y7.
        assert_eq!(parse_rating("TV-MA-D"), None);
        assert_eq!(parse_rating("TV-G-V"), None);
        assert_eq!(parse_rating("TV-PG-FV"), None);
        assert_eq!(parse_rating("PG"), None);
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_time("2026-11-26T20:00:00-05:00"),
            parse_time("2026-11-27T01:00:00Z")
        );
        assert_eq!(
            parse_time("2026-11-26T20:00:00.5Z"),
            parse_time("2026-11-26T20:00:00Z")
        );
        assert!(parse_time("2026-11-26 20:00").is_none());
    }

    #[test]
    fn file() {
        let leap = LeapSeconds(18);
        let m = parse(
            r#"{"wall":[{"start":"2026-11-26T21:00:00Z","minutes":60,"title":"B","rating":"TV-MA-LS"},
                        {"start":"2026-11-26T20:00:00Z","minutes":60,"title":"A","description":"first"}]}"#,
            leap,
        )
        .unwrap();
        let w = &m["wall"];
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].event.title, Mss::eng("A"));
        assert_ne!(w[0].event.id, w[1].event.id);
        assert!(parse(r#"{"x":[{"start":"2026-11-26T20:00:00Z","minutes":90,"title":"A"},{"start":"2026-11-26T21:00:00Z","minutes":60,"title":"B"}]}"#, leap).is_err());
        assert!(parse(r#"{"x":[{"start":"2026-11-26T20:00:00Z","minutes":9,"title":"A","rating":"TV-MA-D"}]}"#, leap).is_err());
        let mut f = ScheduleFile::new(PathBuf::from("/nonexistent"));
        f.last = m;
        let now = GpsSeconds::from_unix(parse_time("2026-11-26T21:30:00Z").unwrap(), leap).unwrap();
        assert_eq!(
            f.current_rating("wall", now),
            Some(
                UsRating::TvMa(Lsv {
                    l: true,
                    s: true,
                    v: false
                })
                .into()
            )
        );
    }

    fn systems() -> Vec<RatingSystem> {
        use tb_psip::rating::{RatingDimension, RatingValue};
        let v = |a: &str| RatingValue {
            abbrev: a.into(),
            text: format!("{a} text"),
        };
        vec![
            RatingSystem::new(
                16,
                "Japan",
                vec![RatingDimension {
                    name: "Eirin".into(),
                    graduated: true,
                    values: vec![v("G"), v("PG12"), v("R15+"), v("R18+")],
                }],
            )
            .unwrap(),
            RatingSystem::new(
                17,
                "Cursed",
                vec![
                    RatingDimension {
                        name: "Curse".into(),
                        graduated: true,
                        values: vec![v("C1"), v("C3")],
                    },
                    RatingDimension {
                        name: "Garfield".into(),
                        graduated: false,
                        values: vec![v("Yes")],
                    },
                ],
            )
            .unwrap(),
        ]
    }

    /// Events rated in systems of our own, by number and by name, mixed with a US rating.
    #[test]
    fn ratings_of_our_own() {
        let leap = LeapSeconds(18);
        let sys = systems();
        let m = parse_with(
            r#"{"anime":[{"start":"2026-11-26T20:00:00Z","minutes":30,"title":"A","rating":"TV-14",
                 "ratings":[{"system":16,"values":{"Eirin":"R15+"}},
                            {"system":"Cursed","values":{"Garfield":"Yes","Curse":"C3"},"description":"very cursed"}]},
                {"start":"2026-11-26T20:30:00Z","minutes":30,"title":"B",
                 "ratings":[{"system":"Cursed","description":"just a vibe"}]}]}"#,
            leap,
            &sys,
        )
        .unwrap();
        let a = m["anime"][0].rating.clone().unwrap();
        assert_eq!(a.us, Some(UsRating::Tv14(Dlsv::default())));
        assert_eq!(
            a.regions,
            vec![
                RegionRating {
                    region: 16,
                    values: vec![(0, 3)],
                    description: String::new()
                },
                RegionRating {
                    region: 17,
                    values: vec![(0, 2), (1, 1)],
                    description: "very cursed".into()
                },
            ]
        );
        assert!(matches!(
            &m["anime"][0].event.descriptors[..],
            [Descriptor::ContentAdvisory(x)] if *x == a
        ));
        let b = m["anime"][1].rating.clone().unwrap();
        assert_eq!(b.us, None);
        assert_eq!(b.regions[0].description, "just a vibe");
        let bad = |ratings: &str| {
            parse_with(
                &format!(r#"{{"x":[{{"start":"2026-11-26T20:00:00Z","minutes":9,"title":"A","ratings":{ratings}}}]}}"#),
                leap,
                &sys,
            )
            .is_err()
        };
        assert!(bad(r#"[{"system":99,"values":{"Eirin":"G"}}]"#));
        assert!(bad(r#"[{"system":16,"values":{"Eirin":"X"}}]"#));
        assert!(bad(r#"[{"system":16,"values":{"Nope":"G"}}]"#));
        assert!(bad(r#"[{"system":16}]"#));
        assert!(bad(r#"[{"system":16,"description":"seventeen chars!!"}]"#));
        assert!(bad(
            r#"[{"system":16,"description":"a"},{"system":"Japan","description":"b"}]"#
        ));
        // without the systems, the same events are refused
        assert!(parse(r#"{"x":[{"start":"2026-11-26T20:00:00Z","minutes":9,"title":"A","ratings":[{"system":16,"values":{"Eirin":"G"}}]}]}"#, leap).is_err());
    }
}
