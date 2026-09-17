# 再開できるエージェントタスク

`agent` は既存の `!` の実行ループを使い、依頼・計画・検証・使用量を SQLite に保存する明示的な入口です。APIキーの設定だけでは開始しません。OpenAI互換の Chat Completions、ツール呼び出し、応答の `usage` が必要です。

## 開始と再開

```sh
agent run --tokens 50000 --timeout 900 --write . --allow-command 'cargo test -p dsh-types' --check '対象テストが成功する' -- 'テスト失敗を調査し修正して'
agent list
agent show TASK_ID              # 会話全体・全イベントの生 JSON（--allow-mcp の承認キーはここでしか取れない）
agent show TASK_ID --summary    # goal・criteria の合否・最終応答・ツール集計の短い要約
agent resume TASK_ID --tokens 80000 --timeout 1800
agent cancel TASK_ID
agent delete TASK_ID
```

時間は秒、トークンと時間の指定値は再開分を含む累積上限です。CLI指定がなければシェル変数、プロセス環境の順で `AI_AGENT_TOKEN_BUDGET` と `AI_AGENT_TIMEOUT_SECS` を解決し、どちらも未設定なら既定値（50000トークン／900秒）を使います。いずれも正の残量が必要です。トークン上限は次の要求を止める条件で、請求額の厳密な上限ではありません。使用量を返さないプロバイダではタスクを停止します。

初期の読み取り範囲は開始時の cwd です。`--read DIR`、`--write DIR` は既存ディレクトリを指定し、繰り返せます。コマンドは `--allow-command` の完全一致、MCP操作は `agent show` に表示された承認キーを `--allow-mcp` へ渡して許可します。範囲外の要求はその場で止まらず、ツール結果のエラーとして返ってタスクは回避策を探して続行します（誰も見ていない実行で承認待ちが出ても意味がないため）。どうしても進めなくなったときだけ `Interrupted` で止まり、`stop_reason` に最後の拒否内容が残るので、`agent resume` に追加の権限を明示して再開できます。外部送信・公開も対象と引数を含む別の許可です。

ツール実行前の意図と結果を別々に保存します。プロセス終了後、結果が残っていない操作を自動的に再実行しません。ファイルや外部サービスの状態を確認したうえで `agent resume TASK_ID --reconcile '実際に確認した結果'` を実行します。完了条件は開始後に差し替えられず、成功したツール結果を根拠に各条件を検証します。検証はモデルの判断を含むため、重要な条件は `--check` で具体的に指定してください。

タスク中の `skill_manage` は `edit` などと同じ変更操作として扱います。`task_plan` の記録前には実行できず、実行後は完了条件の検証済み判定をやり直します。`<project>/.dogesh/skills` への書き込みはそのリポジトリの `--write` に従い、`~/.config/dogesh/skills` は許可の外です。`<project>/.agents/skills` は読み取り専用で、`scope` に `project` を指定した書き込みも `.dogesh/skills` へ向きます。

`AI_CHAT_SKILL_STAGING`（既定 `task`）が `off` でない限り、`--write` グラントが無い対象への書き込みは拒否されても止まらず、`skill pending` に積まれてタスクは続行します。人が `skill approve`/`skill reject` で後から判断します。グラントがある対象は今まで通り直接書きます。`delete` は積まれず、他の拒否と同じくツール結果のエラーとして返ります（同じ拒否を3回繰り返すと人の再開待ちになります）。

未信頼のリポジトリの `.dogesh/skills` と `.agents/skills` はタスクでは読まれません。対話とは違い確認も出ず、承認待ちにもなりません。無人実行を止めないためと、人が見ていない入口の既定を厳しくするためです。信頼はディレクトリごとに記録されるので、対話で各ディレクトリについて 1 度 `a` と答えるか `skill trust` で確認しておいてください。

skill ディレクトリ配下のスクリプトは `--allow-command` では許可できません。許可はあなたが読んだコマンド行を指しますが、skill script はエージェント自身が書けるファイルでもあるためです。`.agents/skills` 配下も同じで、実行しようとするたび拒否のエラーとして返ります（タスク自体は続行します）。

AI chat hooks はタスク実行中も発火します。`ask` は対話プロンプトではなく拒否のエラーとして返り、その承認キー `hook:HOOK_ID:対象` は `--allow-command` や `--allow-mcp` では満たせません（タスクは回避策を探して続行します）。`user-prompt-submit` と `pre-tool-use` の hook は失敗やタイムアウトで拒否側に倒れます。`AI_CHAT_HOOK_TURN_BUDGET_MS` もタスク中に効き、予算を使い切ったターンでは gate の hook が残り時間まで縮められて実行されるので、遅い hook はタイムアウト = 拒否側に倒れます。壊れた hook や足りない予算でタスクが止まるときは、予算を上げるか `AI_CHAT_HOOKS=off` を使ってください。

