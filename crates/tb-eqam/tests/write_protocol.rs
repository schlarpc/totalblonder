//! The two writes against the offline fake device built from the saved pages.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use common::{FakeDevice, SID};
use tb_eqam::Method;
use tb_eqam::client::{Client, ClientConfig, ClientEvent, Job, Next};
use tb_eqam::plan::{ModulePlan, RfPlan, SlotConfig, StreamTable};
use tb_eqam::types::{Credentials, FirstChannel, OutputLevel, QamMap, QamMode, Slot};
use tb_eqam::write::{WriteError, WriteOutcome, WriteReport};

/// Run one write job through the public client (it logs in first).
fn run(dev: &mut FakeDevice, job: Job) -> WriteReport {
    let mut c = Client::new(
        Credentials::new("Admin", "pass").unwrap(),
        ClientConfig::default(),
    );
    c.submit(job);
    let mut now = Duration::ZERO;
    for _ in 0..64 {
        let resp = match c.next(now) {
            Next::Send(req) => dev.handle(req),
            Next::WaitUntil(t) => {
                now = t;
                continue;
            }
            other => panic!("{other:?} before the write finished"),
        };
        now += Duration::from_millis(100);
        for ev in c.on_response(now, resp) {
            if let ClientEvent::WriteFinished(r) = ev {
                return r;
            }
        }
    }
    panic!("write did not finish");
}

/// Requests other than the login exchange.
fn targets(dev: &FakeDevice) -> Vec<String> {
    dev.targets()
        .into_iter()
        .filter(|t| !t.contains("login"))
        .collect()
}

fn table() -> StreamTable {
    StreamTable::empty()
        .with(
            Slot::new(1).unwrap(),
            SlotConfig::Assigned {
                url: "UDP://239.10.0.1:5000".parse().unwrap(),
                name: "WALL".parse().unwrap(),
            },
        )
        .unwrap()
        .with(
            Slot::new(9).unwrap(),
            SlotConfig::Assigned {
                url: "RTP://239.10.0.9:5000".parse().unwrap(),
                name: "NEWS9".parse().unwrap(),
            },
        )
        .unwrap()
}

fn write_table(dev: &mut FakeDevice, t: StreamTable) -> WriteReport {
    run(dev, Job::WriteStreams(t))
}

fn write_plan(dev: &mut FakeDevice, p: RfPlan) -> WriteReport {
    run(dev, Job::WriteRfPlan(p))
}

fn plan(firsts: [u8; 4]) -> RfPlan {
    let m = |ch| ModulePlan {
        first_channel: FirstChannel::new(ch).unwrap(),
        qam: QamMode::Qam256B,
        map: QamMap::Std,
        carriers_on: [true; 4],
    };
    RfPlan::new(OutputLevel::new(40).unwrap(), firsts.map(m)).unwrap()
}

#[test]
fn stream_table_write_posts_all_rows_and_verifies() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    let r = write_table(&mut dev, table());
    assert_eq!(r.outcome, WriteOutcome::Verified, "{r:?}");
    assert_eq!(r.posts, 1);
    assert_eq!(
        targets(&dev),
        [
            "GET statusedit.cgi",
            "POST statusedit.cgi",
            "GET statusedit.cgi",
            "GET status.cgi"
        ]
    );
    let post = &dev.posts()[0];
    // Every row, in page order, then session_id and the Save button; no disabled fields.
    assert_eq!(post.fields.len(), 16 * 2 + 2);
    for n in 1..=16 {
        assert!(
            post.fields
                .iter()
                .any(|(k, _)| *k == format!("streamGroupAddress{n}"))
        );
        assert!(
            post.fields
                .iter()
                .any(|(k, _)| *k == format!("streamName{n}"))
        );
    }
    assert!(
        !post
            .fields
            .iter()
            .any(|(k, _)| k.starts_with("streamSourceAddress") || k.starts_with("proIdiom"))
    );
    assert_eq!(
        post.fields[0],
        ("streamGroupAddress1".into(), "UDP://239.10.0.1:5000".into())
    );
    assert_eq!(post.fields[1], ("streamName1".into(), "WALL".into()));
    assert_eq!(post.fields[32], ("session_id".into(), SID.into()));
    assert_eq!(post.fields[33], ("btnSave".into(), "Save".into()));
    assert_eq!(post.method, Method::Post);
    assert_eq!(post.target, "/cgi-bin/statusedit.cgi");
    assert_eq!(
        dev.rows[8],
        ("RTP://239.10.0.9:5000".to_owned(), "NEWS9".to_owned())
    );
    assert_eq!(r.before.len(), 16);
    assert_eq!(r.after[0], "slot 1 = \"UDP://239.10.0.1:5000\" \"WALL\"");
}

