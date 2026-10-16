//! Shared bounded traversal for static Nx discovery and its cache signature.
use std::fs;
use std::path::{Path, PathBuf};

// A project at a/b/c/d/project.json is included; a/b/c/d/e is never opened.
const MAX_DIRECTORY_DEPTH: usize = 4;

pub(crate) fn find_task_files(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_task_files(root, 0, names, &mut files);
    // read_dir ordering is unspecified. Cache signatures need a stable order.
    files.sort();
    files
}

fn collect_task_files(dir: &Path, depth: usize, names: &[&str], files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        // DirEntry::file_type does not follow symlinks. In particular, never
        // open a linked directory, even if it points back into the workspace.
        if file_type.is_dir() {
            if depth < MAX_DIRECTORY_DEPTH
                && !name.starts_with('.')
                && !matches!(name, "node_modules" | "target" | "dist" | "build")
            {
                collect_task_files(&path, depth + 1, names, files);
            }
        } else if names.contains(&name) && path.is_file() {
            // Preserve support for individual linked marker files.
            files.push(path);
        }
    }
}
