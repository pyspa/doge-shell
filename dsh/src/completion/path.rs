//! Directory listings retain the request's lookup location and source spelling.
//! Cache entries describe one exact directory, never a parent-prefix result.
use crate::completion::Candidate;
use crate::completion::cache::CompletionCache;
use crate::completion::fuzzy::fuzzy_rank;
use crate::completion::generators::filesystem::FileSystemContext;
#[cfg(not(test))]
use crate::completion::notify_completion_update;
use crate::completion::shell_path::{format_path_for_token, normalize_path_token};
use anyhow::Result;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

static PATH_COMPLETION_CACHE: LazyLock<CompletionCache<Candidate>> =
    LazyLock::new(|| CompletionCache::new(Duration::from_millis(2000)));

#[cfg(test)]
mod shared_tests;

struct DirectoryRequest {
    key: String,
    lookup: PathBuf,
    display: PathBuf,
}

impl DirectoryRequest {
    fn new(path: PathBuf) -> Result<Self> {
        Self::with_context(path, None)
    }

    fn with_context(path: PathBuf, context: Option<&FileSystemContext>) -> Result<Self> {
        let spelling = path.to_string_lossy();
        let expanded = context.map_or_else(
            || PathBuf::from(shellexpand::tilde(&spelling).as_ref()),
            |context| {
                context.lookup_directory(&spelling, spelling == "~" || spelling.starts_with("~/"))
            },
        );
        // Resolve before background work is queued: a later cd must neither
        // change the scan target nor reuse relative results from another cwd.
        let lookup = if expanded.is_absolute() {
            expanded
        } else {
            std::env::current_dir()?.join(expanded)
        };
        // The same directory can be requested as '.', './', an absolute path,
        // or '~/...'. Keep display provenance separate from lookup identity.
        let key = format!("{spelling}\0{}", lookup.display());
        Ok(Self {
            key,
            lookup,
            display: path,
        })
    }
}

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
            if let Some(score) = fuzzy_rank(&path_str, &search) {
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
                if let Some(score) = fuzzy_rank(&striped_str, &search) {
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
    path_completion_prefix_strict_with_context(input, only_dirs, None)
}

fn path_completion_prefix_strict_with_context(
    input: &str,
    only_dirs: bool,
    context: Option<&FileSystemContext>,
) -> Result<Option<String>> {
    let path = Path::new(input);
    // Path::parent("dir/") is empty. A trailing separator means list the
    // directory itself, before considering a parent or the cwd fallback.
    let directory = if input.ends_with(std::path::MAIN_SEPARATOR) {
        path
    } else {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    };
    let paths = path_completion_path_with_context(directory.to_path_buf(), context)?;

    // Read the shared listing once and retain only a borrowed winner. Ghost
    // completion runs on every keystroke: cloning all paths and sorting the
    // matches makes even a warm cache expensive in large directories.
    let best = paths
        .iter()
        .filter_map(|candidate| {
            let Candidate::Path(path) = candidate else {
                return None;
            };
            if only_dirs && !path.ends_with(std::path::MAIN_SEPARATOR) {
                return None;
            }
            if path.starts_with(input) {
                Some(path.as_str())
            } else {
                // A bare 'fo' is listed as './foo'; preserve the spelling that
                // matched so the result can be appended to the user's input.
                path.strip_prefix("./")
                    .filter(|path| path.starts_with(input))
            }
        })
        .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));

    Ok(best.map(str::to_owned))
}

pub(crate) fn path_completion_prefix_for_shell_token(
    raw_token: &str,
    only_dirs: bool,
    context: Option<&FileSystemContext>,
) -> Result<Option<String>> {
    if raw_token.is_empty() {
        return Ok(None);
    }

    let normalized = normalize_path_token(raw_token);
    // Preserve source provenance before the listing cache expands an unquoted
    // home prefix. A quoted/escaped tilde names a local directory instead.
    let literal_home =
        !raw_token.starts_with("~/") && (normalized == "~" || normalized.starts_with("~/"));
    let lookup = if literal_home {
        format!("./{normalized}")
    } else {
        normalized.clone()
    };
    let Some(candidate) = path_completion_prefix_strict_with_context(&lookup, only_dirs, context)?
    else {
        return Ok(None);
    };
    let candidate = if literal_home {
        candidate
            .strip_prefix("./")
            .unwrap_or(&candidate)
            .to_string()
    } else {
        candidate
    };

    if candidate.len() <= normalized.len() || !candidate.starts_with(&normalized) {
        return Ok(None);
    }

    Ok(Some(format_path_for_token(&candidate, raw_token)))
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
    Ok(path_completion_path_with_context(path, None)?
        .as_ref()
        .clone())
}

