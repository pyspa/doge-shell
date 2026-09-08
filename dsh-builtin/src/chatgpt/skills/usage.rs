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
    let Some(record) = record else {
        return false;
    };
    let reference = if record.last_read_ms > 0 {
        record.last_read_ms
    } else {
        record.created_ms
    };
    days_since(now_ms, reference).is_some_and(|days| days >= UNUSED_AFTER_DAYS)
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
}
