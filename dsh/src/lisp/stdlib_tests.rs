#[cfg(test)]
mod tests {
    use crate::environment::Environment;
    use crate::lisp::default_environment::default_env;
    use crate::lisp::interpreter::eval;
    use crate::lisp::model::{Env, IntType, Symbol, Value};
    use dsh_types::mcp::McpTransport;
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

    #[test]
    fn test_json_parse_stringify() {
        let env = create_test_env();

        // (json-parse "[{\"a\": 1}]")
        let json_str = "[{\"a\": 1}]".to_string();
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("json-parse")),
                Value::String(json_str),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &expr).unwrap();
        // Check if it is a table
        if let Value::Table(table) = result.clone() {
            assert_eq!(table.borrow().count(), 1);
        } else {
            panic!("Expected table from json-parse");
        }

        // (json-stringify table)
        let expr_stringify = Value::List(
            vec![Value::Symbol(Symbol::from("json-stringify")), result]
                .into_iter()
                .collect(),
        );

        let result_str = eval(env.clone(), &expr_stringify).unwrap();
        // The string might vary in whitespace/order, so just check if it's a string
        match result_str {
            Value::String(_) => {} // OK
            _ => panic!("Expected string from json-stringify"),
        }
    }

    #[test]
    fn test_table_operations() {
        let env = create_test_env();

        // (csv-parse "name,age\nAlice,30\nBob,25")
        let csv_str = "name,age\nAlice,30\nBob,25".to_string();
        let expr = Value::List(
            vec![
                Value::Symbol(Symbol::from("csv-parse")),
                Value::String(csv_str),
            ]
            .into_iter()
            .collect(),
        );

        let result = eval(env.clone(), &expr).unwrap();

        // Verify table creation
        let table_val = if let Value::Table(t) = &result {
            t.clone()
        } else {
            panic!("Expected table from csv-parse");
        };
        assert_eq!(table_val.borrow().count(), 2);

        // (table-select table '("name"))
        let quote_sym = Value::Symbol(Symbol::from("quote"));
        let valid_cols = Value::List(
            vec![Value::String("name".to_string())]
                .into_iter()
                .collect(),
        );
        let quoted_cols = Value::List(vec![quote_sym, valid_cols].into_iter().collect());

        let expr_select = Value::List(
            vec![
                Value::Symbol(Symbol::from("table-select")),
                result.clone(),
                quoted_cols,
            ]
            .into_iter()
            .collect(),
        );

        let result_select = eval(env.clone(), &expr_select).unwrap();
        if let Value::Table(t) = result_select {
            // Verify we still have 2 rows but strictly "name" column might be internal detail
            // mostly checking no panic and correct row count
            assert_eq!(t.borrow().count(), 2);
        } else {
            panic!("Expected table from table-select");
        }
    }

    #[test]
    fn lisp_mcp_helpers_add_servers() {
        let shell_env = Environment::new();
        shell_env.write().startup_mode = true;
        let mut env = Env::new(shell_env.clone());
        crate::lisp::stdlib::register(&mut env);

        let engine = Rc::new(RefCell::new(TestEngine {
            env: Rc::new(RefCell::new(env)),
        }));

        // We can just use `eval` directly
        let run = |expr: &str| {
            let mut parser = crate::lisp::parser::parse(expr);
            let val = parser.next().unwrap().unwrap();
            eval(engine.borrow().env.clone(), &val).unwrap();
        };

        run("(mcp-clear)");
        run(
            "(mcp-add-stdio \"local\" \"/bin/echo\" '(\"hello\") '((\"FOO\" \"bar\")) '() \"Local echo\")",
        );
        run("(mcp-add-http \"remote\" \"https://example.com/mcp\" '() '() \"Remote service\")");

        let env_lock = shell_env.read();
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
        let shell_env = Environment::new();
        shell_env.write().startup_mode = true;
        let mut env = Env::new(shell_env.clone());
        crate::lisp::stdlib::register(&mut env);
        let engine = Rc::new(RefCell::new(TestEngine {
            env: Rc::new(RefCell::new(env)),
        }));

        let run = |expr: &str| {
            let mut parser = crate::lisp::parser::parse(expr);
            let val = parser.next().unwrap().unwrap();
            eval(engine.borrow().env.clone(), &val).unwrap();
        };

        run("(mcp-add-stdio \"initial\" \"/bin/true\" '() '() '() '())");
        run("(mcp-clear)");
        run("(mcp-add-sse \"after\" \"https://example.com/sse\" \"Docs\")");

        let env_lock = shell_env.read();
        let servers = env_lock.mcp_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].label, "after");
        match &servers[0].transport {
            McpTransport::Sse { url } => assert_eq!(url, "https://example.com/sse"),
            other => panic!("expected sse transport, got {:?}", other),
        }
    }

    struct TestEngine {
        env: Rc<RefCell<Env>>,
    }
}
