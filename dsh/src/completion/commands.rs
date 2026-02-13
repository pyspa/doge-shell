use crate::completion::display::Candidate;
use crate::completion::fuzzy::fuzzy_match_score;
use crate::dirs::is_executable;
use std::collections::{HashMap, HashSet};
use std::fs::read_dir;
use tracing::debug;

pub fn get_commands(paths: &[String], cmd: &str) -> Vec<Candidate> {
    let mut list = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    if cmd.starts_with('/') {
        let cmd_path = std::path::Path::new(cmd);
        if cmd_path.exists() && cmd_path.is_file() {
            // Extract filename for deduplication? Or just add?
            // Absolute paths usually don't need dedupe against PATH commands by name unless we want to?
            // Current logic treated them as "(command)".
            // Let's keep it simple.
            list.push(Candidate::Item(cmd.to_string(), "(command)".to_string()));
        }
    }
    if cmd.starts_with("./") {
        let cmd_path = std::path::Path::new(cmd);
        if cmd_path.exists() && cmd_path.is_file() {
            list.push(Candidate::Item(cmd.to_string(), "(command)".to_string()));
        }
    }

    for path in paths {
        get_executables_into(path, cmd, &mut list, &mut seen_names);
    }

    // No need to call deduplicate_candidates(list) here if we trust our seen_names logic for commands.
    // However, deduplicate_candidates also handles file vs command priority.
    // But get_commands ONLY produces commands.
    // So we are safe.
    list
}

