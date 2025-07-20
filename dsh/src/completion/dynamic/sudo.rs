use super::CompletionCandidate;
use super::DynamicCompletionHandler;
use super::ParsedCommandLine;
use crate::completion::CompletionType;
use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Handler for `sudo` command dynamic completion.
pub struct SudoCompletionHandler;

impl DynamicCompletionHandler for SudoCompletionHandler {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        parsed_command.command == "sudo" && parsed_command.args.is_empty()
    }

    fn generate_candidates(
        &self,
        _parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        debug!("Generating dynamic completion candidates for 'sudo' command.");

        // Since we can't use async directly, we'll use tokio::task::block_in_place
        // to run the async code in a blocking context
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { Command::new("getent").arg("passwd").output().await })
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if let Some(username) = parts.first() {
                candidates.push(CompletionCandidate {
                    text: username.to_string(),
                    description: None,
                    completion_type: CompletionType::Argument,
                    priority: 100,
                });
            }
        }
        debug!(
            "Generated {} candidates for 'sudo' command.",
            candidates.len()
        );
        Ok(candidates)
    }
}
