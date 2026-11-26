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
use tb_psip::rating::{Dlsv, Lsv, Mpaa, UsRating};
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
    /// Its rating.
    pub rating: Option<UsRating>,
}

/// Reads and keeps the schedule file.
pub struct ScheduleFile {
    path: PathBuf,
    last: HashMap<String, Vec<Entry>>,
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
        ScheduleFile {
            path,
            last: HashMap::new(),
        }
    }

    /// Reads the file again; on any error keeps the previous content and reports why.
    pub fn reload(&mut self, leap: LeapSeconds) {
        match std::fs::read_to_string(&self.path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse(&t, leap))
        {
            Ok(m) => self.last = m,
            Err(e) => tracing::warn!(path = %self.path.display(), "schedule not reloaded: {e}"),
        }
    }

    /// The schedules of `services` (service id, source id, name).
    pub fn schedules(&self, services: &[(String, SourceId, String)]) -> Vec<Schedule> {
        services
            .iter()
            .map(|(id, src, name)| Schedule {
                source: *src,
                name: name.clone(),
                events: self
                    .last
                    .get(id)
                    .map(|v| v.iter().map(|e| e.event.clone()).collect())
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// The rating of the event of `service` airing at `now`, if any.
    pub fn current_rating(&self, service: &str, now: GpsSeconds) -> Option<UsRating> {
        self.last.get(service)?.iter().find_map(|e| {
            let s = e.event.start.0;
            (s <= now.0 && now.0 < s.saturating_add(e.event.length))
                .then_some(e.rating)
                .flatten()
        })
    }
}

/// Parses the JSON text of a schedule file.
pub fn parse(text: &str, leap: LeapSeconds) -> Result<HashMap<String, Vec<Entry>>, String> {
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
            let rating = match e.get("rating").and_then(|x| x.as_str()) {
                Some(r) => Some(
                    parse_rating(r)
                        .ok_or_else(|| at(&format!("rating {r:?} is not a permitted US rating")))?,
                ),
                None => None,
            };
            let mut descriptors = Vec::new();
            if let Some(r) = rating {
                descriptors.push(Descriptor::ContentAdvisory(r));
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
            Some(UsRating::TvMa(Lsv {
                l: true,
                s: true,
                v: false
            }))
        );
    }
}
