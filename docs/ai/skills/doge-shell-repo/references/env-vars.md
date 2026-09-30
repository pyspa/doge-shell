# Debug / Test Environment Variables

散在していて毎回探しに行くもの。挙動を切り替えたいときはコードを変える前にここを見る。

| 変数 | 定義位置 | 用途 |
|---|---|---|
| `DOGESH_LOG` | `dsh/src/bootstrap.rs` | tracing の `EnvFilter`。**`RUST_LOG` ではない**。既定は `info`、出力はファイル |
| `DOGESH_NO_TERMINAL_CONTROL` | `dsh/src/terminal/mod.rs` | 実端末への書き込み・termios 変更・`tcsetpgrp` を止める。`terminal_control_enabled()` は unit test ビルドでは常に false |
| `DOGESH_NO_PTY` | `dsh/src/process/job_pty.rs` | PTY 経路を無効化する |
| `DOGESH_STATUS_LINE` | `dsh/src/repl/status_line.rs` | ステータス行の有効/無効 |
| `DOGESH_HISTORY_PICKER` | `dsh/src/repl/init.rs` | `skim` を指定すると Ctrl-R が skim ピッカーになる |
| `DOGESH_COMPLETION_TIMING` | `dsh/src/completion/integrated/timing.rs` | 補完の所要時間を計測する |
| `DOGESH_COMPLETION_FISH_FALLBACK` | `dsh/src/completion/dynamic.rs` | fish の補完定義へのフォールバックを切り替える |
| `DOGESH_COMPLETION_MAX_ITEMS` | `dsh/src/completion/display.rs` | 補完グリッドの最大表示件数 |
| `DOGESH_EXTERNAL_COMPLETER` | `dsh/src/completion/dynamic.rs` / `dynamic/external.rs` | 外部コンプリータへ渡される（`DOGESH_COMPLETION_*` 一式と一緒に export される） |
| `DOGESH_ATUIN_DUAL_WRITE` | `dsh/src/history/command_history.rs` | `1` で atuin への二重書き込みを有効化 |
| `DOGESH_PERF_ITERS` | `dsh/benches/latency.rs` | `cargo bench` の反復回数 |
| `DOGESH_CHECK_BASE_REF` | `scripts/check.sh` | 差分チェックの base ref。既定 `develop`、無ければ `origin/develop` |

インストーラ側は `CODEX_HOME` / `XDG_CONFIG_HOME` / `CLAUDE_CONFIG_DIR` を見る（`scripts/install-runtime-skills.sh`）。`doctor skills` も `CODEX_HOME` を見る（`dsh-builtin/src/doctor/util.rs` の `codex_runtime_skills_dir`）。

## AI 機能・Herdr 連携の変数

`AI_CHAT_*` / `AI_*` / `SAFETY_LEVEL` / `DOGESH_HOOK_DEPTH` / `HERDR_*` / `DOGESH_HERDR_*` の正典の表と
既定値・解決順は `docs/design/ai/env-vars.md` にある。ここには重複して書かない。
