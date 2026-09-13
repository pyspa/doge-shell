//! The `(secret-...)` forms: extending the secret-detection patterns and
//! keywords, listing them back, choosing how history treats a line holding a
//! secret, and the per-session secret store.
use super::*;

/// Add a regex pattern for secret detection
/// Usage: (secret-add-pattern "MY_CUSTOM_.*")
pub fn secret_add_pattern(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new(
            "secret-add-pattern requires at least 1 argument: pattern",
        ));
    }

    let pattern = args[0].to_string();
    env.borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .add_pattern(&pattern)
        .map_err(|e| RuntimeError::new(&e))?;

    debug!("Added secret pattern: {}", pattern);
    Ok(Value::NIL)
}

/// Add a keyword for secret detection
/// Usage: (secret-add-keyword "MY_SECRET")
pub fn secret_add_keyword(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new(
            "secret-add-keyword requires at least 1 argument: keyword",
        ));
    }

    let keyword = args[0].to_string();
    env.borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .add_keyword(&keyword);

    debug!("Added secret keyword: {}", keyword);
    Ok(Value::NIL)
}

/// List registered secret patterns
/// Usage: (secret-list-patterns)
pub fn secret_list_patterns(
    env: Rc<RefCell<Env>>,
    _args: Vec<Value>,
) -> Result<Value, RuntimeError> {
    let patterns = env
        .borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .list_patterns();

    let result: Vec<Value> = patterns.into_iter().map(Value::String).collect();
    Ok(Value::List(result.into_iter().collect()))
}

/// Set history mode for secrets
/// Usage: (secret-history-mode "skip") ; or "redact" or "none"
pub fn secret_history_mode(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    use crate::secrets::SecretHistoryMode;

    if args.is_empty() {
        // Return current mode
        let mode = env
            .borrow()
            .shell_env
            .read()
            .policy_state
            .secret_manager
            .history_mode();
        let mode_str = match mode {
            SecretHistoryMode::Skip => "skip",
            SecretHistoryMode::Redact => "redact",
            SecretHistoryMode::None => "none",
        };
        return Ok(Value::String(mode_str.to_string()));
    }

    let mode_str = args[0].to_string();
    let mode: SecretHistoryMode = mode_str
        .parse()
        .map_err(|e| RuntimeError::new(&format!("Invalid mode: {}", e)))?;

    env.borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .set_history_mode(mode);

    debug!("Set secret history mode to: {}", mode_str);
    Ok(Value::NIL)
}

/// Set a session-only secret
/// Usage: (secret-set "DB_PASS" "secret123")
pub fn secret_set(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::new(
            "secret-set requires 2 arguments: key and value",
        ));
    }

    let key = args[0].to_string();
    let value = args[1].to_string();

    env.borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .set_session_secret(&key, &value);

    debug!("Set session secret: {}", key);
    Ok(Value::NIL)
}

/// Get a session-only secret
/// Usage: (secret-get "DB_PASS")
pub fn secret_get(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new("secret-get requires 1 argument: key"));
    }

    let key = args[0].to_string();

    let value = env
        .borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .get_session_secret(&key);

    match value {
        Some(v) => Ok(Value::String(v)),
        None => Ok(Value::NIL),
    }
}

/// Clear all session secrets
/// Usage: (secret-clear)
pub fn secret_clear(env: Rc<RefCell<Env>>, _args: Vec<Value>) -> Result<Value, RuntimeError> {
    env.borrow()
        .shell_env
        .read()
        .policy_state
        .secret_manager
        .clear_session_secrets();

    debug!("Cleared all session secrets");
    Ok(Value::NIL)
}
