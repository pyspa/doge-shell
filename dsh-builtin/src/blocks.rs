use super::{BuiltinFuture, ShellProxy};
use crate::capability::AiCapability;
use dsh_types::command_block::CommandBlock;
use dsh_types::quick_fix::{DeterministicQuickFixProvider, QuickFix, QuickFixProvider};
use dsh_types::{Context, ExitStatus};
use serde_json::json;

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

/// Resolve an export selection against the newest-first block list and return
/// the chosen blocks in chronological order — a runbook is a procedure, so
/// steps must read in execution order.
fn select_blocks_for_export(
    blocks: &[CommandBlock],
    selection: &ExportSelection,
) -> Result<Vec<CommandBlock>, String> {
    if blocks.is_empty() {
        return Err("no command blocks available".to_string());
    }

    let selected: Vec<CommandBlock> = match selection {
        ExportSelection::Range(start, end) => {
            if *end > blocks.len() {
                return Err(format!(
                    "range end {end} exceeds available blocks ({})",
                    blocks.len()
                ));
            }
            blocks[start - 1..*end].to_vec()
        }
        ExportSelection::Last(count) => blocks.iter().take(*count).cloned().collect(),
        ExportSelection::Ids(ids) => {
            let mut selected = Vec::with_capacity(ids.len());
            for id in ids {
                let Some(block) = blocks.iter().find(|block| block.id == *id) else {
                    return Err(format!("no block with id {id}"));
                };
                selected.push(block.clone());
            }
            // Ids may arrive in any order; sort newest-first like the other
            // arms so the single reversal below yields chronological order.
            selected.sort_by_key(|block| std::cmp::Reverse(block.id));
            selected
        }
    };

    Ok(selected.into_iter().rev().collect())
}

fn export_blocks(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    selection: &ExportSelection,
    output: Option<&str>,
    title: Option<String>,
    descriptions: Option<Vec<String>>,
) -> ExitStatus {
    let blocks = proxy.get_command_blocks();
    let selected = match select_blocks_for_export(&blocks, selection) {
        Ok(selected) => selected,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    };

    write_runbook(ctx, &selected, output, title, descriptions)
}

/// Render the already-selected blocks and write them out.
///
/// Takes the resolved blocks rather than the selection so the AI path cannot
/// re-resolve after its await: a block recorded meanwhile would shift what
/// `--last`/`--range` mean and attach each description to the wrong step.
fn write_runbook(
    ctx: &Context,
    selected: &[CommandBlock],
    output: Option<&str>,
    title: Option<String>,
    descriptions: Option<Vec<String>>,
) -> ExitStatus {
    let options = crate::runbook::RunbookOptions {
        title,
        descriptions,
        ..Default::default()
    };
    let markdown = crate::runbook::render_runbook(selected, &options);

    match output {
        Some(path) => match std::fs::write(path, &markdown) {
            Ok(()) => {
                let _ = ctx.write_stdout(&format!(
                    "Exported {} block(s) to {path}. Replay with `notebook-play {path}`.",
                    selected.len()
                ));
                ExitStatus::ExitedWith(0)
            }
            Err(err) => {
                let _ = ctx.write_stderr(&format!("blocks: failed to write {path}: {err}"));
                ExitStatus::ExitedWith(1)
            }
        },
        None => {
            let _ = ctx.write_stdout(&markdown);
            ExitStatus::ExitedWith(0)
        }
    }
}

