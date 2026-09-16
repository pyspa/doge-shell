//! The skills the agent can read, and where they live.
//!
//! A skill is a `SKILL.md` (or a bare `*.md`) holding a short, reusable
//! procedure. Only the name, the path and a one-line summary reach the system
//! prompt; the body is read with `read_file` when the model decides the skill is
//! relevant. That keeps the per-turn cost proportional to the number of skills
//! rather than to their length.
//!
//! Skills are **read** from up to three roots and **written** to two.
//!
//! The personal one is the user's own configuration directory. A project can
//! carry its own procedures in `.dogesh/skills`, and - because the same files are
//! useful to whichever agent the user is driving - in the vendor-neutral
//! `.agents/skills` other tools have settled on. More specific wins a name
//! clash, so `.dogesh/skills` shadows `.agents/skills`, which shadows the personal
//! root.
//!
//! `skill_manage` still only ever writes to `.dogesh/skills` or the personal root.
//! `.agents/skills` is shared with other tools, and a directory this shell does
//! not own is not a place for it to leave files.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use tracing::{debug, warn};

mod frontmatter;
pub(crate) mod lint;
mod loader;
mod mentions;
pub(crate) mod pending;
pub(crate) mod trust;
pub(crate) mod usage;

pub(crate) use frontmatter::truncate_chars;
use frontmatter::{extract_skill_summary, frontmatter_field, is_indented, split_frontmatter};
use loader::load_root;
pub(crate) use loader::{SkillsManager, clear_skills_fragment_cache};
pub(crate) use mentions::{note_skill_read, render_mention, split_leading_mentions};

/// FNV-1a over raw bytes, formatted as lowercase hex.
///
/// Deterministic across builds and processes, unlike `DefaultHasher`, whose
/// output is explicitly not stable between Rust releases - a toolchain
/// upgrade would otherwise re-ask about every trusted project. Shared by
/// `trust::digest`, `pending::content_digest` and `usage::lifecycle_digest`
/// so there is exactly one place that formula lives; none of the three may
/// change this output for one input without invalidating the others' stored
/// records.
pub(crate) fn fnv1a_hex(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The most `skill_manage` will ever write into `description:`.
///
/// Shared with `lint`, which enforces it on the final content before a write
/// is asked about, and with `tool/skill.rs`, which enforces it on `create`'s
/// input before `render_skill_md` ever runs.
pub(crate) const MAX_DESCRIPTION_CHARS: usize = 300;

/// Where a skill came from. Declaration order is precedence order.
///
/// Deliberately **still two values** now that a project has two roots. Every
/// `scope == SkillScope::Project` comparison in this crate - the trust gate,
/// `doctor`, `skill_manage` - reads as "did this arrive with the checkout", and
/// a third variant would answer `false` to all of them at once. That is the
/// permissive direction for a gate, and nothing would fail to compile. Which
/// directory it was is `SkillOrigin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SkillScope {
    Project,
    User,
}

/// Which directory of a scope, where a scope has more than one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SkillOrigin {
    /// This shell's own directory: `.dogesh/skills`, or the personal root.
    Dsh,
    /// The cross-agent convention: `<project>/.agents/skills`.
    Agents,
}

impl SkillScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SkillScope::Project => "project",
            SkillScope::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillRoot {
    pub scope: SkillScope,
    pub origin: SkillOrigin,
    pub path: PathBuf,
}

