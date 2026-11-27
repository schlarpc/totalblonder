//! The table builder: every carrier's PSI/PSIP rebuilt from its lineup on a fixed period and
//! sent to its mux, so the guide windows roll and content changes reach the air (SPEC §7).
//! Versions change only when content does (`tb_psip::version`).

use std::sync::mpsc::SyncSender;

use tb_psip::build::{GuideConfig, build_with_ratings};
use tb_psip::gps::{GpsSeconds, LeapSeconds};
use tb_psip::guide::Schedule;
use tb_psip::lineup::Carrier;
use tb_psip::version::{Key, Stored, Versions};
use tb_ts::section::Version;

use crate::carrier::CarrierMsg;
use crate::time::{Clock, HZ};

/// One carrier's inputs to the builder.
pub struct TableTarget {
    /// The lineup.
    pub lineup: Carrier,
    /// The mux queue.
    pub tx: SyncSender<CarrierMsg>,
    /// Versions (restored from persistence when available).
    pub versions: Versions,
    /// The service id of each program, in lineup order (for the schedule file).
    pub services: Vec<String>,
}

/// Table versions kept across restarts (SPEC §7.3: versions keep incrementing, so a receiver
/// never sees an old version number with new content). One JSON file, replaced atomically.
pub struct VersionFile {
    path: std::path::PathBuf,
}

impl VersionFile {
    /// The file at `path` (it need not exist yet).
    pub fn new(path: std::path::PathBuf) -> Self {
        VersionFile { path }
    }

