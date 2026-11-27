//! The sandbox really denies things: the probe program (`src/bin/probe.rs`) is started in the
//! sandbox and reports what it could do. Assertions depend on what the machine offers: with
//! user namespaces (a workstation, the server) every layer is checked; without them (the Nix
//! build sandbox may forbid them) the namespace checks are skipped, loudly, and the layers
//! that need no privilege (Landlock, seccomp, rlimits) are still checked.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::BTreeMap;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::time::Duration;

use tb_sandbox::{Child, Exit, ExitStatus, Layer, Policy, Requirement, Stderr};

const PROBE: &str = env!("CARGO_BIN_EXE_tb-sandbox-probe");

struct Run {
    lines: BTreeMap<String, i32>,
    landlock_abi: u32,
    done: bool,
    exit: Exit,
    child: Child,
}

/// An abstract UNIX socket listening outside the sandbox (its name is the probe's target).
fn listener() -> (OwnedFd, String) {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};
    let name = format!("tb-sandbox-probe-{}", std::process::id());
    let s = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
    rustix::net::bind(
        &s,
        &SocketAddrUnix::new_abstract_name(name.as_bytes()).unwrap(),
    )
    .unwrap();
    rustix::net::listen(&s, 4).unwrap();
    (s, name)
}

fn start(policy: &Policy, args: &[&str]) -> (Child, std::fs::File) {
    let (r, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    let mut cmd = tb_sandbox::Command::new(PROBE);
    cmd.env(
        "TB_PROBE_ABSTRACT",
        format!("tb-sandbox-probe-{}", std::process::id()),
    );
    for a in args {
        cmd.arg(a);
    }
    let child = tb_sandbox::spawn(&cmd, w, policy).expect("spawn");
    (child, std::fs::File::from(r))
}

fn run(policy: &Policy, args: &[&str]) -> Run {
    let (mut child, mut out) = start(policy, args);
    let mut s = String::new();
    out.read_to_string(&mut s).unwrap();
    let exit = child.wait().unwrap();
    child.finish_reports(Duration::from_secs(1));
    let mut lines = BTreeMap::new();
    let mut landlock_abi = 0;
    let mut done = false;
    for l in s.lines() {
        let f: Vec<&str> = l.split(' ').collect();
        match f.as_slice() {
            [stage, op, e] => {
                lines.insert(format!("{stage} {op}"), e.parse().unwrap());
            }
            ["landlock", n] => landlock_abi = n.parse().unwrap(),
            ["done"] => done = true,
            _ => panic!("unexpected line {l:?}"),
        }
    }
    eprintln!("confinement: {}", child.confinement());
    eprintln!("probe: {lines:?}");
    Run {
        lines,
        landlock_abi,
        done,
        exit,
        child,
    }
}

fn policy() -> Policy {
    Policy {
        stderr: Stderr::Inherit,
        ..Policy::default()
    }
}

fn denied(r: &Run, key: &str) {
    let e = *r.lines.get(key).unwrap_or_else(|| panic!("no {key}"));
    assert_ne!(e, 0, "{key} succeeded in the sandbox");
}

#[test]
fn every_layer_reports_and_denies() {
    let (_l, name) = listener();
    // the listener is reachable from outside the sandbox
    {
        use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};
        let s = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
        rustix::net::connect(
            &s,
            &SocketAddrUnix::new_abstract_name(name.as_bytes()).unwrap(),
        )
        .unwrap();
    }
    let r = run(&policy(), &["exec"]);
    assert!(r.done, "probe did not finish: {:?}", r.exit);
    assert_eq!(r.exit.status, ExitStatus::Exited(0));
    let c = r.child.confinement();
    for l in [
        Layer::NoNewPrivs,
        Layer::CloseFds,
        Layer::Rlimits,
        Layer::DeathSignal,
        Layer::NotDumpable,
        Layer::Seccomp,
    ] {
        assert!(c.in_effect(l), "{l:?} not in effect: {c}");
    }
    // after the filter (errno mode), whatever else is on
    for op in [
        "Socket",
        "ConnectUnix",
        "Network",
        "ReadPasswd",
        "WriteFile",
        "Ptrace",
        "SignalOutside",
        "Exec",
    ] {
        denied(&r, &format!("after {op}"));
    }
    assert_eq!(r.lines["after Socket"], libc::EPERM);
    assert_eq!(r.lines["after Exec"], libc::EPERM);
    assert_eq!(r.lines["after ReadPasswd"], libc::EPERM);

    if c.in_effect(Layer::UserNs) {
        assert!(c.in_effect(Layer::MountNs), "{c}");
        assert!(
            c.in_effect(Layer::PidNs) && c.in_effect(Layer::NetNs),
            "{c}"
        );
        // the namespaces alone, before the filter
        assert_eq!(r.lines["before ReadPasswd"], libc::ENOENT);
        denied(&r, "before WriteFile");
        assert_eq!(r.lines["before Network"], libc::ENETUNREACH);
        // (meaningless when this process is itself pid 1 or 2 of an outer namespace: the
        // same numbers exist inside)
        if std::process::id() > 2 {
            assert_eq!(r.lines["before SignalOutside"], libc::ESRCH);
        }
        denied(&r, "before ConnectUnix");
        denied(&r, "before Ptrace");
    } else {
        eprintln!("SKIPPED: no user namespaces here; namespace layers not checked");
    }
    if c.in_effect(Layer::Landlock) {
        assert!(r.landlock_abi >= 1);
        denied(&r, "before ReadPasswd");
        denied(&r, "before WriteFile");
    } else {
        eprintln!("SKIPPED: no Landlock here");
    }
}

