//! The parent's side: `clone3` into new namespaces, set up the child's world, `execve`.
//!
//! Everything the child needs between `clone3` and `execve` is prepared here first, in the
//! parent ([`Plan`]): paths as `CString`s, the id maps as bytes, the mount steps, argv and envp
//! as null-terminated pointer arrays. The child, a copy of a possibly multi-threaded process,
//! then only makes system calls on that data: no allocation, no locks, no panics (rustix's raw
//! system calls with `&CStr` arguments allocate nothing; the few calls rustix lacks are made
//! through `libc`). Each layer it puts in place is reported on the report pipe as it goes; a
//! failure of a required step is reported on the status pipe and ends the child with
//! [`EXIT_SETUP`].
//!
//! # Order in the child
//!
//! 1. (`clone3` made it PID 1 of new user, mount, PID, network, IPC and UTS namespaces, and
//!    gave the parent a pidfd.)
//! 2. `setgroups` deny, `uid_map` and `gid_map`: our own uid and gid, mapped to themselves.
//!    The child has every capability in its user namespace until `execve`, which drops them
//!    (the uid inside is not 0).
//! 3. Mounts: everything private; a tmpfs on `/tmp` becomes the new root; the read-only paths
//!    (default `/nix/store`) are bound in read-only with the source mount's locked flags kept;
//!    the program is bound at `/exe`; device nodes (GPU) bound read-write; `/proc` only if
//!    asked; `pivot_root`, the old root detached; the new root remounted read-only.
//! 4. Host name; resource limits; `PR_SET_NO_NEW_PRIVS`.
//! 5. File descriptors: the IPC descriptor to 3, the status pipe to 4, the report pipe to 5,
//!    `/dev/null` (opened by the parent) to 0 and 1, stderr to 2; everything else closed
//!    (`close_range`). Signal mask emptied, `SIGPIPE` to default.
//! 6. `execve` with an environment built from nothing.
//!
//! The death signal, `PR_SET_DUMPABLE`, Landlock and seccomp come after `execve`, in the
//! program ([`crate::child`]): `execve` clears the death signal when it drops capabilities,
//! and Landlock's ruleset is built more easily (and as safely) in a normal process.
//!
//! Without user namespaces (the kernel or a container forbids them), the child is a plain
//! fork: the namespace layers are reported unavailable and the rest still applies.

use std::ffi::{CString, OsStr, OsString, c_char, c_int};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags, StatVfsMountFlags};
use rustix::io::Errno;
use rustix::mount::{MountFlags, MountPropagationFlags, UnmountFlags};
use rustix::process::{Resource, Rlimit, WaitIdOptions};

use crate::report::{
    Confinement, Layer, LayerReport, LayerState, MAX_RECORDS, RECORD, Requirement, Stage, Status,
    parse_layer, parse_status,
};

/// Exit code of a child whose set-up failed (the same number as
/// `tb_decoder_proto::supervise::EXIT_SETUP`).
pub const EXIT_SETUP: i32 = 88;

/// File descriptor of the IPC channel in the child.
pub const FD_IPC: RawFd = 3;
/// File descriptor of the status pipe in the child.
pub const FD_STATUS: RawFd = 4;
/// File descriptor of the report pipe in the child.
pub const FD_REPORT: RawFd = 5;

/// Where the new root may be assembled before `pivot_root`: the first of these that is a
/// directory and holds none of the paths bound into the root (a tmpfs is mounted over it, in
/// the child's own mount namespace only). `/tmp` on any normal system; the Nix build sandbox
/// may have none, and has `/dev`.
const BASES: [&str; 8] = [
    "/tmp", "/mnt", "/run", "/var/tmp", "/opt", "/srv", "/home", "/dev",
];

/// The directory the new root is assembled on, if there is a usable one.
fn pick_base(sources: &[&Path]) -> Option<PathBuf> {
    BASES
        .iter()
        .map(PathBuf::from)
        .find(|b| b.is_dir() && !sources.iter().any(|s| s.starts_with(b)))
}
/// Where the program is bound in the new root.
pub const EXE: &str = "/exe";

/// Resource limits of the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rlimits {
    /// `RLIMIT_NOFILE`.
    pub nofile: u64,
    /// `RLIMIT_AS` in bytes (none for a GPU process: the driver reserves large address ranges).
    pub address_space: Option<u64>,
    /// `RLIMIT_CPU` in seconds (soft; `SIGXCPU`, the hard limit 5 s later is `SIGKILL`).
    pub cpu_seconds: Option<u64>,
}

impl Default for Rlimits {
    fn default() -> Self {
        Self {
            nofile: 64,
            address_space: Some(4 << 30),
            cpu_seconds: None,
        }
    }
}

/// What the child gets for stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stderr {
    /// `/dev/null`.
    Null,
    /// The parent's stderr.
    Inherit,
    /// A pipe the parent reads ([`Child::stderr`]).
    Pipe,
}

