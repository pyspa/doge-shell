//! `sched` no longer exists — replaced entirely by `cron`, which adds wall-
//! clock schedules and persistence across restarts on top of everything `sched`
//! did.
//!
//! Removing the builtin outright would let `sched` fall through to PATH
//! lookup and fail as `command not found`, with nothing to say why a command
//! that worked yesterday no longer does. This stub exists only to say that.

pub fn description() -> &'static str {
    "Removed: replaced by cron"
}

pub fn command(
    ctx: &dsh_types::Context,
    _argv: Vec<String>,
    _proxy: &mut dyn crate::ShellProxy,
) -> dsh_types::ExitStatus {
    let _ = ctx.write_stderr("sched: replaced by cron. Try: cron --help");
    dsh_types::ExitStatus::ExitedWith(1)
}
