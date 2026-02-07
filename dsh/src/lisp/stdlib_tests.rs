#[cfg(test)]
mod tests {
    use crate::environment::Environment;
    use crate::lisp::default_environment::default_env;
    use crate::lisp::interpreter::eval;
    use crate::lisp::model::{Env, IntType, Symbol, Value};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn create_test_env() -> Rc<RefCell<Env>> {
        let shell_env = Environment::new();
        Rc::new(RefCell::new(default_env(shell_env)))
    }

    #[test]
    fn test_math_basic() {
        let env = create_test_env();

        // (+ 1 2)
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("+")),
                Value::Int(IntType::from(1)),
                Value::Int(IntType::from(2)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            eval(env.clone(), &expr).unwrap(),
            Value::Int(IntType::from(3))
        );

        // (- 10 3)
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("-")),
                Value::Int(IntType::from(10)),
                Value::Int(IntType::from(3)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            eval(env.clone(), &expr).unwrap(),
            Value::Int(IntType::from(7))
        );

        // (* 2 3)
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("*")),
                Value::Int(IntType::from(2)),
                Value::Int(IntType::from(3)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            eval(env.clone(), &expr).unwrap(),
            Value::Int(IntType::from(6))
        );
    }

    #[test]
    fn test_math_division() {
        let env = create_test_env();

        // (/ 10 2)
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("/")),
                Value::Int(IntType::from(10)),
                Value::Int(IntType::from(2)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            eval(env.clone(), &expr).unwrap(),
            Value::Int(IntType::from(5))
        );

        // truncate (integer division)
        // (truncate 10 3) -> 3
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("truncate")),
                Value::Int(IntType::from(10)),
                Value::Int(IntType::from(3)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            eval(env.clone(), &expr).unwrap(),
            Value::Int(IntType::from(3))
        );
    }

    #[test]
    fn test_range() {
        let env = create_test_env();

        // (range 0 3) -> (0 1 2)
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("range")),
                Value::Int(IntType::from(0)),
                Value::Int(IntType::from(3)),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &expr).unwrap();
        if let Value::List(list) = result {
            let vec: Vec<Value> = list.into_iter().collect();
            assert_eq!(vec.len(), 3);
            assert_eq!(vec[0], Value::Int(IntType::from(0)));
            assert_eq!(vec[1], Value::Int(IntType::from(1)));
            assert_eq!(vec[2], Value::Int(IntType::from(2)));
        } else {
            panic!("Expected list from range");
        }
    }

    #[test]
    fn test_is_number() {
        let env = create_test_env();

        // (is_number 1) -> T
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("is_number")),
                Value::Int(IntType::from(1)),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(eval(env.clone(), &expr).unwrap(), Value::True);

        // (is_number "s") -> NIL
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("is_number")),
                Value::String("s".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(eval(env.clone(), &expr).unwrap(), Value::NIL);
    }
}
