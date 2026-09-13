//! `cron-add` and friends for `config.lisp`.
//!
//! `sched-add`, kept here as a deprecated alias, used to register an
//! in-memory task that vanished at exit; `config.lisp` running it again on
//! every launch was therefore always registering, never re-registering.
//! `cron-add` persists to the same on-disk store the `cron` builtin uses, so
//! it has to behave differently on a second run: it **upserts by name**,
//! replacing the job rather than erroring or duplicating it. The `cron add`
//! builtin keeps the stricter behaviour (refuses a duplicate name without
//! `--force`) because a person typing that command a second time is far more
//! likely to have mistyped than to be idempotently re-declaring one.

use crate::cron::cli::parse_schedule_arg;
use crate::lisp::model::{Env, List, RuntimeError, Value};
use crate::lisp::utils::require_typed_arg;
use dsh_builtin::config_paths;
use dsh_builtin::shell_capabilities::CronStore;
use dsh_types::cron::job::{CronJobSpec, JobKind};
use dsh_types::schedule::NotifyPolicy;
use std::{cell::RefCell, rc::Rc};

fn open_store() -> Result<crate::cron::store::SqliteCronStore, RuntimeError> {
    crate::cron::store::SqliteCronStore::open(&config_paths::cron_state_dir())
        .map_err(|err| RuntimeError::new(format!("cron: {err}").as_str()))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `(cron-add "<name>" "<schedule>" "<command>" ["<notify-policy>"])`
///
/// `<schedule>` accepts everything `cron add` does: `30s`/`5m`/`1h`, a
/// five-field expression, or an `@` macro. The working directory is the one
/// in effect when this runs — for `config.lisp` that is wherever the shell
/// started, so prefer absolute paths or a `cd` inside the command.
///
/// Registering the same name twice **replaces** the job (upsert), which is
/// what makes it safe to leave in `config.lisp`: every launch runs this line
/// again.
pub fn cron_add(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(RuntimeError::new(
            "cron-add requires 3 or 4 arguments: name, schedule, command, [notify]",
        ));
    }

    let name = require_typed_arg::<&String>("cron-add", &args, 0)?.clone();
    // `cron add`'s `--name` enforces this same bound (`cli::parse::MAX_NAME_LEN`)
    // because a job's name becomes a notepad and a lease-file name; this is
    // the only other place a `CronJobSpec::name` is built, and skipping the
    // check here would let `(cron-add "" ...)` or an oversized name reach the
    // store unvalidated.
    if name.is_empty() || name.len() > crate::cron::cli::MAX_NAME_LEN {
        return Err(RuntimeError::new(
            format!(
                "cron-add: name must be 1-{} characters",
                crate::cron::cli::MAX_NAME_LEN
            )
            .as_str(),
        ));
    }
    let schedule_spec = require_typed_arg::<&String>("cron-add", &args, 1)?.clone();
    let command = require_typed_arg::<&String>("cron-add", &args, 2)?.clone();

    let schedule = parse_schedule_arg(&schedule_spec)
        .map_err(|err| RuntimeError::new(format!("cron-add: {err}").as_str()))?;

    let notify = if args.len() == 4 {
        let policy = require_typed_arg::<&String>("cron-add", &args, 3)?;
        NotifyPolicy::parse(policy)
            .map_err(|err| RuntimeError::new(format!("cron-add: {err}").as_str()))?
    } else {
        NotifyPolicy::default()
    };

    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());

    let mut timeout_secs = dsh_types::schedule::DEFAULT_TIMEOUT_SECS;
    if let dsh_types::schedule::Schedule::Every(interval) = schedule {
        // Same clamp the builtin applies: a run must not outlast its own
        // interval, or the next one is starved forever.
        timeout_secs = timeout_secs.min(interval.secs());
    }

    let spec = CronJobSpec {
        name,
        schedule,
        schedule_spec,
        kind: JobKind::Sh,
        command,
        agent: None,
        cwd,
        notify,
        timeout_secs,
        catchup_secs: crate::cron::cli::DEFAULT_CATCHUP_SECS,
        paused: false,
    };

    let store = open_store()?;
    let id = store
        .upsert(&spec, &std::env::vars().collect(), now())
        .map_err(|err| RuntimeError::new(format!("cron-add: {err}").as_str()))?;

    Ok(Value::Int(id))
}

