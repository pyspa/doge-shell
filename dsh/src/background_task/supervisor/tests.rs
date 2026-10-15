use super::*;
fn handle(root: &Path) -> TaskHandle {
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let scope = TaskScope {
        profile: Profile::WorkspaceEdit,
        cwd: root.into(),
        read_roots: vec![root.into()],
        write_roots: vec![root.into()],
        environment_names: vec![],
        network_hosts: vec![],
        sandbox_verified: false,
        budget: dsh_types::background_task::BudgetLimits {
            time_ms: 10_000,
            tool_calls: 100,
            tokens: 1000,
            output_bytes: 2_000_000,
        },
    };
    TaskHandle {
        shared: Shared::new(
            TaskState::new("test-task".into(), scope).unwrap(),
            socket,
            None,
            vec!["credential-value".into()],
        ),
        thread: None,
    }
}
fn admit(handle: &TaskHandle, tool: &str, path: &Path, preimage: Option<&str>) -> Operation {
    let operation = Operation {
        tool: tool.into(),
        args: json!({"path":path}),
        canonical_targets: vec![path.into()],
        preimage_digest: preimage.map(String::from),
        content_digest: Some("new".into()),
    };
    admit_operation(handle, operation.clone());
    operation
}
fn admit_operation(handle: &TaskHandle, operation: Operation) {
    let mut inner = handle.shared.inner.lock();
    inner.admitted = Some(Admitted {
        name: operation.tool.clone(),
        arguments: operation.args.to_string(),
        operation: operation.clone(),
        epoch: inner.task.generation(),
        approved: false,
    });
}

#[test]
fn native_admission_matches_arguments_targets_and_current_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let path = root.join("file");
    let operation = admit(&handle, "read_file", &path, None);
    let context = LocalContext {
        shared: handle.shared.clone(),
    };
    assert!(context.permits_tool(
        "read_file",
        &serde_json::to_string_pretty(&operation.args).unwrap()
    ));
    assert!(!context.permits_tool("execute", &operation.args.to_string()));
    assert!(!context.permits_tool("read_file", &json!({"path":root.join("other")}).to_string()));
    assert!(context.read_authorized(&path));
    assert!(!context.write_authorized(&path));
    handle.restrict_read_only().unwrap();
    assert!(!context.permits_tool("read_file", &operation.args.to_string()));
    assert!(!context.read_authorized(&path));
}

#[test]
fn write_admission_rechecks_preimage_and_continued_absence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let path = root.join("file");
    std::fs::write(&path, "before").unwrap();
    // Exercise real normalization as part of the admission boundary instead
    // of maintaining a second preimage-only helper test.
    let scope = handle.shared.inner.lock().task.scope().clone();
    let (operation, _) = super::super::normalize_operation(
        "edit",
        &json!({"path":path,"contents":"new"}).to_string(),
        &scope,
    )
    .unwrap();
    admit_operation(&handle, operation.clone());
    let context = LocalContext {
        shared: handle.shared.clone(),
    };
    assert!(context.write_authorized(&path));
    std::fs::write(&path, "external edit").unwrap();
    assert_ne!(
        super::super::revalidate_operation(&operation, &scope).unwrap(),
        operation
    );
    assert!(!context.write_authorized(&path));
    std::fs::remove_file(&path).unwrap();
    admit(&handle, "edit", &path, None);
    assert!(context.write_authorized(&path));
    std::fs::write(&path, "new external file").unwrap();
    assert!(!context.write_authorized(&path));
}

#[test]
fn native_io_rejects_aba_bytes_despite_restored_preimage_and_valid_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let path = root.join("file");
    std::fs::write(&path, "approved A").unwrap();
    let context = LocalContext {
        shared: handle.shared.clone(),
    };

    admit(&handle, "read_file", &path, Some("approved A"));
    std::fs::write(&path, "unapproved B").unwrap();
    let actual_read = dsh_builtin::agent::files::read(&path).unwrap();
    std::fs::write(&path, "approved A").unwrap();
    assert!(context.read_authorized(&path));
    assert!(!context.read_result_authorized(&path, &actual_read));
    assert!(context.read_result_authorized(&path, "approved A"));
    assert!(!context.read_result_authorized(&root.join("other"), "approved A"));

    admit(&handle, "str_replace", &path, Some("approved A"));
    // A fresh preimage check succeeds after A->B->A, but cannot authorize
    // an update derived from bytes B that the tool read in between.
    assert!(context.write_authorized(&path));
    assert!(!context.read_result_authorized(&path, &actual_read));
    assert!(!context.write_contents_authorized(&path, "update derived from B"));
    assert!(context.write_contents_authorized(&path, "new"));
    handle.restrict_read_only().unwrap();
    assert!(!context.read_result_authorized(&path, "approved A"));
    assert!(!context.write_contents_authorized(&path, "new"));
}

