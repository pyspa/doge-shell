use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::sync::LazyLock;
use std::time::Duration;

// Cache TTL for group list (5 seconds - groups don't change often)
const GROUP_CACHE_TTL_MS: u64 = 5000;

static GROUP_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(GROUP_CACHE_TTL_MS)));

/// Generator for system group name completion
pub struct GroupGenerator;

/// One offered group. The GID is what tells two similar names apart.
fn candidate(groupname: String, gid: Option<u32>) -> CompletionCandidate {
    CompletionCandidate::argument(groupname, gid.map(|id| format!("GID: {}", id)))
}

/// The groups to offer, unsorted.
///
/// `/etc/group` is the whole story on a stock Linux box, and reading it costs
/// one `open` where enumerating through NSS would fan out to every configured
/// backend on each keystroke.
#[cfg(not(target_os = "macos"))]
fn load_groups() -> Vec<CompletionCandidate> {
    use std::fs;

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
                candidates.push(candidate(groupname, gid));
            }
        }
    }

    candidates
}

/// macOS answers group lookups from Open Directory.
///
/// Unlike `/etc/passwd`, the flat `/etc/group` here is populated -- `wheel`,
/// `staff` and `admin` are all in it -- so this is not the empty-file problem
/// that `user.rs` documents. What the file misses is everything Open Directory
/// adds on top: groups an MDM profile or `dseditgroup` created. `getgrent` asks
/// Directory Service and so returns a superset of the file, which also keeps
/// this generator reading its database the same way its `user.rs` sibling does.
#[cfg(target_os = "macos")]
fn load_groups() -> Vec<CompletionCandidate> {
    use std::collections::HashSet;
    use std::ffi::CStr;
    use std::sync::Mutex;

    /// `getgrent` walks a process-wide cursor and returns a pointer into a
    /// static buffer, so only one caller may be inside the loop at a time.
    static ENUMERATION: Mutex<()> = Mutex::new(());

    let _guard = ENUMERATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    // SAFETY: the lock above serialises the cursor, `gr_name` is read before
    // the next `getgrent` call invalidates the buffer it points into, and
    // `endgrent` closes the cursor on the single path out of the loop.
    unsafe {
        libc::setgrent();

        loop {
            let entry = libc::getgrent();
            if entry.is_null() {
                break;
            }

            let groupname = CStr::from_ptr((*entry).gr_name)
                .to_string_lossy()
                .into_owned();

            // Directory Service can serve the same group from more than one
            // node, so the local and the directory copy both arrive.
            if seen.insert(groupname.clone()) {
                candidates.push(candidate(groupname, Some((*entry).gr_gid)));
            }
        }

        libc::endgrent();
    }

    candidates
}

/// Just the group names, for callers that want values rather than candidates.
///
/// Shared so the `chown`/`chgrp` group provider in `completion::dynamic` reads
/// the same database this generator does, the way the owner side already reads
/// [`super::user::user_names`].
pub(crate) fn group_names() -> Vec<String> {
    load_groups()
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
}

impl GroupGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        // Check cache first
        if let Some(cached) = GROUP_CACHE.get_entry("") {
            return Ok(self.filter_candidates(&cached, current_token));
        }

        let mut candidates = load_groups();

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

    /// The primary group of the process running this test, straight from the
    /// same database the generator reads.
    ///
    /// macOS only, because it is only true there: the other branch reads
    /// `/etc/group` by choice, so on a host whose groups come from LDAP the
    /// running group is legitimately absent from the file while `getgrgid`,
    /// which goes through NSS, still names it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_primary_group_of_the_test_process_is_offered() {
        // SAFETY: `getgrgid` returns a pointer into a static buffer that stays
        // valid until the next call in this thread; the name is copied out
        // before anything else can call it.
        let mine = unsafe {
            let entry = libc::getgrgid(libc::getgid());
            assert!(!entry.is_null(), "no group entry for the current gid");
            std::ffi::CStr::from_ptr((*entry).gr_name)
                .to_string_lossy()
                .into_owned()
        };

        let candidates = load_groups();
        assert!(
            candidates.iter().any(|c| c.text == mine),
            "{mine} is missing from {} candidates",
            candidates.len()
        );
    }
}
