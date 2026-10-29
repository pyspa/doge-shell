//! Exercise trigger through a real watcher and the shell capture boundary.
mod common;

use std::time::{Duration, Instant};

#[test]
fn relative_watch_preserves_literal_arguments() {
    assert_watch_preserves_arguments(std::ffi::OsStr::new("change.txt"));
}

#[test]
fn relative_watch_matches_non_utf8_filename() {
    use std::os::unix::ffi::OsStrExt;
    assert_watch_preserves_arguments(std::ffi::OsStr::from_bytes(b"raw-\xff.txt"));
}

fn assert_watch_preserves_arguments(filename: &std::ffi::OsStr) {
    let _serial = common::serial_guard();
    let script = r#"mkdir src; trigger 'src/*.txt' printf '<%s>\n' '' 'a b' "a'b" 'SAFE; printf EXTRA_COMMAND' '$HOME' '$(pwd)' '*.txt' "trailing\\" "backslash\\'quote" "last\\" > watcher.log"#;
    let mut child = common::process::spawn_dsh_unlocked(["-c", script], None);
    let log = child.workdir().join("watcher.log");
    let deadline = Instant::now() + Duration::from_secs(8);
    let wait_for = |needle: &str| {
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text.contains(needle) {
                return text;
            }
            assert!(Instant::now() < deadline, "missing {needle:?}: {text}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    // This message is emitted only after watch registration succeeds.
    wait_for("Press Ctrl+C to stop.");
    std::fs::write(child.workdir().join("src").join(filename), "change").unwrap();
    let text = wait_for("[trigger] Success");
    child.kill_group();
    child.wait(Duration::from_secs(3)).unwrap();
    assert!(text.contains("<>\n<a b>\n<a'b>\n<SAFE; printf EXTRA_COMMAND>\n<$HOME>\n<$(pwd)>\n<*.txt>\n<trailing\\>\n<backslash\\'quote>\n<last\\>\n"), "{text}");
}
