use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::fs;

/// Generator for process ID completion
pub struct ProcessGenerator;

impl ProcessGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::new();

        // Scan /proc for processes
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir()
                    && let Some(file_name) = path.file_name().and_then(|n| n.to_str())
                {
                    // Check if it's a PID (all digits)
                    if let Ok(pid) = file_name.parse::<u32>() {
                        let pid_str = pid.to_string();

                        // Read process name from /proc/<pid>/comm
                        let comm_path = path.join("comm");
                        let description = if let Ok(comm) = fs::read_to_string(comm_path) {
                            Some(comm.trim().to_string())
                        } else {
                            // Fallback to cmdline if comm fails or for more detail?
                            // comm is usually short name, cmdline is full.
                            // fish uses comm (or similar short name)
                            None
                        };

                        // Only add if it matches the current token (prefix match)
                        // We match against PID OR description name?
                        // Standard behavior usually filters by text (PID).
                        // But fish allows filtering by name.
                        // If we want to support name matching, we should probably output all candidates
                        // and let the fuzzy matcher handle it against the full "PID Name" string.
                        // The fuzzy matcher in completion.rs matches against `text` by default.
                        // But `Candidate::text()` returns the display string (PID + Name).
                        // So generating all candidates might be expensive but correct for fuzzy matching.

                        // Optimization: if current_token is numeric, filter by PID prefix.
                        // If user types text, filter by description prefix?

                        let mut matches = false;
                        if current_token.is_empty() {
                            matches = true;
                        } else if pid_str.starts_with(current_token) {
                            matches = true;
                        } else if let Some(desc) = &description {
                            // Simple case-insensitive match for convenience?
                            if desc.to_lowercase().contains(&current_token.to_lowercase()) {
                                matches = true;
                            }
                        }

                        // To mimic fish behavior strictly:
                        // If I type `kill fire`, fish shows `123 firefox`.

                        if matches {
                            candidates.push(CompletionCandidate::process(pid_str, description));
                        }
                    }
                }
            }
        }

        // Sort by PID?
        // Or sort by description?
        // Fish sorts by PID usually? No, it sorts by usage or PID.
        // Let's sort by PID numeric for now.
        candidates.sort_by(|a, b| {
            let pid_a = a.text.parse::<u32>().unwrap_or(0);
            let pid_b = b.text.parse::<u32>().unwrap_or(0);
            pid_a.cmp(&pid_b)
        });

        Ok(candidates)
    }
}

impl Default for ProcessGenerator {
    fn default() -> Self {
        Self::new()
    }
}
