use super::*;
use tokio::io::AsyncReadExt;

#[test]
fn tool_names_are_stable_bounded_and_preserve_unambiguous_names() {
    assert_eq!(
        stable_name("mcp__server__read", "server", "read"),
        "mcp__server__read"
    );
    let first = stable_name("mcp__a_b__read", "a-b", "read");
    let second = stable_name("mcp__a_b__read", "a_b", "read");
    assert_ne!(first, second);
    assert_eq!(first, stable_name("mcp__a_b__read", "a-b", "read"));
    assert!(stable_name(&"x".repeat(100), "label", "tool").len() <= 64);
}

#[test]
fn mcp_log_names_are_sanitized() {
    assert_eq!(sanitized_command_name("/usr/bin/node"), "node");
    assert_eq!(
        sanitized_command_name("bad/name with spaces"),
        "name_with_spaces"
    );
    assert_eq!(sanitized_command_name(""), "unknown");
}

#[tokio::test]
async fn mcp_stdio_transport_uses_configured_stderr() {
    let marker = "dsh-mcp-stderr-regression";
    let mut cmd = Command::new("sh");
    cmd.args(["-c", &format!("printf '{marker}\\n' >&2")]);

    let (_transport, stderr) = spawn_mcp_stdio_transport_with_stderr(cmd, Stdio::piped()).unwrap();
    let mut stderr = stderr.expect("stderr should be piped by the MCP transport builder");
    let mut output = String::new();
    tokio::time::timeout(Duration::from_secs(1), stderr.read_to_string(&mut output))
        .await
        .expect("stderr read should finish")
        .expect("stderr should be readable");

    assert!(output.contains(marker));
}

fn mock_server(label: &str) -> McpServer {
    McpServer {
        label: label.to_string(),
        description: Some(format!("{label} server")),
        transport: McpTransport::Sse {
            url: format!("https://example.com/{label}"),
        },
        tools: Vec::new(),
    }
}

fn discovery_fixture(dir: &Path, label: &str, slow: bool) -> McpServer {
    let script = dir.join(format!("{label}.py"));
    let marker = dir.join(format!("{label}.started"));
    std::fs::write(&script, r#"
import json, sys, time
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    if request['method'] == 'initialize':
        result = {'protocolVersion':request['params']['protocolVersion'], 'capabilities':{'tools':{}}, 'serverInfo':{'name':'fixture','version':'1'}}
    else:
        open(sys.argv[1], 'w').close()
        if sys.argv[2] == 'slow': time.sleep(10)
        result = {'tools':[{'name':'discovered','inputSchema':{'type':'object'}}]}
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}), flush=True)
"#).unwrap();
    McpServer {
        label: label.into(),
        description: None,
        tools: vec![],
        transport: McpTransport::Stdio {
            command: "python3".into(),
            args: vec![
                script.to_string_lossy().into(),
                marker.to_string_lossy().into(),
                if slow { "slow" } else { "fast" }.into(),
            ],
            env: Default::default(),
            cwd: None,
        },
    }
}

#[test]
fn discovery_keeps_cached_tools_and_refreshes_healthy_servers_after_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = McpManager::default();
    let cached: Tool =
        serde_json::from_value(json!({"name":"cached","inputSchema":{"type":"object"}})).unwrap();
    let mut offline = mock_server("offline");
    offline.tools = vec![cached.clone()];
    manager.register_server(offline, vec![cached]).unwrap();
    manager
        .register_server(discovery_fixture(dir.path(), "healthy", false), vec![])
        .unwrap();
    manager.tools_refreshed = Instant::now() - Duration::from_secs(301);
    manager.refresh_tools_if_expired(&|| false).unwrap();
    assert!(manager.has_tool_binding("mcp__offline__cached"));
    assert!(manager.has_tool_binding("mcp__healthy__discovered"));
    assert!(manager.connection_errors_read().contains_key("offline"));
}

