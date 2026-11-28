//! The client: one request at a time, paced, logged in, retried once for reads.
//!
//! The adapter loop is:
//!
//! ```text
//! loop {
//!     match client.next(now()) {
//!         Next::Send(req) => { let r = transport.send(req); events = client.on_response(now(), r) }
//!         Next::WaitUntil(t) => sleep_until(t),
//!         Next::Idle => wait for a job,
//!         Next::AwaitingResponse => unreachable in a synchronous adapter,
//!     }
//! }
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use crate::html::Document;
use crate::pages::debug::DebugPage;
use crate::pages::diagnostic::DiagnosticPage;
use crate::pages::eventlog::EventLogPage;
use crate::pages::qamconfig::QamConfigPage;
use crate::pages::status::StatusPage;
use crate::pages::statusedit::StatusEditPage;
use crate::pages::streaminfo::StreamInfoPage;
use crate::pages::{PageClass, ParseError, classify};
use crate::plan::{RfPlan, StreamTable};
use crate::refusal::{Refusal, RefusalRule, Request, screen};
use crate::request::{ReadPage, RequestDraft, Response, TransportFailure};
use crate::session::{Pacer, Session, SessionConfig, SessionEvent, SessionNeed};
use crate::types::Credentials;
use crate::write::{RfPlanWrite, StreamTableWrite, WriteReport, WriteStep};

/// Work for the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Read and parse a page.
    Read(ReadPage),
    /// Write the stream table (all 16 rows).
    WriteStreams(StreamTable),
    /// Write the RF plan (the four-module sequence).
    WriteRfPlan(RfPlan),
}

/// A parsed page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// `status.cgi`.
    Status(StatusPage),
    /// `statusedit.cgi`.
    StatusEdit(StatusEditPage),
    /// `qamconfig.cgi?slotNumber=m`.
    QamConfig(QamConfigPage),
    /// `streaminfo.cgi`.
    StreamInfo(StreamInfoPage),
    /// `diagnostic.cgi`.
    Diagnostic(DiagnosticPage),
    /// `debug.cgi`.
    Debug(DebugPage),
    /// `errorlog.cgi`.
    EventLog(EventLogPage),
    /// `admin.cgi`.
    Admin(crate::pages::admin::AdminPage),
}

impl Observation {
    /// Parse `body` as `page`.
    pub fn parse(page: ReadPage, body: &[u8]) -> Result<Self, ParseError> {
        Ok(match page {
            ReadPage::Status => Observation::Status(StatusPage::parse(body)?),
            ReadPage::StatusEdit => Observation::StatusEdit(StatusEditPage::parse(body)?),
            ReadPage::QamConfig(m) => {
                let p = QamConfigPage::parse(body)?;
                if p.module != m {
                    return Err(ParseError::structure(
                        "qamconfig.cgi",
                        format!("asked for module {m}, got {}", p.module),
                    ));
                }
                Observation::QamConfig(p)
            }
            ReadPage::StreamInfo => Observation::StreamInfo(StreamInfoPage::parse(body)?),
            ReadPage::Diagnostic => Observation::Diagnostic(DiagnosticPage::parse(body)?),
            ReadPage::Debug => Observation::Debug(DebugPage::parse(body)?),
            ReadPage::EventLog => Observation::EventLog(EventLogPage::parse(body)?),
            ReadPage::Admin => Observation::Admin(crate::pages::admin::AdminPage::parse(body)?),
        })
    }
}

/// Why a read produced no page.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// No response (after the one retry).
    #[error("transport: {0}")]
    Transport(TransportFailure),
    /// Non-200 status.
    #[error("HTTP status {0}")]
    Status(u16),
    /// The page did not parse (after one retry for a Modulator Error, one re-login for a
    /// Logged Out page).
    #[error("parse: {0}")]
    Parse(ParseError),
    /// A request was refused (a bug in this crate if it happens).
    #[error("{0}")]
    Refused(Refusal),
}

/// What the client reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// A page was read.
    Observed(Box<Observation>),
    /// A read failed.
    ReadFailed {
        /// Page.
        page: ReadPage,
        /// Why.
        error: ReadError,
    },
    /// A write finished.
    WriteFinished(WriteReport),
    /// Session news.
    Session(SessionEvent),
}

/// What the adapter should do next.
#[derive(Debug)]
pub enum Next<'a> {
    /// Send this request now and pass the result to [`Client::on_response`].
    Send(&'a Request),
    /// Nothing may be sent before this time.
    WaitUntil(Duration),
    /// No work queued.
    Idle,
    /// A request is out; its response has not been fed back.
    AwaitingResponse,
}

/// Client settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// Session timing.
    pub session: SessionConfig,
    /// Minimum gap between the end of one request and the start of the next (1 s).
    pub min_gap: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            session: SessionConfig::default(),
            min_gap: Duration::from_secs(1),
        }
    }
}