    /// The stored versions of the carrier with `tsid` (empty when there are none or the file
    /// cannot be read; a damaged file is reported and ignored).
    pub fn load(&self, tsid: u16) -> Versions {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Versions::new();
        };
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %self.path.display(), "version file unreadable: {e}");
                return Versions::new();
            }
        };
        let entries = v
            .get(tsid.to_string())
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();
        Versions::import(entries.iter().filter_map(|e| {
            let a = e.as_array()?;
            let n = |i: usize| a.get(i).and_then(serde_json::Value::as_u64);
            let ext = a.get(2).and_then(serde_json::Value::as_i64)?;
            Some((
                Key {
                    pid: u16::try_from(n(0)?).ok()?,
                    table_id: u8::try_from(n(1)?).ok()?,
                    extension: u16::try_from(ext).ok(),
                },
                Stored {
                    version: Version::new(u8::try_from(n(3)?).ok()?),
                    digest: n(4)?,
                },
            ))
        }))
    }

    /// Writes every carrier's versions.
    pub fn save(&self, all: &mut [(u16, &mut Versions)]) -> std::io::Result<()> {
        let mut root = serde_json::Map::new();
        for (tsid, v) in all.iter_mut() {
            let rows: Vec<serde_json::Value> = v
                .export()
                .into_iter()
                .map(|(k, s)| {
                    serde_json::json!([
                        k.pid,
                        k.table_id,
                        k.extension.map_or(-1, i64::from),
                        s.version.get(),
                        s.digest
                    ])
                })
                .collect();
            root.insert(tsid.to_string(), serde_json::Value::Array(rows));
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::Value::Object(root).to_string())?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// Rebuilds every `period` of process time until the clock stops, saving versions to `file`
/// whenever one changed.
#[allow(clippy::too_many_arguments, reason = "the table thread's whole setup")]
pub fn run(
    mut targets: Vec<TableTarget>,
    guide: GuideConfig,
    ratings: &[tb_psip::rating::RatingSystem],
    leap: LeapSeconds,
    period: u64,
    clock: &dyn Clock,
    file: Option<VersionFile>,
    mut schedule: Option<crate::schedule::ScheduleFile>,
) {
    let mut t = clock.now();
    loop {
        let unix = clock.unix_nanos_at(t).div_euclid(1_000_000_000) as i64;
        // One wide event per rebuild: what was read, built and handed over, and how long it took.
        let cycle = tb_telemetry::wide!("tables.cycle", carriers = targets.len());
        if let Some(s) = schedule.as_mut() {
            let ok = s.reload(leap);
            cycle
                .set("schedule.ok", ok)
                .set("schedule.services", s.services());
            if !ok {
                cycle.level(tracing::Level::WARN);
            }
        }
        if let Some(now) = GpsSeconds::from_unix(unix, leap) {
            for c in &mut targets {
                let (lineup, schedules) = match &schedule {
                    Some(s) => {
                        let svc: Vec<(String, tb_psip::lineup::SourceId, String)> = c
                            .services
                            .iter()
                            .zip(c.lineup.programs())
                            .map(|(id, p)| (id.clone(), p.channel.source_id, id.to_uppercase()))
                            .collect();
                        // The PMT carries the current event's rating (SCTE 54 §7.9.3.11).
                        let programs: Vec<_> = c
                            .lineup
                            .programs()
                            .iter()
                            .zip(&c.services)
                            .map(|(p, id)| {
                                let mut p = p.clone();
                                p.rating = s.current_rating(id, now);
                                p
                            })
                            .collect();
                        let lineup = tb_psip::lineup::Carrier::new(c.lineup.tsid(), programs)
                            .unwrap_or_else(|_| c.lineup.clone());
                        (lineup, s.schedules(&svc))
                    }
                    None => (c.lineup.clone(), Vec::<Schedule>::new()),
                };
                match build_with_ratings(
                    &lineup,
                    &schedules,
                    &guide,
                    ratings,
                    now,
                    leap,
                    &mut c.versions,
                ) {
                    Ok(set) => {
                        if c.tx.try_send(CarrierMsg::Tables(set)).is_err() {
                            tracing::warn!(
                                tsid = c.lineup.tsid(),
                                "table set not delivered: queue full"
                            );
                            cycle.add("queue_full", 1u64).level(tracing::Level::WARN);
                        } else {
                            cycle.add("delivered", 1u64);
                        }
                    }
                    Err(e) => {
                        tracing::error!(tsid = c.lineup.tsid(), "tables: {e}");
                        cycle.add("failed", 1u64).level(tracing::Level::ERROR);
                    }
                }
            }
        }
        if let Some(f) = &file
            && targets.iter().any(|c| c.versions.dirty > 0)
        {
            let mut all: Vec<(u16, &mut Versions)> = targets
                .iter_mut()
                .map(|c| (c.lineup.tsid(), &mut c.versions))
                .collect();
            cycle.set("versions_saved", true);
            if let Err(e) = f.save(&mut all) {
                tracing::warn!("saving table versions: {e}");
                cycle.error(format!("saving table versions: {e}"));
            }
        }
        drop(cycle);
        t += period.max(HZ);
        if !clock.sleep_until(t) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let f = VersionFile::new(dir.path().join("versions.json"));
        let mut v = Versions::new();
        let k = Key {
            pid: 0x1FFB,
            table_id: 0xC9,
            extension: Some(0x101),
        };
        let build = |c: u8| {
            move |ver: Version| {
                let h = tb_ts::section::Long {
                    table_id: tb_ts::section::TableId::CVCT,
                    extension: 0x101,
                    version: ver,
                    current: true,
                    number: 0,
                    last: 0,
                };
                Ok(vec![tb_ts::section::Section::long(
                    tb_ts::section::Kind::Private1024,
                    h,
                    &[c],
                )?])
            }
        };
        v.stamp(k, build(1)).unwrap();
        v.stamp(k, build(2)).unwrap();
        f.save(&mut [(0x101, &mut v)]).unwrap();
        let mut w = f.load(0x101);
        assert_eq!(w.stamp(k, build(2)).unwrap().0.get(), 1);
        assert_eq!(w.stamp(k, build(3)).unwrap().0.get(), 2);
        assert!(f.load(0x999).export().is_empty());
    }
}
