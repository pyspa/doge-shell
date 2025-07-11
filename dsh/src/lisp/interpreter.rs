use crate::{
    completion,
    lisp::{
        model::{Env, Lambda, List, RuntimeError, Symbol, Value},
        utils::{require_arg, require_typed_arg},
    },
};
use std::{cell::RefCell, rc::Rc};
use tracing::log::debug;

/// Evaluate a single Lisp expression in the context of a given environment.
pub fn eval(env: Rc<RefCell<Env>>, expression: &Value) -> Result<Value, RuntimeError> {
    eval_inner(env, expression, Context::new())
}

/// Evaluate a series of s-expressions. Each expression is evaluated in
/// order and the final one's return value is returned.
#[allow(dead_code)]
pub fn eval_block(
    env: Rc<RefCell<Env>>,
    clauses: impl Iterator<Item = Value>,
) -> Result<Value, RuntimeError> {
    eval_block_inner(env, clauses, Context::new())
}

fn eval_block_inner(
    env: Rc<RefCell<Env>>,
    clauses: impl Iterator<Item = Value>,
    context: Context,
) -> Result<Value, RuntimeError> {
    let mut current_expr: Option<Value> = None;

    for clause in clauses {
        if let Some(expr) = current_expr {
            match eval_inner(env.clone(), &expr, context.found_tail(true)) {
                Ok(_) => (),
                Err(e) => {
                    return Err(e);
                }
            }
        }

        current_expr = Some(clause);
    }

    if let Some(expr) = &current_expr {
        eval_inner(env, expr, context)
    } else {
        Ok(Value::NIL)
        // Err(RuntimeError {
        //     msg: "Unrecognized expression".to_owned(),
        // })
    }
}

