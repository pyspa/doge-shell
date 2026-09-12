//! `skill` - see and prune the skills the agent reads.
//!
//! The counterpart to the `skill_manage` tool. The agent writes skills; this is
//! how a person finds out what it wrote, how often each one has actually been
//! opened, and which ones are dead weight in every prompt.
//!
//! Distinct from `doctor skills`, which compares this repository's canonical
//! skills against what the installer placed in a runtime directory. That is a
//! drift check, not a way to manage what the shell reads.

use crate::ShellProxy;
use crate::chatgpt::skills::pending::{self, Proposal};
use crate::chatgpt::skills::usage;
use crate::chatgpt::skills::{self, Skill, SkillsManager};
use crate::chatgpt::tool::skill as skill_tool;
use crate::config_paths::display_path;
use dsh_types::{Context, ExitStatus};
use serde_json::json;
use std::path::PathBuf;

/// Every skill this shell would load here, as `(name, summary)`.
///
/// Exported for completion. A skill name is chosen by the model, so until this
/// existed the only way to learn one was to run `skill list` and read it back -
/// worse than the snippet and bookmark names a person picked themselves.
///
/// Both roots regardless of `AI_CHAT_PROJECT_SKILLS`: completing a name is not
/// the same as putting it in a prompt, and `skill show` reaches either way.
pub fn installed_names(current_dir: Option<&std::path::Path>) -> Vec<(String, String)> {
    SkillsManager::new(current_dir, true)
        .load_skills()
        .into_iter()
        .map(|skill| {
            let summary = skill.summary().to_string();
            (skill.name, summary)
        })
        .collect()
}

/// Every staged proposal, as `(id, "<origin> <action> <name>")`.
///
/// Exported for completion, mirroring `installed_names` - a proposal id is
/// generated (or model-chosen), not typed by a person.
pub fn pending_proposal_ids() -> Vec<(String, String)> {
    pending::list()
        .0
        .into_iter()
        .map(|proposal| {
            let summary = format!("{} {} {}", proposal.origin, proposal.action, proposal.name);
            (proposal.id, summary)
        })
        .collect()
}

pub fn description() -> &'static str {
    "List, review and manage the skills the AI chat runtime reads"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();

    match args.split_first() {
        None | Some((&"list", [])) => list(ctx, proxy),
        Some((&"list", _)) => usage_error(ctx, "Usage: skill list"),
        Some((&"show", [name])) => show(ctx, proxy, name),
        Some((&"path", [name])) => path(ctx, proxy, name),
        Some((&"remove", [name])) | Some((&"rm", [name])) => remove(ctx, proxy, name),
        Some((&"trust", [])) => trust_status(ctx, proxy),
        Some((&"trust", _)) => usage_error(ctx, "Usage: skill trust"),
        Some((&"untrust", [])) => untrust(ctx, proxy),
        Some((&"untrust", _)) => usage_error(ctx, "Usage: skill untrust"),
        Some((&"pending", [])) => pending_cmd(ctx),
        Some((&"pending", _)) => usage_error(ctx, "Usage: skill pending"),
        Some((&"diff", [id])) => diff(ctx, proxy, id),
        Some((&"diff", _)) => usage_error(ctx, "Usage: skill diff <id>"),
        Some((&"approve", [id])) => approve(ctx, proxy, id),
        Some((&"approve", _)) => usage_error(ctx, "Usage: skill approve <id>"),
        Some((&"reject", [id])) => reject(ctx, proxy, id),
        Some((&"reject", _)) => usage_error(ctx, "Usage: skill reject <id>"),
        Some((&"archive", [name])) => archive(ctx, proxy, name),
        Some((&"archive", _)) => usage_error(ctx, "Usage: skill archive <name>"),
        Some((&"unarchive", [name])) => unarchive(ctx, proxy, name),
        Some((&"unarchive", _)) => usage_error(ctx, "Usage: skill unarchive <name>"),
        Some((&"pin", [name])) => pin(ctx, proxy, name),
        Some((&"pin", _)) => usage_error(ctx, "Usage: skill pin <name>"),
        Some((&"unpin", [name])) => unpin(ctx, proxy, name),
        Some((&"unpin", _)) => usage_error(ctx, "Usage: skill unpin <name>"),
        Some((&"help", _)) | Some((&"-h", _)) | Some((&"--help", _)) => {
            print_help(ctx);
            ExitStatus::ExitedWith(0)
        }
        Some((unknown, _)) => usage_error(ctx, &format!("skill: unknown subcommand `{unknown}`")),
    }
}

