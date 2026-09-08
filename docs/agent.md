# 再開できるエージェントタスク

`agent` は既存の `!` の実行ループを使い、依頼・計画・検証・使用量を SQLite に保存する明示的な入口です。APIキーの設定だけでは開始しません。OpenAI互換の Chat Completions、ツール呼び出し、応答の `usage` が必要です。

## 開始と再開

```sh
agent run --tokens 50000 --timeout 900 --write . --allow-command 'cargo test -p dsh-types' --check '対象テストが成功する' -- 'テスト失敗を調査し修正して'
agent list
agent show TASK_ID
agent resume TASK_ID --tokens 80000 --timeout 1800
agent cancel TASK_ID
agent delete TASK_ID
```

時間は秒、トークンと時間の指定値は再開分を含む累積上限です。CLI指定がなければシェル変数、プロセス環境の順で `AI_AGENT_TOKEN_BUDGET` と `AI_AGENT_TIMEOUT_SECS` を解決します。両方に正の残量が必要です。トークン上限は次の要求を止める条件で、請求額の厳密な上限ではありません。使用量を返さないプロバイダではタスクを停止します。

初期の読み取り範囲は開始時の cwd です。`--read DIR`、`--write DIR` は既存ディレクトリを指定し、繰り返せます。コマンドは `--allow-command` の完全一致、MCP操作は `agent show` に表示された承認キーを `--allow-mcp` へ渡して許可します。範囲外の要求は入力待ちになります。`agent resume` に追加の権限を明示できます。外部送信・公開も対象と引数を含む別の許可です。

ツール実行前の意図と結果を別々に保存します。プロセス終了後、結果が残っていない操作を自動的に再実行しません。ファイルや外部サービスの状態を確認したうえで `agent resume TASK_ID --reconcile '実際に確認した結果'` を実行します。完了条件は開始後に差し替えられず、成功したツール結果を根拠に各条件を検証します。検証はモデルの判断を含むため、重要な条件は `--check` で具体的に指定してください。

タスク中の `skill_manage` は `edit` などと同じ変更操作として扱います。`task_plan` の記録前には実行できず、実行後は完了条件の検証済み判定をやり直します。`<project>/.dsh/skills` への書き込みはそのリポジトリの `--write` に従い、`~/.config/dsh/skills` は許可の外なので入力待ちになります。

未信頼のリポジトリの `.dsh/skills` はタスクでは読まれません。対話とは違い確認も出ず、承認待ちにもなりません。無人実行を止めないためと、人が見ていない入口の既定を厳しくするためです。対話で 1 度 `a` と答えるか `skill trust` で確認しておいてください。

skill ディレクトリ配下のスクリプトは `--allow-command` では許可できません。許可はあなたが読んだコマンド行を指しますが、skill script はエージェント自身が書けるファイルでもあるためです。実行のたびに入力待ちになります。

AI chat hooks はタスク実行中も発火します。`ask` は対話プロンプトではなく入力待ちになり、その承認キー `hook:HOOK_ID:対象` は `--allow-command` や `--allow-mcp` では満たせません。`user-prompt-submit` と `pre-tool-use` の hook は失敗やタイムアウトで拒否側に倒れます。壊れた hook でタスクが止まるときは `AI_CHAT_HOOKS=off` を使ってください。

## 隔離と長時間処理

任意の隔離バックエンドは `@anthropic-ai/sandbox-runtime@0.0.75` の `srt` です。利用者が別途インストールし、PATHから発見できる状態で `--sandbox` を指定してください。Linuxではbubblewrap、socatなどSRTのOS依存も必要です。固定バージョン不一致や起動失敗時に通常実行へ切り替えません。

```sh
agent run --sandbox --tokens 30000 --timeout 600 --write . --allow-command 'python3 validate.py' --check '検証スクリプトが終了コード0' -- 'データを検証して'
```

システムの実行ファイル・ライブラリと明示したフォルダを読めます。Homebrewなど別の場所のツールチェーンには必要な場所だけ `--read` を追加します。`--network example.com` は隔離コマンドの通信先、`--env NAME` は子プロセスへ渡す追加環境変数です。通常はPATH、HOME、言語・一時ディレクトリのみ継承します。隔離なしのコマンド権限はコマンド実行の承認であり、OSによるファイル・通信の制限にはなりません。MCPの通信先・リモート書き込みは別の承認経路です。

長時間の `execute` はジョブIDを返します。モデルは `job_status`、`job_output`、`job_cancel` で追跡します。出力は各ストリーム末尾1MiBまで保持し、欠落・読取未完了を明示します。終了時にマスク済みのログをstateディレクトリの TASK_ID/JOB_ID.json へ保存します。終了コード、取消、タイムアウトを区別します。初期版は同時に1タスクです。常駐デーモンではなく、シェル終了後にジョブへ再接続はできません。

## MCPと外部タスク

タスクでは `tool_search` で必要なツール定義だけを追加します。検索自体は実行許可ではありません。接続はサーバー単位で再利用し、切断後の変更操作を無条件に再送しません。一覧キャッシュは5分または一覧変更通知で更新します。

Tasks対応サーバーが返したハンドルを保存し、`mcp_task_status` / `mcp_task_cancel` で追跡します。追加入力には `agent respond TASK_ID SERVER REMOTE_TASK_ID JSON_INPUT_RESPONSES` を使います。取消要求はサーバー側の停止を保証しません。

状態はXDGのstateディレクトリ配下 `dsh/agent`（取得できなければconfig配下のstate）に置き、ディレクトリ0700・DB0600を要求します。認識できる認証情報は保存前にマスクします。任意の秘密文字列を完全に検出するものではありません。`agent delete` はタスクと保存イベントを削除します。タスクが作ったユーザーの成果物は保持します。

## 代表ワークフローと評価

- 開発: `shell_context` / `shell_history` →対象ファイル→最小の修正→対象テスト→成功イベントで検証。
- データ処理: 入力件数・形式を確認→新しい出力先へ変換→出力件数・内容を再読→検証。
- 調査: `tool_search` →許可された検索・取得→URLと取得日を含む文書→出典と文書の再確認。外部サービス更新は下書きを成果物とし、送信には別の許可を得る。

比較測定は同一モデル・同一入力・初期状態を復元した作業フォルダで各3回以上行い、完遂率、確認回数、累積トークン、所要時間、強制中断後の再開成功率を記録します。現行の `!` と `agent` を比較し、実API測定前に改善率を断定しません。自動検証は `cargo test -p doge-shell --lib agent::tests` と `cargo test -p dsh-builtin --lib agent::`。実隔離試験は `cargo test -p dsh-builtin --lib agent::sandbox::tests::real_sandbox -- --ignored` です。
