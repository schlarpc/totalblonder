//! Installing the subscriber: stderr, the wide-event layer and, when configured, OTLP.

use std::sync::Arc;
use std::time::Duration;

use tracing::Level;
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt;

use crate::layer::{Sink, WideLayer};

/// What to export and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The OTLP/HTTP logs URL, whole (`…/v1/logs`, or VictoriaLogs'
    /// `…/insert/opentelemetry/v1/logs`): log lines and wide events. `None`: not exported.
    pub logs: Option<String>,
    /// The OTLP/HTTP metrics URL, whole (`…/v1/metrics`, or VictoriaMetrics'
    /// `…/opentelemetry/v1/metrics`). `None`: not exported.
    pub metrics: Option<String>,
    /// The least severe log line exported (wide events are always exported).
    pub log_level: Level,
    /// How often metrics are pushed.
    pub metrics_interval: Duration,
    /// One export request's time limit.
    pub timeout: Duration,
    /// `service.name`.
    pub service_name: String,
    /// `service.instance.id` and `host.name`; `None`: the host's name.
    pub instance: Option<String>,
    /// More resource attributes.
    pub resource: Vec<(String, String)>,
    /// More HTTP headers on every request (an authorization, VictoriaLogs' `VL-Stream-Fields`).
    pub headers: Vec<(String, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            logs: None,
            metrics: None,
            log_level: Level::INFO,
            metrics_interval: Duration::from_secs(10),
            timeout: Duration::from_secs(5),
            service_name: "totalblonder".into(),
            instance: None,
            resource: Vec::new(),
            headers: Vec::new(),
        }
    }
}

impl Config {
    /// The host's name (`/proc/sys/kernel/hostname`), or `unknown`.
    pub fn host_name() -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_owned())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into())
    }

    /// `instance`, or the host's name.
    pub fn instance_name(&self) -> String {
        self.instance.clone().unwrap_or_else(Config::host_name)
    }
}

/// Telemetry could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An exporter could not be built.
    #[error("telemetry: {0}")]
    Exporter(String),
    /// A subscriber was already installed.
    #[error("telemetry: a tracing subscriber is already installed")]
    AlreadyInstalled,
}

/// The installed telemetry. Dropping it flushes and stops the exporters; keep it until the
/// process ends.
#[derive(Debug)]
pub struct Telemetry {
    #[cfg(feature = "otlp")]
    otlp: crate::otlp::Otlp,
}

impl Telemetry {
    /// The meter metrics are registered on, when metrics are exported.
    #[cfg(feature = "otlp")]
    pub fn meter(&self) -> Option<opentelemetry::metrics::Meter> {
        self.otlp.meter()
    }

    /// Whether log lines and wide events leave the process.
    pub fn exports_logs(&self) -> bool {
        #[cfg(feature = "otlp")]
        return self.otlp.exports_logs();
        #[cfg(not(feature = "otlp"))]
        false
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        #[cfg(feature = "otlp")]
        self.otlp.shutdown();
    }
}

/// Installs the process's subscriber: log lines at INFO and wide events to stderr (see
/// [`WideLayer::new`] for which), and to OTLP when `cfg` names endpoints.
pub fn init(cfg: &Config) -> Result<Telemetry, Error> {
    #[cfg(feature = "otlp")]
    let otlp = crate::otlp::Otlp::new(cfg)?;
    #[cfg(feature = "otlp")]
    let sink: Option<Arc<dyn Sink>> = otlp.sink();
    #[cfg(not(feature = "otlp"))]
    let sink: Option<Arc<dyn Sink>> = {
        if cfg.logs.is_some() || cfg.metrics.is_some() {
            return Err(Error::Exporter(
                "built without the `otlp` feature: no exporters".into(),
            ));
        }
        None
    };
    // The exporter's own complaints (a collector that is down) are kept to errors, at most one
    // per batch.
    let stderr_filter = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("opentelemetry", LevelFilter::ERROR)
        .with_target("opentelemetry_sdk", LevelFilter::ERROR)
        .with_target("opentelemetry-otlp", LevelFilter::ERROR)
        .with_target("opentelemetry_otlp", LevelFilter::ERROR)
        .with_target("reqwest", LevelFilter::WARN)
        .with_target("hyper", LevelFilter::WARN);
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(stderr_filter);
    let level = if sink.is_some() {
        cfg.log_level
    } else {
        Level::INFO
    };
    let subscriber = tracing_subscriber::registry()
        .with(WideLayer::new(sink, level))
        .with(fmt);
    tracing::subscriber::set_global_default(subscriber).map_err(|_| Error::AlreadyInstalled)?;
    Ok(Telemetry {
        #[cfg(feature = "otlp")]
        otlp,
    })
}
