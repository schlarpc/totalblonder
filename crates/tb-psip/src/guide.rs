//! The program guide's time structure (A/65 §5, §6.5–6.6; SPEC §7.5).
//!
//! - Windows are three hours long and start at 00, 03, … 21 h UTC. Window `w` is counted from
//!   the GPS epoch in UTC seconds (1980-01-06 00:00 UTC is itself a boundary), so
//!   `w = (gps − leap) / 10800`.
//! - EIT-k is the window `now + k`. Window `w` always uses EIT PID `0x1D00 + (w mod 128)` and
//!   event ETT PID `0x1E00 + (w mod 128)`: at a boundary only the MGT changes, which is how
//!   the standard expects the roll (A/65 Annex D.9); version numbers are kept per PID.
//! - An event that spans windows appears in each with the same `event_id` (A/65 §6.5, p.40).

use tb_ts::Pid;

use crate::Error;
use crate::descriptor::Descriptor;
use crate::gps::{GpsSeconds, LeapSeconds};
use crate::lineup::{EtmLocation, SourceId};
use crate::tables::{EitEvent, EtmId, EventId};
use crate::text::Mss;

/// Seconds in a guide window.
pub const WINDOW: u32 = 3 * 3600;

/// A three-hour window, counted from the GPS epoch in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Window(pub u32);

impl Window {
    /// The window containing `t`.
    pub fn of(t: GpsSeconds, leap: LeapSeconds) -> Window {
        Window(t.0.saturating_sub(u32::from(leap.0)) / WINDOW)
    }

    /// Its first second, as GPS time.
    pub fn start(self, leap: LeapSeconds) -> GpsSeconds {
        GpsSeconds(self.0 * WINDOW + u32::from(leap.0))
    }

    /// The window `k` after this one.
    #[must_use]
    pub fn plus(self, k: u32) -> Window {
        Window(self.0 + k)
    }

    /// Its EIT PID.
    pub fn eit_pid(self) -> Pid {
        Pid::new(0x1D00 + (self.0 % 128) as u16).unwrap_or(Pid::NULL)
    }

    /// Its event ETT PID.
    pub fn ett_pid(self) -> Pid {
        Pid::new(0x1E00 + (self.0 % 128) as u16).unwrap_or(Pid::NULL)
    }
}

/// The channel ETT's PID (SPEC §7.1).
pub fn channel_ett_pid() -> Pid {
    Pid::new(0x1E80).unwrap_or(Pid::NULL)
}

/// One event of a service's schedule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Event {
    /// Stable for the event's life.
    pub id: EventId,
    /// Start.
    pub start: GpsSeconds,
    /// Duration, seconds.
    pub length: u32,
    /// Title.
    pub title: Mss,
    /// Description (sent as an event ETM), if any.
    pub description: Option<Mss>,
    /// Event descriptors (caption service, AC-3, rating).
    pub descriptors: Vec<Descriptor>,
}

impl Event {
    fn end(&self) -> u64 {
        u64::from(self.start.0) + u64::from(self.length)
    }
}

/// A service's schedule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Schedule {
    /// The service.
    pub source: SourceId,
    /// Name, for placeholder events.
    pub name: String,
    /// Events, in any order (sorted and checked here).
    pub events: Vec<Event>,
}

/// Placeholder event for a window with no scheduled event (SPEC §7.5: the guide is never
/// blank): the whole window, titled with the service's name, id `w mod 2^14`.
pub fn placeholder(
    w: Window,
    name: &str,
    leap: LeapSeconds,
    descriptors: Vec<Descriptor>,
) -> Event {
    Event {
        id: EventId::wrapping(w.0),
        start: w.start(leap),
        length: WINDOW,
        title: Mss::eng(name),
        description: None,
        descriptors,
    }
}

/// The EIT events of `schedule` that overlap window `w` (or a placeholder when none does and
/// `fill` is set), sorted and checked for overlap.
pub fn events_in(
    schedule: &Schedule,
    w: Window,
    leap: LeapSeconds,
    fill: bool,
    placeholder_descriptors: &[Descriptor],
) -> Result<Vec<(EitEvent, Option<Mss>)>, Error> {
    let ws = u64::from(w.start(leap).0);
    let we = ws + u64::from(WINDOW);
    let mut ev: Vec<&Event> = schedule
        .events
        .iter()
        .filter(|e| u64::from(e.start.0) < we && e.end() > ws)
        .collect();
    ev.sort_by_key(|e| e.start);
    for p in ev.windows(2) {
        if p[1].start.0 < p[0].start.0.saturating_add(p[0].length) {
            return Err(Error::Invalid("schedule events overlap"));
        }
    }
    let owned;
    if ev.is_empty() && fill {
        owned = placeholder(w, &schedule.name, leap, placeholder_descriptors.to_vec());
        ev.push(&owned);
    }
    Ok(ev
        .into_iter()
        .map(|e| {
            (
                EitEvent {
                    id: e.id,
                    start: e.start,
                    length: e.length,
                    etm: if e.description.is_some() {
                        EtmLocation::Here
                    } else {
                        EtmLocation::None
                    },
                    title: e.title.clone(),
                    descriptors: e.descriptors.clone(),
                },
                e.description.clone(),
            )
        })
        .collect())
}

/// The event ETMs of a window: (ETM id, text).
pub fn etms(source: SourceId, events: &[(EitEvent, Option<Mss>)]) -> Vec<(EtmId, Mss)> {
    events
        .iter()
        .filter_map(|(e, d)| d.as_ref().map(|t| (EtmId::Event(source, e.id), t.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAP: LeapSeconds = LeapSeconds(18);

    #[test]
    fn windows_on_utc_boundaries() {
        // 2026-11-26 01:30 UTC.
        let unix = 1_795_656_600i64 + 5400;
        let g = GpsSeconds::from_unix(unix, LEAP).unwrap();
        let w = Window::of(g, LEAP);
        let start_unix = w.start(LEAP).to_unix(LEAP);
        assert_eq!(start_unix % 10_800, 0);
        assert!(start_unix <= unix && unix < start_unix + 10_800);
        assert_eq!(w.eit_pid().get(), 0x1D00 + (w.0 % 128) as u16);
        assert_eq!(w.plus(128).eit_pid(), w.eit_pid());
    }

    #[test]
    fn spanning_event_in_both_windows_and_placeholders() {
        let w = Window(100_000);
        let s = Schedule {
            source: SourceId::new(1).unwrap(),
            name: "WALL".into(),
            events: vec![Event {
                id: EventId::new(7).unwrap(),
                start: GpsSeconds(w.start(LEAP).0 + WINDOW - 600),
                length: 1200,
                title: Mss::eng("Late show"),
                description: Some(Mss::eng("Text")),
                descriptors: vec![],
            }],
        };
        let a = events_in(&s, w, LEAP, true, &[]).unwrap();
        let b = events_in(&s, w.plus(1), LEAP, true, &[]).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].0.id, b[0].0.id);
        assert_eq!(etms(s.source, &a).len(), 1);
        let c = events_in(&s, w.plus(2), LEAP, true, &[]).unwrap();
        assert_eq!(c[0].0.title, Mss::eng("WALL"));
        assert_eq!(c[0].0.length, WINDOW);
        assert!(
            events_in(&s, w.plus(2), LEAP, false, &[])
                .unwrap()
                .is_empty()
        );
    }
}