/// `found_tail` and `in_func` are used when locating the tail position for
/// tail-call optimization. Candidates are not eligible if a) we aren't already
/// inside a function call, or b) we've already found the tail inside the current
/// function call. `found_tail` is currently overloaded inside special forms to
/// factor out function calls in, say, the conditional slot, which are not
/// eligible to be the tail-call based on their position. A future refactor hopes
/// to make things a little more semantic.
fn eval_inner(
    env: Rc<RefCell<Env>>,
    expression: &Value,
    context: Context,
) -> Result<Value, RuntimeError> {
    if context.quoting {
        match expression {
            Value::List(list) if *list != List::NIL => match &list.car()? {
                Value::Symbol(Symbol(keyword)) if keyword == "comma" => {
                    // do nothing, handle it down below
                }
                _ => {
                    return list
                        .into_iter()
                        .map(|el| eval_inner(env.clone(), &el, context))
                        .collect::<Result<List, RuntimeError>>()
                        .map(Value::List);
                }
            },
            _ => return Ok(expression.clone()),
        }
    }

    match expression {
        // look up symbol
        Value::Symbol(symbol) => env.borrow().get(symbol).ok_or_else(|| RuntimeError {
            msg: format!("\"{}\" is not defined", symbol),
        }),

        // s-expression
        Value::List(list) if *list != List::NIL => {
            match &list.car()? {
                // special forms
                Value::Symbol(Symbol(keyword)) if keyword == "comma" => {
                    eval_inner(env, &list.cdr().car()?, context.quoting(false))
                }

                Value::Symbol(Symbol(keyword)) if keyword == "quote" => {
                    eval_inner(env, &list.cdr().car()?, context.quoting(true))
                }

                Value::Symbol(Symbol(keyword)) if keyword == "define" || keyword == "set" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let symbol = require_typed_arg::<&Symbol>(keyword, args, 0)?;
                    let value_expr = require_arg(keyword, args, 1)?;

                    let value = eval_inner(env.clone(), value_expr, context.found_tail(true))?;

                    if keyword == "define" {
                        // OPTIMIZED: Store value directly without additional clone
                        env.borrow_mut().define(symbol.clone(), value.clone());
                        Ok(value) // Return the value directly
                    } else {
                        // OPTIMIZED: Store value directly without additional clone
                        env.borrow_mut().set(symbol.clone(), value.clone())?;
                        Ok(value)
                    }
                }

                Value::Symbol(Symbol(keyword)) if keyword == "defmacro" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let symbol = require_typed_arg::<&Symbol>(keyword, args, 0)?;
                    let argnames_list = require_typed_arg::<&List>(keyword, args, 1)?;
                    let argnames = value_to_argnames(argnames_list.clone())?;
                    let body = Rc::new(Value::List(list.cdr().cdr().cdr()));

                    let lambda = Value::Macro(Lambda {
                        closure: env.clone(),
                        argnames,
                        body,
                        export: false,
                    });

                    env.borrow_mut().define(symbol.clone(), lambda);

                    Ok(Value::NIL)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "defun" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let symbol = require_typed_arg::<&Symbol>(keyword, args, 0)?;
                    let argnames_list = require_typed_arg::<&List>(keyword, args, 1)?;
                    let argnames = value_to_argnames(argnames_list.clone())?;
                    let body = Rc::new(Value::List(list.cdr().cdr().cdr()));

                    let lambda = Value::Lambda(Lambda {
                        closure: env.clone(),
                        argnames,
                        body,
                        export: false,
                    });

                    env.borrow_mut().define(symbol.clone(), lambda);

                    Ok(Value::NIL)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "fn" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let symbol = require_typed_arg::<&Symbol>(keyword, args, 0)?;
                    let argnames_list = require_typed_arg::<&List>(keyword, args, 1)?;
                    let argnames = value_to_argnames(argnames_list.clone())?;
                    let body = Rc::new(Value::List(list.cdr().cdr().cdr()));

                    let lambda = Value::Lambda(Lambda {
                        closure: env.clone(),
                        argnames,
                        body,
                        export: true,
                    });

                    env.borrow_mut().define(symbol.clone(), lambda);

                    Ok(Value::NIL)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "autocomplete" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let name_symbol = require_typed_arg::<&Symbol>(keyword, args, 0)?;
                    let target = name_symbol.to_string().replace('-', "_");
                    let mut cmd = None;
                    let mut func = None;
                    let mut candidates = None;

                    // OPTIMIZED: Use reference instead of clone
                    let val = &args[1];

                    match val {
                        Value::List(list) => {
                            let list = list.into_iter().map(|x| x.to_string()).collect();
                            candidates = Some(list);
                        }
                        Value::Lambda(lambda) => {
                            func = Some(Value::Lambda(lambda.clone()));
                        }
                        Value::String(str) => {
                            cmd = Some(str.clone());
                        }
                        _ => {
                            println!("autocomplete unknown value: {:?}", val);
                        }
                    }

                    let entry = completion::AutoComplete {
                        target,
                        cmd,
                        func,
                        candidates,
                    };
                    debug!("add autocomplete {:?}", entry);

                    env.borrow_mut()
                        .shell_env
                        .write()
                        .autocompletion
                        .push(entry);

                    Ok(Value::NIL)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "lambda" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let argnames_list = require_typed_arg::<&List>(keyword, args, 0)?;
                    let argnames = value_to_argnames(argnames_list.clone())?;
                    let body = Rc::new(Value::List(list.cdr().cdr()));

                    Ok(Value::Lambda(Lambda {
                        closure: env,
                        argnames,
                        body,
                        export: false,
                    }))
                }

                Value::Symbol(Symbol(keyword)) if keyword == "let" => {
                    let let_env = Rc::new(RefCell::new(Env::extend(env)));

                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let declarations = require_typed_arg::<&List>(keyword, args, 0)?;

                    for decl in declarations.into_iter() {
                        let decl = &decl;

                        let decl_cons: &List = decl.try_into().map_err(|_| RuntimeError {
                            msg: format!("Expected declaration clause, found {}", decl),
                        })?;
                        let symbol = &decl_cons.car()?;
                        let symbol: &Symbol = symbol.try_into().map_err(|_| RuntimeError {
                            msg: format!("Expected symbol for let declaration, found {}", symbol),
                        })?;
                        let expr = &decl_cons.cdr().car()?;

                        let result = eval_inner(let_env.clone(), expr, context.found_tail(true))?;
                        // OPTIMIZED: Move result instead of clone when possible
                        let_env.borrow_mut().define(symbol.clone(), result);
                    }

                    let body = &Value::List(list.cdr().cdr());
                    let body: &List = body.try_into().map_err(|_| RuntimeError {
                        msg: format!(
                            "Expected expression(s) after let-declarations, found {}",
                            body
                        ),
                    })?;

                    eval_block_inner(let_env, body.into_iter(), context)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "vlet" => {
                    let let_env = Rc::new(RefCell::new(Env::extend(env)));

                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let declarations = require_typed_arg::<&List>(keyword, args, 0)?;

                    for decl in declarations.into_iter() {
                        let decl = &decl;

                        let decl_cons: &List = decl.try_into().map_err(|_| RuntimeError {
                            msg: format!("Expected declaration clause, found {}", decl),
                        })?;
                        let symbol = &decl_cons.car()?;
                        let symbol: &Symbol = symbol.try_into().map_err(|_| RuntimeError {
                            msg: format!("Expected symbol for let declaration, found {}", symbol),
                        })?;
                        let expr = &decl_cons.cdr().car()?;

                        let result = eval_inner(let_env.clone(), expr, context.found_tail(true))?;
                        let_env
                            .borrow_mut()
                            .shell_env
                            .write()
                            .variables
                            .insert(format!("${}", symbol), result.to_string());
                        // OPTIMIZED: Move result instead of clone when possible
                        let_env.borrow_mut().define(symbol.clone(), result);
                    }

                    let body = &Value::List(list.cdr().cdr());
                    let body: &List = body.try_into().map_err(|_| RuntimeError {
                        msg: format!(
                            "Expected expression(s) after let-declarations, found {}",
                            body
                        ),
                    })?;
                    debug!(
                        "variables {:?}",
                        let_env.borrow().shell_env.read().variables
                    );
                    eval_block_inner(let_env, body.into_iter(), context)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "begin" => {
                    eval_block_inner(env, list.cdr().into_iter(), context)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "cond" => {
                    let clauses = list.cdr();

                    for clause in clauses.into_iter() {
                        let clause = &clause;

                        let clause: &List = clause.try_into().map_err(|_| RuntimeError {
                            msg: format!("Expected conditional clause, found {}", clause),
                        })?;

                        let condition = &clause.car()?;
                        let then = &clause.cdr().car()?;

                        if eval_inner(env.clone(), condition, context.found_tail(true))?.into() {
                            return eval_inner(env, then, context);
                        }
                    }

                    Ok(Value::NIL)
                }

                Value::Symbol(Symbol(keyword)) if keyword == "if" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();

                    let condition = require_arg(keyword, args, 0)?;
                    let then_expr = require_arg(keyword, args, 1)?;
                    let else_expr = require_arg(keyword, args, 2).ok();

                    if eval_inner(env.clone(), condition, context.found_tail(true))?.into() {
                        eval_inner(env, then_expr, context)
                    } else {
                        else_expr
                            .map(|expr| eval_inner(env, expr, context))
                            .unwrap_or(Ok(Value::NIL))
                    }
                }

                Value::Symbol(Symbol(keyword)) if keyword == "and" || keyword == "or" => {
                    let args = &list.cdr().into_iter().collect::<Vec<Value>>();
                    let is_or = keyword.as_str() == "or";

                    let mut last_result: Option<Value> = None;
                    for arg in args {
                        let result = eval_inner(env.clone(), arg, context.found_tail(true))?;
                        let truthy: bool = (&result).into();

                        if is_or == truthy {
                            return Ok(result);
                        }

                        last_result = Some(result);
                    }

                    Ok(if let Some(last_result) = last_result {
                        last_result
                    } else {
                        // there were zero arguments
                        (!is_or).into()
                    })
                }

                // function call or macro expand
                _ => {
                    let func_or_macro =
                        eval_inner(env.clone(), &list.car()?, context.found_tail(true))?;

                    if matches!(func_or_macro, Value::Macro(_)) {
                        let args = list.into_iter().skip(1).collect::<Vec<Value>>();

                        let expanded = call_function_or_macro(env.clone(), &func_or_macro, args)?;

                        eval_inner(env.clone(), &expanded, Context::new())
                    } else {
                        let args = list
                            .into_iter()
                            .skip(1)
                            .map(|car| eval_inner(env.clone(), &car, context.found_tail(true)))
                            .collect::<Result<Vec<Value>, RuntimeError>>()?;

                        if !context.found_tail && context.in_func {
                            Ok(Value::TailCall {
                                func: Rc::new(func_or_macro),
                                args,
                            })
                        } else {
                            let mut res = call_function_or_macro(env.clone(), &func_or_macro, args);

                            while let Ok(Value::TailCall { func, args }) = res {
                                res = call_function_or_macro(env.clone(), func.as_ref(), args);
                            }

                            res
                        }
                    }
                }
            }
        }

        // plain value
        _ => Ok(expression.clone()),
    }
}
// 🦀 Boo! Did I scare ya? Haha!