impl SkillRoot {
    /// The prompt heading for this root. On the root rather than on the scope,
    /// because one scope can now hold more than one directory.
    fn heading(&self) -> &'static str {
        match (self.scope, self.origin) {
            (SkillScope::Project, SkillOrigin::Dsh) => "Project skills",
            (SkillScope::Project, SkillOrigin::Agents) => "Shared project skills",
            (SkillScope::User, _) => "Personal skills",
        }
    }

    fn provenance(&self) -> &'static str {
        match (self.scope, self.origin) {
            (SkillScope::Project, SkillOrigin::Dsh) => ", provided by this repository",
            (SkillScope::Project, SkillOrigin::Agents) => {
                ", provided by this repository for any agent"
            }
            (SkillScope::User, _) => "",
        }
    }

    /// The word `doctor` and `skill list` use for this root.
    pub(crate) fn label(&self) -> &'static str {
        match (self.scope, self.origin) {
            (SkillScope::Project, SkillOrigin::Dsh) => "project",
            (SkillScope::Project, SkillOrigin::Agents) => "project-agents",
            (SkillScope::User, _) => "user",
        }
    }
}

/// The project-relative skills directory.
///
/// `.dogesh` rather than `.doge`: the binary, the configuration directory and
/// `config_paths::APP` all spell it `dogesh`.
pub(crate) const PROJECT_SKILLS_DIR: &str = ".dogesh/skills";

/// The cross-agent project skills directory.
///
/// The convention that grew up alongside `AGENTS.md`, so one checkout can carry
/// one set of procedures for every agent working in it instead of one per tool.
pub(crate) const PROJECT_AGENTS_SKILLS_DIR: &str = ".agents/skills";

/// The enclosing project, when `current_dir` is inside one.
///
/// Reuses `workspace_root`, which climbs to the outermost enclosing project and
/// stops short of `$HOME`. Resolving the root a second way is exactly how the
/// skills directory once ended up meaning three different paths on macOS.
fn project_marker_root(current_dir: &Path) -> Option<PathBuf> {
    let root = super::tool::workspace_root(current_dir);
    crate::project_context::has_project_marker(&root).then_some(root)
}

/// The project skills root for `current_dir`, when there is a project at all.
///
/// **This keeps meaning `.dogesh/skills` specifically.** `skill_manage` and
/// `doctor` call it to answer "where would a write go", so leaving it alone is
/// what guarantees the interop root stays read-only.
pub(crate) fn project_skills_root(current_dir: &Path) -> Option<PathBuf> {
    project_marker_root(current_dir).map(|root| root.join(PROJECT_SKILLS_DIR))
}

/// The cross-agent project skills root for `current_dir`.
pub(crate) fn project_agents_skills_root(current_dir: &Path) -> Option<PathBuf> {
    project_marker_root(current_dir).map(|root| root.join(PROJECT_AGENTS_SKILLS_DIR))
}

/// Every root to read skills from, most specific first.
///
/// `allow_project` is the kill switch for `AI_CHAT_PROJECT_SKILLS`: a cloned
/// repository can put text in front of the model just by existing, so turning
/// that off has to be possible without also giving up personal skills.
pub(crate) fn skill_roots(current_dir: Option<&Path>, allow_project: bool) -> Vec<SkillRoot> {
    let mut roots = Vec::with_capacity(3);

    if allow_project && let Some(cwd) = current_dir {
        // `.dogesh` first: the tool-specific answer beats the shared one.
        if let Some(path) = project_skills_root(cwd) {
            roots.push(SkillRoot {
                scope: SkillScope::Project,
                origin: SkillOrigin::Dsh,
                path,
            });
        }
        if let Some(path) = project_agents_skills_root(cwd) {
            roots.push(SkillRoot {
                scope: SkillScope::Project,
                origin: SkillOrigin::Agents,
                path,
            });
        }
    }

    roots.push(SkillRoot {
        scope: SkillScope::User,
        origin: SkillOrigin::Dsh,
        path: crate::config_paths::skills_dir(),
    });

    dedupe_roots(roots)
}