/// The confinement asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// A user namespace (the other namespaces need it when unprivileged).
    pub user_ns: Requirement,
    /// A mount namespace with a minimal read-only root.
    pub mount_ns: Requirement,
    /// A PID namespace.
    pub pid_ns: Requirement,
    /// A network namespace (no interfaces).
    pub net_ns: Requirement,
    /// An IPC namespace.
    pub ipc_ns: Requirement,
    /// A UTS namespace.
    pub uts_ns: Requirement,
    /// Landlock (applied by the program in [`crate::child::enter`]).
    pub landlock: Requirement,
    /// Seccomp (applied by the program in [`crate::child::Entered::lock_down`]).
    pub seccomp: Requirement,
    /// Paths the child may read and execute (bound read-only into its root).
    pub ro_paths: Vec<PathBuf>,
    /// Device nodes the child may open read-write (GPU decode: `/dev/nvidia*`).
    pub devices: Vec<PathBuf>,
    /// Paths the program may read and write through Landlock (not bound into the root: they
    /// must exist there, like `/proc/self/task` of a mounted `/proc`).
    pub landlock_rw: Vec<PathBuf>,
    /// Mount a `/proc` of the PID namespace.
    pub proc: bool,
    /// Resource limits.
    pub rlimits: Rlimits,
    /// stderr of the child.
    pub stderr: Stderr,
}

impl Default for Policy {
    /// Everything on, best effort for what needs kernel support beyond the basics; seccomp
    /// required (any kernel this runs on has it).
    fn default() -> Self {
        Self {
            user_ns: Requirement::BestEffort,
            mount_ns: Requirement::BestEffort,
            pid_ns: Requirement::BestEffort,
            net_ns: Requirement::BestEffort,
            ipc_ns: Requirement::BestEffort,
            uts_ns: Requirement::BestEffort,
            landlock: Requirement::BestEffort,
            seccomp: Requirement::Required,
            ro_paths: vec![PathBuf::from("/nix/store")],
            devices: Vec::new(),
            landlock_rw: Vec::new(),
            proc: false,
            rlimits: Rlimits::default(),
            stderr: Stderr::Pipe,
        }
    }
}

impl Policy {
    /// Every namespace and Landlock required.
    pub fn strict() -> Self {
        Self {
            user_ns: Requirement::Required,
            mount_ns: Requirement::Required,
            pid_ns: Requirement::Required,
            net_ns: Requirement::Required,
            ipc_ns: Requirement::Required,
            uts_ns: Requirement::Required,
            landlock: Requirement::Required,
            ..Self::default()
        }
    }

    /// The layers this policy requires.
    pub fn required(&self) -> Vec<Layer> {
        let mut v = vec![Layer::NoNewPrivs, Layer::CloseFds, Layer::Rlimits];
        for (l, r) in [
            (Layer::UserNs, self.user_ns),
            (Layer::MountNs, self.mount_ns),
            (Layer::PidNs, self.pid_ns),
            (Layer::NetNs, self.net_ns),
            (Layer::IpcNs, self.ipc_ns),
            (Layer::UtsNs, self.uts_ns),
            (Layer::Landlock, self.landlock),
            (Layer::Seccomp, self.seccomp),
        ] {
            if r == Requirement::Required {
                v.push(l);
            }
        }
        v
    }

    fn namespaces(&self) -> [(Layer, Requirement, u64); 6] {
        [
            (Layer::UserNs, self.user_ns, libc::CLONE_NEWUSER as u64),
            (Layer::MountNs, self.mount_ns, libc::CLONE_NEWNS as u64),
            (Layer::PidNs, self.pid_ns, libc::CLONE_NEWPID as u64),
            (Layer::NetNs, self.net_ns, libc::CLONE_NEWNET as u64),
            (Layer::IpcNs, self.ipc_ns, libc::CLONE_NEWIPC as u64),
            (Layer::UtsNs, self.uts_ns, libc::CLONE_NEWUTS as u64),
        ]
    }
}

/// A program to run in the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    program: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
}

impl Command {
    /// The program at this absolute path.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// Adds an argument.
    pub fn arg(&mut self, a: impl Into<OsString>) -> &mut Self {
        self.args.push(a.into());
        self
    }

    /// Adds an environment variable (the child's environment starts empty).
    pub fn env(&mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> &mut Self {
        self.env.push((k.into(), v.into()));
        self
    }

    /// The program.
    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// Why a child could not be started.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The policy or the command cannot be used as given.
    #[error("sandbox: {0}")]
    BadPolicy(String),
    /// A required layer is not available on this system.
    #[error("sandbox: required layer {layer} unavailable: {errno}")]
    Unavailable {
        /// The layer.
        layer: &'static str,
        /// Why.
        errno: Errno,
    },
    /// A system call of the parent failed.
    #[error("sandbox: {what}: {errno}")]
    Sys {
        /// The call.
        what: &'static str,
        /// The error.
        errno: Errno,
    },
}

fn sys(what: &'static str) -> impl FnOnce(Errno) -> SpawnError {
    move |errno| SpawnError::Sys { what, errno }
}

fn cstr(what: &str, s: &OsStr) -> Result<CString, SpawnError> {
    CString::new(s.as_bytes()).map_err(|_| SpawnError::BadPolicy(format!("{what}: NUL byte")))
}

/// A path that is absolute and has no `.` or `..` components.
fn clean_abs(p: &Path) -> Result<(), SpawnError> {
    if !p.is_absolute()
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(SpawnError::BadPolicy(format!(
            "{}: not an absolute normal path",
            p.display()
        )));
    }
    if p.as_os_str().as_bytes().contains(&b':') {
        // the manifest separates paths with ':'
        return Err(SpawnError::BadPolicy(format!(
            "{}: ':' in a path",
            p.display()
        )));
    }
    Ok(())
}

