use crate::completion::command::CompletionCandidate;
use crate::completion::parser::ParsedCommandLine;
use anyhow::Result;

// Trait for command execution to facilitate testing
pub trait ScriptRunner {
    fn run(&self, command: &str) -> Result<String>;
}

pub struct DefaultScriptRunner;

impl ScriptRunner for DefaultScriptRunner {
    fn run(&self, command: &str) -> Result<String> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()?;

        if !output.status.success() {
            return Ok(String::new());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
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
}
