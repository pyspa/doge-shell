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
            declared_read_only: None,
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
            declared_read_only: None,
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
            declared_read_only: None,
        },
    );
    manager.bindings.insert(
        "mcp__beta__tool".to_string(),
        ToolBinding {
            server_label: "beta".to_string(),
            tool_name: "tool".to_string(),
            function_name: "mcp__beta__tool".to_string(),
            declared_read_only: None,
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

/// The gate's judgement now depends on a field that travels from the server's
/// tool listing, through `bind_tool`, to `tool_facts_for`. Every binding in
/// these tests is built by hand, so without this the whole path could stop
/// being wired and every test would still pass.
#[test]
fn a_binding_carries_what_the_server_declared_about_side_effects() {
    use rmcp::model::ToolAnnotations;

    let annotated = |annotations: Option<ToolAnnotations>| {
        let mut tool = Tool::new("sync", "sync things", Arc::new(serde_json::Map::new()));
        tool.annotations = annotations;
        let (function_name, binding) = super::bind_tool("ops", &tool);
        assert_eq!(function_name, "mcp__ops__sync");
        assert_eq!(binding.tool_name, "sync");
        binding.declared_read_only
    };

    // Nothing said is not a declaration: `ToolAnnotations` skips serializing
    // `None`, so anything we see the server chose to send.
    assert_eq!(annotated(None), None);
    assert_eq!(annotated(Some(ToolAnnotations::default())), None);

    // `ToolAnnotations` is `#[non_exhaustive]`, so it is built and then set.
    let with = |set: fn(&mut ToolAnnotations)| {
        let mut annotations = ToolAnnotations::new();
        set(&mut annotations);
        Some(annotations)
    };

    // Both of these say "this tool modifies its environment".
    assert_eq!(
        annotated(with(|a| a.read_only_hint = Some(false))),
        Some(false)
    );
    assert_eq!(
        annotated(with(|a| a.destructive_hint = Some(true))),
        Some(false)
    );

    // And this one is passed through for the guard to refuse to act on.
    assert_eq!(
        annotated(with(|a| a.read_only_hint = Some(true))),
        Some(true)
    );
}

/// The two halves of one judgement come from one lookup.
#[test]
fn tool_facts_report_the_name_and_the_declaration_together() {
    let mut manager = McpManager::default();
    manager.servers.push(mock_server("ops"));
    manager.bindings.insert(
        "mcp__ops__sync".to_string(),
        ToolBinding {
            server_label: "ops".to_string(),
            tool_name: "sync".to_string(),
            function_name: "mcp__ops__sync".to_string(),
            declared_read_only: Some(false),
        },
    );

    assert_eq!(
        manager.tool_facts_for("mcp__ops__sync"),
        Some(("sync".to_string(), Some(false)))
    );
    assert_eq!(manager.tool_facts_for("mcp__ops__missing"), None);
}

fn group_tool(name: &str) -> Tool {
    Tool::new(
        name.to_string(),
        format!("{name} description"),
        Arc::new(serde_json::Map::new()),
    )
}

/// Two servers, one implicit group each: the Phase 1 group model.
fn grouped_manager() -> McpManager {
    let mut manager = McpManager::default();
    let github_tools = vec![
        group_tool("list_issues"),
        group_tool("get_issue"),
        group_tool("create_issue"),
    ];
    let mut github = mock_server("github");
    github.tools = github_tools.clone();
    manager.register_server(github, github_tools).unwrap();
    let fs_tools = vec![group_tool("read_file"), group_tool("write_file")];
    let mut filesystem = mock_server("filesystem");
    filesystem.description = None;
    filesystem.tools = fs_tools.clone();
    manager.register_server(filesystem, fs_tools).unwrap();
    manager
}

#[test]
fn implicit_groups_mirror_their_servers() {
    let groups = grouped_manager().tool_groups();

    assert_eq!(groups.len(), 2);
    let github = groups.iter().find(|group| group.name == "github").unwrap();
    assert_eq!(github.description.as_deref(), Some("github server"));
    assert!(github.enabled);
    assert_eq!(
        github.tools,
        vec![
            "mcp__github__create_issue",
            "mcp__github__get_issue",
            "mcp__github__list_issues",
        ]
    );
    // No server description: the group still gets a short fallback.
    let filesystem = groups
        .iter()
        .find(|group| group.name == "filesystem")
        .unwrap();
    assert_eq!(
        filesystem.description.as_deref(),
        Some("Tools provided by MCP server 'filesystem'")
    );
}

#[test]
fn disabling_a_group_hides_only_its_tools() {
    let manager = grouped_manager();
    assert_eq!(manager.all_tool_definitions().len(), 5);
    assert_eq!(manager.active_tool_count(), 5);

    assert_eq!(manager.disable_group("github"), Ok(true));
    assert!(!manager.is_group_enabled("github"));
    assert!(manager.is_group_enabled("filesystem"));

    assert_eq!(manager.all_tool_definitions().len(), 5);
    assert_eq!(manager.active_tool_count(), 2);
    // The compatibility alias follows the active set.
    assert_eq!(manager.tool_definitions().len(), 2);
    assert!(manager.group_tool_definitions("github").is_empty());
    assert_eq!(manager.group_tool_definitions("filesystem").len(), 2);

    assert_eq!(manager.enable_group("github"), Ok(true));
    assert_eq!(manager.active_tool_count(), 5);
    // Re-enabling an active group changes nothing and duplicates nothing.
    assert_eq!(manager.enable_group("github"), Ok(false));
    assert_eq!(manager.active_tool_count(), 5);
    assert_eq!(manager.disable_group("github"), Ok(true));
    assert_eq!(manager.disable_group("github"), Ok(false));
}

#[test]
fn disabling_an_unknown_group_names_what_exists() {
    let manager = grouped_manager();
    let err = manager.disable_group("nope").unwrap_err();
    assert!(err.contains("Unknown MCP tool group: 'nope'"), "{err}");
    assert!(err.contains("github"), "{err}");
    assert!(err.contains("filesystem"), "{err}");
    assert_eq!(manager.enable_group("nope").unwrap_err(), err);
}

#[test]
fn group_disable_is_exposure_only_not_a_disconnect() {
    let manager = grouped_manager();
    manager.disable_group("github").unwrap();

    // The binding survives: execution still resolves, and the server is not
    // marked disconnected. Only the model's view shrinks.
    assert!(manager.has_tool_binding("mcp__github__list_issues"));
    assert!(!manager.is_disabled("github"));
    assert!(manager.system_prompt_fragment().is_some());
}

#[test]
fn enabling_a_group_on_a_disconnected_server_fails() {
    let manager = grouped_manager();
    manager.disconnect("filesystem").unwrap();
    // Enabling the toggle must not report success while the tools stay
    // hidden: the operator has to reconnect first.
    let err = manager.enable_group("filesystem").unwrap_err();
    assert!(err.contains("disconnected"), "{err}");
    assert!(err.contains("mcp connect filesystem"), "{err}");
    assert!(manager.is_group_enabled("filesystem"));
    assert_eq!(manager.active_tool_count(), 3);
    assert!(manager.group_tool_definitions("filesystem").is_empty());
}

#[test]
fn same_tool_name_on_two_servers_does_not_collide() {
    let mut manager = McpManager::default();
    for label in ["alpha", "beta"] {
        let tools = vec![group_tool("search")];
        let mut server = mock_server(label);
        server.tools = tools.clone();
        manager.register_server(server, tools).unwrap();
    }
    let mut names: Vec<String> = manager
        .active_tool_definitions()
        .into_iter()
        .filter_map(|definition| {
            definition
                .get("function")?
                .get("name")?
                .as_str()
                .map(str::to_string)
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 2);

    manager.disable_group("alpha").unwrap();
    assert_eq!(manager.active_tool_count(), 1);
    assert_eq!(manager.group_tool_definitions("beta").len(), 1);
}

#[test]
fn tool_exposure_reports_total_and_active_footprint() {
    let manager = grouped_manager();
    let exposure = manager.tool_exposure();
    assert_eq!(exposure.total_tools, 5);
    assert_eq!(exposure.active_tools, 5);
    assert_eq!(exposure.total_groups, 2);
    assert_eq!(exposure.active_groups, 2);
    assert!(exposure.schema_bytes > 0);

    manager.disable_group("github").unwrap();
    let exposure = manager.tool_exposure();
    assert_eq!(exposure.total_tools, 5);
    assert_eq!(exposure.active_tools, 2);
    assert_eq!(exposure.active_groups, 1);
    assert!(exposure.schema_bytes > 0);
}
