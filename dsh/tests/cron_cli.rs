//! End-to-end regression coverage for `cron`'s CLI output.
//!
//! `common::run_dsh` gives each call its own throwaway `XDG_STATE_HOME`,
//! which is wrong here: these tests need one `cron add` to be visible to a
//! later `cron list`/`logs`/`show` in the *same* store, so they spawn `dsh -c`
//! directly against one shared temporary directory instead. Every dsh run
//! prints a leading `\r\n` in `-c` mode (a pre-existing terminal-negotiation
//! artifact, unrelated to cron) - callers strip it before asserting.

mod common;

use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("create sandbox dir"),
        }
    }

    /// Runs one `dsh -c "<command>"` against this sandbox's store, returning
    /// stdout with the leading `-c`-mode `\r\n` stripped.
    fn run(&self, command: &str) -> String {
        let child = Command::new(env!("CARGO_BIN_EXE_dogesh"))
            .args(["-c", command])
            .env("XDG_STATE_HOME", self.dir.path())
            .env("XDG_DATA_HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn dsh");
        let output = wait(child);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        stdout
            .strip_prefix("\r\n")
            .map(str::to_string)
            .unwrap_or(stdout)
    }
}

fn wait(mut child: Child) -> Output {
    if child
        .wait_timeout(Duration::from_secs(10))
        .expect("wait for dsh")
        .is_none()
    {
        let _ = child.kill();
        panic!("dsh did not exit within 10s");
    }
    child.wait_with_output().expect("collect dsh output")
}

/// Guards against a regression anywhere in the newline sweep: no cron
/// subcommand's output may end in more than one trailing newline, and none
/// may contain a blank line it did not ask for.
fn assert_single_trailing_newline(label: &str, text: &str) {
    assert!(
        text.ends_with('\n') && !text.ends_with("\n\n"),
        "{label}: expected exactly one trailing newline, got {text:?}"
    );
}

#[test]
fn cron_list_has_exactly_one_trailing_newline() {
    let sandbox = Sandbox::new();
    sandbox.run("cron add --name t1 30s 'echo hello'");
    let out = sandbox.run("cron list");
    assert_single_trailing_newline("cron list", &out);
    assert!(out.contains("t1"), "{out}");
}

#[test]
fn cron_status_and_incidents_and_doctor_have_exactly_one_trailing_newline() {
    let sandbox = Sandbox::new();
    sandbox.run("cron add --name t1 30s 'echo hello'");
    for command in [
        "cron status",
        "cron incidents",
        "cron doctor",
        "cron history",
    ] {
        let out = sandbox.run(command);
        assert_single_trailing_newline(command, &out);
    }
}

/// The bug this guards against: `cron logs job --stdout` used to pipe the
/// job's own output plus two extra blank lines (`stream_section` and
/// `write_stdout` each adding their own newline on top of the job's own).
#[test]
fn cron_logs_stdout_pipes_exactly_the_jobs_own_output() {
    let sandbox = Sandbox::new();
    sandbox.run("cron add --name t1 30s 'echo hello'");
    sandbox.run("cron run t1 --now");
    let out = sandbox.run("cron logs t1 --stdout");
    assert_eq!(out, "hello\n");
}

/// The bug this guards against: `cron show --json probe` read `--json`
/// itself as the job name (`args.first()` without stripping known flags
/// first), so the flag had to come after the job name to work at all.
#[test]
fn cron_show_json_works_with_the_flag_before_the_job_name() {
    let sandbox = Sandbox::new();
    sandbox.run("cron add --name t1 30s 'echo hello'");
    let out = sandbox.run("cron show --json t1");
    assert!(out.trim_start().starts_with('{'), "{out}");
    assert!(out.contains("\"name\": \"t1\""), "{out}");
}

#[test]
fn tick_dry_run_previews_the_real_claim_without_changing_the_store() {
    use doge_shell::cron::store::SqliteCronStore;
    use dsh_builtin::shell_capabilities::CronStore;
    use dsh_types::cron::job::{RunOutcome, RunQuery, RunTrigger};

    let sandbox = Sandbox::new();
    for name in ["later", "duplicate", "leased", "first", "expired"] {
        sandbox.run(&format!("cron add --name {name} 1h 'echo hello'"));
    }
    let store = SqliteCronStore::open(&sandbox.dir.path().join("dogesh/cron")).unwrap();
    let now = chrono::Utc::now().timestamp();
    store
        .claim_one("leased", now, "live", RunTrigger::Tick)
        .unwrap();
    store
        .claim_one("expired", now - 86400, "dead", RunTrigger::Tick)
        .unwrap();
    let duplicate = store
        .claim_one("duplicate", now - 20, "prior", RunTrigger::Tick)
        .unwrap();
    store
        .complete(&duplicate.run_id, &RunOutcome::default(), now - 19)
        .unwrap();
    for (name, offset) in [
        ("leased", 30),
        ("duplicate", 20),
        ("expired", 10),
        ("first", 5),
        ("later", 1),
    ] {
        store.trigger(name, now - offset).unwrap();
    }

    let jobs = store.list().unwrap();
    let runs = store.runs(&RunQuery::default()).unwrap();
    let preview = |limit| {
        let stdout = sandbox.run(&format!("cron tick --dry-run --max {limit} --json"));
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["would_run"].clone()
    };
    assert_eq!(preview(0), serde_json::json!([]));
    // The duplicate consumes a candidate slot before the unique-run constraint skips it.
    assert_eq!(preview(2), serde_json::json!(["expired"]));
    let names = preview(3);
    assert_eq!(names, serde_json::json!(["expired", "first"]));
    assert_eq!(store.list().unwrap(), jobs);
    assert_eq!(store.runs(&RunQuery::default()).unwrap(), runs);
    assert!(store.incidents(true, 10).unwrap().is_empty());

    let claimed = store.claim_due(now, "tick", 3, RunTrigger::Tick).unwrap();
    let claimed: Vec<_> = claimed.into_iter().map(|run| run.job_name).collect();
    assert_eq!(names, serde_json::json!(claimed));
}

/// The bug this guards against: `cron doctor`'s ok/warn counts were only
/// ever visible in `--json`; the human-readable listing gave no total.
#[test]
fn cron_doctor_reports_an_ok_warn_summary() {
    let sandbox = Sandbox::new();
    sandbox.run("cron add --name t1 30s 'echo hello'");
    let out = sandbox.run("cron doctor");
    assert!(out.contains(" ok, ") && out.contains(" warn"), "{out}");
}

/// The bug this guards against: a very long `command` had no clamp in
/// `cron list`, so one such job blew the table out to hundreds of columns.
#[test]
fn cron_list_clamps_a_very_long_command() {
    let sandbox = Sandbox::new();
    let long = "x".repeat(200);
    sandbox.run(&format!("cron add --name t1 30s 'echo {long}'"));
    let out = sandbox.run("cron list");
    assert!(!out.contains(&long), "{out}");
    assert!(out.contains("..."), "{out}");
}

/// The bug this guards against: `preview` (what `cron history`/`cron list`
/// show) never stripped ANSI escapes, so a job that coloured its output
/// broke the table's column-width math.
#[test]
fn cron_history_strips_ansi_from_the_preview() {
    let sandbox = Sandbox::new();
    sandbox.run("cron add --name t1 30s 'printf \"\\033[31mred-line\\033[0m\\n\"'");
    sandbox.run("cron run t1 --now");
    let out = sandbox.run("cron history");
    assert!(out.contains("red-line"), "{out}");
    assert!(!out.contains('\u{1b}'), "{out}");
}
