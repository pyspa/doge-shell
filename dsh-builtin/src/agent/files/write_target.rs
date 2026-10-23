//! Bind an approved canonical path to parent and file identities, then write via fd.
use super::traversal;
use nix::libc;
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path},
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: libc::dev_t,
    inode: libc::ino_t,
}
impl Identity {
    fn from_stat(stat: &libc::stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        }
    }
    fn of(file: &File) -> io::Result<Self> {
        Ok(Self::from_stat(&nix::sys::stat::fstat(file)?))
    }
}

pub(crate) struct WriteTarget {
    names: Vec<CString>,
    // Pin every existing ancestor so its inode cannot be recycled during approval.
    parents: Vec<File>,
    existing: Option<Identity>,
    // Keep an unlinked approved inode alive, preventing dev/ino reuse.
    _existing_pin: Option<File>,
    restricted: bool,
}

fn regular_unlinked(stat: &libc::stat) -> io::Result<()> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(io::Error::other(
            "writes require a regular file with no hard links",
        ));
    }
    Ok(())
}
fn conflict() -> io::Error {
    io::Error::other("write target changed during approval; retry after reading the path again")
}

impl WriteTarget {
    pub(crate) fn prepare(path: &Path, restricted: bool) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::other("write path must be absolute"));
        }
        let names = path
            .components()
            .filter(|c| *c != Component::RootDir)
            .map(|part| {
                let Component::Normal(name) = part else {
                    return Err(io::Error::other("write path must be normalized"));
                };
                use std::os::unix::ffi::OsStrExt;
                Ok(CString::new(name.as_bytes())?)
            })
            .collect::<io::Result<Vec<_>>>()?;
        if names.is_empty() {
            return Err(io::Error::other("file path required"));
        }
        let mut parents = vec![traversal::root()?];
        for name in &names[..names.len() - 1] {
            match traversal::directory(parents.last().expect("root"), name) {
                Ok(file) => parents.push(file),
                Err(err) if err.kind() == io::ErrorKind::NotFound => break,
                Err(err) => return Err(err),
            }
        }
        let existing_pin = if parents.len() == names.len() {
            traversal::stat_at(parents.last().expect("root"), names.last().expect("file"))?
                .map(|stat| {
                    regular_unlinked(&stat)?;
                    let file = traversal::pin_file(
                        parents.last().expect("root"),
                        names.last().expect("file"),
                    )?;
                    let pinned = nix::sys::stat::fstat(&file)?;
                    regular_unlinked(&pinned)?;
                    if Identity::from_stat(&stat) != Identity::from_stat(&pinned) {
                        return Err(conflict());
                    }
                    Ok::<_, io::Error>(file)
                })
                .transpose()?
        } else {
            None
        };
        let existing = existing_pin.as_ref().map(Identity::of).transpose()?;
        Ok(Self {
            names,
            parents,
            existing,
            _existing_pin: existing_pin,
            restricted,
        })
    }

    pub(crate) fn exists(&self) -> bool {
        self.existing.is_some()
    }

    pub(crate) fn open_read(&self) -> io::Result<File> {
        if self.parents.len() != self.names.len() {
            return Err(io::ErrorKind::NotFound.into());
        }
        let file = traversal::open_at(
            self.parents.last().expect("root"),
            self.names.last().expect("file"),
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        )?;
        if Some(Identity::of(&file)?) != self.existing {
            return Err(conflict());
        }
        regular_unlinked(&nix::sys::stat::fstat(&file)?)?;
        Ok(file)
    }

    pub(crate) fn previous(&self) -> io::Result<String> {
        let mut file = self.open_read()?;
        let mut text = String::new();
        if self.restricted {
            (&mut file)
                .take(16 * 1024 * 1024 + 1)
                .read_to_string(&mut text)?;
            if text.len() > 16 * 1024 * 1024 {
                return Err(io::Error::other("task file exceeds 16 MiB"));
            }
        } else {
            file.read_to_string(&mut text)?;
        }
        Ok(text)
    }

    /// Reopen the original path without following any component symlink.
    /// Check before truncating; a newly appearing final file is never overwritten.
    pub(crate) fn open_checked(&self, read: bool) -> io::Result<File> {
        let mut parent = traversal::root()?;
        if Identity::of(&parent)? != Identity::of(&self.parents[0])? {
            return Err(conflict());
        }
        for (index, name) in self.names[..self.names.len() - 1].iter().enumerate() {
            let next = match traversal::directory(&parent, name) {
                Ok(file) => file,
                Err(err)
                    if err.kind() == io::ErrorKind::NotFound && index + 1 >= self.parents.len() =>
                {
                    traversal::mkdir(&parent, name, if self.restricted { 0o700 } else { 0o777 })?;
                    traversal::directory(&parent, name)?
                }
                Err(err) => return Err(err),
            };
            if let Some(original) = self.parents.get(index + 1)
                && Identity::of(&next)? != Identity::of(original)?
            {
                return Err(conflict());
            }
            parent = next;
        }
        let flags = libc::O_NONBLOCK
            | if read { libc::O_RDWR } else { libc::O_WRONLY }
            | if self.existing.is_none() {
                libc::O_CREAT | libc::O_EXCL
            } else {
                0
            };
        let file = traversal::open_at(
            &parent,
            self.names.last().expect("file"),
            flags,
            if self.restricted { 0o600 } else { 0o666 },
        )?;
        let stat = nix::sys::stat::fstat(&file)?;
        regular_unlinked(&stat)?;
        if let Some(original) = self.existing
            && Identity::from_stat(&stat) != original
        {
            return Err(conflict());
        }
        // Recheck the final binding before returning the descriptor to its writer.
        let current =
            traversal::stat_at(&parent, self.names.last().expect("file"))?.ok_or_else(conflict)?;
        if Identity::from_stat(&current) != Identity::from_stat(&stat) {
            return Err(conflict());
        }
        Ok(file)
    }

    pub(crate) fn write(self, contents: &str) -> io::Result<()> {
        let mut file = self.open_checked(false)?;
        file.set_len(0)?;
        file.write_all(contents.as_bytes())?;
        if self.restricted {
            file.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn has_open_handle(identity: Identity) -> bool {
        // Both supported Unix systems expose this process's open descriptors.
        fs::read_dir("/dev/fd")
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                let Ok(fd) = entry.file_name().to_string_lossy().parse::<i32>() else {
                    return false;
                };
                let mut stat = std::mem::MaybeUninit::uninit();
                // SAFETY: fstat borrows the fd number; no ownership is taken.
                (unsafe { libc::fstat(fd, stat.as_mut_ptr()) }) == 0
                    && Identity::from_stat(&unsafe { stat.assume_init() }) == identity
            })
    }

    #[test]
    fn existing_inode_remains_pinned_through_approval_even_after_unlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("file");
        fs::write(&path, "original").unwrap();
        let source = File::open(&path).unwrap();
        let identity = Identity::of(&source).unwrap();
        drop(source);
        let target = WriteTarget::prepare(&path, false).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(target.exists());
        assert!(
            has_open_handle(identity),
            "the approved inode must remain alive so it cannot be recycled"
        );
        drop(target);
        assert!(
            !has_open_handle(identity),
            "the approval snapshot must release its inode afterwards"
        );
    }
}
