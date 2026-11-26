//! One RTMP connection from a publisher, server side, sans-io.
//!
//! ```text
//!  Handshake ──► AwaitConnect ──connect──► Connected ──publish──► Publishing
//!     │              │                        │                     │ FCUnpublish,
//!     │              │                        │                     │ deleteStream,
//!     ▼              ▼                        ▼                     ▼ closeStream
//!   Closed ◄──────── errors, timeouts, rejection, peer close ──── Unpublished ──► Closed
//! ```
//!
//! The driver owns the socket and the clock. It calls [`ServerSession::on_bytes`] with what it
//! read, [`ServerSession::on_tick`] now and then (timeouts are evaluated there and in
//! `on_bytes`), [`ServerSession::on_eof`] / [`ServerSession::on_transport_error`] when the
//! connection ends; each returns the [`InputEvent`]s produced. Bytes to send wait in a bounded
//! queue ([`ServerSession::pending_output`], [`ServerSession::consume_output`]). Once
//! [`ServerSession::wants_close`] is true, the driver sends what is queued and closes.
//!
//! **Events.** Nothing is emitted before a publish is accepted. From then on the events are
//! one input session in the sense of `tb_media`: `Streams`, packets (and further `Streams` on a
//! reconfiguration), and exactly one `End`. A connection that never publishes (rejected,
//! timed out, broken) emits no events; [`ServerSession::close_reason`] says why it closed.
//!
//! **Command flow** (§7.2, as ffmpeg, OBS/librtmp and GStreamer publish):
//! - `connect` → Window Acknowledgment Size, Set Peer Bandwidth, Set Chunk Size, then
//!   `_result` with `NetConnection.Connect.Success` (or `_error` with
//!   `NetConnection.Connect.Rejected`, then close). We answer `objectEncoding` 0 whatever the
//!   client asked: commands are AMF0 (type 20), or type 17 with an AMF0 body.
//! - `releaseStream`, `FCPublish` → `_result` (null, undefined); `createStream` → `_result`
//!   with a new message stream id (1, 2, ...; at most [`ServerConfig::max_streams`]).
//! - `publish` (on a created stream) → User Control StreamBegin and `onStatus`
//!   `NetStream.Publish.Start` (ffmpeg starts sending media only after it), or `onStatus`
//!   level `error`, `NetStream.Publish.BadName`, then close.
//! - `@setDataFrame("onMetaData", {...})` (or a bare `onMetaData`) → hints and track
//!   expectations for the media assembler ([`crate::media`]).
//! - `FCUnpublish`, `deleteStream` / `closeStream` of the published stream → `End(Closed)`.
//!   We stay open after `FCUnpublish` until `deleteStream` or the peer closes (ffmpeg sends
//!   both, in that order, and a write error on the second would be reported by it), then ask
//!   to close.
//! - Unknown commands are ignored (counted); `play` is not served (this is a publish server).

use std::time::Duration;

use tb_media::{EndReason, InputEvent};

use crate::amf0::{self, Amf0Value};
use crate::chunk::{self, ChunkDecoder, ChunkEncoder};
use crate::handshake::{HandshakeError, ServerHandshake};
use crate::media::{Assembler, MediaConfig, MediaStats, Metadata};
use crate::message::{self, Command, LimitType, UserControl, status};
use crate::types::{ChunkStreamId, Message, MessageStreamId, MessageType, RtmpTime};

