//! Redirections: what they mean, and how they are applied.
//!
//! Each redirection names a descriptor and where it should point. Applying the
//! list left to right is what gives the ordering its meaning: `cmd > f 2>&1`
//! sends both streams to the file, while `cmd 2>&1 > f` leaves stderr on the
//! terminal, and neither needs a special case.
//!
//! Files are opened *before* the command runs and handed to it directly, rather
//! than piping through a copier task. That is what makes `>` visible to the
//! very next command, lets `>>` create a missing file, and stops a foreground
//! builtin from blocking once it writes more than a pipe buffer.

use anyhow::{Context as _, Result, bail};
use dsh_types::Context;
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::dup;
use std::fs::{File, OpenOptions};
use std::os::fd::BorrowedFd;
use std::os::unix::io::{AsRawFd, RawFd};

/// One redirection, in the order the user wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// The descriptor being redirected: `2` in `2> err`.
    pub fd: RawFd,
    pub op: RedirectOp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectOp {
    ReadFile(String),
    WriteFile(String),
    AppendFile(String),
    /// `2>&1`: point at whatever that descriptor points at *right now*.
    DupFrom(RawFd),
    /// `2>&-`. Modelled as `/dev/null` so a child writing to it sees a
    /// well-behaved sink rather than an unexpected `EBADF`.
    Close,
}

impl Redirect {
    pub fn write(fd: RawFd, path: String) -> Self {
        Self {
            fd,
            op: RedirectOp::WriteFile(path),
        }
    }

    pub fn append(fd: RawFd, path: String) -> Self {
        Self {
            fd,
            op: RedirectOp::AppendFile(path),
        }
    }

    pub fn input(path: String) -> Self {
        Self {
            fd: STDIN_FILENO,
            op: RedirectOp::ReadFile(path),
        }
    }

    pub fn dup(fd: RawFd, from: RawFd) -> Self {
        Self {
            fd,
            op: RedirectOp::DupFrom(from),
        }
    }

    pub fn close(fd: RawFd) -> Self {
        Self {
            fd,
            op: RedirectOp::Close,
        }
    }

    /// `&> f` is `> f` followed by `2>&1`, so it desugars into two entries
    /// instead of needing a variant of its own.
    pub fn both(path: String, append: bool) -> Vec<Self> {
        let first = if append {
            Self::append(STDOUT_FILENO, path)
        } else {
            Self::write(STDOUT_FILENO, path)
        };
        vec![first, Self::dup(STDERR_FILENO, STDOUT_FILENO)]
    }

    pub fn is_stdin(&self) -> bool {
        self.fd == STDIN_FILENO
    }
}

/// Open files kept alive for as long as the redirections are in force.
///
/// The child inherits the descriptors at fork, and a foreground builtin writes
/// to them in-process, so these must outlive both.
pub(crate) struct AppliedRedirects {
    _files: Vec<File>,
    /// Slot number and what it held before, so the change can be undone.
    ///
    /// `Context` is shared across a pipeline, so a redirection left in place
    /// becomes the *next* command's descriptor. `ls 2>&1 | wc` pointed stderr
    /// at the pipe, and `wc` then inherited that same descriptor after the
    /// shell had already closed it.
    saved: Vec<(RawFd, RawFd)>,
    changed_stdin: bool,
}

impl AppliedRedirects {
    pub(crate) fn changed_stdin(&self) -> bool {
        self.changed_stdin
    }

    /// Whether this guard owns `fd`, and will therefore close it itself.
    ///
    /// Callers that close a process's descriptors after launch must ask first:
    /// closing one of these too would be a double close, and once an unrelated
    /// allocation reuses the number it stops being harmless.
    pub(crate) fn owns(&self, fd: RawFd) -> bool {
        self._files.iter().any(|file| file.as_raw_fd() == fd)
    }

    /// Put the descriptors back the way they were, once the process that
    /// wanted them has been launched.
    pub(crate) fn restore(&self, ctx: &mut Context) {
        for (slot, previous) in self.saved.iter().rev() {
            set_slot(ctx, *slot, *previous);
        }
    }
}

/// Point `ctx`'s descriptors at what `redirects` asks for, in order.
pub(crate) fn apply(redirects: &[Redirect], ctx: &mut Context) -> Result<AppliedRedirects> {
    // Reject unsupported slots before touching anything: these are a mistake in
    // the command, not a runtime failure, so they should not half-apply the
    // redirects written before them. Failures that only show up on open are
    // rolled back in the loop below instead.
    for redirect in redirects {
        if !is_standard_slot(redirect.fd) {
            bail!(
                "redirecting file descriptor {} is not supported",
                redirect.fd
            );
        }
        // The source matters just as much: only the three standard slots are
        // tracked, so any other number would name one of the *shell's* own
        // descriptors -- its history database, config file or PTY master --
        // and hand the child a writable duplicate of it.
        if let RedirectOp::DupFrom(source) = redirect.op
            && !is_standard_slot(source)
        {
            bail!("{source}: bad file descriptor");
        }
    }

    let mut applied = AppliedRedirects {
        _files: Vec::new(),
        saved: Vec::new(),
        changed_stdin: false,
    };

    for redirect in redirects {
        // Every arm below can fail, and by then earlier redirects have already
        // been written into `ctx`. Returning straight away dropped their files —
        // closing the descriptors `ctx` still named — and the next `pipe()` got
        // the same number back, which the shell then closed a second time:
        // `IO Safety violation: owned file descriptor already closed, aborting`.
        // So roll `ctx` back before the files go out of scope.
        let source = match open_redirect_source(redirect, ctx, &mut applied._files) {
            Ok(source) => source,
            Err(err) => {
                applied.restore(ctx);
                return Err(err);
            }
        };

        applied
            .saved
            .push((redirect.fd, current_slot(ctx, redirect.fd)));
        set_slot(ctx, redirect.fd, source);
        applied.changed_stdin |= redirect.is_stdin();
    }

    Ok(applied)
}

