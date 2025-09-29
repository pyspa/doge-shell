use super::CompletionCandidate;
use super::DynamicCompletionHandler;
use super::ParsedCommandLine;
use crate::completion::CompletionType;
use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Handler for `git` command dynamic completion.
pub struct GitCompletionHandler;

impl DynamicCompletionHandler for GitCompletionHandler {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        // Match git switch or git checkout commands
        parsed_command.command == "git"
            && !parsed_command.args.is_empty()
            && (parsed_command.args[0] == "checkout"
                || parsed_command.args[0] == "merge"
                || parsed_command.args[0] == "switch")
            && parsed_command.args.len() <= 2
    }

    fn generate_candidates(
        &self,
        _parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        debug!("Generating dynamic completion candidates for 'git' command.");

        // Since we can't use async directly, we'll use tokio::task::block_in_place
        // to run the async code in a blocking context
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { Command::new("git").arg("branch").arg("-a").output().await })
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
