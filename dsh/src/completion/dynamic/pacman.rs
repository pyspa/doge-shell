use super::CompletionCandidate;
use super::DynamicCompletionHandler;
use super::ParsedCommandLine;
use crate::completion::CompletionType;
use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

/// Handler for `pacman` command dynamic completion.
pub struct PacmanCompletionHandler;

impl DynamicCompletionHandler for PacmanCompletionHandler {
    fn matches(&self, parsed_command: &ParsedCommandLine) -> bool {
        // Match sudo pacman -S command
        // debug!(
        //     \"@parsed_command {:?} {:?}\",
        //     parsed_command.args, parsed_command.options
        // );
        parsed_command.command == "sudo"
            && !parsed_command.args.is_empty()
            && parsed_command.args[0] == "pacman"
            && parsed_command.args.len() > 1
            && parsed_command.args[1].contains("S")
    }

    fn generate_candidates(
        &self,
        parsed_command: &ParsedCommandLine,
    ) -> Result<Vec<CompletionCandidate>> {
        debug!(
            "Generating dynamic completion candidates for 'pacman' command. {:?}",
            parsed_command
        );

        let query = parsed_command.current_arg.as_deref().unwrap_or("");

        // Since we can't use async directly, we'll use tokio::task::block_in_place
        // to run the async code in a blocking context
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                Command::new("pacman")
                    .arg("-Ssq")
                    .arg("^".to_string() + query)
                    .output()
                    .await
            })
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut candidates = Vec::new();

        for line in stdout.lines() {
            let package = line.trim();
            if !package.is_empty() && package.starts_with(query) {
                candidates.push(CompletionCandidate {
                    text: package.to_string(),
                    description: None,
                    completion_type: CompletionType::Argument,
                    priority: 100,
                });
            }
        }

        debug!(
            "Generated {} candidates for 'pacman' command.",
            candidates.len()
        );
        Ok(candidates)
    }
}
