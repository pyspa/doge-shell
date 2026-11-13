//! No-follow directory enumeration; ignore rules never own filesystem traversal.
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) struct Entry {
    pub path: PathBuf,
    pub is_file: bool,
}

pub(super) struct Walk {
    pending: Vec<(PathBuf, bool, bool)>,
    ignores: super::super::gitignore::IgnoreMatcher,
}

impl Walk {
    pub fn new(root: &Path, base: &Path) -> Self {
        Self {
            pending: vec![(root.to_owned(), root.is_dir(), true)],
            ignores: super::super::gitignore::IgnoreMatcher::new(if root.starts_with(base) {
                base
            } else if root.is_dir() {
                root
            } else {
                root.parent().unwrap_or(root)
            }),
        }
    }
}

impl Iterator for Walk {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((path, directory, regular)) = self.pending.pop() {
            if !directory {
                return Some(Ok(Entry {
                    path,
                    is_file: regular,
                }));
            }
            // read_directory uses the opened fd for readdir and fstatat. A
            // symlink swap before this call is refused; a later swap cannot
            // redirect enumeration to a different directory.
            let entries = match crate::agent::files::read_directory(&path) {
                Ok(entries) => entries,
                Err(err) => return Some(Err(err)),
            };
            for entry in entries {
                let child = path.join(entry.name);
                let Some(metadata) = entry.metadata else {
                    continue;
                };
                match self.ignores.is_search_ignored(&child, metadata.is_dir) {
                    Ok(true) => continue,
                    Err(err) => return Some(Err(io::Error::other(err))),
                    Ok(false) => {}
                }
                self.pending
                    .push((child, metadata.is_dir, metadata.is_file));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn swapping_root_after_walk_creation_never_lists_outside() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(outside.path().join("outside-sentinel"), "private").unwrap();
        let mut walk = Walk::new(&root, dir.path());
        fs::remove_dir(&root).unwrap();
        symlink(outside.path(), &root).unwrap();
        assert!(walk.next().unwrap().is_err());
        assert!(walk.next().is_none());
    }

    #[test]
    fn external_roots_and_nested_whitelists_keep_their_ignore_rules() {
        let workspace = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(external.path()).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join(".ignore"), "private.txt\n").unwrap();
        fs::write(root.join("private.txt"), "private").unwrap();
        fs::write(root.join("nested/.ignore"), "*\n!keep.txt\n").unwrap();
        fs::write(root.join("nested/keep.txt"), "keep").unwrap();
        fs::write(root.join("nested/skip.txt"), "skip").unwrap();
        let paths: Vec<_> = Walk::new(&root, workspace.path())
            .map(|entry| entry.unwrap().path.strip_prefix(&root).unwrap().to_owned())
            .collect();
        assert_eq!(paths, vec![PathBuf::from("nested/keep.txt")]);
    }
}
