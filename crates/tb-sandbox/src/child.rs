//! The sandboxed program's side: what it does first, and what it does once it is initialized.
//!
//! ```text
//! fn main() {
//!     let mut sb = tb_sandbox::child::enter()?;      // first thing, single-threaded
//!     let ipc = sb.take_ipc();
//!     ... initialize libraries (they may still open files they need) ...
//!     sb.lock_down(Profile::Compute, Action::Kill)?; // seccomp; the report is complete
//!     ... only now read untrusted input ...
//! }
//! ```
//!
//! [`enter`]:
//!
//! 1. With a PID namespace the program is its PID 1. It forks: the parent becomes the
//!    namespace's init (death signal, not dumpable, Landlock with no rights at all, a seccomp
//!    filter that allows waiting and one `write`), reaps, and when the program ends writes its
//!    status on the status pipe and exits; the child goes on as the program. A PID 1 cannot be
//!    killed by signals it has no handler for, so a crash of the program (`SIGSEGV`,
//!    `SIGABRT`) must not happen in PID 1; and when the init dies, the kernel kills everything
//!    in the namespace, so killing the init is how the parent kills the whole sandbox.
//! 2. The status pipe is closed (the program cannot forge its own exit record).
//! 3. `PR_SET_PDEATHSIG(SIGKILL)` (set here and not before `execve`, which clears it when it
//!    drops the capabilities of the user namespace); a parent already gone ends the program.
//! 4. `PR_SET_DUMPABLE` 0.
//! 5. Landlock: of the file system, only reading and executing the paths the parent named
//!    (and read-write on the device nodes); no TCP bind or connect (ABI 4); no abstract UNIX
//!    sockets or signals outside the domain (ABI 6). Best effort or required, as the policy
//!    says; the ABI in effect is reported.
//!
//! [`Entered::lock_down`] installs the seccomp filter ([`crate::seccomp`]) on all threads,
//! reports it and closes the report pipe. The program must not read untrusted input before.

use std::ffi::OsString;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, LandlockStatus, PathBeneath, PathFd,
    Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
};
use rustix::io::Errno;

use crate::report::{Layer, LayerReport, LayerState, Requirement, Status};
use crate::seccomp::{self, Action, Profile};
use crate::spawn::{EXIT_SETUP, FD_IPC, FD_REPORT, FD_STATUS};

