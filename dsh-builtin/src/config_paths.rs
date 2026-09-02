//! One resolution of the user's `dsh` configuration directory.
//!
//! `dirs::config_dir()` and `xdg::BaseDirectories` name the *same* directory on
//! Linux and different ones on macOS, where the former is
//! `~/Library/Application Support`. Mixing them is how the runtime skills ended
//! up written by the installer to `~/.config/dsh/skills`, read by the skill
//! loader from `~/Library/Application Support/dsh/skills`, and advertised to the
//! model as a third thing: on macOS an installed skill was simply never seen.
//!
//! Everything under `dsh-builtin` that needs a configuration path goes through
//! here. `scripts/check-portability.py` enforces that by refusing a direct
//! `dirs::config_dir()` outside this file.

use std::path::{Path, PathBuf};

const APP: &str = "dsh";

/// Where new configuration is written.
///
/// Always the XDG location, because that is what the docs, the installer and
/// `dsh`'s own `environment::get_config_file` all use.
pub fn config_home() -> PathBuf {
    xdg::BaseDirectories::with_prefix(APP)
        .get_config_home()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
                .join(APP)
        })
}

/// Every directory that may hold configuration, most authoritative first.
///
/// The platform directory is included as a fallback so a macOS user who already
/// has files under `~/Library/Application Support/dsh` keeps them working. It is
/// never written to.
pub fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = vec![config_home()];

    if let Some(platform) = dirs::config_dir().map(|path| path.join(APP))
        && !paths.contains(&platform)
    {
        paths.push(platform);
    }

    paths
}

/// The canonical path of a configuration file, whether or not it exists.
///
/// An existing file in any search path wins; otherwise the XDG path is returned
/// so a caller that creates the file creates it where the shell will read it.
pub fn config_file(name: &str) -> PathBuf {
    find_config_file(name).unwrap_or_else(|| config_home().join(name))
}

/// The path of an existing configuration file, if there is one.
pub fn find_config_file(name: &str) -> Option<PathBuf> {
    config_search_paths()
        .into_iter()
        .map(|root| root.join(name))
        .find(|path| path.is_file())
}

/// The runtime skills directory the agent reads and its tools may reach.
///
/// An existing directory in any search path wins, so a macOS install under
/// either spelling is found; a fresh install lands in the XDG one.
pub fn skills_dir() -> PathBuf {
    config_search_paths()
        .into_iter()
        .map(|root| root.join("skills"))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| config_home().join("skills"))
}

/// Render a path for a prompt or a message, shortening `$HOME` to `~`.
///
/// The skills fragment used to hard-code `~/.config/dsh/skills/`, which was
/// wrong for anyone with `XDG_CONFIG_HOME` set and wrong on macOS.
pub fn display_path(path: &Path) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.display().to_string();
    };
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex, MutexGuard};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    struct XdgGuard(Option<std::ffi::OsString>);

    impl XdgGuard {
        fn set(value: &Path) -> Self {
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: single-threaded under `env_lock`.
            unsafe { std::env::set_var("XDG_CONFIG_HOME", value) };
            Self(previous)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            // SAFETY: single-threaded under `env_lock`.
            match self.0.take() {
                Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
                None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
            }
        }
    }

    #[test]
    fn config_home_follows_xdg() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = XdgGuard::set(dir.path());

        assert_eq!(config_home(), dir.path().join("dsh"));
    }

    #[test]
    fn skills_dir_defaults_to_the_xdg_location_when_nothing_exists() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = XdgGuard::set(dir.path());

        assert_eq!(skills_dir(), dir.path().join("dsh").join("skills"));
    }

    #[test]
    fn skills_dir_finds_an_existing_xdg_directory() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = XdgGuard::set(dir.path());
        let expected = dir.path().join("dsh").join("skills");
        std::fs::create_dir_all(&expected).unwrap();

        assert_eq!(skills_dir(), expected);
    }

    /// A path that does not exist anywhere still resolves to the XDG location,
    /// so `doctor fix` creates the file where the shell will load it.
    #[test]
    fn config_file_writes_into_the_xdg_location() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = XdgGuard::set(dir.path());

        assert_eq!(
            config_file("config.lisp"),
            dir.path().join("dsh").join("config.lisp")
        );
    }

    #[test]
    fn config_file_prefers_a_file_that_exists() {
        let _lock = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = XdgGuard::set(dir.path());
        let root = dir.path().join("dsh");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.lisp"), ";; test\n").unwrap();

        assert_eq!(config_file("config.lisp"), root.join("config.lisp"));
        assert!(find_config_file("config.lisp").is_some());
    }

    #[test]
    fn display_path_shortens_the_home_directory() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert_eq!(display_path(&home.join("a/b")), "~/a/b");
        assert_eq!(display_path(Path::new("/etc/hosts")), "/etc/hosts");
    }
}
