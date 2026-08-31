use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use anyhow::Result;
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

    /// Create a generator that also offers the service accounts the default
    /// generator hides (below UID 1000 on Linux, below 500 or `_`-prefixed on
    /// macOS)
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

        let mut candidates = load_users(self.include_system_users);

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

/// Whether an account belongs in the default (non-`--all`) candidate list.
///
/// `root` is always offered: it is the account most often typed after `su`,
/// `chown` or `sudo -u`, and it sits below every "first real user" cutoff.
fn is_offered(
    username: &str,
    uid: u32,
    include_system_users: bool,
    first_regular_uid: u32,
) -> bool {
    include_system_users || uid >= first_regular_uid || username == "root"
}

/// The accounts to offer, unsorted.
///
/// `/etc/passwd` is the whole story on a stock Linux box, and reading it costs
/// one `open` where enumerating through NSS would fan out to every configured
/// backend on each keystroke.
#[cfg(not(target_os = "macos"))]
fn load_users(include_system_users: bool) -> Vec<CompletionCandidate> {
    use std::fs;

    // Linux hands the first interactive account UID 1000.
    const FIRST_REGULAR_UID: u32 = 1000;

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

                if let Ok(uid) = parts.get(2).unwrap_or(&"0").parse::<u32>()
                    && is_offered(&username, uid, include_system_users, FIRST_REGULAR_UID)
                {
                    candidates.push(CompletionCandidate::argument(
                        username,
                        gecos.filter(|s| !s.is_empty()),
                    ));
                }
            }
        }
    }

    candidates
}

/// macOS keeps interactive accounts in Open Directory, not in `/etc/passwd`.
///
/// That file is only consulted in single-user mode -- it says so in its own
/// header -- and holds nothing but the `_`-prefixed service accounts, so
/// parsing it offered `root` and nothing else. `getpwent` asks Directory
/// Service and returns the accounts a person would actually type.
#[cfg(target_os = "macos")]
fn load_users(include_system_users: bool) -> Vec<CompletionCandidate> {
    use std::collections::HashSet;
    use std::ffi::CStr;
    use std::sync::Mutex;

    // macOS numbers its service accounts below 500 and gives the first real
    // account 501, so Linux's 1000 would hide every human on the machine.
    const FIRST_REGULAR_UID: u32 = 500;

    /// `getpwent` walks a process-wide cursor and returns a pointer into a
    /// static buffer, so only one caller may be inside the loop at a time.
    static ENUMERATION: Mutex<()> = Mutex::new(());

    let _guard = ENUMERATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    // SAFETY: the lock above serialises the cursor, every pointer is read
    // before the next `getpwent` call invalidates the buffer it points into,
    // and `endpwent` closes the cursor on every path out of the loop.
    unsafe {
        libc::setpwent();

        loop {
            let entry = libc::getpwent();
            if entry.is_null() {
                break;
            }

            let username = CStr::from_ptr((*entry).pw_name)
                .to_string_lossy()
                .into_owned();
            let uid = (*entry).pw_uid;

            // Directory Service can serve the same account from more than one
            // node; `nobody` in particular comes back twice.
            if !seen.insert(username.clone()) {
                continue;
            }

            // Service accounts are `_`-prefixed by convention here, and a
            // handful of them sit above the UID cutoff.
            if !include_system_users && username.starts_with('_') {
                continue;
            }

            if !is_offered(&username, uid, include_system_users, FIRST_REGULAR_UID) {
                continue;
            }

            let gecos = if (*entry).pw_gecos.is_null() {
                None
            } else {
                Some(
                    CStr::from_ptr((*entry).pw_gecos)
                        .to_string_lossy()
                        .into_owned(),
                )
            };

            candidates.push(CompletionCandidate::argument(
                username,
                gecos.filter(|s| !s.is_empty()),
            ));
        }

        libc::endpwent();
    }

    candidates
}

/// Just the account names, for callers that want values rather than candidates.
///
/// Shared so the `chown`/`chgrp` owner provider in `completion::dynamic` reads
/// the same database this generator does; parsing `/etc/passwd` a second time
/// there produced only service accounts on macOS.
pub(crate) fn user_names(include_system_users: bool) -> Vec<String> {
    load_users(include_system_users)
        .into_iter()
        .map(|candidate| candidate.text)
        .collect()
}

impl Default for UserGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name of the account running this test, straight from the same
    /// database the generator reads.
    #[cfg(target_os = "macos")]
    fn current_username() -> String {
        // SAFETY: `getpwuid` returns a pointer into a static buffer that stays
        // valid until the next call in this thread; the name is copied out
        // before anything else can call it.
        unsafe {
            let entry = libc::getpwuid(libc::getuid());
            assert!(!entry.is_null(), "no passwd entry for the current uid");
            std::ffi::CStr::from_ptr((*entry).pw_name)
                .to_string_lossy()
                .into_owned()
        }
    }

    /// The regression this guards is macOS-shaped: interactive accounts live in
    /// Open Directory there, so reading `/etc/passwd` offered `root` and the
    /// service accounts and left out every human on the machine.
    ///
    /// Asking for system users too keeps the assertion about *finding* the
    /// account rather than about where this platform draws its UID cutoff.
    ///
    /// macOS only, because it is only true there: the other branch reads
    /// `/etc/passwd` by choice, so on a host whose accounts come from LDAP or
    /// SSSD the running user is legitimately absent from the list while
    /// `getpwuid`, which goes through NSS, still names them.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_account_running_the_test_is_offered() {
        let me = current_username();
        let candidates = load_users(true);

        assert!(
            candidates.iter().any(|c| c.text == me),
            "{me} is missing from {} candidates",
            candidates.len()
        );
    }

    /// root has no gecos on either platform, but a real account usually does,
    /// and the description is what tells two similar names apart.
    #[test]
    fn the_list_survives_both_cutoffs() {
        let all = load_users(true);
        let default = load_users(false);

        assert!(
            default.len() <= all.len(),
            "the default list ({}) is larger than the full one ({})",
            default.len(),
            all.len()
        );
        assert!(
            default.iter().any(|c| c.text == "root"),
            "root is always offered"
        );
    }

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
