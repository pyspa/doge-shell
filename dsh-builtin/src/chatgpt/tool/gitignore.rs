use ignore::gitignore::GitignoreBuilder;
use std::path::Path;

/// Check if a path should be ignored according to .gitignore rules.
/// Returns true if the path is ignored and should NOT be accessed.
pub fn is_gitignored(path: &Path, base_dir: &Path) -> bool {
    // Build gitignore matcher from the base directory
    let gitignore_path = base_dir.join(".gitignore");

    if !gitignore_path.exists() {
        // No .gitignore file, allow access
        return false;
    }

    let mut builder = GitignoreBuilder::new(base_dir);

    // Add the .gitignore file
    if builder.add(&gitignore_path).is_some() {
        // Error adding gitignore, allow access as fallback
        return false;
    }

    match builder.build() {
        Ok(gitignore) => {
            // Check if the path itself matches any ignore pattern
            let is_dir = path.is_dir();
            if matches!(gitignore.matched(path, is_dir), ignore::Match::Ignore(_)) {
                return true;
            }

            // Also check if any parent directory is ignored
            // This handles the case where a file is inside a gitignored directory
            if let Ok(relative_path) = path.strip_prefix(base_dir) {
                let mut current = base_dir.to_path_buf();
                for component in relative_path.components() {
                    current.push(component);
                    if current != path {
                        // Check if this intermediate directory is ignored
                        if matches!(gitignore.matched(&current, true), ignore::Match::Ignore(_)) {
                            return true;
                        }
                    }
                }
            }

            false
        }
        Err(_) => {
            // Failed to build gitignore, allow access as fallback
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_gitignore_allows_all() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();

        assert!(!is_gitignored(&file_path, dir.path()));
    }

    #[test]
    fn test_gitignore_blocks_matching_file() {
        let dir = tempdir().unwrap();
        let gitignore_path = dir.path().join(".gitignore");
        fs::write(&gitignore_path, "*.secret\n.env\n").unwrap();

        let secret_file = dir.path().join("password.secret");
        fs::write(&secret_file, "secret").unwrap();

        let env_file = dir.path().join(".env");
        fs::write(&env_file, "SECRET=value").unwrap();

        let normal_file = dir.path().join("normal.txt");
        fs::write(&normal_file, "normal").unwrap();

        assert!(is_gitignored(&secret_file, dir.path()));
        assert!(is_gitignored(&env_file, dir.path()));
        assert!(!is_gitignored(&normal_file, dir.path()));
    }

    #[test]
    fn test_gitignore_directory_pattern() {
        let dir = tempdir().unwrap();
        let gitignore_path = dir.path().join(".gitignore");
        fs::write(&gitignore_path, "node_modules/\n").unwrap();

        let node_modules = dir.path().join("node_modules");
        fs::create_dir(&node_modules).unwrap();

        let file_in_node_modules = node_modules.join("package.json");
        fs::write(&file_in_node_modules, "{}").unwrap();

        assert!(is_gitignored(&node_modules, dir.path()));
        assert!(is_gitignored(&file_in_node_modules, dir.path()));
    }
}
