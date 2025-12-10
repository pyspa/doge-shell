use crate::completion::command::CompletionCandidate;
use anyhow::Result;
use std::fs;
use std::path::{MAIN_SEPARATOR, Path};

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

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with(&file_prefix) {
                    let path = entry.path();

                    // Extension filter
                    if let Some(exts) = extensions
                        && path.is_file()
                    {
                        if let Some(ext) = path.extension() {
                            let ext_str = format!(".{}", ext.to_string_lossy());
                            if !exts.contains(&ext_str) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }

                    let full_path = Self::build_candidate_path(&dir_path, &file_name);

                    if path.is_dir() {
                        candidates.push(CompletionCandidate::directory(full_path));
                    } else {
                        candidates.push(CompletionCandidate::file(full_path));
                    }
                }
            }
        }

        Ok(candidates)
    }

    /// Generate directory completion candidates
    pub fn generate_directory_candidates(current_token: &str) -> Result<Vec<CompletionCandidate>> {
        let mut candidates = Vec::with_capacity(16);

        let (dir_path, dir_prefix) = Self::split_dir_and_prefix(current_token);

        if let Ok(entries) = fs::read_dir(&dir_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with(&dir_prefix) && entry.path().is_dir() {
                    let full_path = Self::build_candidate_path(&dir_path, &file_name);

                    candidates.push(CompletionCandidate::directory(full_path));
                }
            }
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