/// `(cron-remove "<name-or-id>")`
pub fn cron_remove(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let selector = single_selector("cron-remove", &args)?;
    let store = open_store()?;
    Ok(Value::from(store.delete(&selector).is_ok()))
}

/// `(cron-pause "<name-or-id>")`
pub fn cron_pause(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let selector = single_selector("cron-pause", &args)?;
    let store = open_store()?;
    Ok(Value::from(
        store.set_paused(&selector, true, now()).is_ok(),
    ))
}

/// `(cron-resume "<name-or-id>")`
pub fn cron_resume(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let selector = single_selector("cron-resume", &args)?;
    let store = open_store()?;
    Ok(Value::from(
        store.set_paused(&selector, false, now()).is_ok(),
    ))
}

/// `(cron-list)` — one description string per job.
pub fn cron_list(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(RuntimeError::new("cron-list takes no arguments"));
    }

    let store = open_store()?;
    let jobs = store
        .list()
        .map_err(|err| RuntimeError::new(format!("cron-list: {err}").as_str()))?;

    let lines: Vec<Value> = jobs
        .into_iter()
        .map(|job| {
            Value::from(format!(
                "{} {} -> {}{}",
                job.name,
                job.schedule_spec,
                job.command,
                if job.paused { " (paused)" } else { "" }
            ))
        })
        .collect();

    Ok(Value::List(lines.into_iter().collect::<List>()))
}

/// `(sched-add "<name>" "<interval>" "<command>" ["<notify-policy>"])` —
/// deprecated, kept for one release so an existing `config.lisp` does not
/// stop partway through and silently drop the aliases and exports that used
/// to come after it. Forwards straight to [`cron_add`]'s upsert.
pub fn sched_add(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    eprintln!(
        "warning: sched-add is deprecated; replace with cron-add in your config.lisp \
         (same arguments, and it upserts by name the same way)"
    );
    cron_add(env, args)
}

/// `(sched-remove "<name-or-id>")` — deprecated, forwards to [`cron_remove`].
///
/// Kept for the same reason as [`sched_add`]: an undefined symbol aborts the
/// rest of `config.lisp`, silently dropping every alias/abbr/PATH line after
/// it, not just this one call.
pub fn sched_remove(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    eprintln!("warning: sched-remove is deprecated; replace with cron-remove in your config.lisp");
    cron_remove(env, args)
}

/// `(sched-pause "<name-or-id>")` — deprecated, forwards to [`cron_pause`].
pub fn sched_pause(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    eprintln!("warning: sched-pause is deprecated; replace with cron-pause in your config.lisp");
    cron_pause(env, args)
}

/// `(sched-resume "<name-or-id>")` — deprecated, forwards to [`cron_resume`].
pub fn sched_resume(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    eprintln!("warning: sched-resume is deprecated; replace with cron-resume in your config.lisp");
    cron_resume(env, args)
}

/// `(sched-list)` — deprecated, forwards to [`cron_list`].
pub fn sched_list(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    eprintln!("warning: sched-list is deprecated; replace with cron-list in your config.lisp");
    cron_list(env, args)
}

fn single_selector(name: &'static str, args: &[Value]) -> Result<String, RuntimeError> {
    if args.len() != 1 {
        return Err(RuntimeError::new(
            format!("{name} requires exactly 1 argument: name or id").as_str(),
        ));
    }
    Ok(require_typed_arg::<&String>(name, args, 0)?.clone())
}