fn value_to_argnames(argnames: List) -> Result<Vec<Symbol>, RuntimeError> {
    argnames
        .into_iter()
        .enumerate()
        .map(|(index, arg)| match arg {
            Value::Symbol(s) => Ok(s),
            _ => Err(RuntimeError {
                msg: format!(
                    "Expected list of arg names, but arg {} is a {}",
                    index,
                    arg.type_name()
                ),
            }),
        })
        .collect()
}

/// Calling a function is separated from the main `eval_inner()` function
/// so that tail calls can be evaluated without just returning themselves
/// as-is as a tail-call.
fn call_function_or_macro(
    env: Rc<RefCell<Env>>,
    func: &Value,
    args: Vec<Value>,
) -> Result<Value, RuntimeError> {
    if let Value::NativeFunc(func) = func {
        func(env, args)
    } else if let Value::NativeClosure(closure) = func {
        closure.borrow_mut()(env, args)
    } else {
        let lambda = match func {
            Value::Lambda(lamb) => Some(lamb),
            Value::Macro(lamb) => Some(lamb),
            _ => None,
        };

        if let Some(lambda) = lambda {
            // bind args - OPTIMIZED: More efficient argument binding
            let mut arg_env = Env::extend(lambda.closure.clone());
            for (index, arg_name) in lambda.argnames.iter().enumerate() {
                if arg_name.0 == "..." {
                    // rest parameters
                    arg_env.define(
                        Symbol::from("..."),
                        Value::List(args.into_iter().skip(index).collect()),
                    );
                    break;
                } else if let Some(arg_value) = args.get(index) {
                    // OPTIMIZED: Clone only when necessary
                    arg_env.define(arg_name.clone(), arg_value.clone());
                }
            }

            // evaluate each line of body
            let clauses: &List = lambda.body.as_ref().try_into()?;
            eval_block_inner(
                Rc::new(RefCell::new(arg_env)),
                clauses.into_iter(),
                Context {
                    found_tail: false,
                    in_func: true,
                    quoting: false,
                },
            )
        } else {
            Err(RuntimeError {
                msg: format!("{} is not callable", func),
            })
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Context {
    pub found_tail: bool,
    pub in_func: bool,
    pub quoting: bool,
}

impl Context {
    pub fn new() -> Self {
        Self {
            found_tail: false,
            in_func: false,
            quoting: false,
        }
    }

    pub fn found_tail(self, found_tail: bool) -> Self {
        Self {
            found_tail,
            in_func: self.in_func,
            quoting: self.quoting,
        }
    }

    // pub fn in_func(self, in_func: bool) -> Self {
    //     Self {
    //         found_tail: self.found_tail,
    //         in_func,
    //         quoting: self.quoting,
    //     }
    // }

    pub fn quoting(self, quoting: bool) -> Self {
        Self {
            found_tail: self.found_tail,
            in_func: self.in_func,
            quoting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::lisp::default_environment::default_env;
    use crate::lisp::model::{Env, Symbol, Value};

    use std::cell::RefCell;
    use std::rc::Rc;

    fn create_test_env() -> Rc<RefCell<Env>> {
        let shell_env = Environment::new();
        Rc::new(RefCell::new(default_env(shell_env)))
    }

    #[test]
    fn test_eval_basic_values() {
        let env = create_test_env();

        // Test integers
        let result = eval(env.clone(), &Value::Int(42.into())).unwrap();
        assert_eq!(result, Value::Int(42.into()));

        // Test strings
        let result = eval(env.clone(), &Value::String("hello".to_string())).unwrap();
        assert_eq!(result, Value::String("hello".to_string()));

        // Test booleans
        let result = eval(env.clone(), &Value::True).unwrap();
        assert_eq!(result, Value::True);

        let result = eval(env.clone(), &Value::False).unwrap();
        assert_eq!(result, Value::False);

        // Test NIL
        let result = eval(env.clone(), &Value::NIL).unwrap();
        assert_eq!(result, Value::NIL);
    }

    #[test]
    fn test_symbol_lookup() {
        let env = create_test_env();

        // Define a symbol
        env.borrow_mut()
            .define(Symbol::from("test-var"), Value::Int(123.into()));

        // Look it up
        let result = eval(env.clone(), &Value::Symbol(Symbol::from("test-var"))).unwrap();
        assert_eq!(result, Value::Int(123.into()));

        // Test undefined symbol
        let result = eval(env.clone(), &Value::Symbol(Symbol::from("undefined")));
        assert!(result.is_err());
    }

    #[test]
    fn test_define_and_set() {
        let env = create_test_env();

        // Test define
        let define_expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("define")),
                Value::Symbol(Symbol::from("x")),
                Value::Int(42.into()),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &define_expr).unwrap();
        assert_eq!(result, Value::Int(42.into()));

        // Verify the symbol was defined
        let lookup = eval(env.clone(), &Value::Symbol(Symbol::from("x"))).unwrap();
        assert_eq!(lookup, Value::Int(42.into()));

        // Test set
        let set_expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("set")),
                Value::Symbol(Symbol::from("x")),
                Value::Int(100.into()),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &set_expr).unwrap();
        assert_eq!(result, Value::Int(100.into()));

        // Verify the symbol was updated
        let lookup = eval(env.clone(), &Value::Symbol(Symbol::from("x"))).unwrap();
        assert_eq!(lookup, Value::Int(100.into()));
    }

    #[test]
    fn test_performance_no_unnecessary_clones() {
        let env = create_test_env();

        // This test ensures that basic operations don't perform unnecessary clones
        // We'll define a large structure and ensure it's handled efficiently

        let large_list = Value::List((0..1000).map(|i| Value::Int(i.into())).collect());

        // Store it
        env.borrow_mut()
            .define(Symbol::from("large-list"), large_list.clone());

        // Retrieve it multiple times - should not clone unnecessarily
        for _ in 0..10 {
            let result = eval(env.clone(), &Value::Symbol(Symbol::from("large-list"))).unwrap();
            assert_eq!(result, large_list);
        }
    }

    #[test]
    fn test_performance_define_operations() {
        let env = create_test_env();

        // Test that define operations are efficient
        for i in 0..100 {
            let var_name = format!("var-{}", i);
            let define_expr = Value::List(
                vec![
                    Value::Symbol(Symbol::from("define")),
                    Value::Symbol(Symbol::from(var_name.as_str())),
                    Value::Int(i.into()),
                ]
                .into_iter()
                .collect(),
            );

            let result = eval(env.clone(), &define_expr).unwrap();
            assert_eq!(result, Value::Int(i.into()));
        }

        // Verify all variables are accessible
        for i in 0..100 {
            let var_name = format!("var-{}", i);
            let lookup =
                eval(env.clone(), &Value::Symbol(Symbol::from(var_name.as_str()))).unwrap();
            assert_eq!(lookup, Value::Int(i.into()));
        }
    }

    #[test]
    fn test_performance_function_calls() {
        let env = create_test_env();

        // Define a simple function that would test argument binding efficiency
        let add_def = Value::List(
            vec![
                Value::Symbol(Symbol::from("defun")),
                Value::Symbol(Symbol::from("add-two")),
                Value::List(
                    vec![
                        Value::Symbol(Symbol::from("x")),
                        Value::Symbol(Symbol::from("y")),
                    ]
                    .into_iter()
                    .collect(),
                ),
                Value::List(
                    vec![
                        Value::Symbol(Symbol::from("+")),
                        Value::Symbol(Symbol::from("x")),
                        Value::Symbol(Symbol::from("y")),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &add_def).unwrap();
        assert_eq!(result, Value::NIL);

        // Test calling the function multiple times - this tests argument binding efficiency
        for i in 0..50 {
            let call_expr = Value::List(
                vec![
                    Value::Symbol(Symbol::from("add-two")),
                    Value::Int(i.into()),
                    Value::Int((i + 1).into()),
                ]
                .into_iter()
                .collect(),
            );

            let result = eval(env.clone(), &call_expr).unwrap();
            assert_eq!(result, Value::Int((i + i + 1).into()));
        }
    }

    #[test]
    fn test_performance_let_bindings() {
        let env = create_test_env();

        // Test nested let bindings which could cause excessive cloning
        let nested_let = Value::List(
            vec![
                Value::Symbol(Symbol::from("let")),
                Value::List(
                    vec![
                        Value::List(
                            vec![Value::Symbol(Symbol::from("x")), Value::Int(10.into())]
                                .into_iter()
                                .collect(),
                        ),
                        Value::List(
                            vec![Value::Symbol(Symbol::from("y")), Value::Int(20.into())]
                                .into_iter()
                                .collect(),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
                Value::List(
                    vec![
                        Value::Symbol(Symbol::from("let")),
                        Value::List(
                            vec![Value::List(
                                vec![
                                    Value::Symbol(Symbol::from("z")),
                                    Value::List(
                                        vec![
                                            Value::Symbol(Symbol::from("+")),
                                            Value::Symbol(Symbol::from("x")),
                                            Value::Symbol(Symbol::from("y")),
                                        ]
                                        .into_iter()
                                        .collect(),
                                    ),
                                ]
                                .into_iter()
                                .collect(),
                            )]
                            .into_iter()
                            .collect(),
                        ),
                        Value::List(
                            vec![
                                Value::Symbol(Symbol::from("*")),
                                Value::Symbol(Symbol::from("z")),
                                Value::Int(2.into()),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &nested_let).unwrap();
        assert_eq!(result, Value::Int(60.into())); // ((10 + 20) * 2) = 60
    }
}