/// Settings of a server session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerConfig {
    /// Chunk layer bounds.
    pub chunk: chunk::Limits,
    /// AMF0 decode bounds.
    pub amf: amf0::Limits,
    /// When to announce streams.
    pub media: MediaConfig,
    /// Largest command or data message parsed (commands are small; this bounds what AMF
    /// decoding can allocate).
    pub max_command_len: usize,
    /// Our chunk size, sent after `connect`.
    pub out_chunk_size: u32,
    /// Window Acknowledgment Size sent to the client.
    pub window_ack_size: u32,
    /// Set Peer Bandwidth sent to the client.
    pub peer_bandwidth: u32,
    /// Most bytes queued for sending; beyond it the peer is not reading and the session
    /// closes.
    pub max_output: usize,
    /// Most message streams one connection may create.
    pub max_streams: u32,
    /// From the start of the session to the end of the handshake.
    pub handshake_timeout: Duration,
    /// From the start of the session to an accepted publish.
    pub publish_timeout: Duration,
    /// Longest time with no bytes from the peer.
    pub idle_timeout: Duration,
    /// Seed of the handshake's filler bytes.
    pub seed: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            chunk: chunk::Limits::default(),
            amf: amf0::Limits::default(),
            media: MediaConfig::default(),
            max_command_len: 64 << 10,
            out_chunk_size: 4096,
            window_ack_size: 2_500_000,
            peer_bandwidth: 2_500_000,
            max_output: 256 << 10,
            max_streams: 8,
            handshake_timeout: Duration::from_secs(10),
            publish_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(10),
            seed: 0x7462_726d_7470,
        }
    }
}

/// What the policy is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request<'a> {
    /// `connect` to an application.
    Connect {
        /// The `app` property (a trailing '/' removed).
        app: &'a str,
        /// The `tcUrl` property, if given.
        tc_url: Option<&'a str>,
    },
    /// `publish` of a stream.
    Publish {
        /// The application connected to.
        app: &'a str,
        /// The stream name (the "key") before any '?'.
        key: &'a str,
        /// What follows '?' in the stream name, if anything.
        query: Option<&'a str>,
    },
}

/// Decides which connections and publishes are accepted.
pub trait PublishPolicy {
    /// `true` to accept.
    fn check(&mut self, request: &Request<'_>) -> bool;
}

impl<F: FnMut(&Request<'_>) -> bool> PublishPolicy for F {
    fn check(&mut self, request: &Request<'_>) -> bool {
        self(request)
    }
}

/// Accepts everything (the bind address and firewall are the access control, SPEC §9.1).
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl PublishPolicy for AllowAll {
    fn check(&mut self, _: &Request<'_>) -> bool {
        true
    }
}

/// Accepts listed applications, and in them listed stream keys (or any key).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowList {
    entries: Vec<(String, Option<String>)>,
}

impl AllowList {
    /// An empty list (accepts nothing).
    pub fn new() -> Self {
        Self::default()
    }
    /// Accept `key` in `app`.
    pub fn allow(mut self, app: &str, key: &str) -> Self {
        self.entries.push((app.to_owned(), Some(key.to_owned())));
        self
    }
    /// Accept any key in `app`.
    pub fn allow_app(mut self, app: &str) -> Self {
        self.entries.push((app.to_owned(), None));
        self
    }
}

impl PublishPolicy for AllowList {
    fn check(&mut self, request: &Request<'_>) -> bool {
        match *request {
            Request::Connect { app, .. } => self.entries.iter().any(|(a, _)| a == app),
            Request::Publish { app, key, .. } => self
                .entries
                .iter()
                .any(|(a, k)| a == app && k.as_deref().is_none_or(|k| k == key)),
        }
    }
}

/// Why a session closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// The publisher unpublished and deleted its stream.
    Finished,
    /// The peer closed the connection.
    PeerClosed,
    /// The transport failed.
    Transport(String),
    /// The peer broke the protocol.
    Protocol(String),
    /// A timeout (which one).
    Timeout(&'static str),
    /// The policy refused the application or stream.
    Rejected(String),
    /// A local limit (output queue).
    Local(String),
}

impl CloseReason {
    fn end_reason(&self) -> EndReason {
        match self {
            Self::Finished | Self::PeerClosed => EndReason::Closed,
            Self::Transport(s) => EndReason::Transport(s.clone()),
            Self::Timeout(s) => EndReason::Transport(format!("{s} timeout")),
            Self::Protocol(s) => EndReason::Protocol(s.clone()),
            Self::Rejected(s) | Self::Local(s) => EndReason::Local(s.clone()),
        }
    }
}

