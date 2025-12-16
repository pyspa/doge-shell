use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::fs;
use std::sync::LazyLock;
use std::time::Duration;

// Cache TTL for process list (1 second should be enough to feel responsive but vaguely fresh)
const PROCESS_CACHE_TTL_MS: u64 = 1000;

static PROCESS_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(PROCESS_CACHE_TTL_MS)));

/// Generator for process ID completion
pub struct ProcessGenerator;

impl ProcessGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        // We need to handle the two branches separately to avoid type mismatch (Arc vs Vec)
        // or unify them.

        let candidates_vec: Vec<CompletionCandidate>;
        let candidates_arc: std::sync::Arc<Vec<CompletionCandidate>>;

        let candidates_iter = if let Some(hit) = PROCESS_CACHE.get_entry("") {
            candidates_arc = hit;
            candidates_arc.iter()
        } else {
            // 2. If not in cache, scan /proc
            let mut candidates = Vec::new();

            if let Ok(entries) = fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir()
                        && let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                            // Check if it's a PID (all digits)
                            if let Ok(pid) = file_name.parse::<u32>() {
                                let pid_str = pid.to_string();

                                // Read process name from /proc/<pid>/comm
                                let comm_path = path.join("comm");
                                let description = if let Ok(comm) = fs::read_to_string(comm_path) {
                                    Some(comm.trim().to_string())
                                } else {
                                    None
                                };

                                candidates.push(CompletionCandidate::process(pid_str, description));
                            }
                        }
                }
            }

            // Sort by PID
            candidates.sort_by(|a, b| {
                let pid_a = a.text.parse::<u32>().unwrap_or(0);
                let pid_b = b.text.parse::<u32>().unwrap_or(0);
                pid_a.cmp(&pid_b)
            });

            // Store in cache
            PROCESS_CACHE.set("".to_string(), candidates.clone());
            candidates_vec = candidates;
            candidates_vec.iter()
        };

        // 3. Filter based on current token
        if current_token.is_empty() {
            return Ok(candidates_iter.cloned().collect());
        }

        let token_lower = current_token.to_lowercase();

        let filtered: Vec<CompletionCandidate> = candidates_iter
            .filter(|c| {
                // PID prefix match
                if c.text.starts_with(current_token) {
                    return true;
                }
                // Name subsequence/substring match
                if let Some(desc) = &c.description
                    && desc.to_lowercase().contains(&token_lower) {
                        return true;
                    }
                false
            })
            .cloned() // Clone only the matches
            .collect();

        Ok(filtered)
    }
}

impl Default for ProcessGenerator {
    fn default() -> Self {
        Self::new()
    }
}
