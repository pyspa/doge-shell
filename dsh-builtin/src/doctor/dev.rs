//! `doctor dev` / `doctor validate`: suggest validation commands from
//! changed files, mirroring `scripts/check.sh`'s per-path rules.
use crate::ShellProxy;
use dsh_types::Context;
use dsh_types::process_runtime::CommandRuntimeSnapshot;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) fn check_dev(ctx: &Context, proxy: &mut dyn ShellProxy, current_dir: &Path) {
    let Some(repo_root) = find_repo_root(current_dir) else {
        let _ = ctx.write_stdout("warn repo-root not-found for validation suggestions");
        return;
    };
    let _ = ctx.write_stdout(&format!("ok repo-root {}", repo_root.display()));

    // `git status` resolves through the logical runtime and runs with the
    // exported child environment in the repo root — the same `git` the
    // shell would run.
    let snapshot = proxy.command_runtime_snapshot().ok();
    let Some(snapshot) = snapshot.as_ref() else {
        let _ = ctx.write_stdout("warn changed-files unavailable runtime-snapshot-unavailable");
        return;
    };
    let changed = changed_paths(snapshot, &repo_root);
    match changed {
        Ok(paths) if paths.is_empty() => {
            let _ = ctx.write_stdout("skip changed-files none");
        }
        Ok(paths) => {
            let _ = ctx.write_stdout(&format!("ok changed-files {}", paths.len()));
            for path in &paths {
                let _ = ctx.write_stdout(&format!("ok changed {}", path.display()));
            }
            let commands = validation_commands_for_paths(&paths);
            if commands.is_empty() {
                let _ = ctx.write_stdout("skip validation no focused command for changed files");
            } else {
                for command in commands {
                    let _ = ctx.write_stdout(&format!("ok validate {command}"));
                }
            }
            for note in notes_for_paths(&paths) {
                let _ = ctx.write_stdout(&format!("ok note {note}"));
            }
        }
        Err(err) => {
            let _ = ctx.write_stdout(&format!("warn changed-files unavailable {err}"));
        }
    }
}