## バックグラウンド実行（`--detach`／`-d`）

`agent run`/`agent resume` に `--detach`（`-d`）を足すと、タスクを別プロセス（`dogesh -c "agent run-detached <id>"`）に渡してすぐプロンプトへ戻ります。権限モデル・予算の扱いは前景実行と何も変わりません。無人実行なので承認プロンプトは出ません。代わりに、権限不足の操作はその場で止まらずツール結果のエラーとして返り、タスクは回避策を探して続行します。同じ拒否操作を3回繰り返す・結果不明の操作が残る・予算切れの場合だけ人が再開する状態（`input-required` または権限ヒント付きの `interrupted`）で止まります。`--tokens`/`--timeout` の省略時は既定値が使われます。

```sh
agent run --tokens 50000 --timeout 900 --write . --detach -- 'テスト失敗を調査し修正して'
#  Task 1a2b3c4d-... detached (pid 12345); log at ~/.local/state/dogesh/agent/1a2b3c4d-.../run.log

agent list [--all] [--json]      # * が付いた行は今まさに実行中のタスク。--all は 24 時間より前に終わった completed/cancelled も表示
agent logs TASK_ID [--follow] [--json]  # 記録済みイベントを1行1件で表示。--follow はタスクが停止するか Ctrl-C まで追従（無期限には待たない）
agent wait TASK_ID [--timeout N]  # 終了（または input-required）まで待ってから summary を表示
agent doctor [--json]           # 死んだプロセスに取り残されたタスク・承認待ちの放置・孤立ファイルを診断
```

セッションが開いている間は、detach したタスクが完了・失敗・承認待ち・権限不足の中断になると `[agent 1a2b3c4d]  ...` の 1 行がプロンプトの上に表示され、`(pref-status-line t)` なら `🤖` セグメントにも反映されます。この通知はメモリ内のみで、**シェルを開いていない間に終わったタスクは通知されません**（`agent list`/`agent logs` で後から確認してください）。`DOGESH_AGENT_WATCH=0` で無効化、`DOGESH_AGENT_WATCH_INTERVAL_SECS`（既定 2 秒、1〜60 に clamp。実行中のタスクが無ければ自動で 60 秒に伸びる）で頻度を調整できます。デスクトップ通知は既存の `(pref-auto-notify t)` に乗ります。

同時に実行できるタスク数は `AI_AGENT_MAX_CONCURRENT`（既定 1、今までと同じ挙動）で決まり、detach か前景かを問いません。上限に達しているときの `--detach` はその場でエラーになり、子プロセスは起動されません。

`input-required` で止まったタスクと、権限ヒント付きの `interrupted` で止まったタスクは、`agent show --summary` の `needs:` 行、`agent doctor`、または通知の本文に「次に打つ 1 行」が出ます（`agent list`/`agent logs` 自体には出ません）。`agent resume TASK_ID --reconcile '...'` / `--allow-command '...'` / `--allow-mcp '...'` のどれが必要かはここに書かれた通りに打てば済みますが、hook の `ask` や機微パスの読み取り、`skill_manage delete` は `--allow-*` では満たせないため、人が直接操作した結果を `agent resume TASK_ID --reconcile '...'` で伝えて再開してください。

```sh
agent approve TASK_ID          # 最後の拒否を1件だけ確認して許可し、そのまま前景で再開（--dry-run で下見可）
```

`approve` は `needs:` 行と同じ判定で最後の拒否を読み取り、追加する grant を表示して y/N で聞きます。承認すると grant を足してそのまま再開します。一度に1件だけ、前景再開のみです。残りは次回停止時に再度 `approve` してください。付与できない拒否（skill script 実行、hook の `ask`、機微パス、`skill_manage delete`）の場合は理由と手動手順を表示して何も変えません。

`!` チャットから「あとでやっといて」と頼みたいときは、この `--detach` そのものではなく `cron_manage`（`action: "create"`）でジョブを作らせ、`cron run --now` で起動してください。タスクがタスクを無制限に生む経路を作らないため、`!` から直接 detach できる chat tool は意図的に用意していません（将来 `agent_delegate` のようなツールを足す場合は、grant が呼び出し元タスクの grant を超えられないこと、`agent_runtime` の内側からは呼べないこと、対話では通常の確認を通すこと、親の残予算から子の予算を差し引くことが前提になります）。

## 隔離と長時間処理

任意の隔離バックエンドは `@anthropic-ai/sandbox-runtime@0.0.75` の `srt` です。利用者が別途インストールし、PATHから発見できる状態で `--sandbox` を指定してください。Linuxではbubblewrap、socatなどSRTのOS依存も必要です。固定バージョン不一致や起動失敗時に通常実行へ切り替えません。

```sh
agent run --sandbox --tokens 30000 --timeout 600 --write . --allow-command 'python3 validate.py' --check '検証スクリプトが終了コード0' -- 'データを検証して'
```

