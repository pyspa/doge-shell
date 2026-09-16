use super::*;
use crate::test_support::TestShellProxy;
use dsh_types::command_block::{AiWatchSummary, CommandBlock};
use dsh_types::observed_output::{ObservedOutput, ObservedOutputSnapshot};

fn blocks_proxy(blocks: Vec<CommandBlock>) -> TestShellProxy {
    TestShellProxy {
        current_dir: "/tmp".into(),
        command_blocks: blocks,
        confirm_result: true,
        ai_response: Some("explained".to_string()),
        ..TestShellProxy::default()
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
    let mut proxy = blocks_proxy(export_fixture());
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
    let mut proxy = blocks_proxy(export_fixture());
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
    failed.stderr = "dogesh: command not found: gti".to_string();
    let mut proxy = blocks_proxy(vec![failed]);
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
    let mut proxy = blocks_proxy(vec![block("echo hi", 0, false)]);

    let status = command(
        &ctx,
        vec!["blocks".to_string(), "command".to_string(), "1".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
}

#[test]
fn show_stdout_outputs_block_stdout() {
    let mut proxy = blocks_proxy(vec![block_with_streams(
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
    let mut proxy = blocks_proxy(vec![block_with_streams(
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
    let mut proxy = blocks_proxy(vec![block_with_streams(
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
    let mut proxy = blocks_proxy(vec![block("echo hi", 0, false)]);

    let status = command(
        &ctx,
        vec!["blocks".to_string(), "clear".to_string()],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    assert!(proxy.command_blocks.is_empty());
}

#[test]
fn rerun_requests_normal_shell_eval() {
    let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), true);
    let mut proxy = blocks_proxy(vec![block("echo hi", 0, false)]);

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
    let mut proxy = blocks_proxy(vec![block("blocks rerun 1", 0, false)]);
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
    let mut proxy = blocks_proxy(vec![block("echo hi", 0, true)]);

    let status = command_async(
        &ctx,
        vec!["blocks".to_string(), "explain".to_string(), "1".to_string()],
        &mut proxy,
    )
    .await;

    assert_eq!(status, ExitStatus::ExitedWith(0));
}
