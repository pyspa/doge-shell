//! `chat_status` — answered where the MCP manager is reachable.
//!
//! The carried conversation's continuity depends on the operator prompt, the
//! language, the MCP connections and the project. A plain `ShellProxy` can
//! only re-check the idle clock, so this runs as a core action with the full
//! `ChatToolHost` and reports exactly what the next `!` would do.
use crate::shell::Shell;
use anyhow::Result;
use dsh_types::Context;

pub fn execute(shell: &mut Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    dsh_builtin::chat_status_detailed(ctx, argv, shell)
}
