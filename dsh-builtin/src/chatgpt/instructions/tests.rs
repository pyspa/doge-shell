use super::*;
use crate::test_support::TestShellProxy;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    dir
}
fn proxy(dir: &Path) -> TestShellProxy {
    TestShellProxy {
        current_dir: dir.to_path_buf(),
        confirm_result: true,
        ..Default::default()
    }
}
fn call(path: &Path) -> Value {
    json!({"function":{"name":"edit","arguments":json!({"path":path,"contents":"new"}).to_string()}})
}

#[test]
fn parent_before_child_siblings_excluded_and_worktree_boundary() {
    let outer = fixture();
    fs::write(outer.path().join("AGENTS.md"), "outer").unwrap();
    let repo = outer.path().join("worktree");
    let child = repo.join("0/deep");
    fs::create_dir_all(&child).unwrap();
    fs::write(repo.join(".git"), "gitdir: /unused").unwrap();
    fs::write(repo.join("AGENTS.md"), "root").unwrap();
    fs::write(repo.join("0/AGENTS.md"), "child").unwrap();
    fs::create_dir(repo.join("other")).unwrap();
    fs::write(repo.join("other/AGENTS.md"), "sibling").unwrap();
    let (docs, errors) = discover(&child);
    assert!(errors.is_empty());
    assert_eq!(
        docs.iter().map(|d| d.body.as_str()).collect::<Vec<_>>(),
        ["root", "child"]
    );
}

#[test]
fn no_git_uses_existing_workspace_boundary_not_home() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    fs::write(dir.path().join("AGENTS.md"), "workspace").unwrap();
    fs::create_dir(dir.path().join("0")).unwrap();
    assert_eq!(discover(&dir.path().join("0")).0[0].body, "workspace");
}

#[test]
fn oversized_invalid_utf8_and_symlinks_are_diagnosed() {
    let dir = fixture();
    let path = dir.path().join("AGENTS.md");
    fs::write(&path, "a".repeat(FILE_BYTES + 1)).unwrap();
    assert!(discover(dir.path()).0.is_empty());
    assert!(discover(dir.path()).1[0].contains("limit"));
    fs::write(&path, [255]).unwrap();
    assert!(discover(dir.path()).1[0].contains("UTF-8"));
    fs::remove_file(&path).unwrap();
    fs::write(dir.path().join("real.md"), "safe").unwrap();
    std::os::unix::fs::symlink("real.md", &path).unwrap();
    assert!(discover(dir.path()).0.is_empty());
    assert!(discover(dir.path()).1[0].contains("symlink"));
}

#[test]
fn combined_scope_budget_is_bounded_without_partial_instructions() {
    let dir = fixture();
    let mut scope = dir.path().to_path_buf();
    for _ in 0..6 {
        fs::write(scope.join("AGENTS.md"), "a".repeat(FILE_BYTES)).unwrap();
        scope = scope.join("sub");
        fs::create_dir(&scope).unwrap();
    }
    let (docs, errors) = discover(&scope);
    assert_eq!(docs.len(), 4);
    assert_eq!(errors.len(), 2);
}

#[test]
fn new_target_defers_whole_batch_and_reload_does_not_grant_execution() {
    let dir = fixture();
    fs::create_dir(dir.path().join("0")).unwrap();
    fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();
    fs::write(dir.path().join("0/AGENTS.md"), "child rules").unwrap();
    let mut proxy = proxy(dir.path());
    let mut context = InstructionContext::default();
    let initial = context.refresh(&mut proxy);
    assert!(initial.contains("root rules"));
    assert!(!initial.contains("child rules"));
    let operation = call(&dir.path().join("0/file.rs"));
    assert!(context.preflight(&operation, &mut proxy).is_some());
    assert!(context.preflight(&operation, &mut proxy).is_some());
    assert!(!dir.path().join("0/file.rs").exists());
    let next = context.refresh(&mut proxy);
    assert!(next.contains("child rules"));
    assert!(next.find("root rules").unwrap() < next.find("child rules").unwrap());
    assert!(context.preflight(&operation, &mut proxy).is_none());
    assert!(
        proxy
            .agent_session_allowlist
            .iter()
            .all(|key| key.starts_with("instructions:"))
    );
    fs::write(dir.path().join("0/AGENTS.md"), "changed rules").unwrap();
    assert!(context.preflight(&operation, &mut proxy).is_some());
    assert!(context.refresh(&mut proxy).contains("changed rules"));
    assert!(context.preflight(&operation, &mut proxy).is_none());
}

#[test]
fn denied_guidance_body_is_not_sent_and_not_reasked_each_iteration() {
    let dir = fixture();
    fs::write(dir.path().join("AGENTS.md"), "DO NOT SEND").unwrap();
    let mut proxy = proxy(dir.path());
    proxy.confirm_result = false;
    let mut context = InstructionContext::default();
    let rendered = context.refresh(&mut proxy);
    assert!(!rendered.contains("DO NOT SEND"));
    assert!(rendered.contains("not trusted"));
    let count = proxy.confirm_calls;
    context.refresh(&mut proxy);
    assert_eq!(proxy.confirm_calls, count);
    assert!(proxy.agent_session_allowlist.is_empty());
}

