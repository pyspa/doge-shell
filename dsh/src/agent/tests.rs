use super::*;
use dsh_builtin::{
    ShellProxy,
    agent::{ToolOutcome, task_tool},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};

fn task(root: &Path) -> AgentTask {
    AgentTask {
        id: uuid::Uuid::new_v4().to_string(),
        goal: "create and verify a greeting".into(),
        root: root.canonicalize().unwrap(),
        status: TaskStatus::Running,
        grant: TaskGrant {
            read_roots: vec![root.canonicalize().unwrap()],
            write_roots: vec![root.canonicalize().unwrap()],
            ..Default::default()
        },
        criteria: vec![Verification {
            criterion: "greeting contains hello".into(),
            passed: false,
            evidence_event: None,
        }],
        plan: vec!["write and read back".into()],
        progress: String::new(),
        token_budget: 10000,
        tokens_used: 0,
        time_budget_ms: 30000,
        elapsed_ms: 0,
        stop_reason: None,
        checkpoint: None,
        pending_operation: None,
        created_at: 0,
    }
}
#[test]
fn durable_intent_results_redaction_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut task = task(dir.path());
    task.pending_operation =
        Some(json!({"function":{"name":"execute","arguments":"API_KEY=not-for-disk"}}));
    store
        .save(
            &task,
            Some(("tool_intent", &task.pending_operation.clone().unwrap())),
        )
        .unwrap();
    drop(store);
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let restored = store.load(&task.id).unwrap();
    assert!(restored.pending_operation.is_some());
    assert!(
        !serde_json::to_string(&restored)
            .unwrap()
            .contains("not-for-disk")
    );
    assert_eq!(store.events(&task.id).unwrap().len(), 1);
    assert_eq!(
        std::fs::metadata(dir.path().join("state/tasks.sqlite3"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    store.delete(&task.id).unwrap();
    assert!(store.load(&task.id).is_err());
    assert!(store.events(&task.id).unwrap().is_empty());
}
#[test]
fn cancellation_wins_and_execution_lock_is_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let first = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let second = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let task = task(dir.path());
    first.save(&task, None).unwrap();
    let lock = first.execution_lock().unwrap();
    assert!(second.execution_lock().is_err());
    drop(lock);
    assert!(second.execution_lock().is_ok());
    let mut cancelled = task.clone();
    cancelled.status = TaskStatus::Cancelled;
    second.save(&cancelled, None).unwrap();
    let late = first.save(&task, None).unwrap();
    assert_eq!(late.task.status, TaskStatus::Cancelled);
    assert_eq!(first.load(&task.id).unwrap().status, TaskStatus::Cancelled);
}
#[test]
fn verification_rejects_failed_fabricated_stale_and_running_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
    let task = task(dir.path());
    store.save(&task, None).unwrap();
    let mut runtime = AgentRuntime::new(task, store);
    assert!(
        task_tool(
            &mut runtime,
            "task_verify",
            &json!({"criterion":0,"evidence_event":999,"explanation":"looks good"})
        )
        .is_err()
    );
    let call = json!({"function":{"name":"execute","arguments":"{}"}});
    let failed = runtime
        .after_tool(&call, "test failed", ToolOutcome::Failure)
        .unwrap();
    assert!(
        task_tool(
            &mut runtime,
            "task_verify",
            &json!({"criterion":0,"evidence_event":failed,"explanation":"passed"})
        )
        .is_err()
    );
    let pending = runtime
        .after_tool(&call, "{\"status\":\"running\"}", ToolOutcome::Success)
        .unwrap();
    assert!(
        task_tool(
            &mut runtime,
            "task_verify",
            &json!({"criterion":0,"evidence_event":pending,"explanation":"started"})
        )
        .is_err()
    );
    let success = runtime
        .after_tool(&call, "{\"exit_code\":0}", ToolOutcome::Success)
        .unwrap();
    task_tool(
        &mut runtime,
        "task_verify",
        &json!({"criterion":0,"evidence_event":success,"explanation":"checked exit code"}),
    )
    .unwrap();
    assert!(runtime.task.verified());
    runtime.before_tool(&call, Value::Null).unwrap();
    assert!(!runtime.task.verified());
    assert!(
        task_tool(
            &mut runtime,
            "task_verify",
            &json!({"criterion":0,"evidence_event":success,"explanation":"reuse old result"})
        )
        .is_err()
    );
}
#[test]
fn unknown_outcome_and_budget_never_complete() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
    let task = task(dir.path());
    store.save(&task, None).unwrap();
    let mut runtime = AgentRuntime::new(task, store);
    let call = json!({"function":{"name":"mcp__service__write","arguments":"{}"}});
    runtime.before_tool(&call, Value::Null).unwrap();
    let known_failure = runtime
        .after_tool(
            &call,
            "Error: document literally says outcome unknown",
            ToolOutcome::Failure,
        )
        .unwrap();
    assert_eq!(runtime.task.status, TaskStatus::Running);
    assert!(runtime.task.pending_operation.is_none());
    let event = runtime
        .store
        .events(&runtime.task.id)
        .unwrap()
        .into_iter()
        .find(|event| event.sequence == known_failure)
        .unwrap();
    assert_eq!(event.data["outcome"], "failure");

    runtime.before_tool(&call, Value::Null).unwrap();
    let unknown = runtime
        .after_tool(
            &call,
            "Error: connection lost; outcome unknown",
            ToolOutcome::OutcomeUnknown,
        )
        .unwrap();
    assert!(runtime.stopped());
    assert!(runtime.task.pending_operation.is_some());
    assert_eq!(runtime.task.status, TaskStatus::InputRequired);
    let event = runtime
        .store
        .events(&runtime.task.id)
        .unwrap()
        .into_iter()
        .find(|event| event.sequence == unknown)
        .unwrap();
    assert_eq!(event.data["outcome"], "unknown");
    runtime.finish(false, None).unwrap();
    assert_ne!(runtime.task.status, TaskStatus::Completed);
}

