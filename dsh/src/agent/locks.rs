//! Per-task execution locks and the run-admission check built on them.
//!
//! A single global lock cannot tell "this task is running" apart from "a
//! different task is running", so before this module existed, `agent
//! delete`/`agent respond` on one task would refuse merely because some
//! *other* task happened to be executing, and `recover_interrupted` marked
//! every `Running` row `Interrupted` the instant any one process proved no
//! live owner remained, even for a task a different, still-alive process was
//! in the middle of running.
//!
//! Each task gets its own lock file, `<agent state dir>/locks/<task id>.lock`,
//! held for exactly as long as that task is executing. Dropping the lock
//! releases it, which is how a crashed process's claim is reclaimed: the
//! kernel releases an `flock` the moment the holding process exits, no
//! matter how it exited. [`admit_run`] additionally enforces
//! `AI_AGENT_MAX_CONCURRENT` (default 1, today's behaviour) by counting how
//! many *other* tasks currently hold their lock; the count-then-lock
//! sequence is itself serialised through `<agent state dir>/admission.lock`
//! so two processes racing `admit_run` at once cannot both see room.

use super::SqliteTaskStore;
use anyhow::{Context as _, Result};
use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// [`try_lock_task`] retries this many times before reporting a task busy.
/// `is_locked`'s own probe (used by `admit_run`'s own counting loop, `agent
/// list`, the watcher, ...) takes and immediately releases the same lock
/// file, so a genuine acquire attempt can collide with a probe that is
/// already on its way out - a window measured in CPU cycles, not time a
/// real, sustained holder would ever vacate in. A handful of short retries
/// absorbs that without meaningfully slowing down genuine contention, where
/// every retry keeps failing regardless.
const LOCK_PROBE_RETRIES: u32 = 3;
const LOCK_PROBE_RETRY_DELAY: Duration = Duration::from_millis(2);

/// Holds one task's execution lock for as long as it is alive.
pub(crate) struct TaskLock {
    _file: File,
}

/// The result of asking to run a specific task right now.
pub(crate) enum Admission {
    /// Nothing else holds this task's lock, and there is room under the
    /// concurrency ceiling. Run it for as long as this value is alive.
    Admitted(TaskLock),
    /// This exact task already has a live owner.
    TaskBusy,
    /// `AI_AGENT_MAX_CONCURRENT` is already spent by other tasks.
    NoFreeSlot,
}

fn locks_dir(store: &SqliteTaskStore) -> PathBuf {
    store.root.join("locks")
}

fn lock_path(store: &SqliteTaskStore, id: &str) -> PathBuf {
    locks_dir(store).join(format!("{id}.lock"))
}

fn open_lock_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))
}

/// Whether some other, still-alive process holds the lock at `path`.
///
/// Taking the lock to check it, then immediately releasing it if nobody was
/// there, is the only way to ask "is anyone alive" that cannot itself go
/// stale between the check and the answer.
fn is_locked(path: &Path) -> Result<bool> {
    let file = open_lock_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Tries to take the exclusive lock for one task, without regard to how many
/// other tasks are currently running.
///
/// `Ok(None)` means a live owner already holds it. Used directly by
/// operations that only need this one task's exclusivity - `agent
/// delete`/`agent respond`, and [`SqliteTaskStore::recover_interrupted`]'s
/// per-task proof-of-death - and by [`admit_run`], which additionally
/// weighs the concurrency ceiling.
pub(crate) fn try_lock_task(store: &SqliteTaskStore, id: &str) -> Result<Option<TaskLock>> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(locks_dir(store))?;
    let path = lock_path(store, id);
    for attempt in 0..LOCK_PROBE_RETRIES {
        let file = open_lock_file(&path)?;
        match file.try_lock() {
            Ok(()) => return Ok(Some(TaskLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => {
                if attempt + 1 == LOCK_PROBE_RETRIES {
                    return Ok(None);
                }
                std::thread::sleep(LOCK_PROBE_RETRY_DELAY);
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Ok(None)
}

/// `AI_AGENT_MAX_CONCURRENT`, defaulting to 1 (today's behaviour: one task
/// runs at a time). Read fresh on every call rather than cached, so raising
/// it takes effect on the very next `agent run --detach` or cron tick.
///
/// Shell variable, then environment - the same resolution order
/// `AI_AGENT_TIMEOUT_SECS` and this diff's own
/// `DOGESH_AGENT_WATCH*` use (`super::setting`). A plain `std::env::var` read
/// would silently ignore a value set with the shell's own `set`/`var`
/// builtin rather than exported to the OS environment.
fn max_concurrent(shell: &mut crate::shell::Shell) -> usize {
    super::setting(shell, "AI_AGENT_MAX_CONCURRENT")
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

/// Admits `id` to run right now, honouring both its own exclusivity and the
/// shared concurrency ceiling.
pub(crate) fn admit_run(
    shell: &mut crate::shell::Shell,
    store: &SqliteTaskStore,
    id: &str,
) -> Result<Admission> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(locks_dir(store))?;
    let admission_lock = open_lock_file(&store.root.join("admission.lock"))?;
    admission_lock
        .lock()
        .context("cannot serialise agent task admission")?;
    // Released when `admission_lock` drops at the end of this function -
    // by which point either this task's own lock is held, or nothing here
    // changed.

    let Some(lock) = try_lock_task(store, id)? else {
        return Ok(Admission::TaskBusy);
    };

    let mut held_by_others = 0usize;
    if let Ok(entries) = std::fs::read_dir(locks_dir(store)) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("lock") {
                continue;
            }
            if path.file_stem().and_then(|stem| stem.to_str()) == Some(id) {
                continue; // our own lock, already accounted for below
            }
            if is_locked(&path)? {
                held_by_others += 1;
            }
        }
    }

    if held_by_others + 1 > max_concurrent(shell) {
        return Ok(Admission::NoFreeSlot);
    }
    Ok(Admission::Admitted(lock))
}

/// Whether some process currently holds `id`'s execution lock, for display
/// only (`agent list`'s `*` marker) - never used to gate an action, which
/// always goes through [`admit_run`]/[`try_lock_task`] instead so the check
/// and the act cannot race apart from each other.
pub(crate) fn is_running(store: &SqliteTaskStore, id: &str) -> bool {
    is_locked(&lock_path(store, id)).unwrap_or(false)
}

/// Removes lock files for tasks the store no longer has a row for.
///
/// A finished task's lock file is otherwise never cleaned up (dropping the
/// [`TaskLock`] releases the `flock`, not the file itself), and `agent
/// delete` only ever removes the task's own artifact directory, not
/// `locks/`. Harmless to skip on error - a leftover, unlocked file changes
/// nothing but `agent doctor`'s tidiness.
pub(crate) fn prune_orphaned_locks(store: &SqliteTaskStore, known_ids: &[String]) {
    let Ok(entries) = std::fs::read_dir(locks_dir(store)) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if known_ids.iter().any(|id| id == stem) {
            continue;
        }
        // Only ever remove a lock nobody holds - `is_locked` itself takes
        // and releases it, so this cannot delete out from under a live
        // owner.
        if matches!(is_locked(&path), Ok(false)) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests;
