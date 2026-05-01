---
name: doge-shell-env-startup
description: Use for doge-shell environment, startup, direnv, project context, path activation, 環境変数, 起動処理, プロジェクトコンテキスト, or path work.
---

# Doge Shell Env Startup

- Start with `rg -n "direnv|environment|activation|project context|startup|PATH|path" dsh/src/environment dsh/src/direnv.rs dsh/src/lib.rs dsh/src/main.rs dsh-builtin/src/project_context.rs`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for environment / project context and lisp / startup entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/environment/`, `dsh/src/direnv.rs`, `dsh/src/lib.rs`, `dsh/src/main.rs`, and `dsh-builtin/src/project_context.rs`.
- Validate touched packages: `cargo test -p doge-shell` for `dsh/`; `cargo test -p dsh-builtin` for builtin project context; use workspace checks only for cross-crate behavior.
