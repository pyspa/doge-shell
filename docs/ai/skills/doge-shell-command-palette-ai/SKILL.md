---
name: doge-shell-command-palette-ai
description: Use for doge-shell command palette, AI actions, diagnose, explain, suggest, argument_explainer, command_palette, ai_features, or コマンドパレット work.
---

# Doge Shell Command Palette AI

- Start with `rg -n "command_palette|ai_features|diagnose|explain|suggest|argument_explainer|OpenAI" dsh/src/command_palette dsh/src/ai_features dsh/src/argument_explainer.rs dsh-openai/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for command palette / AI action entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/command_palette/`, `dsh/src/ai_features/`, `dsh/src/argument_explainer.rs`, and `dsh-openai/src/` when client/config behavior is involved.
- Keep chatgpt / MCP tool work in `$doge-shell-chat-tools`; this skill is for shell-side palette and AI action flows.
- Validate touched packages: `cargo test -p doge-shell`; add `cargo test -p dsh-openai` only when OpenAI client/config code changes.