fn print_help(ctx: &Context) {
    let _ = ctx.write_stdout(
        "skill - inspect and manage the skills the AI chat runtime reads\n\
         \n\
         Usage: skill <subcommand> [name]\n\
         \n\
         Subcommands:\n  \
           list             Every skill, with scope, read count and last use\n  \
           show <name>      Render a skill's SKILL.md\n  \
           path <name>      Print its path, for `$EDITOR $(skill path <name>)`\n  \
           remove <name>    Delete a skill, after confirmation\n  \
           trust            Show whether this repository's skills are trusted\n  \
           untrust          Forget that decision for this repository\n  \
           pending          List skill changes staged for review\n  \
           diff <id>        Show what a staged change would write\n  \
           approve <id>     Apply a staged change, after confirmation\n  \
           reject <id>      Discard a staged change, after confirmation\n  \
           archive <name>   Hide a personal skill from the prompt without deleting it\n  \
           unarchive <name> Bring an archived personal skill back\n  \
           pin <name>       Exempt a personal skill from auto-archiving\n  \
           unpin <name>     Undo `pin`\n\
         \n\
         Project skills come from `.dsh/skills` in the enclosing project and take\n\
         precedence over personal ones of the same name. They are only read once\n\
         you have agreed to them, because their descriptions go into every prompt.\n\
         \n\
         With `AI_CHAT_SKILL_STAGING` set, `skill_manage` writes are queued here\n\
         instead of landing immediately; `pending`/`diff`/`approve`/`reject` review them.",
    );
}

fn usage_error(ctx: &Context, message: &str) -> ExitStatus {
    let _ = ctx.write_stderr(message);
    ExitStatus::ExitedWith(1)
}

fn manager(proxy: &mut dyn ShellProxy) -> SkillsManager {
    // Always both roots here: this command is how a person audits what is
    // installed, including skills the prompt is currently configured to skip.
    let cwd = proxy.get_current_dir().ok();
    SkillsManager::new(cwd.as_deref(), true)
}

fn find(proxy: &mut dyn ShellProxy, name: &str) -> Option<Skill> {
    manager(proxy)
        .load_skills()
        .into_iter()
        .find(|skill| skill.name == name)
}

fn list(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let manager = manager(proxy);
    let roots: Vec<skills::SkillRoot> = manager.roots().to_vec();
    let skills = manager.load_skills();
    let records = usage::load();
    let now = usage::now_ms();

    if skills.is_empty() {
        let _ = ctx.write_stdout("No skills yet.");
        for root in &roots {
            let _ = ctx.write_stdout(&format!("  {}: {}", root.label(), display_path(&root.path)));
        }
        let _ =
            ctx.write_stdout("Ask the assistant to save one, or write a SKILL.md there by hand.");
        return ExitStatus::ExitedWith(0);
    }

    let mut unused = Vec::new();
    let mut archived = Vec::new();
    for skill in &skills {
        let record = records.get(&usage::key(skill.dir()));
        let reads = record.map(|r| r.reads).unwrap_or(0);
        let last = record.map(|r| r.last_read_ms).unwrap_or(0);
        let author = record
            .map(|r| r.created_by.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("user");
        let is_archived = usage::is_archived(record);
        let is_pinned = record.is_some_and(|r| r.pinned);

        // Archived, not unread: it is already out of the prompt on purpose,
        // so flagging it again as "unread" would be the same warning twice.
        if !is_archived && usage::is_stale(record, now) {
            unused.push(skill.name.as_str());
        }
        if is_archived {
            archived.push(skill.name.as_str());
        }

        // The root's label, not the scope's: a project has two roots, and
        // printing `project` for both hides which directory to edit.
        let label = roots
            .iter()
            .find(|root| root.path == skill.root())
            .map(|root| root.label())
            .unwrap_or_else(|| skill.scope.as_str());
        let marker = match (is_archived, is_pinned) {
            (true, true) => " [archived,pinned]",
            (true, false) => " [archived]",
            (false, true) => " [pinned]",
            (false, false) => "",
        };
        let _ = ctx.write_stdout(&format!(
            "{:<15} {:<28} reads={:<5} last={:<12} by={:<6} {}{marker}",
            label,
            skill.name,
            reads,
            describe_age(now, last),
            author,
            display_summary(&skill.summary())
        ));
    }

    if !unused.is_empty() {
        // Every summary is in the system prompt on every turn, so an unread
        // skill is a recurring cost rather than a dormant file.
        let _ = ctx.write_stdout(&format!(
            "\n{} unread in the last {} days: {}",
            unused.len(),
            usage::UNUSED_AFTER_DAYS,
            unused.join(", ")
        ));
    }
    if !archived.is_empty() {
        let _ = ctx.write_stdout(&format!(
            "{} archived (not in the prompt): {}",
            archived.len(),
            archived.join(", ")
        ));
    }
    let staged = pending::count();
    if staged > 0 {
        let _ = ctx.write_stdout(&format!(
            "{staged} skill change(s) waiting for review: run `skill pending`"
        ));
    }

    ExitStatus::ExitedWith(0)
}

fn show(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };

    let path = instruction_file(&skill);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let _ = ctx.write_stdout(&crate::markdown::render_markdown_with_fallback(&contents));
            ExitStatus::ExitedWith(0)
        }
        Err(err) => usage_error(
            ctx,
            &format!("skill: failed to read {}: {err}", display_path(&path)),
        ),
    }
}