#[derive(Debug)]
enum Machine {
    Table(Box<StreamTableWrite>),
    Plan(Box<RfPlanWrite>),
}

#[derive(Debug)]
enum Flight {
    Login,
    Read {
        page: ReadPage,
        attempts: u8,
        relogged: bool,
    },
    Write {
        attempts: u8,
    },
}

#[derive(Debug)]
enum Current {
    Read { page: ReadPage, relogged: bool },
    Write(Machine),
}

/// The client state machine.
#[derive(Debug)]
pub struct Client {
    session: Session,
    pacer: Pacer,
    queue: VecDeque<Job>,
    current: Option<Current>,
    prepared: Option<(Request, Flight)>,
    in_flight: Option<(Request, Flight)>,
    refused: BTreeMap<RefusalRule, u64>,
    deferred: Vec<ClientEvent>,
}

impl Client {
    /// A client logging in with `credentials`.
    pub fn new(credentials: Credentials, config: ClientConfig) -> Self {
        Client {
            session: Session::new(credentials, config.session),
            pacer: Pacer::new(config.min_gap),
            queue: VecDeque::new(),
            current: None,
            prepared: None,
            in_flight: None,
            refused: BTreeMap::new(),
            deferred: Vec::new(),
        }
    }

    /// Queue a job. A read already queued is not queued twice.
    pub fn submit(&mut self, job: Job) {
        if matches!(job, Job::Read(_)) && self.queue.contains(&job) {
            return;
        }
        self.queue.push_back(job);
    }

    /// Jobs queued and not started.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Whether a job is in progress.
    pub fn busy(&self) -> bool {
        self.current.is_some() || self.in_flight.is_some()
    }

    /// Events produced outside a response (a job that ended before sending anything).
    pub fn drain_events(&mut self) -> Vec<ClientEvent> {
        std::mem::take(&mut self.deferred)
    }

    /// Refusals so far, by rule (`device_refused_total{rule}`).
    pub fn refused(&self) -> &BTreeMap<RefusalRule, u64> {
        &self.refused
    }

    /// The session layer.
    pub fn session(&self) -> &Session {
        &self.session
    }

    fn note_refusal(&mut self, r: &Refusal) {
        *self.refused.entry(r.rule).or_insert(0) += 1;
    }

