//! `doctor skills`: canonical vs. runtime skill drift, the skills the `!`
//! chat runtime would actually load, project-skill trust, usage, and
//! pending proposals.
use super::*;
use crate::ShellProxy;
use crate::chatgpt::skills::lint::{self, LintLevel};
use crate::chatgpt::skills::usage;
use dsh_types::Context;
use std::fs;
use std::path::Path;

pub(super) const CODEX_CORE_SKILLS: &[&str] = &["doge-shell-repo"];
pub(super) const DOGESH_COMMON_SKILLS: &[&str] = &[
    "doge-shell-repo",
    "doge-shell-validation",
    "doge-shell-investigation",
    "doge-shell-chat-tools",
];

pub(super) fn check_skills(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    // What the chat runtime will actually load, reported first: the drift check
    // below needs this repository, and most shells are not in it.
    report_runtime_skills(ctx, proxy, current_dir);

    let Some(repo_root) = find_repo_root(current_dir) else {
        let _ = ctx.write_stdout("warn repo-root not-found for skill diagnostics");
        return;
    };
    let source_root = repo_root.join("docs").join("ai").join("skills");
    if !source_root.is_dir() {
        let _ = ctx.write_stdout(&format!(
            "warn canonical-skills missing {}",
            source_root.display()
        ));
        return;
    }

    let canonical_count = count_skill_dirs(&source_root);
    let _ = ctx.write_stdout(&format!(
        "ok canonical-skills {} entries={canonical_count}",
        source_root.display()
    ));

    if let Some(root) = codex_runtime_skills_dir(proxy) {
        check_skill_profile(
            ctx,
            "codex",
            "codex-core",
            &source_root,
            &root,
            CODEX_CORE_SKILLS,
        );
    } else {
        let _ = ctx.write_stdout("warn codex-runtime-skills unable-to-determine-home-dir");
    }

    check_skill_profile(
        ctx,
        "dogesh",
        "dogesh-common",
        &source_root,
        &crate::config_paths::skills_dir(),
        DOGESH_COMMON_SKILLS,
    );

    check_claude_project_skills(ctx, &repo_root, &source_root, canonical_count);
}

