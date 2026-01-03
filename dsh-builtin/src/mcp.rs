//! MCP (Model Context Protocol) builtin command
//!
//! This command provides a CLI interface for managing MCP servers.

use crate::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn description() -> &'static str {
    "Manage MCP servers (status, connect, disconnect)"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let args: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();

    if args.is_empty() {
        print_help();
        return ExitStatus::ExitedWith(0);
    }

    match args[0] {
        "status" | "s" => cmd_status(ctx, proxy),
        "connect" | "c" => {
            if args.len() < 2 {
                eprintln!("Usage: mcp connect <label>");
                return ExitStatus::ExitedWith(1);
            }
            cmd_connect(ctx, args[1], proxy)
        }
        "disconnect" | "d" => {
            if args.len() < 2 {
                cmd_disconnect_all(ctx, proxy)
            } else {
                cmd_disconnect(ctx, args[1], proxy)
            }
        }
        "list" | "l" => cmd_list(ctx, proxy),
        "tools" | "t" => cmd_tools(ctx, proxy),
        "help" | "-h" | "--help" => {
            print_help();
            ExitStatus::ExitedWith(0)
        }
        unknown => {
            eprintln!("Unknown subcommand: {}", unknown);
            print_help();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn print_help() {
    println!(
        r#"mcp - Manage MCP (Model Context Protocol) servers

Usage: mcp <subcommand> [options]

Subcommands:
  status, s              Show connection status of all MCP servers
  connect, c <label>     Connect to a specific MCP server
  disconnect, d [label]  Disconnect from a server (all if no label given)
  list, l                List registered MCP servers
  tools, t               List available MCP tools
  help                   Show this help message

Examples:
  mcp status
  mcp connect chrome-devtools
  mcp disconnect
"#
    );
}

fn cmd_status(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = proxy.dispatch(
        ctx,
        "lisp",
        vec!["lisp".to_string(), "(mcp-status)".to_string()],
    ) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_connect(ctx: &Context, label: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let code = format!("(mcp-connect \"{}\")", label);
    if let Err(e) = proxy.dispatch(ctx, "lisp", vec!["lisp".to_string(), code]) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_disconnect(ctx: &Context, label: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let code = format!("(mcp-disconnect \"{}\")", label);
    if let Err(e) = proxy.dispatch(ctx, "lisp", vec!["lisp".to_string(), code]) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_disconnect_all(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = proxy.dispatch(
        ctx,
        "lisp",
        vec!["lisp".to_string(), "(mcp-disconnect-all)".to_string()],
    ) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_list(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = proxy.dispatch(
        ctx,
        "lisp",
        vec!["lisp".to_string(), "(mcp-list)".to_string()],
    ) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_tools(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = proxy.dispatch(
        ctx,
        "lisp",
        vec!["lisp".to_string(), "(mcp-list-tools)".to_string()],
    ) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}
