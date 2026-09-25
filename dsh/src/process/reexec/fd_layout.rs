//! Collision-free descriptors for the internal re-exec protocol.
//!
//! The protocol has no fixed descriptor numbers. Immediately before
//! `posix_spawn`, each protocol endpoint is reserved with
//! `F_DUPFD_CLOEXEC`, which atomically finds *and* holds a currently unused
//! descriptor (`>= INTERNAL_FD_MIN`): anything already open — a
//! process-substitution `/dev/fd/N` handle, a stdio source, a redirect
//! backing file, a status pipe — can never be picked. The returned `OwnedFd`
//! *is* the reservation and must stay alive until `posix_spawn` returns;
//! dropping it early would hand the number back to the kernel for reuse.
//!
//! A process-substitution descriptor is user-visible execution state.
//! Internal re-exec protocol descriptors must never overwrite it.

use anyhow::{Context as _, Result};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::io::{FromRawFd as _, RawFd};

/// Search floor for protocol descriptor allocation, not a reservation.
///
/// `10` keeps the protocol clear of `0/1/2` without assuming anything about
/// higher numbers: the kernel picks whatever is actually unused. A high floor
/// (64/128/200) would fail needlessly under a low `RLIMIT_NOFILE`.
pub const INTERNAL_FD_MIN: RawFd = 10;

/// Descriptor numbers a helper receives through hidden argv.
///
/// Transport metadata only: the numbers travel on the command line, never
/// the request payload itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InternalHelperFds {
    pub request: RawFd,
    pub status: Option<RawFd>,
}

impl InternalHelperFds {
    /// Reject anything that would claim stdio, alias the two channels, or
    /// name a descriptor that is not open. A clean `Err`, never a panic: the
    /// hidden flags are a trust boundary a user can hand-invoke.
    pub fn validate(&self) -> Result<()> {
        validate_protocol_fd(self.request, "internal exec")?;
        if let Some(status) = self.status {
            validate_protocol_fd(status, "internal status")?;
            if status == self.request {
                anyhow::bail!("internal exec and status fds must differ");
            }
        }
        Ok(())
    }
}

fn validate_protocol_fd(fd: RawFd, name: &str) -> Result<()> {
    if !(3..).contains(&fd) {
        anyhow::bail!("{name} fd must not claim a standard descriptor");
    }
    if !is_open_fd(fd) {
        anyhow::bail!("{name} fd is not open");
    }
    Ok(())
}

fn is_open_fd(fd: RawFd) -> bool {
    if fd < 0 {
        return false;
    }
    // SAFETY: `F_GETFD` never takes ownership; the fd is only probed.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).is_ok()
}

