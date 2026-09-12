//! Skill writes waiting for a person to review them.
//!
//! `skill_manage` normally writes straight to disk after the user answers a
//! confirmation, the same as `edit`. Two situations cannot use that path:
//! an unattended `agent run` with no `--write` grant for the target (asking
//! would stall the task, so today it stalls anyway with `InputRequired`), and
//! `AI_CHAT_SKILL_STAGING=always`, where a person has asked to review every
//! skill change before it lands. Both stage a `Proposal` here instead of
//! writing, and `skill approve` applies it later through the exact same
//! writer `skill_manage` itself uses.
//!
//! Deliberately outside the skills directories, beside `skills_state_file()`
//! and `skills_trust_file()`: a proposal is not yet a skill, so it must not
//! be counted by `doctor`'s prompt-footprint warning or be swept up by the
//! installer replacing a runtime skill with `rm -rf <skill>`.
//!
//! A file this shell cannot read is never silently dropped, the same rule
//! the usage and trust state files follow - it is reported as `Broken`
//! rather than skipped, so a person can `skill reject` it by id even when it
//! will not parse.

use super::SkillScope;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::debug;

const PENDING_VERSION: u32 = 1;
/// Ceiling on how many proposals may wait at once. Independent of
/// `MAX_PENDING_TOTAL_BYTES`; either one alone can refuse a new proposal.
const MAX_PENDING: usize = 32;
/// Ceiling on one proposal's encoded size.
const MAX_PROPOSAL_BYTES: usize = 64 * 1024;
/// Ceiling on the whole queue's encoded size.
const MAX_PENDING_TOTAL_BYTES: u64 = 1024 * 1024;

/// A staged `skill_manage` write, applied content and all.
///
/// `contents` already holds the result of applying the change - `patch`'s
/// diff is resolved before staging, not replayed at approval time - so
/// `skill approve` never has to duplicate `skill_manage`'s patch logic. What
/// it must still check is whether the target has moved since: `base_digest`
/// is the target file's content digest as it stood when this was staged (or
/// `None` for a brand-new skill), and approval refuses a proposal whose
/// target no longer matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Proposal {
    pub version: u32,
    /// Deterministic: `proposal_id(scope, name, file)`. Staging the same
    /// skill/file again replaces the earlier proposal rather than growing
    /// the queue.
    pub id: String,
    /// `SkillScope::as_str()`.
    pub scope: String,
    pub name: String,
    /// Path inside the skill, e.g. `SKILL.md` or `references/api.md`.
    pub file: String,
    /// `"create"` or `"write_file"` - what `skill approve` will do.
    pub action: String,
    /// The project this proposal belongs to, for `scope == "project"`. An
    /// approval is refused if the current project does not match: a
    /// proposal staged in one repository must not land in another.
    #[serde(default)]
    pub project_root: Option<PathBuf>,
    pub contents: String,
    /// `content_digest` of the target file when this was staged. `None` for
    /// a proposal that would create a new file.
    #[serde(default)]
    pub base_digest: Option<String>,
    pub created_ms: u64,
    /// `"tool"` (the model called `skill_manage` and got staged) or
    /// `"reflection"` (the turn-end reviewer proposed it).
    pub origin: String,
    /// A one-line reason, shown by `skill pending`.
    #[serde(default)]
    pub note: Option<String>,
}

/// A proposal file this shell could not read. Never deleted automatically -
/// only `skill reject` removes it - so a broken file is a surfaced problem,
/// not a silent loss.
#[derive(Debug)]
pub(crate) struct Broken {
    pub path: PathBuf,
    pub reason: String,
}

/// How many proposals this process has staged, for the end-of-turn notice.
/// Never reset: the notice compares against the value it read before the
/// turn, not against zero.
static STAGED_THIS_PROCESS: AtomicUsize = AtomicUsize::new(0);

fn dir() -> PathBuf {
    crate::config_paths::skills_pending_dir()
}

fn path_for(id: &str) -> PathBuf {
    dir().join(format!("{id}.json"))
}

