//! The supervisor and the client wired together: the whole sans-io device controller.

use std::time::Duration;

use crate::client::{Client, ClientConfig, Next};
use crate::plan::DesiredState;
use crate::request::{Response, TransportFailure};
use crate::supervisor::{Action, Supervisor, SupervisorConfig};
use crate::types::Credentials;

/// What the adapter should do, and what the operator should hear about.
#[derive(Debug)]
pub struct Step<'a> {
    /// Send, wait or idle.
    pub next: Next<'a>,
    /// Alarms, clears, write logs and rate-limit notes (jobs are handled internally).
    pub actions: Vec<Action>,
}

/// Supervisor plus client.
#[derive(Debug)]
pub struct Controller {
    client: Client,
    supervisor: Supervisor,
}

impl Controller {
    /// A controller starting at `now`.
    pub fn new(
        credentials: Credentials,
        client: ClientConfig,
        supervisor: SupervisorConfig,
        desired: DesiredState,
        now: Duration,
    ) -> Self {
        Controller {
            client: Client::new(credentials, client),
            supervisor: Supervisor::new(supervisor, desired, now),
        }
    }

    fn absorb(&mut self, actions: Vec<Action>) -> Vec<Action> {
        let mut out = Vec::new();
        for a in actions {
            match a {
                Action::Job(j) => self.client.submit(j),
                other => out.push(other),
            }
        }
        out
    }

    /// What to do at `now`.
    pub fn next(&mut self, now: Duration) -> Step<'_> {
        let ticked = self.supervisor.tick(now);
        let mut actions = self.absorb(ticked);
        for ev in self.client.drain_events() {
            let a = self.supervisor.on_event(now, ev);
            actions.extend(self.absorb(a));
        }
        let due = self.supervisor.next_due();
        let next = match self.client.next(now) {
            Next::Idle => match due {
                Some(t) => Next::WaitUntil(t.max(now)),
                None => Next::Idle,
            },
            Next::WaitUntil(t) => Next::WaitUntil(due.map_or(t, |d| d.min(t)).max(now)),
            other => other,
        };
        Step { next, actions }
    }

    /// Feed the response to the request [`Controller::next`] handed out.
    pub fn on_response(
        &mut self,
        now: Duration,
        result: Result<Response, TransportFailure>,
    ) -> Vec<Action> {
        let mut out = Vec::new();
        for ev in self.client.on_response(now, result) {
            let a = self.supervisor.on_event(now, ev);
            out.extend(self.absorb(a));
        }
        out
    }

    /// The client (refusal counts, session).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The supervisor (metrics, drift, halt).
    pub fn supervisor(&self) -> &Supervisor {
        &self.supervisor
    }

    /// The supervisor, for pause / resume / compare-now / reload.
    pub fn supervisor_mut(&mut self) -> &mut Supervisor {
        &mut self.supervisor
    }
}