fn path(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };

    let _ = ctx.write_stdout(&instruction_file(&skill).display().to_string());
    ExitStatus::ExitedWith(0)
}

fn remove(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };

    let target = skill.dir().to_path_buf();
    let confirmed = proxy
        .confirm_action(&format!(
            "Delete the {} skill `{}` at {}?",
            skill.scope.as_str(),
            skill.name,
            display_path(&target)
        ))
        .unwrap_or(false);
    if !confirmed {
        let _ = ctx.write_stdout("Cancelled.");
        return ExitStatus::ExitedWith(1);
    }

    let removed = if target.is_dir() {
        std::fs::remove_dir_all(&target)
    } else {
        std::fs::remove_file(&target)
    };

    match removed {
        Ok(()) => {
            usage::forget(&target);
            skills::clear_skills_fragment_cache();
            let _ = ctx.write_stdout(&format!("Removed {}", display_path(&target)));
            ExitStatus::ExitedWith(0)
        }
        Err(err) => usage_error(
            ctx,
            &format!("skill: failed to remove {}: {err}", display_path(&target)),
        ),
    }
}

fn pending_cmd(ctx: &Context) -> ExitStatus {
    let (proposals, broken) = pending::list();
    if proposals.is_empty() && broken.is_empty() {
        let _ = ctx.write_stdout("No skill changes waiting for review.");
        return ExitStatus::ExitedWith(0);
    }

    for proposal in &proposals {
        let note = proposal
            .note
            .as_deref()
            .map(|note| format!("  # {}", sanitize_display(note)))
            .unwrap_or_default();
        let _ = ctx.write_stdout(&format!(
            "{:<26} {:<10} {:<10} {:<8} {}{}",
            proposal.id, proposal.origin, proposal.action, proposal.scope, proposal.name, note
        ));
    }
    for broken in &broken {
        let _ = ctx.write_stdout(&format!(
            "unreadable                 {} ({})",
            display_path(&broken.path),
            broken.reason
        ));
    }
    let _ = ctx.write_stdout(
        "\nReview one with `skill diff <id>`, then `skill approve <id>` or `skill reject <id>`.",
    );
    ExitStatus::ExitedWith(0)
}

/// Resolve a staged proposal back to the `Request` its target path, scope
/// and skill directory - the same checks `skill_manage` itself runs - so
/// `diff`/`approve` never duplicate path resolution.
fn resolve_proposal(
    proxy: &mut dyn ShellProxy,
    proposal: &Proposal,
) -> Result<skill_tool::Request, String> {
    let cwd = proxy
        .get_current_dir()
        .map_err(|err| format!("skill: failed to get current directory: {err}"))?;
    let args = json!({
        "action": proposal.action,
        "name": proposal.name,
        "scope": proposal.scope,
        "file": proposal.file,
    });
    skill_tool::validate_in(&args, &cwd)
}

/// Whether a project proposal's staged root still matches where it would
/// resolve today - the guard against a proposal staged in one repository
/// landing in another one just because both happen to be named the same.
fn project_root_matches(request: &skill_tool::Request, proposal: &Proposal) -> bool {
    if request.scope != skills::SkillScope::Project {
        return true;
    }
    request
        .skill_dir
        .parent()
        .zip(proposal.project_root.as_deref())
        .is_some_and(|(actual, expected)| actual == expected)
}

fn diff(ctx: &Context, proxy: &mut dyn ShellProxy, id: &str) -> ExitStatus {
    let proposal = match pending::find(id) {
        Ok(proposal) => proposal,
        Err(err) => return usage_error(ctx, &err),
    };
    let request = match resolve_proposal(proxy, &proposal) {
        Ok(request) => request,
        Err(err) => return usage_error(ctx, &err),
    };

    let _ = ctx.write_stdout(&format!(
        "{} {} `{}` ({} scope, staged {})",
        proposal.origin,
        proposal.action,
        proposal.name,
        proposal.scope,
        describe_age(usage::now_ms(), proposal.created_ms)
    ));
    if !project_root_matches(&request, &proposal) {
        let _ = ctx.write_stdout(
            "warning: this repository is not where the proposal was staged; `approve` will refuse it.",
        );
    }
    if let Some(note) = &proposal.note {
        let _ = ctx.write_stdout(&format!("# {}", sanitize_display(note)));
    }

    match std::fs::read_to_string(&request.target) {
        Ok(before) => {
            let _ = ctx.write_stdout(&sanitize_display(&unified_lines(
                &before,
                &proposal.contents,
            )));
        }
        Err(_) => {
            let _ = ctx.write_stdout(&sanitize_display(&proposal.contents));
        }
    }
    ExitStatus::ExitedWith(0)
}

