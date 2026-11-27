//! The server's telemetry (docs/SERVER.md "Telemetry"), on top of `tb-telemetry`:
//!
//! - every series of the [`Registry`] as an OpenTelemetry metric: a counter when it only grows
//!   (its name ends in `_total`), a gauge otherwise, its labels as attributes;
//! - at an interval, one wide event per carrier, service, input and the process, with every
//!   series of that entity as a field: what a counter gained over the interval, a gauge's value;
//! - the process's own statistics, read from `/proc` when asked.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tb_telemetry::opentelemetry::KeyValue;
use tb_telemetry::opentelemetry::metrics::Meter;
use tb_telemetry::{Val, Wide};

use crate::control::{Registry, Value};

/// Ticks per second of `/proc`'s CPU times (USER_HZ, 100 on every Linux architecture we run).
const USER_HZ: u64 = 100;

/// Registers the process's statistics: uptime, resident memory, CPU time, threads, open files.
pub fn register_process(reg: &mut Registry) {
    let start = Instant::now();
    let probe = |f: fn() -> u64| Value::Probe(Arc::new(f));
    reg.add(
        "process_uptime_seconds",
        vec![],
        Value::Probe(Arc::new(move || start.elapsed().as_secs())),
    );
    reg.add(
        "process_resident_bytes",
        vec![],
        probe(|| status_field("VmRSS:") * 1024),
    );
    reg.add(
        "process_cpu_milliseconds_total",
        vec![],
        probe(|| {
            std::fs::read_to_string("/proc/self/stat")
                .ok()
                .and_then(|s| cpu_ticks(&s))
                .map_or(0, |(_, t)| t * 1000 / USER_HZ)
        }),
    );
    reg.add(
        "process_threads",
        vec![],
        probe(|| status_field("Threads:")),
    );
    reg.add(
        "process_open_fds",
        vec![],
        probe(|| std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count() as u64)),
    );
}

/// A number from `/proc/self/status` (`VmRSS:` is in KiB).
fn status_field(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix(key))
                .and_then(|v| v.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(0)
}

/// `(comm, utime + stime)` from a `stat` file of `/proc`.
fn cpu_ticks(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_owned();
    // after the name: state is field 3, utime 14, stime 15
    let mut f = stat.get(close + 1..)?.split_whitespace().skip(11);
    let u: u64 = f.next()?.parse().ok()?;
    let s: u64 = f.next()?.parse().ok()?;
    Some((comm, u + s))
}

/// CPU ticks per thread name (threads of one name summed).
fn thread_ticks() -> HashMap<String, u64> {
    let mut m = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
        for e in dir.flatten() {
            if let Ok(s) = std::fs::read_to_string(e.path().join("stat"))
                && let Some((name, t)) = cpu_ticks(&s)
            {
                *m.entry(name).or_insert(0) += t;
            }
        }
    }
    m
}

/// Exports every series of `reg` as an OpenTelemetry metric on `meter`. The returned instruments
/// must be kept as long as they are to be exported.
pub fn export_metrics(meter: &Meter, reg: &Arc<Registry>) -> Vec<Box<dyn std::any::Any + Send>> {
    let mut names: Vec<&'static str> = Vec::new();
    for s in reg.series() {
        if !names.contains(&s.name) {
            names.push(s.name);
        }
    }
    let mut keep: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
    for name in names {
        let idx: Vec<usize> = (0..reg.series().len())
            .filter(|&i| reg.series()[i].name == name)
            .collect();
        let attrs: Vec<Vec<KeyValue>> = idx
            .iter()
            .map(|&i| {
                reg.series()[i]
                    .labels
                    .iter()
                    .map(|(k, v)| KeyValue::new(*k, v.clone()))
                    .collect()
            })
            .collect();
        let cumulative = idx.first().is_some_and(|&i| reg.series()[i].cumulative());
        let r = reg.clone();
        let observe = move |o: &dyn tb_telemetry::opentelemetry::metrics::AsyncInstrument<u64>| {
            for (&i, a) in idx.iter().zip(&attrs) {
                o.observe(r.series()[i].value.get(), a);
            }
        };
        if cumulative {
            keep.push(Box::new(
                meter
                    .u64_observable_counter(name)
                    .with_callback(observe)
                    .build(),
            ));
        } else {
            keep.push(Box::new(
                meter
                    .u64_observable_gauge(name)
                    .with_callback(observe)
                    .build(),
            ));
        }
    }
    keep
}

