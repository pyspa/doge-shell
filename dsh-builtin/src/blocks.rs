use super::{BuiltinFuture, ShellProxy};
use dsh_types::command_block::CommandBlock;
use dsh_types::{Context, ExitStatus};
use serde_json::json;

mod ai;
mod export;
use ai::{explain_block_async, fix_block, fix_block_ai};
use export::{export_blocks, export_blocks_ai};
#[cfg(test)]
use export::{parse_numbered_descriptions, select_blocks_for_export};

pub fn description() -> &'static str {
    "List and inspect session command blocks"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let options = match parse_options(&argv[1..]) {
        Ok(options) => options,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: {err}"));
            let _ = ctx.write_stderr(help_text());
            return ExitStatus::ExitedWith(1);
        }
    };

    match options.mode {
        BlocksMode::List {
            limit,
            failed,
            watched,
            json,
            scope,
        } => list_blocks(ctx, proxy, limit, failed, watched, json, scope),
        BlocksMode::Show { index, output } => show_block(ctx, proxy, index, output),
        BlocksMode::Export {
            selection,
            output,
            ai,
            title,
        } => {
            if ai {
                let _ = ctx.write_stderr("blocks: --ai requires foreground async execution");
                ExitStatus::ExitedWith(1)
            } else {
                export_blocks(ctx, proxy, &selection, output.as_deref(), title, None)
            }
        }
        BlocksMode::Command(index) => print_command(ctx, proxy, index),
        BlocksMode::Rerun(index) => rerun_block(ctx, proxy, index),
        BlocksMode::Fix { index, json, ai } => {
            if ai {
                let _ = ctx.write_stderr("blocks: --ai requires foreground async execution");
                ExitStatus::ExitedWith(1)
            } else {
                fix_block(ctx, proxy, index, json)
            }
        }
        BlocksMode::Explain(_) => {
            let _ = ctx.write_stderr("blocks: AI explanation requires foreground async execution");
            ExitStatus::ExitedWith(1)
        }
        BlocksMode::Clear => clear_blocks(ctx, proxy),
        BlocksMode::Tui => open_tui(ctx, proxy),
        BlocksMode::Help => {
            let _ = ctx.write_stdout(help_text());
            ExitStatus::ExitedWith(0)
        }
    }
}

