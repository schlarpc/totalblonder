//! The controller (supervisor + client) against the offline fake device on a simulated clock.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use common::{FakeDevice, secs};
use tb_eqam::client::{ClientConfig, Next};
use tb_eqam::controller::Controller;
use tb_eqam::pages::eventlog::EventAlarm;
use tb_eqam::plan::{DesiredState, ModulePlan, RfPlan, SlotConfig, StreamTable};
use tb_eqam::supervisor::{Action, Alarm, AlarmKey, HaltReason, ReconcileMode, SupervisorConfig};
use tb_eqam::types::{Credentials, FirstChannel, OutputLevel, QamMap, QamMode, Slot};
use tb_eqam::write::{WriteKind, WriteOutcome};
use tb_eqam::{Method, TransportFailure};

fn live_plan() -> RfPlan {
    let m = |ch| ModulePlan {
        first_channel: FirstChannel::new(ch).unwrap(),
        qam: QamMode::Qam256B,
        map: QamMap::Std,
        carriers_on: [true; 4],
    };
    RfPlan::new(OutputLevel::new(40).unwrap(), [m(2), m(10), m(6), m(140)]).unwrap()
}

fn one_stream() -> StreamTable {
    StreamTable::empty()
        .with(
            Slot::new(1).unwrap(),
            SlotConfig::Assigned {
                url: "UDP://239.10.0.1:5000".parse().unwrap(),
                name: "WALL".parse().unwrap(),
            },
        )
        .unwrap()
}

struct Sim {
    ctl: Controller,
    dev: FakeDevice,
    now: Duration,
    actions: Vec<(Duration, Action)>,
    /// (start, end) of every request.
    times: Vec<(Duration, Duration)>,
}

impl Sim {
    fn new(
        mode: ReconcileMode,
        streams: StreamTable,
        dev: FakeDevice,
        tweak: impl FnOnce(&mut SupervisorConfig),
    ) -> Self {
        let mut cfg = SupervisorConfig {
            mode,
            ..SupervisorConfig::default()
        };
        tweak(&mut cfg);
        let ctl = Controller::new(
            Credentials::new("Admin", "pass").unwrap(),
            ClientConfig::default(),
            cfg,
            DesiredState {
                rf: live_plan(),
                streams,
            },
            Duration::ZERO,
        );
        Sim {
            ctl,
            dev,
            now: Duration::ZERO,
            actions: vec![],
            times: vec![],
        }
    }

    fn advance(&mut self, to: Duration) {
        let to = to.max(self.now + Duration::from_millis(1));
        self.dev.uptime += to - self.now;
        self.now = to;
    }

    fn run_until(&mut self, end: Duration, mut tamper: impl FnMut(&mut FakeDevice, Duration)) {
        while self.now < end {
            tamper(&mut self.dev, self.now);
            let step = self.ctl.next(self.now);
            let now = self.now;
            self.actions
                .extend(step.actions.into_iter().map(|a| (now, a)));
            let resp = match step.next {
                Next::Send(req) => Some(self.dev.handle(req)),
                Next::WaitUntil(t) => {
                    self.advance(t.min(end));
                    None
                }
                Next::Idle => {
                    self.advance(end);
                    None
                }
                Next::AwaitingResponse => panic!("synchronous driver never leaves a request out"),
            };
            if let Some(r) = resp {
                let start = self.now;
                self.advance(self.now + Duration::from_millis(200));
                self.times.push((start, self.now));
                let now = self.now;
                let a = self.ctl.on_response(now, r);
                self.actions.extend(a.into_iter().map(|a| (now, a)));
            }
        }
    }

    fn alarms(&self) -> Vec<&Alarm> {
        self.actions
            .iter()
            .filter_map(|(_, a)| {
                if let Action::Alarm(x) = a {
                    Some(x)
                } else {
                    None
                }
            })
            .collect()
    }

    fn count(&self, target: &str) -> usize {
        self.dev.targets().iter().filter(|t| *t == target).count()
    }
}