#[test]
fn guidance_approval_is_consumed_once_and_output_is_bounded_and_masked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let operation = Operation {
        tool: "read_file".into(),
        args: json!({"guidance_trust":"Read these project instructions?"}),
        canonical_targets: vec![root],
        preimage_digest: None,
        content_digest: None,
    };
    for (task_id, succeeds) in [("test-task", true)] {
        let shared = handle.shared.clone();
        let operation = operation.clone();
        let waiter = std::thread::spawn(move || await_approval(&shared, operation, true));
        let id = {
            let mut inner = handle.shared.inner.lock();
            while inner.pending.is_none() {
                assert!(
                    !handle
                        .shared
                        .changed
                        .wait_for(&mut inner, Duration::from_secs(2))
                        .timed_out(),
                    "approval was not published"
                );
            }
            inner.pending.as_ref().unwrap().request.request_id
        };
        assert!(!handle.status().to_string().contains("credential-value"));
        assert!(handle.approve("another-task", id, true).is_err());
        assert!(handle.shared.inner.lock().pending.is_some());
        assert_eq!(handle.approve(task_id, id, true).is_ok(), succeeds);
        assert_eq!(waiter.join().unwrap().is_ok(), succeeds);
        assert!(handle.approve("test-task", id, true).is_err());
    }
    handle.shared.append_output(&vec![b'x'; OUTPUT_LIMIT + 10]);
    handle
        .shared
        .append_output(b"\nTOKEN=credential-value\x1b[0m\n");
    let output = handle.output();
    assert!(output.len() <= OUTPUT_LIMIT);
    assert!(!output.contains("credential-value"));
    assert!(!output.contains('\x1b'));
}

#[test]
fn disconnected_approval_revokes_authority_and_stops_only_owned_children() {
    use std::process::Command;
    for guidance in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut handle = handle(&root);
        let (socket, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        // The task owns this PID's wait; the test never calls Child::wait on it.
        #[expect(
            clippy::zombie_processes,
            reason = "Shared::reap owns the sole wait for this worker PID"
        )]
        let worker = Command::new("sleep").arg("60").spawn().unwrap();
        let shared = Arc::get_mut(&mut handle.shared).unwrap();
        shared.shutdown = socket;
        *shared.process.lock() = Some(nix::unistd::Pid::from_raw(worker.id() as i32));
        let mut unrelated = Command::new("sleep").arg("60").spawn().unwrap();
        let mut command = Command::new("sleep");
        command.arg("60");
        handle
            .shared
            .jobs
            .lock()
            .start(command, Duration::from_secs(60), None)
            .unwrap();
        let operation = admit(&handle, "edit", &root.join("file"), None);
        let shared = handle.shared.clone();
        let (finished, receive) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            finished
                .send(await_approval(&shared, operation, guidance))
                .unwrap();
        });
        let id = {
            let mut inner = handle.shared.inner.lock();
            while inner.pending.is_none() {
                assert!(
                    !handle
                        .shared
                        .changed
                        .wait_for(&mut inner, Duration::from_secs(2))
                        .timed_out()
                );
            }
            inner.pending.as_ref().unwrap().request.request_id
        };
        let mut peer = Some(peer);
        if guidance {
            peer.as_ref().unwrap().shutdown(Shutdown::Write).unwrap();
        } else {
            drop(peer.take());
        }
        assert!(
            receive
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        waiter.join().unwrap();
        assert!(handle.approve("test-task", id, true).is_err());
        let inner = handle.shared.inner.lock();
        assert_eq!(inner.task.status(), TaskStatus::Cancelled);
        assert!(inner.pending.is_none() && inner.admitted.is_none());
        drop(inner);
        assert!(!handle.shared.jobs.lock().has_running());
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        handle.shared.reap();
    }
}

#[test]
fn owned_job_wait_honors_exit_and_does_not_hold_the_jobs_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("while [ ! -e release ]; do sleep 0.01; done")
        .current_dir(&root)
        .env("PATH", "/usr/bin:/bin");
    let id = handle
        .shared
        .jobs
        .lock()
        .start(command, Duration::from_secs(5), None)
        .unwrap();
    let shared = handle.shared.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send(owned_job(
            "job_status",
            &json!({"job_id":id,"wait_ms":60000}).to_string(),
            &shared,
        ))
        .unwrap()
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(80)).is_err(),
        "wait must not immediately return running"
    );
    assert!(handle.shared.jobs.lock().has_running());
    std::fs::write(root.join("release"), "").unwrap();
    let result: Value =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap()).unwrap();
    assert_eq!(result["status"], "exited");
    waiter.join().unwrap();
}