pub fn command_async<'a>(
    ctx: &'a Context,
    argv: Vec<String>,
    proxy: &'a mut dyn ShellProxy,
) -> BuiltinFuture<'a> {
    Box::pin(async move {
        let options = match parse_options(&argv[1..]) {
            Ok(options) => options,
            Err(err) => {
                let _ = ctx.write_stderr(&format!("blocks: {err}"));
                let _ = ctx.write_stderr(help_text());
                return ExitStatus::ExitedWith(1);
            }
        };

        match options.mode {
            BlocksMode::List {
                limit,
                failed,
                watched,
                json,
                scope,
            } => list_blocks(ctx, proxy, limit, failed, watched, json, scope),
            BlocksMode::Show { index, output } => show_block(ctx, proxy, index, output),
            BlocksMode::Export {
                selection,
                output,
                ai,
                title,
            } => {
                if ai {
                    export_blocks_ai(ctx, proxy, &selection, output.as_deref(), title).await
                } else {
                    export_blocks(ctx, proxy, &selection, output.as_deref(), title, None)
                }
            }
            BlocksMode::Command(index) => print_command(ctx, proxy, index),
            BlocksMode::Rerun(index) => rerun_block(ctx, proxy, index),
            BlocksMode::Fix { index, json, ai } => {
                if ai {
                    fix_block_ai(ctx, proxy, index, json).await
                } else {
                    fix_block(ctx, proxy, index, json)
                }
            }
            BlocksMode::Explain(index) => explain_block_async(ctx, proxy, index).await,
            BlocksMode::Clear => clear_blocks(ctx, proxy),
            BlocksMode::Tui => open_tui(ctx, proxy),
            BlocksMode::Help => {
                let _ = ctx.write_stdout(help_text());
                ExitStatus::ExitedWith(0)
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSelection {
    Stdout,
    Stderr,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockScope {
    Session,
    Persistent,
}

/// Which blocks `blocks export` writes. Display indices are 1-based and
/// newest-first (same as `blocks list`); ids are the stable `CommandBlock.id`
/// values, which the TUI uses because display indices shift with every
/// command.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExportSelection {
    /// Display indices N..M inclusive.
    Range(usize, usize),
    /// Stable block ids.
    Ids(Vec<u64>),
    /// The most recent N blocks.
    Last(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BlocksMode {
    List {
        limit: usize,
        failed: bool,
        watched: bool,
        json: bool,
        scope: BlockScope,
    },
    Export {
        selection: ExportSelection,
        output: Option<String>,
        ai: bool,
        title: Option<String>,
    },
    Show {
        index: usize,
        output: OutputSelection,
    },
    Command(usize),
    Rerun(usize),
    Fix {
        index: usize,
        json: bool,
        ai: bool,
    },
    Explain(usize),
    Clear,
    /// Full-screen browser. Implemented in the `dsh` crate and reached through
    /// the proxy, because it needs clipboard and terminal code this crate
    /// cannot depend on.
    Tui,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlocksOptions {
    mode: BlocksMode,
}

fn parse_options(args: &[String]) -> Result<BlocksOptions, String> {
    if args.is_empty() {
        return Ok(BlocksOptions {
            mode: BlocksMode::List {
                limit: 20,
                failed: false,
                watched: false,
                json: false,
                scope: BlockScope::Session,
            },
        });
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(BlocksOptions {
            mode: BlocksMode::Help,
        }),
        "list" | "-l" | "--list" => parse_list_options(&args[1..]),
        "--scope" | "--json" => parse_list_options(args),
        "show" => parse_show_options(&args[1..]),
        "export" => parse_export_options(&args[1..]),
        "command" => parse_index_mode(&args[1..], BlocksMode::Command),
        "rerun" => parse_index_mode(&args[1..], BlocksMode::Rerun),
        "fix" => parse_fix_options(&args[1..]),
        "explain" => parse_index_mode(&args[1..], BlocksMode::Explain),
        "tui" | "browse" => {
            if args.len() > 1 {
                return Err("tui does not accept extra arguments".to_string());
            }
            Ok(BlocksOptions {
                mode: BlocksMode::Tui,
            })
        }
        "clear" | "-c" | "--clear" => {
            if args.len() > 1 {
                return Err("clear does not accept extra arguments".to_string());
            }
            Ok(BlocksOptions {
                mode: BlocksMode::Clear,
            })
        }
        other if other.starts_with('-') => Err(format!("unknown option: {other}")),
        index => {
            let index = parse_positive_usize(index, "index")?;
            Ok(BlocksOptions {
                mode: BlocksMode::Show {
                    index,
                    output: OutputSelection::All,
                },
            })
        }
    }
}

fn parse_export_options(args: &[String]) -> Result<BlocksOptions, String> {
    let mut selection: Option<ExportSelection> = None;
    let mut output = None;
    let mut ai = false;
    let mut title = None;

    let mut set_selection = |value: ExportSelection| -> Result<(), String> {
        if selection.replace(value).is_some() {
            return Err("export accepts only one of --range, --ids, --last".to_string());
        }
        Ok(())
    };

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--range" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--range requires N..M".to_string());
                };
                set_selection(parse_range(value)?)?;
            }
            "--ids" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--ids requires a comma-separated id list".to_string());
                };
                set_selection(parse_ids(value)?)?;
            }
            "--last" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--last requires a number".to_string());
                };
                set_selection(ExportSelection::Last(parse_positive_usize(value, "last")?))?;
            }
            "-o" | "--output" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--output requires a file path".to_string());
                };
                output = Some(value.clone());
            }
            "--title" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--title requires a value".to_string());
                };
                title = Some(value.clone());
            }
            "--ai" => ai = true,
            value => return Err(format!("unknown export option: {value}")),
        }
        index += 1;
    }

    Ok(BlocksOptions {
        mode: BlocksMode::Export {
            selection: selection.unwrap_or(ExportSelection::Last(1)),
            output,
            ai,
            title,
        },
    })
}

