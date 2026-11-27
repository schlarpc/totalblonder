//! Inputs (SPEC §9): the network side of each configured input. The protocol state machines
//! are pure (`tb-rtmp`, `tb-tsin`); this module owns the sockets and hands every
//! `tb_media::InputEvent` to the input's [`EventSink`] (the sandboxed decoder's client).
//!
//! One thread per input. An RTMP input serves one publisher at a time (one program per
//! input, as the producer pushes, SPEC §9.1); a second connection waits in the backlog until the
//! first ends. Timeouts come from the session (handshake, publish, idle), so a dead publisher
//! frees the input.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use tb_media::{EndReason, InputEvent};
use tb_rtmp::{AllowList, ServerConfig, ServerSession};
use tb_tsin::{TsInput, TsInputConfig};

use crate::metrics::Counter;
use crate::time::Clock;

/// Where an input's events go.
pub trait EventSink: Send {
    /// One event, in order. Must not block for long (a full decoder queue drops and counts).
    fn event(&mut self, ev: InputEvent);
}

/// Counters of an input (SPEC §11.3 timeline/ingest rows).
#[derive(Debug, Clone, Default)]
pub struct InputStats {
    /// Connections accepted.
    pub connections: Counter,
    /// Sessions ended by a protocol violation or a timeout.
    pub errors: Counter,
    /// Packets delivered to the sink.
    pub packets: Counter,
    /// Bytes received.
    pub bytes: Counter,
}

/// Wraps an input's sink to report each session (`Streams` … `End`) as one `input.session`
/// wide event: its streams, what it delivered, how long it lasted and why it ended. Made on the
/// input's thread, so the event carries that thread's context (the input, the publisher).
pub struct SessionReporter<S> {
    inner: S,
    session: Option<SessionTally>,
}

struct SessionTally {
    ev: tb_telemetry::Wide,
    packets: u64,
    keyframes: u64,
    bytes: u64,
}

impl<S> SessionReporter<S> {
    /// Reports the sessions passing through to `inner`.
    pub fn new(inner: S) -> Self {
        SessionReporter {
            inner,
            session: None,
        }
    }

    fn end(&mut self, reason: Option<&EndReason>) {
        let Some(t) = self.session.take() else { return };
        t.ev.set("packets", t.packets)
            .set("keyframes", t.keyframes)
            .set("bytes", t.bytes);
        match reason {
            Some(EndReason::Closed) => {
                t.ev.outcome("closed");
            }
            Some(EndReason::Local(why)) => {
                t.ev.outcome("local").set("reason", why.clone());
            }
            Some(EndReason::Transport(why)) => {
                t.ev.outcome("transport")
                    .set("reason", why.clone())
                    .level(tracing::Level::WARN);
            }
            Some(EndReason::Protocol(why)) => {
                t.ev.outcome("protocol")
                    .set("reason", why.clone())
                    .level(tracing::Level::WARN);
            }
            None => {
                t.ev.outcome("unfinished");
            }
        }
    }
}

impl<S> Drop for SessionReporter<S> {
    fn drop(&mut self) {
        self.end(None);
    }
}

impl<S: EventSink> EventSink for SessionReporter<S> {
    fn event(&mut self, ev: InputEvent) {
        match &ev {
            InputEvent::Streams(list) => {
                let t = self.session.get_or_insert_with(|| SessionTally {
                    ev: tb_telemetry::Wide::new("input.session"),
                    packets: 0,
                    keyframes: 0,
                    bytes: 0,
                });
                // a second Streams is a reconfiguration within the session
                t.ev.add("streams_announced", 1u64);
                for s in list {
                    let p = |k: &str| format!("stream.{}.{k}", s.index.0);
                    t.ev.set(p("codec"), format!("{:?}", s.codec));
                    if let (Some(w), Some(h)) = (s.video.width, s.video.height) {
                        t.ev.set(p("size"), format!("{w}x{h}"));
                    }
                    if let Some(r) = s.video.frame_rate {
                        t.ev.set(p("fps"), f64::from(r.num()) / f64::from(r.den()));
                    }
                    if let Some(r) = s.audio.sample_rate {
                        t.ev.set(p("sample_rate"), r);
                    }
                    if let Some(c) = s.audio.channels {
                        t.ev.set(p("channels"), c);
                    }
                }
            }
            InputEvent::Packet(p) => {
                if let Some(t) = self.session.as_mut() {
                    t.packets += 1;
                    t.keyframes += u64::from(p.keyframe);
                    t.bytes += p.data.len() as u64;
                }
            }
            InputEvent::End(r) => {
                let r = r.clone();
                self.end(Some(&r));
            }
        }
        self.inner.event(ev);
    }
}

