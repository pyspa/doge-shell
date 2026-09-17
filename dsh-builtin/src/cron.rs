//! `cron` — the builtin's public face.
//!
//! Every subcommand needs the store (`SqliteCronStore`, `dsh/src/cron`),
//! which depends on `rusqlite` and so can only live in the `dsh` crate.
//! This file is the thin wrapper that hands off to it.

pub fn description() -> &'static str {
    "Create, edit and run scheduled jobs — shell commands"
}

pub fn command(
    ctx: &dsh_types::Context,
    argv: Vec<String>,
    proxy: &mut dyn crate::ShellProxy,
) -> dsh_types::ExitStatus {
    match proxy.dispatch_core_action(ctx, crate::CoreShellAction::Cron, argv) {
        Ok(()) => dsh_types::ExitStatus::ExitedWith(0),
        Err(error) => {
            let _ = ctx.write_stderr(&format!("cron: {error}"));
            dsh_types::ExitStatus::ExitedWith(1)
        }
    }
}
