//! The modulator's supervisor (SPEC §10) in the server: `tb-eqam`'s sans-io controller driven
//! over plain HTTP/1.0 to the box's Boa server. Every request has passed `tb_eqam::screen`
//! before it reaches [`Http::send`]: this module cannot build a request of its own.
//!
//! Mode `report` (the default) never writes; `apply` must be configured explicitly.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use tb_eqam::client::{ClientConfig, Next};
use tb_eqam::controller::Controller;
use tb_eqam::plan::DesiredState;
use tb_eqam::supervisor::{Action, SupervisorConfig};
use tb_eqam::types::Credentials;
use tb_eqam::{Method, Request, Response, TransportFailure};

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

/// The device metrics for the control API.
#[derive(Debug, Clone, Default)]
pub struct DeviceGauges {
    /// `device_reachable`.
    pub reachable: Gauge,
    /// `device_uptime_seconds`.
    pub uptime: Gauge,
    /// `device_drift_items`.
    pub drift: Gauge,
    /// Alarms raised since start.
    pub alarms: Gauge,
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
        let m = ctl.supervisor().metrics();
        gauges.reachable.set(u64::from(m.reachable));
        gauges.uptime.set(m.uptime_seconds.unwrap_or(0));
        gauges.drift.set(m.drift_items as u64);
        if let Some(d) = wait {
            std::thread::sleep(d.min(Duration::from_millis(500)));
        }
    }
}
