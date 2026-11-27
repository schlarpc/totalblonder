//! The modulator's supervisor (SPEC §10) in the server: `tb-eqam`'s sans-io controller driven
//! over plain HTTP/1.0 to the box's Boa server. Every request has passed `tb_eqam::screen`
//! before it reaches [`Http::send`]: this module cannot build a request of its own.
//!
//! Mode `report` (the default) never writes; `apply` must be configured explicitly.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use tb_eqam::client::{ClientConfig, Next};
use tb_eqam::controller::Controller;
use tb_eqam::plan::DesiredState;
use tb_eqam::supervisor::{Action, DeviceMetrics, Sample, SupervisorConfig};
use tb_eqam::types::Credentials;
use tb_eqam::{Method, Request, Response, TransportFailure};

use crate::control::Registry;
use crate::metrics::Gauge;
use crate::time::Clock;

/// The HTTP transport: one request per connection, HTTP/1.0, 30 s timeout, no redirects.
pub struct Http {
    addr: SocketAddr,
    host: String,
}

impl Http {
    /// A transport to `base` (`http://host[:port]`).
    pub fn new(base: &str) -> std::io::Result<Self> {
        let hostport = base.trim_start_matches("http://").trim_end_matches('/');
        let with_port = if hostport.contains(':') {
            hostport.to_owned()
        } else {
            format!("{hostport}:80")
        };
        let addr = with_port
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| std::io::Error::other("no address"))?;
        Ok(Http {
            addr,
            host: hostport.to_owned(),
        })
    }

    /// Sends a screened request.
    pub fn send(&mut self, r: &Request) -> Result<Response, TransportFailure> {
        let fail = |e: std::io::Error| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                TransportFailure::Timeout
            }
            _ => TransportFailure::Connection(e.to_string()),
        };
        let timeout = Duration::from_secs(30);
        let mut s =
            TcpStream::connect_timeout(&self.addr, Duration::from_secs(10)).map_err(fail)?;
        s.set_read_timeout(Some(timeout)).map_err(fail)?;
        s.set_write_timeout(Some(timeout)).map_err(fail)?;
        let method = match r.method() {
            Method::Get => "GET",
            Method::Post => "POST",
        };
        let mut head = format!(
            "{method} {} HTTP/1.0\r\nHost: {}\r\n",
            r.target(),
            self.host
        );
        let body = r.body();
        if let (Some(b), Some(ct)) = (&body, r.content_type()) {
            head.push_str(&format!(
                "Content-Type: {ct}\r\nContent-Length: {}\r\n",
                b.len()
            ));
        }
        head.push_str("\r\n");
        s.write_all(head.as_bytes()).map_err(fail)?;
        if let Some(b) = &body {
            s.write_all(b).map_err(fail)?;
        }
        let mut raw = Vec::new();
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 16 << 10];
        loop {
            if Instant::now() > deadline || raw.len() > 4 << 20 {
                return Err(TransportFailure::Timeout);
            }
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&buf[..n]),
                Err(e) => return Err(fail(e)),
            }
        }
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| TransportFailure::Connection("no end of headers".into()))?;
        let status_line = String::from_utf8_lossy(&raw[..split.min(64)]).into_owned();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or_else(|| {
                TransportFailure::Connection(format!("bad status line {status_line:?}"))
            })?;
        Ok(Response {
            status,
            body: raw[split + 4..].to_vec(),
        })
    }
}

/// Key of one device series: name and labels.
type SeriesKey = (&'static str, Vec<(&'static str, String)>);

/// The device metrics for the control API: every series [`tb_eqam::supervisor::DeviceMetrics`]
/// can report for the configured slots, registered at start, plus the alarm count.
#[derive(Debug, Clone, Default)]
pub struct DeviceGauges {
    series: HashMap<SeriesKey, Gauge>,
    /// `device_alarms_total`: alarms raised since start.
    alarms: Gauge,
}

impl DeviceGauges {
    /// Registers the series. Per-carrier series exist for the slots `desired` assigns.
    pub fn register(reg: &mut Registry, desired: &DesiredState) -> Self {
        let mut all = DeviceMetrics {
            uptime_seconds: Some(0),
            rf_locked: [Some(false); 4],
            seconds_since_table_reset: Some(0),
            temperature_tenths_f: Some(0),
            ..DeviceMetrics::default()
        };
        for s in desired.streams.assigned() {
            let i = s.index();
            all.input_present[i] = Some(false);
            all.bitrate_bps[i] = Some(0);
            all.overflow_total[i] = Some(0);
            all.timestamp_errors[i] = Some(0);
            all.ts_sync_errors[i] = Some(0);
        }
        let series = all
            .samples(&Default::default())
            .into_iter()
            .map(|x| {
                let g = reg.gauge(x.name, x.labels.clone());
                ((x.name, x.labels), g)
            })
            .collect();
        DeviceGauges {
            series,
            alarms: reg.gauge("device_alarms_total", vec![]),
        }
    }

    /// Copies the samples into the registered series; others are ignored.
    fn set(&self, samples: Vec<Sample>) {
        for x in samples {
            if let Some(g) = self.series.get(&(x.name, x.labels)) {
                // Every device value is a count, a flag or a positive reading.
                g.set(x.value.max(0.0) as u64);
            }
        }
    }
}