#[test]
fn successful_content_cannot_trigger_unknown_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
    let saved = task(dir.path());
    store.save(&saved, None).unwrap();
    let mut runtime = AgentRuntime::new(saved, store);
    let call = json!({"function":{"name":"read_file","arguments":"{}"}});
    runtime.before_tool(&call, Value::Null).unwrap();
    let sequence = runtime
        .after_tool(
            &call,
            "The source text contains outcome unknown.",
            ToolOutcome::Success,
        )
        .unwrap();

    assert_eq!(runtime.task.status, TaskStatus::Running);
    assert!(runtime.task.pending_operation.is_none());
    let event = runtime
        .store
        .events(&runtime.task.id)
        .unwrap()
        .into_iter()
        .find(|event| event.sequence == sequence)
        .unwrap();
    assert_eq!(event.data["outcome"], "success");
    assert_eq!(event.data["failed"], false);
}

fn request(stream: &mut TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let header = String::from_utf8(bytes).unwrap();
    let size = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}
fn reply(stream: &mut TcpStream, message: Value) {
    let reason = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    let body=json!({"choices":[{"message":message,"finish_reason":reason}],"usage":{"prompt_tokens":100,"completion_tokens":20}}).to_string();
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
}
fn tool(name: &str, args: Value, step: usize) -> Value {
    json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("call_{step}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}

#[test]
fn shared_loop_writes_verifies_and_persists_without_real_api() {
    let dir = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&state.path().join("state")).unwrap());
    let mut task = task(dir.path());
    task.token_budget = 120;
    let id = task.id.clone();
    store.save(&task, None).unwrap();
    let file = dir.path().join("hello.txt");
    let file_for_server = file.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for step in 0..4 {
            let started = std::time::Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && started.elapsed() < Duration::from_secs(10) =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => panic!("fixture did not receive request {step}: {e}"),
                }
            };
            let input = request(&mut stream);
            // Only builtin task tools initially; no eager external tool catalog.
            assert!(
                input["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["function"]["name"] == "tool_search")
            );
            let message = match step {
                0 => tool(
                    "edit",
                    json!({"path":file_for_server,"contents":"hello\n"}),
                    step,
                ),
                1 => tool("read_file", json!({"path":file_for_server}), step),
                2 => {
                    let content = input["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .rev()
                        .find(|m| m["role"] == "tool")
                        .unwrap()["content"]
                        .as_str()
                        .unwrap();
                    let event = content
                        .rsplit("[task event ")
                        .next()
                        .unwrap()
                        .trim_end_matches(']')
                        .parse::<u64>()
                        .unwrap();
                    tool(
                        "task_verify",
                        json!({"criterion":0,"evidence_event":event,"explanation":"read back hello"}),
                        step,
                    )
                }
                _ => json!({"role":"assistant","content":"Verified greeting"}),
            };
            reply(&mut stream, message);
        }
    });
    let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
    for (key, value) in [
        ("AI_CHAT_API_KEY", "fixture-key"),
        ("AI_CHAT_BASE_URL", &url),
        ("AI_CHAT_ALLOW_INSECURE_HTTP", "1"),
        ("AI_CHAT_STREAM", "0"),
    ] {
        shell.set_var(key.into(), value.into());
    }
    assert_eq!(
        dsh_builtin::agent::resolved_config(&mut shell).base_url(),
        url
    );
    shell.agent_runtime = Some(Arc::new(Mutex::new(AgentRuntime::new(task, store.clone()))));
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.interactive = false;
    let status =
        dsh_builtin::execute_chat_message(&ctx, &mut shell, "create and verify a greeting", None);
    assert_ne!(status, dsh_types::ExitStatus::ExitedWith(0));
    shell.agent_runtime.take();
    let mut resumed = store.load(&id).unwrap();
    assert_eq!(resumed.tokens_used, 120);
    assert_eq!(resumed.status, TaskStatus::Interrupted);
    assert!(resumed.pending_operation.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
    resumed.status = TaskStatus::Running;
    resumed.token_budget = 10000;
    store.save(&resumed, None).unwrap();
    shell.agent_runtime = Some(Arc::new(Mutex::new(AgentRuntime::new(
        resumed,
        store.clone(),
    ))));
    let status =
        dsh_builtin::execute_chat_message(&ctx, &mut shell, "create and verify a greeting", None);
    server.join().unwrap();
    assert_eq!(status, dsh_types::ExitStatus::ExitedWith(0));
    assert_eq!(std::fs::read_to_string(file).unwrap(), "hello\n");
    let restored = store.load(&id).unwrap();
    assert_eq!(restored.status, TaskStatus::Completed);
    assert_eq!(restored.tokens_used, 480);
    assert!(restored.verified());
    assert!(restored.checkpoint.is_some());
}

