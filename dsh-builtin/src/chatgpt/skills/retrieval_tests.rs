//! Actual model/tool-loop retrieval contract using a scripted, offline client.
use super::*;
use crate::chatgpt::{ChatClient, ChatRequestOptions, chat_with_tools};
use crate::test_support::TestShellProxy;
use serde_json::{Value, json};

#[test]
fn model_discovers_then_reads_only_the_needed_skill_and_resource() {
    let _state_guard = crate::chatgpt::session::tests::isolated_state_home();
    struct Client {
        step: Mutex<usize>,
        instruction: PathBuf,
        seen: Mutex<Vec<String>>,
    }
    impl ChatClient for Client {
        fn send_chat_request(
            &self,
            messages: &[Value],
            _options: &ChatRequestOptions,
        ) -> anyhow::Result<Value> {
            let mut step = self.step.lock().unwrap();
            let request = serde_json::to_string(messages).unwrap();
            self.seen.lock().unwrap().push(request.clone());
            let call = match *step {
                0 => {
                    assert!(request.contains("Use when diagnosing fixture problems"));
                    assert!(!request.contains("BODY_SENTINEL"));
                    assert!(!request.contains("UNUSED_BODY_SENTINEL"));
                    Some(json!({"path":self.instruction}))
                }
                1 => {
                    assert!(request.contains("BODY_SENTINEL"));
                    assert!(!request.contains("RESOURCE_SENTINEL"));
                    Some(json!({"path":"references/detail.md","skill_path":self.instruction}))
                }
                2 => {
                    assert!(request.contains("RESOURCE_SENTINEL"));
                    assert!(!request.contains("UNUSED_BODY_SENTINEL"));
                    None
                }
                _ => panic!("unexpected model request"),
            };
            *step += 1;
            Ok(match call {
                Some(arguments) => {
                    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[{"id":format!("call{}", *step),"type":"function","function":{"name":"read_file","arguments":arguments.to_string()}}]}}]})
                }
                None => {
                    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]})
                }
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    let skill = dir.path().join(".dogesh/skills/fixture");
    std::fs::create_dir_all(skill.join("references")).unwrap();
    let instruction = skill.join("SKILL.md");
    std::fs::write(&instruction, "---\nname: fixture\ndescription: Use when diagnosing fixture problems\n---\nBODY_SENTINEL: read references/detail.md only if needed.\n").unwrap();
    std::fs::write(skill.join("references/detail.md"), "RESOURCE_SENTINEL").unwrap();
    let unused = dir.path().join(".dogesh/skills/unused");
    std::fs::create_dir(&unused).unwrap();
    std::fs::write(
        unused.join("SKILL.md"),
        "---\nname: unused\ndescription: unrelated procedure\n---\nUNUSED_BODY_SENTINEL\n",
    )
    .unwrap();
    let client = Client {
        step: Mutex::new(0),
        instruction,
        seen: Mutex::new(Vec::new()),
    };
    let mut proxy = TestShellProxy {
        current_dir: dir.path().to_path_buf(),
        confirm_result: true,
        ..Default::default()
    };
    proxy
        .vars
        .insert("AI_CHAT_SESSION_TTL_SECS".into(), "0".into());
    proxy.vars.insert("AI_CHAT_HOOKS".into(), "off".into());
    let mcp = proxy.mcp_manager.clone();
    assert_eq!(
        chat_with_tools(
            &client,
            "diagnose fixture",
            None,
            None,
            None,
            None,
            &mcp,
            None,
            &mut proxy
        )
        .unwrap(),
        "done"
    );
    assert_eq!(*client.step.lock().unwrap(), 3);
    assert!(
        proxy.dispatch_attempts.is_empty(),
        "skill text must not auto-run commands"
    );
}
