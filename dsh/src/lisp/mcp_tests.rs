use crate::environment::Environment;
use crate::lisp::default_environment::default_env;
use crate::lisp::interpreter::eval;
use crate::lisp::model::{Env, List, Value};
use crate::lisp::parser::parse;
use dsh_types::mcp::McpTransport;
use parking_lot::RwLock;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

fn create_test_env() -> (Rc<RefCell<Env>>, Arc<RwLock<Environment>>) {
    let shell_env = Environment::new();
    let env = Rc::new(RefCell::new(default_env(shell_env.clone())));
    (env, shell_env)
}

fn run_lisp(env: Rc<RefCell<Env>>, code: &str) -> anyhow::Result<Value> {
    let expr = parse(code)
        .next()
        .ok_or_else(|| anyhow::anyhow!("No expression found"))?
        .map_err(|e| anyhow::anyhow!("Parse error: {}", e.msg))?;
    Ok(eval(env, &expr)?)
}

#[test]
fn test_mcp_add_stdio() {
    let (env, shell_env) = create_test_env();
    let code = r#"
        (mcp-add-stdio "test-stdio" "node" (list "server.js") (list (list "NODE_ENV" "test")) "/app" "Stdio Server")
    "#;
    run_lisp(env, code).unwrap();

    let env_read = shell_env.read();
    assert_eq!(env_read.mcp_servers.len(), 1);
    let server = &env_read.mcp_servers[0];
    assert_eq!(server.label, "test-stdio");
    assert_eq!(server.description, Some("Stdio Server".to_string()));

    if let McpTransport::Stdio {
        command,
        args,
        env,
        cwd,
    } = &server.transport
    {
        assert_eq!(command, "node");
        assert_eq!(args, &vec!["server.js"]);
        assert_eq!(env.get("NODE_ENV").map(|s| s.as_str()), Some("test"));
        assert_eq!(cwd.as_deref(), Some(std::path::Path::new("/app")));
    } else {
        panic!("Expected Stdio transport");
    }
}

#[test]
fn test_mcp_add_sse() {
    let (env, shell_env) = create_test_env();
    let code = r#"
        (mcp-add-sse "test-sse" "http://localhost:8080/sse" "SSE Server")
    "#;
    run_lisp(env, code).unwrap();

    let env_read = shell_env.read();
    assert_eq!(env_read.mcp_servers.len(), 1);
    let server = &env_read.mcp_servers[0];
    assert_eq!(server.label, "test-sse");
    assert_eq!(server.description, Some("SSE Server".to_string()));

    if let McpTransport::Sse { url } = &server.transport {
        assert_eq!(url, "http://localhost:8080/sse");
    } else {
        panic!("Expected SSE transport");
    }
}

#[test]
fn test_mcp_add_http() {
    let (env, shell_env) = create_test_env();
    let code = r#"
        (mcp-add-http "test-http" "http://localhost:8080/mcp" "Bearer token" T "HTTP Server")
    "#;
    run_lisp(env, code).unwrap();

    let env_read = shell_env.read();
    assert_eq!(env_read.mcp_servers.len(), 1);
    let server = &env_read.mcp_servers[0];
    assert_eq!(server.label, "test-http");
    assert_eq!(server.description, Some("HTTP Server".to_string()));

    if let McpTransport::Http {
        url,
        auth_header,
        allow_stateless,
    } = &server.transport
    {
        assert_eq!(url, "http://localhost:8080/mcp");
        assert_eq!(auth_header.as_deref(), Some("Bearer token"));
        assert_eq!(*allow_stateless, Some(true));
    } else {
        panic!("Expected HTTP transport");
    }
}

#[test]
fn test_mcp_list() {
    let (env, _shell_env) = create_test_env();

    // Initial check (empty)
    let result = run_lisp(env.clone(), "(mcp-list)").unwrap();
    assert_eq!(result, Value::List(List::NIL));

    // Add servers
    run_lisp(env.clone(), r#"(mcp-add-stdio "srv1" "echo")"#).unwrap();
    run_lisp(env.clone(), r#"(mcp-add-sse "srv2" "http://localhost")"#).unwrap();

    // Check list
    let result = run_lisp(env.clone(), "(mcp-list)").unwrap();

    if let Value::List(list) = result {
        let labels: Vec<String> = list
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s,
                _ => panic!("Expected string in mcp-list result"),
            })
            .collect();

        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"srv1".to_string()));
        assert!(labels.contains(&"srv2".to_string()));
    } else {
        panic!("mcp-list should return a list");
    }
}

#[test]
fn test_mcp_clear() {
    let (env, shell_env) = create_test_env();

    // Add two servers
    run_lisp(env.clone(), r#"(mcp-add-sse "s1" "url1")"#).unwrap();
    run_lisp(env.clone(), r#"(mcp-add-sse "s2" "url2")"#).unwrap();

    assert_eq!(shell_env.read().mcp_servers.len(), 2);

    // Clear
    run_lisp(env.clone(), "(mcp-clear)").unwrap();

    assert_eq!(shell_env.read().mcp_servers.len(), 0);
}

#[test]
fn test_chat_execute_allowlist() {
    let (env, shell_env) = create_test_env();

    run_lisp(env.clone(), r#"(chat-execute-add "ls" "grep")"#).unwrap();

    {
        let env_read = shell_env.read();
        assert_eq!(env_read.execute_allowlist.len(), 2);
        assert!(env_read.execute_allowlist.contains(&"ls".to_string()));
        assert!(env_read.execute_allowlist.contains(&"grep".to_string()));
    }

    run_lisp(env.clone(), "(chat-execute-clear)").unwrap();

    assert_eq!(shell_env.read().execute_allowlist.len(), 0);
}
