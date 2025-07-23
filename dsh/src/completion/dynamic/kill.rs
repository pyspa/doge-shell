use super::CompletionCandidate;
use super::DynamicCompletionHandler;
use super::ParsedCommandLine;
use crate::completion::CompletionType;
use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Handler for `kill` command dynamic completion.
pub struct KillCompletionHandler;

impl DynamicCompletionHandler for KillCompletionHandler {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        parsed_command.command == "kill" && parsed_command.args.len() <= 1
    }

    fn generate_candidates(
        &self,
        _parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        debug!("Generating dynamic completion candidates for 'kill' command.");

        // Since we can't use async directly, we'll use tokio::task::block_in_place
        // to run the async code in a blocking context
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                Command::new("ps")
                    .arg("-xo")
                    .arg("pid,%cpu,%mem,command")
                    .arg("--sort=%mem")
                    .output()
                    .await
            })
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines().skip(1) {
            let parts: Vec<&str> = line.trim().splitn(2, ' ').collect();
            if parts.len() == 2 {
                let pid = parts[0].trim();
                let comm = parts[1].trim();
                candidates.push(CompletionCandidate {
                    text: pid.to_string(),
                    description: Some(comm.to_owned()),
                    completion_type: CompletionType::Argument,
                    priority: 100,
                });
            }
        }
        debug!(
            "Generated {} candidates for 'kill' command.",
            candidates.len()
        );
        Ok(candidates)
    }
}