/// Counters of one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStats {
    /// Bytes received.
    pub bytes_in: u64,
    /// Messages decoded.
    pub messages: u64,
    /// Messages ignored (unknown commands, media outside a publish, unknown types).
    pub ignored: u64,
    /// Ping requests answered.
    pub pings: u64,
    /// Acknowledgments sent.
    pub acks_sent: u64,
    /// The media assembler's counters.
    pub media: MediaStats,
}

/// After the handshake, before `connect`.
#[derive(Debug, Clone)]
struct AwaitConnect;

/// After an accepted `connect`.
#[derive(Debug, Clone)]
struct Connected {
    app: String,
    streams: Vec<MessageStreamId>,
    next_stream: u32,
}

/// After an accepted `publish`.
#[derive(Debug, Clone)]
struct Publishing {
    app: String,
    key: String,
    stream: MessageStreamId,
    media: Assembler,
}

/// `End` has been emitted (`FCUnpublish`); waiting for `deleteStream` or the peer to close.
#[derive(Debug, Clone)]
struct Unpublished {
    stream: MessageStreamId,
}

#[derive(Debug, Clone)]
enum Phase {
    Handshake(ServerHandshake),
    AwaitConnect(AwaitConnect),
    Connected(Connected),
    Publishing(Box<Publishing>),
    Unpublished(Unpublished),
    Closed(CloseReason),
}

/// An RTMP server session for one connection.
#[derive(Debug, Clone)]
pub struct ServerSession<P> {
    config: ServerConfig,
    policy: P,
    phase: Phase,
    decoder: ChunkDecoder,
    encoder: ChunkEncoder,
    output: Vec<u8>,
    start: Duration,
    last_rx: Duration,
    peer_window: Option<u32>,
    acked: u64,
    stats: SessionStats,
    /// Messages decoded but not yet handled (reused buffer).
    messages: Vec<Message>,
}

const STREAM_CSID: ChunkStreamId = match ChunkStreamId::new(5) {
    Ok(c) => c,
    Err(_) => ChunkStreamId::COMMAND,
};

impl<P: PublishPolicy> ServerSession<P> {
    /// A session for a connection accepted at `now`.
    pub fn new(config: ServerConfig, policy: P, now: Duration) -> Self {
        Self {
            config,
            policy,
            phase: Phase::Handshake(ServerHandshake::new(config.seed)),
            decoder: ChunkDecoder::new(config.chunk),
            encoder: ChunkEncoder::new(),
            output: Vec::new(),
            start: now,
            last_rx: now,
            peer_window: None,
            acked: 0,
            stats: SessionStats::default(),
            messages: Vec::new(),
        }
    }

    /// Bytes waiting to be sent.
    pub fn pending_output(&self) -> &[u8] {
        &self.output
    }

    /// `n` bytes of [`Self::pending_output`] were sent.
    pub fn consume_output(&mut self, n: usize) {
        self.output.drain(..n.min(self.output.len()));
    }

    /// The driver should flush the output and close the connection.
    pub fn wants_close(&self) -> bool {
        matches!(self.phase, Phase::Closed(_))
    }

    /// Why the session closed, once it has.
    pub fn close_reason(&self) -> Option<&CloseReason> {
        match &self.phase {
            Phase::Closed(r) => Some(r),
            _ => None,
        }
    }

    /// The application and stream key being published, if publishing.
    pub fn published(&self) -> Option<(&str, &str)> {
        match &self.phase {
            Phase::Publishing(p) => Some((&p.app, &p.key)),
            _ => None,
        }
    }

    /// Counters.
    pub fn stats(&self) -> SessionStats {
        let mut s = self.stats;
        if let Phase::Publishing(p) = &self.phase {
            s.media = p.media.stats();
        }
        s
    }

