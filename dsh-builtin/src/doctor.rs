//! `doctor`: diagnose shell setup, AI configuration, project detection,
//! runtime skills, safety posture, and suggested validation commands.
//!
//! Each section is an independent submodule with its own `check_*` (text
//! report) function; sections never call each other, so `command()` below
//! is just a dispatch table. `--json` mirrors the same sections through
//! `json.rs`. Shared filesystem/PATH helpers live in `util.rs`.
use crate::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::path::PathBuf;

mod ai;
mod config;
mod dev;
mod hooks;
mod json;
mod performance;
mod project;
mod runtimes;
mod safety;
mod setup;
mod skills;
mod util;

use ai::*;
use config::*;
use dev::*;
use hooks::*;
use json::*;
use performance::*;
use project::*;
use runtimes::*;
use safety::*;
use setup::*;
use skills::*;
use util::*;

pub fn description() -> &'static str {
    "Diagnose config, AI, MCP, project, runtime, skills, safety, setup, and dev validation state"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let json_output = argv.iter().skip(1).any(|value| value == "--json");
    let section = argv
        .iter()
        .skip(1)
        .find(|value| value.as_str() != "--json")
        .map(|value| value.as_str());
    if matches!(section, Some("-h" | "--help" | "help")) {
        return print_help(ctx);
    }
    if section.is_some_and(|value| !is_known_section(value)) {
        let _ = ctx.write_stderr(&format!(
            "doctor: unknown section `{}`. Use `doctor --help`.",
            section.unwrap_or_default()
        ));
        return ExitStatus::ExitedWith(1);
    }

    let current_dir = proxy
        .get_current_dir()
        .unwrap_or_else(|_| PathBuf::from("."));

    if json_output {
        if matches!(section, Some("fix")) {
            let _ = ctx.write_stderr("doctor: fix cannot be combined with --json");
            return ExitStatus::ExitedWith(1);
        }
        return print_json_report(ctx, proxy, &current_dir, section);
    }

    if matches!(section, Some("setup" | "fix")) {
        print_header(ctx, "setup");
        check_setup(ctx, proxy, &current_dir, section == Some("fix"));
        return ExitStatus::ExitedWith(0);
    }

    if show_section(section, "config") {
        print_header(ctx, "config");
        check_config(ctx);
    }
    if show_section(section, "ai") {
        print_header(ctx, "ai");
        check_ai(ctx, proxy);
    }
    if show_section(section, "hooks") {
        print_header(ctx, "hooks");
        check_hooks(ctx, proxy);
    }
    if show_section(section, "mcp") {
        print_header(ctx, "mcp");
        check_mcp(ctx, proxy);
    }
    if show_section(section, "project") {
        print_header(ctx, "project");
        check_project(ctx, proxy, &current_dir);
    }
    if show_section(section, "runtime") || show_section(section, "runtimes") {
        print_header(ctx, "runtimes");
        check_runtimes(ctx, proxy);
    }
    if show_section(section, "performance") || show_section(section, "perf") {
        print_header(ctx, "performance");
        check_performance(ctx, proxy, argv.get(2..).unwrap_or(&[]));
    }
    if show_section(section, "skills") {
        print_header(ctx, "skills");
        check_skills(ctx, proxy, &current_dir);
    }
    if show_section(section, "safety") {
        print_header(ctx, "safety");
        check_safety(ctx, proxy, &current_dir);
    }
    if show_section(section, "dev") || show_section(section, "validate") {
        print_header(ctx, "dev");
        check_dev(ctx, proxy, &current_dir);
    }

    ExitStatus::ExitedWith(0)
}

fn print_help(ctx: &Context) -> ExitStatus {
    let _ = ctx.write_stdout(help_text());
    ExitStatus::ExitedWith(0)
}

fn help_text() -> &'static str {
    concat!(
        "Usage: doctor [config|ai|hooks|mcp|project|runtime|performance|skills|safety|setup|fix|dev|validate] [OPTIONS]\n",
        "\n",
        "Run diagnostics for the current shell setup. Without a section, all checks run.\n",
        "\n",
        "Sections:\n",
        "  config   Check config.lisp and runtime skills directory\n",
        "  ai       Check AI-related environment and defaults\n",
        "  hooks    Inspect AI chat hook configuration without running any hook\n",
        "  mcp      Check configured MCP servers and connection counters\n",
        "  project  Detect project marker files in the current directory\n",
        "  runtime  Check common developer tools in PATH\n",
        "  performance  Show command timing and runtime skill scan state\n",
        "  skills   Show loaded skills and compare repo-local skills with runtime skills\n",
        "  safety   Check AI tool, MCP, direnv, log, and allowlist safety posture\n",
        "  setup    Show first-run setup state and recommended next steps\n",
        "  fix      Create safe missing setup directories/files, then show setup state\n",
        "  dev      Suggest validation commands from changed files\n",
        "  validate Alias for dev\n",
        "\n",
        "Examples:\n",
        "  doctor\n",
        "  doctor ai\n",
        "  doctor project\n",
        "  doctor performance --top 5 --latency --latency-iters 1000\n",
        "  doctor hooks\n",
        "  doctor skills\n",
        "  doctor safety\n",
        "  doctor setup\n",
        "  doctor fix\n",
        "  doctor validate\n",
        "  doctor --json\n",
        "  doctor --help\n",
    )
}

fn is_known_section(value: &str) -> bool {
    matches!(
        value,
        "config"
            | "ai"
            | "hooks"
            | "mcp"
            | "project"
            | "runtime"
            | "runtimes"
            | "performance"
            | "perf"
            | "skills"
            | "safety"
            | "setup"
            | "fix"
            | "dev"
            | "validate"
    )
}

fn show_section(selected: Option<&str>, current: &str) -> bool {
    match selected {
        None => true,
        Some("runtime") if current == "runtimes" => true,
        Some("runtimes") if current == "runtime" => true,
        Some("validate") if current == "dev" => true,
        Some(value) => value == current,
    }
}

fn print_header(ctx: &Context, title: &str) {
    let _ = ctx.write_stdout(&format!("[{title}]"));
}

#[cfg(test)]
mod tests;