/// Drop roots that are the same directory reached two ways.
///
/// `.agents/skills` symlinked to `.dogesh/skills` is a reasonable thing for a
/// repository to do, and without this it would list every skill twice and ask
/// for trust twice for one set of files.
fn dedupe_roots(roots: Vec<SkillRoot>) -> Vec<SkillRoot> {
    let mut seen = BTreeSet::new();
    roots
        .into_iter()
        .filter(|root| {
            let key = std::fs::canonicalize(&root.path)
                .unwrap_or_else(|_| super::tool::normalize_path(&root.path));
            seen.insert(key)
        })
        .collect()
}

/// What a project root's skills are, and whether the user has agreed to them.
pub(crate) struct ProjectSkillDecision {
    pub root: PathBuf,
    pub names: Vec<String>,
    pub digest: String,
}

/// Every project root that has something to decide about, in `roots` order.
///
/// A project skills directory arrives with a `git clone`, and its descriptions
/// enter the system prompt before the user has decided anything. This is the
/// same bar `.dogesh/hooks.json` is held to.
///
/// **A `Vec`, not an `Option`.** The single-root version took
/// `roots.iter().find(scope == Project)`, so a repository whose first project
/// root was empty answered "nothing to decide" while a second one still had
/// descriptions to contribute - the gate passing over untrusted text without
/// asking. Returning every root makes the caller handle each, and changing the
/// signature is what forced every existing caller to be re-read.
pub(crate) fn describe_project_roots(roots: &[SkillRoot]) -> Vec<ProjectSkillDecision> {
    roots
        .iter()
        .filter(|root| root.scope == SkillScope::Project)
        .filter_map(|root| {
            let skills = load_root(root);
            // Nothing to advertise, nothing to decide.
            if skills.is_empty() {
                return None;
            }
            Some(ProjectSkillDecision {
                digest: trust::digest(&skills),
                names: skills.into_iter().map(|skill| skill.name).collect(),
                root: root.path.clone(),
            })
        })
        .collect()
}

/// The roots as they appear on disk, so a `starts_with` against a canonicalised
/// tool path actually matches.
fn resolved_roots(current_dir: &Path) -> Vec<SkillRoot> {
    skill_roots(Some(current_dir), true)
        .into_iter()
        .map(|root| SkillRoot {
            path: std::fs::canonicalize(&root.path).unwrap_or(root.path),
            ..root
        })
        .collect()
}

fn containing_skill_in(roots: &[SkillRoot], path: &Path) -> Option<(PathBuf, SkillScope)> {
    for root in roots {
        let Ok(rest) = path.strip_prefix(&root.path) else {
            continue;
        };
        let Some(first) = rest.components().next() else {
            continue;
        };
        return Some((root.path.join(first), root.scope));
    }

    None
}

/// The skill directory (or bare `*.md` file) that owns `path`, if any.
///
/// Used both to attribute a `read_file` to a skill and to recognise a path as
/// belonging to a skill root at all.
pub(crate) fn containing_skill(path: &Path, current_dir: &Path) -> Option<(PathBuf, SkillScope)> {
    containing_skill_in(&resolved_roots(current_dir), path)
}

/// Is `path` inside one of the skill roots?
pub(crate) fn is_within_skill_root(path: &Path, current_dir: &Path) -> bool {
    resolved_roots(current_dir)
        .iter()
        .any(|root| path.starts_with(&root.path))
}

