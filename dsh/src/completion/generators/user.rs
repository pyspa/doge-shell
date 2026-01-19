use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::fs;
use std::sync::LazyLock;
use std::time::Duration;

// Cache TTL for user list (5 seconds - users don't change often)
const USER_CACHE_TTL_MS: u64 = 5000;

static USER_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(USER_CACHE_TTL_MS)));

/// Generator for system user name completion
pub struct UserGenerator {
    include_system_users: bool,
}

impl UserGenerator {
    pub fn new() -> Self {
        Self {
            include_system_users: false,
        }
    }

    /// Create a generator that includes system users (UID < 1000)
    pub fn with_system_users() -> Self {
        Self {
            include_system_users: true,
        }
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        // Cache key based on whether we include system users
        let cache_key = if self.include_system_users {
            "all"
        } else {
            "normal"
        };

        // Check cache first
        if let Some(cached) = USER_CACHE.get_entry(cache_key) {
            return Ok(self.filter_candidates(&cached, current_token));
        }

        // Parse /etc/passwd for user names
        let mut candidates = Vec::new();

        if let Ok(content) = fs::read_to_string("/etc/passwd") {
            for line in content.lines() {
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                // Format: username:x:uid:gid:gecos:home:shell
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() >= 5 {
                    let username = parts[0].to_string();
                    let gecos = parts.get(4).map(|s| s.to_string());

                    // Filter out system users (UID < 1000) unless requested
                    // but always include root
                    if let Ok(uid) = parts.get(2).unwrap_or(&"0").parse::<u32>() {
                        let include =
                            self.include_system_users || uid >= 1000 || username == "root";

                        if include {
                            candidates.push(CompletionCandidate::argument(
                                username,
                                gecos.filter(|s| !s.is_empty()),
                            ));
                        }
                    }
                }
            }
        }

        // Sort alphabetically
        candidates.sort_by(|a, b| a.text.cmp(&b.text));

        // Store in cache
        USER_CACHE.set(cache_key.to_string(), candidates.clone());

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

impl Default for UserGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_generator_creates() {
        let generator = UserGenerator::new();
        assert!(!generator.include_system_users);

        let generator_all = UserGenerator::with_system_users();
        assert!(generator_all.include_system_users);
    }

    #[test]
    fn test_user_generator_generates_candidates() {
        let generator = UserGenerator::new();
        let result = generator.generate_candidates("");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        // On Linux/macOS, root should be present
        assert!(
            candidates.iter().any(|c| c.text == "root"),
            "Expected 'root' user in candidates"
        );
    }

    #[test]
    fn test_user_generator_filters_by_prefix() {
        let generator = UserGenerator::new();
        let result = generator.generate_candidates("ro");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        // All candidates should start with "ro"
        for c in &candidates {
            assert!(
                c.text.to_lowercase().starts_with("ro"),
                "Expected candidate '{}' to start with 'ro'",
                c.text
            );
        }
    }

    #[test]
    fn test_user_generator_case_insensitive() {
        let generator = UserGenerator::new();
        let lower = generator.generate_candidates("ro").unwrap();
        let upper = generator.generate_candidates("RO").unwrap();
        // Should return same results regardless of case
        assert_eq!(lower.len(), upper.len());
    }

    #[test]
    fn test_user_generator_no_match() {
        let generator = UserGenerator::new();
        let result = generator.generate_candidates("zzzznonexistent");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        // Very unlikely to have a user starting with "zzzznonexistent"
        assert!(candidates.is_empty() || candidates.iter().any(|c| c.text.starts_with("zzzz")));
    }
}
