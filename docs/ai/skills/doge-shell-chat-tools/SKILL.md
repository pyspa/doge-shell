---
name: doge-shell-chat-tools
description: Use for doge-shell chatgpt, MCP, tool, runtime skill, or OpenAI client work. Narrows reads to dsh-builtin chat code and the OpenAI client crate.
---

# Doge Shell Chat Tools

- Start with `rg -n "chat|skill|tool|MCP|OpenAI|environment snapshot" dsh-builtin/src dsh-openai/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the default files.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh-builtin/src/chatgpt/`, `dsh-builtin/src/doctor.rs`, and `dsh-openai/src/`.
- Validate with `cargo test -p dsh-builtin` unless the edit clearly changes `dsh-openai` or crosses crates.
