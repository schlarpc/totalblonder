//! The system-call filters (seccomp-bpf, built with `seccompiler`).
//!
//! # The worker's filter: an allow-list, three stacked programs
//!
//! 1. **Allow-list** (the decisive one): the calls a process that only computes on memory it
//!    has and talks over descriptors it has needs: memory management, threads (`clone` only
//!    with `CLONE_THREAD`), futexes, signals of its own, time, `read`/`write`/`sendmsg`/
//!    `recvmsg` on its descriptors, `close`, `fstat`, CPU affinity queries, `getrandom`,
//!    `prctl` with `PR_SET_NAME`, `PR_GET_NAME` or `PR_GET_AUXV` only (rustix reads the
//!    auxiliary vector lazily, for the vDSO). With [`Action::Kill`] anything else kills the
//!    process (`SECCOMP_RET_KILL_PROCESS`, seen by the parent as `SIGSYS`); with
//!    [`Action::Errno`] it fails with `EPERM` (tests use this to observe each denial).
//! 2. `open`, `openat`, `openat2`, `creat` fail with `EPERM` in both modes. They are on the
//!    allow-list of program 1 so that this program's answer is the one that counts: the C
//!    library opens files in a few rare paths of its own (`/proc/sys/vm/overcommit_memory`
//!    when a malloc arena shrinks, `/sys/devices/system/cpu/online`) and handles a failure
//!    there; a kill would turn those into crashes. There is nothing to open in the root anyway.
//! 3. `clone3` fails with `ENOSYS`, so that the C library falls back to `clone`, whose flags a
//!    filter can see (those of `clone3` are behind a pointer).
//!
//! The kernel runs every program on every call and the strictest answer wins (kill, then
//! errno, then allow).
//!
//! Decision, kill rather than `EPERM` in production: a decoder that makes a call outside the
//! list is either compromised or on a code path nobody has seen; in both cases a crash is
//! the safe outcome, the supervisor classifies it (`SeccompViolation`), restarts with backoff,
//! and the counter makes it visible. `EPERM` would let a compromised process probe at
//! leisure.
//!
//! GPU decode ([`Profile::ComputeGpu`]) is wider by necessity, as found on the card: `ioctl`
//! (the driver's interface), and opening files (`open*`, `access`, `stat`, `readlink`):
//! the driver re-probes `/sys/module/nvidia`, `/proc/driver/nvidia` and opens device nodes
//! when a decoder is created, after the filter is in place. Landlock is what limits those
//! opens (device nodes, the read-only paths, the worker's own `/proc/self/task`). Still
//! refused: `execve`, `ptrace`, processes, `kill`, `connect` (kill); `socket`, `chmod`,
//! `chown`, `mknod` families (EPERM: the driver tries them and copes).

use std::collections::BTreeMap;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

/// `PR_GET_AUXV` (Linux 6.4): the process's own auxiliary vector, read-only.
const PR_GET_AUXV: u64 = 0x4155_5856;

/// What a call outside the allow-list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Fails with `EPERM`.
    Errno,
    /// Kills the process (`SIGSYS`).
    Kill,
}

impl Action {
    /// The value reported as the seccomp layer's detail.
    pub fn code(self) -> u32 {
        match self {
            Self::Errno => 1,
            Self::Kill => 2,
        }
    }
}

/// Which allow-list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// A process that computes and talks over the descriptors it has (software decode).
    Compute,
    /// [`Profile::Compute`] plus `ioctl` (GPU drivers).
    ComputeGpu,
    /// The PID namespace's init: waits and writes one record.
    Init,
}

/// A filter could not be built or installed.
#[derive(Debug, thiserror::Error)]
#[error("seccomp: {0}")]
pub struct SeccompError(pub String);

fn arch() -> Result<TargetArch, SeccompError> {
    TargetArch::try_from(std::env::consts::ARCH).map_err(|e| SeccompError(format!("{e:?}")))
}

fn compute_calls() -> Vec<i64> {
    let mut v = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_lseek,
        libc::SYS_fcntl,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mprotect,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_futex,
        libc::SYS_set_robust_list,
        libc::SYS_get_robust_list,
        libc::SYS_rseq,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getppid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_tgkill,
        libc::SYS_restart_syscall,
        libc::SYS_clock_gettime,
        libc::SYS_clock_getres,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_gettimeofday,
        libc::SYS_getrandom,
        libc::SYS_recvmsg,
        libc::SYS_sendmsg,
        libc::SYS_recvfrom,
        libc::SYS_sendto,
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_getrusage,
        libc::SYS_membarrier,
        // decided by the programs stacked on this one
        libc::SYS_open,
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_creat,
        libc::SYS_clone3,
    ];
    v.sort_unstable();
    v
}

fn rule(conds: Vec<SeccompCondition>) -> Result<SeccompRule, SeccompError> {
    SeccompRule::new(conds).map_err(|e| SeccompError(format!("{e:?}")))
}

fn cond(arg: u8, op: SeccompCmpOp, v: u64) -> Result<SeccompCondition, SeccompError> {
    SeccompCondition::new(arg, SeccompCmpArgLen::Qword, op, v)
        .map_err(|e| SeccompError(format!("{e:?}")))
}

fn compile(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
    mismatch: SeccompAction,
    matched: SeccompAction,
) -> Result<BpfProgram, SeccompError> {
    let f = SeccompFilter::new(rules, mismatch, matched, arch()?)
        .map_err(|e| SeccompError(format!("{e:?}")))?;
    f.try_into()
        .map_err(|e: seccompiler::BackendError| SeccompError(format!("{e:?}")))
}

