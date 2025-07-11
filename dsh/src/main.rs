use crate::environment::Environment;
use crate::repl::Repl;
use crate::shell::Shell;
use anyhow::Result;
use clap::Parser;
use dsh_types::Context;
use std::process::ExitCode;
use tracing::debug;

/// 正常終了を表すカスタムエラー型
#[derive(Debug)]
pub enum ShellExit {
    Normal,
    CtrlC,
    ExitCommand,
}

impl std::fmt::Display for ShellExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellExit::Normal => write!(f, "Normal exit"),
            ShellExit::CtrlC => write!(f, "Exit by Ctrl+C"),
            ShellExit::ExitCommand => write!(f, "Exit by exit command"),
        }
    }
}

impl std::error::Error for ShellExit {}

/// Display error in a user-friendly format without stack traces
fn display_user_error(err: &anyhow::Error) {
    let error_msg = err.to_string();

    // Check if it's a command not found error
    if error_msg.contains("unknown command:") {
        if let Some(cmd_start) = error_msg.find("unknown command: ") {
            let cmd = &error_msg[cmd_start + 17..]; // Skip "unknown command: "
            eprintln!("dsh: {}: command not found", cmd.trim());
        } else {
            eprintln!("dsh: command not found");
        }
    } else if error_msg.contains("Shell terminated by double Ctrl+C")
        || error_msg.contains("Normal exit")
        || error_msg.contains("Exit by")
    {
        // Don't display normal exit messages
        debug!("Shell exiting normally: {}", error_msg);
    } else {
        // For other errors, display the root cause without debug info
        eprintln!("dsh: {}", error_msg);
    }
}

mod completion;
mod direnv;
mod dirs;
mod environment;
mod history;
mod input;
mod lisp;
mod parser;
mod process;
mod prompt;
mod proxy;
mod repl;
mod shell;

#[cfg(test)]
mod error_handling_tests;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    command: Option<String>,
}

fn main() -> ExitCode {
    if let Err(err) = init_tracing() {
        eprintln!("Failed to initialize tracing: {err}");
        return ExitCode::FAILURE;
    }

    // パニックハンドラーを設定
    setup_panic_handler();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run_shell())
}

async fn run_shell() -> ExitCode {
    let cli = Cli::parse();
    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = create_context(&shell);

    if let Some(command) = cli.command.as_deref() {
        execute_command(&mut shell, &mut ctx, command).await
    } else {
        run_interactive(&mut shell, &mut ctx).await
    }
}

fn init_tracing() -> Result<()> {
    let log_file = std::sync::Arc::new(std::fs::File::create("./debug.log")?);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .init();
    // tracing_subscriber::fmt::init();
    Ok(())
}

fn setup_panic_handler() {
    std::panic::set_hook(Box::new(|panic_info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");

        let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic payload".to_string()
        };

        // 正常終了に関連するパニックの場合はstacktraceを表示しない
        if payload.contains("Shell terminated by double Ctrl+C")
            || payload.contains("Normal exit")
            || payload.contains("Exit by")
            || payload.contains("exit command")
        {
            // 正常終了の場合は簡潔なメッセージのみ
            debug!("Shell exiting normally: {}", payload);
            return;
        }

        let location = if let Some(location) = panic_info.location() {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        } else {
            "Unknown location".to_string()
        };

        // Get backtrace (if RUST_BACKTRACE=1 is set)
        let backtrace = std::backtrace::Backtrace::capture();
        let backtrace_str = match backtrace.status() {
            std::backtrace::BacktraceStatus::Captured => format!("\nBacktrace:\n{}", backtrace),
            std::backtrace::BacktraceStatus::Disabled => {
                "\nBacktrace: disabled (set RUST_BACKTRACE=1 to enable)".to_string()
            }
            std::backtrace::BacktraceStatus::Unsupported => "\nBacktrace: unsupported".to_string(),
            _ => "\nBacktrace: unknown status".to_string(),
        };

        // ログファイルに直接書き込み（tracingが初期化されていない可能性があるため）
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.3f UTC");
        let panic_log = format!(
            "\n=== PANIC OCCURRED ===\n\
            Timestamp: {}\n\
            Thread: {}\n\
            Location: {}\n\
            Message: {}{}\n\
            ======================\n",
            timestamp, thread_name, location, payload, backtrace_str
        );

        // Record logs in multiple ways
        // 1. Write directly to log file
        let log_files = ["./debug.log", "./panic.log"];
        for log_file in &log_files {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_file)
            {
                use std::io::Write;
                let _ = writeln!(file, "{}", panic_log);
                let _ = file.flush();
            }
        }

        // 2. Also try to output to tracing log (if initialized)
        tracing::error!("PANIC OCCURRED: {} at {}", payload, location);

        // 3. Also output to stderr (maintain default behavior)
        eprintln!("\n=== doge-shell PANIC ===");
        eprintln!("Message: {}", payload);
        eprintln!("Location: {}", location);
        eprintln!("Thread: {}", thread_name);
        eprintln!("Timestamp: {}", timestamp);
        eprintln!("See debug.log and panic.log for detailed information");
        eprintln!("========================\n");
    }));
}

