---
name: doge-shell-builtin-commands
description: Use for doge-shell builtin commands, proxy builtins, help, project, export, task, snippet, git builtins, or 組み込みコマンド work outside chat/MCP tools.
---

# Doge Shell Builtin Commands

- Start with `rg -n "<command>|builtin|help|project|export|task|snippet|git" dsh-builtin/src dsh/src/proxy/builtin`.
- Read [../doge-shell-repo/references/task-map.md](../doge-shell-repo/references/task-map.md) for builtin command entries.
- Read [../doge-shell-repo/references/package-map.md](../doge-shell-repo/references/package-map.md) before choosing cargo package names.
- Default read targets are command-specific files under `dsh-builtin/src/` and `dsh/src/proxy/builtin/`.
- For interactive builtin prompts, use the shared tty-aware input helper instead of reading `std::io::stdin()` directly; REPL command execution may not have stdin in a safe line-input state.
- Keep `ShellProxy` as the frozen compatibility facade. Add new builtin dependencies to a narrow trait in `dsh-builtin/src/shell_capabilities.rs`, and make internal helpers accept only the capabilities they use.
- After changing `ShellProxy` or a capability trait, run `scripts/check-shell-proxy-capabilities.py` and `cargo test -p dsh-builtin`.
- Directory stack (`pushd` / `popd` / `dirs`, `cd +N`/`-N`) lives in `dsh-builtin/src/dirstack.rs`; the module is not named `dirs` because this crate depends on the `dirs` crate. Read [../doge-shell-repo/references/invariants.md](../doge-shell-repo/references/invariants.md) before touching anything that changes directory.
- Scheduled tasks (`sched`) live in `dsh-builtin/src/sched.rs`; the state machine is `dsh/src/scheduler/`, shared types are `dsh-types/src/schedule.rs`.
- Project activation and tasks live in `project.rs`, `project_context.rs`, and `task.rs`. mise activation must check trusted or conservatively safe config, use `--no-hooks env --json`, and never trust/install/run hooks automatically. External task providers need a timeout and marker-keyed cache; static parsers remain the fallback.
- Machine-readable modes must emit one valid JSON value without tables, ANSI, or interactive selection. Focused `doctor <section> --json` output must retain the common envelope and populate section-specific `details`. Preserve `TaskInfo` fields `id/source/name/command/description/cwd` and `task <source>:<name> -- <args>` forwarding; npm tasks must keep the runner separator (`npm run <name> -- <args>`).
- Use `$doge-shell-chat-tools` for chatgpt / MCP / runtime skill code and `$doge-shell-safety-policy` for safe_run or command policy changes.
- Validate with `cargo test -p dsh-builtin`; add `cargo test -p doge-shell` only when proxy builtin behavior in `dsh/` changes.
