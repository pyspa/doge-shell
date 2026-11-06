//! Isolate fixtures that need exclusive ownership of process-global completion state.
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Returns true in the parent after the exact test succeeds in a fresh process.
/// The child returns false and runs the original fixture and all its assertions.
pub(crate) fn run_isolated(module: &str, function: &str) -> bool {
    const CHILD: &str = "DOGESH_COMPLETION_ISOLATED_TEST";
    let (_, module) = module
        .split_once("::")
        .expect("test module has crate prefix");
    let name = format!("{module}::{function}");
    let marker = format!("isolated completion fixture: {name}");
    if std::env::var(CHILD).ok().as_deref() == Some(name.as_str()) {
        println!("{marker}");
        return false;
    }
    // Files avoid a full output pipe blocking the child while the parent waits.
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture"])
        .env(CHILD, &name)
        .stdin(Stdio::null())
        .stdout(stdout.reopen().unwrap())
        .stderr(stderr.reopen().unwrap())
        .spawn()
        .expect("spawn isolated completion fixture");
    // This is a process hang ceiling; inner scanner/event deadlines stay intact.
    let waited = child.wait_timeout(Duration::from_secs(15));
    if !matches!(waited, Ok(Some(_))) {
        let _ = child.kill();
        child.wait().expect("reap isolated completion fixture");
    }
    let stdout = std::fs::read_to_string(stdout.path()).unwrap();
    let stderr = std::fs::read_to_string(stderr.path()).unwrap();
    assert!(
        matches!(waited, Ok(Some(status)) if status.success()),
        "isolated completion fixture {name}: {waited:?}\n{stdout}\n{stderr}"
    );
    // A misspelled --exact filter otherwise succeeds with zero executed tests.
    assert!(
        stdout.contains(&marker),
        "isolated completion fixture did not run: {name}\n{stdout}\n{stderr}"
    );
    true
}
