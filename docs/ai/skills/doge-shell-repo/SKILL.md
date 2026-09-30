---
name: doge-shell-repo
description: Use when working in the doge-shell repository or doge-shell リポジトリ. Routes to the right crate or narrower doge-shell skill, avoids broad reads, and chooses the smallest effective Rust validation command.
---

# Doge Shell Repo

- Start with `rg --files` or `rg -n`; do not open broad files first.
- Read [references/task-map.md](references/task-map.md) first when the task type is already clear.
- Read [references/package-map.md](references/package-map.md) before choosing Cargo package names.
- Read [references/read-boundaries.md](references/read-boundaries.md) before opening `README.md` or running broad tests.
- Read [references/module-map.md](references/module-map.md) only when ownership is unclear.
- Read [references/invariants.md](references/invariants.md) (index) before editing cwd changes, `Environment` state, key dispatch, terminal drawing, output history, cron, completion definitions, safety checks, or execution/job lifecycle; open only the matching `invariants/*.md`.
- Read [references/test-scope.md](references/test-scope.md) before choosing cargo commands.
- Read [references/platform-support.md](references/platform-support.md) before adding a `target_os` arm, reading an OS-specific source (`/proc`, `/etc/passwd`, `sysctl`), or calling an external command by absolute path from a test.
- Read [references/env-vars.md](references/env-vars.md) when you need a debug switch (`DOGESH_LOG`, PTY/terminal/completion toggles) or an `AI_CHAT_*` setting (pointer to `docs/design/ai/env-vars.md`) instead of editing code.
- Read [references/ai-architecture.md](references/ai-architecture.md) before touching the AI features (`!` chat, MCP, tools, command-palette AI actions, `ai-commit`, `safe-run`, ghost text): it points to the canonical design notes in `docs/design/ai/`, including what must not be reimplemented.
- For shell syntax or execution behavior questions, read `dsh/src/shell.pest` (grammar) and `dsh/tests/spec/*.toml` (contract cases) before the implementation.
- Open `README.md` only for user-facing docs, config examples, or feature behavior that is described there.
- Prefer the smallest test or check that proves the change.
- For domain work, read the router's `skills[].path` directly from the repository root. For example, execution work reads `docs/ai/skills/doge-shell-execution-semantics/SKILL.md`; completion definitions read `docs/ai/skills/doge-shell-completion-spec/SKILL.md`.
- Switch to a narrower skill when the task is clearly about completion definitions, completion, parser, process/PTY, execution semantics, prompt/terminal UI, environment/startup, Lisp/config, history/frecency, command palette/AI actions, builtin commands, serve/web, notebook/markdown, safety policy, chat tools, investigation, validation, or skill authoring.
- If a narrower skill is not installed in runtime, read its repo-local source at `docs/ai/skills/<skill>/SKILL.md` instead of installing every skill.
