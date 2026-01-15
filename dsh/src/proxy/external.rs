//! External command execution handler.

use anyhow::Result;
use dsh_types::Context;
use std::process::Command;
use tracing::debug;

/// Execute an external command.
///
/// This is the fallback handler when no builtin command matches.
/// Uses `std::process::Command` for synchronous execution.
pub fn execute(_ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
    // For other commands, try to execute them as external commands
    // We use std::process::Command because we are in a sync context and cannot call async eval_str
    // Note: This bypasses shell aliases/functions for now, which is a limitation of sync proxy.
    debug!("Dispatching external command: {} {:?}", cmd, argv);

    // If the command contains shell metacharacters or argv is empty (implies potentially complex cmd string passed as one arg), use sh -c
    // Simple heuristic: if argv is empty AND cmd contains space or pipe, or if cmd contains pipe/redirects.
    // Generally safe-run might pass "curl | sh" as cmd with empty argv.
    let use_shell = argv.is_empty()
        && (cmd.contains(' ') || cmd.contains('|') || cmd.contains('>') || cmd.contains('&'));

    let status = if use_shell {
        debug!("Detected complex command, using sh -c");
        Command::new("sh").arg("-c").arg(cmd).status()
    } else {
        Command::new(cmd).args(argv).status()
    };

    match status {
        Ok(status) => {
            if !status.success() {
                // We return Err to signal failure to the caller (safe-run)
                // Since dispatch returns Result<()>, we use Err for non-zero exit status if we want safe-run to know.
                // However, safe-run might want to return the exact exit code.
                // But for now, returning Err is the only way to signal "something went wrong".
                return Err(anyhow::anyhow!("Command exited with status: {}", status));
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to execute '{}': {}", cmd, e));
        }
    }
    Ok(())
}