#[test]
fn recovery_preserves_budgets_and_unknown_intent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
    let mut saved = task(dir.path());
    saved.tokens_used = 120;
    saved.elapsed_ms = 1000;
    saved.pending_operation = Some(json!({"id":"lost", "function":{"name":"edit"}}));
    store.save(&saved, None).unwrap();
    let lock = store.execution_lock().unwrap();
    store.recover_interrupted().unwrap();
    assert_eq!(store.load(&saved.id).unwrap().status, TaskStatus::Running);
    drop(lock);
    store.recover_interrupted().unwrap();
    let mut restored = store.load(&saved.id).unwrap();
    assert_eq!(restored.status, TaskStatus::Interrupted);
    assert_eq!(restored.tokens_used, 120);
    assert_eq!(restored.elapsed_ms, 1000);
    assert!(restored.pending_operation.is_some());
    restored.status = TaskStatus::Running;
    restored.tokens_used = restored.token_budget;
    restored.criteria[0].passed = true;
    restored.criteria[0].evidence_event = Some(1);
    let mut runtime = AgentRuntime::new(restored, store);
    assert!(runtime.stopped());
    runtime.finish(true, None).unwrap();
    assert_ne!(runtime.task.status, TaskStatus::Completed);
}

#[test]
fn remote_handles_use_flattened_mcp_wire_format_and_require_terminal_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
    let saved = task(dir.path());
    store.save(&saved, None).unwrap();
    let mut runtime = AgentRuntime::new(saved, store);
    let call = json!({"function":{"name":"mcp__server__build","arguments":"{}"}});
    runtime.after_tool(&call, &json!({"server":"server","response":{"resultType":"task","taskId":"remote-1","status":"working"}}).to_string(), ToolOutcome::Success).unwrap();
    let events = runtime.store.events(&runtime.task.id).unwrap();
    assert!(dsh_builtin::agent::has_remote_task(
        &events, "server", "remote-1"
    ));
    assert!(!dsh_builtin::agent::has_remote_task(
        &events, "another", "remote-1"
    ));
    assert!(dsh_builtin::agent::pending_remote_tasks(&events));
    let poll = json!({"function":{"name":"mcp_task_status","arguments":json!({"server":"server","task_id":"remote-1"}).to_string()}});
    let evidence = runtime
        .after_tool(
            &poll,
            &json!({"taskId":"remote-1","status":"completed","result":{"isError":false}})
                .to_string(),
            ToolOutcome::Success,
        )
        .unwrap();
    assert!(!dsh_builtin::agent::pending_remote_tasks(
        &runtime.store.events(&runtime.task.id).unwrap()
    ));
    task_tool(
        &mut runtime,
        "task_verify",
        &json!({"criterion":0,"evidence_event":evidence,"explanation":"remote result verified"}),
    )
    .unwrap();
}

