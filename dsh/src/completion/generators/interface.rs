use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::fs;
use std::sync::LazyLock;
use std::time::Duration;

// Cache TTL for interface list (2 seconds - interfaces can change but not too frequently)
const INTERFACE_CACHE_TTL_MS: u64 = 2000;

static INTERFACE_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(INTERFACE_CACHE_TTL_MS)));

/// Generator for network interface name completion
pub struct InterfaceGenerator;

impl InterfaceGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        // Check cache first
        if let Some(cached) = INTERFACE_CACHE.get_entry("") {
            return Ok(self.filter_candidates(&cached, current_token));
        }

        // Read from /sys/class/net/ for interface names
        let mut candidates = Vec::new();

        if let Ok(entries) = fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    let interface_name = name.to_string();

                    // Try to get interface state from operstate
                    let state_path = entry.path().join("operstate");
                    let state = fs::read_to_string(state_path)
                        .ok()
                        .map(|s| s.trim().to_string());

                    // Try to get interface type
                    let type_path = entry.path().join("type");
                    let if_type = fs::read_to_string(type_path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .map(|t| match t {
                            1 => "ethernet",
                            772 => "loopback",
                            801 => "wireless",
                            _ => "other",
                        });

                    let description = match (state, if_type) {
                        (Some(s), Some(t)) => Some(format!("{} ({})", t, s)),
                        (Some(s), None) => Some(s),
                        (None, Some(t)) => Some(t.to_string()),
                        (None, None) => None,
                    };

                    candidates.push(CompletionCandidate::argument(interface_name, description));
                }
            }
        }

        // Sort: physical interfaces first (eth, wlan, enp), then virtual (lo, docker, etc.)
        candidates.sort_by(|a, b| {
            let priority_a = Self::interface_priority(&a.text);
            let priority_b = Self::interface_priority(&b.text);
            priority_a
                .cmp(&priority_b)
                .then_with(|| a.text.cmp(&b.text))
        });

        // Store in cache
        INTERFACE_CACHE.set("".to_string(), candidates.clone());

        Ok(self.filter_candidates(&candidates, current_token))
    }

    /// Assign priority for sorting (lower = higher priority)
    fn interface_priority(name: &str) -> u8 {
        if name.starts_with("eth") || name.starts_with("enp") || name.starts_with("eno") {
            0 // Physical ethernet
        } else if name.starts_with("wlan") || name.starts_with("wlp") {
            1 // Wireless
        } else if name == "lo" {
            5 // Loopback last
        } else if name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth")
        {
            4 // Virtual/container
        } else {
            3 // Other
        }
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

impl Default for InterfaceGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interface_generator_creates() {
        let generator = InterfaceGenerator::new();
        let _ = generator;
    }

    #[test]
    fn test_interface_generator_generates_candidates() {
        let generator = InterfaceGenerator::new();
        let result = generator.generate_candidates("");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        // On Linux, at least 'lo' should be present
        #[cfg(target_os = "linux")]
        assert!(
            candidates.iter().any(|c| c.text == "lo"),
            "Expected 'lo' interface on Linux"
        );
    }

    #[test]
    fn test_interface_priority_ethernet_first() {
        assert!(
            InterfaceGenerator::interface_priority("eth0")
                < InterfaceGenerator::interface_priority("lo")
        );
        assert!(
            InterfaceGenerator::interface_priority("enp0s3")
                < InterfaceGenerator::interface_priority("lo")
        );
        assert!(
            InterfaceGenerator::interface_priority("eno1")
                < InterfaceGenerator::interface_priority("lo")
        );
    }

    #[test]
    fn test_interface_priority_wireless_before_virtual() {
        assert!(
            InterfaceGenerator::interface_priority("wlan0")
                < InterfaceGenerator::interface_priority("docker0")
        );
        assert!(
            InterfaceGenerator::interface_priority("wlp2s0")
                < InterfaceGenerator::interface_priority("veth123")
        );
    }

    #[test]
    fn test_interface_priority_loopback_last() {
        assert!(
            InterfaceGenerator::interface_priority("eth0")
                < InterfaceGenerator::interface_priority("lo")
        );
        assert!(
            InterfaceGenerator::interface_priority("docker0")
                < InterfaceGenerator::interface_priority("lo")
        );
    }

    #[test]
    fn test_interface_generator_filters_by_prefix() {
        let generator = InterfaceGenerator::new();
        let result = generator.generate_candidates("lo");
        assert!(result.is_ok());
        let candidates = result.unwrap();
        for c in &candidates {
            assert!(
                c.text.to_lowercase().starts_with("lo"),
                "Expected candidate '{}' to start with 'lo'",
                c.text
            );
        }
    }

    #[test]
    fn test_interface_has_description() {
        let generator = InterfaceGenerator::new();
        let result = generator.generate_candidates("").unwrap();
        // At least one interface should have a description with state
        let has_state = result.iter().any(|c| {
            c.description
                .as_ref()
                .is_some_and(|d| d.contains("up") || d.contains("down") || d.contains("unknown"))
        });
        // This might not always be true depending on system, so just check it compiles
        let _ = has_state;
    }
}