#[test]
fn owned_job_wait_is_interrupted_by_cancellation_and_task_deadline() {
    for timeout in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut handle = handle(&root);
        if timeout {
            let mut scope = handle.shared.inner.lock().task.scope().clone();
            scope.budget.time_ms = 30;
            handle
                .shared
                .inner
                .lock()
                .task
                .restrict_scope(scope, 0)
                .unwrap();
        }
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("exec sleep 30")
            .env("PATH", "/usr/bin:/bin");
        let id = handle
            .shared
            .jobs
            .lock()
            .start(command, Duration::from_secs(30), None)
            .unwrap();
        let shared = handle.shared.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            tx.send(owned_job(
                "job_status",
                &json!({"job_id":id,"wait_ms":60000}).to_string(),
                &shared,
            ))
            .unwrap()
        });
        if !timeout {
            handle.cancel();
        }
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
        waiter.join().unwrap();
        if timeout {
            assert_eq!(
                handle.shared.inner.lock().task.status(),
                TaskStatus::TimedOut
            );
        }
    }
}

// Publish real pending state, without mocking the task permission engine.
fn pending_preview(handle: &TaskHandle, operation: Operation, guidance: bool) {
    let mut inner = handle.shared.inner.lock();
    inner.task.deny_approval();
    inner.pending = None;
    let request = inner.task.request_manual_approval(operation, 0).unwrap();
    inner.pending = Some(Pending {
        request,
        guidance,
        decision: None,
    });
}

#[test]
fn preview_response_is_one_shot_and_stale_ui_does_not_change_new_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let scope = handle.shared.inner.lock().task.scope().clone();
    let (op, _) = super::super::normalize_operation(
        "edit",
        &json!({"path":"file", "contents":"new"}).to_string(),
        &scope,
    )
    .unwrap();
    pending_preview(&handle, op.clone(), false);
    let old = handle.approval_preview().unwrap();
    pending_preview(&handle, op.clone(), false);
    assert!(handle.respond_preview(&old, true).is_err());
    assert!(
        handle
            .shared
            .inner
            .lock()
            .pending
            .as_ref()
            .unwrap()
            .decision
            .is_none()
    );
    let current = handle.approval_preview().unwrap();
    handle.respond_preview(&current, true).unwrap();
    assert_eq!(
        handle
            .shared
            .inner
            .lock()
            .pending
            .as_ref()
            .unwrap()
            .decision,
        Some(true)
    );
    assert!(handle.respond_preview(&current, true).is_err());
    pending_preview(&handle, op, false);
    let changed = handle.approval_preview().unwrap();
    std::fs::write(root.join("file"), "created externally").unwrap();
    assert!(handle.respond_preview(&changed, true).is_err());
    assert!(handle.shared.inner.lock().pending.is_none());
    assert_eq!(
        std::fs::read_to_string(root.join("file")).unwrap(),
        "created externally"
    );
}

#[test]
fn incomplete_preview_cannot_approve_but_can_deny() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut handle = handle(&root);
    Arc::get_mut(&mut handle.shared)
        .unwrap()
        .secrets
        .push("xy".into());
    for (guidance, path, content) in [
        (true, root.clone(), "g".repeat(2049)),
        (true, root.join("p".repeat(513)), "short".into()),
        (false, root.join("file"), "xy".repeat(300)),
    ] {
        let operation = Operation {
            tool: if guidance { "read_file" } else { "edit" }.into(),
            args: if guidance {
                json!({"guidance_trust":content})
            } else {
                json!({"path":path,"contents":content})
            },
            canonical_targets: vec![path],
            preimage_digest: None,
            content_digest: if guidance { None } else { Some(content) },
        };
        pending_preview(&handle, operation, guidance);
        let preview = handle.approval_preview().unwrap();
        assert!(!preview.complete);
        assert!(!preview.display.to_string().contains("credential-value"));
        assert!(handle.respond_preview(&preview, true).is_err());
        handle.respond_preview(&preview, false).unwrap();
        assert_eq!(
            handle
                .shared
                .inner
                .lock()
                .pending
                .as_ref()
                .unwrap()
                .decision,
            Some(false)
        );
    }
}

#[test]
fn preview_expires_when_scope_changes_without_recording_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let scope = handle.shared.inner.lock().task.scope().clone();
    let (op, _) = super::super::normalize_operation(
        "edit",
        &json!({"path":"file", "contents":"new"}).to_string(),
        &scope,
    )
    .unwrap();
    pending_preview(&handle, op, false);
    let preview = handle.approval_preview().unwrap();
    handle.restrict_read_only().unwrap();
    assert!(handle.respond_preview(&preview, true).is_err());
    assert!(handle.shared.inner.lock().pending.is_none());
}

