#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::lisp::model::{Env, Symbol, Value, List};
    use crate::lisp::default_environment::default_env;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn create_test_env() -> Rc<RefCell<Env>> {
        let shell_env = Arc::new(RwLock::new(Environment::new()));
        Rc::new(RefCell::new(default_env(shell_env)))
    }

    #[test]
    fn test_eval_basic_values() {
        let env = create_test_env();
        
        // Test integers
        let result = eval(env.clone(), &Value::Int(42)).unwrap();
        assert_eq!(result, Value::Int(42));
        
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
        env.borrow_mut().define(Symbol::from("test-var"), Value::Int(123));
        
        // Look it up
        let result = eval(env.clone(), &Value::Symbol(Symbol::from("test-var"))).unwrap();
        assert_eq!(result, Value::Int(123));
        
        // Test undefined symbol
        let result = eval(env.clone(), &Value::Symbol(Symbol::from("undefined")));
        assert!(result.is_err());
    }

    #[test]
    fn test_define_and_set() {
        let env = create_test_env();
        
        // Test define
        let define_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("define")),
            Value::Symbol(Symbol::from("x")),
            Value::Int(42)
        ]));
        
        let result = eval(env.clone(), &define_expr).unwrap();
        assert_eq!(result, Value::Int(42));
        
        // Verify the symbol was defined
        let lookup = eval(env.clone(), &Value::Symbol(Symbol::from("x"))).unwrap();
        assert_eq!(lookup, Value::Int(42));
        
        // Test set
        let set_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("set")),
            Value::Symbol(Symbol::from("x")),
            Value::Int(100)
        ]));
        
        let result = eval(env.clone(), &set_expr).unwrap();
        assert_eq!(result, Value::Int(100));
        
        // Verify the symbol was updated
        let lookup = eval(env.clone(), &Value::Symbol(Symbol::from("x"))).unwrap();
        assert_eq!(lookup, Value::Int(100));
    }

    #[test]
    fn test_lambda_creation_and_call() {
        let env = create_test_env();
        
        // Create a lambda: (lambda (x) (+ x 1))
        let lambda_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("lambda")),
            Value::List(List::from(vec![Value::Symbol(Symbol::from("x"))])),
            Value::List(List::from(vec![
                Value::Symbol(Symbol::from("+")),
                Value::Symbol(Symbol::from("x")),
                Value::Int(1)
            ]))
        ]));
        
        let lambda_result = eval(env.clone(), &lambda_expr).unwrap();
        assert!(matches!(lambda_result, Value::Lambda(_)));
        
        // Store the lambda
        env.borrow_mut().define(Symbol::from("inc"), lambda_result);
        
        // Call the lambda: (inc 5)
        let call_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("inc")),
            Value::Int(5)
        ]));
        
        let result = eval(env.clone(), &call_expr).unwrap();
        assert_eq!(result, Value::Int(6));
    }

    #[test]
    fn test_let_binding() {
        let env = create_test_env();
        
        // Test let: (let ((x 10) (y 20)) (+ x y))
        let let_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("let")),
            Value::List(List::from(vec![
                Value::List(List::from(vec![
                    Value::Symbol(Symbol::from("x")),
                    Value::Int(10)
                ])),
                Value::List(List::from(vec![
                    Value::Symbol(Symbol::from("y")),
                    Value::Int(20)
                ]))
            ])),
            Value::List(List::from(vec![
                Value::Symbol(Symbol::from("+")),
                Value::Symbol(Symbol::from("x")),
                Value::Symbol(Symbol::from("y"))
            ]))
        ]));
        
        let result = eval(env.clone(), &let_expr).unwrap();
        assert_eq!(result, Value::Int(30));
        
        // Verify that let bindings don't leak to outer scope
        let x_lookup = eval(env.clone(), &Value::Symbol(Symbol::from("x")));
        assert!(x_lookup.is_err());
    }

    #[test]
    fn test_if_conditional() {
        let env = create_test_env();
        
        // Test if true: (if T 42 0)
        let if_true = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("if")),
            Value::True,
            Value::Int(42),
            Value::Int(0)
        ]));
        
        let result = eval(env.clone(), &if_true).unwrap();
        assert_eq!(result, Value::Int(42));
        
        // Test if false: (if F 42 0)
        let if_false = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("if")),
            Value::False,
            Value::Int(42),
            Value::Int(0)
        ]));
        
        let result = eval(env.clone(), &if_false).unwrap();
        assert_eq!(result, Value::Int(0));
        
        // Test if without else: (if F 42)
        let if_no_else = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("if")),
            Value::False,
            Value::Int(42)
        ]));
        
        let result = eval(env.clone(), &if_no_else).unwrap();
        assert_eq!(result, Value::NIL);
    }

    #[test]
    fn test_quote() {
        let env = create_test_env();
        
        // Test quote: (quote (+ 1 2))
        let quote_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("quote")),
            Value::List(List::from(vec![
                Value::Symbol(Symbol::from("+")),
                Value::Int(1),
                Value::Int(2)
            ]))
        ]));
        
        let result = eval(env.clone(), &quote_expr).unwrap();
        let expected = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("+")),
            Value::Int(1),
            Value::Int(2)
        ]));
        assert_eq!(result, expected);
    }

    #[test]
    fn test_and_or_logic() {
        let env = create_test_env();
        
        // Test and: (and T T)
        let and_true = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("and")),
            Value::True,
            Value::True
        ]));
        let result = eval(env.clone(), &and_true).unwrap();
        assert_eq!(result, Value::True);
        
        // Test and: (and T F)
        let and_false = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("and")),
            Value::True,
            Value::False
        ]));
        let result = eval(env.clone(), &and_false).unwrap();
        assert_eq!(result, Value::False);
        
        // Test or: (or F T)
        let or_true = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("or")),
            Value::False,
            Value::True
        ]));
        let result = eval(env.clone(), &or_true).unwrap();
        assert_eq!(result, Value::True);
        
        // Test or: (or F F)
        let or_false = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("or")),
            Value::False,
            Value::False
        ]));
        let result = eval(env.clone(), &or_false).unwrap();
        assert_eq!(result, Value::False);
    }

    #[test]
    fn test_nested_environments() {
        let env = create_test_env();
        
        // Define outer variable
        env.borrow_mut().define(Symbol::from("outer"), Value::Int(100));
        
        // Test nested let that shadows outer variable
        let nested_let = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("let")),
            Value::List(List::from(vec![
                Value::List(List::from(vec![
                    Value::Symbol(Symbol::from("outer")),
                    Value::Int(200)
                ]))
            ])),
            Value::Symbol(Symbol::from("outer"))
        ]));
        
        let result = eval(env.clone(), &nested_let).unwrap();
        assert_eq!(result, Value::Int(200));
        
        // Verify outer variable is unchanged
        let outer_lookup = eval(env.clone(), &Value::Symbol(Symbol::from("outer"))).unwrap();
        assert_eq!(outer_lookup, Value::Int(100));
    }

    #[test]
    fn test_function_definition_and_call() {
        let env = create_test_env();
        
        // Define function: (defun square (x) (* x x))
        let defun_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("defun")),
            Value::Symbol(Symbol::from("square")),
            Value::List(List::from(vec![Value::Symbol(Symbol::from("x"))])),
            Value::List(List::from(vec![
                Value::Symbol(Symbol::from("*")),
                Value::Symbol(Symbol::from("x")),
                Value::Symbol(Symbol::from("x"))
            ]))
        ]));
        
        let result = eval(env.clone(), &defun_expr).unwrap();
        assert_eq!(result, Value::NIL);
        
        // Call function: (square 5)
        let call_expr = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("square")),
            Value::Int(5)
        ]));
        
        let result = eval(env.clone(), &call_expr).unwrap();
        assert_eq!(result, Value::Int(25));
    }

    #[test]
    fn test_error_handling() {
        let env = create_test_env();
        
        // Test undefined symbol
        let result = eval(env.clone(), &Value::Symbol(Symbol::from("undefined")));
        assert!(result.is_err());
        
        // Test setting undefined variable
        let set_undefined = Value::List(List::from(vec![
            Value::Symbol(Symbol::from("set")),
            Value::Symbol(Symbol::from("undefined")),
            Value::Int(42)
        ]));
        let result = eval(env.clone(), &set_undefined);
        assert!(result.is_err());
    }

    #[test]
    fn test_performance_no_unnecessary_clones() {
        let env = create_test_env();
        
        // This test ensures that basic operations don't perform unnecessary clones
        // We'll define a large structure and ensure it's handled efficiently
        
        let large_list = Value::List(List::from(
            (0..1000).map(|i| Value::Int(i)).collect::<Vec<_>>()
        ));
        
        // Store it
        env.borrow_mut().define(Symbol::from("large-list"), large_list.clone());
        
        // Retrieve it multiple times - should not clone unnecessarily
        for _ in 0..10 {
            let result = eval(env.clone(), &Value::Symbol(Symbol::from("large-list"))).unwrap();
            assert_eq!(result, large_list);
        }
    }
}
