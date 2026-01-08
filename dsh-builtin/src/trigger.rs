use crate::ShellProxy;
use dsh_types::{Context, ExitStatus, output_history::OutputEntry};
use glob::Pattern;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

pub fn description() -> &'static str {
    "Monitor file changes and execute commands"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 3 {
        let _ = ctx.write_stderr("Usage: trigger <glob-pattern> <command> [args...]");
        return ExitStatus::ExitedWith(1);
    }

    let pattern_str = &argv[1];
    let cmd_parts = &argv[2..];
    let cmd_line = cmd_parts.join(" ");

    let pattern = match Pattern::new(pattern_str) {
        Ok(p) => p,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("trigger: invalid glob pattern: {}", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    let _ = ctx.write_stdout(&format!(
        "trigger: Watching for changes matching '{}' to run '{}'\n",
        pattern_str, cmd_line
    ));
    let _ = ctx.write_stdout("Press Ctrl+C to stop.\n");

    let (tx, rx) = channel();

    let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("trigger: failed to create watcher: {}", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    if let Err(e) = watcher.watch(Path::new("."), RecursiveMode::Recursive) {
        let _ = ctx.write_stderr(&format!("trigger: failed to watch directory: {}", e));
        return ExitStatus::ExitedWith(1);
    }

    let debounce_duration = Duration::from_millis(500);
    let mut last_run = Instant::now()
        .checked_sub(debounce_duration)
        .unwrap_or(Instant::now());

    loop {
        if proxy.is_canceled() {
            let _ = ctx.write_stdout("\ntrigger: stopped\n");
            break;
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(res) => {
                match res {
                    Ok(event) => {
                        let mut triggered = false;
                        for path in event.paths {
                            // Strip "./" prefix if present to match glob patterns easier
                            let path_str = path.to_string_lossy();
                            let relative_path = path_str.strip_prefix("./").unwrap_or(&path_str);

                            if pattern.matches(relative_path) {
                                triggered = true;
                                break;
                            }
                        }

                        if triggered {
                            if last_run.elapsed() < debounce_duration {
                                continue;
                            }

                            let _ = ctx.write_stdout(&format!(
                                "\n[trigger] Change detected. Running: {}\n",
                                cmd_line
                            ));
                            last_run = Instant::now();

                            match proxy.capture_command(ctx, &cmd_line) {
                                Ok((exit_code, stdout, stderr)) => {
                                    // Print output live
                                    if !stdout.is_empty() {
                                        let _ = ctx.write_stdout(&stdout);
                                    }
                                    if !stderr.is_empty() {
                                        let _ = ctx.write_stderr(&stderr);
                                    }

                                    let status_msg =
                                        if exit_code == 0 { "Success" } else { "Failed" };
                                    let _ = ctx.write_stdout(&format!(
                                        "[trigger] {} (Exit: {})\n",
                                        status_msg, exit_code
                                    ));

                                    // Save to history
                                    let entry = OutputEntry::new(
                                        cmd_line.clone(),
                                        stdout,
                                        stderr,
                                        exit_code,
                                    );
                                    proxy.save_output_history(entry);
                                }
                                Err(e) => {
                                    let _ = ctx.write_stderr(&format!(
                                        "trigger: failed to execute command: {}\n",
                                        e
                                    ));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = ctx.write_stderr(&format!("trigger: watch error: {:?}\n", e));
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Continue checking cancellation
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = ctx.write_stderr("trigger: watcher disconnected\n");
                break;
            }
        }
    }

    ExitStatus::ExitedWith(0)
}
