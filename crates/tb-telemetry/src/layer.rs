//! The `tracing` layer that turns spans into context and wide events into records.
//!
//! Every span keeps its fields (those it was made with, those recorded later and those set
//! through [`crate::annotate`]) in a [`Fields`] extension. An event becomes a [`Record`] with
//! its own fields over its spans' (inner spans win); a span whose target is [`crate::TARGET`]
//! becomes one when it closes.

use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record as SpanRecord};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::{Key, TARGET, Val};

/// A span's fields, and how a wide event is to be reported.
#[derive(Debug, Default)]
pub struct Fields {
    vals: Vec<(Key, Val)>,
    /// Severity of a wide event (INFO when unset).
    pub level: Option<Level>,
    /// Reported without a duration.
    pub instant: bool,
}

impl Fields {
    /// Sets `key`, replacing an earlier value.
    pub fn set(&mut self, key: Key, val: Val) {
        match self.vals.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = val,
            None => self.vals.push((key, val)),
        }
    }

    /// Adds to a numeric `key`.
    pub fn add(&mut self, key: Key, val: Val) {
        match self.vals.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = slot.1.plus(val),
            None => self.vals.push((key, val)),
        }
    }

    /// The fields, in the order first set.
    pub fn vals(&self) -> &[(Key, Val)] {
        &self.vals
    }
}

struct Started {
    at: Instant,
    wall: SystemTime,
}

/// Runs `f` on a span's [`Fields`]; nothing when the span is disabled or the subscriber is not
/// built on a [`tracing_subscriber::Registry`].
pub(crate) fn with_fields(span: &tracing::Span, f: impl FnOnce(&mut Fields)) {
    span.with_subscriber(|(id, dispatch)| {
        let Some(reg) = dispatch.downcast_ref::<tracing_subscriber::Registry>() else {
            return;
        };
        let Some(data) = reg.span(id) else {
            return;
        };
        let mut ext = data.extensions_mut();
        if let Some(fields) = ext.get_mut::<Fields>() {
            f(fields);
        } else {
            let mut fields = Fields::default();
            f(&mut fields);
            ext.insert(fields);
        }
    });
}

/// What a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An ordinary log line (`tracing::info!` and friends).
    Log,
    /// A wide event.
    Wide,
}

impl Kind {
    /// `log` or `wide`.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Log => "log",
            Kind::Wide => "wide",
        }
    }
}

/// One thing to report.
#[derive(Debug)]
pub struct Record<'a> {
    /// When it happened (a wide event: when it started).
    pub time: SystemTime,
    /// Severity.
    pub level: Level,
    /// Log line or wide event.
    pub kind: Kind,
    /// The message; a wide event's name.
    pub body: &'a str,
    /// The `tracing` target (a module path; [`TARGET`] for wide events).
    pub target: &'a str,
    /// Context first, its own last; no key twice.
    pub fields: &'a [(Key, Val)],
    /// How long a wide event lasted.
    pub duration: Option<Duration>,
}

/// Where records go.
pub trait Sink: Send + Sync + 'static {
    /// Takes one record. Must not block for long: it is called on the thread that made it.
    fn emit(&self, record: &Record<'_>);
}

/// The layer. See the module documentation.
pub struct WideLayer {
    sink: Option<Arc<dyn Sink>>,
    log_level: Level,
    echo_all: bool,
}

impl std::fmt::Debug for WideLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WideLayer")
            .field("sink", &self.sink.is_some())
            .field("log_level", &self.log_level)
            .finish()
    }
}

/// Targets whose events never go to the sink: the exporter's own stack, which would otherwise
/// report on its own reports.
const INTERNAL: [&str; 7] = [
    "opentelemetry",
    "reqwest",
    "hyper",
    "h2",
    "rustls",
    "tokio",
    "tower",
];

fn internal(target: &str) -> bool {
    INTERNAL.iter().any(|p| target.starts_with(p))
}

impl WideLayer {
    /// Log lines at `log_level` or more severe, and every wide event, go to `sink`. Wide events
    /// are also written to stderr when there is no sink, or when they are WARN or worse (log
    /// lines reach stderr through the formatter beside this layer).
    pub fn new(sink: Option<Arc<dyn Sink>>, log_level: Level) -> WideLayer {
        WideLayer {
            echo_all: sink.is_none(),
            sink,
            log_level,
        }
    }

