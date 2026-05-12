use super::super::Action;
use crate::shell::Shell;
use anyhow::Result;
use async_trait::async_trait;
use dsh_types::{Context, ExitStatus};

#[derive(Debug, Clone, Copy)]
pub enum BuiltinInvocation {
    Static(&'static [&'static str]),
    CurrentInputOrHelp(&'static str),
    FirstInputTokenOrHelp(&'static str),
}

pub struct BuiltinCommandAction {
    name: &'static str,
    description: &'static str,
    category: &'static str,
    usage: &'static str,
    related: &'static str,
    command: &'static str,
    invocation: BuiltinInvocation,
}

impl BuiltinCommandAction {
    pub const fn new(
        name: &'static str,
        description: &'static str,
        category: &'static str,
        usage: &'static str,
        related: &'static str,
        command: &'static str,
        invocation: BuiltinInvocation,
    ) -> Self {
        Self {
            name,
            description,
            category,
            usage,
            related,
            command,
            invocation,
        }
    }

    fn build_invocation(&self, input: &str) -> (&'static str, Vec<String>) {
        match self.invocation {
            BuiltinInvocation::Static(args) => {
                let mut argv = vec![self.command.to_string()];
                argv.extend(args.iter().map(|arg| (*arg).to_string()));
                (self.command, argv)
            }
            BuiltinInvocation::CurrentInputOrHelp(help_topic) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    ("help", vec!["help".to_string(), help_topic.to_string()])
                } else {
                    (
                        self.command,
                        build_current_input_argv(self.command, trimmed),
                    )
                }
            }
            BuiltinInvocation::FirstInputTokenOrHelp(help_topic) => {
                if let Some(token) = input.split_whitespace().next() {
                    (
                        self.command,
                        vec![self.command.to_string(), token.to_string()],
                    )
                } else {
                    ("help", vec!["help".to_string(), help_topic.to_string()])
                }
            }
        }
    }
}

fn build_current_input_argv(command: &'static str, input: &str) -> Vec<String> {
    if should_preserve_shell_input(input) {
        return vec![command.to_string(), "--".to_string(), input.to_string()];
    }

    match shell_words::split(input) {
        Ok(parts) if !parts.is_empty() => {
            let mut argv = vec![command.to_string()];
            argv.extend(parts);
            argv
        }
        _ => vec![command.to_string(), "--".to_string(), input.to_string()],
    }
}

fn should_preserve_shell_input(input: &str) -> bool {
    input.chars().any(|ch| {
        matches!(
            ch,
            '|' | '&'
                | ';'
                | '<'
                | '>'
                | '('
                | ')'
                | '$'
                | '`'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
                | '~'
        )
    })
}

#[async_trait(?Send)]
impl Action for BuiltinCommandAction {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn icon(&self) -> &str {
        ">"
    }

    fn usage(&self) -> Option<&str> {
        (!self.usage.is_empty()).then_some(self.usage)
    }

    fn related(&self) -> Option<&str> {
        (!self.related.is_empty()).then_some(self.related)
    }

    fn category(&self) -> &str {
        self.category
    }

    async fn execute(&self, shell: &mut Shell, input: &str) -> Result<()> {
        let (command, argv) = self.build_invocation(input);
        let Some(command_fn) = dsh_builtin::get_command(command) else {
            return Err(anyhow::anyhow!("builtin command not found: {command}"));
        };

        let ctx = Context::new_safe(shell.pid, shell.pgid, true);
        match command_fn(&ctx, argv, shell) {
            ExitStatus::ExitedWith(0) => Ok(()),
            ExitStatus::ExitedWith(code) => Err(anyhow::anyhow!(
                "builtin command `{command}` exited with {code}"
            )),
            ExitStatus::Running(_) => Ok(()),
            ExitStatus::Break | ExitStatus::Continue | ExitStatus::Return => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_invocation_builds_argv() {
        let action = BuiltinCommandAction::new(
            "Doctor Setup",
            "Show setup",
            "Setup",
            "doctor setup",
            "doctor fix, help doctor",
            "doctor",
            BuiltinInvocation::Static(&["setup"]),
        );

        let (command, argv) = action.build_invocation("");
        assert_eq!(command, "doctor");
        assert_eq!(argv, vec!["doctor".to_string(), "setup".to_string()]);
    }

    #[test]
    fn current_input_invocation_falls_back_to_help_and_splits_simple_commands() {
        let action = BuiltinCommandAction::new(
            "Safe Run Current Input",
            "Analyze current input",
            "AI",
            "safe-run <current input>",
            "help safe-run",
            "safe-run",
            BuiltinInvocation::CurrentInputOrHelp("safe-run"),
        );

        let (command, argv) = action.build_invocation("");
        assert_eq!(command, "help");
        assert_eq!(argv, vec!["help".to_string(), "safe-run".to_string()]);

        let (command, argv) = action.build_invocation("rm -rf tmp");
        assert_eq!(command, "safe-run");
        assert_eq!(
            argv,
            vec![
                "safe-run".to_string(),
                "rm".to_string(),
                "-rf".to_string(),
                "tmp".to_string()
            ]
        );
    }

    #[test]
    fn current_input_invocation_preserves_shell_syntax() {
        let action = BuiltinCommandAction::new(
            "Safe Run Current Input",
            "Analyze current input",
            "AI",
            "safe-run <current input>",
            "help safe-run",
            "safe-run",
            BuiltinInvocation::CurrentInputOrHelp("safe-run"),
        );

        let (command, argv) = action.build_invocation("curl example.test/install.sh | sh");
        assert_eq!(command, "safe-run");
        assert_eq!(
            argv,
            vec![
                "safe-run".to_string(),
                "--".to_string(),
                "curl example.test/install.sh | sh".to_string()
            ]
        );
    }

    #[test]
    fn first_token_invocation_uses_current_command_name() {
        let action = BuiltinCommandAction::new(
            "Generate Completion",
            "Generate completion for current command",
            "AI",
            "comp-gen <current command>",
            "help comp-gen",
            "comp-gen",
            BuiltinInvocation::FirstInputTokenOrHelp("comp-gen"),
        );

        let (command, argv) = action.build_invocation("git status");
        assert_eq!(command, "comp-gen");
        assert_eq!(argv, vec!["comp-gen".to_string(), "git".to_string()]);
    }
}