#[test]
fn a_second_write_changes_only_the_intended_row_and_keeps_the_rest() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    assert_eq!(
        write_table(&mut dev, table()).outcome,
        WriteOutcome::Verified
    );
    let t2 = table()
        .with(
            Slot::new(16).unwrap(),
            SlotConfig::Assigned {
                url: "UDP://239.10.0.16:5000".parse().unwrap(),
                name: "S16".parse().unwrap(),
            },
        )
        .unwrap();
    let r = write_table(&mut dev, t2);
    assert_eq!(r.outcome, WriteOutcome::Verified);
    // Rows 1 and 9 were posted back unchanged (a row left out would have been zeroed).
    assert_eq!(dev.rows[0].1, "WALL");
    assert_eq!(dev.rows[8].1, "NEWS9");
    assert_eq!(dev.rows[15].1, "S16");
}

#[test]
fn already_in_place_posts_nothing() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    let r = write_table(&mut dev, StreamTable::empty());
    assert_eq!(r.outcome, WriteOutcome::AlreadyInPlace);
    assert_eq!(r.posts, 0);
    assert!(dev.posts().is_empty());
}

#[test]
fn an_unexpected_field_aborts_before_posting() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    dev.statusedit_extra = "<input type=\"text\" name=\"programNumber1\" value=\"0\">".into();
    let r = write_table(&mut dev, table());
    match r.outcome {
        WriteOutcome::Aborted(WriteError::FieldSet(e)) => {
            assert_eq!(e.extra, ["programNumber1 (text)"])
        }
        other => panic!("{other:?}"),
    }
    assert!(dev.posts().is_empty());
}

#[test]
fn a_write_that_does_not_stick_fails_on_read_back() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    dev.ignore_writes = true;
    let r = write_table(&mut dev, table());
    match r.outcome {
        WriteOutcome::Failed(WriteError::ReadBack(mm)) => {
            assert_eq!(mm.len(), 4);
            assert_eq!(mm[0].what, "slot 1 url");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(r.posts, 1);
}

#[test]
fn logged_out_before_posting_aborts_cleanly() {
    let mut dev = FakeDevice::surveyed();
    // Logged out right after the login exchange (login page, login post).
    dev.logout_after = Some(2);
    let r = write_table(&mut dev, table());
    assert_eq!(r.outcome, WriteOutcome::Aborted(WriteError::LoggedOut));
    assert!(dev.posts().is_empty());
}

#[test]
fn rf_plan_is_written_as_the_four_module_sequence() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    // Module 3 moves from 6/95/96/97 to 94/100/101/102.
    let r = write_plan(&mut dev, plan([2, 10, 94, 140]));
    assert_eq!(r.outcome, WriteOutcome::Verified, "{r:?}");
    assert_eq!(r.posts, 4);
    assert_eq!(
        targets(&dev),
        [
            "GET status.cgi",
            "GET qamconfig.cgi?slotNumber=1",
            "POST qamconfig.cgi",
            "GET qamconfig.cgi?slotNumber=2",
            "POST qamconfig.cgi",
            "GET qamconfig.cgi?slotNumber=3",
            "POST qamconfig.cgi",
            "GET qamconfig.cgi?slotNumber=4",
            "POST qamconfig.cgi",
            "GET qamconfig.cgi?slotNumber=1",
            "GET qamconfig.cgi?slotNumber=2",
            "GET qamconfig.cgi?slotNumber=3",
            "GET qamconfig.cgi?slotNumber=4",
            "GET status.cgi",
        ]
    );
    let posts = dev.posts();
    for (i, p) in posts.iter().enumerate() {
        let get = |k: &str| {
            p.fields
                .iter()
                .find(|(a, _)| a == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("slotNumber"), Some((i + 1).to_string().as_str()));
        assert_eq!(get("qam_saved"), Some("1"));
        assert_eq!(get("session_id"), Some(SID));
        assert_eq!(get("chkCW1"), None);
        assert_eq!(get("btnQAMSave"), None);
        assert_eq!(get("outputChannelB"), None, "disabled selects are not sent");
        assert_eq!(
            get("outputLevel").is_some(),
            i == 0,
            "level only on module 1"
        );
    }
    let names: Vec<&str> = posts[1].fields.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        [
            "slotNumber",
            "outputChannelA",
            "outputChannelControlA",
            "outputChannelControlB",
            "outputChannelControlC",
            "outputChannelControlD",
            "QAMModeA",
            "QAMMap",
            "qam_saved",
            "session_id"
        ]
    );
    assert_eq!(dev.modules[2].first, 94);
}

