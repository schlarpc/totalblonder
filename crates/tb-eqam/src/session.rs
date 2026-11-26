//! Session handling (SPEC §10.1, device §2): the login exchange, the session id on every
//! request, the "Logged Out" page, re-login with backoff, the idle timeout, and request
//! spacing. All pure policy; time is an argument (a monotonic [`Duration`] since any origin).
//!
//! Facts it is built on: `GET login.cgi` hands out a fresh session id; `POST login.cgi` with
//! that id logs it in; authentication is only the `session_id` parameter; a second Admin login
//! ends the first session at once; the idle timeout is the "Login Timeout" (15 min here); a
//! bad or expired id gets the "Logged Out" page. The login page shows a `confirm()` warning
//! when an Administrator is already logged in.

use std::time::Duration;

use crate::pages::{PageClass, classify};
use crate::refusal::{Refusal, Request, screen};
use crate::request::{RequestDraft, Response, TransportFailure};
use crate::types::{Credentials, SessionId};

/// Spacing of requests: one at a time, the next no sooner than `min_gap` after the previous
/// one finished (SPEC §10.1: at least 1 s apart). The adapter enforces the timing; this
/// decides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pacer {
    min_gap: Duration,
    last_done: Option<Duration>,
    in_flight: bool,
}

impl Pacer {
    /// A pacer with `min_gap` between the end of one request and the start of the next.
    pub fn new(min_gap: Duration) -> Self {
        Pacer {
            min_gap,
            last_done: None,
            in_flight: false,
        }
    }

    /// When the next request may start: `None` while one is in flight.
    pub fn ready_at(&self) -> Option<Duration> {
        if self.in_flight {
            None
        } else {
            Some(self.last_done.map_or(Duration::ZERO, |t| t + self.min_gap))
        }
    }

    /// Whether a request may start at `now`.
    pub fn may_start(&self, now: Duration) -> bool {
        self.ready_at().is_some_and(|t| now >= t)
    }

    /// A request started.
    pub fn started(&mut self) {
        self.in_flight = true;
    }

    /// The request finished (response or failure) at `now`.
    pub fn finished(&mut self, now: Duration) {
        self.in_flight = false;
        self.last_done = Some(now);
    }
}

/// Session timing and backoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// The device's idle timeout ("Login Timeout"; 15 min on this unit).
    pub idle_timeout: Duration,
    /// Log in again, without asking, once idle for `idle_timeout - idle_margin`.
    pub idle_margin: Duration,
    /// First retry delay after a failed login; doubles per failure.
    pub backoff_initial: Duration,
    /// Longest retry delay.
    pub backoff_max: Duration,
    /// Delay after the device said "Login failed" (wrong credentials).
    pub rejected_delay: Duration,
    /// After our live session was ended and the login page says another Administrator is
    /// logged in, wait this long before logging in (which would end theirs).
    pub yield_to_other_admin: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_margin: Duration::from_secs(60),
            backoff_initial: Duration::from_secs(5),
            backoff_max: Duration::from_secs(5 * 60),
            rejected_delay: Duration::from_secs(15 * 60),
            yield_to_other_admin: Duration::from_secs(15 * 60),
        }
    }
}

/// Something the session layer wants reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// Logged in.
    LoggedIn,
    /// The device said "Login failed".
    LoginRejected,
    /// "Maximum number of users reached".
    TooManyUsers,
    /// Our session ended while in use (another Admin login, a reboot, or the device's timer).
    SessionLost,
    /// Another Administrator holds the session; we wait before taking it over.
    YieldingToOtherAdmin {
        /// Until when.
        until: Duration,
    },
    /// The login exchange got a page it did not expect.
    LoginUnexpected(String),
    /// The login exchange failed at the transport.
    LoginTransport(TransportFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// No session; may start the login exchange at `not_before`.
    Idle { not_before: Duration },
    /// `GET login.cgi` is out.
    AwaitLoginPage,
    /// `POST login.cgi` is out with this id.
    AwaitLoginPost { offered: SessionId },
    /// Logged in.
    Active {
        id: SessionId,
        last_activity: Duration,
        since: Duration,
    },
}