    /// Bytes arrived from the peer at `now`.
    pub fn on_bytes(&mut self, now: Duration, input: &[u8]) -> Vec<InputEvent> {
        let mut ev = Vec::new();
        if self.wants_close() {
            return ev;
        }
        self.last_rx = now;
        self.stats.bytes_in += input.len() as u64;
        let mut rest = input;
        if let Phase::Handshake(hs) = &mut self.phase {
            let ms = now.saturating_sub(self.start).as_millis() as u32;
            match hs.push(rest, ms, &mut self.output) {
                Err(e @ HandshakeError::Version(_)) => {
                    self.close(CloseReason::Protocol(e.to_string()), &mut ev);
                    return ev;
                }
                Ok(p) => {
                    rest = &rest[p.consumed..];
                    if p.done {
                        self.phase = Phase::AwaitConnect(AwaitConnect);
                    }
                }
            }
        }
        if !rest.is_empty() && !matches!(self.phase, Phase::Handshake(_)) {
            let mut msgs = std::mem::take(&mut self.messages);
            let r = self.decoder.push(rest, &mut msgs);
            for m in msgs.drain(..) {
                if self.wants_close() {
                    break;
                }
                self.stats.messages += 1;
                self.on_message(now, m, &mut ev);
                self.check_output(&mut ev);
            }
            self.messages = msgs;
            self.acknowledge();
            if let Err(e) = r {
                self.close(CloseReason::Protocol(e.to_string()), &mut ev);
            }
        }
        self.check_timers(now, &mut ev);
        self.check_output(&mut ev);
        ev
    }

    /// Time passes: evaluates timeouts and the media assembler's wait.
    pub fn on_tick(&mut self, now: Duration) -> Vec<InputEvent> {
        let mut ev = Vec::new();
        self.check_timers(now, &mut ev);
        ev
    }

    /// The peer closed the connection (end of file).
    pub fn on_eof(&mut self, _now: Duration) -> Vec<InputEvent> {
        let mut ev = Vec::new();
        self.close(CloseReason::PeerClosed, &mut ev);
        ev
    }

    /// The transport failed.
    pub fn on_transport_error(&mut self, _now: Duration, what: &str) -> Vec<InputEvent> {
        let mut ev = Vec::new();
        self.close(CloseReason::Transport(what.to_owned()), &mut ev);
        ev
    }

    /// Close (idempotent): `End` if a publish had been accepted and not yet ended.
    fn close(&mut self, reason: CloseReason, ev: &mut Vec<InputEvent>) {
        match std::mem::replace(&mut self.phase, Phase::Closed(reason.clone())) {
            Phase::Publishing(mut p) => {
                self.stats.media = p.media.stats();
                p.media.finish(ev);
                ev.push(InputEvent::End(reason.end_reason()));
            }
            Phase::Closed(r) => self.phase = Phase::Closed(r),
            _ => {}
        }
    }

    fn check_timers(&mut self, now: Duration, ev: &mut Vec<InputEvent>) {
        let since_start = now.saturating_sub(self.start);
        let reason = match &mut self.phase {
            Phase::Closed(_) => return,
            Phase::Handshake(_) if since_start >= self.config.handshake_timeout => {
                Some("handshake")
            }
            Phase::Handshake(_) | Phase::AwaitConnect(_) | Phase::Connected(_)
                if since_start >= self.config.publish_timeout =>
            {
                Some("publish")
            }
            Phase::Publishing(p) => {
                p.media.on_tick(now, ev);
                None
            }
            _ => None,
        };
        let reason = reason.or_else(|| {
            (now.saturating_sub(self.last_rx) >= self.config.idle_timeout).then_some("idle")
        });
        if let Some(r) = reason {
            self.close(CloseReason::Timeout(r), ev);
        }
    }

    fn check_output(&mut self, ev: &mut Vec<InputEvent>) {
        if self.output.len() > self.config.max_output && !self.wants_close() {
            let m = format!("output queue over {} bytes", self.config.max_output);
            self.close(CloseReason::Local(m), ev);
        }
    }

    fn send(&mut self, msg: &Message) {
        if let Err(e) = self.encoder.write(msg, &mut self.output) {
            // Only an oversized payload fails, and we build none.
            debug_assert!(false, "{e}");
        }
    }

    fn send_command(&mut self, csid: ChunkStreamId, stream: MessageStreamId, cmd: Command) {
        let m = cmd.to_message(csid, stream);
        self.send(&m);
    }