システムの実行ファイル・ライブラリと明示したフォルダを読めます。Homebrewなど別の場所のツールチェーンには必要な場所だけ `--read` を追加します。`--network example.com` は隔離コマンドの通信先、`--env NAME` は子プロセスへ渡す追加環境変数です。通常はPATH、HOME、言語・一時ディレクトリのみ継承します。隔離なしのコマンド権限はコマンド実行の承認であり、OSによるファイル・通信の制限にはなりません。MCPの通信先・リモート書き込みは別の承認経路です。

長時間の `execute` はジョブIDを返します。モデルは `job_status`、`job_output`、`job_cancel` で追跡します。同じ仕組みは `!` チャットにも入っていますが、そちらはSQLiteに保存せずプロセス内に持ち、会話が続く間だけ残ります（`chat_status` で一覧、`chat_reset` で停止）。出力は各ストリーム末尾1MiBまで保持し、欠落・読取未完了を明示します。終了時にマスク済みのログをstateディレクトリの TASK_ID/JOB_ID.json へ保存します。終了コード、取消、タイムアウトを区別します。同時に実行できるタスク数は `AI_AGENT_MAX_CONCURRENT`（既定 1）で決まります。常駐デーモンではなく、シェル終了後にジョブへ再接続はできません（`--detach` したタスク自体はシェルを閉じても走り続けます。「バックグラウンド実行」の節を参照）。

## MCPと外部タスク

タスクでは `tool_search` で必要なツール定義だけを追加します。検索自体は実行許可ではありません。接続はサーバー単位で再利用し、切断後の変更操作を無条件に再送しません。一覧キャッシュは5分または一覧変更通知で更新します。

Tasks対応サーバーが返したハンドルを保存し、`mcp_task_status` / `mcp_task_cancel` で追跡します。追加入力には `agent respond TASK_ID SERVER REMOTE_TASK_ID JSON_INPUT_RESPONSES` を使います。取消要求はサーバー側の停止を保証しません。

状態はXDGのstateディレクトリ配下 `dogesh/agent`（取得できなければconfig配下のstate）に置き、ディレクトリ0700・DB0600を要求します。認識できる認証情報は保存前にマスクします。任意の秘密文字列を完全に検出するものではありません。`agent delete` はタスクと保存イベントを削除します。タスクが作ったユーザーの成果物は保持します。

## 代表ワークフローと評価

- 開発: `shell_context` / `shell_history` →対象ファイル→最小の修正→対象テスト→成功イベントで検証。
- データ処理: 入力件数・形式を確認→新しい出力先へ変換→出力件数・内容を再読→検証。
- 調査: `tool_search` →許可された検索・取得→URLと取得日を含む文書→出典と文書の再確認。外部サービス更新は下書きを成果物とし、送信には別の許可を得る。

## cron からの無人実行

`cron add --agent` は本ドキュメントと同じ `agent run` 入口を、スケジュールから起動するものです。権限モデル・予算・隔離は一切変わりません。無人実行のため承認プロンプトは出ず、権限不足の操作はツール結果のエラーとして返ってタスクは回避策を探して続行します。どうしても進めなくなった run は権限ヒント付きの `interrupted` で終わり、`needs-approval` の incident として記録され、次回以降も自動では再試行されません。`--check` の完了条件は、無人で判定されることを踏まえてより具体的に書く必要があります。run が何をしたかは `cron logs <job>` で読めます（AI ジョブの `stdout` にはこの要約が入り、`agent show --summary` と同じ内容です）。全イベントを見たいときだけ、cron 側の run 履歴からこのタスクの id を得て `agent show <id>` に渡してください。grant の一覧やタスクの状態機械はここに複製せず、詳細は `docs/cron.md` を参照してください。

逆方向 — タスク自身が cron ジョブを作る／変える — には `cron_manage` という chat tool があります（`execute` 経由の `cron ...` は builtin には届きません）。作成したジョブは常に paused で登録され、渡せる grant は**呼び出し中のタスク自身の grant を超えられません**。それ以外の変更（`update`/`pause`/`resume`/`remove`/`run`/`ack`）は他の書き込み系ツールと同じく毎回拒否のエラーとして返り、タスクは続行します。詳細は `docs/cron.md` の「エージェント自身によるジョブ管理」を参照してください。

比較測定は同一モデル・同一入力・初期状態を復元した作業フォルダで各3回以上行い、完遂率、確認回数、累積トークン、所要時間、強制中断後の再開成功率を記録します。現行の `!` と `agent` を比較し、実API測定前に改善率を断定しません。自動検証は `cargo test -p doge-shell --lib agent::` と `cargo test -p dsh-builtin --lib agent::`。実隔離試験は `cargo test -p dsh-builtin --lib agent::sandbox::tests::real_sandbox -- --ignored` です。
