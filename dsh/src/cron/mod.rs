//! Persistent scheduled jobs: shell commands and unattended agent tasks.
//!
//! # Two drivers, one tick
//!
//! A job fires from either an interactive session's runner or an external
//! `dsh -c "cron tick"` installed in the system crontab. Both call the same
//! claim, so a machine with three shells open and a timer running still
//! executes each slot once: the claim is a conditional `UPDATE` inside an
//! immediate transaction, and whoever loses the race simply finds nothing due.
//!
//! # Why a run is its own process
//!
//! `Shell` is `!Send` - it owns `Rc<RefCell<LispEngine>>` - so a spawned task
//! cannot drive an agent run, which needs `&mut Shell`. Rather than weaken
//! that, each claimed run is executed by a fresh `dsh -c "cron run-job <id>"`
//! child. Two other properties fall out of the same choice: an agent run moves
//! its process's working directory, so sharing one would nest the restores;
//! and a per-run child can be killed on its own deadline without taking the
//! rest of the tick's work with it.
//!
//! The child receives **only a UUID**. The goal, the grant and the command all
//! travel through the store as data, so nothing a job carries is ever parsed
//! by a shell.

pub mod cli;
pub mod clock;
pub mod exec;
pub mod handlers;
pub mod run_job;
pub mod runner;
pub mod setup;
pub mod store;
pub mod tick;
pub mod tool;

use anyhow::{Result, bail};
use dsh_builtin::config_paths;
use dsh_types::Context;
use store::SqliteCronStore;

const HELP: &str = "\
Usage:
  cron add [options] <schedule> <command...>          Register a shell job
  cron add --agent [options] <schedule> -- <goal>      Register an agent job
  cron list [--json]                                   Show jobs
  cron show <job> [--json]                              Show one job in full
  cron edit <job> [options]                             Change one job's fields
  cron rm <job>...                                      Remove job(s)
  cron run <job> [--now]                                Run on the next tick, or right now
  cron pause [<job>...]                                 Pause job(s), or the whole scheduler
  cron resume [<job>...]                                Resume job(s), or the whole scheduler
  cron history [<job>] [--failed] [--json]              Show finished runs
  cron logs [<job>] [--run <id>] [--stdout|--stderr] [--json]
                                                          Show one run's full recorded output
                                                          (needs job, --run, or both)
  cron incidents [--json]                               List things needing a person
  cron incidents ack <id>                               Acknowledge one and unblock its job
  cron notepad <job> [--clear]                          Show (or clear) a job's own notes
  cron status [--json]                                  Is a tick arriving? How many jobs?
  cron tick [--max N] [--dry-run] [--verbose] [--json]  Run due jobs once and exit
  cron doctor [--json]                                  Check jobs for common misconfiguration
  cron setup [--crontab|--systemd|--launchd]            Print external-tick setup text

Options for `add`/`edit`:
  --name <name>          Reference name (default: derived from the command/goal)
  --cwd <dir>            Working directory (default: here)
  --on <policy>          never | failure | change | both (default) | always
  --quiet                Same as --on never
  --timeout <n>          Seconds, or an interval like 5m (default: 60s, 900s for --agent)
  --catchup <n>          How late a missed run may be and still count (default: 1h)
  --paused               Register paused; see the first run with `cron run --now`
  --force                Allow `add` to replace an existing job of the same name

Options for an agent job (`--agent`), matching `agent run`:
  --tokens <n> --check <text> --read <dir> --write <dir> --allow-command <cmd>
  --allow-mcp <entry> --network <host> --env <name> --sandbox
  (agent defaults: --tokens 50000, --timeout 900s)

Commands run under `sh -c` from the job's `--cwd`; shell aliases, abbreviations,
builtins and Lisp functions are not available inside them. An agent job's
grant works exactly like `agent run`'s: nothing is asked at tick time, so a
missing grant stalls the run — see `cron incidents` — instead of prompting.

`cron run <job>` (no `--now`) refuses a paused or blocked job outright —
`cron resume <job>` (or `cron incidents ack`) first, or use `--now` to run it
right now regardless.";

/// The `cron` builtin. Every subcommand but `run-job` opens the store and
/// returns; `run-job` (and `run --now`) additionally need `&mut Shell` to
/// start an agent task, which is why this function — not
/// `dsh-builtin/src/cron.rs` — takes one, the same split `agent::command`
/// uses for the same reason.
pub fn command(shell: &mut crate::shell::Shell, ctx: &Context, argv: Vec<String>) -> Result<()> {
    if argv.len() < 2 || matches!(argv[1].as_str(), "help" | "--help" | "-h") {
        ctx.write_stdout(HELP)?;
        return Ok(());
    }
    let action = argv[1].as_str();
    let rest = &argv[2..];

    // `run-job` is internal - the only caller is the child a tick spawns -
    // and does not print `HELP`-shaped errors, so it is dispatched first.
    if action == "run-job" {
        return handlers::run_job(shell, ctx, rest);
    }

    let store = SqliteCronStore::open(&config_paths::cron_state_dir())?;
    match action {
        "add" | "create" => handlers::add(ctx, &store, rest),
        "list" | "ls" => handlers::list(ctx, &store, rest),
        "show" => handlers::show(ctx, &store, rest),
        "edit" => handlers::edit(ctx, &store, rest),
        "rm" | "remove" => handlers::remove(ctx, &store, rest),
        "run" => handlers::run(shell, ctx, &store, rest),
        "pause" => handlers::set_paused(ctx, &store, rest, true),
        "resume" => handlers::set_paused(ctx, &store, rest, false),
        "history" => handlers::history(ctx, &store, rest),
        "logs" => handlers::logs(ctx, &store, rest),
        "incidents" if handlers::is_ack(rest) => {
            handlers::ack_incident(ctx, &store, &handlers::ack_args(rest))
        }
        "incidents" => handlers::incidents(ctx, &store, rest),
        "notepad" => handlers::notepad(ctx, &store, rest),
        "status" => handlers::status(ctx, &store, rest),
        "tick" => handlers::tick_cmd(ctx, &store, rest),
        "doctor" => handlers::doctor(shell, ctx, &store, rest),
        "setup" => setup::command(ctx, rest),
        other => bail!("{other}: unknown subcommand; try `cron --help`"),
    }
}
