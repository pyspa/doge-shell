use super::CompletionCandidate;
use super::DynamicCompletionHandler;
use super::ParsedCommandLine;
use crate::completion::CompletionType;
use crate::completion::parser::CompletionContext;
use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Handler for `git` command dynamic completion.
pub struct GitCompletionHandler;

impl DynamicCompletionHandler for GitCompletionHandler {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        if parsed_command.command != "git" {
            return false;
        }

        let primary = match primary_subcommand(parsed_command) {
            Some(value) => value,
            None => return false,
        };

        if !is_branch_related_primary(primary) {
            return false;
        }

        match primary {
            "push" | "pull" | "fetch" => is_branch_target_for_remote(parsed_command),
            _ => is_branch_target_position(parsed_command),
        }
    }

    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        debug!(
            "Generating dynamic completion candidates for 'git' command. {:?}",
            parsed_command
        );

        // Since we can't use async directly, we'll use tokio::task::block_in_place
        // to run the async code in a blocking context
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let primary = primary_subcommand(parsed_command).unwrap_or("");
                let is_remote_target = matches!(primary, "push" | "pull" | "fetch")
                    && parsed_command.args.is_empty()
                    && parsed_command.current_token
                        == parsed_command.current_arg.clone().unwrap_or_default();

                if is_remote_target {
                    Command::new("git").arg("remote").output().await
                } else if let Some(arg) = parsed_command.current_arg.as_ref() {
                    Command::new("git")
                        .arg("branch")
                        .arg("-a")
                        .arg("--list")
                        .arg(format!("*{}*", arg))
                        .output()
                        .await
                } else {
                    Command::new("git").arg("branch").arg("-a").output().await
                }
            })
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            let branch = line.trim_start_matches("* ").trim();
            if !branch.is_empty() {
                let is_current = line.starts_with("* ");
                let description = if is_current {
                    Some("current branch".to_string())
                } else {
                    None
                };
                let branch = line.trim_start_matches("+ ").trim();

                candidates.push(CompletionCandidate {
                    text: branch.to_string(),
                    description,
                    completion_type: CompletionType::Argument,
                    priority: 100,
                });
            }
        }

        debug!(
            "Generated {} candidates for 'git' command.",
            candidates.len()
        );
        Ok(candidates)
    }
}

fn primary_subcommand(parsed_command: &ParsedCommandLine) -> Option<&str> {
    if let Some(first) = parsed_command.subcommand_path.first() {
        Some(first.as_str())
    } else {
        parsed_command.args.first().map(|arg| arg.as_str())
    }
}

fn is_branch_related_primary(primary: &str) -> bool {
    matches!(
        primary,
        "checkout" | "switch" | "merge" | "rebase" | "branch" | "reset" | "push" | "pull" | "fetch"
    )
}

fn is_branch_target_position(parsed_command: &ParsedCommandLine) -> bool {
    if parsed_command.subcommand_path.len() <= 1 && parsed_command.args.is_empty() {
        if let Some(current_arg) = parsed_command.current_arg.as_deref()
            && current_arg.is_empty()
        {
            return true;
        }

        matches!(
            parsed_command.completion_context,
            CompletionContext::Argument { .. }
        )
    } else {
        true
    }
}

fn is_branch_target_for_remote(parsed_command: &ParsedCommandLine) -> bool {
    let sub_len = parsed_command.subcommand_path.len();

    if sub_len < 1 {
        return false;
    }

    // If we are completing the subcommand itself (e.g. "git pu"), don't show branches.
    if let Some(last_sub) = parsed_command.subcommand_path.last()
        && parsed_command.current_token == *last_sub {
            return false;
        }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::CompletionContext;

    #[test]
    fn test_git_push_matches() {
        let handler = GitCompletionHandler;

        // Case 1: "git push" (cursor at end)
        let cmd = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec!["push".to_string()],
            args: vec![],
            options: vec![],
            current_token: "".to_string(),
            current_arg: Some("".to_string()),
            completion_context: CompletionContext::Command,
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 8,
        };

        assert!(handler.matches(&cmd), "Should match 'git push'");

        // Case 2: "git push ori" (cursor at end of ori)
        let cmd_arg = ParsedCommandLine {
            command: "git".to_string(),
            subcommand_path: vec!["push".to_string()],
            args: vec![],
            options: vec![],
            current_token: "ori".to_string(),
            current_arg: Some("ori".to_string()),
            completion_context: CompletionContext::Argument {
                arg_index: 0,
                arg_type: None,
            },
            specified_options: vec![],
            specified_arguments: vec![],
            cursor_index: 12,
        };

        assert!(handler.matches(&cmd_arg), "Should match 'git push ori'");
    }
}
