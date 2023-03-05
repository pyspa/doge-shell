pub use dirs;
use std::fs::{read_dir, DirEntry};
use std::os::unix::fs::PermissionsExt;
use std::path;

pub fn search_file(dir: &str, name: &str) -> Option<String> {
    match read_dir(dir) {
        Ok(entries) => {
            let mut entries: Vec<DirEntry> = entries.flatten().collect();
            entries.sort_by_key(|x| x.file_name());

            for entry in entries {
                let buf = entry.file_name();
                let file_name = buf.to_str().unwrap();
                if file_name.starts_with(name)
                    && entry.file_type().unwrap().is_file()
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
mod test {
    use super::*;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_is_dir() {
        let b = is_dir("./");
        assert_eq!(true, b);
        let b = is_dir("../");
        assert_eq!(true, b);
    }

    #[test]
    #[ignore]
    fn test_search_file() {
        let b = search_file("/bin", "g");
        println!("{:?}", b);
    }
}