/// Open (or duplicate) what one redirect points at, handing the file to
/// `files` so the guard owns it.
fn open_redirect_source(
    redirect: &Redirect,
    ctx: &Context,
    files: &mut Vec<File>,
) -> Result<RawFd> {
    let source = match &redirect.op {
        RedirectOp::ReadFile(path) => {
            let file = File::open(path)
                .with_context(|| format!("failed to open input redirect file '{path}'"))?;
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
        RedirectOp::WriteFile(path) => {
            let file = File::create(path)
                .with_context(|| format!("failed to create redirect file '{path}'"))?;
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
        RedirectOp::AppendFile(path) => {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("failed to open redirect file '{path}'"))?;
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
        // Duplicate now rather than remembering the number. The child
        // rewires descriptors one at a time, so by the time it gets to
        // stderr a later `> file` has already replaced fd 1 -- which is
        // why `cmd 2>&1 > f` used to send stderr to the file too.
        RedirectOp::DupFrom(from) => {
            let source = current_slot(ctx, *from);
            let copy = dup(unsafe { BorrowedFd::borrow_raw(source) })
                .with_context(|| format!("failed to duplicate file descriptor {source}"))?;
            let file = File::from(copy);
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
        RedirectOp::Close => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")
                .context("failed to open /dev/null")?;
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
    };

    Ok(source)
}

/// What `fd` currently points at. Only the three standard descriptors are
/// tracked in `Context`; anything else is passed through by number, which is
/// what the child inherits anyway.
fn current_slot(ctx: &Context, fd: RawFd) -> RawFd {
    match fd {
        STDIN_FILENO => ctx.infile,
        STDOUT_FILENO => ctx.outfile,
        STDERR_FILENO => ctx.errfile,
        other => other,
    }
}

/// `Context` carries only the three standard slots, so only these can be
/// redirected; `apply` rejects anything else up front.
fn is_standard_slot(fd: RawFd) -> bool {
    matches!(fd, STDIN_FILENO | STDOUT_FILENO | STDERR_FILENO)
}

fn set_slot(ctx: &mut Context, fd: RawFd, source: RawFd) {
    match fd {
        STDIN_FILENO => ctx.infile = source,
        STDOUT_FILENO => ctx.outfile = source,
        STDERR_FILENO => ctx.errfile = source,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::Context;
    use nix::unistd::Pid;

    fn test_context() -> Context {
        Context::new(Pid::from_raw(0), Pid::from_raw(0), None, true)
    }

    /// A redirect that fails after an earlier one has already been written into
    /// `ctx` must not leave that descriptor behind: the file backing it is
    /// closed on the way out, and the next `pipe()` hands the same number to
    /// someone else who then gets it closed under them.
    #[test]
    fn a_failed_redirect_puts_the_context_back() {
        let mut ctx = test_context();
        let before = (ctx.infile, ctx.outfile, ctx.errfile);

        let redirects = vec![
            Redirect {
                fd: STDERR_FILENO,
                op: RedirectOp::DupFrom(STDOUT_FILENO),
            },
            Redirect::write(
                STDOUT_FILENO,
                "/nonexistent-directory-for-dsh/nope".to_string(),
            ),
        ];

        let err = match apply(&redirects, &mut ctx) {
            Ok(_) => panic!("the second redirect cannot be opened"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("failed to create redirect file"),
            "unexpected error: {err}"
        );
        assert_eq!(
            (ctx.infile, ctx.outfile, ctx.errfile),
            before,
            "the context still names descriptors from the failed apply"
        );
    }

    /// The rejection of an unsupported slot happens before anything is applied,
    /// so it cannot leave a half-applied context either.
    #[test]
    fn an_unsupported_slot_is_rejected_before_anything_is_applied() {
        let mut ctx = test_context();
        let before = (ctx.infile, ctx.outfile, ctx.errfile);

        let redirects = vec![Redirect {
            fd: 7,
            op: RedirectOp::WriteFile("/dev/null".to_string()),
        }];

        assert!(apply(&redirects, &mut ctx).is_err());
        assert_eq!((ctx.infile, ctx.outfile, ctx.errfile), before);
    }
}