#[test]
fn even_an_unchanged_plan_saves_all_four_modules() {
    // Saving all four repairs the stale table bytes of defect A (SPEC §10.3 step 2).
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    let r = write_plan(&mut dev, plan([2, 10, 6, 140]));
    assert_eq!(r.outcome, WriteOutcome::Verified);
    assert_eq!(r.posts, 4);
}

#[test]
fn rf_plan_is_not_written_outside_mpts() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    dev.mpts = false;
    let r = write_plan(&mut dev, plan([2, 10, 94, 140]));
    assert_eq!(r.outcome, WriteOutcome::Aborted(WriteError::NotMpts));
    assert!(dev.posts().is_empty());
}

#[test]
fn intermediate_duplicate_channels_abort_before_posting() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    // Swap modules 2 and 3: after module 2 is saved, modules 2 and 3 would share 6/95/96/97.
    let r = write_plan(&mut dev, plan([2, 6, 10, 140]));
    assert!(
        matches!(
            r.outcome,
            WriteOutcome::Aborted(WriteError::IntermediateDuplicate(_))
        ),
        "{r:?}"
    );
    assert!(dev.posts().is_empty());
}

#[test]
fn session_lost_mid_sequence_is_a_failure_after_posting() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    // Login page, login post, status, module 1 page, module 1 post; then logged out.
    dev.logout_after = Some(5);
    let r = write_plan(&mut dev, plan([2, 10, 94, 140]));
    assert_eq!(r.outcome, WriteOutcome::Failed(WriteError::LoggedOut));
    assert_eq!(r.posts, 1);
}

#[test]
fn a_plan_that_does_not_stick_fails_on_read_back_of_all_pages() {
    let mut dev = FakeDevice::surveyed();
    dev.logged_in = true;
    dev.ignore_writes = true;
    let r = write_plan(&mut dev, plan([2, 10, 94, 140]));
    match r.outcome {
        WriteOutcome::Failed(WriteError::ReadBack(mm)) => {
            assert!(mm.iter().any(|m| m.what == "module 3 carrier A channel"
                && m.expected == "94"
                && m.observed == "6"));
            assert!(mm.iter().any(|m| m.what == "slot 9 channel"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(r.posts, 4);
}

#[test]
fn unknown_current_channels_abort_the_sequence() {
    let mut dev = FakeDevice::surveyed();
    dev.module4_unknown = true;
    let r = write_plan(&mut dev, plan([2, 10, 94, 140]));
    assert_eq!(
        r.outcome,
        WriteOutcome::Aborted(WriteError::UnknownCurrentChannels(4))
    );
    assert!(dev.posts().is_empty());
}

#[test]
fn a_checked_box_that_would_not_be_submitted_aborts() {
    let mut dev = FakeDevice::surveyed();
    dev.pro_idiom3_checked_disabled = true;
    let r = write_table(&mut dev, table());
    assert_eq!(
        r.outcome,
        WriteOutcome::Aborted(WriteError::WouldChange("proIdiomEnable3".into()))
    );
    assert!(dev.posts().is_empty());
}
