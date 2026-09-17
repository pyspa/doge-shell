//! MCP (Model Context Protocol) builtin command
//!
//! This command provides a CLI interface for managing MCP servers.

use crate::capability::ExecutionCapability;
use crate::{CoreShellAction, ShellProxy};
use anyhow::Result;
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
        "tools" | "t" => cmd_tools(ctx, &args[1..], proxy),
        "groups" | "group" | "g" => cmd_group(ctx, &args[1..], proxy),
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
  tools, t [--active] [--group <name>]
                         List registered MCP tools (only active ones with
                         --active, only one group's with --group)
  groups, group, g       Manage MCP tool groups (see below)
  help                   Show this help message

Group subcommands:
  mcp group list              List tool groups and their exposure state
  mcp group show <group>      Show one group's tools and state
  mcp group enable <group>    Offer a group's tools to AI again
  mcp group disable <group>   Hide a group's tools from AI (keeps connection)

Examples:
  mcp status
  mcp connect chrome-devtools
  mcp disconnect
  mcp tools --active
  mcp group disable browser
"#
    );
}

fn cmd_status(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = dispatch_lisp(ctx, proxy, "(mcp-status)".to_string()) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_connect(ctx: &Context, label: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let code = format!("(mcp-connect \"{}\")", label);
    if let Err(e) = dispatch_lisp(ctx, proxy, code) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_disconnect(ctx: &Context, label: &str, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let code = format!("(mcp-disconnect \"{}\")", label);
    if let Err(e) = dispatch_lisp(ctx, proxy, code) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_disconnect_all(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = dispatch_lisp(ctx, proxy, "(mcp-disconnect-all)".to_string()) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_list(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if let Err(e) = dispatch_lisp(ctx, proxy, "(mcp-list)".to_string()) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn cmd_tools(ctx: &Context, args: &[&str], proxy: &mut dyn ShellProxy) -> ExitStatus {
    let options = match parse_tools_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("Usage: mcp tools [--active] [--group <name>]");
            return ExitStatus::ExitedWith(1);
        }
    };
    let code = match (options.active, options.group) {
        (false, None) => "(mcp-list-tools)".to_string(),
        (true, None) => "(mcp-list-tools-active)".to_string(),
        (false, Some(group)) => format!("(mcp-group-tools \"{group}\")"),
        (true, Some(group)) => format!("(mcp-group-tools \"{group}\" \"active\")"),
    };
    if let Err(e) = dispatch_lisp(ctx, proxy, code) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

/// Flags accepted after `mcp tools`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ToolsOptions {
    active: bool,
    group: Option<String>,
}

fn parse_tools_args(args: &[&str]) -> Result<ToolsOptions, String> {
    let mut options = ToolsOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--active" => options.active = true,
            "--group" => {
                index += 1;
                let name = args
                    .get(index)
                    .filter(|name| !name.starts_with('-'))
                    .ok_or("mcp tools: --group requires a group name")?;
                options.group = Some((*name).to_string());
            }
            flag if flag.starts_with("--group=") => {
                let name = flag.trim_start_matches("--group=");
                if name.is_empty() {
                    return Err("mcp tools: --group requires a group name".to_string());
                }
                options.group = Some(name.to_string());
            }
            unknown => return Err(format!("mcp tools: unknown option '{unknown}'")),
        }
        index += 1;
    }
    Ok(options)
}

fn cmd_group(ctx: &Context, args: &[&str], proxy: &mut dyn ShellProxy) -> ExitStatus {
    if args.is_empty() {
        return dispatch_lisp_result(ctx, proxy, "(mcp-groups)".to_string());
    }
    match args[0] {
        "list" | "l" => dispatch_lisp_result(ctx, proxy, "(mcp-groups)".to_string()),
        "show" | "s" => {
            let Some(name) = args.get(1) else {
                eprintln!("Usage: mcp group show <group>");
                return ExitStatus::ExitedWith(1);
            };
            dispatch_lisp_result(ctx, proxy, format!("(mcp-group-show \"{name}\")"))
        }
        "enable" | "e" => {
            let Some(name) = args.get(1) else {
                eprintln!("Usage: mcp group enable <group>");
                return ExitStatus::ExitedWith(1);
            };
            dispatch_lisp_result(ctx, proxy, format!("(mcp-group-enable \"{name}\")"))
        }
        "disable" | "d" => {
            let Some(name) = args.get(1) else {
                eprintln!("Usage: mcp group disable <group>");
                return ExitStatus::ExitedWith(1);
            };
            dispatch_lisp_result(ctx, proxy, format!("(mcp-group-disable \"{name}\")"))
        }
        "help" | "-h" | "--help" => {
            print_group_help();
            ExitStatus::ExitedWith(0)
        }
        unknown => {
            eprintln!("Unknown group subcommand: {unknown}");
            print_group_help();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn print_group_help() {
    println!(
        r#"mcp group - Manage MCP tool groups

Usage: mcp group <subcommand> [options]

Subcommands:
  list, l              List tool groups and their exposure state
  show, s <group>      Show one group's tools and state
  enable, e <group>    Offer a group's tools to AI again
  disable, d <group>   Hide a group's tools from AI (keeps connection)

A disabled group only hides schemas from AI; the server stays connected.
"#
    );
}

fn dispatch_lisp_result(ctx: &Context, proxy: &mut dyn ShellProxy, code: String) -> ExitStatus {
    if let Err(e) = dispatch_lisp(ctx, proxy, code) {
        eprintln!("Error: {}", e);
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

fn dispatch_lisp(ctx: &Context, proxy: &mut dyn ShellProxy, code: String) -> Result<()> {
    proxy.dispatch_core(ctx, CoreShellAction::Lisp, vec!["lisp".to_string(), code])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;

    fn test_proxy() -> TestShellProxy {
        TestShellProxy {
            allow_dispatch: true,
            ..TestShellProxy::default()
        }
    }

    fn test_context() -> Context {
        let pid = nix::unistd::getpid();
        Context::new_safe(pid, pid, false)
    }

    fn run(argv: &[&str], proxy: &mut TestShellProxy) -> ExitStatus {
        let owned: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
        command(&test_context(), owned, proxy)
    }

    fn lisp_code(proxy: &TestShellProxy) -> Vec<String> {
        proxy
            .dispatched
            .iter()
            .map(|(_, argv)| argv[1].clone())
            .collect()
    }

    #[test]
    fn tools_without_flags_lists_all_registered_tools() {
        let mut proxy = test_proxy();
        assert_eq!(
            run(&["mcp", "tools"], &mut proxy),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(lisp_code(&proxy), vec!["(mcp-list-tools)"]);
    }

    #[test]
    fn tools_active_lists_only_exposed_tools() {
        let mut proxy = test_proxy();
        assert_eq!(
            run(&["mcp", "tools", "--active"], &mut proxy),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(lisp_code(&proxy), vec!["(mcp-list-tools-active)"]);
    }

    #[test]
    fn tools_group_selects_one_group() {
        for argv in [
            vec!["mcp", "tools", "--group", "github"],
            vec!["mcp", "tools", "--group=github"],
        ] {
            let mut proxy = test_proxy();
            assert_eq!(run(&argv, &mut proxy), ExitStatus::ExitedWith(0));
            assert_eq!(lisp_code(&proxy), vec!["(mcp-group-tools \"github\")"]);
        }
    }

    #[test]
    fn tools_active_and_group_combine() {
        let mut proxy = test_proxy();
        assert_eq!(
            run(
                &["mcp", "tools", "--active", "--group", "github"],
                &mut proxy
            ),
            ExitStatus::ExitedWith(0)
        );
        assert_eq!(
            lisp_code(&proxy),
            vec!["(mcp-group-tools \"github\" \"active\")"]
        );
    }

    #[test]
    fn tools_rejects_unknown_or_incomplete_options() {
        for argv in [
            vec!["mcp", "tools", "--bogus"],
            vec!["mcp", "tools", "--group"],
            vec!["mcp", "tools", "--group="],
            vec!["mcp", "tools", "--group", "--active"],
        ] {
            let mut proxy = test_proxy();
            assert_eq!(run(&argv, &mut proxy), ExitStatus::ExitedWith(1));
            assert!(proxy.dispatched.is_empty());
        }
    }

    #[test]
    fn group_list_aliases_dispatch_group_listing() {
        for argv in [
            vec!["mcp", "groups"],
            vec!["mcp", "group", "list"],
            vec!["mcp", "g", "l"],
        ] {
            let mut proxy = test_proxy();
            assert_eq!(run(&argv, &mut proxy), ExitStatus::ExitedWith(0));
            assert_eq!(lisp_code(&proxy), vec!["(mcp-groups)"]);
        }
    }

    #[test]
    fn group_show_enable_disable_dispatch_with_name() {
        for (subcommand, code) in [
            ("show", "(mcp-group-show \"github\")"),
            ("s", "(mcp-group-show \"github\")"),
            ("enable", "(mcp-group-enable \"github\")"),
            ("e", "(mcp-group-enable \"github\")"),
            ("disable", "(mcp-group-disable \"github\")"),
            ("d", "(mcp-group-disable \"github\")"),
        ] {
            let mut proxy = test_proxy();
            assert_eq!(
                run(&["mcp", "group", subcommand, "github"], &mut proxy),
                ExitStatus::ExitedWith(0)
            );
            assert_eq!(lisp_code(&proxy), vec![code]);
        }
    }

    #[test]
    fn group_actions_require_a_name() {
        for subcommand in ["show", "enable", "disable"] {
            let mut proxy = test_proxy();
            assert_eq!(
                run(&["mcp", "group", subcommand], &mut proxy),
                ExitStatus::ExitedWith(1)
            );
            assert!(proxy.dispatched.is_empty());
        }
    }

    #[test]
    fn group_rejects_unknown_subcommands() {
        let mut proxy = test_proxy();
        assert_eq!(
            run(&["mcp", "group", "bogus"], &mut proxy),
            ExitStatus::ExitedWith(1)
        );
        assert!(proxy.dispatched.is_empty());
    }
}
