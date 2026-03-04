use crate::completion::cache::CompletionCache;
use crate::completion::fuzzy::fuzzy_match_score;
#[cfg(not(test))]
use crate::completion::notify_completion_update;
use crate::completion::{Candidate, MAX_RESULT};
use anyhow::Result;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

static PATH_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(2000)));

pub fn path_completion_prefix(input: &str) -> Result<Option<String>> {
    let pbuf = PathBuf::from(input);
    let absolute = pbuf.is_absolute();
    let file_name = pbuf.file_name();
    if file_name.is_none() {
        return Ok(None);
    }
    let parent = pbuf.parent();
    let search = input.to_string();

    let paths = if absolute {
        let dir = if let Some(f) = parent {
            f.to_string_lossy().to_string()
        } else {
            input.to_string()
        };
        path_completion_path(PathBuf::from(dir))?
    } else if let Some(dir) = parent {
        if dir.display().to_string().is_empty() {
            // current dir
            path_completion_path(PathBuf::from("."))?
        } else {
            path_completion_path(PathBuf::from(dir))?
        }
    } else {
        path_completion()?
    };

    let mut best_match: Option<(String, i64)> = None;

    for cand in paths.iter() {
        if let Candidate::Path(path) = cand {
            let path_str = path.to_string();

            // Check full path match
            if let Some(mut score) = fuzzy_match_score(&path_str, &search) {
                if path_str.starts_with(&search) {
                    score += 1000;
                }
                match best_match {
                    Some((_, best_score)) if score > best_score => {
                        best_match = Some((path_str.clone(), score));
                    }
                    None => {
                        best_match = Some((path_str.clone(), score));
                    }
                    _ => {}
                }
            }

            // Check stripped path match (relative path)
            if let Ok(striped) = PathBuf::from(path).strip_prefix("./") {
                let striped_str = striped.display().to_string();
                if let Some(mut score) = fuzzy_match_score(&striped_str, &search) {
                    if striped_str.starts_with(&search) {
                        score += 1000;
                    }
                    // Adjust score slightly to prefer shorter/exact matches or keep logic simple?
                    // Verify if better than current best
                    match best_match {
                        Some((_, best_score)) if score > best_score => {
                            best_match = Some((path_str[2..].to_string(), score));
                        }
                        None => {
                            best_match = Some((path_str[2..].to_string(), score));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(best_match.map(|(p, _)| p))
}

pub fn path_completion_prefix_strict(input: &str, only_dirs: bool) -> Result<Option<String>> {
    let pbuf = PathBuf::from(input);
    let absolute = pbuf.is_absolute();
    let _file_name = pbuf.file_name();
    let parent = pbuf.parent();

    // If input is empty or just a dot, suggest from current dir?
    // For ghost text, usually we have some letters.
    // If input ends with '/', parent is the input itself.

    let paths = if absolute {
        let dir = if let Some(f) = parent {
            if input.ends_with(std::path::MAIN_SEPARATOR) {
                input.to_string()
            } else {
                f.to_string_lossy().to_string()
            }
        } else {
            // Root?
            input.to_string()
        };
        path_completion_path(PathBuf::from(dir))?
    } else if let Some(dir) = parent {
        let dir_str = dir.display().to_string();
        if dir_str.is_empty() {
            path_completion_path(PathBuf::from("."))?
        } else {
            // if input ends with separator, we should list contents of directory
            if input.ends_with(std::path::MAIN_SEPARATOR) {
                path_completion_path(PathBuf::from(input))?
            } else {
                path_completion_path(PathBuf::from(dir))?
            }
        }
    } else {
        path_completion()?
    };

    let mut candidates: Vec<String> = Vec::new();
    let search = input.to_string();

    for cand in paths.iter() {
        if let Candidate::Path(path) = cand {
            let path_str = path.to_string();

            // Filter by prefix strictly
            if !path_str.starts_with(&search) {
                // Try handling "./" prefix if input doesn't have it but path does
                if let Ok(stripped) = PathBuf::from(path).strip_prefix("./") {
                    let stripped_str = stripped.display().to_string();
                    if !stripped_str.starts_with(&search) {
                        continue;
                    }
                    // Use striped version? path_completion_path returns relative paths often with ./ ?
                    // Actually path_completion_path implementation joins with dir.
                } else {
                    continue;
                }
            }

            // Filter for directories if requested
            if only_dirs {
                // Candidate::Path doesn't store is_dir directly?
                // Wait, path_completion implementation adds trailing slash for dirs!
                // Let's rely on trailing slash convention used in path_completion_path.
                if !path_str.ends_with(std::path::MAIN_SEPARATOR) {
                    continue;
                }
            }

            candidates.push(path_str);
        }
    }

    // Sort candidates
    // 1. Length (shorter is better/more likely next step)
    // 2. Alphabetical
    // (Frecency integration is omitted for simplicity in this step, relying on default path order or sort)

    candidates.sort_by(|a, b| {
        let len_ord = a.len().cmp(&b.len());
        if len_ord != std::cmp::Ordering::Equal {
            len_ord
        } else {
            a.cmp(b)
        }
    });

    Ok(candidates.first().cloned())
}

fn path_is_dir(path: &PathBuf) -> Result<bool> {
    if let Ok(mut metadata) = path.metadata() {
        if metadata.is_symlink() {
            let link = std::fs::read_link(path)?;
            let relative = link.is_relative();
            if relative {
                metadata = path.join(&link).metadata()?;
            }
        }
        Ok(metadata.is_dir())
    } else {
        Ok(false)
    }
}

pub fn path_completion() -> Result<Vec<Candidate>> {
    let current_dir = std::env::current_dir()?;
    path_completion_path(current_dir)
}

pub fn path_completion_path(path: PathBuf) -> Result<Vec<Candidate>> {
    let path_str = path.display().to_string();

    // Check cache first
    if let Some(hit) = PATH_COMPLETION_CACHE.lookup(&path_str) {
        return Ok(hit.candidates);
    }

    #[cfg(test)]
    {
        // For tests, run synchronously to avoid "no reactor" panic and ensure results are returned immediately
        let candidates = scan_dir_candidates(path.clone())?;
        PATH_COMPLETION_CACHE.set(path_str, candidates.clone());
        Ok(candidates)
    }

    #[cfg(not(test))]
    {
        // Check if pending to avoid duplicate loaded
        if PATH_COMPLETION_CACHE.is_pending(&path_str) {
            // If pending, return empty for now (UI will refresh when ready)
            return Ok(Vec::new());
        }

        // Trigger background load
        // Note: We need to clone path_str for the closure
        let path_str_clone = path_str.clone();
        let path_buf = path.clone();

        PATH_COMPLETION_CACHE.mark_pending(path_str.clone());

        // We assume we are running in a tokio runtime (dsh is tokio::main)
        tokio::spawn(async move {
            // Use spawn_blocking for IO-heavy directory scanning
            let result = tokio::task::spawn_blocking(move || scan_dir_candidates(path_buf)).await;

            match result {
                Ok(Ok(candidates)) => {
                    PATH_COMPLETION_CACHE.set(path_str_clone.clone(), candidates);
                    notify_completion_update();
                }
                Ok(Err(e)) => {
                    // Inner scan error
                    tracing::warn!(
                        "Background path completion failed for '{}': {}",
                        path_str_clone,
                        e
                    );
                }
                Err(e) => {
                    // JoinError
                    tracing::warn!("Background task join error: {}", e);
                }
            }
            PATH_COMPLETION_CACHE.clear_pending(&path_str_clone);
        });

        // Return empty immediately
        Ok(Vec::new())
    }
}

/// Synchronous variant of path_completion_path for explicit user actions (TAB completion).
/// Always returns results immediately, either from cache or by scanning synchronously.
pub fn path_completion_path_sync(path: PathBuf) -> Result<Vec<Candidate>> {
    let path_str = path.display().to_string();

    // Check cache first
    if let Some(hit) = PATH_COMPLETION_CACHE.lookup(&path_str) {
        return Ok(hit.candidates);
    }

    // Scan synchronously and cache
    let candidates = scan_dir_candidates(path)?;
    PATH_COMPLETION_CACHE.set(path_str, candidates.clone());
    Ok(candidates)
}

/// Check if a path exists in the completion cache without triggering a background scan.
/// This is used for syntax highlighting to avoid unnecessary I/O during input processing.
pub fn is_path_cached(path: &Path) -> bool {
    let parent = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => PathBuf::from("."),
        Some(p) => p.to_path_buf(),
        None => return false,
    };

    let parent_str = parent.display().to_string();

    // Only check cache, don't trigger background load
    if let Some(hit) = PATH_COMPLETION_CACHE.lookup(&parent_str) {
        let path_str = path.display().to_string();
        let search = path_str.trim_end_matches(std::path::MAIN_SEPARATOR);

        for cand in hit.candidates {
            if let Candidate::Path(p) = cand {
                let p_clean = p.trim_end_matches(std::path::MAIN_SEPARATOR);
                if p_clean == search {
                    return true;
                }
            }
        }
    }

    false
}

// Synchronous helper moved out for clarity and reuse in background task
fn scan_dir_candidates(path: PathBuf) -> Result<Vec<Candidate>> {
    let path_str = path.display().to_string();
    let exp_str = shellexpand::tilde(&path_str).to_string();
    let expand = path_str != exp_str;

    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .ok_or_else(|| anyhow::Error::msg("HOME environment variable not set"))?;
    let path = PathBuf::from(exp_str);

    let dir = read_dir(&path)?;
    let mut files: Vec<Candidate> = Vec::new();

    for entry in dir.flatten() {
        let entry_path = entry.path();
        let is_dir = path_is_dir(&entry_path)?;
        if expand {
            if let Ok(part) = entry_path.strip_prefix(&home) {
                let mut pb = PathBuf::new();
                pb.push("~/");
                pb.push(part);
                let mut path = pb.display().to_string();
                if is_dir {
                    path += "/";
                }
                files.push(Candidate::Path(path));
            }
        } else {
            let mut path = entry_path.display().to_string();
            if is_dir {
                path += "/";
            }
            files.push(Candidate::Path(path));
        }
        if files.len() >= MAX_RESULT {
            break;
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn test_path_completion_prefix_fuzzy() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();

        // Create files: apple.txt, application.rs, banana.md
        File::create(dir_path.join("apple.txt")).unwrap();
        File::create(dir_path.join("application.rs")).unwrap();
        File::create(dir_path.join("banana.md")).unwrap();

        // Construct paths for input
        let dir_str = dir_path.to_str().unwrap();

        // Case 1: "apl" -> "apple.txt"
        let input_apl = format!("{}/apl", dir_str);
        let result_apl = path_completion_prefix(&input_apl).unwrap();

        assert!(result_apl.is_some());
        let val = result_apl.unwrap();
        assert!(
            val.ends_with("apple.txt"),
            "Expected apple.txt, got {}",
            val
        );

        // Case 2: Exact prefix has higher priority than pure fuzzy match.
        // "aleph.txt" is fuzzy matched by "ap".
        File::create(dir_path.join("aleph.txt")).unwrap();
        let input_ap = format!("{}/ap", dir_str);
        let result_ap = path_completion_prefix(&input_ap).unwrap();

        assert!(result_ap.is_some());
        let val_ap = result_ap.unwrap();
        assert!(
            val_ap.ends_with("apple.txt") || val_ap.ends_with("application.rs"),
            "Expected exact prefix match to have higher priority, got {}",
            val_ap
        );
    }
}
