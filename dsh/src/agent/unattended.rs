//! Two small pieces of setup an unattended agent run needs that an
//! interactive one gets for free.
//!
//! Shared by cron's AI jobs (`dsh/src/cron/run_job.rs`) and `agent run
//! --detach` (`dsh/src/agent/detach.rs`): both start in a fresh `dogesh -c`
//! process, which never runs the interactive startup that would otherwise
//! connect MCP servers.

use crate::shell::Shell;
use dsh_builtin::config_paths;

/// Connects the MCP servers `config.lisp` declared.
///
/// The synchronous path is deliberate: `Shell::reload_mcp_config` spawns, and
/// a run that started before its tools arrived is the failure this exists to
/// prevent. Failures are left to the agent to report - a server that is down
/// is not a reason to refuse to run at all.
pub(crate) fn connect_mcp(shell: &mut Shell) {
    let servers = shell.environment.read().mcp_servers().to_vec();
    if servers.is_empty() {
        return;
    }
    let manager = shell
        .environment
        .read()
        .integration_state
        .mcp_manager
        .clone();
    manager.write().sync_servers_blocking(servers);
}

/// How many skill proposals are waiting for a person.
///
/// Staged writes do not stop an unattended task, so a "successful" run can
/// quietly leave a proposal nobody is looking at. Counting is enough to
/// surface it; approving stays where it already is, in `skill pending`.
pub(crate) fn pending_skill_count() -> u32 {
    std::fs::read_dir(config_paths::skills_pending_dir())
        .map(|entries| entries.filter_map(Result::ok).count() as u32)
        .unwrap_or_default()
}
