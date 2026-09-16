//! The `(pref-...)` setters: each reads the current value when called with no
//! argument and otherwise writes the preference through to the shell's
//! environment, so the config file and an interactive call behave the same way.
use super::*;

pub fn pref_auto_pair(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .completion_state
                .input_preferences
                .auto_pair,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting auto-pair to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_auto_pair_enabled(enabled);
    Ok(Value::NIL)
}

pub fn pref_auto_notify(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .completion_state
                .input_preferences
                .auto_notify_enabled,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting auto-notify to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_auto_notify_enabled(enabled);
    Ok(Value::NIL)
}

/// `(pref-status-line [t])` — read or set whether a status line is pinned to
/// the bottom row.
///
/// Takes effect at the next prompt. Off by default because it reserves a
/// DECSTBM scroll region, which not every terminal handles cleanly.
pub fn pref_status_line(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .completion_state
                .input_preferences
                .status_line,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting status-line to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_status_line_enabled(enabled);
    Ok(Value::NIL)
}

/// `(pref-diagnose-hint [t])` — read or set whether a failed command with no
/// deterministic quick fix points at `Alt-f`/`Alt-d`.
///
/// Costs nothing: the hint is a line of text, not an AI request, and it is
/// only offered when an AI service is configured. Off by default like the rest
/// of the automatic AI path, and only reachable at all once `pref-failure-hint`
/// is on - it is the branch that runs when nothing else had anything to say.
pub fn pref_diagnose_hint(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .completion_state
                .input_preferences
                .auto_diagnose,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting diagnose-hint to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_diagnose_hint_enabled(enabled);
    Ok(Value::NIL)
}

/// `(pref-failure-hint [t])` — read or set whether a failed command shows a
/// one-line proactive hint (deterministic quick fix ghost text, or a pointer
/// to Alt-f/Alt-d). On by default; the automatic path never sends an AI
/// request unless auto-fix is also enabled.
///
/// Turning it off disables the whole automatic post-failure path, automatic
/// AI fixes included, since all of it surfaces as that hint. `Alt-f` and
/// `Alt-d` keep working on demand.
pub fn pref_failure_hint(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .completion_state
                .input_preferences
                .failure_hint,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting failure-hint to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_failure_hint_enabled(enabled);
    Ok(Value::NIL)
}

/// `(pref-command-ledger ["off"|"metadata"|"output"])`.
/// Output capture is intentionally opt-in; all modes still use secret filtering.
pub fn pref_command_ledger(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::String(
            env.borrow()
                .shell_env
                .read()
                .variable_state
                .command_ledger_mode
                .as_str()
                .to_string(),
        ));
    }
    if args.len() != 1 {
        return Err(RuntimeError::new(
            "pref-command-ledger accepts zero or one argument",
        ));
    }
    let value = match &args[0] {
        Value::String(value) => value.as_str(),
        _ => {
            return Err(RuntimeError::new(
                "pref-command-ledger requires a string: off, metadata, or output",
            ));
        }
    };
    let mode = crate::history::CommandLedgerMode::parse(value).ok_or_else(|| {
        RuntimeError::new("pref-command-ledger requires off, metadata, or output")
    })?;
    env.borrow()
        .shell_env
        .write()
        .variable_state
        .command_ledger_mode = mode;
    Ok(Value::NIL)
}

pub fn pref_ai_explanation(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .completion_state
                .input_preferences
                .ai_explanation,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting ai-explanation to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_ai_explanation_enabled(enabled);
    Ok(Value::NIL)
}