/// A readable stand-in for `value`, not a unique one - every character
/// outside `[A-Za-z0-9_-]` collapses to `_`, so two different `file` values
/// can produce the same result. `proposal_id` never uses this alone for
/// that reason; it is followed by a hash of the untouched string.
fn sanitize_id_part(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// How much of the digest suffix rides along for a non-`SKILL.md` file - long
/// enough that two different paths sanitizing to the same text still get
/// different ids, short enough to still type.
const FILE_ID_DIGEST_CHARS: usize = 8;

/// The id a proposal for this scope/name/file will always have.
///
/// Deterministic on purpose: a second proposal for the same skill and file
/// replaces the first instead of piling up, and a person can type
/// `skill diff user.rust-bisect` without ever having seen a generated id.
///
/// For a bundled file, `sanitize_id_part` alone is not enough - it is not
/// injective, so `references/api.md` and a file literally named
/// `references_api_md` would otherwise land on the same id and silently
/// replace each other. The digest suffix is keyed on the untouched `file`
/// string, so it disambiguates whenever the sanitized text does not.
pub(crate) fn proposal_id(scope: SkillScope, name: &str, file: &str) -> String {
    if file.is_empty() || file == "SKILL.md" {
        format!("{}.{name}", scope.as_str())
    } else {
        let digest = &super::fnv1a_hex(file.as_bytes())[..FILE_ID_DIGEST_CHARS];
        format!(
            "{}.{name}.{}-{digest}",
            scope.as_str(),
            sanitize_id_part(file)
        )
    }
}

/// FNV-1a of `contents`, to notice whether a target changed since staging.
pub(crate) fn content_digest(contents: &str) -> String {
    super::fnv1a_hex(contents.as_bytes())
}

fn total_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

/// How many proposals are staged and parseable - what the caller-facing
/// "the queue is full" checks count against.
pub(crate) fn count() -> usize {
    list().0.len()
}

/// Whether another proposal would fit under `MAX_PENDING`. The reflection
/// reviewer checks this before sending a request at all - a full queue is
/// not worth the API call.
pub(crate) fn has_room() -> bool {
    count() < MAX_PENDING
}

/// How many proposals this process has staged, for the turn-end notice.
pub(crate) fn staged_this_process() -> usize {
    STAGED_THIS_PROCESS.load(Ordering::Relaxed)
}

/// Serializes `stage()`'s count-then-write against every other process
/// doing the same thing, not just other threads in this one.
///
/// `dsh` runs each `agent run` as its own process, so an in-process `Mutex`
/// would not have stopped two of them racing near `MAX_PENDING`: both could
/// read the same count before either writes, and both proceed. Best-effort -
/// a filesystem that cannot lock still lets staging work, just without the
/// guarantee that the cap is never briefly exceeded by a concurrent racer.
fn acquire_lock(root: &Path) -> Option<std::fs::File> {
    let path = root.join(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .inspect_err(|err| debug!("could not open the skill-proposal lock file: {err}"))
        .ok()?;
    file.lock()
        .inspect_err(|err| debug!("could not lock the skill-proposal queue: {err}"))
        .ok()?;
    Some(file)
}

/// Stage a proposal, or refuse if the queue is already full.
///
/// `proposal.id` must already be set (`proposal_id`); this does not compute
/// it, so a caller cannot stage under the wrong key by accident.
pub(crate) fn stage(mut proposal: Proposal) -> Result<String, String> {
    let root = dir();
    std::fs::create_dir_all(&root)
        .map_err(|err| format!("chat: failed to create {}: {err}", root.display()))?;
    // Held for the rest of this function, so the count check below and the
    // write further down are one atomic step as far as another `stage()` -
    // in this process or another - is concerned.
    let _lock = acquire_lock(&root);

    proposal.version = PENDING_VERSION;
    let id = proposal.id.clone();
    let path = path_for(&id);

    // Replacing an existing proposal for the same skill/file does not grow
    // the queue - a model that reconsiders should not be penalised for it.
    if !path.exists() {
        let existing = count();
        if existing >= MAX_PENDING {
            return Err(format!(
                "chat: {existing} skill proposals are already waiting; review them with `skill pending`"
            ));
        }
    }

    let serialized = serde_json::to_string_pretty(&proposal)
        .map_err(|err| format!("chat: failed to encode the proposal: {err}"))?;
    if serialized.len() > MAX_PROPOSAL_BYTES {
        return Err(format!(
            "chat: this skill change is too large to stage ({} bytes; limit {MAX_PROPOSAL_BYTES})",
            serialized.len()
        ));
    }
    // Conservative on a replacement (counts the file being overwritten
    // twice): the ceiling exists to bound worst-case disk use, not to be
    // exact, and erring toward refusing a large queue is the safe direction.
    if total_bytes(&root).saturating_add(serialized.len() as u64) > MAX_PENDING_TOTAL_BYTES {
        return Err(
            "chat: the pending skill queue is full; review it with `skill pending`".to_string(),
        );
    }

    crate::atomic_write::write_atomic(&path, &serialized, true, "skill-proposal")
        .map_err(|err| format!("chat: failed to stage the skill change: {err}"))?;

    STAGED_THIS_PROCESS.fetch_add(1, Ordering::Relaxed);
    Ok(id)
}

/// Every staged proposal, oldest first, alongside anything that would not
/// parse. A version this shell does not understand is treated the same as
/// something that failed to parse: reported, never rewritten.
pub(crate) fn list() -> (Vec<Proposal>, Vec<Broken>) {
    let root = dir();
    let mut proposals = Vec::new();
    let mut broken = Vec::new();

    let Ok(entries) = std::fs::read_dir(&root) else {
        return (proposals, broken);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Proposal>(&contents) {
                Ok(proposal) if proposal.version == PENDING_VERSION => proposals.push(proposal),
                Ok(proposal) => broken.push(Broken {
                    path,
                    reason: format!(
                        "this shell does not understand proposal version {}",
                        proposal.version
                    ),
                }),
                Err(err) => broken.push(Broken {
                    path,
                    reason: format!("cannot parse: {err}"),
                }),
            },
            Err(err) => broken.push(Broken {
                path,
                reason: format!("cannot read: {err}"),
            }),
        }
    }

    proposals.sort_by(|a, b| {
        a.created_ms
            .cmp(&b.created_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    (proposals, broken)
}

/// Find one proposal by its exact id, or by an unambiguous prefix of one.
pub(crate) fn find(id_or_prefix: &str) -> Result<Proposal, String> {
    let (proposals, _broken) = list();
    if let Some(exact) = proposals.iter().find(|p| p.id == id_or_prefix) {
        return Ok(exact.clone());
    }
    let mut matches = proposals
        .into_iter()
        .filter(|p| p.id.starts_with(id_or_prefix));
    let Some(first) = matches.next() else {
        return Err(format!(
            "chat: no pending skill proposal matches `{id_or_prefix}`; run `skill pending` to see what is waiting"
        ));
    };
    if matches.next().is_some() {
        return Err(format!(
            "chat: `{id_or_prefix}` matches more than one pending proposal; be more specific"
        ));
    }
    Ok(first)
}

/// Remove a proposal by its exact id, after it is approved or rejected.
pub(crate) fn remove(id: &str) -> bool {
    let removed = std::fs::remove_file(path_for(id)).is_ok();
    if !removed {
        debug!("no pending skill proposal file for `{id}`");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `skills_pending_dir()` is read from the environment, so a test that
    /// stages a proposal has to own `XDG_STATE_HOME` for its duration - the
    /// crate-wide lock, shared with every other module that scopes the same
    /// environment variable, so two test threads never race to set it.
    fn with_state_home<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let previous = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: single-threaded under `env_lock`.
        unsafe { std::env::set_var("XDG_STATE_HOME", dir) };
        let result = f();
        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        result
    }

    fn sample(id: &str) -> Proposal {
        Proposal {
            version: PENDING_VERSION,
            id: id.to_string(),
            scope: "user".to_string(),
            name: "demo".to_string(),
            file: "SKILL.md".to_string(),
            action: "create".to_string(),
            project_root: None,
            contents: "---\nname: demo\ndescription: d\n---\n\nbody\n".to_string(),
            base_digest: None,
            created_ms: 1,
            origin: "tool".to_string(),
            note: None,
        }
    }

    #[test]
    fn a_staged_proposal_round_trips_through_find_and_list() {
        let dir = tempfile::tempdir().unwrap();
        with_state_home(dir.path(), || {
            let id = stage(sample("user.demo")).unwrap();
            assert_eq!(id, "user.demo");

            let (proposals, broken) = list();
            assert_eq!(proposals.len(), 1);
            assert!(broken.is_empty());

            let found = find("user.demo").unwrap();
            assert_eq!(found.id, "user.demo");
            let by_prefix = find("user.de").unwrap();
            assert_eq!(by_prefix.id, "user.demo");

            assert!(remove("user.demo"));
            assert!(list().0.is_empty());
        });
    }

    #[test]
    fn the_same_skill_replaces_its_earlier_proposal_instead_of_growing_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        with_state_home(dir.path(), || {
            let mut first = sample("user.demo");
            first.note = Some("first".to_string());
            stage(first).unwrap();

            let mut second = sample("user.demo");
            second.note = Some("second".to_string());
            stage(second).unwrap();

            let (proposals, _) = list();
            assert_eq!(proposals.len(), 1);
            assert_eq!(proposals[0].note.as_deref(), Some("second"));
        });
    }

    #[test]
    fn the_queue_refuses_a_proposal_past_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        with_state_home(dir.path(), || {
            for i in 0..MAX_PENDING {
                stage(sample(&format!("user.demo-{i}"))).unwrap();
            }
            let err = stage(sample("user.one-too-many")).unwrap_err();
            assert!(err.contains("already waiting"), "{err}");
            assert_eq!(count(), MAX_PENDING);
        });
    }

    /// The count-then-write in `stage()` is not safe without the lock: two
    /// callers could both read the same under-cap count before either
    /// writes. This hammers it from several threads at once - a stand-in
    /// for the separate `agent run` processes the real race is between -
    /// and the cap must still hold exactly.
    #[test]
    fn the_cap_holds_under_concurrent_staging() {
        let dir = tempfile::tempdir().unwrap();
        with_state_home(dir.path(), || {
            for i in 0..MAX_PENDING - 4 {
                stage(sample(&format!("user.demo-{i}"))).unwrap();
            }

            std::thread::scope(|scope| {
                for i in 0..8 {
                    scope.spawn(move || {
                        let _ = stage(sample(&format!("user.race-{i}")));
                    });
                }
            });

            assert!(
                count() <= MAX_PENDING,
                "the cap must hold even when racers overlap: {}",
                count()
            );
        });
    }

    #[test]
    fn an_unreadable_proposal_is_listed_as_broken_and_never_silently_dropped() {
        let dir = tempfile::tempdir().unwrap();
        with_state_home(dir.path(), || {
            std::fs::create_dir_all(super::dir()).unwrap();
            std::fs::write(super::dir().join("junk.json"), "{ not json").unwrap();

            let (proposals, broken) = list();
            assert!(proposals.is_empty());
            assert_eq!(broken.len(), 1);
            assert!(broken[0].path.ends_with("junk.json"));
        });
    }

    /// A file this shell does not understand must not be rewritten out from
    /// under a newer one, the same rule `usage` and `trust` state follow.
    #[test]
    fn a_proposal_from_a_future_version_is_reported_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        with_state_home(dir.path(), || {
            std::fs::create_dir_all(super::dir()).unwrap();
            let path = super::dir().join("user.demo.json");
            let original = r#"{"version":99,"id":"user.demo"}"#;
            std::fs::write(&path, original).unwrap();

            let (proposals, broken) = list();
            assert!(proposals.is_empty());
            assert_eq!(broken.len(), 1);

            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        });
    }

    #[test]
    fn scope_and_name_determine_the_id_and_a_bundled_file_is_appended() {
        assert_eq!(
            proposal_id(SkillScope::User, "demo", "SKILL.md"),
            "user.demo"
        );
        assert_eq!(proposal_id(SkillScope::User, "demo", ""), "user.demo");
        let id = proposal_id(SkillScope::Project, "demo", "references/api.md");
        assert!(id.starts_with("project.demo.references_api_md-"), "{id}");
    }

    /// `sanitize_id_part` alone is not injective - two different bundled
    /// file names can sanitize to the same text - so the id must not
    /// collide even when that happens.
    #[test]
    fn different_bundled_files_that_sanitize_the_same_way_get_different_ids() {
        let a = proposal_id(SkillScope::Project, "demo", "references/api.md");
        let b = proposal_id(SkillScope::Project, "demo", "references_api_md");
        assert_ne!(a, b);
    }

    #[test]
    fn the_content_digest_changes_with_the_content() {
        assert_eq!(content_digest("a"), content_digest("a"));
        assert_ne!(content_digest("a"), content_digest("b"));
    }
}
