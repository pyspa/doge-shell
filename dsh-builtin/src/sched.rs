//! `sched` — run a command every N seconds/minutes/hours for the rest of the
//! session.
//!
//! Tasks are session-scoped on purpose: nothing survives shell exit, and
//! `sched list --lisp` prints the calls to put in `config.lisp` if you want
//! them back next time.
//!
//! Commands run under `sh -c`, detached from the terminal, so shell aliases,
//! abbreviations, builtins and Lisp functions are not available inside them.

use super::ShellProxy;
use dsh_types::schedule::{
    DEFAULT_TIMEOUT_SECS, NotifyPolicy, SchedTaskSpec, SchedTaskView, parse_interval,
};
use dsh_types::{Context, ExitStatus};
use std::borrow::Cow;
use std::time::Duration;
use tabled::{Table, Tabled};

pub fn description() -> &'static str {
    "Run a command periodically in the background for this session"
}

const USAGE: &str = "\
Usage:
  sched add [options] <interval> <command...>   Register a task (interval: 30s, 5m, 1h)
  sched list [--lisp]                           Show tasks, or the config.lisp form
  sched rm <id|name>                            Remove a task
  sched run <id|name>                           Run once, now
  sched pause [<id|name>]                       Pause one task, or the scheduler
  sched resume [<id|name>]                      Resume one task, or the scheduler
  sched log [<id|name>]                         Recent runs

Options for `add`:
  --name <name>        Reference name (default: the first word of the command)
  --on <policy>        never | failure | change | both (default) | always
  --quiet              Same as --on never
  --timeout <interval> Kill a run that takes longer (default: 60s, capped to the interval)

Commands run via `sh -c` from the directory you registered them in; shell
aliases and builtins are not available inside them.";

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    command_with_scheduler(ctx, argv, proxy)
}

