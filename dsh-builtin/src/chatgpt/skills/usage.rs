//! How often each skill actually gets read, and when it was last useful.
//!
//! This is the feedback half of "a skill the agent grows": without it there is
//! no way to tell a skill that earns its place in every prompt from one that has
//! not been opened since it was written.
//!
//! Two deliberate choices:
//!
//! - The file lives outside the skills directory
//!   (`config_paths::skills_state_file`). The installer replaces a runtime skill
//!   with `rm -rf <skill>`, and `doctor` counts entries under the skills root to
//!   warn about prompt footprint.
//! - Counters are buffered in memory and written once per turn. A `read_file`
//!   happens inside the agent loop, which can run a hundred iterations; one
//!   `fsync` per iteration is not worth a counter. Two shells racing lose a
//!   count, never a skill.

use super::SkillScope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SkillUsage {
    #[serde(default)]
    pub scope: String,
    /// `"agent"` when `skill_manage` created it, `"user"` otherwise.
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_ms: u64,
    #[serde(default)]
    pub reads: u64,
    #[serde(default)]
    pub last_read_ms: u64,
    #[serde(default)]
    pub writes: u64,
    #[serde(default)]
    pub last_write_ms: u64,
    /// When this skill was archived, `0` if it never was. `#[serde(default)]`
    /// so a record written before this field existed loads as unarchived,
    /// and so a newer shell's record survives round-tripping through an
    /// older one that does not know this key (see `STATE_VERSION`'s comment
    /// on why the version itself does not move for this).
    #[serde(default)]
    pub archived_ms: u64,
    /// Pinned skills are never touched by the optional auto-archive sweep.
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageFile {
    version: u32,
    #[serde(default)]
    skills: BTreeMap<String, SkillUsage>,
}

impl Default for UsageFile {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            skills: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
struct PendingSkill {
    scope: Option<SkillScope>,
    reads: u64,
    writes: u64,
    created_by_agent: bool,
}

static PENDING: LazyLock<Mutex<BTreeMap<PathBuf, PendingSkill>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn with_pending<R>(f: impl FnOnce(&mut BTreeMap<PathBuf, PendingSkill>) -> R) -> Option<R> {
    PENDING.lock().ok().map(|mut pending| f(&mut pending))
}

pub(crate) fn note_read(skill_dir: &Path, scope: SkillScope) {
    with_pending(|pending| {
        let entry = pending.entry(skill_dir.to_path_buf()).or_default();
        entry.scope = Some(scope);
        entry.reads += 1;
    });
}

pub(crate) fn note_write(skill_dir: &Path, scope: SkillScope, created: bool) {
    with_pending(|pending| {
        let entry = pending.entry(skill_dir.to_path_buf()).or_default();
        entry.scope = Some(scope);
        entry.writes += 1;
        entry.created_by_agent |= created;
    });
}

/// Drop everything recorded about a skill that no longer exists.
///
/// Applied immediately rather than buffered: the caller has just deleted the
/// directory, and a stale record would be reported by `skill list` as a skill
/// that is not there.
pub(crate) fn forget(skill_dir: &Path) {
    with_pending(|pending| pending.remove(skill_dir));
    let path = crate::config_paths::skills_state_file();
    let Some(mut state) = read_state(&path) else {
        return;
    };
    if state.skills.remove(&key(skill_dir)).is_some() {
        write_state(&path, &state);
    }
}

/// Whether this turn's buffered counters already include a write.
///
/// Read before `flush()` empties the buffer - reflection has to ask this
/// before the turn's own bookkeeping is merged in, or it would always see
/// zero. A turn where the model wrote a skill itself is not also
/// second-guessed by the reviewer.
pub(crate) fn wrote_this_turn() -> bool {
    with_pending(|pending| pending.values().any(|delta| delta.writes > 0)).unwrap_or(false)
}

/// Every skill directory this turn actually read, for the reviewer to open,
/// and only those - never a skill the turn did not look at. Read before
/// `flush()` for the same reason as `wrote_this_turn`.
pub(crate) fn read_this_turn() -> Vec<PathBuf> {
    with_pending(|pending| {
        pending
            .iter()
            .filter(|(_, delta)| delta.reads > 0)
            .map(|(dir, _)| dir.clone())
            .collect()
    })
    .unwrap_or_default()
}

/// Merge the buffered counters into the state file. Called once per turn.
pub(crate) fn flush() {
    flush_to(&crate::config_paths::skills_state_file());
}

fn flush_to(path: &Path) {
    let Some(pending) = with_pending(std::mem::take) else {
        return;
    };
    if pending.is_empty() {
        return;
    }

    let now = now_ms();
    let Some(mut state) = read_state(path) else {
        return;
    };

    for (dir, delta) in pending {
        // A record for a directory that has since gone is noise, and writing it
        // back would resurrect it for every later reader.
        if !dir.exists() {
            state.skills.remove(&key(&dir));
            continue;
        }

        let entry = state.skills.entry(key(&dir)).or_default();
        if let Some(scope) = delta.scope {
            entry.scope = scope.as_str().to_string();
        }
        if entry.created_ms == 0 {
            entry.created_ms = now;
            entry.created_by = if delta.created_by_agent {
                "agent".to_string()
            } else {
                "user".to_string()
            };
        } else if delta.created_by_agent && entry.created_by.is_empty() {
            entry.created_by = "agent".to_string();
        }
        if delta.reads > 0 {
            entry.reads += delta.reads;
            entry.last_read_ms = now;
        }
        if delta.writes > 0 {
            entry.writes += delta.writes;
            entry.last_write_ms = now;
        }
    }

    write_state(path, &state);
}

/// Everything recorded so far, keyed by skill directory.
pub(crate) fn load() -> BTreeMap<String, SkillUsage> {
    read_state(&crate::config_paths::skills_state_file())
        .unwrap_or_default()
        .skills
}

/// Set or clear a skill's archived flag.
///
/// Written immediately, like `forget` - this is a person's direct command
/// (`skill archive`/`skill unarchive`), not a turn's buffered counter.
/// Returns `Ok(false)`, touching nothing, when the state file is a version
/// this shell does not understand - the same "leave it alone" rule every
/// other write here follows.
pub(crate) fn set_archived(skill_dir: &Path, archived: bool) -> Result<bool, String> {
    set_flag_at(
        &crate::config_paths::skills_state_file(),
        skill_dir,
        |entry| {
            entry.archived_ms = if archived { now_ms() } else { 0 };
        },
    )
}

/// Set or clear a skill's pinned flag. Pinned skills are never touched by
/// the optional auto-archive sweep.
pub(crate) fn set_pinned(skill_dir: &Path, pinned: bool) -> Result<bool, String> {
    set_flag_at(
        &crate::config_paths::skills_state_file(),
        skill_dir,
        |entry| {
            entry.pinned = pinned;
        },
    )
}

fn set_flag_at(
    path: &Path,
    skill_dir: &Path,
    apply: impl FnOnce(&mut SkillUsage),
) -> Result<bool, String> {
    let Some(mut state) = read_state(path) else {
        return Ok(false);
    };
    let entry = state.skills.entry(key(skill_dir)).or_default();
    apply(entry);
    write_state(path, &state);
    Ok(true)
}

pub(crate) fn is_archived(record: Option<&SkillUsage>) -> bool {
    record.is_some_and(|record| record.archived_ms > 0)
}

/// Every skill directory (by `key()`) currently archived.
///
/// Read only by `render_fragment` - `load_skills`/`load_reporting` must
/// never filter on this, or the set of `(name, description)` pairs a trust
/// digest is keyed on would change just by archiving something, re-asking
/// every trusted project for a reason that has nothing to do with what
/// changed in it.
pub(crate) fn archived_keys() -> std::collections::BTreeSet<String> {
    load()
        .into_iter()
        .filter(|(_, record)| is_archived(Some(record)))
        .map(|(key, _)| key)
        .collect()
}

/// A fingerprint of the archived/pinned flags, for the prompt fragment
/// cache's signature.
///
/// Not the directory mtime the rest of the signature uses: `usage::flush()`
/// rewrites the state file on nearly every turn (read/write counters), and
/// keying the cache on that file's mtime would invalidate the fragment on
/// every turn regardless of whether anything archived actually changed -
/// defeating the point of caching it at all. This only moves when
/// `archived_ms` or `pinned` themselves change.
pub(crate) fn lifecycle_digest() -> String {
    let mut parts: Vec<String> = load()
        .into_iter()
        .filter(|(_, record)| record.archived_ms > 0 || record.pinned)
        .map(|(key, record)| format!("{key}\u{1f}{}\u{1f}{}", record.archived_ms, record.pinned))
        .collect();
    parts.sort();
    super::fnv1a_hex(parts.join("\u{1e}").as_bytes())
}

/// Archive every agent-written, unpinned skill unread for `unused_after_days`.
/// Opt-in: the caller only invokes this when `AI_CHAT_SKILL_AUTO_ARCHIVE_DAYS`
/// says to, and passes the threshold it read from that variable.
///
/// Never touches a `user`-written skill (a person's own notes are not this
/// shell's to hide) and never touches a `project` skill (a repository's
/// skills are not this shell's to prune, and archiving is scoped to `user`
/// specifically so it can never change what a trust digest hashes - see
/// `archived_keys`). Returns how many were newly archived.
pub(crate) fn sweep(now_ms: u64, unused_after_days: u64) -> usize {
    sweep_at(
        &crate::config_paths::skills_state_file(),
        now_ms,
        unused_after_days,
    )
}

fn sweep_at(path: &Path, now_ms: u64, unused_after_days: u64) -> usize {
    let Some(mut state) = read_state(path) else {
        return 0;
    };

    let mut archived = 0usize;
    for record in state.skills.values_mut() {
        if record.scope != "user"
            || record.created_by != "agent"
            || record.pinned
            || record.archived_ms > 0
        {
            continue;
        }
        if stale_after(record, now_ms, unused_after_days) {
            record.archived_ms = now_ms;
            archived += 1;
        }
    }

    if archived > 0 {
        write_state(path, &state);
    }
    archived
}

/// How a skill directory is named in the state file.
///
/// One definition: `skill list` and `doctor` look records up by the same string
/// the writer used, and canonicalising in only some of those places produced
/// misses that read as "never used".
pub(crate) fn key(skill_dir: &Path) -> String {
    std::fs::canonicalize(skill_dir)
        .unwrap_or_else(|_| skill_dir.to_path_buf())
        .display()
        .to_string()
}

/// The state on disk, or `None` when there is a file this version must not
/// touch.
///
/// Returning an empty default for a newer file and then writing it back is how
/// a downgrade silently destroys the newer records. A file that will not parse
/// at all carries no such information, so that one is allowed to heal.
fn read_state(path: &Path) -> Option<UsageFile> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Some(UsageFile::default());
    };
    match serde_json::from_str::<UsageFile>(&contents) {
        Ok(state) if state.version == STATE_VERSION => Some(state),
        Ok(state) => {
            debug!(
                "leaving skill usage state version {} alone; this shell writes version {STATE_VERSION}",
                state.version
            );
            None
        }
        Err(err) => {
            debug!("replacing unreadable skill usage state: {err}");
            Some(UsageFile::default())
        }
    }
}