async fn export_blocks_ai(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    selection: &ExportSelection,
    output: Option<&str>,
    title: Option<String>,
) -> ExitStatus {
    let blocks = proxy.get_command_blocks();
    let selected = match select_blocks_for_export(&blocks, selection) {
        Ok(selected) => selected,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: {err}"));
            return ExitStatus::ExitedWith(1);
        }
    };

    // One request for the whole runbook; a failed or unparsable response
    // degrades to an export without descriptions instead of failing.
    let steps = selected
        .iter()
        .enumerate()
        .map(|(index, block)| {
            format!(
                "{}. `{}` (exit {})\n{}",
                index + 1,
                block.command,
                block.exit_code,
                truncate_for_ai(
                    if block.stdout.is_empty() {
                        &block.stderr
                    } else {
                        &block.stdout
                    },
                    1000
                )
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let messages = vec![
        json!({
            "role": "system",
            "content": "You annotate shell runbooks. For each numbered step, describe its purpose in one short sentence. Reply with one line per step in the form `N. description`, nothing else. Use the user's language when the commands suggest one."
        }),
        json!({
            "role": "user",
            "content": steps
        }),
    ];

    let descriptions = match proxy.ask(messages).await {
        Ok(response) => {
            let parsed = parse_numbered_descriptions(&response, selected.len());
            if parsed.iter().all(|line| line.is_empty()) {
                let _ = ctx.write_stderr(
                    "blocks: could not parse AI descriptions; exporting without them",
                );
                None
            } else {
                Some(parsed)
            }
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!(
                "blocks: AI description failed ({err}); exporting without descriptions"
            ));
            None
        }
    };

    // The blocks resolved before the await, so the descriptions line up with
    // exactly the steps they were generated for.
    write_runbook(ctx, &selected, output, title, descriptions)
}

/// Parse `N. description` lines into a per-step vector; unmatched steps stay
/// empty and simply render bare.
fn parse_numbered_descriptions(response: &str, steps: usize) -> Vec<String> {
    let mut descriptions = vec![String::new(); steps];
    for line in response.lines() {
        let trimmed = line.trim().trim_start_matches(['-', '*']).trim_start();
        let Some((number, rest)) = trimmed.split_once(['.', ')']) else {
            continue;
        };
        let Ok(step) = number.trim().parse::<usize>() else {
            continue;
        };
        if step == 0 || step > steps {
            continue;
        }
        let text = rest.trim();
        if !text.is_empty() {
            descriptions[step - 1] = text.to_string();
        }
    }
    descriptions
}

fn deterministic_fixes(block: &CommandBlock) -> Vec<QuickFix> {
    let output = if block.stderr.is_empty() {
        block.stdout.as_str()
    } else {
        block.stderr.as_str()
    };
    DeterministicQuickFixProvider.suggest(&block.command, block.exit_code, output)
}

fn fix_block(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
    json_output: bool,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };
    let fixes = deterministic_fixes(&block);
    if json_output {
        let _ = ctx.write_stdout(
            &serde_json::to_string(&json!({
                "block": index,
                "command": block.command,
                "fixes": fixes,
                "source": "deterministic"
            }))
            .unwrap_or_else(|_| "{}".to_string()),
        );
    } else if fixes.is_empty() {
        let _ = ctx.write_stdout("No deterministic fix found. Retry with `blocks fix <N> --ai`.");
    } else {
        let lines = fixes
            .iter()
            .enumerate()
            .map(|(offset, fix)| format!("{}. {}\n   {}", offset + 1, fix.title, fix.replacement))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = ctx.write_stdout(&lines);
    }
    ExitStatus::ExitedWith(0)
}

