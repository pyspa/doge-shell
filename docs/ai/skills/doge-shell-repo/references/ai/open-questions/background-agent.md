# 背景エージェント (`agent run --detach`)（調査済み・未着手）

[../open-questions.md](../open-questions.md) から分割。AI 機能の設計方針全体の索引は
[../README.md](../README.md) を見る。

`agent run --detach` の再監査（2回目の `/code-review --fix` 相当の調査）で見つかった、
今回は見送った項目。修正は `dsh/src/agent/cli.rs`・`doctor.rs`・`detach.rs`・`watch.rs` が対象になる。

- **`agent doctor`/`agent list` の staleness・非表示判定が `task.created_at`（作成時刻）起点**。
  「いつ `InputRequired` になったか」を記録するフィールドが `AgentTask`/`TaskEvent` に無いため。
  誤差の向きは「早すぎる警告」「早すぎる非表示」だけ（実際のブロック時刻 ≥ created_at なので
  過大評価）なので実害は小さいが、`InputRequired` を立てる箇所が
  `dsh/src/proxy/agent_policy.rs`・`dsh-builtin/src/chatgpt/tool/mod.rs`・
  `.../tool/safety_gates.rs`・`dsh-builtin/src/agent/mod.rs` の4箇所に散っているため、
  直すなら `AgentRuntime` 側への集約が前提になる。
- **`cli.rs` の `still_going()` に、親（`detach::start`）がロックを解放してから子が
  自分のロックを取得するまでの競合窓がある**。`docs/agent.md` 自身が勧めるワークフロー
  （`agent run --detach ...; agent wait ID`）で確実に踏める大きさ（子の起動には
  SQLite open + 設定解決込みで数十〜数百 ms かかる）。
- **`detach::start` は spawn/ログオープンより前に `stop_reason = None` を保存する**ため、
  そこで失敗すると承認理由だけ消えたタスクが残りうる。
- **ステータス行の `🤖` は `task.status == Running` の数**で、`watch.rs` は
  `recover_interrupted` を呼ばないため、プロセスがクラッシュしても誰かが `agent` サブコマンドを
  叩くまで残り続ける。`locks::is_running` で数える方式への変更が要る。
