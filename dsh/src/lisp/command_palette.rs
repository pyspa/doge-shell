use crate::command_palette::REGISTRY;
use crate::command_palette::actions::LispAction;
use crate::lisp::model::{Env, RuntimeError, Value};
use crate::lisp::utils::require_typed_arg;
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// Lisp builtin to register a new action in the Command Palette.
/// Usage: (register-action "Name" "Description" "lisp-function-name")
pub fn register_action(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() != 3 {
        return Err(RuntimeError::new(
            "register-action requires exactly 3 arguments: name, description, and function-name",
        ));
    }

    let name = require_typed_arg::<&String>("register-action", &args, 0)?.clone();
    let description = require_typed_arg::<&String>("register-action", &args, 1)?.clone();
    let function_name = require_typed_arg::<&String>("register-action", &args, 2)?.clone();

    let action = Arc::new(LispAction {
        name,
        description,
        function_name,
    });

    REGISTRY.write().register(action);

    Ok(Value::NIL)
}