fn approve(ctx: &Context, proxy: &mut dyn ShellProxy, id: &str) -> ExitStatus {
    let proposal = match pending::find(id) {
        Ok(proposal) => proposal,
        Err(err) => return usage_error(ctx, &err),
    };
    let request = match resolve_proposal(proxy, &proposal) {
        Ok(request) => request,
        Err(err) => return usage_error(ctx, &err),
    };
    if !project_root_matches(&request, &proposal) {
        return usage_error(
            ctx,
            "skill: this proposal was staged in a different repository; `skill reject` it and ask again from there",
        );
    }

    // The same name-collision guard `create()` itself runs before writing:
    // the staleness check just below only looks at `request.target`, which
    // does not exist either way for a brand-new skill, so it cannot see a
    // *sibling* collision (a `<name>.md` file-skill, or the directory
    // itself appearing through some other route) that showed up after this
    // was staged.
    if proposal.action == "create"
        && let Err(err) = skill_tool::reject_create_collision(&request)
    {
        return usage_error(ctx, &err);
    }

    // Refuse a target that moved since this was staged, rather than clobber
    // it: `None` means "must not exist yet" (a `create`), `Some(digest)`
    // means "must still read exactly this" (`write_file`/`patch`).
    let current_digest = std::fs::read_to_string(&request.target)
        .ok()
        .map(|contents| pending::content_digest(&contents));
    if current_digest != proposal.base_digest {
        return usage_error(
            ctx,
            "skill: the target changed since this was staged; `skill reject` it and ask again",
        );
    }

    let confirmed = proxy
        .confirm_action(&format!(
            "Apply the staged {} change to the {} skill `{}` at {}?",
            proposal.action,
            request.scope.as_str(),
            request.name,
            display_path(&request.target)
        ))
        .unwrap_or(false);
    if !confirmed {
        let _ = ctx.write_stdout("Cancelled.");
        return ExitStatus::ExitedWith(1);
    }

    let warnings = match skill_tool::apply_skill_write(
        &skill_tool::SkillWrite {
            name: &request.name,
            scope: request.scope,
            skill_dir: &request.skill_dir,
            target: &request.target,
            relative_file: &request.relative_file,
            created: proposal.action == "create",
            // `skill approve` runs from the interactive CLI, never under an
            // agent task, so the plain writer (not the symlink-safe one a
            // task's tool call needs) is always correct here.
            symlink_safe: false,
        },
        &proposal.contents,
    ) {
        Ok(warnings) => warnings,
        Err(err) => return usage_error(ctx, &err),
    };

    pending::remove(&proposal.id);
    // `apply_skill_write` only buffers the usage counters - inside `!` chat
    // they ride out to disk on the turn's own `usage::flush()`, but `skill
    // approve` runs as a standalone command with no turn around it, so
    // without this the attribution above is lost the moment the process
    // that ran it exits.
    usage::flush();
    let _ = ctx.write_stdout(&format!("Applied {}", display_path(&request.target)));
    for warning in &warnings {
        let _ = ctx.write_stdout(&format!("warning: {warning}"));
    }
    ExitStatus::ExitedWith(0)
}

fn reject(ctx: &Context, proxy: &mut dyn ShellProxy, id: &str) -> ExitStatus {
    let proposal = match pending::find(id) {
        Ok(proposal) => proposal,
        Err(err) => return usage_error(ctx, &err),
    };
    let confirmed = proxy
        .confirm_action(&format!(
            "Discard the staged {} change to the {} skill `{}`?",
            proposal.action, proposal.scope, proposal.name
        ))
        .unwrap_or(false);
    if !confirmed {
        let _ = ctx.write_stdout("Cancelled.");
        return ExitStatus::ExitedWith(1);
    }
    pending::remove(&proposal.id);
    let _ = ctx.write_stdout(&format!("Rejected {}", proposal.id));
    ExitStatus::ExitedWith(0)
}

fn archive(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };
    if skill.scope != skills::SkillScope::User {
        return usage_error(
            ctx,
            "skill: only personal (user-scope) skills can be archived; a project's skills belong to the repository, not to this shell",
        );
    }
    match usage::set_archived(skill.dir(), true) {
        Ok(true) => {
            skills::clear_skills_fragment_cache();
            let _ = ctx.write_stdout(&format!(
                "Archived `{name}`; it stays out of the prompt until `skill unarchive {name}`."
            ));
            ExitStatus::ExitedWith(0)
        }
        Ok(false) => version_mismatch(ctx),
        Err(err) => usage_error(ctx, &format!("skill: failed to archive `{name}`: {err}")),
    }
}

fn unarchive(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };
    match usage::set_archived(skill.dir(), false) {
        Ok(true) => {
            skills::clear_skills_fragment_cache();
            let _ = ctx.write_stdout(&format!("Unarchived `{name}`."));
            ExitStatus::ExitedWith(0)
        }
        Ok(false) => version_mismatch(ctx),
        Err(err) => usage_error(ctx, &format!("skill: failed to unarchive `{name}`: {err}")),
    }
}