    /// Acknowledgments (§5.4.3): one each time the peer's window of bytes has arrived.
    fn acknowledge(&mut self) {
        let Some(w) = self.peer_window else { return };
        let total = self.stats.bytes_in;
        if total - self.acked >= u64::from(w) {
            self.acked = total;
            let m = message::acknowledgment(total as u32);
            self.send(&m);
            self.stats.acks_sent += 1;
        }
    }

    fn on_message(&mut self, now: Duration, m: Message, ev: &mut Vec<InputEvent>) {
        match m.type_id {
            // Applied by the decoder.
            MessageType::SET_CHUNK_SIZE | MessageType::ABORT | MessageType::ACKNOWLEDGMENT => {}
            MessageType::WINDOW_ACK_SIZE => {
                if let Some(b) = m.payload.get(..4) {
                    let w = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                    self.peer_window = (w > 0).then_some(w);
                }
            }
            MessageType::SET_PEER_BANDWIDTH => {}
            MessageType::USER_CONTROL => match UserControl::parse(&m.payload) {
                Some(UserControl::PingRequest(t)) => {
                    self.stats.pings += 1;
                    let r = UserControl::PingResponse(t).to_message();
                    self.send(&r);
                }
                _ => self.stats.ignored += 1,
            },
            MessageType::COMMAND_AMF0 | MessageType::COMMAND_AMF3 => {
                if m.payload.len() > self.config.max_command_len {
                    let e = format!("command message of {} bytes", m.payload.len());
                    self.close(CloseReason::Protocol(e), ev);
                    return;
                }
                let amf3 = m.type_id == MessageType::COMMAND_AMF3;
                match Command::parse(&m.payload, amf3, self.config.amf) {
                    Ok(c) => self.on_command(now, m.stream, c, ev),
                    Err(e) => self.close(CloseReason::Protocol(e.to_string()), ev),
                }
            }
            MessageType::DATA_AMF0 | MessageType::DATA_AMF3 => self.on_data(now, &m, ev),
            MessageType::AUDIO | MessageType::VIDEO | MessageType::AGGREGATE => {
                match &mut self.phase {
                    Phase::Publishing(p) if p.stream == m.stream => {
                        media_message(&mut p.media, now, &m, ev);
                    }
                    _ => self.stats.ignored += 1,
                }
            }
            _ => self.stats.ignored += 1,
        }
    }

    fn on_data(&mut self, now: Duration, m: &Message, ev: &mut Vec<InputEvent>) {
        let Phase::Publishing(p) = &mut self.phase else {
            self.stats.ignored += 1;
            return;
        };
        if p.stream != m.stream || m.payload.len() > self.config.max_command_len {
            self.stats.ignored += 1;
            return;
        }
        let body = if m.type_id == MessageType::DATA_AMF3 {
            m.payload.get(1..).unwrap_or_default()
        } else {
            &m.payload
        };
        let Ok(values) = amf0::decode_all(body, self.config.amf) else {
            self.stats.ignored += 1;
            return;
        };
        let mut it = values.iter();
        let mut first = it.next().and_then(Amf0Value::as_str);
        if first == Some("@setDataFrame") {
            first = it.next().and_then(Amf0Value::as_str);
        }
        match (first, it.next()) {
            (Some("onMetaData"), Some(obj)) => {
                p.media.on_metadata(Metadata::from_amf(obj), now, ev);
            }
            _ => self.stats.ignored += 1,
        }
    }

