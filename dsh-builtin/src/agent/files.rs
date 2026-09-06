//! Descriptor-relative access after host path authorization. Never follow a
//! symlink introduced between authorization and opening a task file.
use nix::libc;
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

pub(crate) fn open(path: &Path, write: bool) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::other("task file must be absolute"));
    }
    let mut directory = File::open("/")?;
    let mut parts = path
        .components()
        .filter(|c| *c != Component::RootDir)
        .peekable();
    while let Some(component) = parts.next() {
        let Component::Normal(name) = component else {
            return Err(io::Error::other("task file must be normalized"));
        };
        let name = CString::new(name.as_bytes())?;
        let final_part = parts.peek().is_none();
        let flags = libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if final_part {
                libc::O_NONBLOCK
                    | if write {
                        libc::O_WRONLY | libc::O_CREAT
                    } else {
                        libc::O_RDONLY
                    }
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY
            };
        // SAFETY: directory owns a valid fd; CString is terminated, and all
        // returned descriptors are immediately owned by File.
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0
            && write
            && !final_part
            && io::Error::last_os_error().kind() == io::ErrorKind::NotFound
        {
            let made = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
            if made < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                return Err(io::Error::last_os_error());
            }
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if final_part {
            let metadata = file.metadata()?;
            if !metadata.is_file() || (write && metadata.nlink() > 1) {
                return Err(io::Error::other(
                    "task access requires a regular file; writes reject hard links",
                ));
            }
            return Ok(file);
        }
        directory = file;
    }
    Err(io::Error::other("task file required"))
}

pub fn read(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    open(path, false)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("task file exceeds 16 MiB"));
    }
    Ok(text)
}
pub fn write(path: &Path, contents: &str) -> io::Result<()> {
    let mut file = open(path, true)?;
    file.set_len(0)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replaced_parent_symlink_and_hardlink_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("inside")).unwrap();
        std::fs::create_dir(root.join("outside")).unwrap();
        write(&root.join("outside/target"), "original").unwrap();
        let authorized = root.join("inside/target");
        std::fs::remove_dir(root.join("inside")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("inside")).unwrap();
        assert!(write(&authorized, "escaped").is_err());
        assert!(read(&authorized).is_err());
        std::fs::hard_link(root.join("outside/target"), root.join("hardlink")).unwrap();
        assert!(write(&root.join("hardlink"), "escaped").is_err());
        assert_eq!(read(&root.join("outside/target")).unwrap(), "original");
        write(&root.join("new/nested/file"), "safe").unwrap();
        assert_eq!(read(&root.join("new/nested/file")).unwrap(), "safe");
    }
}
