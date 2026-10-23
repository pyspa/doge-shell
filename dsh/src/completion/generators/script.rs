use crate::completion::command::CompletionCandidate;
use crate::completion::parser::ParsedCommandLine;
use crate::completion::subprocess;
use anyhow::Result;
use std::time::Duration;

// Trait for command execution to facilitate testing
pub trait ScriptRunner {
    fn run(&self, command: &str) -> Result<String>;
}

pub struct DefaultScriptRunner;

const SCRIPT_TIMEOUT: Duration = Duration::from_millis(2000);

impl ScriptRunner for DefaultScriptRunner {
    fn run(&self, command: &str) -> Result<String> {
        subprocess::collect_stdout_or_empty(subprocess::shell_command(command), SCRIPT_TIMEOUT)
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
        let command = expand_script_template(command_template, parsed);

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

/// Interpolate complete, unbraced placeholders in one pass over the template.
/// Values are shell-quoted data and are never scanned again. Unknown identifiers,
/// noncanonical/overflow/out-of-range ARG indexes remain verbatim for compatibility
/// with ordinary shell variables. This is not a shell-template parser: existing
/// textual substitution also applies inside template quotes and after backslashes.
/// Suffix concatenation needs a boundary: `$CURRENT_TOKEN'_suffix'`, rather
/// than `$CURRENT_TOKEN_suffix` (an unknown identifier, left unchanged).
fn expand_script_template(template: &str, parsed: &ParsedCommandLine) -> String {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }
        let mut end = start + 1;
        while let Some(&(offset, next)) = chars.peek() {
            if !next.is_alphanumeric() && next != '_' {
                break;
            }
            end = offset + next.len_utf8();
            chars.next();
        }
        let name = &template[start + 1..end];
        let value = match name {
            "COMMAND" => Some(parsed.command.as_str()),
            "CURRENT_TOKEN" => Some(parsed.current_arg.as_deref().unwrap_or_default()),
            "SUBCOMMAND" => Some(
                parsed
                    .subcommand_path
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default(),
            ),
            _ => name
                .strip_prefix("ARG_")
                .filter(|index| {
                    !index.is_empty()
                        && index.bytes().all(|ch| ch.is_ascii_digit())
                        && (*index == "0" || !index.starts_with('0'))
                })
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| parsed.specified_arguments.get(index))
                .map(String::as_str),
        };
        if let Some(value) = value {
            result.push_str(&subprocess::shell_quote(value));
        } else {
            result.push_str(&template[start..end]);
        }
    }
    result
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
    fn script_placeholder_boundaries_and_unknown_values() {
        let mut parsed = crate::completion::parser::CommandLineParser::new().parse("probe ", 6);
        parsed.current_arg = None;
        parsed.specified_arguments = (0..=10).map(|n| format!("value{n}")).collect();
        let unknown = "$ARG_ $ARG_-1 $ARG_01 $ARG_999999999999999999999999999999 $ARG_11 $ARG_1suffix $CURRENT_TOKEN_suffix $COMMAND日本 ${COMMAND} $UNKNOWN";
        assert_eq!(expand_script_template(unknown, &parsed), unknown);
        assert_eq!(
            expand_script_template(
                "$ARG_1/$ARG_10/$ARG_1 $CURRENT_TOKEN $SUBCOMMAND $COMMAND",
                &parsed
            ),
            "'value1'/'value10'/'value1' '' '' 'probe'"
        );
        parsed.command = "$CURRENT_TOKEN".into();
        parsed.current_arg = Some("日本語 e\u{301} \\ ' \n$COMMAND".into());
        assert_eq!(
            expand_script_template("$COMMAND $CURRENT_TOKEN", &parsed),
            format!(
                "'$CURRENT_TOKEN' {}",
                subprocess::shell_quote(parsed.current_arg.as_deref().unwrap())
            )
        );
    }

    #[test]
    fn script_placeholders_are_complete_and_nonrecursive() {
        let mut parsed =
            crate::completion::parser::CommandLineParser::new().parse("probe value", 11);
        parsed.current_arg = Some("$ARG_0".into());
        parsed.specified_arguments = vec!["x; printf INJECTED; #".into()];
        // Mock only: these strings must never reach DefaultScriptRunner.
        ScriptGenerator::new(MockScriptRunner::new("printf %s '$ARG_0'", ""))
            .generate_script_candidates("printf %s $CURRENT_TOKEN", &parsed)
            .unwrap();
        parsed.specified_arguments = (0..=10).map(|n| format!("value{n}")).collect();
        ScriptGenerator::new(MockScriptRunner::new("printf %s 'value10'", ""))
            .generate_script_candidates("printf %s $ARG_10", &parsed)
            .unwrap();
    }

    #[test]
    fn test_script_variable_substitution() {
        let runner = MockScriptRunner::new("echo 'br'", "branch1\nbranch2");

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
            source: None,
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
            source: None,
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
        let runner = MockScriptRunner::new("echo 'foo'", "result");

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
            source: None,
        };

        let template = "echo $ARG_0";

        let result = generator
            .generate_script_candidates(template, &parsed)
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "result");
    }
    #[test]
    fn script_current_token_is_shell_quoted_against_injection() {
        // A malicious token must be passed as a single literal argument, never
        // interpreted as additional shell commands.
        let runner = MockScriptRunner::new(r#"git checkout '"; rm -rf ~'"#, "");

        let generator = ScriptGenerator::new(runner);

        let parsed = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec![],
            raw_args: vec![],
            args: vec![],
            options: vec![],
            current_token: r#""; rm -rf ~"#.to_string(),
            current_arg: Some(r#""; rm -rf ~"#.to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 0,
            source: None,
        };

        // Assertion of the exact quoted command happens inside MockScriptRunner::run.
        let result = generator
            .generate_script_candidates("git checkout $CURRENT_TOKEN", &parsed)
            .unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn test_large_output_deadlock() {
        let runner = DefaultScriptRunner;
        let res = runner.run("seq 1 20000").unwrap(); // 20000 lines * ~6 chars = 120KB > 64KB.
        assert!(res.len() > 100000);
    }

    #[test]
    fn script_runner_does_not_wait_for_background_stdout_holder() {
        let runner = DefaultScriptRunner;
        let started = std::time::Instant::now();

        let res = runner.run("(sleep 2; printf late) & exit 0").unwrap();

        assert_eq!(res, "");
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }
}