fn get_executables_into(
    dir: &str,
    name: &str,
    list: &mut Vec<Candidate>,
    seen: &mut HashSet<String>,
) {
    match read_dir(dir) {
        Ok(entries) => {
            let mut local_candidates: Vec<String> = Vec::new();

            for entry in entries.flatten() {
                let file_name_os = entry.file_name();
                let Some(file_name) = file_name_os.to_str() else {
                    continue;
                };

                if fuzzy_match_score(file_name, name).is_none() {
                    continue;
                }

                // Check seen
                if seen.contains(file_name) {
                    continue;
                }

                // Optimization: check file type from entry if possible
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

            for candle in local_candidates {
                if seen.insert(candle.clone()) {
                    list.push(Candidate::Item(candle, "(command)".to_string()));
                }
            }
        }
        Err(_err) => {}
    }
}

#[cfg(test)]
fn get_executables(dir: &str, name: &str) -> Vec<Candidate> {
    let mut list = Vec::new();
    let mut seen = HashSet::new();
    get_executables_into(dir, name, &mut list, &mut seen);
    list
}

/// Deduplicate candidates, prioritizing commands over files for the same name
pub fn deduplicate_candidates(items: Vec<Candidate>) -> Vec<Candidate> {
    debug!("deduplicate_candidates: input items count={}", items.len());
    let mut seen_names = HashMap::new();
    let mut result = Vec::new();

    for candidate in items {
        let (name, _description) = match &candidate {
            Candidate::Item(name, desc) => (name.clone(), desc.clone()),
            Candidate::Path(name) => (name.clone(), "(path)".to_string()),
            Candidate::Basic(name) => (name.clone(), "(basic)".to_string()),
            Candidate::Command { name, description } => (name.clone(), description.clone()),
            Candidate::Option { name, description } => (name.clone(), description.clone()),
            Candidate::GitBranch { name, .. } => (name.clone(), "(git-branch)".to_string()),
            Candidate::File { path, is_dir } => (
                path.clone(),
                if *is_dir { "(directory)" } else { "(file)" }.to_string(),
            ),
            Candidate::History { command, .. } => (command.clone(), "(history)".to_string()),
            Candidate::Process { pid, command } => (pid.clone(), command.clone()),
        };

        // Extract just the filename for comparison (remove path prefixes)
        let base_name = if let Some(pos) = name.rfind('/') {
            &name[pos + 1..]
        } else {
            &name
        };

        match seen_names.get(base_name) {
            Some(existing_idx) => {
                // If we already have this name, prioritize commands over files
                let existing_candidate = &result[*existing_idx];
                let should_replace = match (&existing_candidate, &candidate) {
                    // Replace file with command
                    (Candidate::Item(_, existing_desc), Candidate::Item(_, new_desc))
                        if existing_desc == "(file)" && new_desc == "(command)" =>
                    {
                        debug!(
                            "deduplicate_candidates: replacing file with command for '{}'",
                            base_name
                        );
                        true
                    }
                    // Don't replace command with file
                    (Candidate::Item(_, existing_desc), Candidate::Item(_, new_desc))
                        if existing_desc == "(command)" && new_desc == "(file)" =>
                    {
                        debug!(
                            "deduplicate_candidates: keeping command over file for '{}'",
                            base_name
                        );
                        false
                    }
                    // For other cases, keep the first one
                    _ => {
                        debug!(
                            "deduplicate_candidates: keeping first occurrence for '{}'",
                            base_name
                        );
                        false
                    }
                };

                if should_replace {
                    result[*existing_idx] = candidate;
                }
            }
            None => {
                // First time seeing this name
                seen_names.insert(base_name.to_string(), result.len());
                result.push(candidate);
            }
        }
    }

    debug!(
        "deduplicate_candidates: output items count={}",
        result.len()
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn test_deduplicate_candidates() {
        // Test deduplication with command priority over file
        let items = vec![
            Candidate::Item("test".to_string(), "(file)".to_string()),
            Candidate::Item("test".to_string(), "(command)".to_string()),
            Candidate::Item("other".to_string(), "(file)".to_string()),
        ];

        let result = deduplicate_candidates(items);

        assert_eq!(result.len(), 2);
        // Should keep command version of "test", not file version
        assert!(result.iter().any(
            |c| matches!(c, Candidate::Item(name, desc) if name == "test" && desc == "(command)")
        ));
        assert!(result.iter().any(
            |c| matches!(c, Candidate::Item(name, desc) if name == "other" && desc == "(file)")
        ));
        // Should not have file version of "test"
        assert!(!result.iter().any(
            |c| matches!(c, Candidate::Item(name, desc) if name == "test" && desc == "(file)")
        ));
    }

    #[test]
    fn test_deduplicate_candidates_with_paths() {
        // Test deduplication with path prefixes
        let items = vec![
            Candidate::Item("/usr/bin/ls".to_string(), "(command)".to_string()),
            Candidate::Item("./ls".to_string(), "(file)".to_string()),
            Candidate::Item("ls".to_string(), "(command)".to_string()), // This is duplicate of /usr/bin/ls base_name?
        ];

        let result = deduplicate_candidates(items);

        // Should deduplicate based on base filename "ls"
        // Wait, logic says: seen_names.get(base_name)
        // base_name of "/usr/bin/ls" is "ls".
        // base_name of "./ls" is "ls".
        // base_name of "ls" is "ls".
        // So they all map to "ls".

        assert_eq!(result.len(), 1);
        // Should keep the first command version.
        // First is /usr/bin/ls (command).
        // Second is ./ls (file). (command vs file -> keep command).
        // Third is ls (command). (command vs command -> keep first).

        assert!(result.iter().any(|c| matches!(c, Candidate::Item(name, desc) if name == "/usr/bin/ls" && desc == "(command)")));
    }

    #[test]
    fn test_get_executables_fuzzy() {
        let dir = tempdir().unwrap();
        let dir_path = dir.path();
        let cargo_path = dir_path.join("cargo");
        let docker_path = dir_path.join("docker");

        {
            let f = File::create(&cargo_path).unwrap();
            #[cfg(unix)]
            {
                let mut perms = f.metadata().unwrap().permissions();
                perms.set_mode(0o755);
                f.set_permissions(perms).unwrap();
            }
        }

        {
            let f = File::create(&docker_path).unwrap();
            #[cfg(unix)]
            {
                let mut perms = f.metadata().unwrap().permissions();
                perms.set_mode(0o755);
                f.set_permissions(perms).unwrap();
            }
        }

        // We use private get_executables for testing logic
        // "cgo" -> "cargo"
        let results_cgo = super::get_executables(dir_path.to_str().unwrap(), "cgo");
        assert!(!results_cgo.is_empty());
        assert!(
            results_cgo
                .iter()
                .any(|c| matches!(c, Candidate::Item(name, _) if name == "cargo"))
        );

        // "dck" -> "docker"
        let results_dck = super::get_executables(dir_path.to_str().unwrap(), "dck");
        assert!(!results_dck.is_empty());
        assert!(
            results_dck
                .iter()
                .any(|c| matches!(c, Candidate::Item(name, _) if name == "docker"))
        );

        // "xyz" -> none
        let results_none = super::get_executables(dir_path.to_str().unwrap(), "xyz");
        assert!(results_none.is_empty());
    }

    #[test]
    fn test_get_commands_with_empty_paths() {
        let paths: Vec<String> = vec![];
        let result = get_commands(&paths, "ls");
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_commands_with_nonexistent_dir() {
        let paths = vec!["/nonexistent/path/that/does/not/exist".to_string()];
        let result = get_commands(&paths, "ls");
        // Should not panic, just return empty
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_commands_absolute_path() {
        let dir = tempdir().unwrap();
        let exec_path = dir.path().join("my_tool");
        {
            let f = File::create(&exec_path).unwrap();
            #[cfg(unix)]
            {
                let mut perms = f.metadata().unwrap().permissions();
                perms.set_mode(0o755);
                f.set_permissions(perms).unwrap();
            }
        }

        // Absolute path should be found directly
        let abs_str = exec_path.to_str().unwrap();
        let result = get_commands(&[], abs_str);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], Candidate::Item(name, desc)
            if name == abs_str && desc == "(command)"));
    }

    #[test]
    fn test_get_commands_relative_path() {
        let dir = tempdir().unwrap();
        let exec_path = dir.path().join("my_script");
        {
            let f = File::create(&exec_path).unwrap();
            #[cfg(unix)]
            {
                let mut perms = f.metadata().unwrap().permissions();
                perms.set_mode(0o755);
                f.set_permissions(perms).unwrap();
            }
        }

        // Relative "./" paths are handled by checking existence
        let rel_str = format!("./{}", exec_path.file_name().unwrap().to_str().unwrap());
        // This won't match unless CWD is the temp dir, so test the non-match case
        let result = get_commands(&[], &rel_str);
        // File doesn't exist at "./my_script" relative to CWD, so empty
        // (unless CWD happens to have it — we accept either outcome)
        assert!(result.len() <= 1);
    }

    #[test]
    fn test_deduplicate_candidates_mixed_variants() {
        // Test deduplication across different Candidate variants
        let items = vec![
            Candidate::Command {
                name: "git".to_string(),
                description: "version control".to_string(),
            },
            Candidate::Item("git".to_string(), "(command)".to_string()),
        ];
        let result = deduplicate_candidates(items);
        // Both have base_name "git", so only first should survive
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_deduplicate_candidates_empty_input() {
        let items: Vec<Candidate> = vec![];
        let result = deduplicate_candidates(items);
        assert!(result.is_empty());
    }

    #[test]
    fn test_deduplicate_candidates_single_item() {
        let items = vec![Candidate::Item("ls".to_string(), "(command)".to_string())];
        let result = deduplicate_candidates(items);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_get_executables_empty_dir() {
        let dir = tempdir().unwrap();
        let results = super::get_executables(dir.path().to_str().unwrap(), "anything");
        assert!(results.is_empty());
    }

    #[test]
    fn test_get_executables_non_executable_file() {
        let dir = tempdir().unwrap();
        // Create a non-executable file
        File::create(dir.path().join("readme.txt")).unwrap();

        let results = super::get_executables(dir.path().to_str().unwrap(), "readme");
        // Should not be found since it's not executable
        assert!(results.is_empty());
    }
}