#[test]
fn discovery_cancellation_interrupts_wait_and_skips_remaining_servers() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = McpManager::default();
    manager
        .register_server(discovery_fixture(dir.path(), "slow", true), vec![])
        .unwrap();
    manager
        .register_server(discovery_fixture(dir.path(), "untouched", false), vec![])
        .unwrap();
    manager.tools_refreshed = Instant::now() - Duration::from_secs(301);
    let started = Instant::now();
    assert!(
        manager
            .refresh_tools_if_expired(&|| started.elapsed() >= Duration::from_millis(250))
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!dir.path().join("untouched.started").exists());
    assert!(manager.tools_refreshed.elapsed() > Duration::from_secs(300));
}

fn mock_config(label: &str) -> McpServerConfig {
    McpServerConfig {
        label: label.to_string(),
        description: Some(format!("{label} server")),
        transport: McpTransport::Sse {
            url: format!("https://example.com/{label}"),
        },
    }
}

/// A tool's own name is what the safety guard classifies.
#[test]
fn a_binding_reports_the_tool_name_behind_the_namespaced_one() {
    let mut manager = McpManager::default();
    manager.servers.push(mock_server("ops"));
    manager.bindings.insert(
        "mcp__ops__bash".to_string(),
        ToolBinding {
            server_label: "ops".to_string(),
            tool_name: "bash".to_string(),
            function_name: "mcp__ops__bash".to_string(),
        },
    );

    assert_eq!(
        manager.tool_name_for("mcp__ops__bash").as_deref(),
        Some("bash")
    );
    assert_eq!(manager.tool_name_for("mcp__ops__missing"), None);
}

/// `disconnect` used to clear metadata only, so `mcp status` said the
/// server was disconnected while the agent kept calling its tools.
#[test]
fn a_disconnected_server_stops_offering_its_tools() {
    let mut manager = McpManager::default();
    let mut server = mock_server("ops");
    server.tools.push(Tool::new(
        "bash",
        "run a command",
        Arc::new(serde_json::Map::new()),
    ));
    manager.servers.push(server);
    manager.bindings.insert(
        "mcp__ops__bash".to_string(),
        ToolBinding {
            server_label: "ops".to_string(),
            tool_name: "bash".to_string(),
            function_name: "mcp__ops__bash".to_string(),
        },
    );

    assert_eq!(manager.tool_definitions().len(), 1);
    assert!(manager.system_prompt_fragment().is_some());

    manager.disconnect("ops").unwrap();

    assert!(manager.is_disabled("ops"));
    assert!(manager.tool_definitions().is_empty());
    assert!(manager.system_prompt_fragment().is_none());
    let err = manager
        .execute_tool("mcp__ops__bash", "{}")
        .expect_err("a disconnected server must not run tools");
    assert!(err.contains("disconnected"), "{err}");

    // An unknown server is a mistake worth reporting, not a silent no-op.
    assert!(manager.disconnect("nope").is_err());
}

#[test]
fn disconnect_all_takes_every_server_out_of_service() {
    let mut manager = McpManager::default();
    manager.servers.push(mock_server("alpha"));
    manager.servers.push(mock_server("beta"));

    manager.disconnect_all();

    assert!(manager.is_disabled("alpha"));
    assert!(manager.is_disabled("beta"));
}

