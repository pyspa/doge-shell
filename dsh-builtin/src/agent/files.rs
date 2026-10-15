//! Descriptor-relative access after host path authorization. Never follow a
//! symlink introduced between authorization and opening a task file.
use nix::libc;
use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::MetadataExt,
        },
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

/// Open a host-authorized directory without following any symlink component.
/// The returned handle pins the directory for descriptor-relative listing.
pub fn open_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::other("task directory must be absolute"));
    }
    let mut directory = File::open("/")?;
    for component in path.components().filter(|part| *part != Component::RootDir) {
        let Component::Normal(name) = component else {
            return Err(io::Error::other("task directory must be normalized"));
        };
        let name = CString::new(name.as_bytes())?;
        // SAFETY: directory owns the base fd, name is NUL-terminated, and
        // every successful open is immediately wrapped in exactly one File.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[derive(Debug)]
pub struct DirectoryEntry {
    pub name: OsString,
    pub metadata: Option<DirectoryMetadata>,
}

#[derive(Debug)]
pub struct DirectoryMetadata {
    pub is_dir: bool,
    pub size: u64,
}

pub fn read_directory(path: &Path) -> io::Result<Vec<DirectoryEntry>> {
    list_open_directory(open_directory(path)?)
}

fn list_open_directory(file: File) -> io::Result<Vec<DirectoryEntry>> {
    let mut directory = nix::dir::Dir::from_fd(file.into())?;
    let fd = directory.as_raw_fd();
    let mut entries = Vec::new();
    for entry in directory.iter() {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fd remains owned by directory for the complete iteration,
        // name is a directory-entry C string, and stat is initialized only on
        // success. NOFOLLOW prevents an entry swap from inspecting its target.
        let metadata = if unsafe {
            libc::fstatat(
                fd,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0
        {
            let stat = unsafe { stat.assume_init() };
            u64::try_from(stat.st_size)
                .ok()
                .map(|size| DirectoryMetadata {
                    is_dir: stat.st_mode & libc::S_IFMT == libc::S_IFDIR,
                    size,
                })
        } else {
            None
        };
        entries.push(DirectoryEntry {
            name: OsString::from_vec(name.to_bytes().to_vec()),
            metadata,
        });
    }
    Ok(entries)
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

    #[test]
    fn directory_listing_pins_its_handle_and_rejects_symlink_ancestors() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let inside = root.join("inside");
        let outside = root.join("outside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(inside.join("allowed"), "a").unwrap();
        std::fs::write(outside.join("secret"), "s").unwrap();
        let pinned = open_directory(&inside).unwrap();
        std::fs::rename(&inside, root.join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &inside).unwrap();
        let entries = list_open_directory(pinned).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "allowed");
        assert!(read_directory(&inside).is_err());
        assert!(read_directory(&inside.join("nested")).is_err());
        assert!(open_directory(Path::new("relative")).is_err());
        assert!(open_directory(&root.join("../elsewhere")).is_err());
    }

    #[test]
    fn directory_metadata_does_not_follow_entry_symlinks() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let entries = read_directory(&root).unwrap();
        let entry = entries.iter().find(|entry| entry.name == "link").unwrap();
        let metadata = entry.metadata.as_ref().unwrap();
        assert!(!metadata.is_dir);
        assert_eq!(
            metadata.size,
            std::fs::symlink_metadata(link).unwrap().len()
        );
    }
}
