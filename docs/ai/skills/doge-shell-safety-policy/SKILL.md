---
name: doge-shell-safety-policy
description: Use for doge-shell safety, guard, safe_run, command policy, approval, 安全確認, ガード, コマンドポリシー, or 実行制御 work.
---

# Doge Shell Safety Policy

- Start with `rg -n "safe|safety|guard|policy|approval|confirm|danger|execute" dsh/src/safety dsh-builtin/src/safe_run.rs dsh-builtin/src/chatgpt/tool/execute.rs`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for safety / guard / command policy entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Read [../doge-shell-repo/references/ai-architecture.md](../doge-shell-repo/references/ai-architecture.md) for where the safety gate applies to the AI features and how the three allowlists differ.
- Default read targets are `dsh/src/safety/`, `dsh-types/src/safety_policy.rs`, `dsh-builtin/src/safe_run.rs`, `dsh-builtin/src/chatgpt/tool/execute.rs`, and `AgentCommandPolicy` in `dsh/src/proxy/mod.rs`.
- `SafetyLevel` and the shared command classifiers live in `dsh-types/src/safety_policy.rs`; the single source for the current level is `policy_state.safety_level`, not the `SAFETY_LEVEL` variable.
- Every agent confirmation goes through `confirm_agent_action` in `dsh-builtin/src/chatgpt/tool/mod.rs`, which offers Allow / AllowAlways / Deny. Do not add a `ShellProxy::confirm_action` call to a chat tool - the bool cannot say "always".
- An "always" answer is stored in `policy_state.agent_session_allowlist` and matched exactly, keyed by prefix: a bare command line, `mcp:<function_name>:<args>`, `write:<path>`, `sensitive:<action>:<path>`. Never write one into `execute_allowlist`, which is the operator's configuration.
- Do not put "Proceed?" in a confirmation message; `dsh/src/repl/confirmation.rs` appends the question.
- `loose` passes commands, MCP calls and sensitive reads. It does **not** skip file writes or skill scripts - those confirm at every level.
- Keep policy and execution changes separated unless the task requires both.
- Validate touched packages: `cargo test -p doge-shell` for shell policy changes; `cargo test -p dsh-builtin` for builtin or tool execution changes; add `cargo test -p dsh-types` when the shared classifiers change.
