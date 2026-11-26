//! Test helper of `tests/confine.rs`: enters the sandbox, tries what it must not be able to
//! do before and after the seccomp filter, and writes the outcomes to its IPC descriptor as
//! text lines (`<stage> <op> <errno>`), then `done`.
//!
//! Arguments: `errno` or `kill` (the filter's action); `exec` to also try `execve` after the
//! filter; `crash` to raise SIGSEGV at the end; `sleep` to sleep a minute at the end;
//! `exit=N` to exit with code N.
#![allow(unsafe_code, reason = "raise(SIGSEGV) for the crash test")]

use std::io::Write;

use tb_sandbox::probe::{Op, attempt};
use tb_sandbox::seccomp::{Action, Profile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut sb = match tb_sandbox::child::enter() {
        Ok(s) => s,
        Err(_) => std::process::exit(tb_sandbox::EXIT_SETUP),
    };
    let Some(ipc) = sb.take_ipc() else {
        std::process::exit(tb_sandbox::EXIT_SETUP);
    };
    let mut out = std::fs::File::from(ipc);
    let line = |out: &mut std::fs::File, stage: &str, op: Op, e: i32| {
        let _ = writeln!(out, "{stage} {op:?} {e}");
    };
    for op in Op::SAFE {
        line(&mut out, "before", op, attempt(op));
    }
    let _ = writeln!(out, "landlock {}", sb.landlock_abi());
    let action = if args.iter().any(|a| a == "kill") {
        Action::Kill
    } else {
        Action::Errno
    };
    if sb.lock_down(Profile::Compute, action).is_err() {
        std::process::exit(tb_sandbox::EXIT_SETUP);
    }
    if action == Action::Errno {
        for op in Op::SAFE {
            line(&mut out, "after", op, attempt(op));
        }
        if args.iter().any(|a| a == "exec") {
            line(&mut out, "after", Op::Exec, attempt(Op::Exec));
        }
    }
    let _ = writeln!(out, "done");
    let _ = out.flush();
    if args.iter().any(|a| a == "sleep") {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    if args.iter().any(|a| a == "crash") {
        // SAFETY: signal(3) and raise(3) (rt_sigaction, tgkill: allowed by the filter) have no
        // memory arguments. Rust's runtime handles SIGSEGV to detect stack overflows and
        // ignores a raised one; with the default action restored it ends the process, as a
        // real fault would.
        unsafe {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            libc::raise(libc::SIGSEGV);
        }
    }
    if action == Action::Kill {
        // outside the allow-list: the filter kills the process here
        let _ = attempt(Op::Socket);
    }
    if let Some(n) = args.iter().find_map(|a| a.strip_prefix("exit=")) {
        std::process::exit(n.parse().unwrap_or(1));
    }
}
