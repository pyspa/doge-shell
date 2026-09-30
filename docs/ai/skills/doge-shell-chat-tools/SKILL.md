---
name: doge-shell-chat-tools
description: Use for doge-shell `!` chat, chatgpt, MCP client, chat tool, runtime skill, AI chat hooks, OpenAI client, チャット, ツール, or スキル work. Narrows reads to builtin chat/MCP code and the OpenAI client crate.
---

# Doge Shell Chat Tools

- Start with `rg -n "chat|skill|tool|tool_call|MCP|OpenAI|environment snapshot" dsh-builtin/src/chatgpt dsh-builtin/src/mcp.rs dsh-openai/src dsh-types/src/mcp.rs`.
- Read [../doge-shell-repo/references/ai-architecture.md](../doge-shell-repo/references/ai-architecture.md) first: it points to `docs/design/ai/`, the canonical policy for the AI features, including what must not be reimplemented.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for the default files.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh-builtin/src/chatgpt/`, `dsh-builtin/src/mcp.rs`, `dsh-openai/src/`, and `dsh-types/src/mcp.rs`. `serve` is [doge-shell-serve-web](../doge-shell-serve-web/SKILL.md); `doctor` is [doge-shell-builtin-commands](../doge-shell-builtin-commands/SKILL.md).
- Skills live in `dsh-builtin/src/chatgpt/skills/` (loading, usage counters) and `dsh-builtin/src/chatgpt/tool/skill.rs` (the `skill_manage` tool); AI chat hooks live in `dsh-builtin/src/chatgpt/hooks/`.
- Validate with `cargo test -p dsh-builtin`; add `cargo test -p dsh-openai` or `cargo test -p dsh-types` only when those crates change.
