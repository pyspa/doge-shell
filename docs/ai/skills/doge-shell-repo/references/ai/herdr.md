# 10. Herdr 連携

[README.md](README.md) の §10。AI 機能の設計方針全体の索引はそちらを見る。

`dsh/src/agent_lifecycle/` が唯一の実装(`dogesh` crate に閉じる。`dsh-builtin`/`dsh-types`/
`ShellProxy` は変更しない)。

- Herdr は「custom source が lifecycle authority を握っている間、そのペインでは組み込みの画面
  検出を止める」仕様(herdr.dev の integrations ガイド)。つまり dogesh が
  `custom:doge-shell` を握り続けると、dogesh の中で `codex`/`claude` を起動しても Herdr 側は
  `dogesh` のまま変わらない。
- そこで `AgentLifecycleManager::begin_yield`/`YieldGuard`
  (`agent_lifecycle::yield_to_foreground_agent` がエントリポイント)が、認識済みエージェント
  CLI(既定リストは `agent_lifecycle/agent_command.rs`、`DOGESH_HERDR_AGENT_COMMANDS` で追加/除外)
  が **前景** で実行されている間だけ `release-agent` で authority を明け渡し、終了時に
  `report-agent` で取り戻す。フック位置は `shell/eval.rs`(通常実行)と
  `proxy/builtin/jobs.rs::execute_fg`(`fg` での再開)の 2 箇所だけ。
- `report-agent`/`release-agent` の argv 生成・`--seq` 採番・`herdr` 呼び出しの timeout/killpg
  は `agent_lifecycle::herdr` に一本化されている。再実装しない。

## 永続タスク (廃止)

`agent` コマンドは削除された。`dsh-types/src/agent.rs` が状態型、
`dsh-builtin/src/agent/` が `!` チャットのツール承認に使う実行時 (`AgentRuntime`・ファイル・ジョブ・SRTアダプター) を所有する。ループは追加しない。

結果のない変更操作を再送しない。チェックポイントに残るtool callは永続イベントの結果で補い、結果不明ならユーザーの実状態確認を要求する。予算は再開でリセットしない。モデルの最終回答だけで完了にしない。認証情報の保存、権限の外部コンテンツからの拡大、隔離失敗時の通常実行へのフォールバックは禁止。

再発防止の焦点: 対話用セッションTTLをタスクへ適用しない。互換APIのusage欠落をゼロ使用扱いしない。ジョブ結果のJSONを文字列途中で切らない。SRTがPATHで別のbashを選んでもシステム外への読み取り権限を自動で追加しない。操作結果の不明判定は表示文字列ではなく型で渡す。`Cancelled` は通常保存で解除しない。
