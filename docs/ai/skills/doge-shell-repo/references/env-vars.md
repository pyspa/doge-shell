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

## AI 機能の変数

正典の表と既定値は [ai/env-vars.md](ai/env-vars.md) にある。ここには「探しに行く先」だけ置く。

| 変数 | 定義位置 | 用途 |
|---|---|---|
| `AI_CHAT_API_KEY` / `AI_CHAT_BASE_URL` / `AI_CHAT_MODEL` / `AI_CHAT_TIMEOUT_SECS` / `AI_CHAT_ALLOW_INSECURE_HTTP` | `dsh-openai/src/config.rs` | プロバイダ設定。`OPENAI_*` は legacy alias |
| `AI_SUMMARY_MODEL` / `AI_CHAT_CONTEXT_TOKEN_BUDGET` / `AI_CHAT_TURN_TOKEN_BUDGET` / `AI_MESSAGE_LANG` / `CHAT_PROMPT` / `AI_CHAT_STREAM` | `dsh-builtin/src/chatgpt/settings.rs` | `!` チャットの会話管理・応答言語・逐次表示の有効/無効（既定 on） |
| `AI_CHAT_SESSION_TTL_SECS` | `dsh-builtin/src/chatgpt/session/mod.rs` | 連続する `!` が会話を共有する idle timeout。`0` で無効。継続範囲は cwd 完全一致ではなく `tool::workspace_root`（プロジェクト単位） |
| `AI_CHAT_EXECUTE_ALLOWLIST` / `DOGESH_EXECUTE_TOOL_CONFIG` | `dsh-builtin/src/chatgpt/tool/execute.rs` | `execute` ツールの allowlist と JSON 設定の置き場所 |
| `AI_CHAT_PROJECT_SKILLS` | `dsh-builtin/src/chatgpt/settings.rs` | `0`/`false`/`off`/`no` で `<project>/.dogesh/skills` をプロンプトから外す。既定 on |
| `AI_CHAT_HOOKS` / `DOGESH_AI_HOOKS_CONFIG` | `dsh-builtin/src/chatgpt/hooks/config.rs` | AI chat hooks の有効/無効と `ai-hooks.json` の場所 |
| `DOGESH_HOOK_DEPTH` | 同上 | hook プロセスに立つ。立っているシェルは hooks を全面無効化する。**プロセス環境だけを見る**（シェル変数で消せると無限再帰する） |
| `SAFETY_LEVEL` | `dsh-types/src/safety_policy.rs` | 起動時に `policy_state.safety_level` へ seed される。**単一ソースは policy_state のほう**、変数は表示用 |

これらは **シェル変数 → プロセス環境** の順に解決する（`chatgpt::load_openai_config`）。`std::env::var` だけを見る新しいキーを足さない。
この表の中での例外は `DOGESH_HOOK_DEPTH` で、これは意図的にプロセス環境だけを見る（理由はコードのコメントにある）。同種の例外は下の「Herdr 連携の変数」にもある。

## Herdr 連携の変数

| 変数 | 定義位置 | 用途 |
|---|---|---|
| `HERDR_ENV` / `HERDR_PANE_ID` / `HERDR_BIN_PATH` | `dsh/src/agent_lifecycle/herdr.rs`（`HerdrEnv::detect`） | Herdr pane 内で起動されたことの検出。**プロセス環境だけを見る**（`DOGESH_HOOK_DEPTH` と同じ理由。シェル変数で「Herdr 配下だ」と偽装・抑止できてはいけない） |
| `DOGESH_HERDR_OWNER_PID` | 同上 | 同一 pane 内の入れ子 `dogesh` が lifecycle authority を取り合わないためのガード。プロセス環境だけを見る |
| `DOGESH_HERDR_AGENT_COMMANDS` | `dsh/src/agent_lifecycle/agent_command.rs` | `codex`/`claude` など前景で認識するエージェント CLI 名の `:` 区切りリスト。素の名前は追加、`-name` は既定リストから除外 |
| `DOGESH_HERDR_AGENT_HANDOFF` | 同上 | `0`/`false`/`off`/`no` で前景エージェントへの pane 明け渡し機能自体を無効化。既定 on |

`DOGESH_HERDR_AGENT_COMMANDS`/`DOGESH_HERDR_AGENT_HANDOFF` は他の AI 機能の変数と同じく **シェル変数 → プロセス環境** の順（`Environment::get_var`）。`HERDR_*`/`DOGESH_HERDR_OWNER_PID` は `DOGESH_HOOK_DEPTH` と同じ理由でプロセス環境のみを見る。
