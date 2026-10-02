---
name: doge-shell-repo
description: Use when working in the doge-shell repository or doge-shell リポジトリ. Routes to the right crate or narrower doge-shell skill, avoids broad reads, and chooses the smallest effective Rust validation command.
---

# Doge Shell Repo

- Follow `AGENTS.md`; use `python3 scripts/agent-context.py --topic "<task>" --json` for nontrivial work. Add a known repo-relative `--path` to narrow ambiguous requests.
- Read the returned `skills[].path` and relevant references directly from the checkout. Do not install every domain skill or read the full catalog first. If already routed in this task, reuse that result until scope changes.
- Use [references/task-map.md](references/task-map.md) only when routing is unavailable or returns `repo-general`; search for the relevant entry rather than reading the whole map.
- Start source exploration with targeted `rg --files` / `rg -n`. Read [references/read-boundaries.md](references/read-boundaries.md) when search output is broad or a large file needs inspection.
- Use [references/module-map.md](references/module-map.md) only when ownership is unclear. Use [references/package-map.md](references/package-map.md) when the Cargo package is unknown.
- Before editing a shared authority, open only the relevant entry in [references/invariants.md](references/invariants.md). For OS-specific work use [references/platform-support.md](references/platform-support.md); for product AI features use [references/ai-architecture.md](references/ai-architecture.md).
- For syntax or execution behavior, inspect `dsh/src/shell.pest` and the relevant `dsh/tests/spec/*.toml` contracts before implementation.
- Select validation from [references/test-scope.md](references/test-scope.md); routing is not the validation authority.
- For debug switches or AI settings use [references/env-vars.md](references/env-vars.md). For agent guidance improvements and evaluation use [references/agent-efficiency.md](references/agent-efficiency.md).