/// One step of the mount set-up.
#[derive(Debug)]
enum Step {
    Mkdir(CString),
    Touch(CString),
    Bind { src: CString, dst: CString },
    RemountRo { dst: CString, flags: MountFlags },
    Proc(CString),
}

/// The locked flags of the mount holding `p` (a remount in a user namespace must keep them),
/// plus read-only.
fn ro_flags(p: &Path) -> Result<MountFlags, SpawnError> {
    let st = rustix::fs::statvfs(p).map_err(sys("statvfs"))?;
    let f = st.f_flag;
    let mut m = MountFlags::BIND | MountFlags::RDONLY;
    for (s, mf) in [
        (StatVfsMountFlags::NOSUID, MountFlags::NOSUID),
        (StatVfsMountFlags::NODEV, MountFlags::NODEV),
        (StatVfsMountFlags::NOEXEC, MountFlags::NOEXEC),
        (StatVfsMountFlags::NOATIME, MountFlags::NOATIME),
        (StatVfsMountFlags::NODIRATIME, MountFlags::NODIRATIME),
        (StatVfsMountFlags::RELATIME, MountFlags::RELATIME),
    ] {
        if f.contains(s) {
            m |= mf;
        }
    }
    Ok(m)
}

fn under_base(base: &Path, p: &Path) -> Result<CString, SpawnError> {
    let mut s = OsString::from(base.as_os_str());
    s.push(p.as_os_str());
    cstr("path", &s)
}

/// The mount steps that build the new root under `base`.
fn mount_steps(policy: &Policy, program: &Path, base: &Path) -> Result<Vec<Step>, SpawnError> {
    let mut steps = Vec::new();
    let mut made: Vec<PathBuf> = Vec::new();
    let mut mkdirs =
        |steps: &mut Vec<Step>, p: &Path, include_self: bool| -> Result<(), SpawnError> {
            let mut anc: Vec<&Path> = p.ancestors().skip(usize::from(!include_self)).collect();
            anc.reverse();
            for a in anc {
                if a == Path::new("/") || made.iter().any(|m| m == a) {
                    continue;
                }
                made.push(a.to_owned());
                steps.push(Step::Mkdir(under_base(base, a)?));
            }
            Ok(())
        };
    for p in &policy.ro_paths {
        clean_abs(p)?;
        let meta = std::fs::metadata(p)
            .map_err(|e| SpawnError::BadPolicy(format!("{}: {e}", p.display())))?;
        mkdirs(&mut steps, p, meta.is_dir())?;
        if !meta.is_dir() {
            steps.push(Step::Touch(under_base(base, p)?));
        }
        let dst = under_base(base, p)?;
        steps.push(Step::Bind {
            src: cstr("path", p.as_os_str())?,
            dst: dst.clone(),
        });
        steps.push(Step::RemountRo {
            dst,
            flags: ro_flags(p)?,
        });
    }
    for d in &policy.devices {
        clean_abs(d)?;
        mkdirs(&mut steps, d, false)?;
        steps.push(Step::Touch(under_base(base, d)?));
        steps.push(Step::Bind {
            src: cstr("path", d.as_os_str())?,
            dst: under_base(base, d)?,
        });
    }
    let exe = under_base(base, Path::new(EXE))?;
    steps.push(Step::Touch(exe.clone()));
    steps.push(Step::Bind {
        src: cstr("program", program.as_os_str())?,
        dst: exe.clone(),
    });
    steps.push(Step::RemountRo {
        dst: exe,
        flags: ro_flags(program)?,
    });
    if policy.proc {
        let p = under_base(base, Path::new("/proc"))?;
        steps.push(Step::Mkdir(p.clone()));
        steps.push(Step::Proc(p));
    }
    Ok(steps)
}

/// Everything the child uses between `clone3` and `execve`, prepared by the parent.
struct Plan {
    uid_map: Vec<u8>,
    gid_map: Vec<u8>,
    steps: Vec<Step>,
    base: CString,
    /// `<base>/.oldroot`, where `pivot_root` puts the old root.
    oldroot: CString,
    oldroot_abs: CString,
    rlimits: Vec<(Resource, Rlimit)>,
    /// `execve` path after `pivot_root`, and without it.
    exec_ns: CString,
    exec_plain: CString,
    /// Kept alive for the pointer arrays.
    _argv: Vec<CString>,
    _envp: Vec<CString>,
    argv_ptrs: Vec<*const c_char>,
    envp_ptrs: Vec<*const c_char>,
    /// Source descriptors of the child's 0..=5 (`None` for 2: inherit).
    fds: [Option<RawFd>; 6],
    report: RawFd,
    status: RawFd,
    mount_required: bool,
    uts: bool,
    empty_sigset: libc::sigset_t,
}