fn serve_rtmp(
    mut s: TcpStream,
    app: &str,
    key: &str,
    sink: &mut dyn EventSink,
    st: &InputStats,
    clock: &dyn Clock,
) {
    let t0 = Instant::now();
    let now = || t0.elapsed();
    let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = s.set_nodelay(true);
    let mut sess = ServerSession::new(
        ServerConfig::default(),
        AllowList::new().allow(app, key),
        now(),
    );
    let mut buf = vec![0u8; 64 << 10];
    let emit = |evs: Vec<InputEvent>, sink: &mut dyn EventSink| {
        for e in evs {
            match &e {
                InputEvent::Packet(_) => st.packets.inc(),
                InputEvent::End(EndReason::Protocol(_) | EndReason::Transport(_)) => {
                    st.errors.inc()
                }
                _ => {}
            }
            sink.event(e);
        }
    };
    loop {
        if clock.stopping() {
            emit(sess.on_transport_error(now(), "server stopping"), sink);
            return;
        }
        match s.read(&mut buf) {
            Ok(0) => {
                emit(sess.on_eof(now()), sink);
                return;
            }
            Ok(n) => {
                st.bytes.add(n as u64);
                emit(sess.on_bytes(now(), &buf[..n]), sink);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                emit(sess.on_tick(now()), sink);
            }
            Err(e) => {
                emit(sess.on_transport_error(now(), &e.to_string()), sink);
                return;
            }
        }
        while !sess.pending_output().is_empty() {
            match s.write(sess.pending_output()) {
                Ok(n) => sess.consume_output(n),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    emit(sess.on_transport_error(now(), &e.to_string()), sink);
                    return;
                }
            }
        }
        if sess.wants_close() {
            if let Some(r) = sess.close_reason() {
                tracing::info!(app, "RTMP session closed: {r:?}");
                tb_telemetry::annotate(&tracing::Span::current(), "rtmp.close", format!("{r:?}"));
            }
            return;
        }
    }
}

