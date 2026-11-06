//! Bind a foreground replacement preview to one file and revalidate before writing.
use std::{
    fs::{self, File},
    io::{self, Read, Seek, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

pub(super) struct Preimage {
    // Keep the original inode alive throughout approval, including after unlink.
    file: File,
    target: crate::agent::files::WriteTarget,
    pub(super) contents: String,
}

impl Preimage {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let target = crate::agent::files::WriteTarget::prepare(path, false)?;
        let mut file = target.open_read()?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("replacement requires a regular file"));
        }
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(Self {
            file,
            target,
            contents,
        })
    }

    pub(super) fn write(self, path: &Path, updated: &str) -> io::Result<()> {
        let mut target = self.open_unchanged(path).map_err(|err| {
            io::Error::other(format!("replacement conflict: {err}; read the file again"))
        })?;
        // Validation and writing use this same fd. This is not atomic CAS
        // against arbitrary writers, and I/O failures cannot promise rollback.
        target.rewind()?;
        target.write_all(updated.as_bytes())?;
        target.set_len(updated.len() as u64)
    }

    fn open_unchanged(&self, path: &Path) -> io::Result<File> {
        // Never create or truncate on open: deletion and replacement during the
        // prompt must be conflicts, even when a replacement has identical bytes.
        let mut target = self.target.open_checked(true)?;
        let original = self.file.metadata()?;
        let same_file = |metadata: &fs::Metadata| {
            metadata.is_file()
                && metadata.dev() == original.dev()
                && metadata.ino() == original.ino()
        };
        if !same_file(&target.metadata()?) {
            return Err(io::Error::other("file identity changed"));
        }
        let mut current = Vec::new();
        target.read_to_end(&mut current)?;
        if current != self.contents.as_bytes() || !same_file(&fs::symlink_metadata(path)?) {
            return Err(io::Error::other("file contents or identity changed"));
        }
        Ok(target)
    }
}
