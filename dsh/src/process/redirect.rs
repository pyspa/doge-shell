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

use dsh_types::Context;
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::unistd::dup;
use std::fs::{File, OpenOptions};
use std::os::fd::BorrowedFd;
use std::os::unix::io::{AsRawFd, RawFd};

/// Exit status for any redirection setup failure, runnable or no-command.
///
/// POSIX only requires non-zero; the single shared constant keeps both paths
/// reporting the same status instead of drifting apart.
pub(crate) const REDIRECTION_FAILURE_EXIT_CODE: i32 = 1;

/// A redirection setup failure: missing file, permission denied, bad fd,
/// unsupported slot.
///
/// This is an *expected command-level failure*, not a runtime infrastructure
/// error: the command fails with [`REDIRECTION_FAILURE_EXIT_CODE`] while the
/// shell stays alive. The message is the user-facing diagnostic (without the
/// `dsh: ` prefix; the caller that reports it owns the prefix). Callers must
/// match on this type, never on the message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedirectFailure {
    message: String,
}

impl RedirectFailure {
    pub(crate) fn new(message: String) -> Self {
        Self { message }
    }

    /// The user-facing diagnostic, e.g.
    /// `failed to create redirect file '/x': Permission denied (os error 13)`.
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RedirectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RedirectFailure {}

/// One redirection, in the order the user wrote it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Redirect {
    /// The descriptor being redirected: `2` in `2> err`.
    pub fd: RawFd,
    pub op: RedirectOp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug)]
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
///
/// A `RedirectFailure` leaves `ctx` exactly as it was: unsupported slots are
/// rejected before anything is applied, and a mid-list open failure rolls
/// back the earlier entries first. The caller reports the failure as an
/// ordinary command failure; this layer never prints anything itself because
/// the stderr destination differs per execution path (normal, capture,
/// struct pipe, helper).
pub(crate) fn apply(
    redirects: &[Redirect],
    ctx: &mut Context,
) -> Result<AppliedRedirects, RedirectFailure> {
    // Reject unsupported slots before touching anything: these are a mistake in
    // the command, not a runtime failure, so they should not half-apply the
    // redirects written before them. Failures that only show up on open are
    // rolled back in the loop below instead.
    for redirect in redirects {
        if !is_standard_slot(redirect.fd) {
            return Err(RedirectFailure::new(format!(
                "redirecting file descriptor {} is not supported",
                redirect.fd
            )));
        }
        // The source matters just as much: only the three standard slots are
        // tracked, so any other number would name one of the *shell's* own
        // descriptors -- its history database, config file or PTY master --
        // and hand the child a writable duplicate of it.
        if let RedirectOp::DupFrom(source) = redirect.op
            && !is_standard_slot(source)
        {
            return Err(RedirectFailure::new(format!(
                "{source}: bad file descriptor"
            )));
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
) -> Result<RawFd, RedirectFailure> {
    let source = match &redirect.op {
        RedirectOp::ReadFile(path) => {
            let file = File::open(path).map_err(|err| {
                RedirectFailure::new(format!(
                    "failed to open input redirect file '{path}': {err}"
                ))
            })?;
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
        RedirectOp::WriteFile(path) => {
            let file = File::create(path).map_err(|err| {
                RedirectFailure::new(format!("failed to create redirect file '{path}': {err}"))
            })?;
            let fd = file.as_raw_fd();
            files.push(file);
            fd
        }
        RedirectOp::AppendFile(path) => {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|err| {
                    RedirectFailure::new(format!("failed to open redirect file '{path}': {err}"))
                })?;
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
            let copy = dup(unsafe { BorrowedFd::borrow_raw(source) }).map_err(|err| {
                RedirectFailure::new(format!(
                    "failed to duplicate file descriptor {source}: {err}"
                ))
            })?;
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
                .map_err(|err| RedirectFailure::new(format!("failed to open /dev/null: {err}")))?;
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

    /// A bad duplication source rolls back an earlier successful redirect:
    /// the source check runs up front, but this guards the shape where a
    /// dup fails at open time after a file was already created.
    #[test]
    fn a_bad_duplication_source_restores_the_context() {
        let mut ctx = test_context();
        let before = (ctx.infile, ctx.outfile, ctx.errfile);

        let redirects = vec![
            Redirect::write(STDOUT_FILENO, "/dev/null".to_string()),
            Redirect::dup(STDERR_FILENO, 9),
        ];

        let err = match apply(&redirects, &mut ctx) {
            Ok(_) => panic!("duplicating an untracked descriptor must fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("bad file descriptor"),
            "unexpected error: {err}"
        );
        assert_eq!(
            (ctx.infile, ctx.outfile, ctx.errfile),
            before,
            "the context still names descriptors from the failed apply"
        );
    }

    /// Every failure kind carries a stable diagnostic category (not an
    /// exact OS message): callers report these without string-matching.
    #[test]
    fn failures_carry_stable_diagnostic_categories() {
        let missing = "/nonexistent-directory-for-dsh/nope";
        let cases: Vec<(Redirect, &str)> = vec![
            (
                Redirect::input(missing.to_string()),
                "failed to open input redirect file",
            ),
            (
                Redirect::write(STDOUT_FILENO, missing.to_string()),
                "failed to create redirect file",
            ),
            (
                Redirect::append(STDOUT_FILENO, missing.to_string()),
                "failed to open redirect file",
            ),
            (Redirect::dup(STDERR_FILENO, 9), "bad file descriptor"),
            (
                Redirect {
                    fd: 7,
                    op: RedirectOp::WriteFile("/dev/null".to_string()),
                },
                "is not supported",
            ),
        ];
        for (redirect, category) in cases {
            let mut ctx = test_context();
            let err = match apply(std::slice::from_ref(&redirect), &mut ctx) {
                Ok(_) => panic!("{redirect:?} must fail"),
                Err(err) => err,
            };
            assert!(
                err.message().contains(category),
                "{redirect:?}: {err:?} does not carry {category:?}"
            );
        }
    }

    /// Successful redirects preserve left-to-right ordering: `> f 2>&1`
    /// points both streams at the file, while `2>&1 > f` leaves stderr on
    /// the previous stdout.
    #[test]
    fn successful_redirects_preserve_left_to_right_ordering() {
        use std::os::fd::BorrowedFd;

        fn same_file(a: RawFd, b: RawFd) -> bool {
            let stat = |fd: RawFd| nix::sys::stat::fstat(unsafe { BorrowedFd::borrow_raw(fd) });
            match (stat(a), stat(b)) {
                (Ok(a), Ok(b)) => a.st_dev == b.st_dev && a.st_ino == b.st_ino,
                _ => false,
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("order.txt");

        // `> f 2>&1`: both streams leave their original homes for the file.
        let mut ctx = test_context();
        let before = (ctx.infile, ctx.outfile, ctx.errfile);
        let redirects = vec![
            Redirect::write(STDOUT_FILENO, file.to_string_lossy().into_owned()),
            Redirect::dup(STDERR_FILENO, STDOUT_FILENO),
        ];
        let applied = apply(&redirects, &mut ctx).expect("> f 2>&1 applies");
        assert_ne!(ctx.outfile, before.1, "stdout must point at the file");
        assert!(
            same_file(ctx.outfile, ctx.errfile),
            "stderr must follow stdout into the file"
        );
        applied.restore(&mut ctx);
        assert_eq!((ctx.infile, ctx.outfile, ctx.errfile), before);

        // `2>&1 > f`: stderr keeps the destination stdout had *at that
        // point*, so only stdout moves to the file.
        let mut ctx = test_context();
        let before = (ctx.infile, ctx.outfile, ctx.errfile);
        let redirects = vec![
            Redirect::dup(STDERR_FILENO, STDOUT_FILENO),
            Redirect::write(STDOUT_FILENO, file.to_string_lossy().into_owned()),
        ];
        let applied = apply(&redirects, &mut ctx).expect("2>&1 > f applies");
        assert_ne!(ctx.outfile, before.1, "stdout must point at the file");
        assert!(
            same_file(ctx.errfile, before.1),
            "stderr must keep the previous stdout"
        );
        assert!(
            !same_file(ctx.outfile, ctx.errfile),
            "stderr must not follow stdout into the file"
        );
        applied.restore(&mut ctx);
        assert_eq!((ctx.infile, ctx.outfile, ctx.errfile), before);
    }
}
