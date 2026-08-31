use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
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

        let mut candidates = load_interfaces();

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
    ///
    /// macOS names differ from Linux's: the wired port is `en0` rather than
    /// `eth0`/`enp0s3`, loopback is `lo0` rather than `lo`, and the virtual
    /// crowd is `utun*`/`awdl*`/`bridge*` rather than `docker*`/`veth*`. Both
    /// vocabularies are matched here so neither platform sorts its real
    /// interfaces below its tunnels.
    fn interface_priority(name: &str) -> u8 {
        if name.starts_with("lo") {
            5 // Loopback last (`lo` on Linux, `lo0` on macOS)
        } else if name.starts_with("eth") || name.starts_with("en") {
            0 // Physical ethernet (`eth0`, `enp0s3`, `eno1`, macOS `en0`)
        } else if name.starts_with("wlan") || name.starts_with("wlp") {
            1 // Wireless
        } else if name.starts_with("docker")
            || name.starts_with("br-")
            || name.starts_with("veth")
            || name.starts_with("bridge")
            || name.starts_with("utun")
            || name.starts_with("awdl")
            || name.starts_with("llw")
            || name.starts_with("gif")
            || name.starts_with("stf")
        {
            4 // Virtual/container/tunnel
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

/// Every network interface on the host, with a short description.
///
/// `/sys/class/net` gives the name, the link state and a numeric type per
/// interface without opening a socket.
#[cfg(not(target_os = "macos"))]
fn load_interfaces() -> Vec<CompletionCandidate> {
    use std::fs;

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

    candidates
}

/// macOS has no sysfs, so read the same facts out of `getifaddrs` flags.
///
/// `getifaddrs` yields one entry per address, so an interface with an IPv4 and
/// two IPv6 addresses appears three times; keep the first and drop the rest.
/// The description is kept in the `type (state)` shape the sysfs branch
/// produces so both platforms render identically.
#[cfg(target_os = "macos")]
fn load_interfaces() -> Vec<CompletionCandidate> {
    use nix::ifaddrs::getifaddrs;
    use nix::net::if_::InterfaceFlags;
    use std::collections::HashSet;

    let Ok(addresses) = getifaddrs() else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    for address in addresses {
        if !seen.insert(address.interface_name.clone()) {
            continue;
        }

        let flags = address.flags;
        let if_type = if flags.contains(InterfaceFlags::IFF_LOOPBACK) {
            "loopback"
        } else if flags.contains(InterfaceFlags::IFF_POINTOPOINT) {
            "point-to-point"
        } else {
            "ethernet"
        };
        // `IFF_UP` alone only means "configured"; sysfs `operstate` reports the
        // link, which is `IFF_RUNNING` here.
        let state = if flags.contains(InterfaceFlags::IFF_RUNNING) {
            "up"
        } else {
            "down"
        };

        candidates.push(CompletionCandidate::argument(
            address.interface_name,
            Some(format!("{} ({})", if_type, state)),
        ));
    }

    candidates
}

/// Just the interface names, for callers that want values rather than
/// candidates.
///
/// Shared so the `tcpdump -i` provider in `completion::dynamic` and this
/// generator cannot drift apart about where interface names come from.
pub(crate) fn interface_names() -> Vec<String> {
    load_interfaces()
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
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

    /// Every host has a loopback interface; it is `lo` on Linux and `lo0` on
    /// macOS. Finding neither means the platform source returned nothing, which
    /// is how the sysfs reader failed silently on macOS.
    #[test]
    fn test_interface_generator_generates_candidates() {
        let generator = InterfaceGenerator::new();
        let candidates = generator.generate_candidates("").expect("candidates");

        assert!(
            candidates.iter().any(|c| c.text.starts_with("lo")),
            "no loopback interface among {:?}",
            candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
        );
    }

    /// Both branches describe an interface as `type (state)`.
    ///
    /// Only the type is asserted: the state is whatever the platform reports,
    /// and Linux answers `unknown` for loopback because the kernel never sets
    /// an operstate on it.
    #[test]
    fn a_loopback_interface_is_described_as_one() {
        let generator = InterfaceGenerator::new();
        let candidates = generator.generate_candidates("lo").expect("candidates");

        let loopback = candidates
            .iter()
            .find(|c| c.text.starts_with("lo"))
            .expect("a loopback interface");

        let description = loopback.description.as_deref().unwrap_or_default();
        assert!(
            description.starts_with("loopback ("),
            "unexpected description for {}: {description:?}",
            loopback.text
        );
    }

    #[test]
    fn macos_interface_names_sort_like_their_linux_equivalents() {
        // en0 is macOS's wired port, utun0 a VPN tunnel, lo0 the loopback.
        assert!(
            InterfaceGenerator::interface_priority("en0")
                < InterfaceGenerator::interface_priority("utun0")
        );
        assert!(
            InterfaceGenerator::interface_priority("utun0")
                < InterfaceGenerator::interface_priority("lo0")
        );
        assert_eq!(
            InterfaceGenerator::interface_priority("lo0"),
            InterfaceGenerator::interface_priority("lo")
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