/// `struct clone_args` up to `tls` (`CLONE_ARGS_SIZE_VER0`).
#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
}

/// A sandboxed child process.
#[derive(Debug)]
pub struct Child {
    pid: i32,
    pidfd: OwnedFd,
    status: OwnedFd,
    report: Option<OwnedFd>,
    report_buf: Vec<u8>,
    records: usize,
    stderr: Option<OwnedFd>,
    confinement: Confinement,
    pid_ns: bool,
    exit: Option<Exit>,
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// `exit(code)`.
    Exited(i32),
    /// A signal.
    Signaled(i32),
}

/// The end of a child: the program's own status when known (with a PID namespace, its init
/// reports it), and the set-up failure if there was one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The program's status (or the init's, when the init was killed before it could say).
    pub status: ExitStatus,
    /// The child never ran the program: this stage failed with this error number.
    pub setup_failed: Option<(Stage, i32)>,
}

/// Starts `cmd` in the sandbox `policy` describes, with `ipc` as its descriptor 3.
pub fn spawn(cmd: &Command, ipc: OwnedFd, policy: &Policy) -> Result<Child, SpawnError> {
    clean_abs(&cmd.program)?;
    let mut confinement = Confinement::default();
    let use_ns = policy.user_ns != Requirement::Off;
    if !use_ns
        && let Some((l, _, _)) = policy
            .namespaces()
            .iter()
            .find(|(_, r, _)| *r == Requirement::Required)
    {
        return Err(SpawnError::BadPolicy(format!(
            "{} is required but the user namespace is off (unprivileged namespaces need it)",
            l.name()
        )));
    }

    // pipes: (read, write), both close-on-exec; the child moves its ends into place
    let (status_r, status_w) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(sys("pipe"))?;
    let (report_r, report_w) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(sys("pipe"))?;
    let (stderr_r, stderr_w) = match policy.stderr {
        Stderr::Pipe => {
            let (r, w) =
                rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(sys("pipe"))?;
            (Some(r), Some(w))
        }
        _ => (None, None),
    };
    let devnull = rustix::fs::open(c"/dev/null", OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
        .map_err(sys("open /dev/null"))?;

    let mut env: Vec<(OsString, OsString)> = cmd.env.clone();
    let req = |r: Requirement| match r {
        Requirement::Required => "required",
        Requirement::BestEffort => "best-effort",
        Requirement::Off => "off",
    };
    env.push(("TB_SANDBOX".into(), "1".into()));
    env.push(("TB_SANDBOX_LANDLOCK".into(), req(policy.landlock).into()));
    env.push(("TB_SANDBOX_SECCOMP".into(), req(policy.seccomp).into()));
    env.push((
        "TB_SANDBOX_PPID".into(),
        rustix::process::getpid()
            .as_raw_nonzero()
            .to_string()
            .into(),
    ));
    let join = |ps: &[PathBuf]| -> OsString {
        let mut s = OsString::new();
        for (i, p) in ps.iter().enumerate() {
            if i > 0 {
                s.push(":");
            }
            s.push(p.as_os_str());
        }
        s
    };

    let argv0 = cmd
        .program
        .file_name()
        .map(OsStr::to_owned)
        .unwrap_or_else(|| "program".into());
    let mut argv = vec![cstr("argv", &argv0)?];
    for a in &cmd.args {
        argv.push(cstr("argv", a)?);
    }

    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    let mut rl = vec![
        (
            Resource::Nofile,
            Rlimit {
                current: Some(policy.rlimits.nofile),
                maximum: Some(policy.rlimits.nofile),
            },
        ),
        (
            Resource::Core,
            Rlimit {
                current: Some(0),
                maximum: Some(0),
            },
        ),
    ];
    if let Some(a) = policy.rlimits.address_space {
        rl.push((
            Resource::As,
            Rlimit {
                current: Some(a),
                maximum: Some(a),
            },
        ));
    }
    if let Some(c) = policy.rlimits.cpu_seconds {
        rl.push((
            Resource::Cpu,
            Rlimit {
                current: Some(c),
                maximum: Some(c + 5),
            },
        ));
    }

    let mount = use_ns && policy.mount_ns != Requirement::Off;
    let base = {
        let mut sources: Vec<&Path> = policy.ro_paths.iter().map(PathBuf::as_path).collect();
        sources.extend(policy.devices.iter().map(PathBuf::as_path));
        sources.push(&cmd.program);
        pick_base(&sources)
    };
    let mount = mount && base.is_some();
    let base = base.unwrap_or_default();
    let mut steps = if mount {
        mount_steps(policy, &cmd.program, &base)?
    } else {
        Vec::new()
    };
    // in the plain case the program reads its own path; in the namespace it is /exe
    let mut ro_plain = policy.ro_paths.clone();
    ro_plain.push(cmd.program.clone());
    let mut ro_ns = policy.ro_paths.clone();
    ro_ns.push(PathBuf::from(EXE));
    if policy.proc {
        // Landlock read rights on the /proc that is mounted for the program
        ro_ns.push(PathBuf::from("/proc"));
    }

    let mut flags = 0u64;
    for (_, r, f) in policy.namespaces() {
        if use_ns && r != Requirement::Off {
            flags |= f;
        }
    }

    // two attempts: with the namespaces, then (if none is required) without
    let attempts: &[u64] = if flags != 0 { &[flags, 0] } else { &[0] };
    let mut last_err = Errno::INVAL;
    for (i, &f) in attempts.iter().enumerate() {
        if i > 0 {
            let required: Vec<_> = policy
                .namespaces()
                .iter()
                .filter(|(_, r, _)| *r == Requirement::Required)
                .map(|(l, _, _)| l.name())
                .collect();
            if let Some(layer) = required.first() {
                return Err(SpawnError::Unavailable {
                    layer,
                    errno: last_err,
                });
            }
            tracing::warn!(error = %last_err, "sandbox: no user namespace; namespaces are OFF for this child");
        }
        let in_ns = f != 0;
        let mut env = env.clone();
        env.push((
            "TB_SANDBOX_PIDNS".into(),
            if in_ns && f & libc::CLONE_NEWPID as u64 != 0 {
                "1"
            } else {
                "0"
            }
            .into(),
        ));
        let pivot = in_ns && f & libc::CLONE_NEWNS as u64 != 0 && !steps.is_empty();
        env.push((
            "TB_SANDBOX_RO".into(),
            join(if pivot { &ro_ns } else { &ro_plain }),
        ));
        env.push(("TB_SANDBOX_DEV".into(), join(&policy.devices)));
        env.push(("TB_SANDBOX_RW".into(), join(&policy.landlock_rw)));
        let envp: Vec<CString> = env
            .iter()
            .map(|(k, v)| {
                let mut s = k.clone();
                s.push("=");
                s.push(v);
                cstr("environment", &s)
            })
            .collect::<Result<_, _>>()?;
        let mut argv_ptrs: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());
        let mut envp_ptrs: Vec<*const c_char> = envp.iter().map(|a| a.as_ptr()).collect();
        envp_ptrs.push(std::ptr::null());
        // SAFETY: an all-zero sigset_t is a valid value to pass to sigemptyset, which sets it.
        let mut empty_sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `empty_sigset` is a valid, writable sigset_t.
        unsafe { libc::sigemptyset(&raw mut empty_sigset) };
        let plan = Plan {
            uid_map: format!("{uid} {uid} 1\n").into_bytes(),
            gid_map: format!("{gid} {gid} 1\n").into_bytes(),
            steps: if pivot {
                std::mem::take(&mut steps)
            } else {
                Vec::new()
            },
            base: cstr("base", base.as_os_str())?,
            oldroot: cstr("base", base.join(".oldroot").as_os_str())?,
            oldroot_abs: cstr("base", OsStr::new("/.oldroot"))?,
            rlimits: rl.clone(),
            exec_ns: cstr("exe", OsStr::new(EXE))?,
            exec_plain: cstr("program", cmd.program.as_os_str())?,
            _argv: argv.clone(),
            _envp: envp,
            argv_ptrs,
            envp_ptrs,
            fds: [
                Some(devnull.as_raw_fd()),
                Some(devnull.as_raw_fd()),
                match policy.stderr {
                    Stderr::Null => Some(devnull.as_raw_fd()),
                    Stderr::Inherit => None,
                    Stderr::Pipe => stderr_w.as_ref().map(AsRawFd::as_raw_fd),
                },
                Some(ipc.as_raw_fd()),
                Some(status_w.as_raw_fd()),
                Some(report_w.as_raw_fd()),
            ],
            report: report_w.as_raw_fd(),
            status: status_w.as_raw_fd(),
            mount_required: policy.mount_ns == Requirement::Required,
            uts: in_ns && f & libc::CLONE_NEWUTS as u64 != 0,
            empty_sigset,
        };
        let pivot_planned = !plan.steps.is_empty();
        let mut pidfd: c_int = -1;
        let args = CloneArgs {
            flags: f | libc::CLONE_PIDFD as u64,
            pidfd: (&raw mut pidfd) as u64,
            exit_signal: libc::SIGCHLD as u64,
            ..CloneArgs::default()
        };
        // SAFETY: clone3 with a valid clone_args of the size given, without CLONE_VM: the
        // child gets a copy of the address space (as with fork) and runs `child_main`, which
        // only makes system calls on `plan` and never returns.
        let r = unsafe {
            libc::syscall(
                libc::SYS_clone3,
                &raw const args,
                std::mem::size_of::<CloneArgs>(),
            )
        };
        if r == 0 {
            child_main(&plan, in_ns);
        }
        if r < 0 {
            last_err = Errno::from_raw_os_error(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            );
            if f == 0 {
                return Err(SpawnError::Sys {
                    what: "clone3",
                    errno: last_err,
                });
            }
            continue;
        }
        drop(plan);
        // SAFETY: clone3 succeeded with CLONE_PIDFD, so `pidfd` is a new descriptor this
        // process owns.
        let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
        for (l, req, bit) in policy.namespaces() {
            let st = if req == Requirement::Off || !use_ns {
                LayerReport::new(l, LayerState::Off, 0, 0)
            } else if f & bit != 0 {
                LayerReport::applied(l, 0)
            } else {
                LayerReport::new(l, LayerState::Unavailable, last_err.raw_os_error(), 0)
            };
            // the mount namespace's minimal root is reported by the child, unless there was
            // no directory to build it on
            if l == Layer::MountNs && st.state == LayerState::Applied {
                if !pivot_planned {
                    confinement.set(LayerReport::new(
                        l,
                        LayerState::Unavailable,
                        libc::ENOENT,
                        0,
                    ));
                }
            } else {
                confinement.set(st);
            }
        }
        rustix::io::ioctl_fionbio(&report_r, true).map_err(sys("fionbio"))?;
        rustix::io::ioctl_fionbio(&status_r, true).map_err(sys("fionbio"))?;
        if let Some(e) = &stderr_r {
            rustix::io::ioctl_fionbio(e, true).map_err(sys("fionbio"))?;
        }
        return Ok(Child {
            pid: r as i32,
            pidfd,
            status: status_r,
            report: Some(report_r),
            report_buf: Vec::new(),
            records: 0,
            stderr: stderr_r,
            confinement,
            pid_ns: in_ns && f & libc::CLONE_NEWPID as u64 != 0,
            exit: None,
        });
    }
    Err(SpawnError::Sys {
        what: "clone3",
        errno: last_err,
    })
}