#[test]
fn job_artifacts_are_private_redacted_and_deleted_with_task() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let saved = task(dir.path());
    store.save(&saved, None).unwrap();
    let job = uuid::Uuid::new_v4().to_string();
    store
        .save_artifact(&saved.id, &job, &json!({"stdout":"API_KEY=do-not-save"}))
        .unwrap();
    let path = store.root.join(&saved.id).join(format!("{job}.json"));
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("do-not-save")
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        store
            .save_artifact("../escape", &job, &Value::Null)
            .is_err()
    );
    store.delete(&saved.id).unwrap();
    assert!(!path.exists());
}

#[test]
fn restart_requires_reconciliation_for_unfinished_processes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let saved = task(dir.path());
    store.save(&saved, Some(("tool_result", &json!({"result":json!({"job_id":"job-1","status":"running","pid":12345}).to_string()})))).unwrap();
    store.recover_interrupted().unwrap();
    let restored = store.load(&saved.id).unwrap();
    assert_eq!(
        restored.pending_operation.unwrap()["unfinished_jobs"],
        json!(["job-1"])
    );
    store
        .save(
            &saved,
            Some((
                "job_artifact",
                &json!({"job_id":"job-1","status":"cancelled"}),
            )),
        )
        .unwrap();
    store.recover_interrupted().unwrap();
    assert!(store.load(&saved.id).unwrap().pending_operation.is_none());
}

#[test]
fn cancellation_preserves_checkpoint_published_after_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let writer = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let canceller = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut latest = task(dir.path());
    writer.save(&latest, None).unwrap();
    let stale = canceller.load(&latest.id).unwrap();
    latest.tokens_used = 900;
    latest.elapsed_ms = 3000;
    latest.pending_operation = Some(json!({"id":"new-call","function":{"name":"execute"}}));
    latest.checkpoint = Some(json!({"current":"new checkpoint"}));
    writer
        .save(
            &latest,
            Some(("tool_intent", &latest.pending_operation.clone().unwrap())),
        )
        .unwrap();
    canceller.cancel(&stale.id).unwrap();
    let saved = writer.load(&latest.id).unwrap();
    assert_eq!(saved.status, TaskStatus::Cancelled);
    assert_eq!(saved.tokens_used, 900);
    assert_eq!(saved.elapsed_ms, 3000);
    assert_eq!(saved.pending_operation, latest.pending_operation);
    assert_eq!(saved.checkpoint, latest.checkpoint);
    latest.status = TaskStatus::Completed;
    latest.pending_operation = None;
    let late = writer.save(&latest, None).unwrap();
    assert_eq!(late.task.status, TaskStatus::Cancelled);
    assert_eq!(late.task.tokens_used, 900);
    assert!(late.task.pending_operation.is_none());
    assert_eq!(
        writer.load(&latest.id).unwrap().status,
        TaskStatus::Cancelled
    );

    let mut resumed = late.task;
    resumed.status = TaskStatus::Running;
    resumed.stop_reason = None;
    let normal = writer.save(&resumed, None).unwrap();
    assert_eq!(normal.task.status, TaskStatus::Cancelled);
    let explicit = writer
        .resume(&resumed, Some(("started", &Value::Null)))
        .unwrap();
    assert_eq!(explicit.task.status, TaskStatus::Running);
    assert_eq!(writer.load(&latest.id).unwrap().status, TaskStatus::Running);
}