#[test]
fn execute_tool_rejects_an_unknown_binding() {
    let manager = McpManager::default();
    let err = manager
        .execute_tool("mcp__ghost__missing", "{}")
        .expect_err("an unknown binding must not look successful");

    assert!(err.contains("mcp__ghost__missing"), "{err}");
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn test_remove_server_cleans_related_state() {
    let mut manager = McpManager::default();
    manager.servers.push(mock_server("alpha"));
    manager.servers.push(mock_server("beta"));

    manager.bindings.insert(
        "mcp__alpha__tool".to_string(),
        ToolBinding {
            server_label: "alpha".to_string(),
            tool_name: "tool".to_string(),
            function_name: "mcp__alpha__tool".to_string(),
        },
    );
    manager.bindings.insert(
        "mcp__beta__tool".to_string(),
        ToolBinding {
            server_label: "beta".to_string(),
            tool_name: "tool".to_string(),
            function_name: "mcp__beta__tool".to_string(),
        },
    );

    manager.session_meta.write().unwrap().insert(
        "alpha".to_string(),
        SessionMeta {
            connected_at: Instant::now(),
        },
    );
    manager
        .connection_errors
        .write()
        .unwrap()
        .insert("alpha".to_string(), "error".to_string());

    assert!(manager.remove_server("alpha"));
    assert_eq!(manager.server_count(), 1);
    assert_eq!(manager.servers[0].label, "beta");
    assert!(
        !manager
            .bindings
            .values()
            .any(|binding| binding.server_label == "alpha")
    );
    assert!(!manager.session_meta.read().unwrap().contains_key("alpha"));
    assert!(
        !manager
            .connection_errors
            .read()
            .unwrap()
            .contains_key("alpha")
    );
}

#[test]
fn test_sync_servers_blocking_ignores_order_only() {
    let mut manager = McpManager::default();
    manager.servers.push(mock_server("alpha"));
    manager.servers.push(mock_server("beta"));

    let stats = manager.sync_servers_blocking(vec![mock_config("beta"), mock_config("alpha")]);

    assert_eq!(
        stats,
        McpSyncStats {
            removed: 0,
            updated: 0,
            added: 0,
            unchanged: 2
        }
    );
    assert_eq!(manager.server_count(), 2);
}

#[test]
fn test_sync_servers_blocking_removes_missing_servers() {
    let mut manager = McpManager::default();
    manager.servers.push(mock_server("alpha"));
    manager.servers.push(mock_server("beta"));

    let stats = manager.sync_servers_blocking(vec![mock_config("beta")]);

    assert_eq!(
        stats,
        McpSyncStats {
            removed: 1,
            updated: 0,
            added: 0,
            unchanged: 1
        }
    );
    assert_eq!(manager.server_count(), 1);
    assert_eq!(manager.servers[0].label, "beta");
}

#[test]
fn test_runtime_state_snapshot_restore() {
    let manager = McpManager::default();
    manager.session_meta.write().unwrap().insert(
        "alpha".to_string(),
        SessionMeta {
            connected_at: Instant::now(),
        },
    );
    manager
        .connection_errors
        .write()
        .unwrap()
        .insert("beta".to_string(), "network error".to_string());

    let snapshot = manager.snapshot_runtime_state();

    manager.session_meta.write().unwrap().clear();
    manager
        .connection_errors
        .write()
        .unwrap()
        .insert("gamma".to_string(), "temporary mutation".to_string());

    manager.restore_runtime_state(snapshot.clone());

    assert_eq!(manager.snapshot_runtime_state(), snapshot);
}

#[tokio::test]
async fn test_sse_transport_error() {
    let transport = McpTransport::Sse {
        url: "http://localhost:8080/sse".to_string(),
    };

    // Test list_tools_via_transport
    let result = list_tools_via_transport(transport.clone()).await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().to_string(),
        LEGACY_SSE_UNSUPPORTED_MESSAGE
    );

    // Test call_tool_via_transport
    let result = connection::open(transport).await;
    // Verify the specific error message
    match result {
        Err(e) => assert_eq!(e.to_string(), LEGACY_SSE_UNSUPPORTED_MESSAGE),
        Ok(_) => panic!("Expected SSE transport to fail"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_load_blocking_inside_runtime_does_not_panic() {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        McpManager::load_blocking(vec![])
    }));

    assert!(result.is_ok(), "load_blocking panicked inside runtime");
    assert!(result.expect("panic check").is_empty());
}
