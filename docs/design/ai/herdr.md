# 10. Herdr 連携

[README.md](README.md) の §10。AI 機能の設計方針全体の索引はそちらを見る。

`dsh/src/agent_lifecycle/` が唯一の実装(`doge-shell` package に閉じる。`dsh-builtin`/`dsh-types`/
`ShellProxy` は変更しない)。既定は OFF — `DOGESH_HERDR_ENABLED=1`（`Environment::get_var` 経由、値は `1`/`true`/`on`/`yes` のみ ON）で有効化する。無効時は `NullReporter` で `herdr` 呼び出しは一切行わない。

- Herdr は「custom source が lifecycle authority を握っている間、そのペインでは組み込みの画面
  検出を止める」仕様(herdr.dev の integrations ガイド)。つまり dogesh が
  `custom:doge-shell` を握り続けると、dogesh の中で `codex`/`claude` を起動しても Herdr 側は
  `dogesh` のまま変わらない。
- そこで `AgentLifecycleManager::begin_yield`/`YieldGuard`
  (`agent_lifecycle::yield_to_foreground_agent` がエントリポイント)が、認識済みエージェント
  CLI(既定リストは `agent_lifecycle/agent_command.rs`、`DOGESH_HERDR_AGENT_COMMANDS` で追加/除外)
  が **前景** で実行されている間だけ `release-agent` で authority を明け渡し、終了時に
  `report-agent` で取り戻す。フック位置は `shell/eval.rs`(通常実行)と
  `proxy/builtin/jobs/fg.rs::execute_fg`(`fg` での再開)の 2 箇所だけ。
- `report-agent`/`release-agent` の argv 生成・`--seq` 採番・`herdr` 呼び出しの timeout/killpg
  は `agent_lifecycle::herdr` に一本化されている。再実装しない。

## `agent` コマンドの残骸

`agent` builtin は削除済み。`dsh-types/src/agent.rs`（状態型）と `dsh-builtin/src/agent/`
（`AgentRuntime`・ファイル・ジョブ・SRT アダプター）は残っているが、本番の `Shell.agent_runtime` は
常に `None` で、タスク経路はテストからしか到達しない（[README.md](README.md) §2）。新しいループの
入口として再利用しない。
