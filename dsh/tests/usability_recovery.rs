//! CLI regressions exercised in a private HOME and XDG tree.
mod common;

use common::process::{DEFAULT_CASE_TIMEOUT, contract_env, wait_child_with_output_deadline};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

fn run(dir: &TempDir, args: &[&str]) -> Output {
    let _guard = common::serial_guard();
    let child = Command::new(env!("CARGO_BIN_EXE_dogesh"))
        .args(args)
        .env_clear()
        .envs(contract_env(dir))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = nix::unistd::Pid::from_raw(child.id() as i32);
    wait_child_with_output_deadline(child, pgid, DEFAULT_CASE_TIMEOUT).unwrap()
}

fn command(dir: &TempDir, script: &str) -> Output {
    run(dir, &["-c", script])
}

fn report(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn history_path(dir: &TempDir) -> PathBuf {
    dir.path().join("data/dogesh/dsh_cmd_history.db")
}

#[test]
fn command_mode_queries_saved_history_without_recording_the_query() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fish_history");
    std::fs::write(
        &source,
        "- cmd: echo test-history-entry\n  when: 1720000000\n",
    )
    .unwrap();
    let imported = run(
        &dir,
        &["import", "fish", "--path", source.to_str().unwrap()],
    );
    assert!(imported.status.success(), "{imported:?}");

    let entries = report(&command(&dir, "history --json"));
    assert_eq!(entries.as_array().unwrap().len(), 1, "{entries}");
    assert_eq!(entries[0]["entry"], "echo test-history-entry");
    assert_eq!(
        report(&command(&dir, "history --scope session --json")),
        serde_json::json!([])
    );
    assert_eq!(report(&command(&dir, "history --json")), entries);
}

#[test]
fn history_help_works_without_opening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let output = command(&dir, "history --help");
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Usage: history"),
        "{output:?}"
    );
    assert!(!history_path(&dir).exists());
    assert_eq!(
        report(&command(&dir, "history --json")),
        serde_json::json!([])
    );
    assert!(!history_path(&dir).exists());
}

#[test]
fn command_mode_reports_unreadable_history_as_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "invalid sqlite data").unwrap();
    let output = command(&dir, "history --json");
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("history:"),
        "{output:?}"
    );
}

#[test]
fn saved_history_reads_a_legacy_database_without_changing_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE command_history (id INTEGER PRIMARY KEY, command TEXT, timestamp INTEGER);
         INSERT INTO command_history(command, timestamp) VALUES
            ('echo legacy-entry', 1720000000), ('echo newer', 1720000001), ('! old chat', 1720000002);",
    )
    .unwrap();
    drop(db);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let before = std::fs::read(&path).unwrap();

    let entries = report(&command(&dir, "history legacy --limit 1 --json"));
    assert_eq!(entries.as_array().unwrap().len(), 1, "{entries}");
    assert_eq!(entries[0]["entry"], "echo legacy-entry");
    assert_eq!(entries[0]["count"], 1);
    assert_eq!(entries[0]["cwd"], serde_json::Value::Null);
    let all = report(&command(&dir, "history --json"));
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!(all[0]["entry"], "echo newer");
    assert_eq!(
        report(&command(&dir, "history --author all --json")),
        serde_json::json!([])
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o400);
}

#[test]
fn saved_history_reports_a_row_decoding_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE command_history (
             id INTEGER PRIMARY KEY, command TEXT, timestamp INTEGER, exit_code INTEGER
         );
         INSERT INTO command_history(command, timestamp, exit_code)
             VALUES ('echo invalid-date', 'invalid', 0), ('echo invalid-status', 1, 'invalid');",
    )
    .unwrap();
    drop(db);

    let output = command(&dir, "history --json");
    assert_failed_history_read(&output);
    let db = rusqlite::Connection::open(history_path(&dir)).unwrap();
    db.execute(
        "DELETE FROM command_history WHERE command = 'echo invalid-date'",
        [],
    )
    .unwrap();
    drop(db);
    assert_failed_history_read(&command(&dir, "history --status failure --json"));
}

fn assert_failed_history_read(output: &Output) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot read saved history"),
        "{output:?}"
    );
}

#[test]
fn saved_history_reads_committed_wal_entries_and_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = doge_shell::db::Db::new(path).unwrap();
    let connection = db.get_connection();
    connection.execute_batch(
        "INSERT INTO command_history (command, timestamp, is_command) VALUES ('echo aggregate', 1, 1);
         INSERT INTO command_events (command, started_at, author, is_command) VALUES
             ('echo event', 1, 'fixture', 1), ('echo unrelated', 2, 'other', 1);",
    ).unwrap();
    let entries = report(&command(&dir, "history --json"));
    assert_eq!(entries[0]["entry"], "echo aggregate");
    let events = report(&command(&dir, "history --author fixture --json"));
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["command"], "echo event");
}

#[test]
fn doctor_config_reports_startup_result_in_text_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config/dogesh/config.lisp");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let missing = report(&command(&dir, "doctor config --json"));
    assert_eq!(missing["details"]["config"]["load_status"], "missing");

    std::fs::write(&path, "(pref-status-line t)\n(unknown-fixture-function)\n").unwrap();
    let failed = report(&command(&dir, "doctor config --json"));
    assert_eq!(
        failed["details"]["config"]["load_status"],
        "evaluation_failed"
    );
    assert_eq!(failed["details"]["effective"]["status_line"], false);
    let text = command(&dir, "doctor config");
    assert!(text.status.success(), "{text:?}");
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("warn config evaluation-failed"),
        "{text:?}"
    );
    assert!(
        !String::from_utf8_lossy(&text.stdout).contains("ok config "),
        "{text:?}"
    );

    std::fs::write(&path, "(pref-status-line t)\n").unwrap();
    let loaded = report(&command(&dir, "doctor config --json"));
    assert_eq!(loaded["details"]["config"]["load_status"], "loaded");
    assert_eq!(loaded["details"]["effective"]["status_line"], true);

    std::fs::write(&path, [0xff]).unwrap();
    let unreadable = report(&command(&dir, "doctor config --json"));
    assert_eq!(
        unreadable["details"]["config"]["load_status"],
        "read_failed"
    );
}

#[test]
fn doctor_config_tracks_reload_and_reports_the_effective_values_after_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config/dogesh/config.lisp");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "(pref-status-line t)\n").unwrap();
    std::fs::write(
        dir.path().join("broken.lisp"),
        "(pref-status-line f) (unknown-fixture-function)\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("working.lisp"), "(pref-status-line f)\n").unwrap();

    let failed_reload = report(&command(
        &dir,
        "cp broken.lisp config/dogesh/config.lisp && reload; doctor config --json",
    ));
    assert_eq!(
        failed_reload["details"]["config"]["load_status"],
        "evaluation_failed"
    );
    assert_eq!(failed_reload["details"]["effective"]["status_line"], true);

    let recovered = report(&command(
        &dir,
        "cp working.lisp config/dogesh/config.lisp && reload > /dev/null && doctor config --json",
    ));
    assert_eq!(recovered["details"]["config"]["load_status"], "loaded");
    assert_eq!(recovered["details"]["effective"]["status_line"], false);
}
