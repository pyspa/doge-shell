use crate::environment::Environment;
use crate::suggestion::SuggestionMode;
use crate::{
    lisp,
    lisp::{
        interpreter::eval,
        model::{Env, HashMapRc, IntType, List, RuntimeError, Symbol, Value},
        utils::{require_arg, require_typed_arg},
    },
};
use cfg_if::cfg_if;

use parking_lot::RwLock;
use std::sync::Arc;
use std::{cell::RefCell, collections::HashMap, convert::TryInto, rc::Rc};

use skim::prelude::*;
use std::borrow::Cow;

struct StringItem {
    text: String,
}

impl SkimItem for StringItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.text)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.text)
    }
}

/// Initialize an instance of `Env` with several core Lisp functions implemented
/// in Rust. **Without this, you will only have access to the functions you
/// implement yourself.**
pub fn default_env(environment: Arc<RwLock<Environment>>) -> Env {
    let mut env = Env::new(environment);

    crate::lisp::stdlib::register(&mut env);

    env.define(
        Symbol::from("edit"),
        Value::NativeFunc(crate::lisp::builtin::edit),
    );

    env.define(
        Symbol::from("register-action"),
        Value::NativeFunc(crate::lisp::command_palette::register_action),
    );

    env.define(
        Symbol::from("selector"),
        Value::NativeFunc(|_env, args| {
            let prompt = require_typed_arg::<&String>("selector", &args, 0)?.clone();
            let list_val = require_typed_arg::<&List>("selector", &args, 1)?;

            // Check for optional multi-select argument
            let multi = if args.len() > 2 {
                let val = &args[2];
                !(*val == Value::NIL || *val == Value::False)
            } else {
                false
            };

            let mut items = Vec::new();
            for val in list_val.into_iter() {
                if let Value::String(s) = val {
                    items.push(s.clone());
                } else {
                    return Err(RuntimeError {
                        msg: format!("selector requires a list of strings; found {}", val),
                    });
                }
            }

            let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
            for item in items {
                tx_item
                    .send(vec![Arc::new(StringItem { text: item })])
                    .map_err(|_| RuntimeError {
                        msg: "Failed to send item to skim".to_string(),
                    })?;
            }
            drop(tx_item);

            let options = SkimOptionsBuilder::default()
                .multi(multi)
                .prompt(prompt)
                .bind(vec!["Enter:accept".to_string(), "Esc:abort".to_string()])
                .build()
                .map_err(|e| RuntimeError {
                    msg: format!("Failed to build skim options: {}", e),
                })?;

            let selected_items = Skim::run_with(options, Some(rx_item))
                .ok()
                .map(|out| out.selected_items)
                .unwrap_or_default();

            if multi {
                let strings: Vec<Value> = selected_items
                    .iter()
                    .map(|item| Value::String(item.output().to_string()))
                    .collect();
                Ok(Value::List(strings.into_iter().collect()))
            } else if let Some(item) = selected_items.first() {
                Ok(Value::String(item.output().to_string()))
            } else {
                Ok(Value::NIL)
            }
        }),
    );

    env.define(
        Symbol::from("chat-execute-clear"),
        Value::NativeFunc(|env, _args| {
            env.borrow().shell_env.write().clear_execute_allowlist();
            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("chat-execute-add"),
        Value::NativeFunc(|env, args| {
            let env_ref = env.borrow();
            let shell_env = env_ref.shell_env.write();
            for arg in args {
                let command = match arg {
                    Value::String(s) => s.clone(),
                    _ => {
                        return Err(RuntimeError {
                            msg: format!(
                                "chat-execute-add requires all arguments to be strings; found {}",
                                arg
                            ),
                        });
                    }
                };
                shell_env.add_execute_allowlist_entry(command);
            }
            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("set-suggestion-mode"),
        Value::NativeFunc(|env, args| {
            let symbol = require_typed_arg::<&Symbol>("set-suggestion-mode", &args, 0)?;
            let mode_name = symbol.to_string();
            let mode = match mode_name.as_str() {
                "ghost" => SuggestionMode::Ghost,
                "off" => SuggestionMode::Off,
                other => {
                    return Err(RuntimeError {
                        msg: format!("Unknown suggestion mode: {other}"),
                    });
                }
            };
            env.borrow().shell_env.write().set_suggestion_mode(mode);
            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("set-suggestion-ai-enabled"),
        Value::NativeFunc(|env, args| {
            let flag = require_arg("set-suggestion-ai-enabled", &args, 0)?;
            let enabled = !(*flag == Value::NIL || *flag == Value::False);
            env.borrow()
                .shell_env
                .write()
                .set_suggestion_ai_enabled(enabled);
            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("set-auto-fix-enabled"),
        Value::NativeFunc(|env, args| {
            let flag = require_arg("set-auto-fix-enabled", &args, 0)?;
            let enabled = !(*flag == Value::NIL || *flag == Value::False);
            env.borrow().shell_env.write().set_auto_fix_enabled(enabled);
            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("set-notify-config"),
        Value::NativeFunc(|env, args| {
            let flag = require_arg("set-notify-config", &args, 0)?;
            let enabled = !(*flag == Value::NIL || *flag == Value::False);

            let env_ref = env.borrow();
            let mut env_write = env_ref.shell_env.write();
            env_write.set_auto_notify_enabled(enabled);

            if args.len() > 1 {
                let threshold_val = require_typed_arg::<IntType>("set-notify-config", &args, 1)?;
                use std::convert::TryInto;
                let threshold: u64 = threshold_val.try_into().map_err(|_| RuntimeError {
                    msg: "Threshold must be a non-negative integer".to_string(),
                })?;
                env_write.set_auto_notify_threshold(threshold);
            }

            Ok(Value::NIL)
        }),
    );

    // Define global hook variables
    env.define(Symbol::from("*pre-prompt-hooks*"), Value::List(List::NIL));

    env.define(Symbol::from("openai_usage_limit"), Value::NIL);

    // GitHub Integration
    env.define(Symbol::from("*github-pat*"), Value::NIL);
    env.define(
        Symbol::from("*github-notify-interval*"),
        Value::from("60".to_string()),
    );
    env.define(Symbol::from("*github-notifications-filter*"), Value::NIL);
    env.define(Symbol::from("*github-icon*"), Value::from("🐙".to_string()));

    env.define(Symbol::from("*pre-exec-hooks*"), Value::List(List::NIL));

    env.define(Symbol::from("*post-exec-hooks*"), Value::List(List::NIL));

    env.define(Symbol::from("*on-chdir-hooks*"), Value::List(List::NIL));

    // New enhanced hooks
    env.define(
        Symbol::from("*command-not-found-hooks*"),
        Value::List(List::NIL),
    );

    env.define(Symbol::from("*completion-hooks*"), Value::List(List::NIL));

    env.define(
        Symbol::from("*input-timeout-hooks*"),
        Value::List(List::NIL),
    );

    // Define add-hook function
    env.define(
        Symbol::from("add-hook"),
        Value::NativeFunc(|env, args| {
            if args.len() != 2 {
                return Err(RuntimeError {
                    msg: "add-hook requires exactly 2 arguments: hook-name and function"
                        .to_string(),
                });
            }

            let hook_name = require_typed_arg::<&Symbol>("add-hook", &args, 0)?;
            let func = args[1].clone();

            // Get the hook variable name
            let hook_var_name = Symbol::from(format!("*{}*", hook_name).as_str());

            // Get the current value of the hook variable
            let current_value = match env.borrow().get(&hook_var_name) {
                Some(Value::List(list)) => list,
                Some(_) => {
                    return Err(RuntimeError {
                        msg: format!("{} is not a hook variable", hook_var_name).to_string(),
                    });
                }
                None => {
                    return Err(RuntimeError {
                        msg: format!("Hook variable {} does not exist", hook_var_name).to_string(),
                    });
                }
            };

            // Add the function to the hook list
            let new_list = current_value.cons(func);
            let new_list_value = Value::List(new_list);

            // Set the new value in the environment using set to update existing binding
            match env
                .borrow_mut()
                .set(hook_var_name.clone(), new_list_value.clone())
            {
                Ok(_) => {}
                Err(_) => {
                    // If set fails (variable not found), define it in current env
                    env.borrow_mut().define(hook_var_name, new_list_value);
                }
            }

            Ok(Value::NIL)
        }),
    );

    // Define bound? function to check if a symbol is bound
    env.define(
        Symbol::from("bound?"),
        Value::NativeFunc(|env, args| {
            if args.len() != 1 {
                return Err(RuntimeError {
                    msg: "bound? requires exactly 1 argument: symbol".to_string(),
                });
            }

            let symbol = require_typed_arg::<&Symbol>("bound?", &args, 0)?;

            match env.borrow().get(symbol) {
                Some(_) => Ok(Value::True),
                None => Ok(Value::NIL),
            }
        }),
    );

    env.define(
        Symbol::from("map"),
        Value::NativeFunc(|env, args| {
            let func = require_arg("map", &args, 0)?;
            let list = require_typed_arg::<&List>("map", &args, 1)?;

            list.into_iter()
                .map(|val| {
                    let expr = lisp! { ({func.clone()} (quote {val})) };

                    eval(env.clone(), &expr)
                })
                .collect::<Result<List, RuntimeError>>()
                .map(Value::List)
        }),
    );

    // 🦀 Oh the poor `filter`, you must feel really sad being unused.
    env.define(
        Symbol::from("filter"),
        Value::NativeFunc(|env, args| {
            let func = require_arg("filter", &args, 0)?;
            let list = require_typed_arg::<&List>("filter", &args, 1)?;

            list.into_iter()
                .filter_map(|val: Value| -> Option<Result<Value, RuntimeError>> {
                    let expr = lisp! { ({func.clone()} (quote {val.clone()})) };

                    match eval(env.clone(), &expr) {
                        Ok(matches) => {
                            if matches.into() {
                                Some(Ok(val))
                            } else {
                                None
                            }
                        }
                        Err(e) => Some(Err(e)),
                    }
                })
                .collect::<Result<List, RuntimeError>>()
                .map(Value::List)
        }),
    );

    env.define(
        Symbol::from("length"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("length", &args, 0)?;

            cfg_if! {
                if #[cfg(feature = "bigint")] {
                    Ok(Value::Int(list.into_iter().len().into()))
                } else {
                    Ok(Value::Int(list.into_iter().len() as IntType))
                }
            }
        }),
    );

    env.define(
        Symbol::from("hash"),
        Value::NativeFunc(|_env, args| {
            let chunks = args.chunks(2);

            #[allow(clippy::mutable_key_type)]
            let mut hash = HashMap::new();

            for pair in chunks {
                let key = pair.first().unwrap();
                let value = pair.get(1);

                if let Some(value) = value {
                    hash.insert(key.clone(), value.clone());
                } else {
                    return Err(RuntimeError {
                        msg: format!("Must pass an even number of arguments to 'hash', because they're used as key/value pairs; found extra argument {key}")
                    });
                }
            }

            Ok(Value::HashMap(Rc::new(RefCell::new(hash))))
        }),
    );

    env.define(
        Symbol::from("hash_get"),
        Value::NativeFunc(|_env, args| {
            let hash = require_typed_arg::<&HashMapRc>("hash_get", &args, 0)?;
            let key = require_arg("hash_get", &args, 1)?;

            Ok(hash.borrow().get(key).cloned().unwrap_or(Value::NIL))
        }),
    );

    env.define(
        Symbol::from("hash_set"),
        Value::NativeFunc(|_env, args| {
            let hash = require_typed_arg::<&HashMapRc>("hash_set", &args, 0)?;
            let key = require_arg("hash_set", &args, 1)?;
            let value = require_arg("hash_set", &args, 2)?;

            hash.borrow_mut().insert(key.clone(), value.clone());

            Ok(Value::HashMap(hash.clone()))
        }),
    );

    env.define(
        Symbol::from("not"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("not", &args, 0)?;
            let a: bool = a.into();

            Ok(Value::from(!a))
        }),
    );

    env.define(
        Symbol::from("=="),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("==", &args, 0)?;
            let b = require_arg("==", &args, 1)?;

            Ok(Value::from(a == b))
        }),
    );

    env.define(
        Symbol::from("!="),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("!=", &args, 0)?;
            let b = require_arg("!=", &args, 1)?;

            Ok(Value::from(a != b))
        }),
    );

    env.define(
        Symbol::from("<"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("<", &args, 0)?;
            let b = require_arg("<", &args, 1)?;

            Ok(Value::from(a < b))
        }),
    );

    env.define(
        Symbol::from("<="),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("<=", &args, 0)?;
            let b = require_arg("<=", &args, 1)?;

            Ok(Value::from(a <= b))
        }),
    );

    env.define(
        Symbol::from(">"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg(">", &args, 0)?;
            let b = require_arg(">", &args, 1)?;

            Ok(Value::from(a > b))
        }),
    );

    env.define(
        Symbol::from(">="),
        Value::NativeFunc(|_env, args| {
            let a = require_arg(">=", &args, 0)?;
            let b = require_arg(">=", &args, 1)?;

            Ok(Value::from(a >= b))
        }),
    );

    env.define(
        Symbol::from("eval"),
        Value::NativeFunc(|env, args| {
            let expr = require_arg("eval", &args, 0)?;

            eval(env, expr)
        }),
    );

    env.define(
        Symbol::from("apply"),
        Value::NativeFunc(|env, args| {
            let func = require_arg("apply", &args, 0)?;
            let params = require_typed_arg::<&List>("apply", &args, 1)?;

            eval(env, &Value::List(params.cons(func.clone())))
        }),
    );

    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::lisp::LispEngine;

    fn run(engine: &Rc<RefCell<LispEngine>>, expr: &str) {
        engine.borrow().run(expr).unwrap();
    }

    #[test]
    fn execute_allowlist_helpers_manage_entries() {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        run(&engine, "(chat-execute-clear)");
        run(&engine, "(chat-execute-add \"ls\")");
        run(&engine, "(chat-execute-add \"git\")");
        run(&engine, "(chat-execute-add \"ls\")"); // duplicate should be ignored

        let env_lock = env.read();
        let allowlist = env_lock.execute_allowlist();
        assert_eq!(allowlist, &["ls", "git"]);

        drop(env_lock);
        run(&engine, "(chat-execute-clear)");
        let env_lock = env.read();
        let allowlist_after = env_lock.execute_allowlist();
        assert!(allowlist_after.is_empty());
    }
    #[test]
    fn chat_execute_add_multiple_commands_single_call() {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        run(&engine, "(chat-execute-clear)");
        run(
            &engine,
            "(chat-execute-add \"ls\" \"cat\" \"grep\" \"find\")",
        );

        let env_lock = env.read();
        let allowlist = env_lock.execute_allowlist();

        // Verify that all expected commands are present regardless of order
        assert!(allowlist.contains(&"ls".to_string()));
        assert!(allowlist.contains(&"cat".to_string()));
        assert!(allowlist.contains(&"grep".to_string()));
        assert!(allowlist.contains(&"find".to_string()));
        assert_eq!(allowlist.len(), 4); // Ensure no duplicates were added

        drop(env_lock);
        run(&engine, "(chat-execute-clear)");
    }

    #[test]
    fn mcp_list_tools_returns_nil_when_empty() {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        // Call mcp-list-tools
        let result = engine.borrow().run("(mcp-list-tools)").unwrap();

        // It should return NIL (empty list of tools)
        assert_eq!(result, Value::List(List::NIL));
    }

    #[test]
    fn selector_validation_checks() {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        // Case 1: Too few arguments (missing list)
        // (selector "Prompt") -> expects arg at index 1
        let result = engine.borrow().run("(selector \"Prompt\")");
        assert!(result.is_err());

        // Case 2: Invalid first argument (not a string)
        let result = engine.borrow().run("(selector 123 '(\"a\"))");
        assert!(result.is_err());

        // Case 3: Invalid second argument (not a list)
        let result = engine.borrow().run("(selector \"Prompt\" \"not-a-list\")");
        assert!(result.is_err());

        // Case 4: List contains non-string items
        let result = engine.borrow().run("(selector \"Prompt\" '(\"a\" 123))");
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(
                e.to_string()
                    .contains("selector requires a list of strings")
            );
        }
    }
}