fn parse_range(value: &str) -> Result<ExportSelection, String> {
    let Some((start, end)) = value.split_once("..") else {
        return Err("--range requires the form N..M".to_string());
    };
    let start = parse_positive_usize(start, "range start")?;
    let end = parse_positive_usize(end, "range end")?;
    if start > end {
        return Err("range start must not exceed range end".to_string());
    }
    Ok(ExportSelection::Range(start, end))
}

fn parse_ids(value: &str) -> Result<ExportSelection, String> {
    let ids = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| format!("invalid block id: {part}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Err("--ids requires at least one id".to_string());
    }
    Ok(ExportSelection::Ids(ids))
}

fn parse_fix_options(args: &[String]) -> Result<BlocksOptions, String> {
    let mut index_value = None;
    let mut json = false;
    let mut ai = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--ai" => ai = true,
            value if value.starts_with('-') => return Err(format!("unknown fix option: {value}")),
            value => {
                if index_value
                    .replace(parse_positive_usize(value, "index")?)
                    .is_some()
                {
                    return Err("fix accepts only one index".to_string());
                }
            }
        }
    }
    let Some(index) = index_value else {
        return Err("fix requires an index".to_string());
    };
    Ok(BlocksOptions {
        mode: BlocksMode::Fix { index, json, ai },
    })
}

fn parse_list_options(args: &[String]) -> Result<BlocksOptions, String> {
    let mut limit = 20;
    let mut failed = false;
    let mut watched = false;
    let mut json = false;
    let mut scope = BlockScope::Session;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-n" | "--limit" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--limit requires a number".to_string());
                };
                limit = parse_positive_usize(value, "limit")?;
            }
            value if value.starts_with("--limit=") => {
                limit = parse_positive_usize(value.trim_start_matches("--limit="), "limit")?;
            }
            "--failed" => failed = true,
            "--watched" => watched = true,
            "--json" => json = true,
            "--scope" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--scope requires session or persistent".to_string());
                };
                scope = match value.as_str() {
                    "session" => BlockScope::Session,
                    "persistent" => BlockScope::Persistent,
                    _ => return Err("--scope requires session or persistent".to_string()),
                };
            }
            value => return Err(format!("unknown list option: {value}")),
        }
        index += 1;
    }

    Ok(BlocksOptions {
        mode: BlocksMode::List {
            limit,
            failed,
            watched,
            json,
            scope,
        },
    })
}

fn parse_show_options(args: &[String]) -> Result<BlocksOptions, String> {
    let mut index_value = None;
    let mut output = OutputSelection::All;

    for arg in args {
        match arg.as_str() {
            "--stdout" => output = OutputSelection::Stdout,
            "--stderr" => output = OutputSelection::Stderr,
            "--all" => output = OutputSelection::All,
            value if value.starts_with('-') => return Err(format!("unknown show option: {value}")),
            value => {
                if index_value
                    .replace(parse_positive_usize(value, "index")?)
                    .is_some()
                {
                    return Err("show accepts only one index".to_string());
                }
            }
        }
    }

    let Some(index) = index_value else {
        return Err("show requires an index".to_string());
    };

    Ok(BlocksOptions {
        mode: BlocksMode::Show { index, output },
    })
}

fn parse_index_mode<F>(args: &[String], build: F) -> Result<BlocksOptions, String>
where
    F: Fn(usize) -> BlocksMode,
{
    if args.len() != 1 {
        return Err("expected exactly one index".to_string());
    }
    let index = parse_positive_usize(&args[0], "index")?;
    Ok(BlocksOptions { mode: build(index) })
}