#[test]
fn polls_on_schedule_one_request_at_a_time_and_spaced() {
    let mut s = Sim::new(
        ReconcileMode::Report,
        StreamTable::empty(),
        FakeDevice::surveyed(),
        |_| {},
    );
    s.run_until(secs(3600), |_, _| {});
    // Every 30 s for status; every 5 min for the rest; one login.
    let status = s.count("GET status.cgi");
    assert!((118..=122).contains(&status), "{status}");
    for m in 1..=4 {
        let q = s.count(&format!("GET qamconfig.cgi?slotNumber={m}"));
        assert!((12..=13).contains(&q), "{q}");
    }
    assert!((12..=13).contains(&s.count("GET debug.cgi")));
    assert_eq!(s.count("GET login.cgi"), 1);
    assert_eq!(s.count("POST login.cgi"), 1);
    for w in s.times.windows(2) {
        assert!(
            w[1].0 >= w[0].1 + secs(1),
            "requests closer than 1 s: {w:?}"
        );
    }
    assert!(s.dev.posts().is_empty());
    // No drift against the surveyed unit; the boot-time reconcile line is reported once.
    let alarms = s.alarms();
    assert_eq!(alarms.len(), 1, "{alarms:?}");
    assert!(matches!(
        alarms[0],
        Alarm::EventLog(EventAlarm::ConfigFileReconcile, _)
    ));
    assert!(s.ctl.client().refused().is_empty());
    let m = s.ctl.supervisor().metrics();
    assert!(m.reachable);
    assert_eq!(m.rf_locked, [Some(true); 4]);
    assert_eq!(m.overflow_total, [Some(0); 16]);
    assert_eq!(m.drift_items, 0);
    let samples = m.samples(s.ctl.client().refused());
    assert!(samples.iter().any(|x| x.name == "device_uptime_seconds"));
    assert!(
        samples
            .iter()
            .any(|x| x.name == "device_rf_locked" && x.value == 1.0)
    );
}

#[test]
fn report_mode_alarms_on_drift_and_never_writes() {
    let mut s = Sim::new(
        ReconcileMode::Report,
        one_stream(),
        FakeDevice::surveyed(),
        |_| {},
    );
    s.run_until(secs(3600), |_, _| {});
    assert!(s.dev.posts().is_empty());
    let drift: Vec<_> = s
        .alarms()
        .into_iter()
        .filter(|a| matches!(a, Alarm::Drift(_)))
        .collect();
    assert_eq!(drift.len(), 1);
    let Alarm::Drift(items) = drift[0] else {
        unreachable!()
    };
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|(k, _)| *k == WriteKind::StreamTable));
    assert_eq!(s.ctl.supervisor().metrics().drift_items, 2);
}

