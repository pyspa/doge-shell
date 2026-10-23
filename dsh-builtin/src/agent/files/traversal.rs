//! Search-only directory handles; listing must open a separate readable fd.
use nix::libc;
use std::{
    ffi::CStr,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
};

fn search_flags() -> i32 {
    #[cfg(target_os = "macos")]
    {
        libc::O_SEARCH
    }
    #[cfg(target_os = "linux")]
    {
        libc::O_PATH | libc::O_DIRECTORY
    }
}

pub(super) fn root() -> io::Result<File> {
    // SAFETY: literal C string, no creation; successful fd has one owner.
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            search_flags() | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned(fd)
}

pub(super) fn directory(parent: &File, name: &CStr) -> io::Result<File> {
    open_at(parent, name, search_flags(), 0)
}

/// Keep an existing inode alive without requiring file read permission.
/// This handle is used only for fstat/lifetime, never traversal, listing or I/O.
pub(super) fn pin_file(parent: &File, name: &CStr) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        open_at(parent, name, libc::O_PATH | libc::O_NONBLOCK, 0)
    }
    #[cfg(target_os = "macos")]
    {
        // A normal fd pins the inode. O_EVTONLY is not suitable for write-only
        // files: Darwin still requires read permission for that mode.
        // Neither attempt creates, truncates, reads or writes file contents.
        match open_at(parent, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) => Ok(file),
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                open_at(parent, name, libc::O_WRONLY | libc::O_NONBLOCK, 0)
            }
            Err(err) => Err(err),
        }
    }
}

pub(super) fn open_at(parent: &File, name: &CStr, flags: i32, mode: i32) -> io::Result<File> {
    // SAFETY: live base fd and terminated name; wrap each successful fd once.
    owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    })
}

fn owned(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: fd was freshly returned by open/openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

pub(super) fn stat_at(parent: &File, name: &CStr) -> io::Result<Option<libc::stat>> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: output initialized only on success; final symlinks aren't followed.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        Ok(Some(unsafe { stat.assume_init() }))
    } else {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

pub(super) fn mkdir(parent: &File, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
    // SAFETY: parent lives through this call; name is terminated.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode) } == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::AlreadyExists {
        Ok(())
    } else {
        Err(err)
    }
}