// ---------------------------------------------------------------------------------------------
// The child, between clone3 and execve. No allocation, no panics, system calls only.

fn report(fd: RawFd, r: LayerReport) {
    // SAFETY: `fd` is a descriptor of this process (the report pipe) for the duration.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let _ = rustix::io::write(fd, &r.encode());
}

fn fail(status: RawFd, stage: Stage, e: Errno) -> ! {
    // SAFETY: as in `report`.
    let fd = unsafe { BorrowedFd::borrow_raw(status) };
    let _ = rustix::io::write(
        fd,
        &Status::SetupFailed {
            stage,
            errno: e.raw_os_error(),
        }
        .encode(),
    );
    // SAFETY: ends the child at once, without running anything of the parent's copy.
    unsafe { libc::_exit(EXIT_SETUP) }
}

fn write_file(path: &std::ffi::CStr, data: &[u8]) -> Result<(), Errno> {
    let fd = rustix::fs::open(path, OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty())?;
    let n = rustix::io::write(&fd, data)?;
    if n != data.len() {
        return Err(Errno::IO);
    }
    Ok(())
}

/// The mount set-up. `Err((pivoted, step, errno))`.
fn mounts(p: &Plan) -> Result<(), (bool, u32, Errno)> {
    let at = |i: u32| move |e: Errno| (false, i, e);
    rustix::mount::mount_change(
        c"/",
        MountPropagationFlags::REC | MountPropagationFlags::PRIVATE,
    )
    .map_err(at(0))?;
    rustix::mount::mount(
        c"tmpfs",
        p.base.as_c_str(),
        c"tmpfs",
        MountFlags::NOSUID | MountFlags::NODEV,
        c"mode=0755,size=64k",
    )
    .map_err(at(1))?;
    let mut i = 2u32;
    for s in &p.steps {
        let r = match s {
            Step::Mkdir(d) => match rustix::fs::mkdir(d.as_c_str(), Mode::from_raw_mode(0o755)) {
                Err(Errno::EXIST) => Ok(()),
                r => r,
            },
            Step::Touch(f) => rustix::fs::open(
                f.as_c_str(),
                OFlags::CREATE | OFlags::WRONLY | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o644),
            )
            .map(drop),
            Step::Bind { src, dst } => {
                rustix::mount::mount_bind_recursive(src.as_c_str(), dst.as_c_str())
            }
            Step::RemountRo { dst, flags } => {
                rustix::mount::mount_remount(dst.as_c_str(), *flags, c"")
            }
            Step::Proc(d) => rustix::mount::mount(
                c"proc",
                d.as_c_str(),
                c"proc",
                MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
                c"",
            ),
        };
        r.map_err(at(i))?;
        i += 1;
    }
    rustix::fs::mkdir(p.oldroot.as_c_str(), Mode::from_raw_mode(0o700)).map_err(at(i))?;
    rustix::process::pivot_root(p.base.as_c_str(), p.oldroot.as_c_str()).map_err(at(i + 1))?;
    // from here on the root has changed: later failures leave a usable (if less tidy) root
    let after = |j: u32| move |e: Errno| (true, j, e);
    rustix::process::chdir(c"/").map_err(after(i + 2))?;
    rustix::mount::unmount(p.oldroot_abs.as_c_str(), UnmountFlags::DETACH).map_err(after(i + 3))?;
    rustix::fs::rmdir(p.oldroot_abs.as_c_str()).map_err(after(i + 4))?;
    rustix::mount::mount_remount(
        c"/",
        MountFlags::BIND | MountFlags::RDONLY | MountFlags::NOSUID | MountFlags::NODEV,
        c"",
    )
    .map_err(after(i + 5))?;
    Ok(())
}

