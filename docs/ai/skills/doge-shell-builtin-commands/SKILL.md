---
name: doge-shell-builtin-commands
description: Use for doge-shell builtin commands, proxy builtins, help, project, export, task, snippet, git builtins, or 組み込みコマンド work outside chat/MCP tools.
---

# Doge Shell Builtin Commands

- Start with `rg -n "<command>|builtin|help|project|export|task|snippet|git" dsh-builtin/src dsh/src/proxy/builtin`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for builtin command entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are command-specific files under `dsh-builtin/src/` and `dsh/src/proxy/builtin/`.
- Use `$doge-shell-chat-tools` for chatgpt / MCP / runtime skill code and `$doge-shell-safety-policy` for safe_run or command policy changes.
- Validate with `cargo test -p dsh-builtin`; add `cargo test -p doge-shell` only when proxy builtin behavior in `dsh/` changes.
