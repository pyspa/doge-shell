---
name: doge-shell-lisp-config
description: Use for doge-shell Lisp, config.lisp, config loader, startup Lisp, stdlib, Lisp設定, 起動設定, include, or reload work. Narrows reads to Lisp/config startup paths.
---

# Doge Shell Lisp Config

- Start with `rg -n "lisp|config\\.lisp|default environment|stdlib|include|reload" dsh/src/lisp dsh/src/lib.rs dsh/src/main.rs dsh-builtin/src`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for lisp / config loader / startup entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are `dsh/src/lisp/`, `dsh/src/lib.rs`, `dsh/src/main.rs`, `dsh-builtin/src/lisp.rs`, `dsh-builtin/src/include.rs`, and `dsh-builtin/src/reload.rs`.
- Keep shell parser work in `$doge-shell-parser-shell`; this skill is for Lisp/config semantics.
- Register native functions in `dsh/src/lisp/default_environment.rs`; per-area implementations live beside it (`command_palette.rs`, `keybind.rs`, `cron.rs`).
- `config.lisp` is evaluated as one program via `LispEngine::run_program()`: it parses every form before running any, so a single failing expression rolls the whole file back. Check new `README.md` examples actually evaluate.
- `LispEngine::run()` is the single-expression API (shell `lisp` builtin, internal lookups). `LispEngine::run_program()` is the complete-program API (parse-all-before-eval, last value wins): `config.lisp`, `dogesh -l`, and `dogesh lisp FILE` all go through it. Do not mix them up.
- Script argv comes from the shared `Environment::invocation` (synced to `*argv0*`/`*argv*` just before program execution, after `config.lisp`). Never add a second argv authority or export argv as environment variables.
- Adding config state to `Environment` means adding it to `EnvironmentSnapshot` too — see [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md) for which state must *not* be rolled back.
- Only `fn` exports a lambda as a shell command; `defun` does not (`is_export`). Generated Lisp that should be callable must use `fn`.
- Validate touched packages: `cargo test -p doge-shell` for `dsh/`; add `cargo test -p dsh-builtin` when builtin include/reload/lisp commands change.
