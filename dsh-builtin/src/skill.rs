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
use crate::chatgpt::skills::usage;
use crate::chatgpt::skills::{self, Skill, SkillScope, SkillsManager};
use crate::config_paths::display_path;
use dsh_types::{Context, ExitStatus};
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

pub fn description() -> &'static str {
    "List, show and remove the skills the AI chat runtime reads"
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
        Some((&"help", _)) | Some((&"-h", _)) | Some((&"--help", _)) => {
            print_help(ctx);
            ExitStatus::ExitedWith(0)
        }
        Some((unknown, _)) => usage_error(ctx, &format!("skill: unknown subcommand `{unknown}`")),
    }
}

fn print_help(ctx: &Context) {
    let _ = ctx.write_stdout(
        "skill - inspect the skills the AI chat runtime reads\n\
         \n\
         Usage: skill <subcommand> [name]\n\
         \n\
         Subcommands:\n  \
           list             Every skill, with scope, read count and last use\n  \
           show <name>      Render a skill's SKILL.md\n  \
           path <name>      Print its path, for `$EDITOR $(skill path <name>)`\n  \
           remove <name>    Delete a skill, after confirmation\n  \
           trust            Show whether this repository's skills are trusted\n  \
           untrust          Forget that decision for this repository\n\
         \n\
         Project skills come from `.dsh/skills` in the enclosing project and take\n\
         precedence over personal ones of the same name. They are only read once\n\
         you have agreed to them, because their descriptions go into every prompt.",
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
    let roots: Vec<(SkillScope, PathBuf)> = manager
        .roots()
        .iter()
        .map(|root| (root.scope, root.path.clone()))
        .collect();
    let skills = manager.load_skills();
    let records = usage::load();
    let now = usage::now_ms();

    if skills.is_empty() {
        let _ = ctx.write_stdout("No skills yet.");
        for (scope, path) in &roots {
            let _ = ctx.write_stdout(&format!("  {}: {}", scope.as_str(), display_path(path)));
        }
        let _ =
            ctx.write_stdout("Ask the assistant to save one, or write a SKILL.md there by hand.");
        return ExitStatus::ExitedWith(0);
    }

    let mut unused = Vec::new();
    for skill in &skills {
        let record = records.get(&usage::key(skill.dir()));
        let reads = record.map(|r| r.reads).unwrap_or(0);
        let last = record.map(|r| r.last_read_ms).unwrap_or(0);
        let author = record
            .map(|r| r.created_by.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("user");

        if usage::is_stale(record, now) {
            unused.push(skill.name.as_str());
        }

        let _ = ctx.write_stdout(&format!(
            "{:<8} {:<28} reads={:<5} last={:<12} by={:<6} {}",
            skill.scope.as_str(),
            skill.name,
            reads,
            describe_age(now, last),
            author,
            skill.summary()
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

/// The project root this shell would ask about, if there is one.
fn project_decision(proxy: &mut dyn ShellProxy) -> Option<skills::ProjectSkillDecision> {
    let cwd = proxy.get_current_dir().ok()?;
    skills::describe_project_root(&skills::skill_roots(Some(&cwd), true))
}

fn trust_status(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let Some(decision) = project_decision(proxy) else {
        let _ = ctx.write_stdout("No project skills here.");
        return ExitStatus::ExitedWith(0);
    };

    let remembered = skills::trust::is_remembered(&decision.root, &decision.digest);
    let _ = ctx.write_stdout(&format!(
        "{} {} ({} skill(s): {})",
        if remembered { "trusted" } else { "untrusted" },
        display_path(&decision.root),
        decision.names.len(),
        decision.names.join(", ")
    ));
    if !remembered {
        let _ = ctx.write_stdout(
            "The next `!` here asks before reading them. Answer `a` to remember this repository.",
        );
    }
    ExitStatus::ExitedWith(0)
}

fn untrust(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let Some(decision) = project_decision(proxy) else {
        let _ = ctx.write_stdout("No project skills here.");
        return ExitStatus::ExitedWith(0);
    };

    if skills::trust::forget(&decision.root) {
        let _ = ctx.write_stdout(&format!("Forgot {}", display_path(&decision.root)));
    } else {
        let _ = ctx.write_stdout(&format!("{} was not trusted", display_path(&decision.root)));
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
}
