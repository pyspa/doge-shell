//! The per-iteration "environment snapshot" message: current directory, OS,
//! and git worktree/branch state, rebuilt only when the signature
//! (cwd + `.git/HEAD` mtime) actually changes rather than on every tool call.
use super::*;

/// Environment snapshot for one agent run, rebuilt only when it changes.
///
/// It used to be regenerated on every iteration, which cost two `git`
/// subprocesses per tool call for information that rarely moves.
#[derive(Default)]
pub(super) struct DynamicContext {
    signature: Option<EnvironmentSignature>,
    rendered: String,
}

/// What the snapshot depends on.
///
/// Only the directory and the repository state now: the file list and the
/// alias table moved out of the snapshot, and with them the reasons to watch
/// the directory mtime and count aliases on every iteration.
#[derive(PartialEq, Eq)]
pub(super) struct EnvironmentSignature {
    cwd: PathBuf,
    git_head_modified_ms: u128,
}

impl DynamicContext {
    pub(super) fn message(&mut self, proxy: &mut dyn ShellProxy) -> Value {
        let signature = environment_signature(proxy);
        if self.signature.as_ref() != Some(&signature) {
            self.rendered = build_dynamic_context(proxy);
            self.signature = Some(signature);
        }

        json!({ "role": "user", "content": self.rendered.clone() })
    }
}

pub(super) fn environment_signature(proxy: &mut dyn ShellProxy) -> EnvironmentSignature {
    let cwd = proxy
        .get_current_dir()
        .or_else(|_| std::env::current_dir())
        .unwrap_or_default();

    EnvironmentSignature {
        git_head_modified_ms: git_head_path(&cwd)
            .map(|head| modified_ms(&head))
            .unwrap_or(0),
        cwd,
    }
}

pub(super) fn modified_ms(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

/// Find the `HEAD` of the repository containing `start` without spawning `git`.
///
/// Checkouts rewrite this file; commits that only move a ref do not, so the
/// directory mtime is what catches ordinary edits.
pub(super) fn git_head_path(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let git = dir.join(".git");
        if git.is_dir() {
            return Some(git.join("HEAD"));
        }
        if git.is_file() {
            // Linked worktree or submodule: the pointer file itself changes.
            return Some(git);
        }
        current = dir.parent();
    }
    None
}

/// The summary text, or why there is none.
///
/// Through `turn`, like every other answer. Reading `choices[0].message` by
/// hand accepted a summary the provider had cut short
/// (`finish_reason=length`), and because the caller drops the buffer this
/// summary replaces, that loss is not recoverable. Returning `Err` leaves the
/// conversation as it was and only skips the compaction.
pub(super) fn summary_from_response(response: &Value) -> Result<String, String> {
    turn::answer_text(response)
        .map_err(|err| format!("Summarization returned no usable summary: {err}"))
}

pub(super) fn build_dynamic_context(proxy: &mut dyn ShellProxy) -> String {
    format!(
        "Environment snapshot (reference only; the task is stated in the first user message):\n{}",
        environment_snapshot(proxy)
    )
}

/// The few facts worth paying for on every single request.
///
/// The file list and the alias table used to live here too. Both are answers to
/// questions the model asks occasionally, and both were being re-sent on every
/// iteration of a hundred-step run; `ls` and `shell_context` now serve them on
/// demand instead.
pub(super) fn environment_snapshot(proxy: &mut dyn ShellProxy) -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let cwd = proxy
        .get_current_dir()
        .or_else(|_| std::env::current_dir())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "(failed to resolve current directory)".to_string());

    format!(
        "- OS: {os} ({arch})\n- Current directory: {cwd}\n- Git: {}",
        describe_git_state(proxy)
    )
}

pub(super) fn describe_git_state(proxy: &mut dyn ShellProxy) -> String {
    // Resolved through the logical runtime, spawned with exactly the
    // exported child environment: the model sees the same git the shell
    // would run.
    let output = crate::runtime_spawn::runtime_command(proxy, "git").and_then(|mut command| {
        command
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .map_err(|e| anyhow::anyhow!("{e}"))
    });
    match output {
        Ok(output) if output.status.success() => {
            let inside = String::from_utf8_lossy(&output.stdout)
                .trim()
                .eq_ignore_ascii_case("true");

            if !inside {
                return "not inside a Git worktree".to_string();
            }

            match git_state_details(proxy) {
                Some((root, branch)) => match root {
                    Some(root) => format!("inside a Git worktree (root: {root}, {branch})"),
                    None => format!("inside a Git worktree ({branch})"),
                },
                None => "inside a Git worktree (branch unknown)".to_string(),
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.trim().is_empty() {
                let code = output
                    .status
                    .code()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string());
                format!("unable to determine Git status (exit status {code})")
            } else {
                format!("unable to determine Git status ({})", stderr.trim())
            }
        }
        Err(err) => format!("git command unavailable ({err})"),
    }
}

pub(super) fn git_state_details(proxy: &mut dyn ShellProxy) -> Option<(Option<String>, String)> {
    let output = crate::runtime_spawn::runtime_command(proxy, "git")
        .ok()?
        .args([
            "rev-parse",
            "--show-toplevel",
            "--abbrev-ref",
            "HEAD",
            "--short",
            "HEAD",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let root = lines.next().map(|line| line.to_string());
    let branch = lines.next()?;
    let short_head = lines.next().map(|line| line.to_string());

    let branch_description = if branch == "HEAD" {
        short_head
            .map(|commit| format!("detached at {commit}"))
            .unwrap_or_else(|| "detached HEAD".to_string())
    } else {
        format!("branch {branch}")
    };

    Some((root, branch_description))
}