fn create_context(shell: &Shell) -> Context {
    // Use safe Context creation (avoid panics)
    Context::new_safe(shell.pid, shell.pgid, true)
}

async fn execute_command(shell: &mut Shell, ctx: &mut Context, command: &str) -> ExitCode {
    debug!("start shell");
    shell.set_signals();

    match shell.eval_str(ctx, command.to_string(), false).await {
        Ok(code) => {
            debug!("run command mode {:?} : {:?}", command, &code);
            code
        }
        Err(err) => {
            display_user_error(&err);
            ExitCode::FAILURE
        }
    }
}

async fn run_interactive(shell: &mut Shell, ctx: &mut Context) -> ExitCode {
    debug!("start shell");
    shell.set_signals();
    ctx.save_history = false;

    let mut repl = Repl::new(shell);
    if let Err(err) = repl.shell.eval_str(ctx, "cd .".to_string(), false).await {
        display_user_error(&err);
        return ExitCode::FAILURE;
    }

    // Check if stdin is a terminal
    use nix::unistd::isatty;
    use std::os::unix::io::AsRawFd;
    if isatty(std::io::stdin().as_raw_fd()).unwrap_or(false) {
        // Interactive mode
        debug!("Running in interactive mode");
        match repl.run_interactive().await {
            Ok(_) => ExitCode::from(0),
            Err(err) => {
                // Don't display error message for normal exit
                let err_str = err.to_string();
                if err_str.contains("Shell terminated by double Ctrl+C")
                    || err_str.contains("Normal exit")
                    || err_str.contains("Exit by")
                {
                    debug!("Shell exiting normally: {}", err_str);
                    ExitCode::from(0)
                } else {
                    display_user_error(&err);
                    ExitCode::FAILURE
                }
            }
        }
    } else {
        // Pipe mode - read from stdin
        debug!("Running in pipe mode");
        use std::io::{self, BufRead, BufReader};
        let stdin = io::stdin();
        let reader = BufReader::new(stdin);

        for line in reader.lines() {
            match line {
                Ok(input) => {
                    let input = input.trim();
                    if input.is_empty() {
                        continue;
                    }
                    if input == "exit" {
                        break;
                    }
                    debug!("Processing pipe input: {}", input);
                    match repl.shell.eval_str(ctx, input.to_string(), false).await {
                        Ok(_) => {}
                        Err(err) => {
                            eprint!("Error executing '{}': ", input);
                            display_user_error(&err);
                        }
                    }
                }
                Err(err) => {
                    eprintln!("Error reading input: {}", err);
                    break;
                }
            }
        }
        ExitCode::from(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    #[test]
    #[ignore] // 通常のテスト実行では無視（手動実行用）
    fn test_panic_handler() {
        // テスト用のログファイルを使用
        let test_log_files = ["./debug.log", "./panic.log"];

        // 既存のログファイルを削除
        for log_file in &test_log_files {
            let _ = fs::remove_file(log_file);
        }

        // パニックハンドラーを設定
        setup_panic_handler();

        // 別スレッドでパニックを発生させる
        let handle = thread::spawn(|| {
            panic!("Test panic for logging verification");
        });

        // パニックを待つ
        let _ = handle.join();

        // 少し待ってからログファイルをチェック
        thread::sleep(Duration::from_millis(200));

        // ログファイルが作成され、パニック情報が記録されているかチェック
        let mut found_panic_log = false;
        for log_file in &test_log_files {
            if let Ok(content) = fs::read_to_string(log_file) {
                if content.contains("PANIC OCCURRED")
                    && content.contains("Test panic for logging verification")
                {
                    found_panic_log = true;
                    println!("Panic information found in {}", log_file);
                    break;
                }
            }
        }

        assert!(
            found_panic_log,
            "Panic information not found in any log file"
        );
        println!("Panic handler test passed - check debug.log and panic.log for details");
    }
}
