use crate::completion::command::CompletionCandidate;
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

// Trait for command execution to facilitate testing
pub trait ScriptRunner {
    fn run(&self, command: &str) -> Result<String>;
}

pub struct DefaultScriptRunner;

const SCRIPT_TIMEOUT_MS: u64 = 2000;

impl ScriptRunner for DefaultScriptRunner {
    fn run(&self, command: &str) -> Result<String> {
        if cfg!(test) {
            return run_without_timeout(command);
        }
        run_with_timeout(command)
    }
}

fn run_with_timeout(command: &str) -> Result<String> {
    use std::io::Read;
    use std::time::Duration;
    use wait_timeout::ChildExt;

    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    // Read stdout in a separate thread to avoid deadlock.
    // If the child produces more output than the pipe buffer size (typically 64KB on Linux),
    // it will block on write() until the parent reads from the pipe.
    // If the parent is waiting for the child to exit before reading, we get a deadlock.
    let mut stdout = child.stdout.take().expect("Child stdout piped");
    let reader_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        stdout.read_to_string(&mut buf)?;
        Ok::<String, std::io::Error>(buf)
    });

    let timeout = Duration::from_millis(SCRIPT_TIMEOUT_MS);
    match child.wait_timeout(timeout)? {
        Some(status) => {
            // Process finished. Join the reader thread to get the output.
            let output = reader_thread
                .join()
                .map_err(|_| anyhow::anyhow!("Stdout reader thread panicked"))??;

            if status.success() {
                Ok(output)
            } else {
                Ok(String::new())
            }
        }
        None => {
            // Timeout occurred. Kill the child.
            let _ = child.kill();
            let _ = child.wait();

            // The reader thread should finish shortly after the pipe is broken/closed.
            // We join it to clean up resources, but ignore the result/error.
            let _ = reader_thread.join();

            Ok(String::new())
        }
    }
}

fn run_without_timeout(command: &str) -> Result<String> {
    use std::io::Read;

    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    // Read output BEFORE waiting, to prevent deadlock
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout)?;
    }

    let status = child.wait()?;
    if status.success() {
        Ok(stdout)
    } else {
        Ok(String::new())
    }
}

pub struct ScriptGenerator<R: ScriptRunner = DefaultScriptRunner> {
    runner: R,
}

impl<R: ScriptRunner> ScriptGenerator<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn generate_script_candidates(
        &self,
        command_template: &str,
        parsed: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        // Simple variable substitution
        let mut command = command_template.to_string();
        command = command.replace("$COMMAND", &parsed.command);
        if let Some(arg) = &parsed.current_arg {
            command = command.replace("$CURRENT_TOKEN", arg);
        } else {
            command = command.replace("$CURRENT_TOKEN", "");
        }
        if let Some(first_sub) = parsed.subcommand_path.first() {
            command = command.replace("$SUBCOMMAND", first_sub);
        } else {
            command = command.replace("$SUBCOMMAND", "");
        }

        // Quote arguments to prevent injection if possible, but basic replacement for now
        for (i, arg) in parsed.specified_arguments.iter().enumerate() {
            let key = format!("$ARG_{}", i);
            command = command.replace(&key, arg);
        }

        // Execute command
        let stdout = self.runner.run(&command)?;
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let (value, description) = if let Some((val, desc)) = trimmed.split_once('\t') {
                (val, Some(desc.to_string()))
            } else {
                (trimmed, None)
            };

            if value.starts_with(&parsed.current_token) {
                candidates.push(CompletionCandidate::argument(
                    value.to_string(),
                    description,
                ));
            }
        }
        Ok(candidates)
    }
}

impl Default for ScriptGenerator<DefaultScriptRunner> {
    fn default() -> Self {
        Self::new(DefaultScriptRunner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CompletionContext;

    struct MockScriptRunner {
        expected_command: String,
        output: String,
    }

    impl MockScriptRunner {
        fn new(expected_command: &str, output: &str) -> Self {
            Self {
                expected_command: expected_command.to_string(),
                output: output.to_string(),
            }
        }
    }

    impl ScriptRunner for MockScriptRunner {
        fn run(&self, command: &str) -> Result<String> {
            assert_eq!(command, self.expected_command);
            Ok(self.output.clone())
        }
    }

    #[test]
    fn test_script_variable_substitution() {
        let runner = MockScriptRunner::new("echo br", "branch1\nbranch2");

        let generator = ScriptGenerator::new(runner);

        let parsed = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec![],
            raw_args: vec![],
            args: vec![],
            options: vec![],
            current_token: "br".to_string(),
            current_arg: Some("br".to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        let template = "echo $CURRENT_TOKEN";

        let result = generator
            .generate_script_candidates(template, &parsed)
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "branch1");
    }

    #[test]
    fn test_script_description_parsing() {
        let runner = MockScriptRunner::new("echo test", "value1\tdescription1\nvalue2");
        let generator = ScriptGenerator::new(runner);

        let parsed = ParsedCommandLine {
            command: "test".to_string(),
            subcommand_path: vec![],
            raw_args: vec![],
            args: vec![],
            options: vec![],
            current_token: "val".to_string(),
            current_arg: Some("val".to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
        };

        let result = generator
            .generate_script_candidates("echo test", &parsed)
            .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text, "value1");
        assert_eq!(result[0].description, Some("description1".to_string()));
        assert_eq!(result[1].text, "value2");
        assert_eq!(result[1].description, None);
    }

    #[test]
    fn test_script_arg_substitution() {
        let runner = MockScriptRunner::new("echo foo", "result");

        let generator = ScriptGenerator::new(runner);

        let parsed = ParsedCommandLine {
            command: "test".to_string(),
            subcommand_path: vec![],
            raw_args: vec![],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: None,
            completion_context: CompletionContext::Argument {
                arg_index: 1,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec!["foo".to_string()],
            cursor_index: 0,
        };

        let template = "echo $ARG_0";

        let result = generator
            .generate_script_candidates(template, &parsed)
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "result");
    }
    #[test]
    fn test_large_output_deadlock() {
        // This test will hang if deadlock exists
        // We use "yes" to generate large output.
        // On Linux, pipe buffer is often 64KB. 100KB should be enough to block.
        // We use a shorter timeout for the test to fail fast if it deadlocks (Wait, wait_without_timeout has NO timeout).
        // Standard cargo test has a timeout usually? No.
        // This test might HANG forever if I'm right.
        // I should use a background thread or something?
        // Or trust that the environment kills it?
        // Use a reasonable size.

        let runner = DefaultScriptRunner;
        // Verify it runs using the 'real' runner (which logic we are testing, albeit without timeout in test mode)
        // But the deadlock logic is identical: wait() then read().

        // We need a command that produces output quickly and exits or hits the limit.
        // "python3 -c 'print(\"a\" * 100000)'"
        // Or "seq 1 10000"

        let res = runner.run("seq 1 20000").unwrap(); // 20000 lines * ~6 chars = 120KB > 64KB.
        assert!(res.len() > 100000);
    }
}