    fn report(&self, r: &Record<'_>) {
        if let Some(s) = &self.sink {
            s.emit(r);
        }
        if r.kind == Kind::Wide && (self.echo_all || r.level <= Level::WARN) {
            echo(r);
        }
    }
}

fn merge(into: &mut Vec<(Key, Val)>, from: &[(Key, Val)]) {
    for (k, v) in from {
        match into.iter_mut().find(|(k2, _)| k2 == k) {
            Some(slot) => slot.1 = v.clone(),
            None => into.push((k.clone(), v.clone())),
        }
    }
}

#[derive(Default)]
struct Visitor {
    message: Option<String>,
    fields: Fields,
}

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(s);
        } else {
            self.fields.set(Key::Borrowed(field.name()), Val::from(s));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        } else {
            self.fields
                .set(Key::Borrowed(field.name()), Val::from(value));
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .set(Key::Borrowed(field.name()), Val::I64(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .set(Key::Borrowed(field.name()), Val::U64(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields
            .set(Key::Borrowed(field.name()), Val::F64(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .set(Key::Borrowed(field.name()), Val::Bool(value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields
            .set(Key::Borrowed(field.name()), Val::from(value.to_string()));
    }
}

impl<S> tracing_subscriber::Layer<S> for WideLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn max_level_hint(&self) -> Option<tracing_subscriber::filter::LevelFilter> {
        // Context spans and wide events are INFO; log lines below that are wanted only when
        // they are exported.
        Some(tracing_subscriber::filter::LevelFilter::from_level(
            self.log_level.max(Level::INFO),
        ))
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = Visitor::default();
        attrs.record(&mut v);
        let mut ext = span.extensions_mut();
        match ext.get_mut::<Fields>() {
            Some(f) => merge(&mut f.vals, &v.fields.vals),
            None => ext.insert(v.fields),
        }
        ext.insert(Started {
            at: Instant::now(),
            wall: SystemTime::now(),
        });
    }

    fn on_record(&self, id: &Id, values: &SpanRecord<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = Visitor::default();
        values.record(&mut v);
        let mut ext = span.extensions_mut();
        match ext.get_mut::<Fields>() {
            Some(f) => merge(&mut f.vals, &v.fields.vals),
            None => ext.insert(v.fields),
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        if self.sink.is_none() || *meta.level() > self.log_level || internal(meta.target()) {
            return;
        }
        let mut fields = Vec::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(f) = span.extensions().get::<Fields>() {
                    merge(&mut fields, &f.vals);
                }
            }
        }
        // A log line made inside a wide event carries its fields, but is not that event.
        fields.retain(|(k, _)| k != "event.name");
        let mut v = Visitor::default();
        event.record(&mut v);
        merge(&mut fields, &v.fields.vals);
        self.report(&Record {
            time: SystemTime::now(),
            level: *meta.level(),
            kind: Kind::Log,
            body: v.message.as_deref().unwrap_or(""),
            target: meta.target(),
            fields: &fields,
            duration: None,
        });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        if span.metadata().target() != TARGET {
            return;
        }
        let mut fields = Vec::new();
        let ancestors: Vec<_> = span.scope().skip(1).collect();
        for a in ancestors.iter().rev() {
            if let Some(f) = a.extensions().get::<Fields>() {
                merge(&mut fields, &f.vals);
            }
        }
        let ext = span.extensions();
        let (level, instant) = match ext.get::<Fields>() {
            Some(f) => {
                merge(&mut fields, &f.vals);
                (f.level.unwrap_or(Level::INFO), f.instant)
            }
            None => (Level::INFO, false),
        };
        let (wall, elapsed) = match ext.get::<Started>() {
            Some(s) => (s.wall, s.at.elapsed()),
            None => (SystemTime::now(), Duration::ZERO),
        };
        let name = match fields.iter().position(|(k, _)| k == "event.name") {
            Some(i) => match fields.remove(i).1 {
                Val::Str(s) => s.into_owned(),
                other => other.to_string(),
            },
            None => "wide".to_owned(),
        };
        self.report(&Record {
            time: wall,
            level,
            kind: Kind::Wide,
            body: &name,
            target: TARGET,
            fields: &fields,
            duration: (!instant).then_some(elapsed),
        });
    }
}

/// One line on stderr: `<time> <LEVEL> <name> k=v … duration_ms=…`.
fn echo(r: &Record<'_>) {
    let mut line = format!("{} {:>5} {}", rfc3339(r.time), r.level, r.body);
    for (k, v) in r.fields {
        let _ = write!(line, " {k}={v}");
    }
    if let Some(d) = r.duration {
        let _ = write!(line, " duration_ms={}", Val::ms(d));
    }
    line.push('\n');
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

/// `2026-11-27T10:00:00.123456Z`.
pub(crate) fn rfc3339(t: SystemTime) -> String {
    let d = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = d.as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        d.subsec_micros()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::wide;

    /// Kind, level, body, fields, has a duration.
    type Captured = (Kind, Level, String, Vec<(String, String)>, bool);

    #[derive(Default)]
    struct Capture(Mutex<Vec<Captured>>);

    impl Sink for Capture {
        fn emit(&self, r: &Record<'_>) {
            let fields = r
                .fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            self.0.lock().unwrap().push((
                r.kind,
                r.level,
                r.body.to_owned(),
                fields,
                r.duration.is_some(),
            ));
        }
    }

    fn get<'a>(f: &'a [(String, String)], k: &str) -> Option<&'a str> {
        f.iter().find(|(k2, _)| k2 == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn wide_events_carry_context_and_fields() {
        let cap = Arc::new(Capture::default());
        let sub = tracing_subscriber::registry().with(WideLayer::new(
            Some(cap.clone() as Arc<dyn Sink>),
            Level::INFO,
        ));
        tracing::subscriber::with_default(sub, || {
            let ctx = tracing::info_span!("carrier", carrier = 3u64).entered();
            crate::annotate(&tracing::Span::current(), "tsid", 7u64);
            let ev = wide!("input.session", input = "cam1", "rtmp.app" = "live");
            ev.add("bytes", 10u64).add("bytes", 5u64);
            {
                let _in = ev.enter();
                tracing::warn!(code = 4, "inside");
                wide!("source.switch", to = "standby").emit();
            }
            tracing::debug!("below the level");
            ev.error("eof");
            drop(ev);
            drop(ctx);
        });
        let got = cap.0.lock().unwrap();
        assert_eq!(got.len(), 3, "{got:?}");

        let (kind, level, body, f, dur) = &got[0];
        assert_eq!(
            (*kind, *level, body.as_str()),
            (Kind::Log, Level::WARN, "inside")
        );
        assert_eq!(get(f, "carrier"), Some("3"));
        assert_eq!(get(f, "input"), Some("cam1"));
        assert_eq!(get(f, "code"), Some("4"));
        assert_eq!(
            get(f, "event.name"),
            None,
            "a log line is not the wide event"
        );
        assert!(!dur);

        let (kind, _, body, f, dur) = &got[1];
        assert_eq!((*kind, body.as_str()), (Kind::Wide, "source.switch"));
        assert_eq!(
            get(f, "input"),
            Some("cam1"),
            "the outer wide event is context"
        );
        assert_eq!(get(f, "to"), Some("standby"));
        assert!(!dur, "emit() reports no duration");

        let (kind, level, body, f, dur) = &got[2];
        assert_eq!(
            (*kind, *level, body.as_str()),
            (Kind::Wide, Level::ERROR, "input.session")
        );
        assert_eq!(get(f, "carrier"), Some("3"));
        assert_eq!(get(f, "tsid"), Some("7"));
        assert_eq!(get(f, "rtmp.app"), Some("live"));
        assert_eq!(get(f, "bytes"), Some("15"));
        assert_eq!(get(f, "outcome"), Some("error"));
        assert_eq!(get(f, "event.name"), None);
        assert!(dur);
    }

    #[test]
    fn no_subscriber_is_harmless() {
        let ev = wide!("x", a = 1);
        ev.set("b", 2).add("c", 3u64);
        drop(ev);
    }

    #[test]
    fn rfc3339_dates() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_micros(1_795_779_000_123_456);
        assert_eq!(rfc3339(t), "2026-11-27T11:30:00.123456Z");
        assert_eq!(
            rfc3339(SystemTime::UNIX_EPOCH),
            "1970-01-01T00:00:00.000000Z"
        );
    }
}
