//! Raw post-`fork` child execution for external commands.
//!
//! This is the only code that runs in a `fork()` child before `execve`.
//! The child of a multi-threaded Tokio process must not touch anything that
//! can block on another thread's lock: no allocator-dependent business logic,
//! no `tracing`, no `anyhow`, no `parking_lot`, no `Environment`, no `Shell`
//! methods, no `Builtin` handlers, and no `std::process::exit` (its
//! destructors are skipped anyway; `libc::_exit` is the honest primitive).
//!
//! The parent prepares everything — resolved `CString`s, `execve` pointer
//! arrays, stdio fds, job-control flags, the exec-error pipe — and the child
//! only performs raw syscalls from a preallocated, read-only plan:
//!
//! ```text
//! getpid / setpgid / setsid / sigaction / dup2 / close / ioctl /
//! read / write / execve / _exit (+ fcntl where noted)
//! ```
//!
//! Anything else added here needs a comment explaining why it is fork-safe.
//!
//! Foreground terminal handoff (`tcsetpgrp`) is intentionally *not* here: the
//! parent owns it through `job_wait::put_in_foreground`. A launch barrier was
//! considered (child waits on a gate pipe until the parent finished
//! `setpgid`+`tcsetpgrp`) but omitted on purpose — the pre-existing double
//! `setpgid` (child here, parent in `job::launch_process`) is retained, and
//! the wait path grants the terminal promptly, so a gate would add pipe
//! lifetime complexity for no new ordering guarantee.

use std::ffi::c_char;
use std::os::unix::io::RawFd;

/// exec-error pipe stage codes. Small POD, written with `write(2)` only.
pub const STAGE_SETPGID: u8 = 1;
pub const STAGE_SETSID: u8 = 2;
pub const STAGE_SIGNAL: u8 = 3;
pub const STAGE_DUP2_STDIN: u8 = 4;
pub const STAGE_DUP2_STDOUT: u8 = 5;
pub const STAGE_DUP2_STDERR: u8 = 6;
pub const STAGE_CLOSE: u8 = 7;
pub const STAGE_EXECVE: u8 = 8;

/// Failure record the child writes to the exec-error pipe before `_exit`.
///
/// The pipe's write end is `CLOEXEC`: `execve` success closes it and the
/// parent sees EOF; failure writes exactly one of these and the parent
/// formats the user-visible diagnostic (it owns allocation and `format!`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ChildExecError {
    pub stage: u8,
    pub errno: i32,
}

/// Read-only plan prepared by the parent before `fork`.
///
/// All pointed-to memory (`executable`, `argv`, `envp`, `not_found_msg`)
/// lives in parent-owned `CString`/byte buffers that outlive the `fork`;
/// the child only reads through these pointers and never allocates.
pub struct RawChildPlan {
    pub executable: *const c_char,
    pub argv: *const *const c_char,
    pub envp: *const *const c_char,
    pub stdin: RawFd,
    pub stdout: RawFd,
    pub stderr: RawFd,
    /// Target pgid for `setpgid(0, pgid)` when `interactive` and not a
    /// full-proxy PTY job (those `setsid` instead).
    pub pgid: libc::pid_t,
    pub interactive: bool,
    pub full_proxy_pty: bool,
    /// PTY slave fd for `TIOCSCTTY`; `-1` when no PTY.
    pub pty_slave: RawFd,
    /// Write end of the exec-error pipe (`CLOEXEC`); `-1` disables reporting.
    pub exec_error_fd: RawFd,
    /// Prebuilt `command not found` message; null pointer means "resolved".
    pub not_found_msg: *const u8,
    pub not_found_len: usize,
    pub not_found_fd: RawFd,
}

// The child reads the plan through `fork`-duplicated memory only.
unsafe impl Send for RawChildPlan {}
unsafe impl Sync for RawChildPlan {}

/// Write the whole buffer with `write(2)`, retrying short writes.
/// No allocation, no errno formatting — best effort only.
unsafe fn write_all(fd: RawFd, mut ptr: *const u8, mut len: usize) {
    while len > 0 {
        let n = unsafe { libc::write(fd, ptr as *const libc::c_void, len) };
        if n <= 0 {
            break;
        }
        ptr = unsafe { ptr.add(n as usize) };
        len -= n as usize;
    }
}

unsafe fn report_error(fd: RawFd, stage: u8, errno: i32) {
    if fd < 0 {
        return;
    }
    let record = ChildExecError { stage, errno };
    let ptr = (&record as *const ChildExecError) as *const u8;
    unsafe { write_all(fd, ptr, std::mem::size_of::<ChildExecError>()) };
}

/// Portability note: Linux exposes `__errno_location`, macOS exposes
/// `__error`; both return a pointer to the thread-local errno.
#[cfg(target_os = "macos")]
unsafe fn last_errno() -> i32 {
    unsafe { *libc::__error() }
}

#[cfg(not(target_os = "macos"))]
unsafe fn last_errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

/// Reset one signal to `SIG_DFL` with raw `sigaction(2)`.
/// Returns 0 on success, errno on failure. `nix`/`anyhow` stay out.
unsafe fn reset_signal(sig: libc::c_int) -> i32 {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        action.sa_flags = 0;
        if libc::sigemptyset(&mut action.sa_mask) != 0 {
            return last_errno();
        }
        if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
            return last_errno();
        }
    }
    0
}