fn pin(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };
    // Same bar as `archive`: pinning only means anything for the skills
    // auto-archive can touch (`user` scope), and without this a project
    // skill silently got a `pinned` flag that would never do anything.
    if skill.scope != skills::SkillScope::User {
        return usage_error(
            ctx,
            "skill: only personal (user-scope) skills can be pinned; a project's skills are never auto-archived to begin with",
        );
    }
    match usage::set_pinned(skill.dir(), true) {
        Ok(true) => {
            let _ = ctx.write_stdout(&format!("Pinned `{name}`; it is never auto-archived."));
            ExitStatus::ExitedWith(0)
        }
        Ok(false) => version_mismatch(ctx),
        Err(err) => usage_error(ctx, &format!("skill: failed to pin `{name}`: {err}")),
    }
}

fn unpin(ctx: &Context, proxy: &mut dyn ShellProxy, name: &str) -> ExitStatus {
    let Some(skill) = find(proxy, name) else {
        return not_found(ctx, name);
    };
    if skill.scope != skills::SkillScope::User {
        return usage_error(
            ctx,
            "skill: only personal (user-scope) skills can be pinned; a project's skills are never auto-archived to begin with",
        );
    }
    match usage::set_pinned(skill.dir(), false) {
        Ok(true) => {
            let _ = ctx.write_stdout(&format!("Unpinned `{name}`."));
            ExitStatus::ExitedWith(0)
        }
        Ok(false) => version_mismatch(ctx),
        Err(err) => usage_error(ctx, &format!("skill: failed to unpin `{name}`: {err}")),
    }
}

fn version_mismatch(ctx: &Context) -> ExitStatus {
    usage_error(
        ctx,
        "skill: the usage state file is a newer version this shell does not understand; nothing was changed",
    )
}

/// Strip control characters (`\n`/`\t` excepted) before showing text that
/// came from a model, not from a person.
fn sanitize_display(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
        .collect()
}

/// Above this many lines on either side, the LCS table below (`O(n*m)`
/// `usize` cells) stops being a review command's problem to allocate.
/// A skill body this large was never going to be reviewed line by line
/// anyway; 2000 lines is already generous headroom over the handful a
/// `SKILL.md` normally holds.
const MAX_DIFF_LINES: usize = 2000;

