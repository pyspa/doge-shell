//! Pin the public root and consume handles, not checked-then-reopened paths.
use crate::agent::files;
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

pub(super) struct ServeRoot {
    pub path: PathBuf,
    directory: File,
    root_listing: parking_lot::Mutex<()>,
}

pub(super) struct OpenedPath {
    pub file: File,
    pub relative: PathBuf,
}

impl ServeRoot {
    pub fn new(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let directory = files::open_directory(&path)?;
        Ok(Self {
            path,
            directory,
            root_listing: parking_lot::Mutex::new(()),
        })
    }

    // Preserve links that resolve within the public root. Resolution supplies
    // only a relative name: every actual open is anchored to the pinned root,
    // refusing any symlink inserted after this check, including parent swaps.
    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        self.path
            .join(path)
            .canonicalize()?
            .strip_prefix(&self.path)
            .map(Path::to_owned)
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "outside public root"))
    }

    fn open_resolved(&self, relative: &Path) -> io::Result<File> {
        files::open_relative(&self.directory, relative)
    }

    pub fn open(&self, path: &Path) -> io::Result<OpenedPath> {
        let relative = self.resolve(path)?;
        let file = self.open_resolved(&relative)?;
        Ok(OpenedPath { file, relative })
    }

    pub fn list(&self, file: File, relative: &Path) -> io::Result<Vec<files::DirectoryEntry>> {
        // The root handle is duplicated so even mode 0400 can be listed.
        // Its duplicates share an offset; nix's iterator rewinds on drop.
        // Other directories were opened independently and need no lock.
        let _guard = relative
            .as_os_str()
            .is_empty()
            .then(|| self.root_listing.lock());
        files::list_open_directory(file)
    }

    pub fn index(&self, directory: &OpenedPath) -> io::Result<Option<OpenedPath>> {
        // An absent index allows a listing; a dangling or escaping index must
        // return 404. Inspect the already-open directory, not its mutable path.
        match nix::sys::stat::fstatat(
            &directory.file,
            "index.html",
            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => self.open(&directory.relative.join("index.html")).map(Some),
            Err(nix::errno::Errno::ENOENT) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests;