async fn fix_block_ai(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
    json_output: bool,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };

    let deterministic = deterministic_fixes(&block);
    if !deterministic.is_empty() {
        return fix_block(ctx, proxy, index, json_output);
    }

    let output = if block.stderr.is_empty() {
        block.stdout.as_str()
    } else {
        block.stderr.as_str()
    };
    let messages = vec![
        json!({
            "role": "system",
            "content": "Return one corrected shell command only. Never claim it has been executed."
        }),
        json!({
            "role": "user",
            "content": format!("Command: {}\nExit code: {}\nOutput:\n{}", block.command, block.exit_code, truncate_for_ai(output, 4000))
        }),
    ];
    match proxy.ask(messages).await {
        Ok(replacement) => {
            if json_output {
                let _ = ctx.write_stdout(
                    &serde_json::to_string(&json!({
                        "block": index,
                        "command": block.command,
                        "fixes": [{
                            "id": "ai",
                            "title": "AI suggestion",
                            "replacement": replacement.trim()
                        }],
                        "source": "ai"
                    }))
                    .unwrap_or_else(|_| "{}".to_string()),
                );
            } else {
                let _ = ctx.write_stdout(replacement.trim());
            }
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: AI fix failed: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}

async fn explain_block_async(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    index: usize,
) -> ExitStatus {
    let Some(block) = get_block(proxy, index) else {
        let _ = ctx.write_stderr(&format!("blocks: no block at index {index}"));
        return ExitStatus::ExitedWith(1);
    };

    let output = if block.stdout.is_empty() {
        block.stderr.as_str()
    } else {
        block.stdout.as_str()
    };
    let output = truncate_for_ai(output, 4000);

    let messages = vec![
        json!({
            "role": "system",
            "content": "You are a shell command analyst. Explain this command block concisely, focusing on result, errors, and the next useful action. Respond in the user's language when possible."
        }),
        json!({
            "role": "user",
            "content": format!(
                "Command: `{}`\nExit code: {}\nDuration: {} ms\nOutput:\n```\n{}\n```",
                block.command, block.exit_code, block.duration_ms, output
            )
        }),
    ];

    match proxy.ask(messages).await {
        Ok(response) => {
            let _ = ctx.write_stdout(&response);
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            let _ = ctx.write_stderr(&format!("blocks: AI explanation failed: {err}"));
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

fn truncate_for_ai(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut end = max_bytes;
    while !input.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}...(truncated)", &input[..end])
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
mod tests {
    use super::*;
    use dsh_types::command_block::{AiWatchSummary, CommandBlock};
    use dsh_types::mcp::McpServerConfig;
    use dsh_types::observed_output::{ObservedOutput, ObservedOutputSnapshot};
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct MockShellProxy {
        blocks: Vec<CommandBlock>,
        requested_eval: Vec<String>,
        ai_response: Option<String>,
        request_eval_error: Option<String>,
    }

    impl MockShellProxy {
        fn new(blocks: Vec<CommandBlock>) -> Self {
            Self {
                blocks,
                requested_eval: Vec::new(),
                ai_response: Some("explained".to_string()),
                request_eval_error: None,
            }
        }
    }

    impl ShellProxy for MockShellProxy {
        fn exit_shell(&mut self) {}
        fn get_github_status(&self) -> (usize, usize, usize) {
            (0, 0, 0)
        }
        fn get_git_branch(&self) -> Option<String> {
            None
        }
        fn get_job_count(&self) -> usize {
            0
        }
        fn dispatch(
            &mut self,
            _ctx: &Context,
            _cmd: &str,
            argv: Vec<String>,
        ) -> anyhow::Result<()> {
            self.requested_eval.push(argv.join(" "));
            Ok(())
        }
        fn save_path_history(&mut self, _path: &str) {}
        fn changepwd(&mut self, _path: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn insert_path(&mut self, _index: usize, _path: &str) {}
        fn get_var(&mut self, _key: &str) -> Option<String> {
            None
        }
        fn set_var(&mut self, _key: String, _value: String) {}
        fn set_env_var(&mut self, _key: String, _value: String) {}
        fn unset_env_var(&mut self, _key: &str) {}
        fn get_alias(&mut self, _name: &str) -> Option<String> {
            None
        }
        fn set_alias(&mut self, _name: String, _command: String) {}
        fn list_aliases(&mut self) -> HashMap<String, String> {
            HashMap::new()
        }
        fn add_abbr(&mut self, _name: String, _expansion: String) {}
        fn remove_abbr(&mut self, _name: &str) -> bool {
            false
        }
        fn list_abbrs(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn get_abbr(&self, _name: &str) -> Option<String> {
            None
        }
        fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
            Vec::new()
        }
        fn list_execute_allowlist(&mut self) -> Vec<String> {
            Vec::new()
        }
        fn list_exported_vars(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn export_var(&mut self, _key: &str) -> bool {
            false
        }
        fn set_and_export_var(&mut self, _key: String, _value: String) {}
        fn get_current_dir(&self) -> anyhow::Result<PathBuf> {
            Ok(PathBuf::from("/tmp"))
        }
        fn get_lisp_var(&self, _key: &str) -> Option<String> {
            None
        }
        fn confirm_action(&mut self, _message: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn ask_ai_async<'a>(
            &'a mut self,
            _messages: Vec<serde_json::Value>,
        ) -> crate::ProxyFuture<'a, String> {
            let response = self.ai_response.clone();
            Box::pin(async move { response.ok_or_else(|| anyhow::anyhow!("no ai")) })
        }
        fn get_command_blocks(&self) -> Vec<CommandBlock> {
            self.blocks.clone()
        }
        fn clear_command_blocks(&mut self) -> usize {
            let removed = self.blocks.len();
            self.blocks.clear();
            removed
        }
        fn request_eval_command(&mut self, command: String) -> anyhow::Result<()> {
            if let Some(error) = &self.request_eval_error {
                return Err(anyhow::anyhow!(error.clone()));
            }
            self.requested_eval.push(command);
            Ok(())
        }
    }

    fn block(command: &str, exit_code: i32, watched: bool) -> CommandBlock {
        let summary =
            watched.then(|| AiWatchSummary::new(None, "completed".into(), "watch summary".into()));
        let mut block = CommandBlock::new(command.into(), None, exit_code, 42, &[], summary);
        block.stdout = "hello".to_string();
        block
    }

    fn block_with_streams(command: &str, stdout: &str, stderr: &str) -> CommandBlock {
        let mut block = block(command, 0, false);
        block.stdout = stdout.to_string();
        block.stderr = stderr.to_string();
        block
    }

    fn run_with_observer(
        argv: Vec<String>,
        proxy: &mut dyn ShellProxy,
    ) -> (ExitStatus, ObservedOutputSnapshot) {
        let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let observer = ObservedOutput::shared(4096);
        ctx.output_observer = Some(observer.clone());

        let status = command(&ctx, argv, proxy);
        let snapshot = observer.lock().unwrap().snapshot();
        (status, snapshot)
    }

    #[test]
    fn parse_tui_subcommand() {
        for name in ["tui", "browse"] {
            assert_eq!(
                parse_options(&[name.to_string()]).unwrap(),
                BlocksOptions {
                    mode: BlocksMode::Tui
                },
                "`blocks {name}` should open the browser"
            );
        }
    }

    #[test]
    fn parse_tui_rejects_extra_arguments() {
        assert!(parse_options(&["tui".to_string(), "3".to_string()]).is_err());
    }

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn parse_export_selections() {
        assert_eq!(
            parse_options(&args(&["export"])).unwrap().mode,
            BlocksMode::Export {
                selection: ExportSelection::Last(1),
                output: None,
                ai: false,
                title: None,
            }
        );
        assert_eq!(
            parse_options(&args(&[
                "export", "--range", "2..5", "-o", "rb.md", "--title", "Deploy", "--ai"
            ]))
            .unwrap()
            .mode,
            BlocksMode::Export {
                selection: ExportSelection::Range(2, 5),
                output: Some("rb.md".to_string()),
                ai: true,
                title: Some("Deploy".to_string()),
            }
        );
        assert_eq!(
            parse_options(&args(&["export", "--ids", "3, 7,9"]))
                .unwrap()
                .mode,
            BlocksMode::Export {
                selection: ExportSelection::Ids(vec![3, 7, 9]),
                output: None,
                ai: false,
                title: None,
            }
        );
    }

    #[test]
    fn parse_export_rejects_bad_input() {
        assert!(parse_options(&args(&["export", "--range", "5..2"])).is_err());
        assert!(parse_options(&args(&["export", "--range", "abc"])).is_err());
        assert!(parse_options(&args(&["export", "--ids", ""])).is_err());
        assert!(parse_options(&args(&["export", "--ids", "x"])).is_err());
        assert!(parse_options(&args(&["export", "--range", "1..2", "--ids", "1"])).is_err());
        assert!(parse_options(&args(&["export", "-o"])).is_err());
        assert!(parse_options(&args(&["export", "--bogus"])).is_err());
    }

    fn export_fixture() -> Vec<CommandBlock> {
        // Newest-first, as `get_command_blocks` returns them.
        let mut newest = block("cargo test", 0, false);
        newest.id = 3;
        let mut middle = block("cargo build", 0, false);
        middle.id = 2;
        let mut oldest = block("git pull", 0, false);
        oldest.id = 1;
        vec![newest, middle, oldest]
    }

    #[test]
    fn export_selection_is_chronological() {
        let blocks = export_fixture();

        let range = select_blocks_for_export(&blocks, &ExportSelection::Range(1, 2)).unwrap();
        assert_eq!(
            range.iter().map(|b| b.command.as_str()).collect::<Vec<_>>(),
            vec!["cargo build", "cargo test"]
        );

        let last = select_blocks_for_export(&blocks, &ExportSelection::Last(2)).unwrap();
        assert_eq!(
            last.iter().map(|b| b.command.as_str()).collect::<Vec<_>>(),
            vec!["cargo build", "cargo test"]
        );

        // Ids in any order come out chronological.
        let ids = select_blocks_for_export(&blocks, &ExportSelection::Ids(vec![3, 1])).unwrap();
        assert_eq!(
            ids.iter().map(|b| b.command.as_str()).collect::<Vec<_>>(),
            vec!["git pull", "cargo test"]
        );

        assert!(select_blocks_for_export(&blocks, &ExportSelection::Range(1, 9)).is_err());
        assert!(select_blocks_for_export(&blocks, &ExportSelection::Ids(vec![42])).is_err());
        assert!(select_blocks_for_export(&[], &ExportSelection::Last(1)).is_err());
    }

    #[test]
    fn export_writes_a_runbook_notebook_play_can_load() {
        let mut proxy = MockShellProxy::new(export_fixture());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runbook.md");

        let (status, snapshot) = run_with_observer(
            args(&[
                "blocks",
                "export",
                "--range",
                "1..3",
                "-o",
                path.to_str().unwrap(),
            ]),
            &mut proxy,
        );
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(snapshot.stdout.contains("Exported 3 block(s)"));

        // Round-trip: notebook-play must see exactly the commands, in
        // chronological order, and nothing else as executable.
        let notebook = dsh_types::notebook::Notebook::load_from_file(&path).unwrap();
        let executable: Vec<String> = notebook
            .blocks
            .iter()
            .filter(|block| {
                matches!(&block.kind, dsh_types::notebook::BlockKind::Code(lang)
                    if lang == "sh" || lang == "bash" || lang.is_empty())
            })
            .map(|block| block.raw_content())
            .collect();
        assert_eq!(executable, vec!["git pull", "cargo build", "cargo test"]);
    }

    #[test]
    fn export_without_output_prints_markdown() {
        let mut proxy = MockShellProxy::new(export_fixture());
        let (status, snapshot) =
            run_with_observer(args(&["blocks", "export", "--last", "1"]), &mut proxy);
        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(snapshot.stdout.contains("## Step 1: cargo test"));
        assert!(snapshot.stdout.contains("```sh\ncargo test\n```"));
    }

    #[test]
    fn numbered_descriptions_parse_and_tolerate_noise() {
        let parsed = parse_numbered_descriptions(
            "1. Pull the latest changes.\nnoise\n3) Run the tests.\n9. out of range",
            3,
        );
        assert_eq!(
            parsed,
            vec![
                "Pull the latest changes.".to_string(),
                String::new(),
                "Run the tests.".to_string(),
            ]
        );
        assert!(
            parse_numbered_descriptions("no numbers here", 2)
                .iter()
                .all(String::is_empty)
        );
    }

    #[test]
    fn parse_default_lists_blocks() {
        assert_eq!(
            parse_options(&[]).unwrap(),
            BlocksOptions {
                mode: BlocksMode::List {
                    limit: 20,
                    failed: false,
                    watched: false,
                    json: false,
                    scope: BlockScope::Session,
                }
            }
        );
    }

    #[test]
    fn parse_list_filters() {
        let args = vec![
            "list".to_string(),
            "--limit".to_string(),
            "5".to_string(),
            "--failed".to_string(),
            "--watched".to_string(),
        ];
        assert_eq!(
            parse_options(&args).unwrap(),
            BlocksOptions {
                mode: BlocksMode::List {
                    limit: 5,
                    failed: true,
                    watched: true,
                    json: false,
                    scope: BlockScope::Session,
                }
            }
        );
    }

    #[test]
    fn parse_show_stdout() {
        let args = vec!["show".to_string(), "2".to_string(), "--stdout".to_string()];
        assert_eq!(
            parse_options(&args).unwrap(),
            BlocksOptions {
                mode: BlocksMode::Show {
                    index: 2,
                    output: OutputSelection::Stdout
                }
            }
        );
    }

    #[test]
    fn parse_fix_supports_json_and_ai_flags() {
        assert_eq!(
            parse_options(&[
                "fix".to_string(),
                "2".to_string(),
                "--json".to_string(),
                "--ai".to_string(),
            ])
            .unwrap(),
            BlocksOptions {
                mode: BlocksMode::Fix {
                    index: 2,
                    json: true,
                    ai: true,
                }
            }
        );
    }

    #[test]
    fn fix_json_uses_deterministic_engine_without_running_command() {
        let mut failed = block("gti status", 127, false);
        failed.stderr = "dsh: command not found: gti".to_string();
        let mut proxy = MockShellProxy::new(vec![failed]);
        let (status, snapshot) = run_with_observer(
            vec![
                "blocks".to_string(),
                "fix".to_string(),
                "1".to_string(),
                "--json".to_string(),
            ],
            &mut proxy,
        );
        assert_eq!(status, ExitStatus::ExitedWith(0));
        let value: serde_json::Value = serde_json::from_str(snapshot.stdout.trim()).unwrap();
        assert_eq!(value["fixes"][0]["replacement"], "git status");
        assert!(proxy.requested_eval.is_empty());
    }

    #[test]
    fn command_prints_block_command() {
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let mut proxy = MockShellProxy::new(vec![block("echo hi", 0, false)]);

        let status = command(
            &ctx,
            vec!["blocks".to_string(), "command".to_string(), "1".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
    }

    #[test]
    fn show_stdout_outputs_block_stdout() {
        let mut proxy = MockShellProxy::new(vec![block_with_streams(
            "echo hi",
            "stdout text",
            "stderr text",
        )]);

        let (status, snapshot) = run_with_observer(
            vec![
                "blocks".to_string(),
                "show".to_string(),
                "1".to_string(),
                "--stdout".to_string(),
            ],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(snapshot.stdout, "stdout text\n");
        assert_eq!(snapshot.stderr, "");
    }

    #[test]
    fn show_stderr_outputs_block_stderr_to_stdout() {
        let mut proxy = MockShellProxy::new(vec![block_with_streams(
            "echo hi",
            "stdout text",
            "stderr text",
        )]);

        let (status, snapshot) = run_with_observer(
            vec![
                "blocks".to_string(),
                "show".to_string(),
                "1".to_string(),
                "--stderr".to_string(),
            ],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(snapshot.stdout, "stderr text\n");
        assert_eq!(snapshot.stderr, "");
    }

    #[test]
    fn show_all_outputs_metadata_and_both_streams() {
        let mut proxy = MockShellProxy::new(vec![block_with_streams(
            "echo hi",
            "stdout text",
            "stderr text",
        )]);

        let (status, snapshot) = run_with_observer(
            vec![
                "blocks".to_string(),
                "show".to_string(),
                "1".to_string(),
                "--all".to_string(),
            ],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(snapshot.stdout.contains("Command: echo hi"));
        assert!(snapshot.stdout.contains("--- STDOUT ---"));
        assert!(snapshot.stdout.contains("stdout text"));
        assert!(snapshot.stdout.contains("--- STDERR ---"));
        assert!(snapshot.stdout.contains("stderr text"));
        assert_eq!(snapshot.stderr, "");
    }

    #[test]
    fn clear_removes_blocks() {
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let mut proxy = MockShellProxy::new(vec![block("echo hi", 0, false)]);

        let status = command(
            &ctx,
            vec!["blocks".to_string(), "clear".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert!(proxy.blocks.is_empty());
    }

    #[test]
    fn rerun_requests_normal_shell_eval() {
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let mut proxy = MockShellProxy::new(vec![block("echo hi", 0, false)]);

        let status = command(
            &ctx,
            vec!["blocks".to_string(), "rerun".to_string(), "1".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
        assert_eq!(proxy.requested_eval, vec!["echo hi".to_string()]);
    }

    #[test]
    fn rerun_reports_rejected_nested_eval_request() {
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let mut proxy = MockShellProxy::new(vec![block("blocks rerun 1", 0, false)]);
        proxy.request_eval_error = Some("nested command block rerun is not allowed".to_string());

        let status = command(
            &ctx,
            vec!["blocks".to_string(), "rerun".to_string(), "1".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(1));
        assert!(proxy.requested_eval.is_empty());
    }

    #[tokio::test]
    async fn explain_uses_ai() {
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let mut proxy = MockShellProxy::new(vec![block("echo hi", 0, true)]);

        let status = command_async(
            &ctx,
            vec!["blocks".to_string(), "explain".to_string(), "1".to_string()],
            &mut proxy,
        )
        .await;

        assert_eq!(status, ExitStatus::ExitedWith(0));
    }
}
