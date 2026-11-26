//! The control API (SPEC §13.1), first part: health and metrics over plain HTTP/1.1 on a
//! configured address (the bind address is the access control). Read-only.
//!
//! - `GET /healthz` → `ok`
//! - `GET /api/metrics` → Prometheus text (SPEC §11.3 names)
//! - `GET /api/state` → the same values as JSON
//!
//! Deliberately small: one thread, one request per connection, a 2 s read timeout, requests
//! over 8 KiB refused. Nothing it does can affect the stream: it only reads atomics.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::metrics::{Counter, Gauge};

/// A metric's value source.
#[derive(Debug, Clone)]
pub enum Value {
    /// A counter.
    Counter(Counter),
    /// A gauge.
    Gauge(Gauge),
}

impl Value {
    fn get(&self) -> u64 {
        match self {
            Value::Counter(c) => c.get(),
            Value::Gauge(g) => g.get(),
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Value::Counter(_) => "counter",
            Value::Gauge(_) => "gauge",
        }
    }
}

/// One metric series.
#[derive(Debug, Clone)]
pub struct Series {
    /// Metric name.
    pub name: &'static str,
    /// Labels.
    pub labels: Vec<(&'static str, String)>,
    /// Value.
    pub value: Value,
}

/// Every series of the process, registered at start.
#[derive(Debug, Default)]
pub struct Registry {
    series: Vec<Series>,
}

impl Registry {
    /// Adds a counter and returns it.
    pub fn counter(&mut self, name: &'static str, labels: Vec<(&'static str, String)>) -> Counter {
        let c = Counter::default();
        self.series.push(Series {
            name,
            labels,
            value: Value::Counter(c.clone()),
        });
        c
    }

    /// Adds a gauge and returns it.
    pub fn gauge(&mut self, name: &'static str, labels: Vec<(&'static str, String)>) -> Gauge {
        let g = Gauge::default();
        self.series.push(Series {
            name,
            labels,
            value: Value::Gauge(g.clone()),
        });
        g
    }

    /// Adds an existing value.
    pub fn add(&mut self, name: &'static str, labels: Vec<(&'static str, String)>, value: Value) {
        self.series.push(Series {
            name,
            labels,
            value,
        });
    }

    /// Prometheus text exposition.
    pub fn prometheus(&self) -> String {
        let mut out = String::new();
        let mut typed: Vec<&str> = Vec::new();
        for s in &self.series {
            if !typed.contains(&s.name) {
                out.push_str(&format!("# TYPE {} {}\n", s.name, s.value.kind()));
                typed.push(s.name);
            }
            out.push_str(s.name);
            if !s.labels.is_empty() {
                out.push('{');
                for (i, (k, v)) in s.labels.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&format!(
                        "{k}=\"{}\"",
                        v.replace('\\', "\\\\").replace('"', "\\\"")
                    ));
                }
                out.push('}');
            }
            out.push_str(&format!(" {}\n", s.value.get()));
        }
        out
    }

    /// JSON: an array of `{name, labels, value}`.
    pub fn json(&self) -> String {
        let v: Vec<serde_json::Value> = self
            .series
            .iter()
            .map(|s| {
                let labels: serde_json::Map<String, serde_json::Value> = s
                    .labels
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String(v.clone())))
                    .collect();
                serde_json::json!({"name": s.name, "labels": labels, "value": s.value.get()})
            })
            .collect();
        serde_json::Value::Array(v).to_string()
    }
}

fn respond(mut s: TcpStream, reg: &Registry) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 8192];
    let mut n = 0;
    while n < buf.len() {
        match s.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => {
                n += k;
                if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut parts = req.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (status, ctype, body) = match (method, path) {
        ("GET", "/healthz") => ("200 OK", "text/plain", "ok\n".to_owned()),
        ("GET", "/api/metrics") => ("200 OK", "text/plain; version=0.0.4", reg.prometheus()),
        ("GET", "/api/state") => ("200 OK", "application/json", reg.json()),
        ("GET", _) => ("404 Not Found", "text/plain", "not found\n".to_owned()),
        _ => (
            "405 Method Not Allowed",
            "text/plain",
            "read-only\n".to_owned(),
        ),
    };
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Serves the registry on `addr` from a thread of its own until the process ends.
pub fn serve(addr: SocketAddr, reg: Arc<Registry>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let l = TcpListener::bind(addr)?;
    tracing::info!(%addr, "control API");
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for s in l.incoming().flatten() {
                respond(s, &reg);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_and_http() {
        let mut r = Registry::default();
        let c = r.counter(
            "ts_packets_total",
            vec![("carrier", "1".into()), ("class", "null".into())],
        );
        c.add(5);
        r.gauge(
            "program_source",
            vec![("carrier", "1".into()), ("program", "2".into())],
        )
        .set(3);
        let text = r.prometheus();
        assert!(text.contains(
            "# TYPE ts_packets_total counter\nts_packets_total{carrier=\"1\",class=\"null\"} 5\n"
        ));
        assert!(text.contains("program_source{carrier=\"1\",program=\"2\"} 3"));
        let reg = Arc::new(r);
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let r2 = reg.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten().take(2) {
                respond(s, &r2);
            }
        });
        for (path, want) in [("/healthz", "ok"), ("/api/state", "\"value\":5")] {
            let mut s = TcpStream::connect(addr).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            assert!(out.starts_with("HTTP/1.1 200"), "{out}");
            assert!(out.contains(want), "{out}");
        }
    }
}