fn child_main(p: &Plan, in_ns: bool) -> ! {
    let rep = p.report;
    let st = p.status;
    let mut pivoted = false;
    if in_ns {
        // our own uid and gid, mapped to themselves
        if let Err(e) = write_file(c"/proc/self/setgroups", b"deny")
            .and_then(|()| write_file(c"/proc/self/uid_map", &p.uid_map))
            .and_then(|()| write_file(c"/proc/self/gid_map", &p.gid_map))
        {
            report(
                rep,
                LayerReport::new(Layer::UserNs, LayerState::Failed, e.raw_os_error(), 0),
            );
            fail(st, Stage::IdMaps, e);
        }
        if p.steps.is_empty() {
            // no mount namespace asked for (or none made)
        } else {
            match mounts(p) {
                Ok(()) => {
                    pivoted = true;
                    report(rep, LayerReport::applied(Layer::MountNs, 0));
                }
                Err((piv, step, e)) => {
                    pivoted = piv;
                    let state = if piv {
                        LayerState::Partial
                    } else {
                        LayerState::Failed
                    };
                    report(
                        rep,
                        LayerReport::new(Layer::MountNs, state, e.raw_os_error(), step),
                    );
                    if p.mount_required && !piv {
                        fail(st, Stage::Mounts, e);
                    }
                }
            }
        }
        if p.uts
            && let Err(e) = rustix::system::sethostname(b"tb-sandbox")
        {
            report(
                rep,
                LayerReport::new(Layer::UtsNs, LayerState::Partial, e.raw_os_error(), 0),
            );
        }
    }
    for (res, lim) in &p.rlimits {
        if let Err(e) = rustix::process::setrlimit(*res, *lim) {
            report(
                rep,
                LayerReport::new(Layer::Rlimits, LayerState::Failed, e.raw_os_error(), 0),
            );
            fail(st, Stage::Rlimits, e);
        }
    }
    report(rep, LayerReport::applied(Layer::Rlimits, 0));
    if let Err(e) = rustix::thread::set_no_new_privs(true) {
        report(
            rep,
            LayerReport::new(Layer::NoNewPrivs, LayerState::Failed, e.raw_os_error(), 0),
        );
        fail(st, Stage::NoNewPrivs, e);
    }
    report(rep, LayerReport::applied(Layer::NoNewPrivs, 0));

    // descriptors: first copies above 10 (sources may sit on 0..=5), then into place
    let mut high = [-1 as RawFd; 6];
    for (i, src) in p.fds.iter().enumerate() {
        let src = match src {
            Some(s) => *s,
            None => i as RawFd,
        };
        // SAFETY: F_DUPFD_CLOEXEC on a descriptor of this process; no memory involved.
        let h = unsafe { libc::fcntl(src, libc::F_DUPFD_CLOEXEC, 10) };
        if h < 0 {
            fail(st, Stage::Fds, Errno::BADF);
        }
        if let Some(slot) = high.get_mut(i) {
            *slot = h;
        }
    }
    for (target, h) in high.iter().enumerate() {
        // SAFETY: dup2 between descriptors of this process; the target is cleared of
        // close-on-exec, as it must survive execve.
        if unsafe { libc::dup2(*h, target as c_int) } < 0 {
            fail(st, Stage::Fds, Errno::BADF);
        }
    }
    // SAFETY: closes every descriptor above the six the program gets (the copies included).
    if unsafe { libc::close_range(6, u32::MAX, 0) } < 0 {
        report(
            FD_REPORT,
            LayerReport::new(Layer::CloseFds, LayerState::Failed, 0, 0),
        );
        fail(FD_STATUS, Stage::Fds, Errno::INVAL);
    }
    report(FD_REPORT, LayerReport::applied(Layer::CloseFds, 0));
    // SAFETY: plain signal-state calls on a valid empty set; no handlers are installed.
    unsafe {
        libc::sigprocmask(
            libc::SIG_SETMASK,
            &raw const p.empty_sigset,
            std::ptr::null_mut(),
        );
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let path = if pivoted { &p.exec_ns } else { &p.exec_plain };
    // SAFETY: path, argv and envp are NUL-terminated strings and null-terminated arrays the
    // parent built and that live in this copy of its memory until execve replaces it.
    unsafe { libc::execve(path.as_ptr(), p.argv_ptrs.as_ptr(), p.envp_ptrs.as_ptr()) };
    let e = Errno::from_raw_os_error(
        // SAFETY: reads this thread's errno.
        unsafe { *libc::__errno_location() },
    );
    fail(FD_STATUS, Stage::Exec, e)
}

// ---------------------------------------------------------------------------------------------
// The parent's handle

impl Child {
    /// The child's pid (in the parent's namespace).
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// The pidfd: readable once the child has exited.
    pub fn pidfd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    /// The read end of the child's stderr, with [`Stderr::Pipe`] (non-blocking).
    pub fn stderr(&self) -> Option<BorrowedFd<'_>> {
        self.stderr.as_ref().map(AsFd::as_fd)
    }

    /// Whether the program runs in a PID namespace (under an init of its own).
    pub fn pid_ns(&self) -> bool {
        self.pid_ns
    }

    /// The confinement reported so far.
    pub fn confinement(&self) -> &Confinement {
        &self.confinement
    }

    /// Sends `SIGKILL` (through the pidfd: never another process that reused the pid). With a
    /// PID namespace this is its init, and the kernel kills everything in it.
    pub fn kill(&self) -> Result<(), Errno> {
        match rustix::process::pidfd_send_signal(&self.pidfd, rustix::process::Signal::KILL) {
            Err(Errno::SRCH) => Ok(()),
            r => r,
        }
    }

    /// Reads the report records available now. Returns true once the pipe is closed (the
    /// report is complete).
    pub fn read_reports(&mut self) -> bool {
        let Some(fd) = &self.report else {
            return true;
        };
        let mut buf = [0u8; RECORD * 8];
        loop {
            match rustix::io::read(fd, &mut buf) {
                Ok(0) => {
                    self.report = None;
                    return true;
                }
                Ok(n) => {
                    self.report_buf
                        .extend_from_slice(buf.get(..n).unwrap_or_default());
                    while self.report_buf.len() >= RECORD {
                        let mut r = [0u8; RECORD];
                        r.copy_from_slice(self.report_buf.get(..RECORD).unwrap_or(&[0; RECORD]));
                        self.report_buf.drain(..RECORD);
                        self.records += 1;
                        match parse_layer(&r) {
                            Ok(l) if self.records <= MAX_RECORDS => self.confinement.set(l),
                            _ => {
                                tracing::warn!(
                                    "sandbox: malformed or excess report record ignored"
                                );
                            }
                        }
                    }
                }
                Err(Errno::INTR) => {}
                Err(_) => return false,
            }
        }
    }

    /// Waits until the report is complete (the program closed the pipe after
    /// [`crate::child::Entered::lock_down`]) or `timeout` passed. Returns whether it is.
    pub fn finish_reports(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if self.read_reports() {
                return true;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let Some(fd) = &self.report else { return true };
            let mut pfd = [rustix::event::PollFd::new(fd, rustix::event::PollFlags::IN)];
            let ts = rustix::time::Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: i64::from(left.subsec_nanos()),
            };
            let _ = rustix::event::poll(&mut pfd, Some(&ts));
        }
    }

    /// The exit, if the child has exited (reaps it).
    pub fn try_wait(&mut self) -> Result<Option<Exit>, Errno> {
        self.wait_with(WaitIdOptions::EXITED | WaitIdOptions::NOHANG)
    }

    /// Waits for the exit.
    pub fn wait(&mut self) -> Result<Exit, Errno> {
        loop {
            match self.wait_with(WaitIdOptions::EXITED) {
                Ok(Some(e)) => return Ok(e),
                Ok(None) | Err(Errno::INTR) => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn wait_with(&mut self, opts: WaitIdOptions) -> Result<Option<Exit>, Errno> {
        if let Some(e) = self.exit {
            return Ok(Some(e));
        }
        let Some(st) =
            rustix::process::waitid(rustix::process::WaitId::PidFd(self.pidfd.as_fd()), opts)?
        else {
            return Ok(None);
        };
        let own = if let Some(s) = st.terminating_signal() {
            ExitStatus::Signaled(s)
        } else {
            ExitStatus::Exited(st.exit_status().unwrap_or(0))
        };
        self.read_reports();
        // the status pipe: a set-up failure, or the program's end as its init saw it
        let mut exit = Exit {
            status: own,
            setup_failed: None,
        };
        let mut buf = [0u8; RECORD * 4];
        let mut got = Vec::new();
        while let Ok(n) = rustix::io::read(&self.status, &mut buf) {
            if n == 0 || got.len() > RECORD * 4 {
                break;
            }
            got.extend_from_slice(buf.get(..n).unwrap_or_default());
        }
        for r in got.as_chunks::<RECORD>().0 {
            match parse_status(r) {
                Ok(Status::SetupFailed { stage, errno }) => {
                    exit.setup_failed = Some((stage, errno))
                }
                Ok(Status::Exited(c)) if self.pid_ns => exit.status = ExitStatus::Exited(c),
                Ok(Status::Signaled(s)) if self.pid_ns => exit.status = ExitStatus::Signaled(s),
                _ => tracing::warn!("sandbox: unexpected status record ignored"),
            }
        }
        self.exit = Some(exit);
        Ok(Some(exit))
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.exit.is_none() {
            let _ = self.kill();
            let _ = self.wait();
        }
    }
}