/// What a summary event is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Entity {
    Process,
    Carrier(String),
    Service(String),
    Input(String),
}

impl Entity {
    fn event(&self) -> &'static str {
        match self {
            Entity::Process => "process.interval",
            Entity::Carrier(_) => "carrier.interval",
            Entity::Service(_) => "service.interval",
            Entity::Input(_) => "input.interval",
        }
    }
}

/// A service's description, for its summary events.
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    /// The service's id.
    pub service: String,
    /// Its carrier's id.
    pub carrier: u8,
    /// Its program number on the carrier (1-based).
    pub program: usize,
    /// Its virtual channel (`50.1`).
    pub channel: String,
    /// Its formats, rung 0 first.
    pub ladder: Vec<String>,
    /// Its live and standby inputs.
    pub inputs: [Option<String>; 2],
}

/// Settings of the summary thread.
pub struct Summaries {
    /// Every series.
    pub registry: Arc<Registry>,
    /// How often.
    pub every: Duration,
    /// The services.
    pub services: Vec<ServiceInfo>,
}

impl Summaries {
    /// Which entity a series belongs to, and the labels left to name its field by.
    fn classify<'a>(&self, labels: &'a [(&'static str, String)]) -> (Entity, Vec<&'a str>) {
        let get = |k: &str| {
            labels
                .iter()
                .find(|(l, _)| *l == k)
                .map(|(_, v)| v.as_str())
        };
        let rest = |skip: &[&str]| {
            labels
                .iter()
                .filter(|(k, _)| !skip.contains(k))
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
        };
        if let Some(s) = get("service") {
            return (Entity::Service(s.to_owned()), rest(&["service"]));
        }
        if let (Some(c), Some(p)) = (get("carrier"), get("program")) {
            let svc = self
                .services
                .iter()
                .find(|s| s.carrier.to_string() == c && s.program.to_string() == p);
            if let Some(s) = svc {
                let mut r = rest(&["carrier", "program", "track"]);
                if let Some(t) = get("track") {
                    r.push(t);
                }
                return (Entity::Service(s.service.clone()), r);
            }
        }
        if let Some(c) = get("carrier") {
            return (Entity::Carrier(c.to_owned()), rest(&["carrier"]));
        }
        if let Some(i) = get("input") {
            return (Entity::Input(i.to_owned()), rest(&["input"]));
        }
        (Entity::Process, rest(&[]))
    }

    /// Runs until `stop`: one wide event per entity every `every`.
    pub fn run(self, stop: &AtomicBool) {
        let _ctx = tracing::info_span!("summaries").entered();
        let series = self.registry.series();
        let mut prev: Vec<u64> = series.iter().map(|s| s.value.get()).collect();
        let mut prev_threads = thread_ticks();
        let mut last = Instant::now();
        loop {
            let due = last + self.every;
            while Instant::now() < due {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100).min(due - Instant::now()));
            }
            let interval = last.elapsed();
            last = Instant::now();
            let mut by: std::collections::BTreeMap<Entity, Vec<(String, Val)>> =
                std::collections::BTreeMap::new();
            for (i, s) in series.iter().enumerate() {
                let now = s.value.get();
                let (entity, rest) = self.classify(&s.labels);
                let cumulative = s.cumulative();
                let mut field = if cumulative {
                    s.name.strip_suffix("_total").unwrap_or(s.name).to_owned()
                } else {
                    s.name.to_owned()
                };
                for r in &rest {
                    field.push('.');
                    field.push_str(r);
                }
                let v = if cumulative {
                    now.saturating_sub(prev[i])
                } else {
                    now
                };
                prev[i] = now;
                by.entry(entity).or_default().push((field, Val::U64(v)));
            }
            // the process: CPU per thread name, as a share of one core over the interval
            let threads = thread_ticks();
            let ms = interval.as_secs_f64() * 1e3;
            let mut cpu: Vec<(String, Val)> = threads
                .iter()
                .map(|(name, &t)| {
                    let d = t.saturating_sub(prev_threads.get(name).copied().unwrap_or(0));
                    let pct = (d * 1000 / USER_HZ) as f64 / ms * 100.0;
                    (format!("thread_cpu_pct.{name}"), Val::F64(pct))
                })
                .collect();
            cpu.sort_by(|a, b| a.0.cmp(&b.0));
            by.entry(Entity::Process).or_default().extend(cpu);
            prev_threads = threads;

            for (entity, fields) in by {
                let ev = Wide::new(entity.event()).with("interval_s", interval.as_secs_f64());
                match &entity {
                    Entity::Process => {}
                    Entity::Carrier(c) => {
                        ev.set("carrier", c.clone());
                    }
                    Entity::Input(i) => {
                        ev.set("input", i.clone());
                        if let Some(s) = self
                            .services
                            .iter()
                            .find(|s| s.inputs.iter().flatten().any(|x| x == i))
                        {
                            ev.set("service", s.service.clone());
                        }
                    }
                    Entity::Service(name) => {
                        ev.set("service", name.clone());
                        if let Some(s) = self.services.iter().find(|s| &s.service == name) {
                            ev.set("carrier", u64::from(s.carrier))
                                .set("program", s.program)
                                .set("channel", s.channel.clone());
                            let rung = fields
                                .iter()
                                .find(|(k, _)| k == "video_format_rung")
                                .and_then(|(_, v)| match v {
                                    Val::U64(n) => Some(*n as usize),
                                    _ => None,
                                })
                                .unwrap_or(0);
                            if let Some(f) = s.ladder.get(rung) {
                                ev.set("format", f.clone());
                            }
                        }
                        let source = fields
                            .iter()
                            .find(|(k, _)| k == "program_source")
                            .map(|(_, v)| v.clone());
                        if let Some(Val::U64(n)) = source {
                            let name = match n {
                                1 => "live",
                                2 => "standby",
                                3 => "slate",
                                _ => "none",
                            };
                            ev.set("source", name);
                        }
                    }
                }
                for (k, v) in fields {
                    ev.set(k, v);
                }
                ev.emit();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_lines() {
        let s = "1234 (mux-1 (x)) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0 20 0 9 0 100 0 0";
        assert_eq!(cpu_ticks(s), Some(("mux-1 (x)".into(), 300)));
    }

    #[test]
    fn series_fall_to_their_entities() {
        let s = Summaries {
            registry: Arc::new(Registry::default()),
            every: Duration::from_secs(10),
            services: vec![ServiceInfo {
                service: "bars".into(),
                carrier: 1,
                program: 2,
                channel: "50.2".into(),
                ladder: vec!["720p".into()],
                inputs: [Some("cam".into()), None],
            }],
        };
        let l = |v: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
            v.iter().map(|(k, v)| (*k, (*v).to_owned())).collect()
        };
        let cases = [
            (
                l(&[("carrier", "1"), ("class", "null")]),
                Entity::Carrier("1".into()),
                vec!["null"],
            ),
            (
                l(&[("carrier", "1"), ("program", "2")]),
                Entity::Service("bars".into()),
                vec![],
            ),
            (
                l(&[("carrier", "1"), ("program", "2"), ("track", "0")]),
                Entity::Service("bars".into()),
                vec!["0"],
            ),
            (
                l(&[("service", "bars")]),
                Entity::Service("bars".into()),
                vec![],
            ),
            (l(&[("input", "cam")]), Entity::Input("cam".into()), vec![]),
            (l(&[]), Entity::Process, vec![]),
        ];
        for (labels, entity, rest) in cases {
            let (e, r) = s.classify(&labels);
            assert_eq!(e, entity, "{labels:?}");
            assert_eq!(r, rest, "{labels:?}");
        }
    }
}
