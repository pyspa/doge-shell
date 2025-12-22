use anyhow::{Context, Result};
use nix::fcntl::OFlag;
use nix::pty::{Winsize, grantpt, posix_openpt, ptsname, unlockpt};
use nix::unistd::setsid;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Pty {
    pub master: std::fs::File,
    pub slave: std::fs::File,
    pub name: PathBuf,
}

impl Pty {
    pub fn new() -> Result<Self> {
        // Open the master side
        let master_fd =
            posix_openpt(OFlag::O_RDWR | OFlag::O_NOCTTY).context("posix_openpt failed")?;

        // Grant access to the slave
        grantpt(&master_fd).context("grantpt failed")?;

        // Unlock the slave
        unlockpt(&master_fd).context("unlockpt failed")?;

        // Get the name of the slave
        let slave_name = unsafe { ptsname(&master_fd) }.context("ptsname failed")?;
        let name = PathBuf::from(slave_name);

        // Open the slave side
        // We open it directly using std::fs::File logic or nix::fcntl::open
        // But simply, we can just open it as a file.
        // However, we need to be careful about controlling tty.
        // For the slave side in separate process, we usually open it there.
        // But here we want a handle to it to pass to child or for child to open.
        // Actually, mostly we just need the master here, and child opens the slave path.
        // But keeping a slave handle prevents EOF on master if no child is running yet?
        // Let's open it.

        use std::fs::OpenOptions;
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .context("failed to open slave pty")?;

        let master = unsafe { std::fs::File::from_raw_fd(master_fd.into_raw_fd()) };

        Ok(Self {
            master,
            slave,
            name,
        })
    }

    pub fn try_clone(&self) -> Result<Self> {
        let master = self.master.try_clone().context("failed to clone master")?;
        let slave = self.slave.try_clone().context("failed to clone slave")?;
        Ok(Self {
            master,
            slave,
            name: self.name.clone(),
        })
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        unsafe {
            // TIOCSWINSZ is ioctl for setting window size
            // nix::ioctl! macro is needed or use the function if available.
            // nix has `tcsetwinsize` ? No, `ioctl_write_ptr_bad!` maybe?
            // Actually nix::pty doesn't expose it directly in older versions?
            // Check nix 0.26 docs. It might be in termios?
            // To be safe and simple, let's look for a helper or implement needed ioctl.

            // libc::TIOCSWINSZ
            let res = libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
            if res != 0 {
                return Err(anyhow::anyhow!("ioctl TIOCSWINSZ failed"));
            }
        }
        Ok(())
    }
}

/// Start a new session and set the controlling terminal to the given slave PTY
/// This should be called in the child process
pub fn make_controlling_terminal(slave_fd: RawFd) -> Result<()> {
    setsid().context("setsid failed")?;

    // Set controlling terminal
    unsafe {
        if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) != 0 {
            return Err(anyhow::anyhow!("ioctl TIOCSCTTY failed"));
        }
    }
    Ok(())
}