/// Atomically find and reserve an unused descriptor `>= min_fd` holding a
/// duplicate of `source`.
///
/// Never split this into "find a free number, use it later": on a
/// multithreaded process another thread could claim the number in between.
/// The kernel's descriptor table stays the source of truth — no userspace
/// `HashSet`/`AtomicI32` allocator, no `/proc/self/fd` scan (Linux-only).
pub fn reserve_spawn_fd(source: BorrowedFd<'_>, min_fd: RawFd) -> Result<OwnedFd> {
    let raw = nix::fcntl::fcntl(source, nix::fcntl::FcntlArg::F_DUPFD_CLOEXEC(min_fd))
        .context("failed to reserve internal protocol fd")?;
    // SAFETY: `F_DUPFD_CLOEXEC` returned a fresh descriptor we own exactly once.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Spawn-local reservation of the helper's protocol descriptor numbers.
///
/// Built while every source and auxiliary descriptor is still open, so the
/// kernel cannot hand back a number already in use; held (as `OwnedFd`)
/// until `posix_spawn` returns, so the numbers cannot be recycled mid-spawn.
#[derive(Debug)]
pub struct InternalFdLayout {
    request: OwnedFd,
    status: Option<OwnedFd>,
}

impl InternalFdLayout {
    /// Reserve one target per channel. Each reservation stays open while the
    /// next is made, so `request != status` falls out of the allocation.
    pub fn reserve(
        request_source: BorrowedFd<'_>,
        status_source: Option<BorrowedFd<'_>>,
    ) -> Result<Self> {
        let request = reserve_spawn_fd(request_source, INTERNAL_FD_MIN)
            .context("failed to reserve internal request fd")?;
        let status = match status_source {
            Some(source) => Some(
                reserve_spawn_fd(source, INTERNAL_FD_MIN)
                    .context("failed to reserve internal status fd")?,
            ),
            None => None,
        };
        let layout = Self { request, status };
        layout.check_targets(
            request_source.as_fd().as_raw_fd(),
            status_source.map(|source| source.as_raw_fd()),
        )?;
        Ok(layout)
    }

    pub fn request_fd(&self) -> RawFd {
        self.request.as_fd().as_raw_fd()
    }

    pub fn status_fd(&self) -> Option<RawFd> {
        self.status.as_ref().map(|fd| fd.as_fd().as_raw_fd())
    }

    fn check_targets(&self, request_source: RawFd, status_source: Option<RawFd>) -> Result<()> {
        let request = self.request_fd();
        anyhow::ensure!(
            request > 2,
            "reserved internal request fd must not be stdio"
        );
        anyhow::ensure!(
            request != request_source,
            "reserved internal request fd must differ from its source"
        );
        if let Some(status) = self.status_fd() {
            anyhow::ensure!(status > 2, "reserved internal status fd must not be stdio");
            anyhow::ensure!(
                status != request,
                "reserved internal request and status fds must differ"
            );
            if let Some(source) = status_source {
                anyhow::ensure!(
                    status != source,
                    "reserved internal status fd must differ from its source"
                );
            }
        }
        Ok(())
    }
}

/// Mark the helper's status descriptor close-on-exec, or do nothing.
///
/// `None` (status-less helpers such as background builtins) performs zero
/// syscalls, so an unrelated inherited descriptor can never gain `CLOEXEC`
/// by accident. `dup2` clears `CLOEXEC` on its target, which is why the
/// helper sets it back here: the helper itself needs the descriptor, but a
/// nested external child or sub-helper must not inherit it and hide the
/// parent's EOF.
pub fn setup_helper_status_fd(status: Option<RawFd>) -> Result<()> {
    let Some(fd) = status else {
        return Ok(());
    };
    // SAFETY: only probed/mutated via `fcntl`; ownership stays with the caller.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let flags = nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD)
        .context("internal status fd is not open")?;
    let mut bits = nix::fcntl::FdFlag::from_bits_retain(flags);
    bits.insert(nix::fcntl::FdFlag::FD_CLOEXEC);
    nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_SETFD(bits))
        .context("failed to mark internal status fd close-on-exec")?;
    Ok(())
}

