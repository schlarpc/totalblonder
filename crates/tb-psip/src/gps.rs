//! GPS time and the STT's daylight-saving field (A/65 §6.1 and Annex A, p.24–25, p.84–85;
//! `notes/standards-digest.md` §3.1).

/// Unix time of the GPS epoch, 1980-01-06 00:00:00 UTC.
pub const GPS_EPOCH_UNIX: i64 = 315_964_800;

/// Seconds since the GPS epoch, counting leap seconds (the STT's `system_time` and every
/// A/65 start time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpsSeconds(pub u32);

/// The GPS–UTC offset in whole seconds: 18 since 2017-01-01. Configured, never assumed
/// silently (`notes/standards-digest.md` §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeapSeconds(pub u8);

impl GpsSeconds {
    /// The GPS time of a Unix (UTC) time, or `None` before the GPS epoch or after 2116.
    pub fn from_unix(unix: i64, leap: LeapSeconds) -> Option<Self> {
        let g = unix - GPS_EPOCH_UNIX + i64::from(leap.0);
        u32::try_from(g).ok().map(GpsSeconds)
    }

    /// The Unix (UTC) time of this GPS time.
    pub fn to_unix(self, leap: LeapSeconds) -> i64 {
        i64::from(self.0) + GPS_EPOCH_UNIX - i64::from(leap.0)
    }
}

/// The 16-bit `daylight_saving` field (A/65 Table 6.1 and Annex A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DaylightSaving {
    /// DS_status: daylight saving time is in effect.
    pub status: bool,
    /// DS_day_of_month and DS_hour of the next transition when it is less than a month
    /// away, else `None` (both fields zero).
    pub transition: Option<(u8, u8)>,
}

impl DaylightSaving {
    /// The two bytes: DS_status, reserved '11', DS_day_of_month (5 bits), DS_hour (8 bits).
    pub fn bytes(self) -> [u8; 2] {
        let (day, hour) = self.transition.unwrap_or((0, 0));
        [(u8::from(self.status) << 7) | 0x60 | (day & 0x1F), hour]
    }
}

/// Which daylight-saving rule the plant's viewers live under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DstRule {
    /// No daylight saving: status 0, no transitions.
    None,
    /// The US rule since 2007: from the second Sunday in March to the first Sunday in
    /// November, 02:00 local time.
    #[default]
    Us,
}

const DAY: i64 = 86_400;

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Year of a day count since 1970-01-01.
fn year_of(days: i64) -> i64 {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + i64::from(m <= 2)
}

/// Day of month of the `n`-th Sunday (1-based) of month `m` of year `y`.
fn nth_sunday(y: i64, m: u32, n: u32) -> u32 {
    let first = days_from_civil(y, m, 1);
    // 1970-01-01 was a Thursday; weekday 0 = Sunday.
    let wd = (first + 4).rem_euclid(7) as u32;
    1 + (7 - wd) % 7 + 7 * (n - 1)
}

impl DstRule {
    /// The daylight-saving field for Unix time `unix`.
    ///
    /// US rule as applied here: the status changes at 12:00 UTC on the transition day, after
    /// every continental zone has switched at 02:00 local and well before the latest moment
    /// A/65 Table A2 allows (19:00 Pacific); the transition day and hour (2) are announced
    /// from 30 days before until the status changes.
    pub fn at(self, unix: i64) -> DaylightSaving {
        match self {
            DstRule::None => DaylightSaving::default(),
            DstRule::Us => {
                let y = year_of(unix.div_euclid(DAY));
                let sd = nth_sunday(y, 3, 2);
                let ed = nth_sunday(y, 11, 1);
                let start = days_from_civil(y, 3, sd) * DAY + 12 * 3600;
                let end = days_from_civil(y, 11, ed) * DAY + 12 * 3600;
                let soon = |t: i64| unix >= t - 30 * DAY;
                if unix < start {
                    DaylightSaving {
                        status: false,
                        transition: soon(start).then_some((sd as u8, 2)),
                    }
                } else if unix < end {
                    DaylightSaving {
                        status: true,
                        transition: soon(end).then_some((ed as u8, 2)),
                    }
                } else {
                    DaylightSaving {
                        status: false,
                        transition: None,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unix(y: i64, m: u32, d: u32, h: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + h * 3600
    }

    #[test]
    fn gps_epoch() {
        assert_eq!(days_from_civil(1980, 1, 6) * DAY, GPS_EPOCH_UNIX);
        // 2026-11-26 00:00:00 UTC with 18 leap seconds.
        let t = unix(2026, 11, 26, 0);
        let g = GpsSeconds::from_unix(t, LeapSeconds(18)).unwrap();
        assert_eq!(g.to_unix(LeapSeconds(18)), t);
        assert_eq!(i64::from(g.0), t - GPS_EPOCH_UNIX + 18);
    }

    #[test]
    fn us_transitions_2026() {
        // 2026: DST from Sunday 8 March to Sunday 1 November.
        assert_eq!(nth_sunday(2026, 3, 2), 8);
        assert_eq!(nth_sunday(2026, 11, 1), 1);
        let r = DstRule::Us;
        assert_eq!(
            r.at(unix(2026, 1, 15, 0)),
            DaylightSaving {
                status: false,
                transition: None
            }
        );
        assert_eq!(
            r.at(unix(2026, 2, 20, 0)),
            DaylightSaving {
                status: false,
                transition: Some((8, 2))
            }
        );
        assert!(!r.at(unix(2026, 3, 8, 11)).status);
        assert_eq!(
            r.at(unix(2026, 3, 8, 12)),
            DaylightSaving {
                status: true,
                transition: None
            }
        );
        assert_eq!(
            r.at(unix(2026, 10, 15, 0)),
            DaylightSaving {
                status: true,
                transition: Some((1, 2))
            }
        );
        assert_eq!(
            r.at(unix(2026, 11, 26, 0)),
            DaylightSaving {
                status: false,
                transition: None
            }
        );
        // Field bytes: status, reserved 11, day, hour.
        assert_eq!(
            DaylightSaving {
                status: true,
                transition: Some((1, 2))
            }
            .bytes(),
            [0xE1, 2]
        );
        assert_eq!(DaylightSaving::default().bytes(), [0x60, 0]);
    }

    #[test]
    fn year_boundaries() {
        assert_eq!(year_of(days_from_civil(2027, 1, 1)), 2027);
        assert_eq!(year_of(days_from_civil(2026, 12, 31)), 2026);
        assert_eq!(year_of(days_from_civil(2028, 2, 29)), 2028);
    }
}
