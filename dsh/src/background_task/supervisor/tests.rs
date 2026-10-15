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
    let mut inner = handle.shared.inner.lock();
    inner.admitted = Some(Admitted {
        name: tool.into(),
        arguments: operation.args.to_string(),
        operation: operation.clone(),
        epoch: inner.task.generation(),
        approved: false,
    });
    operation
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
    admit(&handle, "edit", &path, Some("before"));
    let context = LocalContext {
        shared: handle.shared.clone(),
    };
    assert!(context.write_authorized(&path));
    std::fs::write(&path, "external edit").unwrap();
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
        args: json!({"guidance_trust":"TOKEN=credential-value"}),
        canonical_targets: vec![root],
        preimage_digest: None,
        content_digest: None,
    };
    for (task_id, succeeds) in [("another-task", false), ("test-task", true)] {
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
