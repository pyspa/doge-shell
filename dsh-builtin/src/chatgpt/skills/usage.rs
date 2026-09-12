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
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    static TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn test_lock() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        with_pending(|pending| pending.clear());
        guard
    }

    #[test]
    fn reads_accumulate_into_one_write_per_flush() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("state").join("skills.json");

        note_read(&skill, SkillScope::User);
        note_read(&skill, SkillScope::User);
        assert!(!state.exists(), "counters must stay buffered until a flush");

        flush_to(&state);

        let stored = read_state(&state).expect("readable state");
        let entry = stored.skills.get(&key(&skill)).expect("record");
        assert_eq!(entry.reads, 2);
        assert_eq!(entry.scope, "user");
        assert!(entry.last_read_ms > 0);
    }

    #[test]
    fn a_flush_with_nothing_buffered_writes_no_file() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("skills.json");

        flush_to(&state);

        assert!(!state.exists());
    }

    #[test]
    fn a_record_for_a_removed_skill_is_dropped_on_flush() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("gone");
        let state = dir.path().join("skills.json");

        note_read(&skill, SkillScope::User);
        flush_to(&state);

        assert!(
            read_state(&state)
                .expect("readable state")
                .skills
                .is_empty()
        );
    }

    #[test]
    fn a_created_skill_is_attributed_to_the_agent() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("made-up");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("skills.json");

        note_write(&skill, SkillScope::Project, true);
        flush_to(&state);

        let stored = read_state(&state).expect("readable state");
        let entry = stored.skills.get(&key(&skill)).expect("record");
        assert_eq!(entry.created_by, "agent");
        assert_eq!(entry.scope, "project");
        assert_eq!(entry.writes, 1);
    }

    /// A downgrade must leave a newer file exactly as it found it, counters and
    /// all - reading it as empty and then flushing over it is how the records
    /// disappear.
    #[test]
    fn a_state_file_from_a_future_version_is_ignored_not_rewritten() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("skills.json");
        let original = r#"{"version":99,"skills":{"x":{"reads":5}}}"#;
        std::fs::write(&state, original).unwrap();

        assert!(read_state(&state).is_none());

        note_read(&skill, SkillScope::User);
        flush_to(&state);

        assert_eq!(std::fs::read_to_string(&state).unwrap(), original);
    }

    /// A file that will not parse carries nothing worth preserving.
    #[test]
    fn an_unreadable_state_file_is_replaced() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("skills.json");
        std::fs::write(&state, "{ not json").unwrap();

        note_read(&skill, SkillScope::User);
        flush_to(&state);

        assert_eq!(read_state(&state).unwrap().skills.len(), 1);
    }

    #[test]
    fn a_skill_with_no_record_is_never_reported_as_unused() {
        assert!(!is_stale(None, 10 * DAY_MS));
    }

    /// The skill the agent wrote a minute ago must not be suggested for
    /// deletion just because nothing has read it yet.
    #[test]
    fn a_freshly_created_skill_is_not_stale() {
        let now = 400 * DAY_MS;
        let fresh = SkillUsage {
            created_ms: now - DAY_MS,
            ..SkillUsage::default()
        };
        assert!(!is_stale(Some(&fresh), now));

        let old = SkillUsage {
            created_ms: now - 200 * DAY_MS,
            ..SkillUsage::default()
        };
        assert!(is_stale(Some(&old), now));
    }

    #[test]
    fn a_recent_read_keeps_an_old_skill_alive() {
        let now = 400 * DAY_MS;
        let record = SkillUsage {
            created_ms: now - 300 * DAY_MS,
            reads: 3,
            last_read_ms: now - DAY_MS,
            ..SkillUsage::default()
        };
        assert!(!is_stale(Some(&record), now));
    }

    /// A clock that went backwards must not make a skill look abandoned.
    #[test]
    fn a_timestamp_in_the_future_reads_as_zero_days() {
        assert_eq!(days_since(100, 200), Some(0));
        assert_eq!(days_since(100, 0), None);
        let future = SkillUsage {
            last_read_ms: 200,
            ..SkillUsage::default()
        };
        assert!(!is_stale(Some(&future), 100));
    }

    #[test]
    fn archived_and_pinned_survive_a_counter_flush() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("skills.json");

        note_write(&skill, SkillScope::User, true);
        flush_to(&state);

        let mut file = read_state(&state).unwrap();
        let entry = file.skills.get_mut(&key(&skill)).unwrap();
        entry.archived_ms = 42;
        entry.pinned = true;
        write_state(&state, &file);

        note_read(&skill, SkillScope::User);
        flush_to(&state);

        let after = read_state(&state).unwrap();
        let entry = after.skills.get(&key(&skill)).unwrap();
        assert_eq!(entry.archived_ms, 42);
        assert!(entry.pinned);
        assert_eq!(entry.reads, 1);
    }

    /// A state file written before these fields existed must still load, and
    /// the missing fields must read as "not archived, not pinned" rather
    /// than an error - `#[serde(default)]` is what this checks.
    #[test]
    fn a_state_file_written_before_the_lifecycle_fields_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("skills.json");
        std::fs::write(
            &state,
            r#"{"version":1,"skills":{"x":{"scope":"user","reads":3}}}"#,
        )
        .unwrap();

        let loaded = read_state(&state).expect("still readable");
        let entry = loaded.skills.get("x").unwrap();
        assert_eq!(entry.archived_ms, 0);
        assert!(!entry.pinned);
        assert!(!is_archived(Some(entry)));
    }

    /// `sweep` itself has no on/off switch - the environment variable gate
    /// lives in `chatgpt.rs`, which decides whether to call this at all. So
    /// the only thing to prove here is that a skill stale enough to qualify
    /// stays unarchived until something actually calls `sweep_at` - writing
    /// usage records on its own must never archive anything.
    #[test]
    fn auto_archive_is_off_unless_the_caller_invokes_it() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("skills.json");
        let now = 400 * DAY_MS;

        let mut file = UsageFile::default();
        file.skills.insert(
            key(&skill),
            SkillUsage {
                scope: "user".to_string(),
                created_by: "agent".to_string(),
                created_ms: now - 200 * DAY_MS,
                ..SkillUsage::default()
            },
        );
        write_state(&state, &file);

        assert!(
            !is_archived(read_state(&state).unwrap().skills.get(&key(&skill))),
            "a stale skill must not archive itself just by existing"
        );

        let archived = sweep_at(&state, now, UNUSED_AFTER_DAYS);
        assert_eq!(archived, 1);
        assert!(is_archived(
            read_state(&state).unwrap().skills.get(&key(&skill))
        ));
    }

    #[test]
    fn auto_archive_only_touches_agent_written_unpinned_user_skills() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("skills.json");
        let now = 400 * DAY_MS;

        let mut file = UsageFile::default();
        file.skills.insert(
            "agent-user".to_string(),
            SkillUsage {
                scope: "user".to_string(),
                created_by: "agent".to_string(),
                created_ms: now - 200 * DAY_MS,
                ..SkillUsage::default()
            },
        );
        file.skills.insert(
            "pinned".to_string(),
            SkillUsage {
                scope: "user".to_string(),
                created_by: "agent".to_string(),
                created_ms: now - 200 * DAY_MS,
                pinned: true,
                ..SkillUsage::default()
            },
        );
        file.skills.insert(
            "user-written".to_string(),
            SkillUsage {
                scope: "user".to_string(),
                created_by: "user".to_string(),
                created_ms: now - 200 * DAY_MS,
                ..SkillUsage::default()
            },
        );
        file.skills.insert(
            "project".to_string(),
            SkillUsage {
                scope: "project".to_string(),
                created_by: "agent".to_string(),
                created_ms: now - 200 * DAY_MS,
                ..SkillUsage::default()
            },
        );
        file.skills.insert(
            "fresh".to_string(),
            SkillUsage {
                scope: "user".to_string(),
                created_by: "agent".to_string(),
                created_ms: now - DAY_MS,
                ..SkillUsage::default()
            },
        );
        write_state(&state, &file);

        let archived = sweep_at(&state, now, UNUSED_AFTER_DAYS);
        assert_eq!(archived, 1);

        let after = read_state(&state).unwrap();
        assert!(after.skills["agent-user"].archived_ms > 0);
        assert_eq!(after.skills["pinned"].archived_ms, 0);
        assert_eq!(after.skills["user-written"].archived_ms, 0);
        assert_eq!(after.skills["project"].archived_ms, 0);
        assert_eq!(after.skills["fresh"].archived_ms, 0);
    }

    #[test]
    fn set_archived_and_set_pinned_round_trip() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        let state = dir.path().join("skills.json");

        note_write(&skill, SkillScope::User, true);
        flush_to(&state);

        assert!(set_flag_at(&state, &skill, |e| e.pinned = true).unwrap());
        assert!(set_flag_at(&state, &skill, |e| e.archived_ms = 7).unwrap());

        let loaded = read_state(&state).unwrap();
        let entry = loaded.skills.get(&key(&skill)).unwrap();
        assert!(entry.pinned);
        assert_eq!(entry.archived_ms, 7);
    }

    /// `set_flag` (and therefore `set_archived`/`set_pinned`) must not touch
    /// a state file whose version this shell does not understand - the same
    /// rule every other write here follows.
    #[test]
    fn set_flag_leaves_a_future_version_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("skills.json");
        let original = r#"{"version":99,"skills":{}}"#;
        std::fs::write(&state, original).unwrap();

        let applied = set_flag_at(&state, Path::new("/tmp/does-not-matter"), |e| {
            e.pinned = true
        })
        .unwrap();
        assert!(!applied);
        assert_eq!(std::fs::read_to_string(&state).unwrap(), original);
    }

    #[test]
    fn wrote_this_turn_reflects_only_the_buffered_writes() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("demo");

        assert!(!wrote_this_turn());
        note_read(&skill, SkillScope::User);
        assert!(!wrote_this_turn());
        note_write(&skill, SkillScope::User, false);
        assert!(wrote_this_turn());
    }

    #[test]
    fn read_this_turn_lists_only_directories_actually_read() {
        let _lock = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let read_dir = dir.path().join("read");
        let written_dir = dir.path().join("written");

        note_read(&read_dir, SkillScope::User);
        note_write(&written_dir, SkillScope::User, false);

        let opened = read_this_turn();
        assert_eq!(opened, vec![read_dir]);
    }
}
