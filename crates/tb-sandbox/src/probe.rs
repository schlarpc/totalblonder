//! Attempts at what a sandboxed program must not be able to do, for tests: each returns 0 if
//! the operation succeeded (the sandbox failed to stop it) or the error number it failed with.

use rustix::fs::{Mode, OFlags};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};

/// One attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    /// `open("/etc/passwd", O_RDONLY)`.
    ReadPasswd,
    /// Create a file at `/`, in `/tmp`, in `/nix/store`.
    WriteFile,
    /// `socket(AF_INET, SOCK_DGRAM)`.
    Socket,
    /// `connect` to an abstract UNIX socket (named by `TB_PROBE_ABSTRACT`; tests listen on it
    /// outside the sandbox).
    ConnectUnix,
    /// `execve("/bin/sh")`. On success the process is replaced: only try it where it must fail.
    Exec,
    /// `ptrace(PTRACE_ATTACH)` of the parent (the namespace's init, or the server).
    Ptrace,
    /// `kill(server, 0)`: whether the server's process is visible and signalable.
    SignalOutside,
    /// A UDP datagram to 192.0.2.1:9 (TEST-NET-1): whether there is a route out.
    Network,
}

impl Op {
    /// Every operation but [`Op::Exec`].
    pub const SAFE: [Op; 7] = [
        Op::ReadPasswd,
        Op::WriteFile,
        Op::Socket,
        Op::ConnectUnix,
        Op::Ptrace,
        Op::SignalOutside,
        Op::Network,
    ];
}

fn errno(e: rustix::io::Errno) -> i32 {
    e.raw_os_error()
}

/// Tries `op`.
pub fn attempt(op: Op) -> i32 {
    match op {
        Op::ReadPasswd => {
            match rustix::fs::open(
                c"/etc/passwd",
                OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(_) => 0,
                Err(e) => errno(e),
            }
        }
        Op::WriteFile => {
            let mut last = 0;
            for p in [
                c"/tb-sandbox-probe",
                c"/tmp/tb-sandbox-probe",
                c"/nix/store/tb-sandbox-probe",
            ] {
                match rustix::fs::open(
                    p,
                    OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                ) {
                    Ok(_) => {
                        let _ = rustix::fs::unlink(p);
                        return 0;
                    }
                    Err(e) => last = errno(e),
                }
            }
            last
        }
        Op::Socket => match rustix::net::socket(AddressFamily::INET, SocketType::DGRAM, None) {
            Ok(_) => 0,
            Err(e) => errno(e),
        },
        Op::ConnectUnix => {
            let s = match rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None) {
                Ok(s) => s,
                Err(e) => return errno(e),
            };
            let name = std::env::var("TB_PROBE_ABSTRACT")
                .unwrap_or_else(|_| "tb-sandbox-probe".to_owned());
            let a = match SocketAddrUnix::new_abstract_name(name.as_bytes()) {
                Ok(a) => a,
                Err(e) => return errno(e),
            };
            match rustix::net::connect(&s, &a) {
                Ok(()) => 0,
                Err(e) => errno(e),
            }
        }
        Op::Exec => {
            let argv = [
                c"sh".as_ptr(),
                c"-c".as_ptr(),
                c"exit 0".as_ptr(),
                std::ptr::null(),
            ];
            let envp = [std::ptr::null()];
            // SAFETY: NUL-terminated strings and null-terminated arrays that outlive the call.
            unsafe { libc::execve(c"/bin/sh".as_ptr(), argv.as_ptr(), envp.as_ptr()) };
            std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
        }
        Op::Ptrace => {
            let ppid = rustix::process::getppid().map_or(1, |p| p.as_raw_nonzero().get());
            // SAFETY: PTRACE_ATTACH takes no memory arguments; on success the tracee is
            // detached again at once.
            let r = unsafe {
                libc::ptrace(
                    libc::PTRACE_ATTACH,
                    ppid,
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                )
            };
            if r == 0 {
                // SAFETY: as above.
                unsafe {
                    libc::ptrace(
                        libc::PTRACE_DETACH,
                        ppid,
                        std::ptr::null_mut::<libc::c_void>(),
                        std::ptr::null_mut::<libc::c_void>(),
                    )
                };
                0
            } else {
                std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
            }
        }
        Op::SignalOutside => {
            let Some(pid) = std::env::var("TB_SANDBOX_PPID")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .and_then(rustix::process::Pid::from_raw)
            else {
                return libc::EINVAL;
            };
            match rustix::process::test_kill_process(pid) {
                Ok(()) => 0,
                Err(e) => errno(e),
            }
        }
        Op::Network => {
            let s = match rustix::net::socket(AddressFamily::INET, SocketType::DGRAM, None) {
                Ok(s) => s,
                Err(e) => return errno(e),
            };
            let to = std::net::SocketAddr::from(([192, 0, 2, 1], 9));
            match rustix::net::sendto(&s, b"x", rustix::net::SendFlags::empty(), &to) {
                Ok(_) => 0,
                Err(e) => errno(e),
            }
        }
    }
}
