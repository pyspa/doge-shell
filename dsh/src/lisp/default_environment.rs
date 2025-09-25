use crate::environment::Environment;
use crate::{
    lisp,
    lisp::{
        interpreter::eval,
        model::{Env, HashMapRc, IntType, List, RuntimeError, Symbol, Value},
        utils::{require_arg, require_typed_arg},
    },
};
use cfg_if::cfg_if;
use dsh_types::mcp::{McpServerConfig, McpTransport};
use parking_lot::RwLock;
use std::sync::Arc;
use std::{cell::RefCell, collections::HashMap, convert::TryInto, rc::Rc};

/// Initialize an instance of `Env` with several core Lisp functions implemented
/// in Rust. **Without this, you will only have access to the functions you
/// implement yourself.**
pub fn default_env(environment: Arc<RwLock<Environment>>) -> Env {
    let mut env = Env::new(environment);

    env.define(
        Symbol::from("print"),
        Value::NativeFunc(|_env, args| {
            let expr = require_arg("print", &args, 0)?;

            println!("{}", &expr);
            Ok(expr.clone())
        }),
    );

    env.define(
        Symbol::from("is_null"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_null", &args, 0)?;

            Ok(Value::from(*val == Value::NIL))
        }),
    );

    env.define(
        Symbol::from("is_number"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_number", &args, 0)?;

            Ok(match val {
                Value::Int(_) => Value::True,
                Value::Float(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_symbol"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_symbol", &args, 0)?;

            Ok(match val {
                Value::Symbol(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_boolean"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_boolean", &args, 0)?;

            Ok(match val {
                Value::True => Value::True,
                Value::False => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_procedure"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_procedure", &args, 0)?;

            Ok(match val {
                Value::Lambda(_) => Value::True,
                Value::NativeFunc(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_pair"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_pair", &args, 0)?;

            Ok(match val {
                Value::List(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("car"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("car", &args, 0)?;

            list.car()
        }),
    );

    env.define(
        Symbol::from("cdr"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("cdr", &args, 0)?;

            Ok(Value::List(list.cdr()))
        }),
    );

    env.define(
        Symbol::from("cons"),
        Value::NativeFunc(|_env, args| {
            let car = require_arg("cons", &args, 0)?;
            let cdr = require_typed_arg::<&List>("cons", &args, 1)?;

            Ok(Value::List(cdr.cons(car.clone())))
        }),
    );

    env.define(
        Symbol::from("list"),
        Value::NativeFunc(|_env, args| Ok(Value::List(args.iter().collect::<List>()))),
    );

    env.define(
        Symbol::from("nth"),
        Value::NativeFunc(|_env, args| {
            let index = require_typed_arg::<IntType>("nth", &args, 0)?;
            let list = require_typed_arg::<&List>("nth", &args, 1)?;

            let index = TryInto::<usize>::try_into(index).map_err(|_| RuntimeError {
                msg: "Failed converting to `usize`".to_owned(),
            })?;

            Ok(list.into_iter().nth(index).unwrap_or(Value::NIL))
        }),
    );

    env.define(
        Symbol::from("sort"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("sort", &args, 0)?;

            let mut v: Vec<Value> = list.into_iter().collect();

            v.sort();

            Ok(Value::List(v.into_iter().collect()))
        }),
    );

    env.define(
        Symbol::from("reverse"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("reverse", &args, 0)?;

            let mut v: Vec<Value> = list.into_iter().collect();

            v.reverse();

            Ok(Value::List(v.into_iter().collect()))
        }),
    );

    env.define(
        Symbol::from("mcp-clear"),
        Value::NativeFunc(|env, _args| {
            env.borrow().shell_env.write().clear_mcp_servers();
            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("mcp-add-stdio"),
        Value::NativeFunc(|env, args| {
            let label = require_typed_arg::<&String>("mcp-add-stdio", &args, 0)?.clone();
            let command = require_typed_arg::<&String>("mcp-add-stdio", &args, 1)?.clone();
            let arg_list = args.get(2).cloned().unwrap_or(Value::NIL);
            let env_list = args.get(3).cloned().unwrap_or(Value::NIL);
            let cwd_value = args.get(4).cloned().unwrap_or(Value::NIL);
            let description_value = args.get(5).cloned().unwrap_or(Value::NIL);

            let args_vec = list_of_strings("mcp-add-stdio", &arg_list)?;
            let env_map = list_of_pairs("mcp-add-stdio", &env_list)?;
            let cwd = optional_string("mcp-add-stdio", &cwd_value)?;
            let description = optional_string("mcp-add-stdio", &description_value)?;

            let transport = McpTransport::Stdio {
                command,
                args: args_vec,
                env: env_map,
                cwd: cwd.map(Into::into),
            };

            env.borrow()
                .shell_env
                .write()
                .add_mcp_server(McpServerConfig {
                    label,
                    description,
                    transport,
                });

            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("mcp-add-sse"),
        Value::NativeFunc(|env, args| {
            let label = require_typed_arg::<&String>("mcp-add-sse", &args, 0)?.clone();
            let url = require_typed_arg::<&String>("mcp-add-sse", &args, 1)?.clone();
            let description_value = args.get(2).cloned().unwrap_or(Value::NIL);
            let description = optional_string("mcp-add-sse", &description_value)?;

            env.borrow()
                .shell_env
                .write()
                .add_mcp_server(McpServerConfig {
                    label,
                    description,
                    transport: McpTransport::Sse { url },
                });

            Ok(Value::NIL)
        }),
    );

    env.define(
        Symbol::from("mcp-add-http"),
        Value::NativeFunc(|env, args| {
            let label = require_typed_arg::<&String>("mcp-add-http", &args, 0)?.clone();
            let url = require_typed_arg::<&String>("mcp-add-http", &args, 1)?.clone();
            let auth_value = args.get(2).cloned().unwrap_or(Value::NIL);
            let allow_value = args.get(3).cloned().unwrap_or(Value::NIL);
            let description_value = args.get(4).cloned().unwrap_or(Value::NIL);

            let auth_header = optional_string("mcp-add-http", &auth_value)?;
            let allow_stateless = optional_bool("mcp-add-http", &allow_value)?;
            let description = optional_string("mcp-add-http", &description_value)?;

            env.borrow()
                .shell_env
                .write()
                .add_mcp_server(McpServerConfig {
                    label,
                    description,
                    transport: McpTransport::Http {
                        url,
                        auth_header,
                        allow_stateless,
                    },
                });

            Ok(Value::NIL)
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
            let mut shell_env = env_ref.shell_env.write();
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
        Symbol::from("range"),
        Value::NativeFunc(|_env, args| {
            let start = require_typed_arg::<IntType>("range", &args, 0)?;
            let end = require_typed_arg::<IntType>("range", &args, 1)?;

            let mut current = start;

            Ok(Value::List(
                std::iter::from_fn(move || {
                    if current == end {
                        None
                    } else {
                        #[cfg(feature = "bigint")]
                        let res = Some(current.clone());
                        #[cfg(not(feature = "bigint"))]
                        let res = Some(current);

                        current += IntType::from(1);

                        res
                    }
                })
                .map(Value::from)
                .collect(),
            ))
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
        Symbol::from("+"),
        Value::NativeFunc(|_env, args| {
            let first_arg = require_arg("+", &args, 1)?;

            let mut total = match first_arg {
                Value::Int(_) => Ok(Value::Int(IntType::default())),
                Value::Float(_) => Ok(Value::Float(0.0)),
                Value::String(_) => Ok(Value::String("".into())),
                _ => Err(RuntimeError {
                    msg: format!(
                        "Function \"+\" requires arguments to be numbers or strings; found {first_arg}"
                    ),
                }),
            }?;

            for arg in args {
                total = (&total + &arg).map_err(|_| RuntimeError {
                    msg: format!(
                        "Function \"+\" requires arguments to be numbers or strings; found {arg}"
                    ),
                })?;
            }

            Ok(total)
        }),
    );

    env.define(
        Symbol::from("-"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("-", &args, 0)?;
            let b = require_arg("-", &args, 1)?;

            (a - b).map_err(|_| RuntimeError {
                msg: String::from("Function \"-\" requires arguments to be numbers"),
            })
        }),
    );

    env.define(
        Symbol::from("*"),
        Value::NativeFunc(|_env, args| {
            let mut product = Value::Int(IntType::from(1));

            for arg in args {
                product = (&product * &arg).map_err(|_| RuntimeError {
                    msg: format!("Function \"*\" requires arguments to be numbers; found {arg}"),
                })?;
            }

            Ok(product)
        }),
    );

    env.define(
        Symbol::from("/"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("/", &args, 0)?;
            let b = require_arg("/", &args, 1)?;

            (a / b).map_err(|_| RuntimeError {
                msg: String::from("Function \"/\" requires arguments to be numbers"),
            })
        }),
    );

    env.define(
        Symbol::from("truncate"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("truncate", &args, 0)?;
            let b = require_arg("truncate", &args, 1)?;

            if let (Ok(a), Ok(b)) = (
                TryInto::<IntType>::try_into(a),
                TryInto::<IntType>::try_into(b),
            ) {
                return Ok(Value::Int(a / b));
            }

            Err(RuntimeError {
                msg: String::from("Function \"truncate\" requires arguments to be integers"),
            })
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

fn list_of_strings(name: &str, value: &Value) -> Result<Vec<String>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(Vec::new()),
        Value::List(list) => list
            .into_iter()
            .map(|item| match item {
                Value::String(s) => Ok(s),
                other => Err(RuntimeError {
                    msg: format!("\"{name}\" expects a list of strings; got element {other}"),
                }),
            })
            .collect(),
        Value::False => Ok(Vec::new()),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a list of strings or NIL; got {other}"),
        }),
    }
}

fn list_of_pairs(name: &str, value: &Value) -> Result<HashMap<String, String>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(HashMap::new()),
        Value::List(list) => {
            let mut map = HashMap::new();
            for entry in list.into_iter() {
                match entry {
                    Value::List(pair) => {
                        let mut iter = pair.into_iter();
                        let key = match iter.next() {
                            Some(Value::String(s)) => s,
                            Some(other) => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries as (key value); got key {other}"
                                    ),
                                });
                            }
                            None => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries with two elements"
                                    ),
                                });
                            }
                        };
                        let value = match iter.next() {
                            Some(Value::String(s)) => s,
                            Some(other) => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries as (key value); got value {other}"
                                    ),
                                });
                            }
                            None => {
                                return Err(RuntimeError {
                                    msg: format!(
                                        "\"{name}\" expects env entries with two elements"
                                    ),
                                });
                            }
                        };
                        map.insert(key, value);
                    }
                    other => {
                        return Err(RuntimeError {
                            msg: format!("\"{name}\" expects env entries as lists; got {other}"),
                        });
                    }
                }
            }
            Ok(map)
        }
        Value::False => Ok(HashMap::new()),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a list of key/value pairs or NIL; got {other}"),
        }),
    }
}