#[test]
fn without_namespaces_landlock_and_seccomp_still_hold() {
    let p = Policy {
        user_ns: Requirement::Off,
        mount_ns: Requirement::Off,
        pid_ns: Requirement::Off,
        net_ns: Requirement::Off,
        ipc_ns: Requirement::Off,
        uts_ns: Requirement::Off,
        ..policy()
    };
    let r = run(&p, &[]);
    assert!(r.done);
    let c = r.child.confinement();
    assert!(!c.in_effect(Layer::UserNs));
    assert!(c.in_effect(Layer::Seccomp));
    assert_eq!(r.lines["after Socket"], libc::EPERM);
    if c.in_effect(Layer::Landlock) {
        // the host's /etc/passwd exists; Landlock denies reading it
        assert_eq!(r.lines["before ReadPasswd"], libc::EACCES);
        denied(&r, "before WriteFile");
        if r.landlock_abi >= 6 {
            // scoped signals: the server is outside the domain
            assert_eq!(r.lines["before SignalOutside"], libc::EPERM);
        }
    }
}

#[test]
fn seccomp_kill_mode_is_sigsys() {
    let r = run(&policy(), &["kill"]);
    assert!(r.done);
    assert_eq!(r.exit.status, ExitStatus::Signaled(libc::SIGSYS));
}

#[test]
fn the_programs_own_end_comes_through_its_init() {
    let r = run(&policy(), &["crash"]);
    assert_eq!(r.exit.status, ExitStatus::Signaled(libc::SIGSEGV));
    let r = run(&policy(), &["exit=7"]);
    assert_eq!(r.exit.status, ExitStatus::Exited(7));
    assert_eq!(r.exit.setup_failed, None);
}

#[test]
fn kill_through_the_pidfd_ends_the_whole_sandbox() {
    let (mut child, mut out) = start(&policy(), &["sleep"]);
    // wait for "done": the program is in its sleep
    let mut buf = [0u8; 4096];
    let mut s = String::new();
    while !s.contains("done") {
        let n = out.read(&mut buf).unwrap();
        assert!(n > 0);
        s.push_str(std::str::from_utf8(&buf[..n]).unwrap());
    }
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    let e = child.wait().unwrap();
    assert_eq!(e.status, ExitStatus::Signaled(libc::SIGKILL));
    // the program (pid 2 of the namespace, holding the pipe) died with its init: EOF
    assert_eq!(out.read(&mut buf).unwrap(), 0);
}

#[test]
fn a_missing_program_is_a_setup_failure() {
    let (r, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    drop(r);
    let p = Policy {
        mount_ns: Requirement::Off,
        ..policy()
    };
    let cmd = tb_sandbox::Command::new("/nonexistent/program");
    let mut child = tb_sandbox::spawn(&cmd, w, &p).unwrap();
    let e = child.wait().unwrap();
    assert_eq!(e.status, ExitStatus::Exited(tb_sandbox::EXIT_SETUP));
    assert_eq!(
        e.setup_failed,
        Some((tb_sandbox::report::Stage::Exec, libc::ENOENT))
    );
}

#[test]
fn bad_policies_are_refused() {
    let (_, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    let p = Policy {
        user_ns: Requirement::Off,
        net_ns: Requirement::Required,
        ..policy()
    };
    assert!(tb_sandbox::spawn(&tb_sandbox::Command::new(PROBE), w, &p).is_err());
    let (_, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    assert!(tb_sandbox::spawn(&tb_sandbox::Command::new("relative"), w, &policy()).is_err());
}

/// A parent with more descriptors open than the child's RLIMIT_NOFILE (a server some time into
/// its run, with its GPU, sockets and other workers) still starts a child: the limit is set
/// after the descriptors are moved into place and the rest closed.
#[test]
fn a_parent_with_many_descriptors_still_starts_a_child() {
    let p = policy();
    let held: Vec<std::fs::File> = (0..2 * p.rlimits.nofile)
        .map(|_| std::fs::File::open("/dev/null").unwrap())
        .collect();
    let r = run(&p, &["exit=7"]);
    assert_eq!(r.exit.setup_failed, None, "{}", r.child.confinement());
    assert_eq!(r.exit.status, ExitStatus::Exited(7));
    drop(held);
}