#[test]
fn actual_chat_request_gets_scoped_text_before_edit_and_retains_confirmation() {
    let _state_guard = crate::chatgpt::session::tests::isolated_state_home();
    use std::sync::Mutex;
    struct Client {
        messages: Mutex<Vec<Vec<Value>>>,
        target: PathBuf,
    }
    impl ChatClient for Client {
        fn send_chat_request(
            &self,
            messages: &[Value],
            _options: &ChatRequestOptions,
        ) -> anyhow::Result<Value> {
            let mut seen = self.messages.lock().unwrap();
            seen.push(messages.to_vec());
            if seen.len() < 3 {
                assert!(
                    !self.target.exists(),
                    "edit executed before guidance was shown"
                );
                Ok(
                    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":format!("call{}",seen.len()),"type":"function","function":{"name":"edit","arguments":json!({"path":self.target,"contents":"done"}).to_string()}}]}}]}),
                )
            } else {
                Ok(
                    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"complete"}}]}),
                )
            }
        }
    }
    let dir = fixture();
    fs::create_dir(dir.path().join("0")).unwrap();
    fs::write(dir.path().join("AGENTS.md"), "root task instructions").unwrap();
    fs::write(dir.path().join("0/AGENTS.md"), "child task instructions").unwrap();
    let target = dir.path().join("0/answer.txt");
    let client = Client {
        messages: Mutex::new(Vec::new()),
        target: target.clone(),
    };
    let mut proxy = proxy(dir.path());
    proxy
        .vars
        .insert("AI_CHAT_SESSION_TTL_SECS".into(), "0".into());
    proxy.vars.insert("AI_CHAT_HOOKS".into(), "off".into());
    let mcp = proxy.mcp_manager.clone();
    let answer = chat_with_tools(
        &client,
        "write answer.txt",
        None,
        None,
        None,
        None,
        &mcp,
        None,
        &mut proxy,
    )
    .unwrap();
    assert_eq!(answer, "complete");
    let messages = client.messages.lock().unwrap();
    let first = serde_json::to_string(&messages[0]).unwrap();
    let second = serde_json::to_string(&messages[1]).unwrap();
    assert!(first.contains("root task instructions"));
    assert!(!first.contains("child task instructions"));
    assert!(second.contains("child task instructions"));
    assert!(second.contains("Operation was NOT executed"));
    assert_eq!(fs::read_to_string(target).unwrap(), "done");
    assert!(
        proxy.confirm_calls >= 3,
        "guidance trust must not authorize edit"
    );
}

#[test]
fn opt_out_never_loads_or_prompts() {
    let dir = fixture();
    fs::write(dir.path().join("AGENTS.md"), "secret guidance").unwrap();
    let mut proxy = proxy(dir.path());
    proxy
        .vars
        .insert(PROJECT_INSTRUCTIONS_KEY.into(), "off".into());
    let mut context = InstructionContext::default();
    assert!(context.refresh(&mut proxy).is_empty());
    assert!(
        context
            .preflight(&call(&dir.path().join("file")), &mut proxy)
            .is_none()
    );
    assert_eq!(proxy.confirm_calls, 0);
}

#[test]
fn deletion_is_reflected_and_nested_repository_scope_is_explicit() {
    let dir = fixture();
    fs::write(dir.path().join("AGENTS.md"), "outer rules").unwrap();
    let nested = dir.path().join("nested");
    fs::create_dir_all(nested.join(".git")).unwrap();
    fs::write(nested.join("AGENTS.md"), "inner rules").unwrap();
    let mut proxy = proxy(dir.path());
    let mut context = InstructionContext::default();
    context.refresh(&mut proxy);
    context.preflight(&call(&nested.join("file")), &mut proxy);
    let shown = context.refresh(&mut proxy);
    assert!(shown.contains("excluding nested Git checkouts"));
    assert!(shown.contains("outer rules"));
    assert!(shown.contains("inner rules"));
    fs::remove_file(dir.path().join("AGENTS.md")).unwrap();
    fs::remove_file(nested.join("AGENTS.md")).unwrap();
    assert!(
        context
            .preflight(&call(&nested.join("file")), &mut proxy)
            .is_some()
    );
    let shown = context.refresh(&mut proxy);
    assert!(shown.contains("No active project guidance"));
    assert!(!shown.contains("outer rules"));
    assert!(
        context
            .preflight(&call(&nested.join("file")), &mut proxy)
            .is_none()
    );
}

#[test]
fn execute_cwd_uses_the_same_expansion_as_file_tools() {
    let _env_guard = crate::chatgpt::tool::execute::tests::env_lock();
    let dir = fixture();
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join("sub/AGENTS.md"), "expanded cwd rules").unwrap();
    let mut proxy = proxy(dir.path());
    let mut context = InstructionContext::default();
    context.refresh(&mut proxy);
    let home = dirs::home_dir().unwrap();
    let to_root = "../".repeat(home.components().count() - 1);
    let expanded = format!(
        "~/{to_root}{}",
        dir.path().join("sub").strip_prefix("/").unwrap().display()
    );
    let operation = json!({"function":{"name":"execute","arguments":json!({"command":"true","cwd":expanded}).to_string()}});
    assert!(context.preflight(&operation, &mut proxy).is_some());
    assert!(context.refresh(&mut proxy).contains("expanded cwd rules"));
    assert!(context.preflight(&operation, &mut proxy).is_none());
}

#[test]
fn skill_relative_reads_inspect_the_resource_scope() {
    let dir = fixture();
    let skill = dir.path().join(".dogesh/skills/demo");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::write(
        skill.join("references/AGENTS.md"),
        "reference scope guidance",
    )
    .unwrap();
    let mut proxy = proxy(dir.path());
    let mut context = InstructionContext::default();
    context.refresh(&mut proxy);
    let read = json!({"function":{"name":"read_file","arguments":json!({"path":"references/detail.md","skill_path":skill.join("SKILL.md")}).to_string()}});
    assert!(context.preflight(&read, &mut proxy).is_some());
    assert!(
        context
            .refresh(&mut proxy)
            .contains("reference scope guidance")
    );
    assert!(context.preflight(&read, &mut proxy).is_none());
}
