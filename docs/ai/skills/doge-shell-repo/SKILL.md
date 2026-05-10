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
- Read [references/test-scope.md](references/test-scope.md) before choosing cargo commands.
- Open `README.md` only for user-facing docs, config examples, or feature behavior that is described there.
- Prefer the smallest test or check that proves the change.
- Repo-local completion skill: [doge-shell-repl-completion](../doge-shell-repl-completion/SKILL.md).
- Repo-local process skill: [doge-shell-process-pty](../doge-shell-process-pty/SKILL.md).
- Repo-local Lisp/config skill: [doge-shell-lisp-config](../doge-shell-lisp-config/SKILL.md).
- Repo-local command palette skill: [doge-shell-command-palette-ai](../doge-shell-command-palette-ai/SKILL.md).
- Repo-local builtin command skill: [doge-shell-builtin-commands](../doge-shell-builtin-commands/SKILL.md).
- Repo-local chat tool skill: [doge-shell-chat-tools](../doge-shell-chat-tools/SKILL.md).
- Repo-local serve/web skill: [doge-shell-serve-web](../doge-shell-serve-web/SKILL.md).
- Repo-local notebook/markdown skill: [doge-shell-notebook-markdown](../doge-shell-notebook-markdown/SKILL.md).
- Switch to a narrower skill when the task is clearly about completion, parser, process/PTY, prompt/terminal UI, environment/startup, Lisp/config, history/frecency, command palette/AI actions, builtin commands, serve/web, notebook/markdown, safety policy, chat tools, investigation, validation, or skill authoring.
- If a narrower skill is not installed in runtime, read its repo-local source at `docs/ai/skills/<skill>/SKILL.md` instead of installing every skill.
