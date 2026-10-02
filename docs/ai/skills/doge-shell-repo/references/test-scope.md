# Test Scope

- `cargo test -p dsh-builtin`: builtin, chat, MCP, runtime skill loading
- `cargo test -p dsh-builtin footprint`: prompt footprint profiler (`doctor ai --prompt-size` の計測・回帰。`budget.json` の上限更新時は `prompt_footprint_report_canonical_values_for_baselining -- --nocapture` で現在値を再測定する)
- `cargo test -p dsh-builtin doctor`: doctor text/JSON レポート
- `cargo test -p dsh-openai usage`: provider token usage と cache 計算
- `cargo test -p doge-shell`: parser, repl, completion, prompt, shell behavior
- `cargo test -p doge-shell --lib <filter>`: 反復ループの既定。`dsh/tests/` の統合テストはサブプロセスをグローバル Mutex で直列化するので、実装を回している間は `--lib` とテスト名フィルタが速い。完了前に一度だけフルの `cargo test -p doge-shell` を回す
- `cargo test -p doge-shell --test shell_contract`: 構文・実行の契約（`dsh/tests/spec/*.toml`）を変えたとき
- `completions/*.json` を触ったとき: `cargo test -p doge-shell --lib completion::json_loader`
- `cargo test -p dsh-openai`: OpenAI-compatible client or config loading
- `cargo test -p dsh-types`: shared type changes, especially MCP/project/output data shapes
- `cargo test -p dsh-frecency`: frecency scoring or store changes
- `cargo test`: cross-crate changes only
- `cargo check --workspace`: broad compile check when behavior spans many crates
- `scripts/check-portability.py`: `target_os` arms, OS-specific paths, absolute command paths in tests, or `.cargo/config.toml` changes
- `scripts/check-file-budget.py`: a new or split `.rs` file over 400/800 lines, or path references inside `AGENTS.md`/`CLAUDE.md`/`docs/ai/`
- `scripts/check-ai-guidance.sh`: `AGENTS.md`, `CLAUDE.md`, `docs/ai/`, Skill, runtime skill installer, or `.claude/` changes
- `docs/ai/agent-routing.json`、`docs/ai/evals/`、`scripts/agent_routing.py`、`scripts/agent-context.py`、またはroutingに影響するSkill metadataを変えたら: `python3 scripts/eval-agent-routing.py` と `python3 scripts/check-agent-eval-fixtures.py`。mutation/task自体を変えたら `python3 scripts/check-agent-eval-fixtures.py --case <case-id> --smoke` も回す。全AI guidance変更で毎回全smokeは要求しない（routing/evalに影響する変更に限定）
- `scripts/check-project-consistency.py`: workspace `Cargo.toml`, crate manifests, `README.md`, or `LICENSE` changes
- `scripts/install-runtime-skills.sh --dry-run --target codex --profile codex-core`: Codex runtime profile changes
- `scripts/install-runtime-skills.sh --status --target codex --profile codex-core`: canonical/runtime state display (always exits zero for valid arguments)
- `scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core`: strict canonical/runtime drift gate

The `dsh/` directory uses the Cargo package name `doge-shell`, so prefer package names from `package-map.md` when selecting commands.

## CI layering

- PR/push CI (`.github/workflows/ci.yml`): deterministic workspace + execution contract tests only (`test`/`contract` on Linux+macOS; `lint`/`msrv` on Ubuntu). Do not move stress jobs back into `ci.yml`.
- develop push (`.github/workflows/stress.yml` `stress-develop`): lightweight Ubuntu execution stress (`--stress-count 2`). Not a PR merge gate.
- nightly/manual (same file: `stress-full`, `stress-resource-fd`, `stress-resource-repeated`): full Linux/macOS stress (fast x10, FD-budget x3, repeated substitution x5) against `develop`. Full stress is never a PR merge gate.

Do not start with workspace-wide tests unless the change clearly crosses crate boundaries.

`--message-format short` を付けると clippy / rustc の 1 診断が 8 行から 1 行になる。clippy は CI と同じく必ず `--all-targets` を付ける: `cargo clippy -p <package> --all-targets --message-format short -- -D warnings`。

## CI をローカルで再現する最小セット

`doctor validate`（`./target/release/dogesh -c "doctor validate --json"`）は release バイナリがソースより新しいときだけ使う。無い・古いときはこのセットと上の package 表を正とする。

- 常に: `cargo fmt --all -- --check`、触った package ごとに `cargo clippy -p <package> --all-targets -- -D warnings`、触った package の `cargo test`
- `.rs` を触ったら: `scripts/check-portability.py`、`scripts/check-runtime-authority.py`、`scripts/check-file-budget.py`
- `dsh/src/process/`・`dsh/src/proxy/builtin/jobs/`・`dsh/src/shell/{job,job_exit}.rs`・`dsh/src/shell/process_substitution/`: `scripts/check-execution-authority.py` と `cargo test -p doge-shell --test shell_contract` / `--test resource_contract`
- `ShellProxy` / capability trait: `scripts/check-shell-proxy-capabilities.py`
- AI guidance・Skill・`.claude/`: `scripts/check-ai-guidance.sh`
- routing/evalに影響する変更（`docs/ai/agent-routing.json`、`docs/ai/evals/`、`scripts/agent_routing.py`、`scripts/agent-context.py`、routingに影響するSkill metadata）: `python3 scripts/eval-agent-routing.py` と `python3 scripts/check-agent-eval-fixtures.py`。mutation/task自体を変更した場合は `python3 scripts/check-agent-eval-fixtures.py --case <case-id> --smoke` も推奨
- `Cargo.toml` / `README.md` / `LICENSE`: `scripts/check-project-consistency.py`

Terminal-touching code (`dsh/src/repl/`, `dsh/src/terminal/`, `dsh/src/process/job_pty.rs`, `dsh/src/process/job_wait.rs`, `dsh/src/shell/eval.rs`): see the "テストと実端末" section of `invariants/terminal.md` first. If a run leaves the terminal misbehaving, `cargo test < /dev/null` isolates fd 0, and `reset` clears a stale DECSTBM margin that `stty sane` cannot.

Never use `cargo test -p dsh`; the `dsh/` directory is the `doge-shell` package.
