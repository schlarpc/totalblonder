//! The OpenTelemetry SDK's exporters, OTLP/HTTP with protobuf bodies.
//!
//! Logs go through the SDK's batch processor (a thread of its own; a full queue drops records
//! rather than block the caller), metrics through its periodic reader (another thread). The
//! HTTP client is reqwest's blocking one over rustls with the system's roots.

use std::sync::Arc;

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::{Key as OtelKey, KeyValue};
use opentelemetry_otlp::{Protocol, WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, Temporality};
use tracing::Level;

use crate::config::{Config, Error};
use crate::layer::{Record, Sink};
use crate::{Key, Val};

/// The providers, when configured.
pub(crate) struct Otlp {
    logs: Option<SdkLoggerProvider>,
    metrics: Option<SdkMeterProvider>,
    sink: Option<Arc<dyn Sink>>,
}

impl std::fmt::Debug for Otlp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Otlp")
            .field("logs", &self.logs.is_some())
            .field("metrics", &self.metrics.is_some())
            .finish()
    }
}

fn err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Exporter(format!("{what}: {e}"))
}

impl Otlp {
    pub(crate) fn new(cfg: &Config) -> Result<Otlp, Error> {
        if cfg.logs.is_none() && cfg.metrics.is_none() {
            return Ok(Otlp {
                logs: None,
                metrics: None,
                sink: None,
            });
        }
        // reqwest is built without a crypto provider; ring is the process's. Already installed
        // is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::blocking::Client::builder()
            .timeout(cfg.timeout)
            .user_agent(concat!("totalblonder-otlp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| err("HTTP client", e))?;
        let instance = cfg.instance_name();
        let resource = Resource::builder()
            .with_service_name(cfg.service_name.clone())
            .with_attributes(
                [
                    KeyValue::new("service.instance.id", instance.clone()),
                    KeyValue::new("host.name", instance),
                ]
                .into_iter()
                .chain(
                    cfg.resource
                        .iter()
                        .map(|(k, v)| KeyValue::new(k.clone(), v.clone())),
                ),
            )
            .build();
        let headers: std::collections::HashMap<String, String> =
            cfg.headers.iter().cloned().collect();

        let logs = match &cfg.logs {
            Some(url) => {
                let exporter = opentelemetry_otlp::LogExporter::builder()
                    .with_http()
                    .with_http_client(client.clone())
                    .with_protocol(Protocol::HttpBinary)
                    .with_endpoint(url.clone())
                    .with_timeout(cfg.timeout)
                    .with_headers(headers.clone())
                    .build()
                    .map_err(|e| err("logs exporter", e))?;
                Some(
                    SdkLoggerProvider::builder()
                        .with_resource(resource.clone())
                        .with_batch_exporter(exporter)
                        .build(),
                )
            }
            None => None,
        };
        let metrics = match &cfg.metrics {
            Some(url) => {
                let exporter = opentelemetry_otlp::MetricExporter::builder()
                    .with_http()
                    .with_http_client(client)
                    .with_protocol(Protocol::HttpBinary)
                    .with_endpoint(url.clone())
                    .with_timeout(cfg.timeout)
                    .with_headers(headers)
                    .with_temporality(Temporality::Cumulative)
                    .build()
                    .map_err(|e| err("metrics exporter", e))?;
                let reader = PeriodicReader::builder(exporter)
                    .with_interval(cfg.metrics_interval)
                    .build();
                Some(
                    SdkMeterProvider::builder()
                        .with_resource(resource)
                        .with_reader(reader)
                        .build(),
                )
            }
            None => None,
        };
        let sink = logs.as_ref().map(|p| {
            Arc::new(LogSink {
                logger: p.logger("totalblonder"),
            }) as Arc<dyn Sink>
        });
        Ok(Otlp {
            logs,
            metrics,
            sink,
        })
    }

    pub(crate) fn sink(&self) -> Option<Arc<dyn Sink>> {
        self.sink.clone()
    }

    pub(crate) fn exports_logs(&self) -> bool {
        self.logs.is_some()
    }

    pub(crate) fn meter(&self) -> Option<opentelemetry::metrics::Meter> {
        self.metrics.as_ref().map(|p| p.meter("totalblonder"))
    }

    /// Flushes what is queued and stops the exporters' threads.
    pub(crate) fn shutdown(&mut self) {
        if let Some(p) = self.metrics.take()
            && let Err(e) = p.shutdown()
        {
            tracing::warn!("metrics exporter shutdown: {e}");
        }
        if let Some(p) = self.logs.take()
            && let Err(e) = p.shutdown()
        {
            tracing::warn!("logs exporter shutdown: {e}");
        }
    }
}

struct LogSink {
    logger: SdkLogger,
}

fn key(k: &Key) -> OtelKey {
    match k {
        Key::Borrowed(s) => OtelKey::from_static_str(s),
        Key::Owned(s) => OtelKey::new(s.clone()),
    }
}

fn any(v: &Val) -> AnyValue {
    match v {
        Val::Bool(b) => AnyValue::Boolean(*b),
        Val::I64(n) => AnyValue::Int(*n),
        Val::U64(n) => AnyValue::Int(i64::try_from(*n).unwrap_or(i64::MAX)),
        Val::F64(x) => AnyValue::Double(*x),
        Val::Str(s) => AnyValue::String(s.clone().into_owned().into()),
    }
}

fn severity(l: Level) -> (Severity, &'static str) {
    match l {
        Level::TRACE => (Severity::Trace, "TRACE"),
        Level::DEBUG => (Severity::Debug, "DEBUG"),
        Level::INFO => (Severity::Info, "INFO"),
        Level::WARN => (Severity::Warn, "WARN"),
        Level::ERROR => (Severity::Error, "ERROR"),
    }
}

impl Sink for LogSink {
    fn emit(&self, r: &Record<'_>) {
        let mut rec = self.logger.create_log_record();
        rec.set_timestamp(r.time);
        rec.set_observed_timestamp(std::time::SystemTime::now());
        let (sev, text) = severity(r.level);
        rec.set_severity_number(sev);
        rec.set_severity_text(text);
        rec.set_body(AnyValue::String(r.body.to_owned().into()));
        rec.set_target(r.target.to_owned());
        rec.add_attribute(OtelKey::from_static_str("event.kind"), r.kind.as_str());
        if r.kind == crate::layer::Kind::Wide {
            rec.add_attribute(OtelKey::from_static_str("event.name"), r.body.to_owned());
        }
        for (k, v) in r.fields {
            rec.add_attribute(key(k), any(v));
        }
        if let Some(d) = r.duration {
            rec.add_attribute(
                OtelKey::from_static_str("duration_ms"),
                d.as_secs_f64() * 1e3,
            );
        }
        self.logger.emit(rec);
    }
}