#[test]
fn cancellation_wins_over_late_result_and_finish() {
    let dir = tempfile::tempdir().unwrap();
    let writer = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
    let canceller = SqliteTaskStore::open(&dir.path().join("state")).unwrap();
    let mut saved = task(dir.path());
    saved.criteria[0].passed = true;
    saved.criteria[0].evidence_event = Some(1);
    writer.save(&saved, None).unwrap();
    let mut runtime = AgentRuntime::new(saved, writer.clone());
    let call = json!({"function":{"name":"read_file","arguments":"{}"}});
    runtime
        .before_tool(&call, json!({"checkpoint":"latest"}))
        .unwrap();

    canceller.cancel(&runtime.task.id).unwrap();
    runtime
        .after_tool(&call, "finished reading", ToolOutcome::Success)
        .unwrap();
    assert_eq!(runtime.task.status, TaskStatus::Cancelled);
    assert!(runtime.task.pending_operation.is_none());

    runtime.finish(true, Some("work finished".into())).unwrap();
    let persisted = writer.load(&runtime.task.id).unwrap();
    assert_eq!(runtime.task.status, TaskStatus::Cancelled);
    assert_eq!(persisted.status, TaskStatus::Cancelled);
    assert_eq!(persisted.stop_reason.as_deref(), Some("cancelled by user"));
    assert_eq!(persisted.checkpoint, Some(json!({"checkpoint":"latest"})));
    let stopped = writer
        .events(&persisted.id)
        .unwrap()
        .into_iter()
        .rev()
        .find(|event| event.kind == "stopped")
        .unwrap();
    assert_eq!(stopped.data["status"], "cancelled");
    assert_eq!(stopped.data["reason"], "cancelled by user");
}

#[test]
fn summary_budget_and_missing_usage_stop_before_another_request() {
    for missing_usage in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state")).unwrap());
        let mut saved = task(dir.path());
        saved.token_budget = 100;
        saved.checkpoint = Some(json!({
            "summary":null,"buffer":[{"role":"user","content":"context ".repeat(10000)}],
            "buffer_chars":80000,"last_prompt_tokens":200000,"prompt_token_budget":100000,
            "turn_usage":{"requests":0,"prompt_tokens":0,"cached_prompt_tokens":0,"completion_tokens":0},
            "pinned_messages":[{"role":"system","content":"system"},{"role":"user","content":"goal"}]
        }));
        let id = saved.id.clone();
        store.save(&saved, None).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(10));
            };
            let input = request(&mut stream);
            assert!(
                input["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("summarizer")
            );
            let mut body = json!({"choices":[{"message":{"role":"assistant","content":"short summary"},"finish_reason":"stop"}]});
            if !missing_usage {
                body["usage"] = json!({"prompt_tokens":100,"completion_tokens":20});
            }
            let body = body.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            drop(stream);
            let until = std::time::Instant::now() + Duration::from_millis(300);
            while std::time::Instant::now() < until {
                assert!(
                    listener.accept().is_err(),
                    "unexpected request after summary stop"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut shell = crate::shell::Shell::new(crate::environment::Environment::new());
        for (key, value) in [
            ("AI_CHAT_API_KEY", "fixture-key"),
            ("AI_CHAT_BASE_URL", &url),
            ("AI_CHAT_ALLOW_INSECURE_HTTP", "1"),
            ("AI_CHAT_STREAM", "0"),
        ] {
            shell.set_var(key.into(), value.into());
        }
        assert_eq!(
            dsh_builtin::agent::resolved_config(&mut shell).base_url(),
            url
        );
        shell.agent_runtime = Some(Arc::new(Mutex::new(AgentRuntime::new(
            saved,
            store.clone(),
        ))));
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        assert_ne!(
            dsh_builtin::execute_chat_message(&ctx, &mut shell, "goal", None),
            dsh_types::ExitStatus::ExitedWith(0)
        );
        server.join().unwrap();
        let saved = store.load(&id).unwrap();
        if missing_usage {
            assert!(
                saved
                    .stop_reason
                    .unwrap()
                    .contains("summary provider omitted")
            );
        } else {
            assert_eq!(saved.tokens_used, 120);
            assert_eq!(saved.status, TaskStatus::Interrupted);
        }
    }
}
