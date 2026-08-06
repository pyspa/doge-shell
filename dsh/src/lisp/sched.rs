//! `sched-add` and friends for `config.lisp`.
//!
//! Scheduled tasks do not persist across sessions, so putting `sched-add` calls
//! in `config.lisp` is how a set of tasks is made permanent. `sched list --lisp`
//! prints exactly these calls for the tasks registered right now.

use crate::lisp::model::{Env, List, RuntimeError, Value};
use crate::lisp::utils::require_typed_arg;
use dsh_types::schedule::{DEFAULT_TIMEOUT_SECS, NotifyPolicy, SchedTaskSpec, parse_interval};
use std::time::Duration;
use std::{cell::RefCell, rc::Rc};

/// `(sched-add "<name>" "<interval>" "<command>" ["<notify-policy>"])`
///
/// The working directory is the one in effect when this runs — for
/// `config.lisp` that is wherever the shell started, so prefer absolute paths
/// or a `cd` inside the command.
pub fn sched_add(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 3 || args.len() > 4 {
        return Err(RuntimeError::new(
            "sched-add requires 3 or 4 arguments: name, interval, command, [notify]",
        ));
    }

    let name = require_typed_arg::<&String>("sched-add", &args, 0)?.clone();
    let interval_spec = require_typed_arg::<&String>("sched-add", &args, 1)?.clone();
    let command = require_typed_arg::<&String>("sched-add", &args, 2)?.clone();

    let interval = parse_interval(&interval_spec)
        .map_err(|err| RuntimeError::new(format!("sched-add: {err}").as_str()))?;

    let notify = if args.len() == 4 {
        let policy = require_typed_arg::<&String>("sched-add", &args, 3)?;
        NotifyPolicy::parse(policy)
            .map_err(|err| RuntimeError::new(format!("sched-add: {err}").as_str()))?
    } else {
        NotifyPolicy::default()
    };

    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_string());

    // Same clamp as the builtin: a run must not outlast its own interval.
    let timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS).min(interval.duration());

    let spec = SchedTaskSpec {
        name,
        interval,
        command,
        cwd,
        notify,
        timeout,
    };

    let id = env
        .borrow()
        .shell_env
        .write()
        .sched_add(spec)
        .map_err(|err| RuntimeError::new(format!("sched-add: {err}").as_str()))?;

    Ok(Value::Int(id as i64))
}

/// `(sched-remove "<name-or-id>")`
pub fn sched_remove(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let selector = single_selector("sched-remove", &args)?;
    let removed = env.borrow().shell_env.write().sched_remove(&selector);
    Ok(Value::from(removed.is_ok()))
}

/// `(sched-pause "<name-or-id>")`
pub fn sched_pause(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let selector = single_selector("sched-pause", &args)?;
    let result = env
        .borrow()
        .shell_env
        .write()
        .sched_set_paused(&selector, true);
    Ok(Value::from(result.is_ok()))
}

/// `(sched-resume "<name-or-id>")`
pub fn sched_resume(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let selector = single_selector("sched-resume", &args)?;
    let result = env
        .borrow()
        .shell_env
        .write()
        .sched_set_paused(&selector, false);
    Ok(Value::from(result.is_ok()))
}

/// `(sched-list)` — one description string per task.
pub fn sched_list(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(RuntimeError::new("sched-list takes no arguments"));
    }

    let lines: Vec<Value> = env
        .borrow()
        .shell_env
        .read()
        .sched_descriptions()
        .into_iter()
        .map(Value::from)
        .collect();

    Ok(Value::List(lines.into_iter().collect::<List>()))
}

fn single_selector(name: &'static str, args: &[Value]) -> Result<String, RuntimeError> {
    if args.len() != 1 {
        return Err(RuntimeError::new(
            format!("{name} requires exactly 1 argument: name or id").as_str(),
        ));
    }
    Ok(require_typed_arg::<&String>(name, args, 0)?.clone())
}
