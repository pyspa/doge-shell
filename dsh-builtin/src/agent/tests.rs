    use super::*;
    use crate::shell_capabilities::{AgentTaskSave, AgentTaskStore};

    fn running_task() -> AgentTask {
        AgentTask {
            id: "task-1".into(),
            goal: "goal".into(),
            root: std::path::PathBuf::from("/tmp"),
            status: TaskStatus::Running,
            grant: Default::default(),
            criteria: vec![],
            plan: vec![],
            progress: String::new(),
            tokens_used: 0,
            time_budget_ms: 60_000,
            elapsed_ms: 0,
            stop_reason: None,
            checkpoint: None,
            pending_operation: None,
            created_at: 0,
        }
    }

    /// The same command failing the same way twice must match itself even
    /// though every `execute` result carries a fresh `job_id`/`pid`: the old
    /// verbatim-result signature never advanced `repeats` past 1.
    #[test]
    fn failure_signature_ignores_volatile_job_metadata() {
        let call = json!({"id":"call-1","function":{"name":"execute","arguments":"{\"command\":\"cargo test\"}"}});
        let first = failure_signature(
            &call,
            &json!({"status":"exited","exit_code":1,"job_id":"aaa","pid":111,"stdout":"boom","stderr":"","stdout_bytes":4,"stdout_next_offset":4}).to_string(),
        );
        let second = failure_signature(
            &call,
            &json!({"status":"exited","exit_code":1,"job_id":"bbb","pid":222,"stdout":"boom","stderr":"","stdout_bytes":4,"stdout_next_offset":4}).to_string(),
        );
        assert_eq!(first, second);
    }

    #[test]
    fn failure_signature_distinguishes_commands_and_outcomes() {
        let call = json!({"id":"call-1","function":{"name":"execute","arguments":"{\"command\":\"cargo test\"}"}});
        let other_command = json!({"id":"call-2","function":{"name":"execute","arguments":"{\"command\":\"cargo build\"}"}});
        let failed = failure_signature(
            &call,
            &json!({"status":"exited","exit_code":1,"job_id":"aaa","stdout":"boom"}).to_string(),
        );
        assert_ne!(
            failed,
            failure_signature(
                &other_command,
                &json!({"status":"exited","exit_code":1,"job_id":"aaa","stdout":"boom"})
                    .to_string(),
            )
        );
        assert_ne!(
            failed,
            failure_signature(
                &call,
                &json!({"status":"exited","exit_code":0,"job_id":"aaa","stdout":"ok"}).to_string(),
            )
        );
    }

    struct MemoryStore {
        task: std::sync::Mutex<AgentTask>,
        fail_load: bool,
    }

    impl MemoryStore {
        fn running() -> Self {
            Self {
                task: std::sync::Mutex::new(running_task()),
                fail_load: false,
            }
        }

        fn load_fails() -> Self {
            Self {
                task: std::sync::Mutex::new(running_task()),
                fail_load: true,
            }
        }
    }

    impl AgentTaskStore for MemoryStore {
        fn save(
            &self,
            task: &AgentTask,
            _event: Option<(&str, &Value)>,
        ) -> anyhow::Result<AgentTaskSave> {
            *self.task.lock().unwrap() = task.clone();
            Ok(AgentTaskSave {
                sequence: 0,
                task: task.clone(),
            })
        }
        fn resume(
            &self,
            task: &AgentTask,
            event: Option<(&str, &Value)>,
        ) -> anyhow::Result<AgentTaskSave> {
            self.save(task, event)
        }
        fn load(&self, _id: &str) -> anyhow::Result<AgentTask> {
            if self.fail_load {
                return Err(anyhow::anyhow!("transient sqlite lock"));
            }
            Ok(self.task.lock().unwrap().clone())
        }
        fn list(&self) -> anyhow::Result<Vec<AgentTask>> {
            Ok(vec![self.task.lock().unwrap().clone()])
        }
        fn events(&self, _id: &str) -> anyhow::Result<Vec<dsh_types::agent::TaskEvent>> {
            Ok(Vec::new())
        }
        fn delete(&self, _id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn save_artifact(&self, _id: &str, _name: &str, _content: &Value) -> anyhow::Result<()> {
            Ok(())
        }
        fn load_artifact(&self, _id: &str, _name: &str) -> anyhow::Result<Value> {
            Ok(Value::Null)
        }
    }

    /// A transient store read failure is not a cancellation: `map_or(true)`
    /// here used to turn momentary SQLite contention into "the task was
    /// cancelled", and the 20-50ms pollers made that contention likely.
    #[test]
    fn stopped_treats_a_store_read_failure_as_not_cancelled() {
        let store = Arc::new(MemoryStore::load_fails());
        let runtime = AgentRuntime::new(running_task(), store);
        assert!(!runtime.stopped());
    }

    #[test]
    fn stopped_still_sees_a_persisted_cancellation() {
        let store = Arc::new(MemoryStore::running());
        store.task.lock().unwrap().status = TaskStatus::Cancelled;
        let runtime = AgentRuntime::new(running_task(), store);
        assert!(runtime.stopped());
    }

    fn verified_at_time_budget() -> (AgentTask, Arc<MemoryStore>) {
        let mut task = running_task();
        task.elapsed_ms = task.time_budget_ms;
        task.criteria = vec![Verification {
            criterion: "done".into(),
            evidence_event: Some(1),
            passed: true,
        }];
        let store = Arc::new(MemoryStore::running());
        *store.task.lock().unwrap() = task.clone();
        (task, store)
    }

    /// A final round landing exactly on its time budget with verified work done
    /// completed the task; resuming would stop again at the loop head unless
    /// the time budget is raised.
    #[test]
    fn finish_completes_verified_work_at_exact_budget() {
        let (task, store) = verified_at_time_budget();
        let mut runtime = AgentRuntime::new(task, store);
        runtime.finish(true, None).unwrap();
        assert_eq!(runtime.task.status, TaskStatus::Completed);
    }

    /// A turn stopped *by* the time budget is resumable, not failed on its merits.
    #[test]
    fn finish_interrupts_a_budget_stopped_failure() {
        let (mut task, store) = verified_at_time_budget();
        task.criteria = vec![];
        let mut runtime = AgentRuntime::new(task, store);
        runtime.finish(false, Some("boom".into())).unwrap();
        assert_eq!(runtime.task.status, TaskStatus::Interrupted);
    }

    /// A turn that met grant refusals and ended unsuccessfully with time
    /// left is stuck, not failed: `Interrupted` with the refusal hint, so a
    /// resume can name the missing grant.
    #[test]
    fn finish_marks_a_denial_stuck_turn_interrupted_with_the_hint() {
        let store = Arc::new(MemoryStore::running());
        let mut runtime = AgentRuntime::new(running_task(), store);
        runtime.note_denial("cargo test: command is not in the task's exact command grants");
        runtime
            .finish(false, Some("agent: cannot complete with unverified criteria".into()))
            .unwrap();
        assert_eq!(runtime.task.status, TaskStatus::Interrupted);
        assert_eq!(
            runtime.task.stop_reason.as_deref(),
            Some("cargo test: command is not in the task's exact command grants")
        );
    }

    /// The `Interrupted` above is only for refusal-stuck turns: a genuine
    /// failure with no refusals behind it stays `Failed`.
    #[test]
    fn finish_keeps_a_failure_without_denials_failed() {
        let store = Arc::new(MemoryStore::running());
        let mut runtime = AgentRuntime::new(running_task(), store);
        runtime
            .finish(false, Some("agent: cannot complete with unverified criteria".into()))
            .unwrap();
        assert_eq!(runtime.task.status, TaskStatus::Failed);
    }

    /// Only first-party denial wordings count: a hook refusal or ordinary
    /// output mentioning permission must not preserve a refusal hint.
    #[test]
    fn denial_text_matches_only_first_party_refusals() {
        assert!(is_denial_text(
            "Error: agent: permission required: AI wants to write `x` [approval_key: write:/tmp/x]\nPlease analyze the error and retry with corrected arguments."
        ));
        assert!(is_denial_text(
            "Error: agent: command permission required: cargo test\nPlease analyze the error and retry with corrected arguments."
        ));
        assert!(is_denial_text(
            "Error: agent: skill script permission required: bash run.sh\nPlease analyze the error and retry with corrected arguments."
        ));
        assert!(is_denial_text("MCP tool execution cancelled by user."));
        assert!(!is_denial_text("Blocked by hook `h`: reason"));
        assert!(!is_denial_text("test failed"));
        assert!(!is_denial_text("finished reading"));
    }

    /// Repeating the same refused operation three times stops on the refusal
    /// hint itself, so the stuck task names its resume command.
    #[test]
    fn three_refused_repeats_stop_on_the_refusal_hint() {
        let store = Arc::new(MemoryStore::running());
        let mut task = running_task();
        task.plan = vec!["step".into()];
        task.criteria = vec![Verification {
            criterion: "done".into(),
            evidence_event: None,
            passed: false,
        }];
        let mut runtime = AgentRuntime::new(task, store);
        let call = json!({"id":"call-1","function":{"name":"execute","arguments":"{\"command\":\"cargo test\"}"}});
        let result = "Error: agent: command permission required: cargo test\nPlease analyze the error and retry with corrected arguments.";
        for _ in 0..3 {
            runtime.before_tool(&call, Value::Null).unwrap();
            // Dispatch records the refusal before `after_tool` sees it,
            // mirroring `execute::authorize`'s agent branch.
            runtime.note_denial("cargo test: command is not in the task's exact command grants");
            runtime.after_tool(&call, result, ToolOutcome::Failure).unwrap();
        }
        assert_eq!(runtime.task.status, TaskStatus::InputRequired);
        assert_eq!(
            runtime.task.stop_reason.as_deref(),
            Some("cargo test: command is not in the task's exact command grants")
        );
    }

    /// A stale hint must not survive past an unrelated result: three genuine
    /// failures after an earlier refusal stop on the stall message, not on
    /// the unrelated grant.
    #[test]
    fn a_stale_hint_does_not_survive_a_genuine_result() {
        let store = Arc::new(MemoryStore::running());
        let mut task = running_task();
        task.plan = vec!["step".into()];
        task.criteria = vec![Verification {
            criterion: "done".into(),
            evidence_event: None,
            passed: false,
        }];
        let mut runtime = AgentRuntime::new(task, store);
        runtime.note_denial("cargo test: command is not in the task's exact command grants");
        let call = json!({"id":"call-1","function":{"name":"execute","arguments":"{\"command\":\"cargo test\"}"}});
        runtime.before_tool(&call, Value::Null).unwrap();
        runtime.after_tool(&call, "finished reading", ToolOutcome::Success).unwrap();
        for _ in 0..3 {
            runtime.before_tool(&call, Value::Null).unwrap();
            runtime.after_tool(&call, "test failed", ToolOutcome::Failure).unwrap();
        }
        assert_eq!(runtime.task.status, TaskStatus::InputRequired);
        assert_eq!(
            runtime.task.stop_reason.as_deref(),
            Some("same operation failed three times; inspect the cause before resuming")
        );
    }

    /// The missing-plan classifier must survive a `.context()` wrapper and
    /// must not match unrelated errors.
    #[test]
    fn missing_plan_classifier_survives_context() {
        use anyhow::Context as _;
        let plain = anyhow::anyhow!(MISSING_PLAN_MESSAGE);
        assert!(is_missing_plan_error(&plain));
        let wrapped: anyhow::Result<()> = Err(anyhow::anyhow!(MISSING_PLAN_MESSAGE));
        let wrapped = wrapped.context("before_tool failed").unwrap_err();
        assert!(is_missing_plan_error(&wrapped));
        let other = anyhow::anyhow!("agent: task stopped or time budget exhausted");
        assert!(!is_missing_plan_error(&other));
    }
