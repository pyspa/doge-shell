//! `doctor runtime`: common developer tools in PATH, and Herdr pane state.
use crate::ShellProxy;
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

pub(super) fn check_runtimes(ctx: &Context, proxy: &mut dyn ShellProxy) {
    // One immutable runtime for resolution and version probes: the text
    // report and the JSON report share this resolver, so both describe the
    // actual shell runtime instead of the process-global PATH.
    let snapshot = proxy.command_runtime_snapshot().ok();
    for command in [
        "mise", "direnv", "rustc", "cargo", "node", "npm", "pnpm", "python3", "uv", "go", "just",
        "herdr",
    ] {
        let resolved = snapshot
            .as_ref()
            .and_then(|snapshot| resolve_in_path(snapshot, command));
        match resolved {
            Some(path) => {
                let version = snapshot
                    .as_ref()
                    .and_then(|snapshot| read_version(snapshot, command))
                    .unwrap_or_else(|| "-".to_string());
                let _ = ctx.write_stdout(&format!("ok {command} {version} {}", path.display()));
            }
            None => {
                let _ = ctx.write_stdout(&format!("warn {command} not-found"));
            }
        }
    }

    // `DOGESH_HERDR_ENABLED` is a shell setting: read it through the
    // logical runtime authority like every other shell setting, so `unset`
    // in the shell stays unset here too. Keep the truthy spelling in sync
    // with `dsh/src/agent_lifecycle/agent_command.rs::herdr_enabled`.
    let herdr_enabled = proxy
        .get_var("DOGESH_HERDR_ENABLED")
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| matches!(v.as_str(), "1" | "true" | "on" | "yes"))
        .is_some();
    if !herdr_enabled {
        let _ = ctx.write_stdout(
            "skip herdr-pane disabled (DOGESH_HERDR_ENABLED not set to 1/true/on/yes)",
        );
        return;
    }
    // The rest are ambient process facts about how this process was
    // launched, not shell settings — the same boundary
    // `dsh/src/agent_lifecycle/herdr.rs::HerdrEnv::detect` reads. A shell
    // variable must neither spoof nor suppress them.
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
