# Test Scope

- `cargo test -p dsh-builtin`: builtin, chat, MCP, runtime skill loading
- `cargo test -p doge-shell`: parser, repl, completion, prompt, shell behavior
- `cargo test -p doge-shell --lib <filter>`: 反復ループの既定。`dsh/tests/` の統合テストはサブプロセスをグローバル Mutex で直列化するので、実装を回している間は `--lib` とテスト名フィルタが速い。コミット前に一度だけフルの `cargo test -p doge-shell` を回す
- `completions/*.json` を触ったとき: `cargo test -p doge-shell --lib completion::json_loader`
- `cargo test -p dsh-openai`: OpenAI-compatible client or config loading
- `cargo test -p dsh-types`: shared type changes, especially MCP/project/output data shapes
- `cargo test -p dsh-frecency`: frecency scoring or store changes
- `cargo test`: cross-crate changes only
- `cargo check --workspace`: broad compile check when behavior spans many crates
- `scripts/check-portability.py`: `target_os` arms, OS-specific paths, absolute command paths in tests, or `.cargo/config.toml` changes
- `scripts/check-ai-guidance.sh`: AGENTS, docs/ai, Skill, or runtime skill installer guidance changes
- `scripts/install-runtime-skills.sh --dry-run --target codex --profile codex-core`: Codex runtime profile changes
- `scripts/install-runtime-skills.sh --status --target codex --profile codex-core`: canonical/runtime drift checks

The `dsh/` directory uses the Cargo package name `doge-shell`, so prefer package names from `package-map.md` when selecting commands.

Do not start with workspace-wide tests unless the change clearly crosses crate boundaries.

`--message-format short` を付けると clippy / rustc の 1 診断が 8 行から 1 行になる。広い範囲を確認するときは `cargo clippy -p doge-shell --all-targets --message-format short -- -D warnings` を使う。

Terminal-touching code (`dsh/src/repl/`, `dsh/src/terminal/`, `dsh/src/process/job_pty.rs`, `dsh/src/process/job_wait.rs`, `dsh/src/shell/eval.rs`): see the "テストと実端末" section of `invariants.md` first. If a run leaves the terminal misbehaving, `cargo test < /dev/null` isolates fd 0, and `reset` clears a stale DECSTBM margin that `stty sane` cannot.

Never use `cargo test -p dsh`; the `dsh/` directory is the `doge-shell` package.

Use `cargo test -p doge-shell --lib` only as a fallback for library-scoped edits when package-level tests are blocked by known macOS sandbox child-`dsh` tracing failures.
