//! Whether a repository's skills may reach the prompt.
//!
//! A `.dsh/hooks.json` is not read at all, on the grounds that cloning a
//! repository should not be enough to run its commands. Project skills were
//! read unconditionally, which opened the same door one room over: a skill's
//! `description` goes into the system prompt before the user has decided
//! anything, and the agent on the other side of that prompt has `execute`.
//! The Agent Skills implementation guide names this case directly and
//! recommends gating project-level loading on a trust check.
//!
//! What is trusted is a **directory plus the set of (name, description) pairs
//! it currently advertises**, because that pair is exactly what enters the
//! prompt. A body only reaches the model through a `read_file` the user can
//! see. Adding a skill, or rewording one, asks again.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tracing::debug;

use super::Skill;

const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TrustRecord {
    pub digest: String,
    #[serde(default)]
    pub decided_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    #[serde(default)]
    roots: BTreeMap<String, TrustRecord>,
}

impl Default for TrustFile {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            roots: BTreeMap::new(),
        }
    }
}

/// A stable fingerprint of what this root would put in the prompt.
///
/// FNV-1a rather than `DefaultHasher`, whose output is explicitly not stable
/// between Rust releases - a toolchain upgrade would have re-asked about every
/// project. It detects change; it is not a defence against a deliberate
/// collision, and it does not need to be: crafting one requires write access to
/// a repository the user has already trusted, and at that point the attacker
/// can simply reword a skill they know is trusted.
pub(crate) fn digest(skills: &[Skill]) -> String {
    // The untruncated summary, not `summary()`: keying on the prompt's
    // display budget would re-ask about every trusted project the next time
    // `MAX_SKILL_SUMMARY_CHARS` changes, for a reason that has nothing to do
    // with what changed in that project.
    let mut pairs: Vec<String> = skills
        .iter()
        .map(|skill| format!("{}\u{1f}{}", skill.name, skill.raw_summary()))
        .collect();
    pairs.sort();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pairs.join("\u{1e}").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn key(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

/// The approval key for a one-session answer.
///
/// Carries the digest so that trusting today's skills does not silently cover
/// one added an hour later.
pub(crate) fn session_key(root: &Path, digest: &str) -> String {
    format!("skills:{}:{digest}", key(root))
}

pub(crate) fn is_remembered(root: &Path, digest: &str) -> bool {
    load()
        .get(&key(root))
        .is_some_and(|record| record.digest == digest)
}

pub(crate) fn remember(root: &Path, digest: &str) {
    let path = crate::config_paths::skills_trust_file();
    let Some(mut state) = read_state(&path) else {
        return;
    };
    state.roots.insert(
        key(root),
        TrustRecord {
            digest: digest.to_string(),
            decided_ms: super::usage::now_ms(),
        },
    );
    write_state(&path, &state);
}

/// Move an already-trusted root to a new digest.
///
/// Called after the user approves a `skill_manage` write into a project: the
/// set of descriptions just changed by their own hand, so re-asking on the next
/// turn would be asking about a decision they have already made. A root that
/// was never trusted stays untrusted.
pub(crate) fn refresh(root: &Path, digest: &str) {
    if load().contains_key(&key(root)) {
        remember(root, digest);
    }
}

pub(crate) fn forget(root: &Path) -> bool {
    let path = crate::config_paths::skills_trust_file();
    let Some(mut state) = read_state(&path) else {
        return false;
    };
    let removed = state.roots.remove(&key(root)).is_some();
    if removed {
        write_state(&path, &state);
    }
    removed
}

pub(crate) fn load() -> BTreeMap<String, TrustRecord> {
    read_state(&crate::config_paths::skills_trust_file())
        .unwrap_or_default()
        .roots
}

fn read_state(path: &Path) -> Option<TrustFile> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Some(TrustFile::default());
    };
    match serde_json::from_str::<TrustFile>(&contents) {
        Ok(state) if state.version == STATE_VERSION => Some(state),
        // A file this version does not understand is left alone rather than
        // rewritten in this shape, which would drop the other version's
        // decisions and silently re-open every project.
        Ok(state) => {
            debug!("leaving skill trust state version {} alone", state.version);
            None
        }
        Err(err) => {
            debug!("replacing unreadable skill trust state: {err}");
            Some(TrustFile::default())
        }
    }
}

fn write_state(path: &Path, state: &TrustFile) {
    let Ok(serialized) = serde_json::to_string_pretty(state) else {
        return;
    };
    if let Err(err) = crate::atomic_write::write_atomic(path, &serialized, true, "skill-trust") {
        debug!("failed to write skill trust state: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt::skills::{SkillOrigin, SkillRoot, SkillScope, SkillsManager};

    fn skills_in(dir: &Path, entries: &[(&str, &str)]) -> Vec<Skill> {
        for (name, description) in entries {
            let skill = dir.join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {description}\n---\n"),
            )
            .unwrap();
        }
        SkillsManager::with_roots(vec![SkillRoot {
            scope: SkillScope::Project,
            origin: SkillOrigin::Dsh,
            path: dir.to_path_buf(),
        }])
        .load_skills()
    }

    #[test]
    fn the_digest_ignores_order_but_not_content() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = skills_in(a.path(), &[("alpha", "one"), ("beta", "two")]);
        let second = skills_in(b.path(), &[("beta", "two"), ("alpha", "one")]);
        assert_eq!(digest(&first), digest(&second));

        let c = tempfile::tempdir().unwrap();
        let reworded = skills_in(c.path(), &[("alpha", "one"), ("beta", "CHANGED")]);
        assert_ne!(digest(&first), digest(&reworded));

        let d = tempfile::tempdir().unwrap();
        let added = skills_in(
            d.path(),
            &[("alpha", "one"), ("beta", "two"), ("gamma", "three")],
        );
        assert_ne!(digest(&first), digest(&added));
    }

    /// Trusting today's skills must not cover one added an hour later.
    #[test]
    fn the_session_key_changes_with_the_digest() {
        let root = Path::new("/tmp/example");
        assert_ne!(session_key(root, "aaaa"), session_key(root, "bbbb"));
        assert!(session_key(root, "aaaa").starts_with("skills:"));
    }

    #[test]
    fn an_empty_digest_is_stable() {
        assert_eq!(digest(&[]), digest(&[]));
    }
}