fn parse_positive_usize(value: &str, label: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{label} must be a number"))?;
    if parsed == 0 {
        return Err(format!("{label} must be 1 or greater"));
    }
    Ok(parsed)
}

fn list_blocks(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    limit: usize,
    failed: bool,
    watched: bool,
    json_output: bool,
    scope: BlockScope,
) -> ExitStatus {
    if scope == BlockScope::Persistent {
        use crate::CoreShellAction;
        use crate::capability::ExecutionCapability;
        return match proxy.dispatch_core(
            ctx,
            CoreShellAction::BlocksPersistent,
            vec![
                "blocks-persistent".to_string(),
                limit.to_string(),
                failed.to_string(),
                json_output.to_string(),
            ],
        ) {
            Ok(()) => ExitStatus::ExitedWith(0),
            Err(err) => {
                let _ = ctx.write_stderr(&format!("blocks: {err}"));
                ExitStatus::ExitedWith(1)
            }
        };
    }
    let blocks = proxy.get_command_blocks();
    if blocks.is_empty() {
        let _ = ctx.write_stdout(if json_output {
            "[]"
        } else {
            "No command blocks available."
        });
        return ExitStatus::ExitedWith(0);
    }

    if json_output {
        let rows = blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| !failed || block.exit_code != 0)
            .filter(|(_, block)| !watched || block.watched)
            .take(limit)
            .map(|(offset, block)| {
                json!({
                    "index": offset + 1,
                    "id": block.id,
                    "command": block.command,
                    "cwd": block.cwd,
                    "exit_code": block.exit_code,
                    "duration_ms": block.duration_ms,
                    "watched": block.watched,
                    "stdout": block.stdout,
                    "stderr": block.stderr
                })
            })
            .collect::<Vec<_>>();
        let _ =
            ctx.write_stdout(&serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string()));
        return ExitStatus::ExitedWith(0);
    }

    let mut lines = vec![
        "Command Blocks:".to_string(),
        "Index  Exit  Time(ms)  Watch  Command / Preview".to_string(),
        "-----  ----  --------  -----  -----------------".to_string(),
    ];

    for (offset, block) in blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| !failed || block.exit_code != 0)
        .filter(|(_, block)| !watched || block.watched)
        .take(limit)
    {
        let preview = block.output_preview(72);
        let suffix = if preview.is_empty() {
            String::new()
        } else {
            format!(" -- {preview}")
        };
        lines.push(format!(
            "{:>5}  {:>4}  {:>8}  {:>5}  {}{}",
            offset + 1,
            block.exit_code,
            block.duration_ms,
            if block.watched { "yes" } else { "no" },
            block.command,
            suffix
        ));
    }

    let _ = ctx.write_stdout(&lines.join("\n"));
    ExitStatus::ExitedWith(0)
}

fn show_block(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
    output: OutputSelection,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };

    match output {
        OutputSelection::Stdout => {
            let _ = ctx.write_stdout(&block.stdout);
        }
        OutputSelection::Stderr => {
            let _ = ctx.write_stdout(&block.stderr);
        }
        OutputSelection::All => {
            let _ = ctx.write_stdout(&format_block(index, &block));
        }
    }
    ExitStatus::ExitedWith(0)
}

fn print_command(ctx: &Context, proxy: &mut dyn ShellProxy, index: usize) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };
    let _ = ctx.write_stdout(&block.command);
    ExitStatus::ExitedWith(0)
}