/// Why the program cannot go on.
#[derive(Debug, thiserror::Error)]
pub enum EnterError {
    /// The program was not started by [`crate::spawn()`].
    #[error("not started in a tb-sandbox (TB_SANDBOX is not set)")]
    NotSandboxed,
    /// The parent has already gone.
    #[error("the parent process has gone")]
    Orphaned,
    /// `fork` for the namespace's init failed.
    #[error("fork for the namespace init: {0}")]
    Fork(Errno),
    /// A required layer could not be put in place.
    #[error("required layer {0} not in effect: {1}")]
    Required(&'static str, String),
}

fn requirement(var: &str) -> Requirement {
    match std::env::var(var).as_deref() {
        Ok("required") => Requirement::Required,
        Ok("off") => Requirement::Off,
        _ => Requirement::BestEffort,
    }
}

fn paths(var: &str) -> Vec<PathBuf> {
    std::env::var_os(var)
        .map(|v| {
            v.as_bytes()
                .split(|b| *b == b':')
                .filter(|s| !s.is_empty())
                .map(|s| PathBuf::from(OsString::from(std::ffi::OsStr::from_bytes(s))))
                .collect()
        })
        .unwrap_or_default()
}

fn report_to(fd: Option<&OwnedFd>, r: LayerReport) {
    if let Some(fd) = fd {
        let _ = rustix::io::write(fd, &r.encode());
    }
}

/// The program, inside its sandbox, before lock-down.
#[derive(Debug)]
pub struct Entered {
    ipc: Option<OwnedFd>,
    report: Option<OwnedFd>,
    seccomp: Requirement,
    landlock_abi: u32,
}

/// Takes ownership of one of the descriptors the sandbox placed, if it is open.
fn adopt(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: F_GETFD only asks whether `fd` is open; no memory involved.
    let open = unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0;
    // SAFETY: the sandbox placed this descriptor for this program and nothing else in the
    // process owns it (`enter` runs first in `main`, and adopts each number once).
    open.then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Enters the sandbox. Call first in `main`, while the process has one thread.
pub fn enter() -> Result<Entered, EnterError> {
    if std::env::var_os("TB_SANDBOX").is_none() {
        return Err(EnterError::NotSandboxed);
    }
    let status = adopt(FD_STATUS);
    let report = adopt(FD_REPORT);
    let ipc = adopt(FD_IPC);
    let pid_ns = std::env::var("TB_SANDBOX_PIDNS").as_deref() == Ok("1");

    if pid_ns && rustix::process::getpid().as_raw_nonzero().get() == 1 {
        // SAFETY: the process is single-threaded (the precondition of this function), so the
        // child is a complete copy that may go on running Rust code.
        let child = unsafe { libc::fork() };
        if child < 0 {
            return Err(EnterError::Fork(Errno::from_raw_os_error(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            )));
        }
        if child > 0 {
            drop(ipc);
            drop(report);
            run_init(child, status);
        }
        // the program: pid 2 of the namespace
    }
    // the program does not keep the status pipe: it cannot forge its own end
    drop(status);

    // the death signal: when the parent (the server, or the namespace's init) dies
    let ds = rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL));
    report_to(
        report.as_ref(),
        match ds {
            Ok(()) => LayerReport::applied(Layer::DeathSignal, 0),
            Err(e) => LayerReport::new(Layer::DeathSignal, LayerState::Failed, e.raw_os_error(), 0),
        },
    );
    if !pid_ns {
        // without a PID namespace, the parent's pid is known: if it is not ours any more, the
        // parent died before the death signal was set
        let expected = std::env::var("TB_SANDBOX_PPID")
            .ok()
            .and_then(|s| s.parse::<i32>().ok());
        let ppid = rustix::process::getppid().map_or(0, |p| p.as_raw_nonzero().get());
        if expected.is_some_and(|e| e != ppid) {
            return Err(EnterError::Orphaned);
        }
    }
    let nd = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
    report_to(
        report.as_ref(),
        match nd {
            Ok(()) => LayerReport::applied(Layer::NotDumpable, 0),
            Err(e) => LayerReport::new(Layer::NotDumpable, LayerState::Failed, e.raw_os_error(), 0),
        },
    );

    let ll_req = requirement("TB_SANDBOX_LANDLOCK");
    let ll = if ll_req == Requirement::Off {
        LayerReport::new(Layer::Landlock, LayerState::Off, 0, 0)
    } else {
        landlock(
            &paths("TB_SANDBOX_RO"),
            &paths("TB_SANDBOX_DEV"),
            &paths("TB_SANDBOX_RW"),
        )
    };
    report_to(report.as_ref(), ll);
    if ll_req == Requirement::Required && !ll.state.in_effect() {
        return Err(EnterError::Required("landlock", format!("{ll:?}")));
    }
    Ok(Entered {
        ipc,
        report,
        seccomp: requirement("TB_SANDBOX_SECCOMP"),
        landlock_abi: if ll.state.in_effect() { ll.detail } else { 0 },
    })
}