fn optional_string(name: &str, value: &Value) -> Result<Option<String>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(None),
        Value::False => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a string or NIL; got {other}"),
        }),
    }
}

fn optional_bool(name: &str, value: &Value) -> Result<Option<bool>, RuntimeError> {
    match value {
        Value::List(list) if *list == List::NIL => Ok(None),
        Value::False => Ok(Some(false)),
        Value::True => Ok(Some(true)),
        other => Err(RuntimeError {
            msg: format!("\"{name}\" expects a boolean or NIL; got {other}"),
        }),
    }
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
    fn lisp_mcp_helpers_add_servers() {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        run(&engine, "(mcp-clear)");
        run(
            &engine,
            "(mcp-add-stdio \"local\" \"/bin/echo\" '(\"hello\") '((\"FOO\" \"bar\")) '() \"Local echo\")",
        );
        run(
            &engine,
            "(mcp-add-http \"remote\" \"https://example.com/mcp\" '() '() \"Remote service\")",
        );

        let env_lock = env.read();
        let servers = env_lock.mcp_servers();
        assert_eq!(servers.len(), 2);

        let stdio = &servers[0];
        assert_eq!(stdio.label, "local");
        assert_eq!(stdio.description.as_deref(), Some("Local echo"));
        match &stdio.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                assert_eq!(command, "/bin/echo");
                assert_eq!(args, &vec!["hello".to_string()]);
                assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
                assert!(cwd.is_none());
            }
            other => panic!("expected stdio transport, got {:?}", other),
        }

        let http = &servers[1];
        assert_eq!(http.label, "remote");
        assert_eq!(http.description.as_deref(), Some("Remote service"));
        match &http.transport {
            McpTransport::Http {
                url,
                auth_header,
                allow_stateless,
            } => {
                assert_eq!(url, "https://example.com/mcp");
                assert!(auth_header.is_none());
                assert!(allow_stateless.is_none());
            }
            other => panic!("expected http transport, got {:?}", other),
        }
    }

    #[test]
    fn mcp_clear_resets_servers() {
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        run(
            &engine,
            "(mcp-add-stdio \"initial\" \"/bin/true\" '() '() '() '())",
        );
        run(&engine, "(mcp-clear)");
        run(
            &engine,
            "(mcp-add-sse \"after\" \"https://example.com/sse\" \"Docs\")",
        );

        let env_lock = env.read();
        let servers = env_lock.mcp_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].label, "after");
        match &servers[0].transport {
            McpTransport::Sse { url } => assert_eq!(url, "https://example.com/sse"),
            other => panic!("expected sse transport, got {:?}", other),
        }
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
}