fn command_with_scheduler<P: crate::shell_capabilities::ShellScheduling + ?Sized>(
    ctx: &Context,
    argv: Vec<String>,
    proxy: &mut P,
) -> ExitStatus {
    match argv.get(1).map(|s| s.as_str()) {
        None | Some("list") | Some("ls") => list(ctx, argv.get(2).map(|s| s.as_str()), proxy),
        Some("add") => add(ctx, &argv[2..], proxy),
        Some("rm") | Some("remove") => mutate(ctx, "rm", argv.get(2), proxy, |proxy, selector| {
            proxy
                .sched_remove(selector)
                .map(|name| format!("removed {name}"))
        }),
        Some("run") => mutate(ctx, "run", argv.get(2), proxy, |proxy, selector| {
            proxy
                .sched_trigger(selector)
                .map(|name| format!("{name} will run on the next scan"))
        }),
        Some("pause") => set_paused(ctx, argv.get(2), proxy, true),
        Some("resume") => set_paused(ctx, argv.get(2), proxy, false),
        Some("log") => log(ctx, argv.get(2).map(|s| s.as_str()), proxy),
        Some("-h") | Some("--help") | Some("help") => {
            ctx.write_stdout(USAGE).ok();
            ExitStatus::ExitedWith(0)
        }
        Some(other) => {
            ctx.write_stderr(&format!("sched: {other}: unknown subcommand"))
                .ok();
            ctx.write_stderr(USAGE).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn mutate<P: crate::shell_capabilities::ShellScheduling + ?Sized>(
    ctx: &Context,
    subcommand: &str,
    selector: Option<&String>,
    proxy: &mut P,
    action: impl FnOnce(&mut P, &str) -> Result<String, String>,
) -> ExitStatus {
    let Some(selector) = selector else {
        ctx.write_stderr(&format!("sched {subcommand}: expected a task id or name"))
            .ok();
        return ExitStatus::ExitedWith(1);
    };

    match action(proxy, selector) {
        Ok(message) => {
            ctx.write_stdout(&format!("sched: {message}")).ok();
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            ctx.write_stderr(&format!("sched: {err}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn set_paused<P: crate::shell_capabilities::ShellScheduling + ?Sized>(
    ctx: &Context,
    selector: Option<&String>,
    proxy: &mut P,
    paused: bool,
) -> ExitStatus {
    // No argument means the whole scheduler, which is the quickest way to stop
    // everything without losing the task list.
    let Some(selector) = selector else {
        proxy.sched_set_enabled(!paused);
        let state = if paused { "paused" } else { "resumed" };
        ctx.write_stdout(&format!("sched: scheduler {state}")).ok();
        return ExitStatus::ExitedWith(0);
    };

    let verb = if paused { "pause" } else { "resume" };
    match proxy.sched_set_paused(selector, paused) {
        Ok(name) => {
            let state = if paused { "paused" } else { "resumed" };
            ctx.write_stdout(&format!("sched: {name} {state}")).ok();
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            ctx.write_stderr(&format!("sched {verb}: {err}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Parses `sched add`'s arguments. Split out from [`add`] so the option
/// handling is testable without a shell.
pub(crate) fn parse_add(args: &[String], cwd: String) -> Result<SchedTaskSpec, String> {
    let mut name: Option<String> = None;
    let mut notify = NotifyPolicy::default();
    let mut timeout: Option<Duration> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--name" => {
                index += 1;
                name = Some(
                    args.get(index)
                        .ok_or("--name requires a value")?
                        .to_string(),
                );
            }
            "--on" => {
                index += 1;
                let value = args.get(index).ok_or("--on requires a value")?;
                notify = NotifyPolicy::parse(value)?;
            }
            "--quiet" => notify = NotifyPolicy::Never,
            "--timeout" => {
                index += 1;
                let value = args.get(index).ok_or("--timeout requires a value")?;
                timeout = Some(parse_interval(value)?.duration());
            }
            other if other.starts_with("--") => {
                return Err(format!("{other}: unknown option"));
            }
            // First non-option argument: the interval, and everything after it
            // is the command.
            _ => break,
        }
        index += 1;
    }

    let interval_arg = args.get(index).ok_or("expected an interval (e.g. 5m)")?;
    let interval = parse_interval(interval_arg)?;

    let command = args
        .get(index + 1..)
        .filter(|rest| !rest.is_empty())
        .ok_or("expected a command to run")?
        .join(" ");

    // Default the name to the command word, which is what you would reach for
    // anyway when there is only one task per tool.
    let name = name.unwrap_or_else(|| {
        command
            .split_whitespace()
            .next()
            .unwrap_or("task")
            .to_string()
    });

    // Never let a run outlast its own interval: the next one would be skipped
    // forever while the stuck one holds the slot.
    let timeout = timeout
        .unwrap_or(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .min(interval.duration());

    Ok(SchedTaskSpec {
        name,
        interval,
        command,
        cwd,
        notify,
        timeout,
    })
}

fn add<P: crate::shell_capabilities::ShellScheduling + ?Sized>(
    ctx: &Context,
    args: &[String],
    proxy: &mut P,
) -> ExitStatus {
    let cwd = match std::env::current_dir() {
        Ok(dir) => dir.to_string_lossy().into_owned(),
        Err(err) => {
            ctx.write_stderr(&format!("sched add: cannot read current directory: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let spec = match parse_add(args, cwd) {
        Ok(spec) => spec,
        Err(err) => {
            ctx.write_stderr(&format!("sched add: {err}")).ok();
            ctx.write_stderr(USAGE).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let summary = format!("{} every {} -> {}", spec.name, spec.interval, spec.command);
    match proxy.sched_add(spec) {
        Ok(id) => {
            ctx.write_stdout(&format!("sched: [{id}] {summary}")).ok();
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            ctx.write_stderr(&format!("sched add: {err}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

struct TaskRow {
    id: String,
    name: String,
    interval: String,
    state: String,
    next: String,
    last: String,
    runs: String,
    command: String,
}

impl Tabled for TaskRow {
    const LENGTH: usize = 8;

    fn fields(&self) -> Vec<Cow<'_, str>> {
        vec![
            Cow::Borrowed(&self.id),
            Cow::Borrowed(&self.name),
            Cow::Borrowed(&self.interval),
            Cow::Borrowed(&self.state),
            Cow::Borrowed(&self.next),
            Cow::Borrowed(&self.last),
            Cow::Borrowed(&self.runs),
            Cow::Borrowed(&self.command),
        ]
    }

    fn headers() -> Vec<Cow<'static, str>> {
        [
            "id", "name", "every", "state", "next", "last", "runs", "command",
        ]
        .into_iter()
        .map(Cow::Borrowed)
        .collect()
    }
}

pub(crate) fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

pub(crate) fn describe_last(view: &SchedTaskView) -> String {
    let Some(run) = view.last.as_ref() else {
        return "-".to_string();
    };
    let status = if run.timed_out {
        "timeout".to_string()
    } else if run.exit_code == 0 {
        "ok".to_string()
    } else {
        format!("exit {}", run.exit_code)
    };
    format!("{status} {:.1}s", run.duration_ms as f64 / 1000.0)
}

fn task_row(view: &SchedTaskView) -> TaskRow {
    let state = if view.running {
        "running"
    } else if view.paused {
        "paused"
    } else {
        "enabled"
    };
    TaskRow {
        id: view.id.to_string(),
        name: view.name.clone(),
        interval: view.interval.to_string(),
        state: state.to_string(),
        next: view.next_in.map_or("-".to_string(), format_duration),
        last: describe_last(view),
        runs: if view.fail_count > 0 {
            format!("{} ({} failed)", view.run_count, view.fail_count)
        } else {
            view.run_count.to_string()
        },
        command: view.command.clone(),
    }
}

fn list<P: crate::shell_capabilities::ShellScheduling + ?Sized>(
    ctx: &Context,
    flag: Option<&str>,
    proxy: &mut P,
) -> ExitStatus {
    if flag == Some("--lisp") {
        let lines = proxy.sched_as_lisp();
        if lines.is_empty() {
            ctx.write_stdout(";; no scheduled tasks").ok();
        }
        for line in lines {
            ctx.write_stdout(&line).ok();
        }
        return ExitStatus::ExitedWith(0);
    }

    if let Some(flag) = flag.filter(|flag| flag.starts_with('-')) {
        ctx.write_stderr(&format!("sched list: {flag}: unknown option"))
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    let views = proxy.sched_list();
    if views.is_empty() {
        ctx.write_stdout("No scheduled tasks. Try: sched add 5m 'git fetch --all'")
            .ok();
        return ExitStatus::ExitedWith(0);
    }

    let rows: Vec<TaskRow> = views.iter().map(task_row).collect();
    ctx.write_stdout(&Table::new(rows).to_string()).ok();

    if !proxy.sched_enabled() {
        ctx.write_stdout("(scheduler paused - `sched resume` to restart it)")
            .ok();
    }
    ExitStatus::ExitedWith(0)
}

fn log<P: crate::shell_capabilities::ShellScheduling + ?Sized>(
    ctx: &Context,
    selector: Option<&str>,
    proxy: &mut P,
) -> ExitStatus {
    let views = proxy.sched_list();
    let selected: Vec<&SchedTaskView> = match selector {
        Some(selector) => views
            .iter()
            .filter(|view| view.name == selector || view.id.to_string() == selector)
            .collect(),
        None => views.iter().collect(),
    };

    if selected.is_empty() {
        match selector {
            Some(selector) => {
                ctx.write_stderr(&format!("sched log: {selector}: no such task"))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
            None => {
                ctx.write_stdout("No scheduled tasks.").ok();
                return ExitStatus::ExitedWith(0);
            }
        }
    }

    for view in selected {
        ctx.write_stdout(&format!("{} ({})", view.name, view.command))
            .ok();
        if view.history.is_empty() {
            ctx.write_stdout("  (no runs yet)").ok();
            continue;
        }
        // Newest last, matching how a log reads.
        for run in &view.history {
            let status = if run.timed_out {
                "timeout".to_string()
            } else if run.exit_code == 0 {
                "ok".to_string()
            } else {
                format!("exit {}", run.exit_code)
            };
            let changed = if run.changed { " changed" } else { "" };
            ctx.write_stdout(&format!(
                "  {:>7}  {:>6.1}s{}  {}",
                status,
                run.duration_ms as f64 / 1000.0,
                changed,
                run.preview
            ))
            .ok();
        }
    }
    ExitStatus::ExitedWith(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn parse(list: &[&str]) -> Result<SchedTaskSpec, String> {
        parse_add(&args(list), "/tmp".to_string())
    }

    #[test]
    fn parses_the_minimal_form() {
        let spec = parse(&["5m", "git", "fetch"]).unwrap();
        assert_eq!(spec.interval.secs(), 300);
        assert_eq!(spec.command, "git fetch");
        assert_eq!(spec.cwd, "/tmp");
        assert_eq!(spec.notify, NotifyPolicy::Both);
    }

    #[test]
    fn the_name_defaults_to_the_command_word() {
        assert_eq!(parse(&["5m", "git", "fetch"]).unwrap().name, "git");
        assert_eq!(
            parse(&["--name", "prs", "5m", "gh", "pr", "list"])
                .unwrap()
                .name,
            "prs"
        );
    }

    #[test]
    fn parses_the_notify_policy() {
        assert_eq!(
            parse(&["--on", "change", "5m", "true"]).unwrap().notify,
            NotifyPolicy::OnChange
        );
        assert_eq!(
            parse(&["--quiet", "5m", "true"]).unwrap().notify,
            NotifyPolicy::Never
        );
        assert!(parse(&["--on", "sometimes", "5m", "true"]).is_err());
    }

    /// A run that outlasts its interval would hold the slot forever, so the
    /// timeout is clamped down to it.
    #[test]
    fn the_timeout_never_exceeds_the_interval() {
        let spec = parse(&["30s", "true"]).unwrap();
        assert_eq!(spec.timeout, Duration::from_secs(30));

        let spec = parse(&["--timeout", "10m", "1m", "true"]).unwrap();
        assert_eq!(spec.timeout, Duration::from_secs(60));

        let spec = parse(&["--timeout", "10s", "5m", "true"]).unwrap();
        assert_eq!(spec.timeout, Duration::from_secs(10));
    }

    #[test]
    fn the_default_timeout_applies_to_long_intervals() {
        let spec = parse(&["1h", "true"]).unwrap();
        assert_eq!(spec.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn the_whole_tail_becomes_the_command() {
        let spec = parse(&["5m", "sh", "-c", "echo hi"]).unwrap();
        assert_eq!(spec.command, "sh -c echo hi");
    }

    #[test]
    fn rejects_incomplete_invocations() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["5m"]).is_err());
        assert!(parse(&["notaninterval", "true"]).is_err());
        assert!(parse(&["--name"]).is_err());
        assert!(parse(&["--bogus", "5m", "true"]).is_err());
    }

    /// A command starting with a dash must not be eaten as an option: option
    /// parsing stops at the interval.
    #[test]
    fn options_after_the_interval_belong_to_the_command() {
        let spec = parse(&["5m", "ls", "--color=auto"]).unwrap();
        assert_eq!(spec.command, "ls --color=auto");
    }

    #[test]
    fn formats_durations_compactly() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(90), "1m30s");
        assert_eq!(format_duration(3700), "1h1m");
    }
}
