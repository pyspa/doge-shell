---
name: doge-shell-safety-policy
description: Use for doge-shell safety, guard, safe_run, command policy, approval, 安全確認, ガード, コマンドポリシー, or 実行制御 work.
---

# Doge Shell Safety Policy

- Start with `rg -n "safe|safety|guard|policy|approval|confirm|danger|execute" dsh/src/safety dsh-builtin/src/safe_run.rs dsh-builtin/src/chatgpt/tool/execute.rs`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for safety / guard / command policy entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/safety/`, `dsh-builtin/src/safe_run.rs`, and `dsh-builtin/src/chatgpt/tool/execute.rs`.
- Keep policy and execution changes separated unless the task requires both.
- Validate touched packages: `cargo test -p doge-shell` for shell policy changes; `cargo test -p dsh-builtin` for builtin or tool execution changes.
