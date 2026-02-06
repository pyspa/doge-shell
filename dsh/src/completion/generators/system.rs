use crate::completion::cache::CompletionCache;
use crate::completion::command::CompletionCandidate;
use crate::completion::fuzzy_match_score;
use crate::dirs::is_executable;
use anyhow::Result;
use parking_lot::RwLock;
use std::collections::{BTreeSet, HashSet};
use std::fs::read_dir;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

const SYSTEM_COMMAND_CACHE_TTL_MS: u64 = 2000;
static SYSTEM_COMMAND_CACHE: LazyLock<CompletionCache<CompletionCandidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(SYSTEM_COMMAND_CACHE_TTL_MS)));

static GLOBAL_SYSTEM_COMMANDS: LazyLock<Arc<RwLock<Option<BTreeSet<String>>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

static LAST_CACHE_UPDATE: LazyLock<Arc<RwLock<Option<Instant>>>> =
    LazyLock::new(|| Arc::new(RwLock::new(None)));

static GLOBAL_CACHE_INFLIGHT: LazyLock<AtomicBool> = LazyLock::new(|| AtomicBool::new(false));

pub fn set_global_system_commands(commands: BTreeSet<String>) {
    let mut guard = GLOBAL_SYSTEM_COMMANDS.write();
    *guard = Some(commands);
    let mut time_guard = LAST_CACHE_UPDATE.write();
    *time_guard = Some(Instant::now());
}

pub fn clear_global_system_commands() {
    let mut guard = GLOBAL_SYSTEM_COMMANDS.write();
    *guard = None;
    let mut time_guard = LAST_CACHE_UPDATE.write();
    *time_guard = None;
    GLOBAL_CACHE_INFLIGHT.store(false, Ordering::SeqCst);
}

// 30 seconds TTL for global command cache
const GLOBAL_CACHE_TTL_SECS: u64 = 30;

fn ensure_global_cache_populated() {
    let should_refresh = {
        let guard = GLOBAL_SYSTEM_COMMANDS.read();
        if guard.is_none() {
            true
        } else {
            // Check if expired
            let time_guard = LAST_CACHE_UPDATE.read();
            if let Some(last_update) = *time_guard {
                last_update.elapsed() > Duration::from_secs(GLOBAL_CACHE_TTL_SECS)
            } else {
                true
            }
        }
    };

    if !should_refresh {
        return;
    }

    if GLOBAL_CACHE_INFLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    std::thread::spawn(|| {
        let paths: Vec<std::path::PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();

        let mut commands = BTreeSet::new();
        for path in paths {
            if let Ok(entries) = read_dir(&path) {
                for entry in entries.flatten() {
                    if let Ok(ft) = entry.file_type()
                        && !ft.is_file()
                        && !ft.is_symlink()
                    {
                        continue;
                    }
                    if is_executable(&entry)
                        && let Some(name) = entry.file_name().to_str()
                    {
                        commands.insert(name.to_string());
                    }
                }
            }
        }

        let mut guard = GLOBAL_SYSTEM_COMMANDS.write();
        *guard = Some(commands);
        let mut time_guard = LAST_CACHE_UPDATE.write();
        *time_guard = Some(Instant::now());
        GLOBAL_CACHE_INFLIGHT.store(false, Ordering::SeqCst);
    });
}

pub struct SystemCommandGenerator;

impl Default for SystemCommandGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemCommandGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_candidates(&self, current_token: &str) -> Result<Vec<CompletionCandidate>> {
        if !current_token.is_empty()
            && let Some(hit) = SYSTEM_COMMAND_CACHE.lookup(current_token)
        {
            return Ok(hit.candidates);
        }

        let mut candidates = Vec::with_capacity(32);
        let mut seen_names: HashSet<String> = HashSet::new();

        if (current_token.starts_with('/') || current_token.starts_with("./"))
            && Path::new(current_token).is_file()
        {
            candidates.push(CompletionCandidate::subcommand(
                current_token.to_string(),
                None,
            ));
            seen_names.insert(current_token.to_string());
        }

        // Try global cache first
        ensure_global_cache_populated();

        let cache_hit = {
            let guard = GLOBAL_SYSTEM_COMMANDS.read();
            if let Some(commands) = &*guard {
                // Filter from cache - BTreeSet maintains sorted order
                for cmd in commands
                    .iter()
                    .filter(|cmd| fuzzy_match_score(cmd, current_token).is_some())
                {
                    if candidates.len() >= crate::completion::MAX_RESULT {
                        break;
                    }
                    if seen_names.insert(cmd.to_string()) {
                        candidates.push(CompletionCandidate::subcommand(cmd.to_string(), None));
                    }
                }
                true
            } else {
                false
            }
        };

        if !cache_hit {
            // Fallback to synchronous scan if cache not ready
            let paths: Vec<std::path::PathBuf> = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default();

            for path in paths {
                if candidates.len() >= crate::completion::MAX_RESULT {
                    break;
                }

                if let Ok(entries) = read_dir(&path) {
                    let mut local_candidates: Vec<String> = Vec::new();

                    for entry in entries.flatten() {
                        let file_name_os = entry.file_name();
                        let Some(file_name) = file_name_os.to_str() else {
                            continue;
                        };

                        if fuzzy_match_score(file_name, current_token).is_none() {
                            continue;
                        }

                        if seen_names.contains(file_name) {
                            continue;
                        }

                        if let Ok(ft) = entry.file_type()
                            && !ft.is_file()
                            && !ft.is_symlink()
                        {
                            continue;
                        }

                        if is_executable(&entry) {
                            local_candidates.push(file_name.to_string());
                        }
                    }

                    local_candidates.sort();
                    for cmd in local_candidates {
                        if candidates.len() >= crate::completion::MAX_RESULT {
                            break;
                        }
                        if seen_names.insert(cmd.clone()) {
                            candidates.push(CompletionCandidate::subcommand(cmd, None));
                        }
                    }
                }
            }
        }

        if !current_token.is_empty() {
            SYSTEM_COMMAND_CACHE.set(current_token.to_string(), candidates.clone());
        }

        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn system_command_candidates_scan_path() {
        static PATH_LOCK: LazyLock<parking_lot::Mutex<()>> =
            LazyLock::new(|| parking_lot::Mutex::new(()));
        let _guard = PATH_LOCK.lock();

        let dir = tempdir().unwrap();
        let cmd_path = dir.path().join("my-test-cmd");
        std::fs::write(&cmd_path, "").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&cmd_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&cmd_path, perms).unwrap();
        }

        let old_path = std::env::var_os("PATH");
        let new_path = match &old_path {
            Some(p) => format!("{}:{}", dir.path().display(), p.to_string_lossy()),
            None => dir.path().display().to_string(),
        };
        unsafe { std::env::set_var("PATH", &new_path) };

        // Use cached global system commands if previously set?
        // We probably want to clear it to force a scan, or rely on internal logic.
        // ensure_global_cache_populated uses a static cache.
        // If it's already populated, it won't re-scan unless expired.
        // Let's force clear for the test.
        clear_global_system_commands();

        let generator = SystemCommandGenerator::new();
        let candidates = generator
            .generate_candidates("my-")
            .expect("system candidates");
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        assert!(
            texts.contains(&"my-test-cmd".to_string()),
            "expected PATH command in {:?}",
            texts
        );

        if let Some(p) = old_path {
            unsafe { std::env::set_var("PATH", p) };
        } else {
            unsafe { std::env::remove_var("PATH") };
        }
    }
}
