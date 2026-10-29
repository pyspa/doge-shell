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

    let root = match proxy
        .get_current_dir()
        .and_then(|path| path.canonicalize().map_err(Into::into))
    {
        Ok(root) => root,
        Err(error) => {
            let _ = ctx.write_stderr(&format!(
                "trigger: failed to resolve watch directory: {error}"
            ));
            return ExitStatus::ExitedWith(1);
        }
    };
    let pattern_str = &argv[1];
    let cmd_parts = &argv[2..];
    let cmd_line = command_line(cmd_parts);

    let pattern = match Pattern::new(pattern_str) {
        Ok(p) => p,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("trigger: invalid glob pattern: {}", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    let _ = ctx.write_stdout(&format!(
        "trigger: Watching for changes matching '{}' to run '{}'",
        pattern_str, cmd_line
    ));

    let (tx, rx) = channel();

    let mut watcher = match RecommendedWatcher::new(tx, Config::default()) {
        Ok(w) => w,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("trigger: failed to create watcher: {}", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        let _ = ctx.write_stderr(&format!("trigger: failed to watch directory: {}", e));
        return ExitStatus::ExitedWith(1);
    }

    let _ = ctx.write_stdout("Press Ctrl+C to stop.");

    let debounce_duration = Duration::from_millis(500);
    let mut last_run = Instant::now()
        .checked_sub(debounce_duration)
        .unwrap_or(Instant::now());

    loop {
        if proxy.is_canceled() {
            let _ = ctx.write_stdout("\ntrigger: stopped");
            break;
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(res) => {
                match res {
                    Ok(event) => {
                        let mut triggered = false;
                        for path in event.paths {
                            if event_matches(&pattern, &root, &path) {
                                triggered = true;
                                break;
                            }
                        }

                        if triggered {
                            if last_run.elapsed() < debounce_duration {
                                continue;
                            }

                            let _ = ctx.write_stdout(&format!(
                                "\n[trigger] Change detected. Running: {}",
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
                                        "[trigger] {} (Exit: {})",
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
                                        "trigger: failed to execute command: {}",
                                        e
                                    ));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = ctx.write_stderr(&format!("trigger: watch error: {:?}", e));
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Continue checking cancellation
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = ctx.write_stderr("trigger: watcher disconnected");
                break;
            }
        }
    }

    ExitStatus::ExitedWith(0)
}

// capture_command executes through /bin/sh; preserve each argument with POSIX quoting.
fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "'\"'\"'")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_arguments_round_trip() {
        let args: Vec<String> = [
            "printf",
            "%s",
            "",
            "a b",
            "a'b",
            "SAFE; printf EXTRA",
            "$HOME",
            "$(pwd)",
            "*.txt",
            "line\nbreak",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(shell_words::split(&command_line(&args)).unwrap(), args);
    }
}

fn event_matches(pattern: &Pattern, root: &Path, path: &Path) -> bool {
    if Path::new(pattern.as_str()).is_absolute() {
        return pattern.matches(&path.to_string_lossy());
    }
    let relative = if path.is_absolute() {
        match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(_) => return false,
        }
    } else {
        path.strip_prefix(".").unwrap_or(path)
    };
    // Retain the historical lossy matching for filenames containing non-UTF-8 bytes.
    pattern.matches(&relative.to_string_lossy())
}

#[cfg(test)]
mod path_tests {
    use super::*;
    #[test]
    fn non_utf8_events_match_relative_and_absolute_patterns() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = tempfile::tempdir().unwrap();
        let relative = Path::new("src").join(OsString::from_vec(b"raw-\xff.txt".to_vec()));
        let absolute = root.path().join(&relative);
        for pattern in ["*.txt", "src/*.txt"] {
            let pattern = Pattern::new(pattern).unwrap();
            assert!(event_matches(&pattern, root.path(), &absolute));
            assert!(event_matches(&pattern, root.path(), &relative));
        }
        let absolute_pattern =
            Pattern::new(&format!("{}/src/*.txt", root.path().display())).unwrap();
        assert!(event_matches(&absolute_pattern, root.path(), &absolute));
        assert!(!event_matches(
            &Pattern::new("*.rs").unwrap(),
            root.path(),
            &absolute
        ));
    }

    #[test]
    fn absolute_event_matches_relative_pattern() {
        let root = tempfile::tempdir().unwrap();
        let pattern = Pattern::new("src/*.txt").unwrap();
        assert!(event_matches(
            &pattern,
            root.path(),
            &root.path().join("src/file.txt")
        ));
        assert!(event_matches(
            &pattern,
            root.path(),
            Path::new("./src/file.txt")
        ));
        assert!(!event_matches(
            &pattern,
            root.path(),
            &root.path().join("other/file.txt")
        ));
        assert!(!event_matches(
            &pattern,
            root.path(),
            &root.path().parent().unwrap().join("src/file.txt")
        ));
    }
}