/// Runs the controller until the clock stops.
pub fn run(
    base: String,
    credentials: Credentials,
    supervisor: SupervisorConfig,
    desired: DesiredState,
    gauges: DeviceGauges,
    clock: &dyn Clock,
) {
    let mut http = match Http::new(&base) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(base, "device: {e}");
            return;
        }
    };
    let t0 = Instant::now();
    let mut ctl = Controller::new(
        credentials,
        ClientConfig::default(),
        supervisor,
        desired,
        t0.elapsed(),
    );
    let mut alarms = 0u64;
    let mut log = |actions: Vec<Action>| {
        for a in actions {
            match &a {
                Action::Alarm(al) => {
                    alarms += 1;
                    tracing::warn!("device alarm: {al:?}");
                    let detail = format!("{al:?}");
                    let kind = detail
                        .split(['(', ' ', '{'])
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    let ev = tb_telemetry::wide!("device.alarm", alarm = kind, detail = detail);
                    ev.level(tracing::Level::WARN);
                    ev.emit();
                }
                Action::Clear(k) => {
                    tracing::info!("device alarm cleared: {k:?}");
                    tb_telemetry::wide!("device.alarm_cleared", alarm = format!("{k:?}")).emit();
                }
                Action::WriteLogged(w) => {
                    tracing::warn!("device write: {w:?}");
                    let ev = tb_telemetry::wide!("device.write", detail = format!("{w:?}"));
                    ev.level(tracing::Level::WARN);
                    ev.emit();
                }
                Action::EventLogLine(l) => {
                    tb_telemetry::wide!(
                        "device.event_log",
                        time = l.time.clone(),
                        severity = format!("{:?}", l.severity),
                        message = l.message.clone(),
                    )
                    .emit();
                }
                Action::RateLimited { until } => {
                    tracing::info!("device write held back until {until:?}")
                }
                Action::Job(_) => {}
            }
        }
        alarms
    };
    while !clock.stopping() {
        let now = t0.elapsed();
        let step = ctl.next(now);
        let n = log(step.actions);
        gauges.alarms.set(n);
        let wait = match step.next {
            Next::Send(req) => {
                tracing::debug!(target = %req.target(), "device request");
                let ev = tb_telemetry::wide!(
                    "device.request",
                    method = format!("{:?}", req.method()),
                    // the kind, not the target: its query carries the session id
                    kind = format!("{:?}", req.kind()),
                );
                let res = http.send(req);
                match &res {
                    Ok(r) => {
                        ev.set("status", r.status).set("bytes", r.body.len());
                        if r.status >= 400 {
                            ev.outcome("http_error").level(tracing::Level::WARN);
                        }
                    }
                    Err(e) => {
                        ev.outcome("transport").set("error", format!("{e:?}"));
                        ev.level(tracing::Level::WARN);
                    }
                }
                drop(ev);
                let acts = ctl.on_response(t0.elapsed(), res);
                gauges.alarms.set(log(acts));
                None
            }
            Next::WaitUntil(t) => Some(t.saturating_sub(now)),
            Next::Idle | Next::AwaitingResponse => Some(Duration::from_secs(1)),
        };
        gauges.set(ctl.supervisor().metrics().samples(ctl.client().refused()));
        if let Some(d) = wait {
            std::thread::sleep(d.min(Duration::from_millis(500)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb_eqam::types::Slot;

    #[test]
    fn example_plan_exports_every_device_series() {
        let plant = crate::config::load(crate::config::tests::EXAMPLE).unwrap();
        let d = plant.device.unwrap();
        // The example's carrier 1 carries two programs: MPEG-2 video and one AC-3 track each.
        let p = &d.desired.programs[&Slot::new(1).unwrap()];
        assert_eq!(
            p.iter()
                .map(|p| (p.program_number, p.pmt_pid))
                .collect::<Vec<_>>(),
            [(1, 0x30), (2, 0x40)]
        );
        assert_eq!(p[0].streams[0].pid, 0x31);
        assert_eq!(p[0].streams[0].kind, "MPEG-2 Video");
        let mut reg = Registry::default();
        let g = DeviceGauges::register(&mut reg, &d.desired);
        let mut m = DeviceMetrics {
            reachable: true,
            temperature_tenths_f: Some(804),
            ..DeviceMetrics::default()
        };
        m.timestamp_errors[0] = Some(83);
        m.timestamp_errors[15] = Some(5); // slot 16 is not configured: not exported
        g.set(m.samples(&Default::default()));
        let text = reg.prometheus();
        for line in [
            "device_reachable 1",
            "device_timestamp_errors_total{carrier=\"1\"} 83",
            "device_ts_sync_errors_total{carrier=\"1\"} 0",
            "device_rf_locked{module=\"4\"} 0",
            "device_refused_total{rule=\"admin_page\"} 0",
            "device_temperature_decifahrenheit 804",
            "device_alarms_active 0",
            "device_alarms_total 0",
        ] {
            assert!(text.lines().any(|l| l == line), "{line} missing:\n{text}");
        }
        assert!(!text.contains("carrier=\"16\""));
    }
}