/// Serves RTMP publishers on `bind` for application `app` and stream key `key` until the
/// clock stops.
pub fn run_rtmp(
    bind: SocketAddr,
    app: String,
    key: String,
    mut sink: Box<dyn EventSink>,
    st: InputStats,
    clock: &dyn Clock,
) {
    let l = match TcpListener::bind(bind) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%bind, "RTMP listener: {e}");
            return;
        }
    };
    let _ = l.set_nonblocking(true);
    tracing::info!(%bind, app, "RTMP listening");
    while !clock.stopping() {
        match l.accept() {
            Ok((s, peer)) => {
                let _ = s.set_nonblocking(false);
                st.connections.inc();
                tracing::info!(%peer, app, "RTMP publisher connected");
                // The connection's wide event; the session's (input.session) is inside it.
                let conn = tb_telemetry::wide!("rtmp.connection", peer = peer, "rtmp.app" = &app);
                let _in = conn.enter();
                serve_rtmp(s, &app, &key, sink.as_mut(), &st, clock);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => {
                tracing::warn!(%bind, "accept: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Receives MPEG-TS over UDP or RTP on `bind` until the clock stops.
pub fn run_ts_udp(
    bind: SocketAddr,
    mut sink: Box<dyn EventSink>,
    st: InputStats,
    clock: &dyn Clock,
) {
    let s = match UdpSocket::bind(bind) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(%bind, "TS input: {e}");
            return;
        }
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
    let t0 = Instant::now();
    let mut input = TsInput::new(TsInputConfig::default());
    let mut buf = vec![0u8; 65_536];
    tracing::info!(%bind, "TS input listening");
    while !clock.stopping() {
        let evs = match s.recv(&mut buf) {
            Ok(n) => {
                st.bytes.add(n as u64);
                input.on_datagram(t0.elapsed(), &buf[..n])
            }
            Err(_) => input.on_tick(t0.elapsed()),
        };
        for e in evs {
            if matches!(e.event, InputEvent::Packet(_)) {
                st.packets.inc();
            }
            sink.event(e.event);
        }
    }
    for e in input.finish(t0.elapsed()) {
        sink.event(e.event);
    }
}

/// Plays an MPEG-TS file as an input, looped, paced by its own time stamps (a standby source,
/// SPEC §9.5 level 2). Each pass is a session of its own: it ends with `End` and the next starts
/// from the top, so the timeline mapper starts a new session at the loop point.
pub fn run_ts_file(
    path: std::path::PathBuf,
    mut sink: Box<dyn EventSink>,
    st: InputStats,
    clock: &dyn Clock,
) {
    use std::io::BufReader;
    use tb_tsin::{DemuxConfig, Demuxer};
    // Events are handed over this far ahead of their time stamps (the decoder and mapper
    // absorb it as arrival jitter).
    let lead = Duration::from_millis(300);
    while !clock.stopping() {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(path = %path.display(), "file input: {e}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        st.connections.inc();
        let mut r = BufReader::with_capacity(1 << 20, file);
        let mut demux = Demuxer::new(DemuxConfig::default());
        let mut origin: Option<(Instant, i64)> = None;
        let mut buf = vec![0u8; 188 * 512];
        let deliver = |events: Vec<tb_tsin::TsEvent>,
                       sink: &mut dyn EventSink,
                       origin: &mut Option<(Instant, i64)>|
         -> bool {
            for e in events {
                if let InputEvent::Packet(p) = &e.event
                    && let Some(t) = p.dts.or(p.pts)
                {
                    let t = t.saturating_mul(300);
                    let (o_wall, o_t) = *origin.get_or_insert((Instant::now(), t));
                    let due = o_wall + Duration::from_nanos((t - o_t).max(0) as u64 * 1000 / 27);
                    while Instant::now() + lead < due {
                        if clock.stopping() {
                            return false;
                        }
                        std::thread::sleep(
                            (due - lead - Instant::now()).min(Duration::from_millis(20)),
                        );
                    }
                    st.packets.inc();
                }
                sink.event(e.event);
            }
            true
        };
        loop {
            let n = match std::io::Read::read(&mut r, &mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(path = %path.display(), "file input: {e}");
                    st.errors.inc();
                    break;
                }
            };
            st.bytes.add(n as u64);
            demux.push_bytes(&buf[..n]);
            if !deliver(demux.take_events(), sink.as_mut(), &mut origin) {
                return;
            }
        }
        demux.flush();
        if !deliver(demux.take_events(), sink.as_mut(), &mut origin) {
            return;
        }
        sink.event(InputEvent::End(EndReason::Closed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Collect(Arc<Mutex<Vec<InputEvent>>>);
    impl EventSink for Collect {
        fn event(&mut self, ev: InputEvent) {
            if let Ok(mut v) = self.0.lock() {
                v.push(ev);
            }
        }
    }

    /// A real ffmpeg publishes to the listener (skipped when ffmpeg is absent).
    #[test]
    fn rtmp_publish_from_ffmpeg() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg not found: skipped");
            return;
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let clock = crate::time::RealClock::new(Instant::now(), stop.clone());
        let got = Arc::new(Mutex::new(Vec::new()));
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let g2 = got.clone();
        let h = std::thread::spawn(move || {
            run_rtmp(
                addr,
                "live".into(),
                "k".into(),
                Box::new(Collect(g2)),
                InputStats::default(),
                &clock,
            )
        });
        std::thread::sleep(Duration::from_millis(200));
        let out = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=30",
                "-t",
                "2",
            ])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "30",
                "-f",
                "flv",
            ])
            .arg(format!("rtmp://127.0.0.1:{port}/live/k"))
            .output()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        h.join().unwrap();
        let v = got.lock().unwrap();
        if !out.status.success() {
            // An ffmpeg without the FLV muxer or RTMP protocol: nothing to check.
            eprintln!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
            return;
        }
        assert!(
            matches!(v.first(), Some(InputEvent::Streams(_))),
            "{:?}",
            v.first()
        );
        assert!(
            v.iter()
                .filter(|e| matches!(e, InputEvent::Packet(_)))
                .count()
                >= 50
        );
        assert!(matches!(v.last(), Some(InputEvent::End(_))));
    }
}
