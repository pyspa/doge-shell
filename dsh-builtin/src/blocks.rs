use super::ShellProxy;
use dsh_types::command_block::CommandBlock;
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
        } => list_blocks(ctx, proxy, limit, failed, watched),
        BlocksMode::Show { index, output } => show_block(ctx, proxy, index, output),
        BlocksMode::Command(index) => print_command(ctx, proxy, index),
        BlocksMode::Rerun(index) => rerun_block(ctx, proxy, index),
        BlocksMode::Explain(index) => explain_block(ctx, proxy, index),
        BlocksMode::Clear => clear_blocks(ctx, proxy),
        BlocksMode::Help => {
            let _ = ctx.write_stdout(help_text());
            ExitStatus::ExitedWith(0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSelection {
    Stdout,
    Stderr,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BlocksMode {
    List {
        limit: usize,
        failed: bool,
        watched: bool,
    },
    Show {
        index: usize,
        output: OutputSelection,
    },
    Command(usize),
    Rerun(usize),
    Explain(usize),
    Clear,
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
            },
        });
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(BlocksOptions {
            mode: BlocksMode::Help,
        }),
        "list" | "-l" | "--list" => parse_list_options(&args[1..]),
        "show" => parse_show_options(&args[1..]),
        "command" => parse_index_mode(&args[1..], BlocksMode::Command),
        "rerun" => parse_index_mode(&args[1..], BlocksMode::Rerun),
        "explain" => parse_index_mode(&args[1..], BlocksMode::Explain),
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

fn parse_list_options(args: &[String]) -> Result<BlocksOptions, String> {
    let mut limit = 20;
    let mut failed = false;
    let mut watched = false;
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
            value => return Err(format!("unknown list option: {value}")),
        }
        index += 1;
    }

    Ok(BlocksOptions {
        mode: BlocksMode::List {
            limit,
            failed,
            watched,
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
) -> ExitStatus {
    let blocks = proxy.get_command_blocks();
    if blocks.is_empty() {
        let _ = ctx.write_stdout("No command blocks available.");
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

fn explain_block(ctx: &Context, proxy: &mut dyn ShellProxy, index: usize) -> ExitStatus {
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

    match proxy.ask_ai(messages) {
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
        "  list [--limit N] [--failed] [--watched]  List command blocks\n",
        "  show <N> [--stdout|--stderr|--all]        Show a command block\n",
        "  command <N>                               Print the command only\n",
        "  rerun <N>                                 Rerun a command block\n",
        "  explain <N>                               Ask AI to explain a block\n",
        "  clear                                     Clear command blocks\n",
        "  help                                      Show this help\n",
        "\n",
        "Examples:\n",
        "  blocks\n",
        "  blocks list --failed\n",
        "  blocks show 2 --stderr\n",
        "  blocks command 1\n",
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
        fn ask_ai(&mut self, _messages: Vec<serde_json::Value>) -> anyhow::Result<String> {
            self.ai_response
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no ai"))
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
    fn parse_default_lists_blocks() {
        assert_eq!(
            parse_options(&[]).unwrap(),
            BlocksOptions {
                mode: BlocksMode::List {
                    limit: 20,
                    failed: false,
                    watched: false
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
                    watched: true
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

    #[test]
    fn explain_uses_ai() {
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
        let mut proxy = MockShellProxy::new(vec![block("echo hi", 0, true)]);

        let status = command(
            &ctx,
            vec!["blocks".to_string(), "explain".to_string(), "1".to_string()],
            &mut proxy,
        );

        assert_eq!(status, ExitStatus::ExitedWith(0));
    }
}
