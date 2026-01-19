use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::fs;
use std::sync::LazyLock;
use std::time::Duration;

// Cache TTL for group list (5 seconds - groups don't change often)
const GROUP_CACHE_TTL_MS: u64 = 5000;

static GROUP_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(GROUP_CACHE_TTL_MS)));

/// Generator for system group name completion
pub struct GroupGenerator;

impl GroupGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        // Check cache first
        if let Some(cached) = GROUP_CACHE.get_entry("") {
            return Ok(self.filter_candidates(&cached, current_token));
        }

        // Parse /etc/group for group names
        let mut candidates = Vec::new();

        if let Ok(content) = fs::read_to_string("/etc/group") {
            for line in content.lines() {
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                // Format: groupname:x:gid:members
                let parts: Vec<&str> = line.split(':').collect();
                if !parts.is_empty() {
                    let groupname = parts[0].to_string();
                    let gid = parts.get(2).and_then(|s| s.parse::<u32>().ok());

                    // Include all groups, but show GID in description
                    let description = gid.map(|id| format!("GID: {}", id));
                    candidates.push(CompletionCandidate::argument(groupname, description));
                }
            }
        }

        // Sort alphabetically
        candidates.sort_by(|a, b| a.text.cmp(&b.text));

        // Store in cache
        GROUP_CACHE.set("".to_string(), candidates.clone());

        Ok(self.filter_candidates(&candidates, current_token))
    }

    fn filter_candidates(
        &self,
        candidates: &[CompletionCandidate],
        current_token: &str,
    ) -> Vec<CompletionCandidate> {
        if current_token.is_empty() {
            return candidates.to_vec();
        }

        let token_lower = current_token.to_lowercase();
        candidates
            .iter()
            .filter(|c| c.text.to_lowercase().starts_with(&token_lower))
            .cloned()
            .collect()
    }
}

impl Default for GroupGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_generator_creates() {
        let generator = GroupGenerator::new();
        let _ = generator;
    }

    #[test]
    fn test_group_generator_generates_candidates() {
        let generator = GroupGenerator::new();
        let result = generator.generate_candidates("");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        // On Linux/macOS, 'root' or 'wheel' group should be present
        let has_common_group = candidates.iter().any(|c| {
            c.text == "root" || c.text == "wheel" || c.text == "users" || c.text == "staff"
        });
        assert!(has_common_group, "Expected at least one common group");
    }

    #[test]
    fn test_group_generator_filters_by_prefix() {
        let generator = GroupGenerator::new();
        let result = generator.generate_candidates("ro");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        for c in &candidates {
            assert!(
                c.text.to_lowercase().starts_with("ro"),
                "Expected candidate '{}' to start with 'ro'",
                c.text
            );
        }
    }

    #[test]
    fn test_group_generator_has_gid_description() {
        let generator = GroupGenerator::new();
        let result = generator.generate_candidates("").unwrap();
        // At least some groups should have GID in description
        let has_gid = result.iter().any(|c| {
            c.description
                .as_ref()
                .is_some_and(|d| d.starts_with("GID:"))
        });
        assert!(has_gid, "Expected groups to have GID in description");
    }
}