fn path_completion_path_with_context(
    path: PathBuf,
    context: Option<&FileSystemContext>,
) -> Result<Arc<Vec<Candidate>>> {
    let request = DirectoryRequest::with_context(path, context)?;

    // Check cache first
    if let Some(candidates) = PATH_COMPLETION_CACHE.get_entry(&request.key) {
        return Ok(candidates);
    }

    #[cfg(test)]
    {
        // For tests, run synchronously to avoid "no reactor" panic and ensure results are returned immediately
        let key = request.key.clone();
        let candidates = scan_dir_candidates(request)?;
        PATH_COMPLETION_CACHE.set(key, candidates.clone());
        Ok(Arc::new(candidates))
    }

    #[cfg(not(test))]
    {
        // Check if pending to avoid duplicate loaded
        if !PATH_COMPLETION_CACHE.mark_pending(request.key.clone()) {
            // If pending, return empty for now (UI will refresh when ready)
            return Ok(Arc::default());
        }

        // Trigger background load
        // Note: We need to clone path_str for the closure
        let path_str_clone = request.key.clone();

        // We assume we are running in a tokio runtime (dsh is tokio::main)
        tokio::spawn(async move {
            // Use spawn_blocking for IO-heavy directory scanning
            let result = tokio::task::spawn_blocking(move || scan_dir_candidates(request)).await;

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
        Ok(Arc::default())
    }
}

/// Synchronous variant of path_completion_path for explicit user actions (TAB completion).
/// Always returns results immediately, either from cache or by scanning synchronously.
pub fn path_completion_path_sync(path: PathBuf) -> Result<Vec<Candidate>> {
    let request = DirectoryRequest::new(path)?;

    // Check cache first
    if let Some(candidates) = PATH_COMPLETION_CACHE.get_entry(&request.key) {
        return Ok(candidates.as_ref().clone());
    }

    // Scan synchronously and cache
    let key = request.key.clone();
    let candidates = scan_dir_candidates(request)?;
    PATH_COMPLETION_CACHE.set(key, candidates.clone());
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

    let Ok(request) = DirectoryRequest::new(parent) else {
        return false;
    };

    // Only check cache, don't trigger background load.
    //
    // `get_entry` rather than `lookup`: this is a membership test, and `lookup`
    // both walks every byte prefix of the key and clones the whole candidate Vec
    // (one String per directory entry). This runs once per argument-ish highlight range
    // on every keystroke, so the clone is the dominant cost. The prefix walk is
    // also wrong for directory keys — it can match an unrelated directory whose
    // path happens to be a prefix of this one (`/home/ma` vs `/home/ma2`).
    if let Some(candidates) = PATH_COMPLETION_CACHE.get_entry(&request.key) {
        let Some(name) = path.file_name() else {
            return false;
        };
        // A bare 'foo' is listed under '.' as './foo'. Compare in the
        // listing's display spelling rather than losing that provenance.
        let path_str = request.display.join(name).display().to_string();
        let search = path_str.trim_end_matches(std::path::MAIN_SEPARATOR);

        for cand in candidates.iter() {
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

pub(crate) fn is_path_cached_for_shell_token(raw_token: &str) -> bool {
    if raw_token.is_empty() {
        return false;
    }

    let normalized = normalize_path_token(raw_token);
    if normalized.is_empty() {
        return false;
    }

    is_path_cached(Path::new(&normalized))
}

// Synchronous helper moved out for clarity and reuse in background task
fn scan_dir_candidates(request: DirectoryRequest) -> Result<Vec<Candidate>> {
    let dir = read_dir(&request.lookup)?;
    let mut files: Vec<Candidate> = Vec::new();

    for entry in dir.flatten() {
        let entry_path = entry.path();
        let is_dir = path_is_dir(&entry_path)?;
        let mut path = request
            .display
            .join(entry.file_name())
            .display()
            .to_string();
        if is_dir {
            path += "/";
        }
        files.push(Candidate::Path(path));
        // Keep the complete listing: callers filter by name/type first, and
        // the integrated engine limits the ranked matches, not arbitrary entries.
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
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

    #[test]
    fn shell_token_path_prefix_preserves_escaped_space_style() {
        let dir = tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        fs::create_dir(&spaced).unwrap();
        File::create(spaced.join("foo.txt")).unwrap();

        let raw_prefix = format!("{}/dir\\ with\\ space/fo", dir.path().display());
        let expected = format!("{}/dir\\ with\\ space/foo.txt", dir.path().display());

        assert_eq!(
            path_completion_prefix_for_shell_token(&raw_prefix, false, None).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn shell_token_path_prefix_preserves_quote_style() {
        let dir = tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        fs::create_dir(&spaced).unwrap();
        File::create(spaced.join("foo.txt")).unwrap();

        let raw_prefix = format!("\"{}/dir with space/fo", dir.path().display());
        let expected = format!("\"{}/dir with space/foo.txt", dir.path().display());

        assert_eq!(
            path_completion_prefix_for_shell_token(&raw_prefix, false, None).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn shell_token_path_prefix_directory_only_filters_files() {
        let dir = tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        fs::create_dir(&spaced).unwrap();
        File::create(spaced.join("foo.txt")).unwrap();
        fs::create_dir(spaced.join("foodir")).unwrap();

        let raw_prefix = format!("'{}/dir with space/fo", dir.path().display());
        let expected = format!("'{}/dir with space/foodir/", dir.path().display());

        assert_eq!(
            path_completion_prefix_for_shell_token(&raw_prefix, true, None).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn is_path_cached_for_shell_token_decodes_escaped_and_quoted_tokens() {
        let dir = tempdir().unwrap();
        let spaced = dir.path().join("dir with space");
        fs::create_dir(&spaced).unwrap();
        File::create(spaced.join("foo.txt")).unwrap();
        path_completion_path_sync(spaced.clone()).unwrap();

        let escaped = format!("{}/dir\\ with\\ space/foo.txt", dir.path().display());
        let double = format!("\"{}/dir with space/foo.txt\"", dir.path().display());
        let single = format!("'{}/dir with space/foo.txt'", dir.path().display());
        let unclosed = format!("\"{}/dir with space/foo.txt", dir.path().display());
        let missing = format!("\"{}/dir with space/missing.txt", dir.path().display());

        assert!(is_path_cached_for_shell_token(&escaped));
        assert!(is_path_cached_for_shell_token(&double));
        assert!(is_path_cached_for_shell_token(&single));
        assert!(is_path_cached_for_shell_token(&unclosed));
        assert!(!is_path_cached_for_shell_token(&missing));
    }
}

#[cfg(test)]
mod relative_tests;
