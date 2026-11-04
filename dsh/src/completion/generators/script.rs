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

        let query = parsed.completion_query();
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

            if value.starts_with(&query) {
                candidates.push(CompletionCandidate::argument(
                    value.to_string(),
                    description,
                ));
            }
        }
        Ok(candidates)
    }
}

/// Pass input as environment data to a fresh shell, never as shell source.
/// Templates use normal shell quoting: "$ARG_0" preserves a single value,
/// while '$ARG_0' is literal text. Expansion results are never re-evaluated.
fn expand_script_template(template: &str, parsed: &ParsedCommandLine) -> String {
    let mut result = String::new();
    let mut assign = |name: &str, value: &str| {
        result.push_str(name);
        result.push('=');
        result.push_str(&subprocess::shell_quote(value));
        result.push(' ');
    };
    assign("COMMAND", &parsed.command);
    assign(
        "CURRENT_TOKEN",
        parsed.current_arg.as_deref().unwrap_or_default(),
    );
    assign(
        "SUBCOMMAND",
        parsed
            .subcommand_path
            .first()
            .map(String::as_str)
            .unwrap_or_default(),
    );
    for (index, value) in parsed.specified_arguments.iter().enumerate() {
        assign(&format!("ARG_{index}"), value);
    }
    result.push_str("/bin/sh -c ");
    result.push_str(&subprocess::shell_quote(template));
    result
}

impl Default for ScriptGenerator<DefaultScriptRunner> {
    fn default() -> Self {
        Self::new(DefaultScriptRunner)
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn audit_quoted_placeholder_never_executes_data() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let mut parsed = crate::completion::parser::CommandLineParser::new().parse("probe ", 6);
        parsed.specified_arguments = vec![format!("$(printf bad > {})", marker.display())];
        let result = ScriptGenerator::default()
            .generate_script_candidates(r#"printf '%s\n' "$ARG_0""#, &parsed)
            .unwrap();
        assert!(!marker.exists());
        assert_eq!(result[0].text, parsed.specified_arguments[0]);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    #[test]
    fn template_values_are_data_in_shell_quote_contexts() {
        let mut parsed = crate::completion::parser::CommandLineParser::new().parse("probe ", 6);
        let payload = "日本語 e\u{301} ' \\ ; $COMMAND `printf BAD` $(printf BAD)";
        parsed.current_arg = Some(payload.into());
        parsed.specified_arguments = (0..=10).map(|n| format!("value{n}")).collect();
        for (template, expected) in [
            (r#"printf '%s' "$CURRENT_TOKEN""#, payload),
            ("printf '%s' '$CURRENT_TOKEN'", "$CURRENT_TOKEN"),
            (
                r#"printf '%s' "${COMMAND}:$ARG_1/$ARG_10:$SUBCOMMAND""#,
                "probe:value1/value10:",
            ),
            ("printf '%s' $ARG_1", "value1"),
        ] {
            let output = DefaultScriptRunner
                .run(&expand_script_template(template, &parsed))
                .unwrap();
            assert_eq!(output, expected);
        }
    }
    #[test]
    fn descriptions_and_decoded_filter_preserve_values() {
        let input = "probe \"al";
        let parsed = crate::completion::parser::CommandLineParser::new().parse(input, input.len());
        let result = ScriptGenerator::default()
            .generate_script_candidates("printf 'alpha\\tdescription\\nbeta\\n'", &parsed)
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].text, "alpha");
        assert_eq!(result[0].description.as_deref(), Some("description"));
    }
    #[test]
    fn large_output_is_drained() {
        assert!(DefaultScriptRunner.run("seq 1 20000").unwrap().len() > 100000);
    }
    #[test]
    fn background_stdout_holder_does_not_block() {
        let started = std::time::Instant::now();
        assert_eq!(
            DefaultScriptRunner
                .run("(sleep 2; printf late) & exit 0")
                .unwrap(),
            ""
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