    /// Fill `prepared` with the next request, or say why there is none. A read job is built
    /// once a session exists; a write job starts its step machine with the session id it will
    /// keep for its whole run.
    fn prepare(&mut self, now: Duration) -> Option<Next<'static>> {
        loop {
            if self.current.is_none() && self.queue.is_empty() {
                return Some(Next::Idle);
            }
            let id = match self.session.need(now) {
                Ok(SessionNeed::Ready(id)) => id,
                Ok(SessionNeed::Send(r)) => {
                    self.prepared = Some((r, Flight::Login));
                    return None;
                }
                Ok(SessionNeed::WaitUntil(t)) => return Some(Next::WaitUntil(t)),
                Ok(SessionNeed::Pending) => return Some(Next::AwaitingResponse),
                Err(e) => {
                    self.note_refusal(&e);
                    return Some(Next::Idle);
                }
            };
            if self.current.is_none() {
                self.current = match self.queue.pop_front() {
                    None => return Some(Next::Idle),
                    Some(Job::Read(page)) => Some(Current::Read {
                        page,
                        relogged: false,
                    }),
                    Some(Job::WriteStreams(t)) => {
                        let (m, step) = StreamTableWrite::start(t, id.clone());
                        self.start_write(Machine::Table(Box::new(m)), step)
                    }
                    Some(Job::WriteRfPlan(p)) => {
                        let (m, step) = RfPlanWrite::start(p, id.clone());
                        self.start_write(Machine::Plan(Box::new(m)), step)
                    }
                };
                if self.prepared.is_some() {
                    return None;
                }
            }
            match self.current.take() {
                Some(Current::Read { page, relogged }) => {
                    match screen(RequestDraft::read(page, &id)) {
                        Ok(r) => {
                            self.current = Some(Current::Read { page, relogged });
                            self.prepared = Some((
                                r,
                                Flight::Read {
                                    page,
                                    attempts: 1,
                                    relogged,
                                },
                            ));
                            return None;
                        }
                        Err(e) => {
                            self.note_refusal(&e);
                            self.deferred.push(ClientEvent::ReadFailed {
                                page,
                                error: ReadError::Refused(e),
                            });
                        }
                    }
                }
                // A write's steps are prepared by `on_response`, so a write without a pending
                // step cannot occur; if it ever did, report it rather than lose it.
                Some(Current::Write(mut m)) => {
                    let report = match &mut m {
                        Machine::Table(w) => w.abandon(),
                        Machine::Plan(w) => w.abandon(),
                    };
                    self.deferred.push(ClientEvent::WriteFinished(report));
                }
                None => {}
            }
        }
    }

    fn start_write(&mut self, machine: Machine, step: WriteStep) -> Option<Current> {
        match step {
            WriteStep::Send(r) => {
                self.prepared = Some((r, Flight::Write { attempts: 1 }));
                Some(Current::Write(machine))
            }
            WriteStep::Done(report) => {
                self.deferred.push(ClientEvent::WriteFinished(report));
                None
            }
        }
    }

    /// What to do at `now`.
    pub fn next(&mut self, now: Duration) -> Next<'_> {
        if self.in_flight.is_some() {
            return Next::AwaitingResponse;
        }
        if self.prepared.is_none()
            && let Some(early) = self.prepare(now)
        {
            return early;
        }
        match self.pacer.ready_at() {
            Some(t) if now >= t => {
                self.in_flight = self.prepared.take();
                self.pacer.started();
                match &self.in_flight {
                    Some((r, _)) => Next::Send(r),
                    None => Next::Idle,
                }
            }
            Some(t) => Next::WaitUntil(t),
            None => Next::AwaitingResponse,
        }
    }

    /// Feed the result of the request handed out by [`Client::next`].
    pub fn on_response(
        &mut self,
        now: Duration,
        result: Result<Response, TransportFailure>,
    ) -> Vec<ClientEvent> {
        self.pacer.finished(now);
        let Some((request, flight)) = self.in_flight.take() else {
            return std::mem::take(&mut self.deferred);
        };
        let mut events = std::mem::take(&mut self.deferred);
        let logged_out = match &result {
            Ok(r) => r.status == 200 && classify(&Document::parse(&r.body)) == PageClass::LoggedOut,
            Err(_) => false,
        };
        match flight {
            Flight::Login => {
                let (next, ev) = self.session.on_login_response(now, result.as_ref());
                events.extend(ev.into_iter().map(ClientEvent::Session));
                if let Some(r) = next {
                    self.prepared = Some((r, Flight::Login));
                }
            }
            Flight::Read {
                page,
                attempts,
                relogged,
            } => {
                if logged_out {
                    events.extend(
                        self.session
                            .logged_out(now)
                            .into_iter()
                            .map(ClientEvent::Session),
                    );
                    if relogged {
                        self.current = None;
                        events.push(ClientEvent::ReadFailed {
                            page,
                            error: ReadError::Parse(ParseError::LoggedOut),
                        });
                    } else {
                        // Log in again and repeat the read once.
                        self.current = Some(Current::Read {
                            page,
                            relogged: true,
                        });
                    }
                    return events;
                }
                let retry = |this: &mut Self, request: Request| {
                    this.prepared = Some((
                        request,
                        Flight::Read {
                            page,
                            attempts: attempts + 1,
                            relogged,
                        },
                    ));
                };
                match result {
                    Err(e) => {
                        if attempts < 2 && request.retryable() {
                            retry(self, request);
                        } else {
                            self.current = None;
                            events.push(ClientEvent::ReadFailed {
                                page,
                                error: ReadError::Transport(e),
                            });
                        }
                    }
                    Ok(resp) if resp.status != 200 => {
                        self.current = None;
                        events.push(ClientEvent::ReadFailed {
                            page,
                            error: ReadError::Status(resp.status),
                        });
                    }
                    Ok(resp) => {
                        self.session.touch(now);
                        match Observation::parse(page, &resp.body) {
                            Ok(o) => {
                                self.current = None;
                                events.push(ClientEvent::Observed(Box::new(o)));
                            }
                            Err(ParseError::ModulatorError) if attempts < 2 => retry(self, request),
                            Err(e) => {
                                self.current = None;
                                events.push(ClientEvent::ReadFailed {
                                    page,
                                    error: ReadError::Parse(e),
                                });
                            }
                        }
                    }
                }
            }
            Flight::Write { attempts } => {
                if logged_out {
                    events.extend(
                        self.session
                            .logged_out(now)
                            .into_iter()
                            .map(ClientEvent::Session),
                    );
                } else if result.as_ref().is_ok_and(|r| r.status == 200) {
                    self.session.touch(now);
                }
                // One retry of a read step after a timeout; never of a post.
                if result.is_err() && attempts < 2 && request.retryable() {
                    self.prepared = Some((
                        request,
                        Flight::Write {
                            attempts: attempts + 1,
                        },
                    ));
                    return events;
                }
                let step = match &mut self.current {
                    Some(Current::Write(Machine::Table(m))) => m.on_response(result.as_ref()),
                    Some(Current::Write(Machine::Plan(m))) => m.on_response(result.as_ref()),
                    _ => return events,
                };
                match step {
                    WriteStep::Send(r) => self.prepared = Some((r, Flight::Write { attempts: 1 })),
                    WriteStep::Done(report) => {
                        self.current = None;
                        events.push(ClientEvent::WriteFinished(report));
                    }
                }
            }
        }
        events
    }
}
