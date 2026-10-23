//! Descriptor-relative access after host path authorization. Never follow a
//! symlink introduced between authorization and opening a task file.
use nix::libc;
mod traversal;
mod write_target;
use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::{OsStrExt, OsStringExt},
    },
    path::{Component, Path},
};
pub(crate) use write_target::WriteTarget;

pub(crate) fn open(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::other("task file must be absolute"));
    }
    let mut directory = traversal::root()?;
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
        if !final_part {
            directory = traversal::directory(&directory, &name)?;
            continue;
        }
        let flags = libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_RDONLY;
        // SAFETY: directory owns a valid fd; CString is terminated, and all
        // returned descriptors are immediately owned by File.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if final_part {
            let metadata = file.metadata()?;
            if !metadata.is_file() {
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
    open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("task file exceeds 16 MiB"));
    }
    Ok(text)
}
pub fn write(path: &Path, contents: &str) -> io::Result<()> {
    WriteTarget::prepare(path, true)?.write(contents)
}

/// Open a host-authorized directory without following any symlink component.
/// The returned handle pins the directory for descriptor-relative listing.
pub fn open_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::other("task directory must be absolute"));
    }
    let mut directory = traversal::root()?;
    for component in path.components().filter(|part| *part != Component::RootDir) {
        let Component::Normal(name) = component else {
            return Err(io::Error::other("task directory must be normalized"));
        };
        let name = CString::new(name.as_bytes())?;
        directory = traversal::directory(&directory, &name)?;
    }
    // Search handles cannot be passed to readdir/fdopendir.
    traversal::open_at(&directory, c".", libc::O_RDONLY | libc::O_DIRECTORY, 0)
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

#[cfg(test)]
mod permission_tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn search_only_ancestors_do_not_require_listing_permission() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let parent = root.join("search-only");
        let child = parent.join("listable");
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("file"), "original").unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o111)).unwrap();
        let listing = read_directory(&child);
        let parent_listing = read_directory(&parent);
        let contents = read(&child.join("file"));
        let written = write(&child.join("file"), "updated");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(listing.unwrap()[0].name, "file");
        assert_eq!(
            parent_listing.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(contents.unwrap(), "original");
        written.unwrap();
        assert_eq!(fs::read_to_string(child.join("file")).unwrap(), "updated");
    }
}
