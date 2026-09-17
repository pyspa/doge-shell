# `!` チャット / エージェントのコアループ（調査済み・未着手）

[../open-questions.md](../open-questions.md) から分割。AI 機能の設計方針全体の索引は
[../README.md](../README.md) を見る。

`chatgpt.rs`・`conversation.rs`・`dsh-openai`・`AgentRuntime` の新規精査で見つかった、
既知の設計判断（temperature 固定・経路Bの非ストリーミング・400リカバリのモデル非依存学習など、
[../open-questions.md](../open-questions.md) の各セクション）とは別の、未文書化だった項目。
今回は C-4（早期 return が checkpoint/finish を飛ばす）と C-1（要約+rewindで履歴が消える）
だけ直した。残りは以下。

- **「同一操作3回失敗」ガードが `execute` では事実上効かない**。
  `dsh-builtin/src/agent/mod.rs` の `after_tool` の失敗シグネチャに**ツール結果の全文**
  （`AgentJobs::snapshot` の JSON。毎回新しい job_id/pid を含む）が入るため、同じコマンドが
  同じ理由で失敗し続けても署名が毎回不一致になり `repeats` が進まない。
- **store 読み取り失敗が「ユーザーによるキャンセル」に化ける**。
  `AgentRuntime::stopped()`（同ファイル）は `store.load(...).map_or(true, ...)` で、
  一時的な SQLite 読み取り失敗を「タスクはキャンセルされた」と誤解釈する。他の箇所は
  store エラーを一貫して `TaskFailure::StateUnusable` として扱っているのに、ここだけ別の
  意味に変換される。
- **`compact_buffer` は `last_prompt_tokens` を下げない**ため、`should_summarize()` の
  「prompt_tokens > budget」条件で入った要約は、無料の圧縮がどれだけ効いても毎回課金される
  （`last_prompt_tokens` は要約自身でしかクリアされない）。
- **`stopped()` の高頻度同期 SQLite 読み取り**。キャンセル判定のポーリング
  （`dsh-openai/src/client/streaming.rs` の 50ms、`dsh-builtin/src/chatgpt/tool/execute.rs`
  の 20ms）のたびに SQLite の SELECT+デシリアライズが走り、`synchronous=FULL` の同一 DB への
  `save()` との競合を自ら誘発しうる（上の「キャンセルに化ける」の顕在化確率も上げる）。
- **セッションIDの peek/take レース**。`turn_support.rs` の `peek_id` → hook 実行 → 人間の
  承認待ち → `take` の間に `AI_CHAT_SESSION_TTL_SECS` を跨ぐと、`hook_ctx` が古いセッションIDを
  持ったまま新しい会話が始まり、`jobs::retain_session` が前の会話のジョブを誤って生き残らせる。
- **ストリーム劣化リトライで `stream_options` が残り、タイムアウトも効かなくなる**。
  `dsh-openai/src/client.rs` の 400 リカバリが `stream` だけを名指しされたとき
  `stream_options` を消し忘れる。劣化後の非ストリーミング再試行も `MAX_TIMEOUT_SECS`(1800s)
  のままで、`AI_CHAT_TIMEOUT_SECS` が実質無効化されうる。
- **予算ちょうどで完了・検証済みのタスクが `Interrupted` になる**。
  `AgentRuntime::finish`（`dsh-builtin/src/agent/mod.rs`）の最終判定が
  `tokens_used >= token_budget` / `elapsed_ms >= time_budget_ms` を再評価するため、
  最終ラウンドで予算に到達しつつ回答・検証が完了しても `Completed` にならないことがある。
- **`--detach` + `--reconcile` は監査イベント `reconciled` を残さない**。前景 `run_task`
  は reconcile 時に専用イベントを保存するが、`dsh/src/agent/detach.rs` の `start` はタスク本体の
  更新のみで、子プロセスには `reconcile=None` を渡すため detach 経路だけ記録が欠落する。
- **agent タスクでは `begin_turn` が呼ばれず、ターン統計が `agent resume` を跨いで累積する**。
  `AI_CHAT_TURN_TOKEN_BUDGET` を設定した環境で `agent resume` すると、前回消費分だけで
  即座にターン予算超過になりうる。
