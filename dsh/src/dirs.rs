use std::fs::{DirEntry, read_dir};
use std::os::unix::fs::PermissionsExt;
use std::path;

pub fn search_file(dir: &str, name: &str) -> Option<String> {
    match read_dir(dir) {
        Ok(entries) => {
            let mut entries: Vec<DirEntry> = entries.flatten().collect();
            entries.sort_by_key(|x| x.file_name());

            for entry in entries {
                let buf = entry.file_name();
                if let Some(file_name) = buf.to_str()
                    && file_name.starts_with(name)
                    && let Ok(file_type) = entry.file_type()
                    && file_type.is_file()
                    && is_executable(&entry)
                {
                    return Some(file_name.to_string());
                }
            }
            None
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
}
