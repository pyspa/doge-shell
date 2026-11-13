use super::*;
use nix::libc;
use std::{fs, io::Read, os::unix::fs::symlink};

struct Fixture {
    _temp: tempfile::TempDir,
    public: PathBuf,
    outside: PathBuf,
    root: ServeRoot,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let public = base.join("public");
        let outside = base.join("outside");
        fs::create_dir_all(public.join("sub")).unwrap();
        fs::create_dir(&outside).unwrap();
        for name in ["payload.txt", "index.html"] {
            fs::write(public.join("sub").join(name), "PUBLIC").unwrap();
            fs::write(outside.join(name), "OUTSIDE_SENTINEL").unwrap();
        }
        let root = ServeRoot::new(&public).unwrap();
        Self {
            _temp: temp,
            public,
            outside,
            root,
        }
    }
}

#[test]
fn resolved_file_parent_and_index_swaps_never_open_outside() {
    for case in ["file", "parent", "index"] {
        let f = Fixture::new();
        let name = if case == "index" {
            "index.html"
        } else {
            "payload.txt"
        };
        let relative = f.root.resolve(&PathBuf::from("sub").join(name)).unwrap();
        // The exact check/use boundary from the reported exploit: a name was
        // resolved inside the public tree, but has not been opened yet.
        if case == "parent" {
            fs::rename(f.public.join("sub"), f.public.join("original")).unwrap();
            symlink(&f.outside, f.public.join("sub")).unwrap();
        } else {
            fs::remove_file(f.public.join("sub").join(name)).unwrap();
            symlink(f.outside.join(name), f.public.join("sub").join(name)).unwrap();
        }
        assert!(f.root.open_resolved(&relative).is_err(), "{case}");
    }
}

#[test]
fn opened_file_and_listing_survive_parent_replacement() {
    let f = Fixture::new();
    let mut file = f.root.open(Path::new("sub/payload.txt")).unwrap().file;
    let directory = f.root.open(Path::new("sub")).unwrap().file;
    fs::write(f.outside.join("private-name"), "secret").unwrap();
    fs::rename(f.public.join("sub"), f.public.join("original")).unwrap();
    symlink(&f.outside, f.public.join("sub")).unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "PUBLIC");
    let entries = files::list_open_directory(directory).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(!entries.iter().any(|entry| entry.name == "private-name"));
}

#[test]
fn root_handle_never_retargets_to_a_replacement_directory() {
    let f = Fixture::new();
    let relative = f.root.resolve(Path::new("sub/payload.txt")).unwrap();
    let original = f.public.with_file_name("original");
    fs::rename(&f.public, &original).unwrap();
    fs::create_dir_all(f.public.join("sub")).unwrap();
    fs::write(f.public.join("sub/payload.txt"), "REPLACEMENT").unwrap();
    for mut file in [
        f.root.open_resolved(&relative).unwrap(),
        f.root.open(&relative).unwrap().file,
    ] {
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "PUBLIC");
    }
}

#[test]
fn repeated_and_concurrent_root_listings_return_every_entry() {
    let f = Fixture::new();
    for n in 0..128 {
        fs::write(f.public.join(format!("entry-{n}")), "public").unwrap();
    }
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let root = &f.root;
            scope.spawn(move || {
                for _ in 0..3 {
                    let opened = root.open(Path::new("")).unwrap();
                    let entries = root.list(opened.file, &opened.relative).unwrap();
                    assert_eq!(entries.len(), 129);
                    assert!(entries.iter().any(|entry| entry.name == "sub"));
                }
            });
        }
    });
}

#[test]
fn internal_links_work_but_external_and_dangling_indexes_fail() {
    let f = Fixture::new();
    symlink("sub", f.public.join("alias")).unwrap();
    let directory = f.root.open(Path::new("alias")).unwrap();
    assert!(f.root.index(&directory).unwrap().is_some());
    let index = f.public.join("sub/index.html");
    fs::remove_file(&index).unwrap();
    assert!(f.root.index(&directory).unwrap().is_none());
    for target in [f.outside.join("index.html"), f.public.join("missing")] {
        symlink(target, &index).unwrap();
        assert!(f.root.index(&directory).is_err());
        fs::remove_file(&index).unwrap();
    }
    symlink("payload.txt", &index).unwrap();
    let mut file = f.root.index(&directory).unwrap().unwrap().file;
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "PUBLIC");
}

#[test]
fn relative_open_refuses_escapes_and_special_files() {
    let f = Fixture::new();
    for path in [Path::new("../outside/payload.txt"), f.outside.as_path()] {
        assert!(f.root.open_resolved(path).is_err());
    }
    let fifo = f.public.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    // O_NONBLOCK means this never waits for a writer.
    assert!(f.root.open(Path::new("fifo")).is_err());
}

#[test]
fn readable_index_in_search_only_directory_does_not_require_listing_permission() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let sub = f.public.join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o111)).unwrap();
    // Restore permissions before any assertions so TempDir can clean up on
    // both supported platforms, even if directory opening regresses.
    let result = f
        .root
        .open(Path::new("sub"))
        .and_then(|dir| f.root.index(&dir));
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
    let mut file = result.unwrap().unwrap().file;
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "PUBLIC");
}

#[test]
fn rejected_search_only_listing_closes_its_descriptor() {
    use std::{
        os::{fd::AsRawFd, unix::fs::PermissionsExt},
        process::{Command, Stdio},
    };
    // Isolate the fd-table assertion from all other tests allocating fds.
    const CHILD: &str = "DOGESH_SERVE_FD_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "serve::root::tests::rejected_search_only_listing_closes_its_descriptor",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    if unsafe { libc::geteuid() } == 0 {
        return; // root bypasses read-permission denial; CI runs unprivileged.
    }
    let f = Fixture::new();
    let sub = f.public.join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o111)).unwrap();
    let opened = f.root.open(Path::new("sub"));
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
    let opened = opened.unwrap();
    let fd = opened.file.as_raw_fd();
    assert!(f.root.list(opened.file, &opened.relative).is_err());
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
}