/// The skills the `!` runtime would load here, and what they have cost.
///
/// Every summary is in the system prompt on every turn, so a skill nobody reads
/// is a recurring bill rather than a dormant file.
///
/// Reports the project root even when `AI_CHAT_PROJECT_SKILLS` is off, but says
/// so: claiming the runtime loads skills it will never see is worse than not
/// mentioning them.
pub(super) fn report_runtime_skills(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let project_enabled = crate::chatgpt::resolve_project_skills_enabled(proxy);
    let manager = crate::chatgpt::skills::SkillsManager::new(Some(current_dir), true);
    let roots: Vec<crate::chatgpt::skills::SkillRoot> = manager.roots().to_vec();

    for root in &roots {
        let path = &root.path;
        let is_project = root.scope == crate::chatgpt::skills::SkillScope::Project;
        // Per root, not per scope: a project has two, and one label for both
        // would report the shared directory's entry count as the repository's.
        let label = format!("{}-skills", root.label());
        if !path.exists() {
            let _ = ctx.write_stdout(&format!("skip {label} missing {}", path.display()));
        } else if !path.is_dir() {
            // Distinct from "missing": saying missing sends the user looking in
            // the wrong place.
            let _ = ctx.write_stdout(&format!("error {label} not-a-directory {}", path.display()));
        } else if is_project && !project_enabled {
            let _ = ctx.write_stdout(&format!(
                "skip {label} {} entries={} AI_CHAT_PROJECT_SKILLS=off",
                path.display(),
                count_skill_dirs(path)
            ));
        } else {
            let _ = ctx.write_stdout(&format!(
                "ok {label} {} entries={}",
                path.display(),
                count_skill_dirs(path)
            ));
        }
    }

    // The trust decision is what actually decides whether these reach a prompt.
    // One line per root: each is agreed to separately, so one summary would
    // report a repository as trusted while a second directory was not.
    for decision in crate::chatgpt::skills::describe_project_roots(manager.roots()) {
        let trusted =
            crate::chatgpt::skills::trust::is_remembered(&decision.root, &decision.digest);
        let _ = ctx.write_stdout(&format!(
            "{} project-skills-trust {} {} skills={}",
            if trusted { "ok" } else { "warn" },
            if trusted {
                "remembered"
            } else {
                "not-yet-agreed"
            },
            crate::config_paths::display_path(&decision.root),
            decision.names.len()
        ));
    }

    let (skills, problems) = manager.load_reporting();
    for problem in &problems {
        // The root's label, not the scope's: two project roots are trusted
        // separately and edited separately, so one word for both is the
        // ambiguity `label()` was added to remove.
        let label = roots
            .iter()
            .find(|root| problem.path.starts_with(&root.path))
            .map(|root| root.label())
            .unwrap_or_else(|| problem.scope.as_str());
        let _ = ctx.write_stdout(&format!(
            "warn {label}-skill {} {}",
            problem.path.display(),
            problem.problem
        ));
    }
    // The deep pass: read what each loaded skill actually holds. Cheap enough
    // for a `doctor` run (which reads files anyway); too slow to run on every
    // turn, which is why `chat_with_tools` never calls this.
    for skill in &skills {
        let label = roots
            .iter()
            .find(|root| skill.dir().starts_with(&root.path))
            .map(|root| root.label())
            .unwrap_or_else(|| skill.scope.as_str());
        for finding in lint::lint_path(skill.dir(), &skill.name) {
            let severity = match finding.level {
                // A rejection here means `skill_manage` would have refused
                // this exact content; it reached disk some other way (a
                // human edit, or a skill this shell did not write).
                LintLevel::Reject => "error",
                LintLevel::Warn => "warn",
            };
            let _ = ctx.write_stdout(&format!(
                "{severity} {label}-skill {} {}",
                crate::config_paths::display_path(skill.dir()),
                finding.message
            ));
        }
    }

    let records = usage::load();
    let now = usage::now_ms();

    let mut authored = 0usize;
    let mut unused = Vec::new();
    let mut archived = 0usize;
    let mut pinned = 0usize;
    for skill in &skills {
        let record = records.get(&usage::key(skill.dir()));
        if record.is_some_and(|r| r.created_by == "agent") {
            authored += 1;
        }
        let is_archived = usage::is_archived(record);
        if is_archived {
            archived += 1;
        }
        if record.is_some_and(|r| r.pinned) {
            pinned += 1;
        }
        // The same rule `skill list` uses. Two copies disagreed at the boundary,
        // so one command called a skill dead while the other called it healthy.
        // Archived, not unread: it is already out of the prompt on purpose.
        if !is_archived && usage::is_stale(record, now) {
            unused.push(skill.name.clone());
        }
    }

    let _ = ctx.write_stdout(&format!("ok ai-authored-skills {authored}"));
    if unused.is_empty() {
        let _ = ctx.write_stdout("ok unused-skills 0");
    } else {
        let _ = ctx.write_stdout(&format!(
            "warn unused-skills {} not read recently: {}",
            unused.len(),
            unused.join(",")
        ));
    }
    let _ = ctx.write_stdout(&format!("ok archived-skills {archived}"));
    let _ = ctx.write_stdout(&format!("ok pinned-skills {pinned}"));

    let (proposals, broken) = crate::chatgpt::skills::pending::list();
    if proposals.is_empty() {
        // Still printed even when only `broken` is non-empty: a consumer
        // scanning for this line by name must always find one.
        let _ = ctx.write_stdout("ok pending-skills 0");
    } else {
        let names: Vec<&str> = proposals.iter().map(|p| p.name.as_str()).collect();
        let _ = ctx.write_stdout(&format!(
            "warn pending-skills {} awaiting review: {}",
            proposals.len(),
            names.join(",")
        ));
    }
    if !broken.is_empty() {
        let _ = ctx.write_stdout(&format!(
            "warn pending-skills-unreadable {} {}",
            broken.len(),
            broken
                .iter()
                .map(|b| crate::config_paths::display_path(&b.path))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
}

/// `<repo>/.claude/skills` is what Claude Code reads. It is normally a symlink
/// to the canonical source, so every skill is visible with nothing to sync.
pub(super) fn check_claude_project_skills(
    ctx: &Context,
    repo_root: &Path,
    source_root: &Path,
    canonical_count: usize,
) {
    let dest_root = repo_root.join(".claude").join("skills");

    if !dest_root.exists() {
        let _ = ctx.write_stdout(&format!(
            "missing claude-project-skills {}",
            dest_root.display()
        ));
        return;
    }

    if dest_root.is_symlink() {
        match fs::canonicalize(&dest_root) {
            Ok(resolved) if fs::canonicalize(source_root).ok().as_deref() == Some(&resolved) => {
                let _ = ctx.write_stdout(&format!(
                    "ok claude-project-skills symlink -> docs/ai/skills entries={canonical_count}"
                ));
            }
            Ok(resolved) => {
                let _ = ctx.write_stdout(&format!(
                    "warn claude-project-skills symlink points at {}",
                    resolved.display()
                ));
            }
            Err(err) => {
                let _ =
                    ctx.write_stdout(&format!("warn claude-project-skills broken-symlink {err}"));
            }
        }
        return;
    }

    let installed = count_skill_dirs(&dest_root);
    let state = if installed == canonical_count {
        "ok"
    } else {
        "warn"
    };
    let _ = ctx.write_stdout(&format!(
        "{state} claude-project-skills copy entries={installed} canonical={canonical_count}"
    ));
}

pub(super) fn check_skill_profile(
    ctx: &Context,
    target: &str,
    profile: &str,
    source_root: &Path,
    dest_root: &Path,
    expected_skills: &[&str],
) {
    let _ = ctx.write_stdout(&format!(
        "ok {target}-profile {profile} root={}",
        dest_root.display()
    ));

    let mut ok = 0;
    let mut stale = 0;
    let mut missing = 0;
    for skill in expected_skills {
        let source = source_root.join(skill);
        let dest = dest_root.join(skill);
        if !source.is_dir() {
            let _ = ctx.write_stdout(&format!("warn {target} {skill} source-missing"));
            continue;
        }
        if !dest.is_dir() {
            missing += 1;
            let _ = ctx.write_stdout(&format!("missing {target} {skill} -> {}", dest.display()));
        } else if skill_dirs_match(&source, &dest) {
            ok += 1;
            let _ = ctx.write_stdout(&format!("ok {target} {skill} -> {}", dest.display()));
        } else {
            stale += 1;
            let _ = ctx.write_stdout(&format!("stale {target} {skill} -> {}", dest.display()));
        }
    }

    let extra = count_extra_skill_dirs(dest_root, expected_skills);
    if extra > 0 {
        let _ = ctx.write_stdout(&format!(
            "warn {target}-runtime-skills extra entries={extra}"
        ));
    }
    let state = if stale == 0 && missing == 0 {
        "ok"
    } else {
        "warn"
    };
    let _ = ctx.write_stdout(&format!(
        "{state} {target}-runtime-skills summary ok={ok} stale={stale} missing={missing}"
    ));
}
