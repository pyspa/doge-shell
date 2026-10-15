//! The parent-shell entrypoint for one owned background AI task.
use crate::{CoreShellAction, ShellProxy};
use dsh_types::{Context, ExitStatus};
pub fn description() -> &'static str {
    "Start and manage a background AI task"
}
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch_core_action(ctx, CoreShellAction::ChatTask, argv) {
        Ok(()) => ExitStatus::ExitedWith(0),
        Err(error) => {
            let _ = ctx.write_stderr(&format!("chat_task: {error}"));
            ExitStatus::ExitedWith(1)
        }
    }
}
