//! Regression tests for Lisp ordering and arithmetic: comparisons used to
//! recurse forever between `PartialOrd` and `Ord`, and integer division,
//! overflow and backwards `range` used to panic or exhaust memory.
use crate::environment::Environment;
use crate::lisp::default_environment::default_env;
use crate::lisp::interpreter::eval;
use crate::lisp::model::{IntType, RuntimeError, Value};
use crate::lisp::parser::parse;
use std::cell::RefCell;
use std::rc::Rc;

fn eval_str(code: &str) -> Result<Value, RuntimeError> {
    let env = Rc::new(RefCell::new(default_env(Environment::new())));
    let mut result = Value::NIL;
    for expr in parse(code) {
        result = eval(env.clone(), &expr.expect("test source parses"))?;
    }
    Ok(result)
}

fn eval_ok(code: &str) -> Value {
    eval_str(code).unwrap_or_else(|err| panic!("{code}: {}", err.msg))
}

fn eval_err(code: &str) -> String {
    match eval_str(code) {
        Ok(value) => panic!("{code}: expected error, got {value}"),
        Err(err) => err.msg,
    }
}

fn ints(values: &[IntType]) -> Value {
    Value::List(values.iter().copied().map(Value::from).collect())
}

#[test]
fn comparisons_terminate_and_follow_value_order() {
    assert_eq!(eval_ok("(< 1 2)"), Value::True);
    assert_eq!(eval_ok("(< 2 1)"), Value::False);
    assert_eq!(eval_ok("(<= 2 2)"), Value::True);
    assert_eq!(eval_ok("(> 2.5 2)"), Value::True);
    assert_eq!(eval_ok("(>= 1 1.5)"), Value::False);
    assert_eq!(eval_ok(r#"(< "a" "b")"#), Value::True);
}

#[test]
fn comparisons_between_unordered_kinds_are_false() {
    assert_eq!(eval_ok(r#"(< 1 "a")"#), Value::False);
    assert_eq!(eval_ok(r#"(> 1 "a")"#), Value::False);
}

#[test]
fn sort_orders_numbers_and_mixed_kinds_without_hanging() {
    assert_eq!(eval_ok("(sort (list 3 1 2))"), ints(&[1, 2, 3]));
    // Mixed kinds still sort deterministically instead of recursing.
    assert_eq!(
        eval_ok(r#"(sort (list "b" 2 "a" 1.5))"#),
        Value::List(
            [
                Value::Float(1.5),
                Value::from(2 as IntType),
                Value::String("a".into()),
                Value::String("b".into()),
            ]
            .into_iter()
            .collect()
        )
    );
}

#[test]
fn integer_division_by_zero_is_an_error() {
    assert!(eval_err("(/ 1 0)").contains("division by zero"));
    assert!(eval_err("(truncate 1 0)").contains("division by zero"));
}

#[test]
fn integer_overflow_is_an_error() {
    assert!(eval_err("(* 9223372036854775807 2)").contains("overflow"));
    assert!(eval_err("(+ 9223372036854775807 1)").contains("overflow"));
    assert!(eval_err("(- (- 0 9223372036854775807) 2)").contains("overflow"));
    assert!(eval_err("(/ (- (- 0 9223372036854775807) 1) -1)").contains("overflow"));
}

#[test]
fn range_is_empty_when_start_is_not_below_end() {
    assert_eq!(eval_ok("(range 0 3)"), ints(&[0, 1, 2]));
    assert_eq!(eval_ok("(range 3 3)"), ints(&[]));
    assert_eq!(eval_ok("(range 3 1)"), ints(&[]));
}

#[test]
fn plus_takes_result_kind_from_first_argument() {
    assert_eq!(eval_ok("(+ 5)"), Value::from(5 as IntType));
    assert_eq!(eval_ok(r#"(+ "a" 1)"#), Value::String("a1".into()));
}
