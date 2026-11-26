//! Unprivileged confinement of a child process on Linux, and the handle to supervise it.
//!
//! The decoder worker runs libav, which the owner does not trust; this crate is what keeps a
//! compromised or crashing libav from reaching anything but the two descriptors it is given.
//! It is generic (nothing here knows about decoding): [`spawn()`] starts a program confined as
//! a [`Policy`] says, and the program calls [`child::enter`] first thing and
//! [`child::Entered::lock_down`] once it has initialized its libraries.
//!
//! | Layer | Where | What it stops |
//! |---|---|---|
//! | user namespace | `clone3` | every other namespace without privileges; no capabilities after `execve` |
//! | mount namespace, minimal root | child, before `execve` | the file system: the root is an empty read-only tmpfs with `/nix/store` (or a closure) read-only, the program at `/exe`, device nodes if asked |
//! | PID namespace | `clone3`, init in [`child::enter`] | seeing or signaling any process outside; the namespace dies with its init |
//! | network namespace | `clone3` | the network: no interface but a loopback that is down |
//! | IPC, UTS namespaces | `clone3` | System V IPC, POSIX queues; the host name |
//! | `PR_SET_NO_NEW_PRIVS` | child, before `execve` | gaining privileges by `execve`; required by seccomp and Landlock |
//! | descriptors | child, before `execve` | everything the parent had open: only 0-2 (`/dev/null`, a stderr pipe), 3 (IPC), 4 (status), 5 (report) |
//! | rlimits | child, before `execve` | descriptors beyond `NOFILE`, core dumps, memory beyond `AS`, CPU beyond `CPU` |
//! | death signal, not dumpable | [`child::enter`] | outliving the parent; `ptrace` and `/proc/<pid>` by our uid |
//! | Landlock | [`child::enter`] | file access beyond read/execute of the listed paths; TCP; abstract sockets and signals outside (ABI 6) |
//! | seccomp-bpf | [`child::Entered::lock_down`] | every system call outside an allow-list ([`seccomp`]) |
//!
//! Every layer reports whether it took effect ([`report::Confinement`]): the namespaces from
//! `clone3`'s result, the rest from the child over a pipe, as each is put in place, before the
//! program has read anything untrusted. The parent logs it and refuses a child whose required
//! layers are missing. Each layer is required, best effort or off ([`report::Requirement`]);
//! without user namespaces (some containers, some distributions' defaults) the namespace
//! layers are reported unavailable and the rest still applies.
//!
//! Supervision primitives: the child's pidfd ([`Child::pidfd`]: readable when it exits, and
//! signals through it never hit a reused pid), [`Child::kill`], [`Child::try_wait`] with the
//! program's own status as its PID namespace's init reports it.
//!
//! `unsafe` here: `clone3`, `execve`, `fork`, `ptrace` (probe), the descriptor shuffling
//! before `execve` and adopting the descriptors after it. Each block has its `SAFETY`
//! comment; the child between `clone3` and `execve` makes system calls only, on data the
//! parent prepared.
#![allow(
    unsafe_code,
    reason = "process creation (clone3, execve, fork) and descriptor set-up that rustix does not wrap"
)]

pub mod child;
pub mod ipc;
pub mod probe;
pub mod report;
pub mod seccomp;
pub mod shm;
mod spawn;

pub use report::{Confinement, Layer, LayerReport, LayerState, Requirement};
pub use spawn::{
    Child, Command, EXE, EXIT_SETUP, Exit, ExitStatus, FD_IPC, FD_REPORT, FD_STATUS, Policy,
    Rlimits, SpawnError, Stderr, spawn,
};
