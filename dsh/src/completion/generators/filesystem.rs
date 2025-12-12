use crate::completion::command::CompletionCandidate;
use crate::completion::{Candidate, fuzzy_match_score, path_completion_path};
use anyhow::Result;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

pub struct FileSystemGenerator;

impl FileSystemGenerator {
    /// Generate file completion candidates
    pub fn generate_file_candidates(current_token: &str) -> Result<Vec<CompletionCandidate>> {
        Self::generate_file_candidates_with_filter(current_token, None)
    }

    /// Generate file completion candidates with filter
    pub fn generate_file_candidates_with_filter(
        current_token: &str,
        extensions: Option<&Vec<String>>,
    ) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(32);

        let (dir_path, file_prefix) = Self::split_dir_and_prefix(current_token);

        // Reuse legacy path listing cache for better performance.
        let listing = path_completion_path(PathBuf::from(&dir_path)).unwrap_or_default();

        for cand in listing {
            let Candidate::Path(path_str) = cand else {
                continue;
            };

            let is_dir = path_str.ends_with(MAIN_SEPARATOR)
                || (MAIN_SEPARATOR != '/' && path_str.ends_with('/'))
                || (MAIN_SEPARATOR != '\\' && path_str.ends_with('\\'));

            let trimmed = path_str.trim_end_matches(&['/', '\\'][..]);
            let Some(file_name) = Path::new(trimmed).file_name().and_then(|s| s.to_str()) else {
                continue;
            };

            if !file_prefix.is_empty()
                && !file_name.starts_with(&file_prefix)
                && fuzzy_match_score(file_name, &file_prefix).is_none()
            {
                continue;
            }

            if !is_dir
                && let Some(exts) = extensions {
                    let ext_ok = Path::new(file_name)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| format!(".{e}"))
                        .is_some_and(|e| exts.contains(&e));
                    if !ext_ok {
                        continue;
                    }
                }

            let full_path = Self::build_candidate_path(&dir_path, file_name);
            if is_dir {
                candidates.push(CompletionCandidate::directory(full_path));
            } else {
                candidates.push(CompletionCandidate::file(full_path));
            }
        }

        Ok(candidates)
    }

    /// Generate directory completion candidates
    pub fn generate_directory_candidates(current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        let (dir_path, dir_prefix) = Self::split_dir_and_prefix(current_token);

        let listing = path_completion_path(PathBuf::from(&dir_path)).unwrap_or_default();

        for cand in listing {
            let Candidate::Path(path_str) = cand else {
                continue;
            };

            let is_dir = path_str.ends_with(MAIN_SEPARATOR)
                || (MAIN_SEPARATOR != '/' && path_str.ends_with('/'))
                || (MAIN_SEPARATOR != '\\' && path_str.ends_with('\\'));

            if !is_dir {
                continue;
            }

            let trimmed = path_str.trim_end_matches(&['/', '\\'][..]);
            let Some(file_name) = Path::new(trimmed).file_name().and_then(|s| s.to_str()) else {
                continue;
            };

            if !dir_prefix.is_empty()
                && !file_name.starts_with(&dir_prefix)
                && fuzzy_match_score(file_name, &dir_prefix).is_none()
            {
                continue;
            }

            let full_path = Self::build_candidate_path(&dir_path, file_name);
            candidates.push(CompletionCandidate::directory(full_path));
        }

        Ok(candidates)
    }

    // Helpers logic extracted from generator.rs
    pub fn split_dir_and_prefix(current_token: &str) -> (String, String) {
        if current_token.is_empty() {
            return (".".to_string(), String::new());
        }

        let path = Path::new(current_token);

        if Self::ends_with_path_separator(current_token) {
            let dir = Self::normalize_dir_path(path);
            return (dir, String::new());
        }

        if let Some(parent) = path.parent() {
            let dir = Self::normalize_dir_path(parent);
            let prefix = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            return (dir, prefix);
        }

        (".".to_string(), current_token.to_string())
    }

    pub fn ends_with_path_separator(token: &str) -> bool {
        token.ends_with(MAIN_SEPARATOR)
            || (MAIN_SEPARATOR != '/' && token.ends_with('/'))
            || (MAIN_SEPARATOR != '\\' && token.ends_with('\\'))
    }

    pub fn normalize_dir_path(path: &Path) -> String {
        if path.as_os_str().is_empty() {
            ".".to_string()
        } else {
            path.to_string_lossy().to_string()
        }
    }

    pub fn build_candidate_path(dir_path: &str, file_name: &str) -> String {
        if dir_path == "." || dir_path.is_empty() {
            return file_name.to_string();
        }

        Path::new(dir_path)
            .join(file_name)
            .to_string_lossy()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_candidates_support_fuzzy_match() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("apple.txt"), "").unwrap();
        std::fs::write(dir.path().join("application.rs"), "").unwrap();

        let token = dir.path().join("apl").to_string_lossy().to_string();
        let candidates =
            FileSystemGenerator::generate_file_candidates(&token).expect("file candidates");
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        let expected = dir
            .path()
            .join("application.rs")
            .to_string_lossy()
            .to_string();
        assert!(
            texts.contains(&expected),
            "expected fuzzy matched file {:?} in {:?}",
            expected,
            texts
        );
    }
}