/// Deliver the whole payload, retrying `EINTR`. A zero or failed write is an
/// `Err` — the caller must reap the already-spawned helper, not leak it.
pub fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        match nix::unistd::write(fd, bytes) {
            Ok(0) => anyhow::bail!("failed to deliver internal request: write returned zero"),
            Ok(n) => bytes = &bytes[n..],
            Err(nix::errno::Errno::EINTR) => continue,
            Err(err) => anyhow::bail!("failed to deliver internal request: {err}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloexec_pair() -> (OwnedFd, OwnedFd) {
        crate::process::io::cloexec_pipe().expect("pipe")
    }

    /// A non-`CLOEXEC` pipe like the one `<(...)` hands to its consumer as
    /// `/dev/fd/N`: inherited across `posix_spawn` by default.
    fn aux_pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe failed");
        // SAFETY: `pipe` succeeded, so both fds are owned exactly once.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn cloexec_flag(fd: RawFd) -> bool {
        // SAFETY: `F_GETFD` only probes.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let flags = nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).expect("F_GETFD");
        nix::fcntl::FdFlag::from_bits_retain(flags).contains(nix::fcntl::FdFlag::FD_CLOEXEC)
    }

    #[test]
    fn reservation_is_neither_source_nor_below_floor() {
        let (read, _write) = cloexec_pair();
        let target = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        assert!(target.as_raw_fd() >= INTERNAL_FD_MIN);
        assert_ne!(target.as_raw_fd(), read.as_raw_fd());
    }

    #[test]
    fn two_live_reservations_are_distinct() {
        let (read, _write) = cloexec_pair();
        let first = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        let second = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        assert_ne!(first.as_raw_fd(), second.as_raw_fd());
    }

    #[test]
    fn reservation_is_cloexec_before_spawn() {
        let (read, _write) = cloexec_pair();
        let target = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        assert!(cloexec_flag(target.as_raw_fd()));
    }

    #[test]
    fn reservation_avoids_every_open_aux_fd() {
        // Stand-ins for process-substitution handles in both directions
        // (`<(...)` read ends and `>(...)` write ends): whatever numbers
        // they hold, the allocator must pick something else.
        let aux: Vec<(OwnedFd, OwnedFd)> = (0..20).map(|_| aux_pipe()).collect();
        let aux_numbers: Vec<RawFd> = aux
            .iter()
            .flat_map(|(read, write)| [read.as_raw_fd(), write.as_raw_fd()])
            .collect();
        let (read, write) = cloexec_pair();
        let layout = InternalFdLayout::reserve(read.as_fd(), Some(write.as_fd())).expect("reserve");
        assert!(!aux_numbers.contains(&layout.request_fd()));
        assert!(!aux_numbers.contains(&layout.status_fd().expect("status")));
        assert_ne!(layout.request_fd(), layout.status_fd().expect("status"));
        assert!(layout.request_fd() > 2);
        assert!(layout.status_fd().expect("status") > 2);
    }

    #[test]
    fn live_reservation_holds_its_number() {
        let (read, _write) = cloexec_pair();
        let first = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        // While `first` is alive the same number must not be handed out again.
        let second = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        assert_ne!(first.as_raw_fd(), second.as_raw_fd());
    }

    #[test]
    fn dropped_reservation_frees_a_descriptor() {
        let (read, _write) = cloexec_pair();
        // Hold one reservation to learn a valid target, then release it.
        // No exact-number assertion here: `cargo test` runs threads in one
        // process on a shared fd table, so another thread may claim the
        // freed number before we re-reserve. Validity (floor + source
        // distinctness) is the contract; exact reuse is kernel trivia.
        let number = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN)
            .expect("reserve")
            .as_raw_fd();
        assert!(number >= INTERNAL_FD_MIN);
        let again = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
        assert!(again.as_raw_fd() >= INTERNAL_FD_MIN);
        assert_ne!(again.as_raw_fd(), read.as_raw_fd());
    }

    #[test]
    fn helper_fds_validation_rejects_bad_channels() {
        let (read, write) = cloexec_pair();
        let request = read.as_raw_fd();
        let status = write.as_raw_fd();
        InternalHelperFds {
            request,
            status: Some(status),
        }
        .validate()
        .expect("open distinct fds validate");
        // stdio must never be claimable as protocol.
        for std in [0, 1, 2] {
            assert!(
                InternalHelperFds {
                    request: std,
                    status: None,
                }
                .validate()
                .is_err()
            );
            assert!(
                InternalHelperFds {
                    request,
                    status: Some(std),
                }
                .validate()
                .is_err()
            );
        }
        // Aliased channels, closed fds, and negatives fail closed.
        assert!(
            InternalHelperFds {
                request,
                status: Some(request),
            }
            .validate()
            .is_err()
        );
        drop(read);
        drop(write);
        // Recently freed numbers are not probed here: another test thread
        // in the same process may recycle them before validation runs.
        // Closed-channel coverage uses numbers no live thread can hold.
        for closed in [997_333, -1] {
            assert!(
                InternalHelperFds {
                    request: closed,
                    status: None,
                }
                .validate()
                .is_err(),
                "closed request fd {closed} must fail"
            );
        }
    }

    #[test]
    fn status_none_leaves_unrelated_fds_alone() {
        let (read, write) = aux_pipe();
        let read_flags = {
            // SAFETY: `F_GETFD` only probes.
            let borrowed = unsafe { BorrowedFd::borrow_raw(read.as_raw_fd()) };
            nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).expect("F_GETFD")
        };
        setup_helper_status_fd(None).expect("None is a no-op");
        let after = {
            // SAFETY: `F_GETFD` only probes.
            let borrowed = unsafe { BorrowedFd::borrow_raw(read.as_raw_fd()) };
            nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).expect("F_GETFD")
        };
        assert_eq!(read_flags, after, "unrelated fd flags must not change");
        assert!(!cloexec_flag(write.as_raw_fd()));
        setup_helper_status_fd(Some(write.as_raw_fd())).expect("mark");
        assert!(cloexec_flag(write.as_raw_fd()));
    }

    #[test]
    fn concurrent_reservations_are_unique() {
        let count = 16;
        let barrier = std::sync::Barrier::new(count);
        std::thread::scope(|scope| {
            let barrier = &barrier;
            let mut handles = Vec::new();
            for _ in 0..count {
                handles.push(scope.spawn(move || {
                    let (read, _write) = cloexec_pair();
                    let first = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
                    let second = reserve_spawn_fd(read.as_fd(), INTERNAL_FD_MIN).expect("reserve");
                    barrier.wait();
                    (first.as_raw_fd(), second.as_raw_fd())
                }));
            }
            let mut numbers = Vec::new();
            for handle in handles {
                let (first, second) = handle.join().expect("thread");
                assert_ne!(first, second);
                numbers.push(first);
                numbers.push(second);
            }
            numbers.sort_unstable();
            numbers.dedup();
            assert_eq!(numbers.len(), 2 * count, "all live targets must be unique");
        });
    }
}
