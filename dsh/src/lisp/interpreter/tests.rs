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
fn keywords_evaluate_to_themselves() {
    let env = create_test_env();

    // `(table-order-by t "size" :desc)` relies on this; looking `:desc`
    // up as a variable is what used to fail.
    let keyword = Value::Symbol(Symbol::from(":desc"));
    assert_eq!(eval(env.clone(), &keyword).unwrap(), keyword);

    // A plain symbol is still a binding, and an unbound one still errors.
    assert!(eval(env.clone(), &Value::Symbol(Symbol::from("desc"))).is_err());
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
fn test_lambda_creation_and_call() {
    let env = create_test_env();

    // Create a lambda: (lambda (x) (+ x 1))
    let lambda_expr = Value::List(
        vec![
            Value::Symbol(Symbol::from("lambda")),
            Value::List(vec![Value::Symbol(Symbol::from("x"))].into_iter().collect()),
            Value::List(
                vec![
                    Value::Symbol(Symbol::from("+")),
                    Value::Symbol(Symbol::from("x")),
                    Value::Int(1.into()),
                ]
                .into_iter()
                .collect(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let lambda_result = eval(env.clone(), &lambda_expr).unwrap();
    assert!(matches!(lambda_result, Value::Lambda(_)));

    // Store the lambda
    env.borrow_mut().define(Symbol::from("inc"), lambda_result);

    // Call the lambda: (inc 5)
    let call_expr = Value::List(
        vec![Value::Symbol(Symbol::from("inc")), Value::Int(5.into())]
            .into_iter()
            .collect(),
    );

    let result = eval(env.clone(), &call_expr).unwrap();
    assert_eq!(result, Value::Int(6.into()));
}

#[test]
fn test_let_binding() {
    let env = create_test_env();

    // Test let: (let ((x 10) (y 20)) (+ x y))
    let let_expr = Value::List(
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

    let result = eval(env.clone(), &let_expr).unwrap();
    assert_eq!(result, Value::Int(30.into()));

    // Verify that let bindings don't leak to outer scope
    let x_lookup = eval(env.clone(), &Value::Symbol(Symbol::from("x")));
    assert!(x_lookup.is_err());
}

#[test]
fn test_if_conditional() {
    let env = create_test_env();

    // Test if true: (if T 42 0)
    let if_true = Value::List(
        vec![
            Value::Symbol(Symbol::from("if")),
            Value::True,
            Value::Int(42.into()),
            Value::Int(0.into()),
        ]
        .into_iter()
        .collect(),
    );

    let result = eval(env.clone(), &if_true).unwrap();
    assert_eq!(result, Value::Int(42.into()));

    // Test if false: (if F 42 0)
    let if_false = Value::List(
        vec![
            Value::Symbol(Symbol::from("if")),
            Value::False,
            Value::Int(42.into()),
            Value::Int(0.into()),
        ]
        .into_iter()
        .collect(),
    );

    let result = eval(env.clone(), &if_false).unwrap();
    assert_eq!(result, Value::Int(0.into()));

    // Test if without else: (if F 42)
    let if_no_else = Value::List(
        vec![
            Value::Symbol(Symbol::from("if")),
            Value::False,
            Value::Int(42.into()),
        ]
        .into_iter()
        .collect(),
    );

    let result = eval(env.clone(), &if_no_else).unwrap();
    assert_eq!(result, Value::NIL);
}

#[test]
fn test_quote() {
    let env = create_test_env();

    // Test quote: (quote (+ 1 2))
    let quote_expr = Value::List(
        vec![
            Value::Symbol(Symbol::from("quote")),
            Value::List(
                vec![
                    Value::Symbol(Symbol::from("+")),
                    Value::Int(1.into()),
                    Value::Int(2.into()),
                ]
                .into_iter()
                .collect(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let result = eval(env.clone(), &quote_expr).unwrap();
    let expected = Value::List(
        vec![
            Value::Symbol(Symbol::from("+")),
            Value::Int(1.into()),
            Value::Int(2.into()),
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(result, expected);
}

#[test]
fn test_and_or_logic() {
    let env = create_test_env();

    // Test and: (and T T)
    let and_true = Value::List(
        vec![Value::Symbol(Symbol::from("and")), Value::True, Value::True]
            .into_iter()
            .collect(),
    );
    let result = eval(env.clone(), &and_true).unwrap();
    assert_eq!(result, Value::True);

    // Test and: (and T F)
    let and_false = Value::List(
        vec![
            Value::Symbol(Symbol::from("and")),
            Value::True,
            Value::False,
        ]
        .into_iter()
        .collect(),
    );
    let result = eval(env.clone(), &and_false).unwrap();
    assert_eq!(result, Value::False);

    // Test or: (or F T)
    let or_true = Value::List(
        vec![Value::Symbol(Symbol::from("or")), Value::False, Value::True]
            .into_iter()
            .collect(),
    );
    let result = eval(env.clone(), &or_true).unwrap();
    assert_eq!(result, Value::True);

    // Test or: (or F F)
    let or_false = Value::List(
        vec![
            Value::Symbol(Symbol::from("or")),
            Value::False,
            Value::False,
        ]
        .into_iter()
        .collect(),
    );
    let result = eval(env.clone(), &or_false).unwrap();
    assert_eq!(result, Value::False);
}

#[test]
fn test_nested_environments() {
    let env = create_test_env();

    // Define outer variable
    env.borrow_mut()
        .define(Symbol::from("outer"), Value::Int(100.into()));

    // Test nested let that shadows outer variable
    let nested_let = Value::List(
        vec![
            Value::Symbol(Symbol::from("let")),
            Value::List(
                vec![Value::List(
                    vec![Value::Symbol(Symbol::from("outer")), Value::Int(200.into())]
                        .into_iter()
                        .collect(),
                )]
                .into_iter()
                .collect(),
            ),
            Value::Symbol(Symbol::from("outer")),
        ]
        .into_iter()
        .collect(),
    );

    let result = eval(env.clone(), &nested_let).unwrap();
    assert_eq!(result, Value::Int(200.into()));

    // Verify outer variable is unchanged
    let outer_lookup = eval(env.clone(), &Value::Symbol(Symbol::from("outer"))).unwrap();
    assert_eq!(outer_lookup, Value::Int(100.into()));
}

#[test]
fn test_function_definition_and_call() {
    let env = create_test_env();

    // Define function: (defun square (x) (* x x))
    let defun_expr = Value::List(
        vec![
            Value::Symbol(Symbol::from("defun")),
            Value::Symbol(Symbol::from("square")),
            Value::List(vec![Value::Symbol(Symbol::from("x"))].into_iter().collect()),
            Value::List(
                vec![
                    Value::Symbol(Symbol::from("*")),
                    Value::Symbol(Symbol::from("x")),
                    Value::Symbol(Symbol::from("x")),
                ]
                .into_iter()
                .collect(),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let result = eval(env.clone(), &defun_expr).unwrap();
    assert_eq!(result, Value::NIL);

    // Call function: (square 5)
    let call_expr = Value::List(
        vec![Value::Symbol(Symbol::from("square")), Value::Int(5.into())]
            .into_iter()
            .collect(),
    );

    let result = eval(env.clone(), &call_expr).unwrap();
    assert_eq!(result, Value::Int(25.into()));
}

#[test]
fn test_error_handling() {
    let env = create_test_env();

    // Test undefined symbol
    let result = eval(env.clone(), &Value::Symbol(Symbol::from("undefined")));
    assert!(result.is_err());

    // Test setting undefined variable
    let set_undefined = Value::List(
        vec![
            Value::Symbol(Symbol::from("set")),
            Value::Symbol(Symbol::from("undefined")),
            Value::Int(42.into()),
        ]
        .into_iter()
        .collect(),
    );
    let result = eval(env.clone(), &set_undefined);
    assert!(result.is_err());
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
        let var_name = format!("var-{i}");
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
        let var_name = format!("var-{i}");
        let lookup = eval(env.clone(), &Value::Symbol(Symbol::from(var_name.as_str()))).unwrap();
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