/// Something wrong with a skill on disk, kept so `doctor` can say it out loud.
///
/// These used to go to `debug!` and vanish, which meant the answer to "why is
/// the skill I wrote not in the prompt?" was unobtainable. The Agent Skills
/// implementation guide asks for exactly this: read leniently, record what was
/// wrong, and give the user somewhere to see it.
#[derive(Debug, Clone)]
pub(crate) struct SkillDiagnostic {
    pub scope: SkillScope,
    pub path: PathBuf,
    pub problem: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Skill {
    pub name: String,
    pub scope: SkillScope,
    /// Collapsed to one line, not yet truncated for the prompt. `summary()`
    /// computes the truncated form on demand rather than caching a second
    /// copy - see `raw_summary()` for why the untruncated form is what the
    /// trust digest keys on.
    raw_summary: String,
    /// The file the model should read to get the skill, ready to display.
    instruction_path: String,
    /// The directory (or bare file) that is the unit of bookkeeping.
    dir: PathBuf,
    /// The root this skill was loaded from.
    ///
    /// Grouping the prompt by `scope` worked only while one scope meant one
    /// directory. It no longer does, and a scope-keyed filter renders the same
    /// skills once per root of that scope.
    root: PathBuf,
    /// `name:` as written in the frontmatter, when it was written at all.
    declared_name: Option<String>,
    /// Whether the summary came from `description:` rather than from the body.
    has_description: bool,
}

impl Skill {
    pub(crate) fn from_folder(path: &Path, root: &SkillRoot) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let skill_md_path = path.join("SKILL.md");
        if !skill_md_path.exists() {
            anyhow::bail!("SKILL.md not found in folder: {:?}", path);
        }

        let content = std::fs::read_to_string(&skill_md_path)
            .with_context(|| format!("Failed to read SKILL.md: {skill_md_path:?}"))?;

        debug!("Loaded folder skill: {}", name);

        Ok(Self::from_content(
            name,
            content,
            &skill_md_path,
            path.to_path_buf(),
            root,
        ))
    }

    pub(crate) fn from_file(path: &Path, root: &SkillRoot) -> Result<Self> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read skill file: {path:?}"))?;

        debug!("Loaded file skill: {}", name);

        Ok(Self::from_content(
            name,
            content,
            path,
            path.to_path_buf(),
            root,
        ))
    }

    fn from_content(
        name: String,
        instruction: String,
        path: &Path,
        dir: PathBuf,
        root: &SkillRoot,
    ) -> Self {
        let (frontmatter, _) = split_frontmatter(&instruction);
        let declared_name = frontmatter_field(frontmatter, "name");
        let declared_description = frontmatter_field(frontmatter, "description");
        let raw_summary = extract_skill_summary(&instruction);

        Self {
            name,
            scope: root.scope,
            raw_summary,
            instruction_path: crate::config_paths::display_path(path),
            dir,
            root: root.path.clone(),
            declared_name,
            has_description: declared_description.is_some(),
        }
    }

    /// What the prompt shows: collapsed to one line, truncated to
    /// `MAX_SKILL_SUMMARY_CHARS`.
    pub(crate) fn summary(&self) -> String {
        truncate_chars(&self.raw_summary, MAX_SKILL_SUMMARY_CHARS)
    }

    /// The same summary before the prompt's display budget truncates it.
    ///
    /// Trust is about *what a root would put in the prompt if the budget were
    /// unbounded* - the pair the user is agreeing to - not about how much of
    /// it fits on a given day. Keying the digest on `summary()` instead would
    /// mean every future change to `MAX_SKILL_SUMMARY_CHARS` re-asks every
    /// trusted project once, for a reason that has nothing to do with what
    /// changed in that project.
    pub(crate) fn raw_summary(&self) -> &str {
        &self.raw_summary
    }

    /// Where to read this skill from, as the prompt should spell it.
    pub(crate) fn instruction_path(&self) -> &str {
        &self.instruction_path
    }

    /// The directory the usage record is keyed on.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The root this skill came from, which is what the prompt groups by.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The file the prompt points at: `SKILL.md`, or the bare `*.md` itself.
    pub(crate) fn instruction_file(&self) -> PathBuf {
        if self.dir.is_dir() {
            self.dir.join("SKILL.md")
        } else {
            self.dir.clone()
        }
    }
}

/// The longest canonical skill in this repository (`doge-shell-completion-spec`)
/// runs to 234 characters; 140 cut its trigger mid-sentence. Chosen with room
/// to spare rather than tuned to that one file.
pub(crate) const MAX_SKILL_SUMMARY_CHARS: usize = 240;

#[cfg(test)]
mod tests;
