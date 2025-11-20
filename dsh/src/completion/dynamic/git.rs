use super::CompletionCandidate;
use super::DynamicCompletionHandler;
use super::ParsedCommandLine;
use crate::completion::CompletionType;
use crate::completion::parser::CompletionContext;
use anyhow::{Result, bail};
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

        let candidates = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let primary = primary_subcommand(parsed_command).unwrap_or("");

                if matches!(primary, "push" | "pull" | "fetch") {
                    self.generate_push_like_candidates_async(parsed_command)
                        .await
                } else {
                    let filter = parsed_command
                        .current_arg
                        .clone()
                        .filter(|arg| !arg.is_empty());
                    self.run_branch_candidates(filter, None).await
                }
            })
        })?;

        debug!(
            "Generated {} candidates for 'git' command.",
            candidates.len()
        );
        Ok(candidates)
    }
}

impl GitCompletionHandler {
    async fn generate_push_like_candidates_async(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        let state = PushState::from(parsed_command);
        let needs_remotes = !state.has_remote() || state.is_cursor_on_remote();
        let remotes = if needs_remotes {
            Some(self.load_git_remotes().await?)
        } else {
            None
        };

        let remote_slice = remotes.as_deref().unwrap_or(&[]);
        let target = determine_push_target(&state, remote_slice);

        match target {
            PushTarget::Remote => {
                let remotes = remotes.unwrap_or(self.load_git_remotes().await?);
                Ok(Self::remotes_to_candidates(remotes))
            }
            PushTarget::Branch {
                filter,
                remote_injection,
            } => self.run_branch_candidates(filter, remote_injection).await,
        }
    }

    async fn run_branch_candidates(
        &self,
        filter: Option<String>,
        remote_injection: Option<String>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut cmd = Command::new("git");
        cmd.arg("branch").arg("-a");

        if let Some(pattern) = filter.as_ref().filter(|p| !p.is_empty()) {
            cmd.arg("--list").arg(format!("*{}*", pattern));
        }

        let output = cmd.output().await?;
        if !output.status.success() {
            bail!("`git branch -a` exited with {}", output.status);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let candidates = Self::parse_branch_candidates(&stdout, remote_injection.as_deref());

        Ok(candidates)
    }

    async fn load_git_remotes(&self) -> Result<Vec<String>> {
        let output = Command::new("git").arg("remote").output().await?;
        if !output.status.success() {
            bail!("`git remote` exited with {}", output.status);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect())
    }

    fn remotes_to_candidates(remotes: Vec<String>) -> Vec<CompletionCandidate> {
        remotes
            .into_iter()
            .map(|remote| CompletionCandidate {
                text: remote,
                description: None,
                completion_type: CompletionType::Argument,
                priority: 100,
            })
            .collect()
    }

    fn parse_branch_candidates(
        stdout: &str,
        remote_injection: Option<&str>,
    ) -> Vec<CompletionCandidate> {
        stdout
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return None;
                }

                let is_current = trimmed.starts_with("* ");
                let branch = trimmed
                    .trim_start_matches("* ")
                    .trim()
                    .trim_start_matches("+ ")
                    .trim();

                if branch.is_empty() {
                    return None;
                }

                let mut text = branch.to_string();
                if let Some(prefix) = remote_injection {
                    text = format!("{prefix} {branch}");
                }

                let description = if is_current {
                    Some("current branch".to_string())
                } else {
                    None
                };

                Some(CompletionCandidate {
                    text,
                    description,
                    completion_type: CompletionType::Argument,
                    priority: 100,
                })
            })
            .collect()
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

    if sub_len >= 2 {
        return true;
    }

    // If we are completing the subcommand itself (e.g. "git pu"), don't show branches.
    if let Some(last_sub) = parsed_command.subcommand_path.last()
        && parsed_command.current_token == *last_sub
    {
        return false;
    }

    true
}

#[derive(Debug, Clone)]
struct PushState {
    args: Vec<String>,
    editing_index: usize,
    current_token: String,
}

impl PushState {
    fn from(parsed_command: &ParsedCommandLine) -> Self {
        let mut args = Vec::new();
        args.extend(parsed_command.subcommand_path.iter().skip(1).cloned());
        args.extend(parsed_command.args.clone());

        let current_token = parsed_command.current_arg.clone().unwrap_or_default();
        let editing_index = if current_token.is_empty() {
            args.len()
        } else if let Some(pos) = args.iter().position(|arg| arg == &current_token) {
            pos
        } else {
            args.len()
        };

        Self {
            args,
            editing_index,
            current_token,
        }
    }

    fn remote(&self) -> Option<&str> {
        self.args.first().map(|s| s.as_str())
    }

    fn has_remote(&self) -> bool {
        self.remote().is_some()
    }

    fn branch_partial(&self) -> Option<&str> {
        if !self.has_remote() {
            return None;
        }

        if self.editing_index == 0 {
            return None;
        }

        if self.editing_index < self.args.len() {
            Some(self.args[self.editing_index].as_str())
        } else {
            Some("")
        }
    }

    fn is_cursor_on_remote(&self) -> bool {
        self.editing_index == 0 && !self.current_token.is_empty()
    }

    fn only_remote_provided(&self) -> bool {
        self.args.len() == 1
    }

    fn should_inject_branch(&self, remote_confirmed: bool) -> bool {
        remote_confirmed && self.is_cursor_on_remote() && self.only_remote_provided()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PushTarget {
    Remote,
    Branch {
        filter: Option<String>,
        remote_injection: Option<String>,
    },
}

fn determine_push_target(state: &PushState, remotes: &[String]) -> PushTarget {
    match state.remote() {
        None => PushTarget::Remote,
        Some(remote) => {
            if state.is_cursor_on_remote() {
                let remote_confirmed = remotes.iter().any(|name| name == remote);
                if state.should_inject_branch(remote_confirmed) {
                    PushTarget::Branch {
                        filter: None,
                        remote_injection: Some(remote.to_string()),
                    }
                } else {
                    PushTarget::Remote
                }
            } else {
                let filter = state
                    .branch_partial()
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());

                PushTarget::Branch {
                    filter,
                    remote_injection: None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::parser::{CommandLineParser, CompletionContext};

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

    #[test]
    fn test_push_state_without_space_detects_remote() {
        let parser = CommandLineParser::new();
        let parsed = parser.parse("git push origin", "git push origin".len());
        let state = PushState::from(&parsed);

        assert_eq!(state.remote(), Some("origin"));
        assert!(state.is_cursor_on_remote());

        let remotes = vec!["origin".to_string()];
        let target = determine_push_target(&state, &remotes);
        assert_eq!(
            target,
            PushTarget::Branch {
                filter: None,
                remote_injection: Some("origin".to_string()),
            }
        );
    }

    #[test]
    fn test_push_state_requires_confirmed_remote() {
        let parser = CommandLineParser::new();
        let parsed = parser.parse("git push ori", "git push ori".len());
        let state = PushState::from(&parsed);
        let remotes = vec!["origin".to_string()];

        assert_eq!(determine_push_target(&state, &remotes), PushTarget::Remote);
    }

    #[test]
    fn test_push_state_after_space_prefers_branch() {
        let parser = CommandLineParser::new();
        let parsed = parser.parse("git push origin ", "git push origin ".len());
        let state = PushState::from(&parsed);
        let remotes = vec!["origin".to_string()];

        assert_eq!(state.branch_partial(), Some(""));
        assert_eq!(
            determine_push_target(&state, &remotes),
            PushTarget::Branch {
                filter: None,
                remote_injection: None,
            }
        );
    }
}