/// Landlock with read and execute rights on `ro`, read-write on `dev`, nothing else.
fn landlock(ro: &[PathBuf], dev: &[PathBuf], rw: &[PathBuf]) -> LayerReport {
    // ABI 6 has every right this uses (file system, TCP, scopes); later ABIs add logging
    // controls. Older kernels get what they support (best effort) and the level is reported.
    let abi = ABI::V6;
    // a rule on a file may only carry rights that apply to files
    let file_only = |p: &PathBuf| !p.is_dir();
    let build = || -> Result<landlock::RestrictionStatus, landlock::RulesetError> {
        let mut rs = Ruleset::default()
            .set_compatibility(CompatLevel::BestEffort)
            .handle_access(AccessFs::from_all(abi))?
            .handle_access(AccessNet::from_all(abi))?
            .scope(Scope::from_all(abi))?
            .create()?;
        for p in ro {
            let mut rights = AccessFs::from_read(abi);
            if file_only(p) {
                rights &= AccessFs::from_file(abi);
            }
            if let Ok(fd) = PathFd::new(p) {
                rs = rs.add_rule(PathBeneath::new(fd, rights))?;
            }
        }
        for p in dev {
            let rights = (AccessFs::from_read(abi) | AccessFs::WriteFile | AccessFs::IoctlDev)
                & AccessFs::from_file(abi);
            if let Ok(fd) = PathFd::new(p) {
                rs = rs.add_rule(PathBeneath::new(fd, rights))?;
            }
        }
        for p in rw {
            // (truncate too: writers open with O_TRUNC, a right of its own from ABI 3)
            let rights = AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Truncate;
            if let Ok(fd) = PathFd::new(p) {
                rs = rs.add_rule(PathBeneath::new(fd, rights))?;
            }
        }
        rs.restrict_self()
    };
    match build() {
        Ok(st) => {
            let level = match st.landlock {
                LandlockStatus::Available { effective_abi, .. } => {
                    (effective_abi as u32).min(abi as u32)
                }
                _ => 0,
            };
            match st.ruleset {
                RulesetStatus::FullyEnforced => LayerReport::applied(Layer::Landlock, level),
                RulesetStatus::PartiallyEnforced => {
                    LayerReport::new(Layer::Landlock, LayerState::Partial, 0, level)
                }
                RulesetStatus::NotEnforced => LayerReport::new(
                    Layer::Landlock,
                    LayerState::Unavailable,
                    libc::ENOSYS,
                    level,
                ),
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "landlock");
            LayerReport::new(Layer::Landlock, LayerState::Failed, libc::EINVAL, 0)
        }
    }
}

/// The namespace's init: confine itself, reap, report the program's end, exit.
fn run_init(program: i32, status: Option<OwnedFd>) -> ! {
    let _ = rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL));
    let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
    // no file system rights at all; then only waiting and writing
    let _ = landlock(&[], &[], &[]);
    if let Ok(p) = seccomp::build(Profile::Init, Action::Kill) {
        let _ = seccomp::install(&p);
    }
    loop {
        match rustix::process::wait(rustix::process::WaitOptions::empty()) {
            Ok(Some((pid, st))) if pid.as_raw_nonzero().get() == program => {
                let rec = match st.terminating_signal() {
                    Some(s) => Status::Signaled(s),
                    None => Status::Exited(st.exit_status().unwrap_or(0)),
                };
                if let Some(fd) = &status {
                    let _ = rustix::io::write(fd, &rec.encode());
                }
                // SAFETY: ends the init at once.
                unsafe { libc::_exit(0) }
            }
            Ok(_) | Err(Errno::INTR) => {}
            // SAFETY: as above.
            Err(_) => unsafe { libc::_exit(EXIT_SETUP) },
        }
    }
}

impl Entered {
    /// The IPC descriptor (fd 3), once.
    pub fn take_ipc(&mut self) -> Option<OwnedFd> {
        self.ipc.take()
    }

    /// The Landlock ABI in effect (0: none).
    pub fn landlock_abi(&self) -> u32 {
        self.landlock_abi
    }

    /// Installs the seccomp filter of `profile` (unless the policy turned seccomp off),
    /// reports it, and closes the report pipe. Returns the seccomp layer's report.
    pub fn lock_down(
        mut self,
        profile: Profile,
        action: Action,
    ) -> Result<LayerReport, EnterError> {
        let r = if self.seccomp == Requirement::Off {
            LayerReport::new(Layer::Seccomp, LayerState::Off, 0, 0)
        } else {
            match seccomp::build(profile, action).and_then(|p| seccomp::install(&p)) {
                Ok(()) => LayerReport::applied(Layer::Seccomp, action.code()),
                Err(e) => {
                    tracing::warn!(error = %e, "seccomp");
                    LayerReport::new(Layer::Seccomp, LayerState::Failed, libc::EINVAL, 0)
                }
            }
        };
        report_to(self.report.as_ref(), r);
        // the report is complete
        self.report = None;
        if self.seccomp == Requirement::Required && !r.state.in_effect() {
            return Err(EnterError::Required("seccomp", format!("{r:?}")));
        }
        Ok(r)
    }
}
