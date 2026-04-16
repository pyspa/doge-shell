---
name: doge-shell-repo
description: Use when working in the doge-shell repository. Routes to the right crate or narrower doge-shell skill, avoids broad reads, and chooses the smallest effective Rust validation command.
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
- Switch to a narrower skill when the task is clearly about completion, parser work, chat tools, investigation, or validation.