    fn on_command(
        &mut self,
        now: Duration,
        stream: MessageStreamId,
        c: Command,
        ev: &mut Vec<InputEvent>,
    ) {
        let tx = c.transaction;
        match (&mut self.phase, c.name.as_str()) {
            (Phase::AwaitConnect(_), "connect") => self.on_connect(tx, &c.object, ev),
            (Phase::Connected(_), "releaseStream" | "FCPublish")
            | (Phase::Publishing(_), "releaseStream" | "FCPublish") => {
                if tx != 0.0 {
                    self.result(tx, Amf0Value::Null, Amf0Value::Undefined);
                }
            }
            (Phase::Connected(s), "createStream") => {
                if s.streams.len() as u32 >= self.config.max_streams {
                    let e = format!("more than {} createStream", self.config.max_streams);
                    self.close(CloseReason::Protocol(e), ev);
                    return;
                }
                let id = MessageStreamId(s.next_stream);
                s.next_stream += 1;
                s.streams.push(id);
                self.result(tx, Amf0Value::Null, Amf0Value::Number(f64::from(id.0)));
            }
            (Phase::Connected(_), "publish") => self.on_publish(now, stream, &c, ev),
            (Phase::Connected(s), "deleteStream" | "closeStream") => {
                let id = c.args.first().and_then(Amf0Value::as_f64);
                s.streams.retain(|x| Some(f64::from(x.0)) != id);
            }
            (Phase::Publishing(p), "FCUnpublish") => {
                let stream = p.stream;
                self.unpublish(stream, ev);
            }
            (Phase::Publishing(p), "deleteStream" | "closeStream") => {
                let stream = p.stream;
                let target = c.args.first().and_then(Amf0Value::as_f64);
                // closeStream travels on the stream itself; deleteStream names it.
                if c.name == "closeStream" || target == Some(f64::from(stream.0)) {
                    self.unpublish(stream, ev);
                    self.close(CloseReason::Finished, ev);
                }
            }
            (Phase::Unpublished(u), "deleteStream" | "closeStream") => {
                let target = c.args.first().and_then(Amf0Value::as_f64);
                if c.name == "closeStream" || target == Some(f64::from(u.stream.0)) {
                    self.close(CloseReason::Finished, ev);
                }
            }
            (_, "connect") => {
                self.close(CloseReason::Protocol("connect out of order".into()), ev);
            }
            (_, "publish") => {
                self.close(CloseReason::Protocol("publish out of order".into()), ev);
            }
            _ => self.stats.ignored += 1,
        }
    }

    fn result(&mut self, tx: f64, object: Amf0Value, arg: Amf0Value) {
        let cmd = Command {
            name: "_result".into(),
            transaction: tx,
            object,
            args: vec![arg],
        };
        self.send_command(ChunkStreamId::COMMAND, MessageStreamId::CONTROL, cmd);
    }

    fn on_connect(&mut self, tx: f64, obj: &Amf0Value, ev: &mut Vec<InputEvent>) {
        let Some(app) = obj.get("app").and_then(Amf0Value::as_str) else {
            self.close(CloseReason::Protocol("connect without app".into()), ev);
            return;
        };
        let app = app.trim_end_matches('/').to_owned();
        let tc_url = obj.get("tcUrl").and_then(Amf0Value::as_str);
        if !self.policy.check(&Request::Connect { app: &app, tc_url }) {
            let cmd = Command {
                name: "_error".into(),
                transaction: tx,
                object: Amf0Value::Null,
                args: vec![status(
                    "error",
                    "NetConnection.Connect.Rejected",
                    "Application not accepted.",
                )],
            };
            self.send_command(ChunkStreamId::COMMAND, MessageStreamId::CONTROL, cmd);
            self.close(CloseReason::Rejected(format!("application {app:?}")), ev);
            return;
        }
        let was = message::window_ack_size(self.config.window_ack_size);
        self.send(&was);
        let spb = message::set_peer_bandwidth(self.config.peer_bandwidth, LimitType::Dynamic);
        self.send(&spb);
        let size = self.config.out_chunk_size.clamp(1, chunk::MAX_CHUNK_SIZE);
        let scs = message::set_chunk_size(size);
        self.send(&scs);
        // Valid by the clamp.
        let _ = self.encoder.set_chunk_size(size);
        let props = Amf0Value::object([
            ("fmsVer", Amf0Value::string("FMS/3,0,1,123")),
            ("capabilities", Amf0Value::Number(31.0)),
        ]);
        let Amf0Value::Object(mut info) = status(
            "status",
            "NetConnection.Connect.Success",
            "Connection succeeded.",
        ) else {
            return;
        };
        info.push(("objectEncoding".into(), Amf0Value::Number(0.0)));
        self.result(tx, props, Amf0Value::Object(info));
        self.phase = Phase::Connected(Connected {
            app,
            streams: Vec::new(),
            next_stream: 1,
        });
    }