/// What the session layer needs before an authenticated request can go out.
#[derive(Debug)]
pub enum SessionNeed {
    /// A session is ready.
    Ready(SessionId),
    /// Send this login request.
    Send(Request),
    /// Nothing can happen before this time.
    WaitUntil(Duration),
    /// A login request is out; wait for its response.
    Pending,
}

/// The session state machine.
#[derive(Debug, Clone)]
pub struct Session {
    credentials: Credentials,
    config: SessionConfig,
    state: State,
    failures: u32,
    /// Set when a live session was lost: the next login page decides whether to yield.
    lost_live_session: bool,
}

impl Session {
    /// A session that will log in with `credentials`.
    pub fn new(credentials: Credentials, config: SessionConfig) -> Self {
        Session {
            credentials,
            config,
            state: State::Idle {
                not_before: Duration::ZERO,
            },
            failures: 0,
            lost_live_session: false,
        }
    }

    /// The current session id, when logged in.
    pub fn id(&self) -> Option<&SessionId> {
        match &self.state {
            State::Active { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Whether a login exchange is in progress.
    pub fn logging_in(&self) -> bool {
        matches!(
            self.state,
            State::AwaitLoginPage | State::AwaitLoginPost { .. }
        )
    }

    /// What is needed at `now` before an authenticated request. Expires a session that has
    /// been idle for longer than the device keeps it.
    pub fn need(&mut self, now: Duration) -> Result<SessionNeed, Refusal> {
        if let State::Active { last_activity, .. } = self.state
            && now.saturating_sub(last_activity) + self.config.idle_margin
                >= self.config.idle_timeout
        {
            // The device has dropped it (or is about to): start over without a wasted request.
            self.state = State::Idle { not_before: now };
        }
        match &self.state {
            State::Active { id, .. } => Ok(SessionNeed::Ready(id.clone())),
            State::AwaitLoginPage | State::AwaitLoginPost { .. } => Ok(SessionNeed::Pending),
            State::Idle { not_before } if now < *not_before => {
                Ok(SessionNeed::WaitUntil(*not_before))
            }
            State::Idle { .. } => {
                let r = screen(RequestDraft::login_page())?;
                self.state = State::AwaitLoginPage;
                Ok(SessionNeed::Send(r))
            }
        }
    }

    fn backoff(&mut self, now: Duration) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let exp = self.failures.saturating_sub(1).min(16);
        let d = self
            .config
            .backoff_initial
            .saturating_mul(1u32 << exp)
            .min(self.config.backoff_max);
        now + d
    }

    /// Feed the response to the login request that [`Session::need`] handed out. Returns the
    /// next login request to send, if any, and events.
    pub fn on_login_response(
        &mut self,
        now: Duration,
        result: Result<&Response, &TransportFailure>,
    ) -> (Option<Request>, Vec<SessionEvent>) {
        let mut events = Vec::new();
        let state = std::mem::replace(&mut self.state, State::Idle { not_before: now });
        let resp = match result {
            Ok(r) => r,
            Err(e) => {
                events.push(SessionEvent::LoginTransport(e.clone()));
                self.state = State::Idle {
                    not_before: self.backoff(now),
                };
                return (None, events);
            }
        };
        let doc = crate::html::Document::parse(&resp.body);
        let class = classify(&doc);
        match (state, class) {
            (
                State::AwaitLoginPage,
                PageClass::LoginForm {
                    session: Some(id),
                    admin_present,
                    ..
                },
            ) => {
                if admin_present && self.lost_live_session {
                    // Someone took the session over: let them work.
                    self.lost_live_session = false;
                    let until = now + self.config.yield_to_other_admin;
                    self.state = State::Idle { not_before: until };
                    events.push(SessionEvent::YieldingToOtherAdmin { until });
                    return (None, events);
                }
                match screen(RequestDraft::login_post(&self.credentials, &id)) {
                    Ok(r) => {
                        self.state = State::AwaitLoginPost { offered: id };
                        (Some(r), events)
                    }
                    Err(e) => {
                        events.push(SessionEvent::LoginUnexpected(e.to_string()));
                        self.state = State::Idle {
                            not_before: self.backoff(now),
                        };
                        (None, events)
                    }
                }
            }
            (State::AwaitLoginPost { offered }, PageClass::Welcome { session }) => {
                // The welcome page's refresh URL is authoritative; it normally repeats the id.
                let id = session.unwrap_or(offered);
                self.failures = 0;
                self.lost_live_session = false;
                self.state = State::Active {
                    id,
                    last_activity: now,
                    since: now,
                };
                events.push(SessionEvent::LoggedIn);
                (None, events)
            }
            (State::AwaitLoginPost { .. }, PageClass::LoginForm { failed: true, .. }) => {
                self.failures = self.failures.saturating_add(1);
                self.state = State::Idle {
                    not_before: now + self.config.rejected_delay,
                };
                events.push(SessionEvent::LoginRejected);
                (None, events)
            }
            (_, PageClass::TooManyUsers) => {
                self.state = State::Idle {
                    not_before: self.backoff(now),
                };
                events.push(SessionEvent::TooManyUsers);
                (None, events)
            }
            (_, other) => {
                self.state = State::Idle {
                    not_before: self.backoff(now),
                };
                events.push(SessionEvent::LoginUnexpected(format!("{other:?}")));
                (None, events)
            }
        }
    }

    /// An authenticated request got a normal page at `now`: the session is alive.
    pub fn touch(&mut self, now: Duration) {
        if let State::Active { last_activity, .. } = &mut self.state {
            *last_activity = now;
        }
    }

    /// An authenticated request got the "Logged Out" page at `now`.
    pub fn logged_out(&mut self, now: Duration) -> Vec<SessionEvent> {
        match self.state {
            State::Active { since, .. } => {
                // A session that dies straight after login is a failure to back off from
                // (e.g. the session is bound to another address); otherwise log in again now.
                let not_before = if now.saturating_sub(since) < Duration::from_secs(5) {
                    self.backoff(now)
                } else {
                    now
                };
                self.lost_live_session = true;
                self.state = State::Idle { not_before };
                vec![SessionEvent::SessionLost]
            }
            _ => Vec::new(),
        }
    }

    /// Consecutive login failures so far.
    pub fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn session() -> Session {
        Session::new(
            Credentials::new("Admin", "pw").unwrap(),
            SessionConfig::default(),
        )
    }

    fn login(s: &mut Session, now: Duration) {
        let SessionNeed::Send(r) = s.need(now).unwrap() else {
            panic!()
        };
        assert_eq!(r.target(), "/cgi-bin/login.cgi");
        let (post, ev) = s.on_login_response(now, Ok(&Response::ok(fixtures::LOGIN)));
        assert!(ev.is_empty());
        let post = post.unwrap();
        assert_eq!(
            String::from_utf8(post.body().unwrap()).unwrap(),
            "txtUserName=Admin&txtPassword=pw&session_id=123456789&btnSubmit=Submit"
        );
        let (none, ev) = s.on_login_response(now, Ok(&Response::ok(fixtures::LOGIN_POST)));
        assert!(none.is_none());
        assert_eq!(ev, vec![SessionEvent::LoggedIn]);
    }

    #[test]
    fn pacer_spacing() {
        let mut p = Pacer::new(secs(1));
        assert!(p.may_start(secs(0)));
        p.started();
        assert_eq!(p.ready_at(), None);
        assert!(!p.may_start(secs(100)));
        p.finished(secs(10));
        assert_eq!(p.ready_at(), Some(secs(11)));
        assert!(!p.may_start(Duration::from_millis(10_999)));
        assert!(p.may_start(secs(11)));
    }

    #[test]
    fn login_flow_and_session_id() {
        let mut s = session();
        login(&mut s, secs(0));
        assert_eq!(s.id(), Some(&SessionId::parse("123456789").unwrap()));
        assert!(matches!(s.need(secs(1)).unwrap(), SessionNeed::Ready(_)));
    }

    #[test]
    fn idle_timeout_forces_relogin_before_the_device_does() {
        let mut s = session();
        login(&mut s, secs(0));
        s.touch(secs(100));
        assert!(matches!(
            s.need(secs(100 + 13 * 60)).unwrap(),
            SessionNeed::Ready(_)
        ));
        // 14 min idle = 15 min timeout - 1 min margin.
        assert!(matches!(
            s.need(secs(100 + 14 * 60)).unwrap(),
            SessionNeed::Send(_)
        ));
    }

    #[test]
    fn logged_out_relogs_at_once_then_backs_off() {
        let mut s = session();
        login(&mut s, secs(0));
        assert_eq!(s.logged_out(secs(600)), vec![SessionEvent::SessionLost]);
        assert!(matches!(s.need(secs(600)).unwrap(), SessionNeed::Send(_)));
        // The login page now says nobody else is logged in (e.g. after a reboot): log in.
        let (post, ev) = s.on_login_response(secs(601), Ok(&Response::ok(fixtures::LOGIN)));
        assert!(post.is_some() && ev.is_empty());
        let _ = s.on_login_response(secs(602), Ok(&Response::ok(fixtures::LOGIN_POST)));
        // Lost again immediately: back off 5 s, then 10 s, ...
        s.logged_out(secs(603));
        assert!(matches!(s.need(secs(603)).unwrap(), SessionNeed::WaitUntil(t) if t == secs(608)));
    }

    #[test]
    fn yields_to_another_admin_after_losing_a_live_session() {
        let mut s = session();
        login(&mut s, secs(0));
        s.logged_out(secs(600));
        let SessionNeed::Send(_) = s.need(secs(600)).unwrap() else {
            panic!()
        };
        let page = String::from_utf8_lossy(fixtures::LOGIN).replace(
            "return true;",
            "var input; input = confirm(\"An Administrator is currently logged into the system.  If you are logging in as an Administrator, the current Administrator's session will be terminated.\"); return input;",
        );
        let (post, ev) =
            s.on_login_response(secs(601), Ok(&Response::ok(page.as_bytes().to_vec())));
        assert!(post.is_none());
        assert_eq!(
            ev,
            vec![SessionEvent::YieldingToOtherAdmin {
                until: secs(601 + 900)
            }]
        );
        assert!(matches!(
            s.need(secs(700)).unwrap(),
            SessionNeed::WaitUntil(_)
        ));
        // After the wait, the takeover happens (the flag is cleared).
        let SessionNeed::Send(_) = s.need(secs(1501)).unwrap() else {
            panic!()
        };
        let (post, _) =
            s.on_login_response(secs(1501), Ok(&Response::ok(page.as_bytes().to_vec())));
        assert!(post.is_some());
    }

    #[test]
    fn rejected_and_too_many_users() {
        let mut s = session();
        let SessionNeed::Send(_) = s.need(secs(0)).unwrap() else {
            panic!()
        };
        let _ = s.on_login_response(secs(0), Ok(&Response::ok(fixtures::LOGIN)));
        let failed = String::from_utf8_lossy(fixtures::LOGIN)
            .replace("<legend align=\"left\"><b>Login</b></legend>", "<legend><b>Login</b></legend><font color=\"#FF0000\">Login failed... please try again.</font>");
        let (_, ev) = s.on_login_response(secs(1), Ok(&Response::ok(failed.as_bytes().to_vec())));
        assert_eq!(ev, vec![SessionEvent::LoginRejected]);
        assert!(matches!(s.need(secs(2)).unwrap(), SessionNeed::WaitUntil(t) if t == secs(901)));
        let SessionNeed::Send(_) = s.need(secs(901)).unwrap() else {
            panic!()
        };
        let (_, ev) = s.on_login_response(
            secs(901),
            Ok(&Response::ok(
                include_bytes!("../tests/fixtures/synthetic/too_many_users.html").to_vec(),
            )),
        );
        assert_eq!(ev, vec![SessionEvent::TooManyUsers]);
        // Transport failure backs off too.
        let SessionNeed::WaitUntil(t) = s.need(secs(902)).unwrap() else {
            panic!()
        };
        let SessionNeed::Send(_) = s.need(t).unwrap() else {
            panic!()
        };
        let (_, ev) = s.on_login_response(t, Err(&TransportFailure::Timeout));
        assert_eq!(
            ev,
            vec![SessionEvent::LoginTransport(TransportFailure::Timeout)]
        );
        assert!(s.failures() >= 3);
    }
}
