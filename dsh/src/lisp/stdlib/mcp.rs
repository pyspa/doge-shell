//! Lisp bindings for MCP servers, tools, and tool groups.
//!
//! Thin printers over `McpManager`: server registration (`mcp-add-*`),
//! connection control (`mcp-connect` / `mcp-disconnect`), tool listings
//! (`mcp-list-tools*`, `mcp-group-tools`), group exposure control
//! (`mcp-groups`, `mcp-group-show/enable/disable`), and the `mcp-status`
//! report. The `mcp` builtin command dispatches here.
use crate::lisp::model::{Env, List, Symbol, Value};
use crate::lisp::utils::{
    list_of_pairs, list_of_strings, optional_bool, optional_string, require_typed_arg,
};
use dsh_types::mcp::{McpServerConfig, McpTransport};

pub fn register(env: &mut Env) {
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

            // Registered so `mcp list` and `doctor mcp` still show it, but said
            // out loud here: every connection to it fails, and the error
            // otherwise arrives when the model reaches for a tool - long after
            // the person who wrote this line could do anything about it.
            eprintln!(
                "mcp-add-sse: `{label}` will not connect. {}",
                dsh_builtin::LEGACY_SSE_UNSUPPORTED_MESSAGE
            );

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
        Symbol::from("mcp-list"),
        Value::NativeFunc(|env, _args| {
            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let servers = env_read.mcp_servers();

            if servers.is_empty() {
                println!("No MCP servers configured.");
                return Ok(Value::List(List::NIL));
            }

            println!("{:<20} {:<10} DESCRIPTION", "LABEL", "TYPE");
            println!("{:<20} {:<10} -----------", "-----", "----");

            let mut labels = Vec::new();

            for server in servers {
                let transport_type = match server.transport {
                    McpTransport::Stdio { .. } => "Stdio",
                    McpTransport::Sse { .. } => "SSE",
                    McpTransport::Http { .. } => "HTTP",
                };
                let desc = server.description.as_deref().unwrap_or("");
                println!("{:<20} {:<10} {}", server.label, transport_type, desc);
                labels.push(Value::String(server.label.clone()));
            }

            Ok(Value::List(labels.into_iter().collect()))
        }),
    );

    env.define(
        Symbol::from("mcp-list-tools"),
        Value::NativeFunc(|env, _args| {
            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            // Registered tools, including hidden and disconnected ones: the
            // diagnostic view. Use `mcp-list-tools-active` for what the model
            // currently sees.
            Ok(print_tool_definitions(
                manager.all_tool_definitions(),
                "No MCP tools available.",
            ))
        }),
    );

    env.define(
        Symbol::from("mcp-list-tools-active"),
        Value::NativeFunc(|env, _args| {
            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            Ok(print_tool_definitions(
                manager.active_tool_definitions(),
                "No active MCP tools currently exposed to AI.",
            ))
        }),
    );

    // mcp-group-tools: List the tools of one group.
    // With no mode (or any mode but "active") this is the group's registered
    // membership; with "active" it is the subset currently exposed to AI.
    env.define(
        Symbol::from("mcp-group-tools"),
        Value::NativeFunc(|env, args| {
            let group = require_typed_arg::<&String>("mcp-group-tools", &args, 0)?.clone();
            let mode = args
                .get(1)
                .map(|value| match value {
                    Value::String(text) => text.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();

            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            if !manager
                .tool_groups()
                .iter()
                .any(|known| known.name == group)
            {
                println!("Unknown MCP tool group: '{group}'");
                println!("Run `mcp group list` to see available groups.");
                return Ok(Value::False);
            }
            let tools = if mode == "active" {
                manager.group_tool_definitions(&group)
            } else {
                let members: std::collections::HashSet<String> = manager
                    .tool_groups()
                    .into_iter()
                    .find(|known| known.name == group)
                    .map(|known| known.tools.into_iter().collect())
                    .unwrap_or_default();
                manager
                    .all_tool_definitions()
                    .into_iter()
                    .filter(|definition| {
                        definition
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(|name| name.as_str())
                            .is_some_and(|name| members.contains(name))
                    })
                    .collect()
            };
            Ok(print_tool_definitions(
                tools,
                &format!("No MCP tools in group '{group}'."),
            ))
        }),
    );

    // mcp-groups: List MCP tool groups with their exposure state
    env.define(
        Symbol::from("mcp-groups"),
        Value::NativeFunc(|env, _args| {
            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            let groups = manager.tool_groups();

            if groups.is_empty() {
                println!("No MCP tool groups available.");
                return Ok(Value::List(List::NIL));
            }

            println!("{:<20} {:<8} TOOLS", "GROUP", "ENABLED");
            println!("{:<20} {:<8} -----", "-----", "-------");

            let mut names = Vec::new();
            for group in groups {
                println!(
                    "{:<20} {:<8} {}",
                    group.name,
                    if group.enabled { "yes" } else { "no" },
                    group.tools.len()
                );
                names.push(Value::String(group.name));
            }

            Ok(Value::List(names.into_iter().collect()))
        }),
    );

    // mcp-group-show: Show one group's membership and exposure state
    env.define(
        Symbol::from("mcp-group-show"),
        Value::NativeFunc(|env, args| {
            let group = require_typed_arg::<&String>("mcp-group-show", &args, 0)?.clone();

            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            let Some(known) = manager.tool_groups().into_iter().find(|g| g.name == group) else {
                println!("Unknown MCP tool group: '{group}'");
                println!("Run `mcp group list` to see available groups.");
                return Ok(Value::False);
            };

            println!("Group: {}", known.name);
            println!(
                "Description: {}",
                known.description.as_deref().unwrap_or("")
            );
            println!(
                "Status: {}",
                if known.enabled { "enabled" } else { "disabled" }
            );
            if manager.is_disabled(&group) {
                println!(
                    "Server '{group}' is disconnected; run `mcp connect {group}` to use its tools."
                );
            }
            println!();
            println!("Tools:");
            for tool in &known.tools {
                println!("  {tool}");
            }

            Ok(Value::True)
        }),
    );

    // mcp-group-enable: Offer a group's schemas to AI again
    env.define(
        Symbol::from("mcp-group-enable"),
        Value::NativeFunc(|env, args| {
            let group = require_typed_arg::<&String>("mcp-group-enable", &args, 0)?.clone();

            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();

            match manager.enable_group(&group) {
                Ok(true) => {
                    println!("Enabled MCP tool group: {group}");
                    Ok(Value::True)
                }
                Ok(false) => {
                    println!("MCP tool group '{group}' is already active.");
                    Ok(Value::True)
                }
                Err(err) => {
                    println!("{err}");
                    Ok(Value::False)
                }
            }
        }),
    );

    // mcp-group-disable: Hide a group's schemas from AI, keeping the connection
    env.define(
        Symbol::from("mcp-group-disable"),
        Value::NativeFunc(|env, args| {
            let group = require_typed_arg::<&String>("mcp-group-disable", &args, 0)?.clone();

            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();

            match manager.disable_group(&group) {
                Ok(true) => {
                    println!("Disabled MCP tool group: {group}");
                    Ok(Value::True)
                }
                Ok(false) => {
                    println!("MCP tool group '{group}' is already inactive.");
                    Ok(Value::True)
                }
                Err(err) => {
                    println!("{err}");
                    Ok(Value::False)
                }
            }
        }),
    );

    // mcp-status: Show connection status of all MCP servers
    env.define(
        Symbol::from("mcp-status"),
        Value::NativeFunc(|env, _args| {
            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();
            let statuses = manager.get_status();

            if statuses.is_empty() {
                println!("No MCP servers configured.");
                return Ok(Value::List(List::NIL));
            }

            println!(
                "{:<20} {:<12} {:<8} {:<6} UPTIME",
                "LABEL", "STATUS", "TYPE", "TOOLS"
            );
            println!(
                "{:<20} {:<12} {:<8} {:<6} ------",
                "-----", "------", "----", "-----"
            );

            let mut labels = Vec::new();

            for status in statuses {
                let uptime = if let Some(since) = status.connected_since {
                    let elapsed = since.elapsed();
                    if elapsed.as_secs() < 60 {
                        format!("{}s", elapsed.as_secs())
                    } else if elapsed.as_secs() < 3600 {
                        format!("{}m {}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
                    } else {
                        format!(
                            "{}h {}m",
                            elapsed.as_secs() / 3600,
                            (elapsed.as_secs() % 3600) / 60
                        )
                    }
                } else {
                    "-".to_string()
                };

                let status_str = match &status.status {
                    dsh_builtin::McpConnectionStatus::Connected => "connected",
                    dsh_builtin::McpConnectionStatus::Disconnected => "disconnected",
                    dsh_builtin::McpConnectionStatus::Error(_) => "error",
                };

                println!(
                    "{:<20} {:<12} {:<8} {:<6} {}",
                    status.label, status_str, status.transport_type, status.tool_count, uptime
                );
                labels.push(Value::String(status.label));
            }

            let groups = manager.tool_groups();
            if !groups.is_empty() {
                println!();
                println!("Tool Groups:");
                println!("{:<20} {:<8} TOOLS", "GROUP", "ENABLED");
                println!("{:<20} {:<8} -----", "-----", "-------");
                for group in groups {
                    println!(
                        "{:<20} {:<8} {}",
                        group.name,
                        if group.enabled { "yes" } else { "no" },
                        group.tools.len()
                    );
                }
            }

            Ok(Value::List(labels.into_iter().collect()))
        }),
    );

    // mcp-connect: Connect to a specific MCP server
    env.define(
        Symbol::from("mcp-connect"),
        Value::NativeFunc(|env, args| {
            let label = require_typed_arg::<&String>("mcp-connect", &args, 0)?;

            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();

            match manager.connect(label) {
                Ok(()) => {
                    println!("Connected to MCP server: {}", label);
                    Ok(Value::True)
                }
                Err(err) => {
                    println!("Failed to connect to '{}': {}", label, err);
                    Ok(Value::False)
                }
            }
        }),
    );

    // mcp-disconnect: Disconnect from a specific MCP server
    env.define(
        Symbol::from("mcp-disconnect"),
        Value::NativeFunc(|env, args| {
            let label = require_typed_arg::<&String>("mcp-disconnect", &args, 0)?;

            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();

            match manager.disconnect(label) {
                Ok(()) => {
                    println!("Disconnected from MCP server: {}", label);
                    Ok(Value::True)
                }
                Err(err) => {
                    println!("Failed to disconnect from '{}': {}", label, err);
                    Ok(Value::False)
                }
            }
        }),
    );

    // mcp-disconnect-all: Disconnect from all MCP servers
    env.define(
        Symbol::from("mcp-disconnect-all"),
        Value::NativeFunc(|env, _args| {
            let env_borrow = env.borrow();
            let env_read = env_borrow.shell_env.read();
            let manager = env_read.integration_state.mcp_manager.read();

            manager.disconnect_all();
            println!("Disconnected from all MCP servers");
            Ok(Value::True)
        }),
    );
}

/// Print tool definitions as a NAME/DESCRIPTION table and return the names.
///
/// Shared by `mcp-list-tools`, `mcp-list-tools-active` and `mcp-group-tools`
/// so the three listings cannot drift apart.
fn print_tool_definitions(tools: Vec<serde_json::Value>, empty_message: &str) -> Value {
    if tools.is_empty() {
        println!("{empty_message}");
        return Value::List(List::NIL);
    }

    println!("{:<30} DESCRIPTION", "NAME");
    println!("{:<30} -----------", "----");

    let mut names = Vec::new();

    for tool in tools {
        let name = tool
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown");

        let desc = tool
            .get("function")
            .and_then(|f| f.get("description"))
            .and_then(|d| d.as_str())
            .unwrap_or("");

        // Truncate description if too long
        let desc = if desc.len() > 50 {
            format!("{}...", &desc[..47])
        } else {
            desc.to_string()
        };

        println!("{:<30} {}", name, desc);
        names.push(Value::String(name.to_string()));
    }

    Value::List(names.into_iter().collect())
}