pub(super) fn find_repo_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    for ancestor in cwd.ancestors() {
        if ancestor.join("Cargo.toml").is_file() && ancestor.join("docs").join("ai").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

pub(super) fn changed_paths(
    snapshot: &CommandRuntimeSnapshot,
    repo_root: &Path,
) -> std::result::Result<Vec<PathBuf>, String> {
    let output = snapshot
        .std_command("git")
        .ok_or_else(|| "command not found: git".to_string())?
        .args(["status", "--short"])
        .current_dir(repo_root)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(parse_git_status_short(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub(super) fn parse_git_status_short(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim();
            if path.is_empty() {
                return None;
            }
            let path = path
                .rsplit_once(" -> ")
                .map(|(_, new_path)| new_path)
                .unwrap_or(path);
            Some(PathBuf::from(path.trim_matches('"')))
        })
        .collect()
}

pub(super) fn validation_commands_for_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut commands = Vec::new();
    let mut packages = BTreeSet::new();
    let mut needs_workspace_check = false;
    let mut needs_ai_guidance = false;
    let mut needs_project_consistency = false;
    let mut needs_shell_proxy_check = false;
    let mut has_rust = false;
    let mut needs_portability = false;
    let mut needs_file_budget = false;

    for path in paths {
        let text = path.to_string_lossy().replace('\\', "/");
        if text.ends_with(".rs") {
            has_rust = true;
        }
        if text == "Cargo.toml" || text.ends_with("/Cargo.toml") || text == "Cargo.lock" {
            needs_workspace_check = true;
        }
        if text == "Cargo.toml"
            || text.ends_with("/Cargo.toml")
            || text == "README.md"
            || text == "LICENSE"
            || text == "scripts/check-project-consistency.py"
        {
            needs_project_consistency = true;
        }
        // `check.sh` runs this on every change, but nothing here suggested it,
        // so a proxy method added over a `doctor validate` cycle only failed in
        // CI.
        if text == "dsh-builtin/src/lib.rs"
            || text == "dsh-builtin/src/shell_capabilities.rs"
            || text == "dsh-builtin/src/capability.rs"
            || text == "scripts/check-shell-proxy-capabilities.py"
        {
            needs_shell_proxy_check = true;
        }
        // Linker tuning has to stay scoped to the target that accepts it, and
        // the workflow is where the macOS side is actually proven.
        if text.starts_with(".cargo/") || text.starts_with(".github/workflows/") {
            needs_portability = true;
        }
        if text == "AGENTS.md"
            || text == "CLAUDE.md"
            || text.starts_with("docs/ai/")
            || text.starts_with(".claude/")
            || text == "scripts/install-runtime-skills.sh"
        {
            needs_ai_guidance = true;
        }
        // New or split `.rs` files over the 400/800-line budget and stale
        // backtick paths in guidance docs only fail in CI, so surface the
        // lint whenever Rust or guidance files change.
        if text.ends_with(".rs")
            || text.starts_with("docs/ai/")
            || text == "AGENTS.md"
            || text == "CLAUDE.md"
        {
            needs_file_budget = true;
        }

        // `completions/` is embedded into the `doge-shell` binary by rust-embed,
        // and `command-completion-schema.json` is asserted to match the provider
        // list in `dsh-types`.
        if text.starts_with("completions/") {
            packages.insert("doge-shell");
        }
        if text == "command-completion-schema.json" {
            packages.insert("doge-shell");
            packages.insert("dsh-types");
        }

        // `output-schemas/` is embedded the same way (rust-embed), and
        // `command-output-schema.json` mirrors it for `|:`'s output schemas.
        if text.starts_with("output-schemas/") {
            packages.insert("doge-shell");
        }
        if text == "command-output-schema.json" {
            packages.insert("doge-shell");
            packages.insert("dsh-types");
        }

        if text.starts_with("dsh-builtin/") {
            packages.insert("dsh-builtin");
        } else if text.starts_with("dsh-openai/") {
            packages.insert("dsh-openai");
        } else if text.starts_with("dsh-types/") {
            packages.insert("dsh-types");
        } else if text.starts_with("dsh-frecency/") {
            packages.insert("dsh-frecency");
        } else if text.starts_with("dsh/") {
            packages.insert("doge-shell");
        }
    }

    if has_rust {
        add_command(&mut commands, "cargo fmt --check");
    }
    // Every Rust edit is a chance to add a one-armed `#[cfg(target_os = ..)]` or
    // an unported `/proc` read, and neither fails a build on the host that wrote
    // it. The lint scans the whole tree either way, so it costs one line.
    if has_rust || needs_portability {
        add_command(&mut commands, "scripts/check-portability.py");
    }
    for package in [
        "dsh-builtin",
        "doge-shell",
        "dsh-openai",
        "dsh-types",
        "dsh-frecency",
    ] {
        if packages.contains(package) {
            add_command(&mut commands, &format!("cargo test -p {package}"));
        }
    }
    if needs_workspace_check || packages.len() > 1 {
        add_command(&mut commands, "cargo check --workspace");
    }
    if packages.contains("doge-shell") {
        add_command(&mut commands, "cargo clippy -p doge-shell -- -D warnings");
    }
    if needs_ai_guidance {
        add_command(&mut commands, "scripts/check-ai-guidance.sh");
        add_command(&mut commands, "scripts/install-runtime-skills.sh --list");
        add_command(
            &mut commands,
            "scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core",
        );
    }
    if needs_project_consistency {
        add_command(&mut commands, "scripts/check-project-consistency.py");
    }
    if needs_file_budget {
        add_command(&mut commands, "scripts/check-file-budget.py");
    }
    if needs_shell_proxy_check {
        add_command(&mut commands, "scripts/check-shell-proxy-capabilities.py");
    }

    commands
}

pub(super) fn add_command(commands: &mut Vec<String>, command: &str) {
    if !commands.iter().any(|existing| existing == command) {
        commands.push(command.to_string());
    }
}

/// Display-only reminders that are not runnable commands, so they stay out
/// of `validation_commands_for_paths` and ride alongside it instead.
pub(super) fn notes_for_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut notes = Vec::new();
    let embedded_json = paths.iter().any(|path| {
        let text = path.to_string_lossy().replace('\\', "/");
        text.starts_with("completions/") || text.starts_with("output-schemas/")
    });
    // rust-embed only rebuilds on files it already knows: editing an
    // existing definition rebuilds on its own, but a newly added file ships
    // silently without a `touch` of its loader.
    if embedded_json {
        notes.push(
            "if you added (not edited) a file, touch its loader so rust-embed rebuilds: \
             `touch dsh/src/completion/json_loader.rs` for completions/, \
             `touch dsh/src/output_schema/loader.rs` for output-schemas/"
                .to_string(),
        );
    }
    notes
}
