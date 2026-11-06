use super::*;

struct Fixture {
    _dir: tempfile::TempDir,
    shell: Shell,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut history = crate::history::History::new();
        history.db = Some(crate::db::Db::new(dir.path().join("history.db")).unwrap());
        history.add_test_entry("aggregate control");
        let mut shell = Shell::new(crate::environment::Environment::new());
        shell.session_id = "current-session".into();
        shell.cmd_history = Some(std::sync::Arc::new(parking_lot::Mutex::new(history)));
        Self { _dir: dir, shell }
    }

    fn insert(
        &self,
        command: &str,
        time: i64,
        author: &str,
        cwd: Option<&str>,
        session: Option<&str>,
        outcome: (Option<i32>, Option<i64>),
    ) {
        let history = self.shell.cmd_history.as_ref().unwrap().lock();
        history.db.as_ref().unwrap().get_connection().execute(
            "INSERT INTO command_events(command, started_at, author, cwd, session_id, exit_code, duration_ms, hostname, output) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'fixture-host', 'fixture-output')",
            rusqlite::params![command, time, author, cwd, session, outcome.0, outcome.1],
        ).unwrap();
    }

    fn run(&mut self, args: &[&str]) -> String {
        let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let observer = dsh_types::observed_output::ObservedOutput::shared(65536);
        ctx.output_observer = Some(observer.clone());
        let argv = std::iter::once("history")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect();
        execute(&mut self.shell, &ctx, argv).unwrap();
        observer.lock().unwrap().snapshot().stdout
    }

    fn events(&mut self, args: &[&str]) -> Vec<CommandEvent> {
        let mut args = args.to_vec();
        args.push("--json");
        serde_json::from_str(&self.run(&args)).unwrap()
    }
}

#[test]
fn author_combines_text_status_duration_session_before_limit() {
    let mut f = Fixture::new();
    f.insert(
        "ÉCHO Needle",
        1,
        "agent-x",
        None,
        Some("current-session"),
        (Some(1), Some(50)),
    );
    for time in 2..=210 {
        f.insert(
            "newer distractor",
            time,
            "agent-x",
            None,
            Some("current-session"),
            (Some(1), Some(50)),
        );
    }
    f.insert(
        "écho needle wrong author",
        211,
        "human",
        None,
        Some("current-session"),
        (Some(1), Some(50)),
    );
    f.insert(
        "écho needle wrong session",
        212,
        "agent-x",
        None,
        Some("other-session"),
        (Some(1), Some(50)),
    );
    f.insert(
        "écho needle success",
        213,
        "agent-x",
        None,
        Some("current-session"),
        (Some(0), Some(50)),
    );
    f.insert(
        "écho needle unknown status",
        214,
        "agent-x",
        None,
        Some("current-session"),
        (None, Some(50)),
    );
    f.insert(
        "écho needle fast",
        215,
        "agent-x",
        None,
        Some("current-session"),
        (Some(1), Some(49)),
    );
    f.insert(
        "écho needle unknown duration",
        216,
        "agent-x",
        None,
        Some("current-session"),
        (Some(1), None),
    );
    let events = f.events(&[
        "--author",
        "agent-x",
        "--query",
        "éCHO NEEDLE",
        "--scope",
        "session",
        "--status",
        "failure",
        "--slow",
        "50",
        "--limit",
        "1",
    ]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "ÉCHO Needle");
    assert_eq!(events[0].output.as_deref(), Some("fixture-output"));
    assert_eq!(events[0].hostname.as_deref(), Some("fixture-host"));
}

#[test]
fn author_all_success_and_zero_duration_preserve_order_and_unknown_semantics() {
    let mut f = Fixture::new();
    f.insert("match human", 1, "human", None, None, (Some(0), None));
    f.insert("match agent", 1, "agent-x", None, None, (Some(0), Some(0)));
    f.insert("match unknown", 2, "human", None, None, (None, Some(100)));
    f.insert(
        "match failure",
        3,
        "agent-x",
        None,
        None,
        (Some(2), Some(100)),
    );
    let events = f.events(&[
        "--author", "all", "match", "--status", "success", "--slow", "0",
    ]);
    assert_eq!(
        events
            .iter()
            .map(|e| e.command.as_str())
            .collect::<Vec<_>>(),
        ["match agent", "match human"]
    );
    let events = f.events(&["--author", "all", "--slow", "1", "--status", "success"]);
    assert!(events.is_empty());
}

#[test]
fn author_scope_cwd_and_project_use_event_cwd() {
    let mut f = Fixture::new();
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let nested = std::path::Path::new(&cwd)
        .join("dsh")
        .to_string_lossy()
        .into_owned();
    let outside = tempfile::tempdir().unwrap();
    f.insert("cwd match", 1, "agent-x", Some(&cwd), None, (None, None));
    f.insert(
        "nested match",
        2,
        "agent-x",
        Some(&nested),
        None,
        (None, None),
    );
    f.insert(
        "outside",
        3,
        "agent-x",
        outside.path().to_str(),
        None,
        (None, None),
    );
    f.insert("missing cwd", 4, "agent-x", None, None, (None, None));
    let events = f.events(&["--author", "agent-x", "--scope", "cwd", "-n", "1"]);
    assert_eq!(events[0].command, "cwd match");
    let events = f.events(&["--author", "agent-x", "--scope", "project"]);
    assert_eq!(
        events
            .iter()
            .map(|e| e.command.as_str())
            .collect::<Vec<_>>(),
        ["nested match", "cwd match"]
    );
}

#[test]
fn author_only_and_aggregate_controls_keep_schema_and_text_format() {
    let mut f = Fixture::new();
    f.insert(
        "event control",
        1,
        "agent-x",
        Some("/fixture"),
        None,
        (Some(0), None),
    );
    f.insert("other author", 2, "human", None, None, (None, None));
    let events = f.events(&["--author", "agent-x"]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "event control");
    assert_eq!(
        f.run(&["--author", "agent-x"]),
        "1\tagent-x\t0\t/fixture\tevent control\n"
    );
    let aggregate: serde_json::Value = serde_json::from_str(&f.run(&["--json"])).unwrap();
    assert_eq!(aggregate[0]["entry"], "aggregate control");
    assert!(aggregate[0].get("command").is_none());
    assert_eq!(f.run(&[]), "aggregate control\n");
    assert_eq!(
        f.run(&[
            "--author", "agent-x", "--query", "EVENT", "--status", "success"
        ]),
        "1\tagent-x\t0\t/fixture\tevent control\n"
    );
    let aggregate: serde_json::Value = serde_json::from_str(&f.run(&[
        "--json",
        "--query",
        "AgGrEgAtE",
        "--slow",
        "0",
        "--scope",
        "global",
    ]))
    .unwrap();
    assert_eq!(aggregate[0]["entry"], "aggregate control");
}