fn write_state(path: &Path, state: &UsageFile) {
    let Ok(serialized) = serde_json::to_string_pretty(state) else {
        return;
    };
    // Bookkeeping: a failure here must never surface as a chat error.
    if let Err(err) = crate::atomic_write::write_atomic(path, &serialized, true, "skill-usage") {
        debug!("failed to write skill usage state: {err}");
    }
}

/// How long a skill may go unread before it is worth mentioning.
pub(crate) const UNUSED_AFTER_DAYS: u64 = 90;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Whole days between two timestamps, `0` when `then` is in the future.
///
/// A clock correction must not turn a skill that was read a minute ago into one
/// that looks abandoned.
pub(crate) fn days_since(now_ms: u64, then_ms: u64) -> Option<u64> {
    if then_ms == 0 {
        return None;
    }
    Some(now_ms.saturating_sub(then_ms) / DAY_MS)
}

/// Is this skill worth suggesting for removal?
///
/// One definition, shared by `skill list` and `doctor skills`, which used to
/// disagree at the boundary and about clock skew.
///
/// A skill with no record at all is *not* stale: nothing has been observed
/// about it, and a freshly installed one would otherwise be reported as dead on
/// the first run. A skill that has never been read is judged on its age, so the
/// one the agent wrote a minute ago is left alone.
pub(crate) fn is_stale(record: Option<&SkillUsage>, now_ms: u64) -> bool {
    record.is_some_and(|record| stale_after(record, now_ms, UNUSED_AFTER_DAYS))
}

/// `is_stale`'s definition, parametrised on the day count - shared so the
/// optional auto-archive sweep judges staleness the exact same way
/// `skill list`/`doctor skills` display it, just against a threshold the
/// caller picked instead of the fixed one.
fn stale_after(record: &SkillUsage, now_ms: u64, unused_after_days: u64) -> bool {
    let reference = if record.last_read_ms > 0 {
        record.last_read_ms
    } else {
        record.created_ms
    };
    days_since(now_ms, reference).is_some_and(|days| days >= unused_after_days)
}

#[cfg(test)]
mod tests;
