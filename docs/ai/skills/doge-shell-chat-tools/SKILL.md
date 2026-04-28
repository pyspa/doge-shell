---
name: doge-shell-chat-tools
description: Use for doge-shell chatgpt, MCP, serve, tool, runtime skill, doctor, or OpenAI client work. Narrows reads to builtin chat/MCP code and the OpenAI client crate.
---

# Doge Shell Chat Tools

- Start with `rg -n "chat|skill|tool|tool_call|MCP|serve|doctor|OpenAI|environment snapshot" dsh-builtin/src dsh-openai/src dsh-types/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the default files.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh-builtin/src/chatgpt/`, `dsh-builtin/src/serve/`, `dsh-builtin/src/doctor.rs`, `dsh-openai/src/`, and `dsh-types/src/mcp.rs`.
- Validate with `cargo test -p dsh-builtin`; add `cargo test -p dsh-openai` or `cargo test -p dsh-types` only when those crates change.