/// A minimal line diff: shared lines once, `old`-only lines prefixed `-`,
/// `new`-only lines prefixed `+`. Not a real diff algorithm - a proposal is
/// SKILL.md-sized, and pulling in a dependency for this would cost more than
/// it saves.
///
/// Falls back to showing the new content whole past `MAX_DIFF_LINES` on
/// either side: the target on disk has no size cap of its own (unlike a
/// staged proposal's body, which `skill_manage`'s lint already bounds), so
/// without this an unusually large file made the O(n*m) table below - not
/// just this function's input - the thing that could hang or OOM the shell.
fn unified_lines(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let (n, m) = (old_lines.len(), new_lines.len());

    if n > MAX_DIFF_LINES || m > MAX_DIFF_LINES {
        return format!(
            "(too large to diff line by line: {n} existing lines vs {m} proposed lines; showing the proposed content in full)\n\n{new}"
        );
    }

    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = String::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            out.push_str("  ");
            out.push_str(old_lines[i]);
            out.push('\n');
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push_str("- ");
            out.push_str(old_lines[i]);
            out.push('\n');
            i += 1;
        } else {
            out.push_str("+ ");
            out.push_str(new_lines[j]);
            out.push('\n');
            j += 1;
        }
    }
    for line in &old_lines[i..] {
        out.push_str("- ");
        out.push_str(line);
        out.push('\n');
    }
    for line in &new_lines[j..] {
        out.push_str("+ ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Every project root this shell would ask about.
fn project_decisions(proxy: &mut dyn ShellProxy) -> Vec<skills::ProjectSkillDecision> {
    let Ok(cwd) = proxy.get_current_dir() else {
        return Vec::new();
    };
    skills::describe_project_roots(&skills::skill_roots(Some(&cwd), true))
}

fn trust_status(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let decisions = project_decisions(proxy);
    if decisions.is_empty() {
        let _ = ctx.write_stdout("No project skills here.");
        return ExitStatus::ExitedWith(0);
    }

    let mut any_untrusted = false;
    for decision in &decisions {
        let remembered = skills::trust::is_remembered(&decision.root, &decision.digest);
        any_untrusted |= !remembered;
        let _ = ctx.write_stdout(&format!(
            "{} {} ({} skill(s): {})",
            if remembered { "trusted" } else { "untrusted" },
            display_path(&decision.root),
            decision.names.len(),
            decision.names.join(", ")
        ));
    }
    if any_untrusted {
        let _ = ctx.write_stdout(
            "The next `!` here asks before reading them. Answer `a` to remember this repository.",
        );
    }
    ExitStatus::ExitedWith(0)
}

fn untrust(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let decisions = project_decisions(proxy);
    if decisions.is_empty() {
        let _ = ctx.write_stdout("No project skills here.");
        return ExitStatus::ExitedWith(0);
    }

    for decision in &decisions {
        if skills::trust::forget(&decision.root) {
            let _ = ctx.write_stdout(&format!("Forgot {}", display_path(&decision.root)));
        } else {
            let _ = ctx.write_stdout(&format!("{} was not trusted", display_path(&decision.root)));
        }
    }
    ExitStatus::ExitedWith(0)
}

fn not_found(ctx: &Context, name: &str) -> ExitStatus {
    usage_error(
        ctx,
        &format!("skill: no skill named `{name}`. Run `skill list` to see what is installed."),
    )
}

/// The file the prompt points the model at: `SKILL.md`, or the bare `*.md`.
fn instruction_file(skill: &Skill) -> PathBuf {
    if skill.dir().is_dir() {
        skill.dir().join("SKILL.md")
    } else {
        skill.dir().to_path_buf()
    }
}

/// The prompt's display budget and a terminal's column budget are different
/// constraints; this is the second one, independent of
/// `MAX_SKILL_SUMMARY_CHARS`.
const MAX_LIST_SUMMARY_CHARS: usize = 100;

fn display_summary(summary: &str) -> String {
    skills::truncate_chars(summary, MAX_LIST_SUMMARY_CHARS)
}

fn describe_age(now_ms: u64, then_ms: u64) -> String {
    match usage::days_since(now_ms, then_ms) {
        None => "never".to_string(),
        Some(0) => "today".to_string(),
        Some(1) => "1 day ago".to_string(),
        Some(days) => format!("{days} days ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use dsh_types::Context;
    use std::path::Path;
    use tempfile::tempdir;

    /// The crate-wide lock, shared with every other module that scopes
    /// `XDG_CONFIG_HOME`/`XDG_STATE_HOME` for a test, so two test threads
    /// never race to set the same environment variable.
    fn with_homes<R>(config: &Path, state: &Path, f: impl FnOnce() -> R) -> R {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let prev_config = std::env::var_os("XDG_CONFIG_HOME");
        let prev_state = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: single-threaded under `env_lock`.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", config);
            std::env::set_var("XDG_STATE_HOME", state);
        }
        let result = f();
        unsafe {
            match prev_config {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match prev_state {
                Some(value) => std::env::set_var("XDG_STATE_HOME", value),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
        result
    }

    fn project(dir: &Path) -> PathBuf {
        let root = std::fs::canonicalize(dir).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        root
    }

    fn write_project_skill(root: &Path, name: &str, description: &str) -> PathBuf {
        let dir = root.join(".dsh/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nbody\n"),
        )
        .unwrap();
        dir
    }

    fn ctx() -> Context {
        let pid = nix::unistd::getpid();
        Context::new_safe(pid, pid, false)
    }

    fn proxy(cwd: PathBuf) -> TestShellProxy {
        TestShellProxy {
            current_dir: cwd,
            confirm_result: true,
            ..TestShellProxy::default()
        }
    }

    fn stage_project_proposal(
        root: &Path,
        name: &str,
        contents: &str,
        base_digest: Option<String>,
    ) -> String {
        pending::stage(Proposal {
            version: 0,
            id: pending::proposal_id(skills::SkillScope::Project, name, "SKILL.md"),
            scope: "project".to_string(),
            name: name.to_string(),
            file: "SKILL.md".to_string(),
            action: if base_digest.is_some() {
                "write_file"
            } else {
                "create"
            }
            .to_string(),
            project_root: Some(root.join(".dsh/skills")),
            contents: contents.to_string(),
            base_digest,
            created_ms: usage::now_ms(),
            origin: "tool".to_string(),
            note: Some("because reasons".to_string()),
        })
        .unwrap()
    }

    #[test]
    fn approve_writes_through_the_same_path_as_the_tool() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step one\n");
            let id = stage_project_proposal(&root, "demo", &contents, None);

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = approve(&context, &mut p, &id);

            assert_eq!(status, ExitStatus::ExitedWith(0));
            assert_eq!(
                std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap(),
                contents
            );
            assert!(pending::list().0.is_empty());

            let records = usage::load();
            let record = records
                .get(&usage::key(&root.join(".dsh/skills/demo")))
                .expect("a write records usage");
            assert_eq!(record.created_by, "agent");
        });
    }

    /// A `create` proposal's staleness check alone cannot see a *sibling*
    /// collision - `request.target` (`demo/SKILL.md`) still does not exist
    /// either way, so `base_digest` still matches. `create()` itself refuses
    /// this before writing; `approve` must refuse it too, not reproduce the
    /// "two skills, one name" collision through a different door.
    #[test]
    fn approve_refuses_a_create_that_would_collide_with_a_file_skill_staged_after_it() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
            let id = stage_project_proposal(&root, "demo", &contents, None);

            // A file-skill named `demo.md` appears after staging but before
            // approval - through a different tool call, a hand edit, or a
            // second approved proposal.
            std::fs::create_dir_all(root.join(".dsh/skills")).unwrap();
            std::fs::write(
                root.join(".dsh/skills/demo.md"),
                "---\nname: demo\ndescription: unrelated file skill\n---\n",
            )
            .unwrap();

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = approve(&context, &mut p, &id);

            assert_eq!(status, ExitStatus::ExitedWith(1));
            assert!(
                !root.join(".dsh/skills/demo").exists(),
                "the colliding directory must never be created"
            );
            assert_eq!(
                pending::list().0.len(),
                1,
                "a refused approval leaves the proposal for review, not silently dropped"
            );
        });
    }

    /// The same collision, but the directory itself (not a sibling `.md`)
    /// was created by some other route between staging and approval.
    #[test]
    fn approve_refuses_a_create_whose_directory_appeared_after_it_was_staged() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
            let id = stage_project_proposal(&root, "demo", &contents, None);

            // The skill directory exists (created some other way), but not
            // yet `SKILL.md` itself - so `base_digest` (keyed on the target
            // file, not the directory) still reads as unchanged.
            std::fs::create_dir_all(root.join(".dsh/skills/demo")).unwrap();
            std::fs::write(root.join(".dsh/skills/demo/notes.txt"), "unrelated").unwrap();

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = approve(&context, &mut p, &id);

            assert_eq!(status, ExitStatus::ExitedWith(1));
            assert!(!root.join(".dsh/skills/demo/SKILL.md").exists());
            assert_eq!(pending::list().0.len(), 1);
        });
    }

    /// A target that moved since staging (edited by hand, or by a second
    /// proposal already approved) must be refused, not clobbered.
    #[test]
    fn approve_refuses_a_proposal_whose_target_changed_since_it_was_staged() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            let original = "---\nname: demo\ndescription: original\n---\n\nbody\n".to_string();
            std::fs::create_dir_all(root.join(".dsh/skills/demo")).unwrap();
            std::fs::write(root.join(".dsh/skills/demo/SKILL.md"), &original).unwrap();

            // Staged against a digest that does not match what is on disk -
            // as if the file had been edited since.
            let stale_contents =
                skill_tool::render_skill_md("demo", "a change nobody reviewed yet", "step\n");
            let id = stage_project_proposal(
                &root,
                "demo",
                &stale_contents,
                Some(pending::content_digest("something else entirely")),
            );

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = approve(&context, &mut p, &id);

            assert_eq!(status, ExitStatus::ExitedWith(1));
            assert_eq!(
                std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap(),
                original,
                "a stale approval must not overwrite what is actually there"
            );
            assert_eq!(
                pending::list().0.len(),
                1,
                "a refused approval leaves the proposal for review, not silently dropped"
            );
        });
    }

    #[test]
    fn approve_refuses_a_project_proposal_from_another_repository() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let other_dir = tempdir().unwrap();
        let other_root = project(other_dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
            // Staged as if it came from `other_root`, but approved from `root`.
            let id = pending::stage(Proposal {
                version: 0,
                id: pending::proposal_id(skills::SkillScope::Project, "demo", "SKILL.md"),
                scope: "project".to_string(),
                name: "demo".to_string(),
                file: "SKILL.md".to_string(),
                action: "create".to_string(),
                project_root: Some(other_root.join(".dsh/skills")),
                contents,
                base_digest: None,
                created_ms: usage::now_ms(),
                origin: "tool".to_string(),
                note: None,
            })
            .unwrap();

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = approve(&context, &mut p, &id);

            assert_eq!(status, ExitStatus::ExitedWith(1));
            assert!(!root.join(".dsh/skills/demo").exists());
            assert_eq!(pending::list().0.len(), 1);
        });
    }

    #[test]
    fn reject_removes_the_proposal_without_writing_anything() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            let contents = skill_tool::render_skill_md("demo", "Use when demoing", "step\n");
            let id = stage_project_proposal(&root, "demo", &contents, None);

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = reject(&context, &mut p, &id);

            assert_eq!(status, ExitStatus::ExitedWith(0));
            assert!(pending::list().0.is_empty());
            assert!(!root.join(".dsh/skills/demo").exists());
        });
    }

    #[test]
    fn pending_cmd_lists_broken_proposals_without_dropping_them() {
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            std::fs::create_dir_all(crate::config_paths::skills_pending_dir()).unwrap();
            std::fs::write(
                crate::config_paths::skills_pending_dir().join("junk.json"),
                "{ not json",
            )
            .unwrap();

            let context = ctx();
            let status = pending_cmd(&context);
            assert_eq!(status, ExitStatus::ExitedWith(0));
            assert_eq!(pending::list().1.len(), 1, "the broken file must survive");
        });
    }

    #[test]
    fn archive_refuses_a_project_skill() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();

        with_homes(config.path(), state.path(), || {
            write_project_skill(&root, "demo", "Use when demoing");

            let context = ctx();
            let mut p = proxy(root.clone());
            let status = archive(&context, &mut p, "demo");

            assert_eq!(status, ExitStatus::ExitedWith(1));
            assert!(!usage::is_archived(
                usage::load().get(&usage::key(&root.join(".dsh/skills/demo")))
            ),);
        });
    }

    #[test]
    fn archive_and_unarchive_round_trip_for_a_personal_skill() {
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let cwd_path = std::fs::canonicalize(cwd.path()).unwrap();

        with_homes(config.path(), state.path(), || {
            let skill_dir = config.path().join("dsh/skills/demo");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                "---\nname: demo\ndescription: Use when demoing\n---\n\nbody\n",
            )
            .unwrap();

            let context = ctx();
            let mut p = proxy(cwd_path.clone());

            assert_eq!(archive(&context, &mut p, "demo"), ExitStatus::ExitedWith(0));
            assert!(usage::is_archived(
                usage::load().get(&usage::key(&skill_dir))
            ));

            assert_eq!(
                unarchive(&context, &mut p, "demo"),
                ExitStatus::ExitedWith(0)
            );
            assert!(!usage::is_archived(
                usage::load().get(&usage::key(&skill_dir))
            ));
        });
    }

    #[test]
    fn pin_prevents_a_skill_from_being_reported_as_pinned_is_false_by_default() {
        let config = tempdir().unwrap();
        let state = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let cwd_path = std::fs::canonicalize(cwd.path()).unwrap();

        with_homes(config.path(), state.path(), || {
            let skill_dir = config.path().join("dsh/skills/demo");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                "---\nname: demo\ndescription: Use when demoing\n---\n\nbody\n",
            )
            .unwrap();

            let context = ctx();
            let mut p = proxy(cwd_path.clone());

            assert!(
                !usage::load()
                    .get(&usage::key(&skill_dir))
                    .is_some_and(|r| r.pinned)
            );
            assert_eq!(pin(&context, &mut p, "demo"), ExitStatus::ExitedWith(0));
            assert!(usage::load().get(&usage::key(&skill_dir)).unwrap().pinned);
            assert_eq!(unpin(&context, &mut p, "demo"), ExitStatus::ExitedWith(0));
            assert!(!usage::load().get(&usage::key(&skill_dir)).unwrap().pinned);
        });
    }

    #[test]
    fn an_unread_skill_reads_as_never() {
        assert_eq!(describe_age(1_000_000, 0), "never");
    }

    #[test]
    fn age_is_reported_in_whole_days() {
        let day = 24 * 60 * 60 * 1000u64;
        assert_eq!(describe_age(10 * day, 10 * day - 1), "today");
        assert_eq!(describe_age(10 * day, 9 * day), "1 day ago");
        assert_eq!(describe_age(10 * day, 3 * day), "7 days ago");
    }

    /// A clock that went backwards must not read as an ancient timestamp.
    #[test]
    fn a_timestamp_in_the_future_reads_as_today() {
        assert_eq!(describe_age(100, 200), "today");
    }

    #[test]
    fn a_summary_within_the_column_budget_is_unchanged() {
        assert_eq!(display_summary("short trigger"), "short trigger");
    }

    #[test]
    fn a_summary_at_the_prompt_budget_is_still_cut_for_the_terminal() {
        // The prompt's own budget (240) is wider than a terminal column
        // budget should be; `skill list` has its own, smaller limit.
        let long = "a".repeat(MAX_LIST_SUMMARY_CHARS + 40);
        let shown = display_summary(&long);
        assert!(shown.ends_with("..."));
        assert!(shown.chars().count() <= MAX_LIST_SUMMARY_CHARS + 3);
    }

    #[test]
    fn unified_lines_diffs_a_small_change_line_by_line() {
        let diff = unified_lines("a\nb\nc\n", "a\nx\nc\n");
        assert!(diff.contains("- b"), "{diff}");
        assert!(diff.contains("+ x"), "{diff}");
        assert!(diff.contains("  a"), "{diff}");
    }

    /// An on-disk file has no size cap the way a staged proposal's body
    /// does, so an unusually large one must fall back rather than build an
    /// `O(n*m)` table sized by it.
    #[test]
    fn unified_lines_falls_back_instead_of_building_an_unbounded_table() {
        let huge = "line\n".repeat(MAX_DIFF_LINES + 1);
        let diff = unified_lines(&huge, "new content\n");
        assert!(diff.contains("too large to diff"), "{diff}");
        assert!(diff.contains("new content"), "{diff}");
    }
}