/// The programs of a profile, in the order they are installed.
pub fn build(profile: Profile, action: Action) -> Result<Vec<BpfProgram>, SeccompError> {
    let deny = match action {
        Action::Errno => SeccompAction::Errno(libc::EPERM as u32),
        Action::Kill => SeccompAction::KillProcess,
    };
    if profile == Profile::Init {
        let mut rules = BTreeMap::new();
        for n in [
            libc::SYS_wait4,
            libc::SYS_waitid,
            libc::SYS_write,
            libc::SYS_close,
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_rt_sigreturn,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigaction,
            libc::SYS_restart_syscall,
            libc::SYS_munmap,
            libc::SYS_brk,
            libc::SYS_mmap,
            libc::SYS_futex,
        ] {
            rules.insert(n, vec![]);
        }
        rules.insert(
            libc::SYS_prctl,
            vec![rule(vec![cond(0, SeccompCmpOp::Eq, PR_GET_AUXV)?])?],
        );
        return Ok(vec![compile(rules, deny, SeccompAction::Allow)?]);
    }

    let mut allow: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for n in compute_calls() {
        allow.insert(n, vec![]);
    }
    // threads only: clone(2) with CLONE_THREAD (a fork would be a new process)
    let thread = libc::CLONE_THREAD as u64;
    allow.insert(
        libc::SYS_clone,
        vec![rule(vec![cond(
            0,
            SeccompCmpOp::MaskedEq(thread),
            thread,
        )?])?],
    );
    // thread names, and the auxiliary vector (rustix reads it lazily, for the vDSO)
    allow.insert(
        libc::SYS_prctl,
        vec![
            rule(vec![cond(0, SeccompCmpOp::Eq, libc::PR_SET_NAME as u64)?])?,
            rule(vec![cond(0, SeccompCmpOp::Eq, libc::PR_GET_NAME as u64)?])?,
            rule(vec![cond(0, SeccompCmpOp::Eq, PR_GET_AUXV)?])?,
        ],
    );
    if profile == Profile::ComputeGpu {
        // the driver: its ioctls, and re-probing and opening its device and /proc and /sys
        // files when a decoder is created (Landlock decides which files can be opened)
        for n in [
            libc::SYS_ioctl,
            libc::SYS_sched_setaffinity,
            libc::SYS_sched_get_priority_max,
            libc::SYS_sched_get_priority_min,
            libc::SYS_sched_getparam,
            libc::SYS_sched_getscheduler,
            libc::SYS_getpriority,
            libc::SYS_uname,
            libc::SYS_access,
            libc::SYS_faccessat,
            libc::SYS_faccessat2,
            libc::SYS_readlink,
            libc::SYS_readlinkat,
            libc::SYS_stat,
            libc::SYS_lstat,
            libc::SYS_statfs,
            libc::SYS_fstatfs,
            libc::SYS_getdents64,
            libc::SYS_eventfd2,
            libc::SYS_pipe2,
            libc::SYS_dup,
            libc::SYS_dup3,
        ] {
            allow.insert(n, vec![]);
        }
    }
    let mut opens = BTreeMap::new();
    // (with the GPU, opening is allowed: the driver opens its files when a decoder is made)
    // (the driver also tries to fix device permissions and to reach the MPS control socket:
    // those fail with EPERM, as they would outside for a user, instead of killing)
    let refused: &[i64] = if profile == Profile::ComputeGpu {
        &[
            libc::SYS_creat,
            libc::SYS_chmod,
            libc::SYS_fchmod,
            libc::SYS_fchmodat,
            libc::SYS_chown,
            libc::SYS_lchown,
            libc::SYS_fchown,
            libc::SYS_fchownat,
            libc::SYS_mknod,
            libc::SYS_mknodat,
            libc::SYS_socket,
            libc::SYS_setpriority,
        ]
    } else {
        &[
            libc::SYS_open,
            libc::SYS_openat,
            libc::SYS_openat2,
            libc::SYS_creat,
        ]
    };
    for n in refused {
        opens.insert(*n, vec![]);
        // on the allow-list too, so that this program's EPERM is the answer
        allow.entry(*n).or_default();
    }
    let mut clone3 = BTreeMap::new();
    clone3.insert(libc::SYS_clone3, vec![]);
    // the allow-list last: it does not allow seccomp(2), so nothing can be installed after it
    Ok(vec![
        compile(
            opens,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
        )?,
        compile(
            clone3,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::ENOSYS as u32),
        )?,
        compile(allow, deny, SeccompAction::Allow)?,
    ])
}

/// Installs the programs on every thread of the process (`SECCOMP_FILTER_FLAG_TSYNC`).
/// `PR_SET_NO_NEW_PRIVS` must be set (the sandbox sets it before `execve`).
pub fn install(programs: &[BpfProgram]) -> Result<(), SeccompError> {
    for p in programs {
        seccompiler::apply_filter_all_threads(p).map_err(|e| SeccompError(format!("{e:?}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_compile() {
        for p in [Profile::Compute, Profile::ComputeGpu, Profile::Init] {
            for a in [Action::Errno, Action::Kill] {
                let progs = build(p, a).unwrap();
                assert!(!progs.is_empty());
                assert!(progs.iter().all(|p| !p.is_empty() && p.len() < 4096));
            }
        }
    }
}