#[test]
fn hidden_preview_cannot_be_approved_through_cli_or_menu() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut handle = handle(&root);
    Arc::get_mut(&mut handle.shared).unwrap().secrets = vec!["TOKEN".into()];
    let scope = handle.shared.inner.lock().task.scope().clone();
    let (op, _) = super::super::normalize_operation(
        "edit",
        &json!({"path":"file", "contents":"TOKEN=private-dummy-value"}).to_string(),
        &scope,
    )
    .unwrap();
    pending_preview(&handle, op, false);
    let preview = handle.approval_preview().unwrap();
    assert!(!preview.display.to_string().contains("private-dummy-value"));
    assert!(!preview.complete);
    let id = preview.display["request_id"].as_u64().unwrap();
    assert!(handle.respond_preview(&preview, true).is_err());
    assert!(handle.approve("test-task", id, true).is_err());
    assert!(
        handle
            .shared
            .inner
            .lock()
            .pending
            .as_ref()
            .unwrap()
            .decision
            .is_none()
    );
    handle.approve("test-task", id, false).unwrap();
}

#[test]
fn escaped_target_remains_distinct_and_bound_to_the_real_operation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let scope = handle.shared.inner.lock().task.scope().clone();
    for name in [
        "safe\u{8}.txt",
        "safe\u{202e}.txt",
        "safe\\u{8}.txt",
        "safe\n.txt",
        "safe\u{85}.txt",
    ] {
        let (op, _) = super::super::normalize_operation(
            "edit",
            &json!({"path":name, "contents":"new"}).to_string(),
            &scope,
        )
        .unwrap();
        pending_preview(&handle, op, false);
        let preview = handle.approval_preview().unwrap();
        let shown = preview.display["targets"][0].as_str().unwrap();
        assert!(!shown.chars().any(|ch| ch.is_control() || ch == '\u{202e}'));
        assert!(shown.contains('\\'), "{shown}");
        assert!(preview.complete, "lossless escaping permits review");
        handle.respond_preview(&preview, true).unwrap();
        assert_eq!(
            handle
                .shared
                .inner
                .lock()
                .pending
                .as_ref()
                .unwrap()
                .decision,
            Some(true)
        );
    }
}

#[test]
fn wrong_cli_answer_never_changes_current_request_or_recorded_decision() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let handle = handle(&root);
    let scope = handle.shared.inner.lock().task.scope().clone();
    let (op, _) = super::super::normalize_operation(
        "edit",
        &json!({"path":"file", "contents":"new"}).to_string(),
        &scope,
    )
    .unwrap();
    pending_preview(&handle, op, false);
    let original = handle
        .shared
        .inner
        .lock()
        .pending
        .as_ref()
        .unwrap()
        .request
        .clone();
    for (task, id) in [
        ("wrong-task", original.request_id),
        ("test-task", original.request_id + 1),
    ] {
        assert!(handle.approve(task, id, true).is_err());
        let inner = handle.shared.inner.lock();
        assert_eq!(inner.task.pending(), Some(&original));
        assert!(inner.pending.as_ref().unwrap().decision.is_none());
    }
    handle
        .approve("test-task", original.request_id, true)
        .unwrap();
    assert!(
        handle
            .approve("test-task", original.request_id, false)
            .is_err()
    );
    assert_eq!(
        handle
            .shared
            .inner
            .lock()
            .pending
            .as_ref()
            .unwrap()
            .decision,
        Some(true)
    );
}

#[test]
fn approval_limits_apply_after_lossless_escape_and_identical_masks_still_block() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut handle = handle(&root);
    Arc::get_mut(&mut handle.shared).unwrap().secrets = vec!["***".into()];
    for (text, complete) in [
        ("x".repeat(2047), true),
        ("x".repeat(2048), true),
        ("x".repeat(2049), false),
        (format!("{}\u{202e}", "x".repeat(2040)), true),
        (format!("{}\u{202e}", "x".repeat(2041)), false),
        ("***".into(), false),
    ] {
        let op = Operation {
            tool: "execute".into(),
            args: json!({"command":text}),
            canonical_targets: vec![root.clone()],
            preimage_digest: None,
            content_digest: None,
        };
        pending_preview(&handle, op, false);
        let preview = handle.approval_preview().unwrap();
        assert_eq!(preview.complete, complete);
        if !complete {
            let id = preview.display["request_id"].as_u64().unwrap();
            assert!(handle.approve("test-task", id, true).is_err());
            assert!(
                handle
                    .shared
                    .inner
                    .lock()
                    .pending
                    .as_ref()
                    .unwrap()
                    .decision
                    .is_none()
            );
        }
    }
}
