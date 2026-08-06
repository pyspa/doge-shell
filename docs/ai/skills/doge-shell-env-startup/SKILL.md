---
name: doge-shell-env-startup
description: Use for doge-shell environment, startup, direnv, project context, path activation, 環境変数, 起動処理, プロジェクトコンテキスト, or path work.
---

# Doge Shell Env Startup

- Start with `rg -n "direnv|environment|activation|project context|startup|PATH|path" dsh/src/environment dsh/src/direnv.rs dsh/src/lib.rs dsh/src/main.rs dsh-builtin/src/project_context.rs`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for environment / project context and lisp / startup entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/environment/`, `dsh/src/direnv.rs`, `dsh/src/lib.rs`, `dsh/src/main.rs`, and `dsh-builtin/src/project_context.rs`.
- `Environment` now also owns `keybindings` (config, rolled back on config.lisp failure), plus `dir_stack` and `scheduler` (runtime state, deliberately *not* rolled back). Read [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md) before adding a field or changing `changepwd`.
- `config.lisp` runs before `Repl::new`, so anything registrable from config must live on `Environment`, not on `Repl`.
- Validate touched packages: `cargo test -p doge-shell` for `dsh/`; `cargo test -p dsh-builtin` for builtin project context; use workspace checks only for cross-crate behavior.