#[test]
fn apply_mode_fixes_drift_once_after_confirmation() {
    let mut s = Sim::new(
        ReconcileMode::Apply,
        one_stream(),
        FakeDevice::surveyed(),
        |_| {},
    );
    s.run_until(secs(3600), |_, _| {});
    let posts = s.dev.posts();
    assert_eq!(posts.len(), 1, "{:?}", s.dev.targets());
    assert_eq!(posts[0].target, "/cgi-bin/statusedit.cgi");
    assert_eq!(
        s.dev.rows[0],
        ("UDP://239.10.0.1:5000".to_owned(), "WALL".to_owned())
    );
    let logged: Vec<_> = s
        .actions
        .iter()
        .filter_map(|(t, a)| {
            if let Action::WriteLogged(r) = a {
                Some((*t, r))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].1.outcome, WriteOutcome::Verified);
    // Confirmed on the second status poll (~30 s), not the first.
    assert!(
        logged[0].0 >= secs(30) && logged[0].0 < secs(120),
        "{:?}",
        logged[0].0
    );
    assert_eq!(
        s.ctl
            .supervisor()
            .metrics()
            .writes_total
            .get(&WriteKind::StreamTable),
        Some(&1)
    );
    assert_eq!(s.ctl.supervisor().metrics().drift_items, 0);
}

#[test]
fn rf_drift_is_applied_as_the_four_module_sequence() {
    let mut dev = FakeDevice::surveyed();
    dev.modules[3].on[2] = false; // someone turned carrier 15 off
    let mut s = Sim::new(ReconcileMode::Apply, StreamTable::empty(), dev, |_| {});
    s.run_until(secs(1800), |_, _| {});
    let posts: Vec<_> = s.dev.posts().iter().map(|p| p.target.clone()).collect();
    assert_eq!(posts, vec!["/cgi-bin/qamconfig.cgi"; 4]);
    assert_eq!(s.dev.modules[3].on, [true; 4]);
    assert_eq!(
        s.ctl
            .supervisor()
            .metrics()
            .writes_total
            .get(&WriteKind::RfPlan),
        Some(&1)
    );
}

#[test]
fn same_drift_returning_after_three_reapplies_halts_writes() {
    // Slow polls keep the two simulated days short; the limits do not depend on them.
    let mut s = Sim::new(
        ReconcileMode::Apply,
        one_stream(),
        FakeDevice::surveyed(),
        |c| {
            c.status_interval = secs(120);
            c.slow_interval = secs(3600);
        },
    );
    // Something else clears slot 1 two minutes after every write.
    let mut set_at: Option<Duration> = None;
    s.run_until(secs(2 * 86_400), |dev, now| {
        if dev.rows[0].0.is_empty() {
            set_at = None;
        } else if set_at.is_none() {
            set_at = Some(now);
        } else if now >= set_at.unwrap() + secs(120) {
            dev.rows[0] = (String::new(), String::new());
        }
    });
    let posts: Vec<Duration> = s
        .actions
        .iter()
        .filter_map(|(t, a)| match a {
            Action::WriteLogged(r) if r.posts > 0 => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(posts.len(), 3, "{posts:?}");
    for w in posts.windows(2) {
        assert!(
            w[1] - w[0] >= secs(600),
            "reconciles closer than 10 min: {posts:?}"
        );
    }
    let halted: Vec<_> = s
        .alarms()
        .into_iter()
        .filter(|a| matches!(a, Alarm::WritesHalted(_)))
        .collect();
    assert_eq!(halted.len(), 1);
    assert!(matches!(
        halted[0],
        Alarm::WritesHalted(HaltReason::SameDriftReturned {
            kind: WriteKind::StreamTable,
            ..
        })
    ));
    assert!(s.ctl.supervisor().halted().is_some());
    assert!(
        s.actions
            .iter()
            .any(|(_, a)| matches!(a, Action::RateLimited { .. }))
    );
}

#[test]
fn at_most_three_reconciles_per_day() {
    let mut s = Sim::new(
        ReconcileMode::Apply,
        one_stream(),
        FakeDevice::surveyed(),
        |c| {
            c.same_drift_limit = 100;
            c.status_interval = secs(120);
            c.slow_interval = secs(3600);
        },
    );
    let mut set_at: Option<Duration> = None;
    s.run_until(secs(86_400 - 600), |dev, now| {
        if dev.rows[0].0.is_empty() {
            set_at = None;
        } else if set_at.is_none() {
            set_at = Some(now);
        } else if now >= set_at.unwrap() + secs(120) {
            dev.rows[0] = (String::new(), String::new());
        }
    });
    assert_eq!(s.dev.posts().len(), 3);
}

#[test]
fn reboot_triggers_a_full_comparison_and_event_log_alarms_again() {
    let mut s = Sim::new(
        ReconcileMode::Report,
        StreamTable::empty(),
        FakeDevice::surveyed(),
        |_| {},
    );
    s.run_until(secs(400), |_, _| {});
    let before = s
        .alarms()
        .iter()
        .filter(|a| matches!(a, Alarm::EventLog(..)))
        .count();
    assert_eq!(
        before, 1,
        "the boot line is reported once, not on every poll"
    );
    let n0 = s.dev.log.len();
    // Reboot: uptime restarts, the RAM log is rebuilt with the same lines.
    s.dev.uptime = secs(40);
    s.run_until(secs(480), |_, _| {});
    assert!(
        s.alarms()
            .iter()
            .any(|a| matches!(a, Alarm::Rebooted { .. }))
    );
    let after: Vec<String> = s.dev.targets()[n0..].to_vec();
    for p in [
        "GET qamconfig.cgi?slotNumber=1",
        "GET qamconfig.cgi?slotNumber=4",
        "GET streaminfo.cgi",
        "GET diagnostic.cgi",
        "GET debug.cgi",
        "GET errorlog.cgi",
    ] {
        assert!(
            after.iter().any(|t| t == p),
            "{p} not re-read after reboot: {after:?}"
        );
    }
    assert_eq!(
        s.alarms()
            .iter()
            .filter(|a| matches!(a, Alarm::EventLog(..)))
            .count(),
        2
    );
    let m = s.ctl.supervisor().metrics();
    assert!(m.seconds_since_table_reset.unwrap() < 200);
}

#[test]
fn defect_d_limit_alarms_and_clears_on_reset() {
    let mut dev = FakeDevice::surveyed();
    dev.uptime = secs(86_400 - 120);
    let mut s = Sim::new(ReconcileMode::Report, StreamTable::empty(), dev, |c| {
        c.defect_d_limit = Some(secs(86_400))
    });
    s.run_until(secs(60), |_, _| {});
    assert!(
        !s.alarms()
            .iter()
            .any(|a| matches!(a, Alarm::DefectDLimit { .. }))
    );
    s.run_until(secs(300), |_, _| {});
    assert!(
        s.alarms()
            .iter()
            .any(|a| matches!(a, Alarm::DefectDLimit { .. }))
    );
    let now = s.now;
    s.ctl.supervisor_mut().note_table_reset(now);
    s.run_until(now + secs(60), |_, _| {});
    assert!(
        s.actions
            .iter()
            .any(|(_, a)| *a == Action::Clear(AlarmKey::DefectD))
    );
}

#[test]
fn nothing_is_written_outside_mpts() {
    let mut dev = FakeDevice::surveyed();
    dev.mpts = false;
    let mut s = Sim::new(ReconcileMode::Apply, one_stream(), dev, |_| {});
    s.run_until(secs(3600), |_, _| {});
    assert!(s.dev.posts().is_empty());
    assert!(s.alarms().iter().any(|a| matches!(a, Alarm::NotMpts(_))));
}

#[test]
fn logged_out_relogs_and_repeats_the_read() {
    let mut dev = FakeDevice::surveyed();
    dev.logout_after = Some(6);
    let mut s = Sim::new(ReconcileMode::Report, StreamTable::empty(), dev, |_| {});
    s.run_until(secs(120), |_, _| {});
    assert_eq!(s.count("POST login.cgi"), 2);
    assert!(!s.alarms().iter().any(|a| matches!(a, Alarm::Unreachable)));
}

#[test]
fn reads_are_retried_once_and_unreachable_is_raised_and_cleared() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = false;
    // Login page, login post, then status times out once: retried, no failure.
    let mut s = Sim::new(ReconcileMode::Report, StreamTable::empty(), dev, |_| {});
    s.run_until(secs(2), |_, _| {});
    s.dev.failures = vec![TransportFailure::Timeout];
    s.run_until(secs(40), |_, _| {});
    assert!(!s.alarms().iter().any(|a| matches!(a, Alarm::Unreachable)));
    // Now everything times out for a while.
    s.dev.failures = vec![TransportFailure::Timeout; 40];
    s.run_until(secs(300), |_, _| {});
    assert!(s.alarms().iter().any(|a| matches!(a, Alarm::Unreachable)));
    assert!(!s.ctl.supervisor().metrics().reachable);
    s.run_until(secs(600), |_, _| {});
    assert!(
        s.actions
            .iter()
            .any(|(_, a)| *a == Action::Clear(AlarmKey::Unreachable))
    );
    // Posts were never retried (there were none) and nothing was refused.
    assert!(
        s.dev
            .log
            .iter()
            .all(|l| l.method == Method::Get || l.target.contains("login"))
    );
}

#[test]
fn pause_stops_polling() {
    let mut s = Sim::new(
        ReconcileMode::Report,
        StreamTable::empty(),
        FakeDevice::surveyed(),
        |_| {},
    );
    s.run_until(secs(60), |_, _| {});
    let n = s.dev.log.len();
    let now = s.now;
    s.ctl.supervisor_mut().pause(now + secs(1800));
    s.run_until(now + secs(1799), |_, _| {});
    assert_eq!(s.dev.log.len(), n);
    s.run_until(now + secs(1900), |_, _| {});
    assert!(s.dev.log.len() > n);
}

#[test]
fn a_stray_statusedit_observation_does_not_start_polling_it() {
    use tb_eqam::ReadPage;
    use tb_eqam::client::{ClientEvent, Job, Observation};
    use tb_eqam::supervisor::Supervisor;
    let desired = DesiredState {
        rf: live_plan(),
        streams: StreamTable::empty(),
    };
    let mut sup = Supervisor::new(SupervisorConfig::default(), desired, Duration::ZERO);
    assert_eq!(sup.tick(Duration::ZERO).len(), 9);
    let o = Observation::parse(ReadPage::StatusEdit, common::STATUS_EDIT).unwrap();
    sup.on_event(secs(1), ClientEvent::Observed(Box::new(o)));
    let later = sup.tick(secs(10_000));
    assert!(!later.contains(&Action::Job(Job::Read(ReadPage::StatusEdit))));
}