/// The single post-`fork` child function for external commands.
///
/// # Safety
///
/// Call only in the child branch immediately after `fork`, with `plan`
/// referencing parent-preallocated memory that the child may read.
/// Never returns: `execve` replaces the image on success, `_exit` otherwise.
pub unsafe fn exec_external_child(plan: &RawChildPlan) -> ! {
    unsafe {
        // Unresolved command: the message goes to *this* process's stderr
        // target (so `typo 2>/dev/null` is quiet), then exit 127 like every
        // other shell.
        if !plan.not_found_msg.is_null() {
            write_all(plan.not_found_fd, plan.not_found_msg, plan.not_found_len);
            libc::_exit(127);
        }

        if plan.interactive && !plan.full_proxy_pty {
            // Joined by the parent's own `setpgid(child, pgid)` after the
            // fork; both sides calling it is the standard race-free pattern.
            if libc::setpgid(0, plan.pgid) != 0 {
                let errno = last_errno();
                report_error(plan.exec_error_fd, STAGE_SETPGID, errno);
                libc::_exit(1);
            }
        }

        // Restore the job-control defaults the shell itself ignores.
        // (Non-interactive children only need SIGPIPE reset; Rust ignores it
        // by default and a non-reset child would survive EPIPE silently.)
        if plan.interactive {
            const RESET_ALL: &[libc::c_int] = &[
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
                libc::SIGCHLD,
                libc::SIGPIPE,
            ];
            for &sig in RESET_ALL {
                let errno = reset_signal(sig);
                if errno != 0 {
                    report_error(plan.exec_error_fd, STAGE_SIGNAL, errno);
                    libc::_exit(1);
                }
            }
        } else if reset_signal(libc::SIGPIPE) != 0 {
            let errno = last_errno();
            report_error(plan.exec_error_fd, STAGE_SIGNAL, errno);
            libc::_exit(1);
        }

        if plan.full_proxy_pty {
            if libc::setsid() == -1 {
                let errno = last_errno();
                report_error(plan.exec_error_fd, STAGE_SETSID, errno);
                libc::_exit(1);
            }
            if plan.pty_slave >= 0 {
                // Best effort, matching historical behavior: failure means the
                // session is already the leader; the command still runs.
                libc::ioctl(plan.pty_slave, libc::TIOCSCTTY as libc::c_ulong, 0);
            }
        }

        // Stdio wiring. Aliasing (`cmd > file 2>&1`, `cmd 2>&1 > file`) is
        // order-sensitive: stdin's source fd is kept while it still matches a
        // later `dup2` source, exactly as the pre-refactor logic did.
        let keep_stdin = plan.stdin == plan.stdout || plan.stdin == plan.stderr;
        if plan.stdin != libc::STDIN_FILENO && libc::dup2(plan.stdin, libc::STDIN_FILENO) < 0 {
            let errno = last_errno();
            report_error(plan.exec_error_fd, STAGE_DUP2_STDIN, errno);
            libc::_exit(1);
        }
        let stdin_kept_open = plan.stdin > 2 && keep_stdin && plan.stdin != libc::STDIN_FILENO;
        if plan.stdin > 2 && !keep_stdin && libc::close(plan.stdin) != 0 {
            let errno = last_errno();
            report_error(plan.exec_error_fd, STAGE_CLOSE, errno);
            libc::_exit(1);
        }

        if plan.stdout == plan.stderr {
            if plan.stdout != libc::STDOUT_FILENO
                && libc::dup2(plan.stdout, libc::STDOUT_FILENO) < 0
            {
                let errno = last_errno();
                report_error(plan.exec_error_fd, STAGE_DUP2_STDOUT, errno);
                libc::_exit(1);
            }
            if plan.stderr != libc::STDERR_FILENO
                && libc::dup2(plan.stderr, libc::STDERR_FILENO) < 0
            {
                let errno = last_errno();
                report_error(plan.exec_error_fd, STAGE_DUP2_STDERR, errno);
                libc::_exit(1);
            }
            if plan.stdout > 2 && libc::close(plan.stdout) != 0 {
                // `2>&1`-style aliasing: the source may already be closed via
                // the stdin branch above when all three were the same fd.
                let errno = last_errno();
                if errno != libc::EBADF {
                    report_error(plan.exec_error_fd, STAGE_CLOSE, errno);
                    libc::_exit(1);
                }
            }
        } else {
            if plan.stdout != libc::STDOUT_FILENO {
                if libc::dup2(plan.stdout, libc::STDOUT_FILENO) < 0 {
                    let errno = last_errno();
                    report_error(plan.exec_error_fd, STAGE_DUP2_STDOUT, errno);
                    libc::_exit(1);
                }
                if plan.stdout > 2 && libc::close(plan.stdout) != 0 {
                    let errno = last_errno();
                    if errno != libc::EBADF {
                        report_error(plan.exec_error_fd, STAGE_CLOSE, errno);
                        libc::_exit(1);
                    }
                }
            }
            if plan.stderr != libc::STDERR_FILENO {
                if libc::dup2(plan.stderr, libc::STDERR_FILENO) < 0 {
                    let errno = last_errno();
                    report_error(plan.exec_error_fd, STAGE_DUP2_STDERR, errno);
                    libc::_exit(1);
                }
                if plan.stderr > 2 && libc::close(plan.stderr) != 0 {
                    let errno = last_errno();
                    if errno != libc::EBADF {
                        report_error(plan.exec_error_fd, STAGE_CLOSE, errno);
                        libc::_exit(1);
                    }
                }
            }
        }
        if stdin_kept_open && plan.stdin > 2 {
            // Closed late: stdin's fd doubled as a stdout/stderr source above.
            libc::close(plan.stdin);
        }

        libc::execve(plan.executable, plan.argv, plan.envp);
        let errno = last_errno();
        report_error(plan.exec_error_fd, STAGE_EXECVE, errno);
        libc::_exit(1);
    }
}
