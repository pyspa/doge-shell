use std::fs::{DirEntry, read_dir};
use std::os::unix::fs::PermissionsExt;
use std::path;

pub fn search_file(dir: &str, name: &str) -> Option<String> {
    match read_dir(dir) {
        Ok(entries) => {
            let mut best_match: Option<String> = None;

            for entry in entries {
                let entry: DirEntry = match entry {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                let buf = entry.file_name();
                if let Some(file_name) = buf.to_str()
                    && file_name.starts_with(name)
                    && let Ok(file_type) = entry.file_type()
                    && file_type.is_file()
                    && is_executable(&entry)
                {
                    if file_name == name {
                        return Some(file_name.to_string());
                    }
                    match &best_match {
                        Some(current_best) if current_best.as_str() <= file_name => {}
                        _ => best_match = Some(file_name.to_string()),
                    }
                }
            }
            best_match
        }
        Err(_err) => None,
    }
}

pub fn is_executable(entry: &DirEntry) -> bool {
    match entry.metadata() {
        Ok(meta) => {
            let permissions = meta.permissions();
            permissions.mode();
            permissions.mode() & 0o111 != 0
        }
        Err(_err) => false,
    }
}

pub fn is_dir(input: &str) -> bool {
    path::Path::new(&shellexpand::tilde(input).to_string()).is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_is_dir() {
        init();
        let b = is_dir("./");
        assert!(b);
        let b = is_dir("../");
        assert!(b);
    }

    #[test]
    #[ignore]
    fn test_search_file() {
        init();
        let b = search_file("/bin", "g");
        println!("{b:?}");
    }

    #[test]
    fn search_file_returns_lexicographically_first_match_without_sorting() {
        init();
        let dir = tempdir().unwrap();
        let alpha = dir.path().join("alpha-cmd");
        let beta = dir.path().join("beta-cmd");

        std::fs::write(&beta, "").unwrap();
        std::fs::write(&alpha, "").unwrap();

        #[cfg(unix)]
        {
            let mut alpha_perms = std::fs::metadata(&alpha).unwrap().permissions();
            alpha_perms.set_mode(0o755);
            std::fs::set_permissions(&alpha, alpha_perms).unwrap();

            let mut beta_perms = std::fs::metadata(&beta).unwrap().permissions();
            beta_perms.set_mode(0o755);
            std::fs::set_permissions(&beta, beta_perms).unwrap();
        }

        let found = search_file(dir.path().to_str().unwrap(), "");
        assert_eq!(found, Some("alpha-cmd".to_string()));
    }
}