    fn on_publish(
        &mut self,
        _now: Duration,
        stream: MessageStreamId,
        c: &Command,
        ev: &mut Vec<InputEvent>,
    ) {
        let Phase::Connected(s) = &self.phase else {
            return;
        };
        if !s.streams.contains(&stream) {
            let e = format!("publish on message stream {} not created", stream.0);
            self.close(CloseReason::Protocol(e), ev);
            return;
        }
        let Some(name) = c.args.first().and_then(Amf0Value::as_str) else {
            self.close(CloseReason::Protocol("publish without a name".into()), ev);
            return;
        };
        let (key, query) = match name.split_once('?') {
            Some((k, q)) => (k, Some(q)),
            None => (name, None),
        };
        let app = s.app.clone();
        let ok = self.policy.check(&Request::Publish {
            app: &app,
            key,
            query,
        });
        if !ok {
            let cmd = Command {
                name: "onStatus".into(),
                transaction: 0.0,
                object: Amf0Value::Null,
                args: vec![status(
                    "error",
                    "NetStream.Publish.BadName",
                    "Stream not accepted.",
                )],
            };
            self.send_command(STREAM_CSID, stream, cmd);
            let r = CloseReason::Rejected(format!("stream {key:?} in {app:?}"));
            self.close(r, ev);
            return;
        }
        let begin = UserControl::StreamBegin(stream).to_message();
        self.send(&begin);
        let mut info = status(
            "status",
            "NetStream.Publish.Start",
            &format!("{key} is now published."),
        );
        if let Amf0Value::Object(p) = &mut info {
            p.push(("details".into(), Amf0Value::string(key)));
        }
        let cmd = Command {
            name: "onStatus".into(),
            transaction: 0.0,
            object: Amf0Value::Null,
            args: vec![info],
        };
        self.send_command(STREAM_CSID, stream, cmd);
        self.phase = Phase::Publishing(Box::new(Publishing {
            app,
            key: key.to_owned(),
            stream,
            media: Assembler::new(self.config.media),
        }));
    }

    fn unpublish(&mut self, stream: MessageStreamId, ev: &mut Vec<InputEvent>) {
        if let Phase::Publishing(mut p) =
            std::mem::replace(&mut self.phase, Phase::Unpublished(Unpublished { stream }))
        {
            self.stats.media = p.media.stats();
            p.media.finish(ev);
            ev.push(InputEvent::End(EndReason::Closed));
        }
    }
}

/// Audio, video and aggregate messages into the assembler.
fn media_message(media: &mut Assembler, now: Duration, m: &Message, ev: &mut Vec<InputEvent>) {
    match m.type_id {
        MessageType::AUDIO => media.on_audio(now, m.timestamp, &m.payload, ev),
        MessageType::VIDEO => media.on_video(now, m.timestamp, &m.payload, ev),
        _ => aggregate(media, now, m, ev),
    }
}

/// An aggregate message (§7.1.6): FLV tags (11-byte header, body, 4-byte back pointer); each
/// sub-message's time is the aggregate's plus its offset from the first tag's.
fn aggregate(media: &mut Assembler, now: Duration, m: &Message, ev: &mut Vec<InputEvent>) {
    let mut p = m.payload.as_slice();
    let mut first: Option<u32> = None;
    while p.len() >= 11 {
        let size = chunk::be24(&p[1..4]) as usize;
        let ts = chunk::be24(&p[4..7]) | (u32::from(p[7]) << 24);
        let Some(body) = p.get(11..11 + size) else {
            break;
        };
        let base = *first.get_or_insert(ts);
        let t = RtmpTime(m.timestamp.0.wrapping_add(ts.wrapping_sub(base)));
        match p[0] {
            8 => media.on_audio(now, t, body, ev),
            9 => media.on_video(now, t, body, ev),
            _ => {}
        }
        p = p.get(11 + size + 4..).unwrap_or_default();
    }
}