fn rerun_block(ctx: &Context, proxy: &mut dyn ShellProxy, index: usize) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };

    let prompt = format!("Rerun block {index}: `{}`?", block.command);
    match proxy.confirm_action(&prompt) {
        Ok(true) => {}
        Ok(false) => return ExitStatus::ExitedWith(130),
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: confirmation failed: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    }

    match proxy.request_eval_command(block.command) {
        Ok(()) => ExitStatus::ExitedWith(0),
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: rerun failed: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Hand off to the full-screen browser in the `dsh` crate.
///
/// This crate cannot depend on `dsh`, so the real implementation is registered
/// in the builtin registry and reached through `dispatch`; falls back to the
/// plain list when there is no terminal to draw on.
fn open_tui(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    use crate::CoreShellAction;
    use crate::capability::ExecutionCapability;
    use std::io::IsTerminal;

    if !std::io::stdout().is_terminal() {
        return list_blocks(ctx, proxy, 20, false, false, false, BlockScope::Session);
    }

    match proxy.dispatch_core(
        ctx,
        CoreShellAction::BlocksTui,
        vec!["blocks-tui".to_string()],
    ) {
        Ok(()) => ExitStatus::ExitedWith(0),
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: {err}"));
            // A terminal too small for the browser still deserves the list.
            list_blocks(ctx, proxy, 20, false, false, false, BlockScope::Session)
        }
    }
}

fn clear_blocks(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let removed = proxy.clear_command_blocks();
    let _ = ctx.write_stdout(&format!("Cleared {removed} command blocks."));
    ExitStatus::ExitedWith(0)
}

fn get_block(proxy: &mut dyn ShellProxy, index: usize) -> Option<CommandBlock> {
    proxy.get_command_blocks().get(index - 1).cloned()
}

fn format_block(index: usize, block: &CommandBlock) -> String {
    let mut lines = vec![
        format!("Block {index} (id {})", block.id),
        format!("Command: {}", block.command),
        format!("Exit: {}", block.exit_code),
        format!("Duration: {} ms", block.duration_ms),
    ];

    if let Some(cwd) = &block.cwd {
        lines.push(format!("Cwd: {cwd}"));
    }
    if !block.output_entry_ids.is_empty() {
        lines.push(format!("Output IDs: {:?}", block.output_entry_ids));
    }
    if !block.stdout.is_empty() {
        lines.push("--- STDOUT ---".to_string());
        lines.push(block.stdout.clone());
    }
    if !block.stderr.is_empty() {
        lines.push("--- STDERR ---".to_string());
        lines.push(block.stderr.clone());
    }
    if let Some(summary) = &block.watch_summary {
        lines.push("--- AI WATCH ---".to_string());
        lines.push(format!("Status: {}", summary.status));
        if let Some(goal) = &summary.goal {
            lines.push(format!("Goal: {goal}"));
        }
        if let Some(response) = &summary.raw_response {
            lines.push(response.clone());
        }
    }

    lines.join("\n")
}

fn help_text() -> &'static str {
    concat!(
        "Usage: blocks [COMMAND]\n",
        "\n",
        "List and inspect session command blocks.\n",
        "\n",
        "Commands:\n",
        "  list [--limit N] [--failed] [--watched] [--json] [--scope session|persistent]\n",
        "  show <N> [--stdout|--stderr|--all]        Show a command block\n",
        "  export [--range N..M|--ids A,B|--last N] [-o FILE] [--title T] [--ai]\n",
        "                                            Export blocks as a Markdown runbook\n",
        "                                            (replayable with notebook-play; --ai adds step notes)\n",
        "  command <N>                               Print the command only\n",
        "  rerun <N>                                 Rerun a command block\n",
        "  fix <N> [--json] [--ai]                  Suggest a fix without running it\n",
        "  explain <N>                               Ask AI to explain a block\n",
        "  tui                                       Browse blocks full-screen (also Ctrl-O)\n",
        "  clear                                     Clear command blocks\n",
        "  help                                      Show this help\n",
        "\n",
        "Examples:\n",
        "  blocks\n",
        "  blocks list --failed\n",
        "  blocks show 2 --stderr\n",
        "  blocks command 1\n",
        "  blocks export --range 1..5 -o runbook.md\n",
        "  blocks tui\n",
    )
}

#[cfg(test)]
mod tests;
