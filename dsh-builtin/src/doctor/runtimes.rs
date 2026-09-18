//! `doctor runtime`: common developer tools in PATH, and Herdr pane state.
use dsh_types::Context;

use super::*;
/// Mirrors `dsh/src/agent_lifecycle/herdr.rs::non_empty_env` exactly (trims,
/// then treats an all-whitespace value as unset) so this diagnostic and the
/// real detection it describes can never disagree about what counts as
/// "set".
pub(super) fn non_empty_env_for_doctor(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub(super) fn check_runtimes(ctx: &Context) {
    for command in [
        "mise", "direnv", "rustc", "cargo", "node", "npm", "pnpm", "python3", "uv", "go", "just",
        "herdr",
    ] {
        match resolve_in_path(command) {
            Some(path) => {
                let version = read_version(command).unwrap_or_else(|| "-".to_string());
                let _ = ctx.write_stdout(&format!("ok {command} {version} {}", path.display()));
            }
            None => {
                let _ = ctx.write_stdout(&format!("warn {command} not-found"));
            }
        }
    }

    // Pure process-environment reads, mirroring exactly what
    // `dsh/src/agent_lifecycle/herdr.rs::HerdrEnv::detect` requires - these
    // are ambient launch-time facts, not dsh settings, so this deliberately
    // doesn't go through `ShellProxy`/`resolve_setting`. `dsh-builtin`
    // cannot see `dsh`'s own activation state directly (the dependency runs
    // the other way), so replicating the same three checks here is the only
    // way to avoid reporting "active" when this process's own lifecycle
    // manager would in fact be a no-op `NullReporter`.
    //
    // `DOGESH_HERDR_ENABLED` itself is a dsh setting (shell var → process
    // env via `Environment::get_var`), so `doctor` cannot read it precisely
    // here. As a best-effort diagnostic it checks the process environment
    // copy; when not set it reports disabled rather than "not running under
    // herdr" so the user understands why reporting is off.
    // Keep in sync with `dsh/src/agent_lifecycle/agent_command.rs::herdr_enabled`.
    let herdr_enabled = std::env::var("DOGESH_HERDR_ENABLED")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| matches!(v.as_str(), "1" | "true" | "on" | "yes"))
        .is_some();
    if !herdr_enabled {
        let _ = ctx.write_stdout(
            "skip herdr-pane disabled (DOGESH_HERDR_ENABLED not set to 1/true/on/yes)",
        );
        return;
    }
    let herdr_env = std::env::var("HERDR_ENV").ok();
    let pane_id = non_empty_env_for_doctor("HERDR_PANE_ID");
    let bin_path = non_empty_env_for_doctor("HERDR_BIN_PATH");
    let nested_owner = std::env::var_os("DOGESH_HERDR_OWNER_PID").is_some();
    match (herdr_env.as_deref(), pane_id, bin_path, nested_owner) {
        (Some("1"), Some(pane_id), Some(_), false) => {
            let _ = ctx.write_stdout(&format!("ok herdr-pane active pane={pane_id}"));
        }
        (Some("1"), Some(pane_id), _, true) => {
            let _ = ctx.write_stdout(&format!(
                "skip herdr-pane pane={pane_id} but an ancestor dsh already owns lifecycle authority for it"
            ));
        }
        (Some("1"), Some(_), None, false) => {
            let _ = ctx.write_stdout(
                "warn herdr-pane HERDR_ENV set but HERDR_BIN_PATH is missing or empty",
            );
        }
        _ => {
            let _ = ctx.write_stdout("skip herdr-pane not running under herdr");
        }
    }
}
