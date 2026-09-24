//! Building the shell command every `execute` runs (`shell_command`),
//! rendering the JSON result the model sees (`render_result`), and taking a
//! process group down (`kill_process_group`).
//!
//! Capturing the output is [`crate::agent::jobs::AgentJobs`]' job, for both
//! entry points. There used to be a second capture engine here - bounded byte
//! log, background pipe drainers, a timeout/cancel run loop - used only by the
//! interactive path. Two engines meant two sets of answers to "what happens
//! when a grandchild holds the pipe open"; see `jobs.rs` for the one that
//! survived.
use super::*;
use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Run `command` under the fixed system shell.
///
/// Direct `Command::new` execution meant no pipes, no redirection, no `&&` and
/// no globbing, so `cargo test 2>&1 | tail -40` could not be expressed at all
/// and every multi-step job cost one round trip per step. The shell here is
/// what makes the command line real; what makes it safe is `authorize`, which
/// has already put the whole line through the shell's own parser and safety
/// guard.
///
/// Ordinary interactive child: `child_env` is the shell's exported
/// `child_process_env()` snapshot. The ambient environment is cleared first,
/// so an unexported or logically unset variable never reappears from the
/// process-global environment.
///
/// The single place an interactive `execute` turns an authorized line into a
/// process, so the line that was judged and the line that runs cannot drift.
/// stdio and the process group are left to `AgentJobs::start`.
pub(super) fn shell_command(
    command: &str,
    cwd: Option<&Path>,
    child_env: &HashMap<String, String>,
) -> Command {
    let mut builder = Command::new("/bin/sh");
    builder
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .env_clear()
        .envs(child_env);

    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }

    builder
}

/// Serialize the result so that it survives the global tool-output cap intact.
///
/// The per-stream budgets bound the *raw* text, but JSON escaping can double or
/// sextuple it. Handing an oversized object to the shared truncator produced a
/// middle-cut, unparseable JSON document.
pub(super) fn render_result(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    note: Option<String>,
) -> String {
    let mut budget = MAX_STREAM_CHARS;

    loop {
        let mut result = json!({
            "exit_code": exit_code,
            "stdout": dsh_openai::turn::truncate_middle(stdout, budget),
            "stderr": dsh_openai::turn::truncate_middle(stderr, budget),
        });

        if let Some(note) = &note
            && let Some(map) = result.as_object_mut()
        {
            map.insert("note".into(), json!(note));
        }

        let rendered = result.to_string();
        if rendered.len() <= super::super::MAX_OUTPUT_LENGTH || budget <= MIN_STREAM_CHARS {
            return rendered;
        }

        budget /= 2;
    }
}
/// Signal the whole group the child leads, so background grandchildren die too.
pub(crate) fn kill_process_group(child: &std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // `process_group(0)` made the child its own group leader, so pgid == pid.
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
}
