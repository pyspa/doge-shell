//! `bind` / `unbind` / `list-bindings` for `config.lisp`.

use crate::lisp::model::{Env, List, RuntimeError, Value};
use crate::lisp::utils::require_typed_arg;
use crate::repl::keybind::{BoundAction, action_name, chord};
use std::{cell::RefCell, rc::Rc};

/// `(bind "<key>" "<action-or-lisp-function>")`
///
/// The second argument names either a built-in action (`"cancel-completion"`)
/// or a Lisp function. Built-ins win when a name is both, so a user function
/// cannot silently shadow a shell action.
///
/// A bound key takes effect unconditionally, replacing any context-sensitive
/// built-in behaviour that key had.
pub fn bind(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(RuntimeError::new(
            "bind requires exactly 2 arguments: key and action",
        ));
    }

    let key = require_typed_arg::<&String>("bind", &args, 0)?.clone();
    let target = require_typed_arg::<&String>("bind", &args, 1)?.clone();

    let chord = chord::parse_chord(&key)
        .map_err(|err| RuntimeError::new(format!("bind: {key}: {err}").as_str()))?;

    let action = match action_name::action_from_name(&target) {
        Some(action) => BoundAction::Action(action),
        None => BoundAction::Lisp(target),
    };

    env.borrow()
        .shell_env
        .write()
        .set_key_binding(chord, action);

    Ok(Value::NIL)
}

/// `(unbind "<key>")` — drop a binding so the key falls back to its built-in
/// meaning. Returns true when there was one to remove.
pub fn unbind(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(RuntimeError::new("unbind requires exactly 1 argument: key"));
    }

    let key = require_typed_arg::<&String>("unbind", &args, 0)?.clone();
    let chord = chord::parse_chord(&key)
        .map_err(|err| RuntimeError::new(format!("unbind: {key}: {err}").as_str()))?;

    let removed = env.borrow().shell_env.write().remove_key_binding(&chord);
    Ok(Value::from(removed))
}

/// `(list-bindings)` — the configured bindings as `"key -> action"` strings.
pub fn list_bindings(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(RuntimeError::new("list-bindings takes no arguments"));
    }

    let lines: Vec<Value> = env
        .borrow()
        .shell_env
        .read()
        .key_binding_descriptions()
        .into_iter()
        .map(Value::from)
        .collect();

    Ok(Value::List(lines.into_iter().collect::<List>()))
}

/// `(list-bind-actions)` — every name `bind` accepts as a built-in action.
pub fn list_bind_actions(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(RuntimeError::new("list-bind-actions takes no arguments"));
    }

    let names: Vec<Value> = action_name::all_names()
        .into_iter()
        .map(|name| Value::from(name.to_string()))
        .collect();

    Ok(Value::List(names.into_iter().collect::<List>()))
}
