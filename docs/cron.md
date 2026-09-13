# 永続 cron ジョブ

`cron` はシェルコマンドと無人 AI エージェントタスクの両方を、壁時計スケジュールで実行する永続的なジョブスケジューラです。旧 `sched`（セッション限り・インターバルのみ）を置き換えました。ジョブ定義は `$XDG_STATE_HOME/dsh/cron/jobs.sqlite3` に永続化され、シェルの再起動をまたいで残ります。

## 実行モデル: 二系統駆動

ジョブは次の2つの経路のどちらか（両方でも構わない）で発火します。

1. **対話セッションが開いている**: 各セッションが自分の cron runner を持ち、次に due になるジョブまでの秒数だけ sleep しては due ジョブを claim します（アイドル時は最大60秒間隔でポーリング）。セッションを開くだけで有効で、追加のインストールは不要です。
2. **外部 tick**: `dsh -c "cron tick"` を crontab / systemd timer / launchd から定期実行します。これがログアウト中もジョブを回す唯一の経路です。`cron setup`（`--crontab` / `--systemd` / `--launchd`、無指定なら OS を自動判定）が貼り付け可能な設定テキストを標準出力に印字します。**何もインストールしません** — 印字するだけです。

二系統が同時に同じジョブを見つけても二重実行しません。claim は SQLite の `BEGIN IMMEDIATE` トランザクション内で行う条件付き `UPDATE`（`claimed_by`/`claimed_until` 列）で、どちらか一方だけが成功します。claim と同時に `next_run_at` を次のスロットへ進めるので、claim に失敗した側は「due な仕事が無い」と判断してスキップします。

**1 run = 1 子プロセス**です。claim した runner（セッション内 runner または `cron tick`）は `dsh -c "cron run-job <UUID>"` という別プロセスを起動するだけで、実行結果を待ちません。`Shell` が `!Send`（`Rc<RefCell<LispEngine>>` を持つ）なので AI ジョブの実行には `&mut Shell` が要り、tokio task から直接は呼べないためです。子プロセスに渡すのは run の UUID だけで、goal・grant・コマンド行はすべて SQLite 経由で渡ります — シェルのパーサにも `sh -c` にも一度も入りません。

## スケジュール構文

`--schedule`（または `cron add` の位置引数）は3種類の書式を受け付けます。

| 形式 | 例 | 意味 |
|---|---|---|
| interval | `30s` / `5m` / `1h` | 前回実行からの相対時間。5秒〜24時間。 |
| cron 式 | `0 9 * * mon-fri` | 5フィールド（分 時 日 月 曜）、ローカル壁時計。 |
| macro | `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly` | 固定 cron 式の省略形。 |

cron 式は**必ずクォートする**こと。`cron add */5 * * * * git fetch` はシェルが `*/5` 以外のフィールドをファイル名として glob 展開してしまいます。`cron add` はこの典型的な間違いを検出し、直し方を含むエラーを返します。

day-of-month と day-of-week の両方を制限した場合は **OR** になります（Vixie cron の仕様）。`0 0 1 * mon` は「1日、かつ毎週月曜日」であって「1日かつ月曜日」ではありません。

DST（夏時間）境界をまたぐ場合: 存在しない時刻（春の繰り上げで飛ばされる時間帯）は繰り上げ後の最初の瞬間に、二重に存在する時刻（秋の繰り下げの重複）は早い方の瞬間に解決されます。

長時間ログアウトしていた・マシンが休止していたなどで発火を取りこぼした場合、**取りこぼした回数ぶん連続実行しません**。`--catchup`（既定1時間）より古い分は1回にまとめて次のスロットへ進みます。

`0 0 30 2 *` のように永久に一致しない式は、10年先まで探索して見つからなければ `None` を返し無限ループになりません。

## ジョブの永続化とストア

- 置き場所: `$XDG_STATE_HOME/dsh/cron/`（`agent_state_dir()` の**兄弟**であり子ではない — `SafetyGuard::task_file_allowed` が `agent_state_dir()` 配下を無条件拒否するため、notepad をそちらには置けない）。
- SQLite（WAL + `synchronous=NORMAL`）。`jobs` / `runs` / `incidents` の3テーブル。`runs` と `history` はテーブルを分けない — 1 fire = 1 attempt なので状態列（`queued`/`running`/`succeeded`/`failed`/`skipped`/`needs-approval`/`cancelled`）で区別すれば足ります。
- マイグレーションは `PRAGMA user_version` + 番号付きステップ。**store が既知の最大バージョンより新しければ開くのを拒否**します（古い `dsh` バイナリが新しいスキーマを壊さないため — 外部 tick は dsh のアップグレードより長生きしうる）。
- `runs` はジョブあたり200行・30日を超えた分を定期的に刈り込みます。

## `cron tick` の終了コードと出力

**ジョブの失敗は tick 自体の失敗ではありません。** `cron tick` はジョブが失敗しても既定で **無出力・exit 0** を返します。そうしないと system cron が失敗のたびにメールを送り、運用者は結局 tick を無効化してしまいます。非0を返すのは tick 自体が壊れたとき（store が開けない・スキーマが新しすぎる）だけです。`--json` / `--verbose` / `--dry-run` / `--max N` があります。

## AI ジョブ (`--agent`)

`--agent` を付けたジョブは、無人実行される `agent run` そのものです。同じ入口・同じ権限モデルを使います（`docs/agent.md` 参照）。

```sh
cron add --agent --name digest --tokens 50000 --timeout 10m \
  --read . --write out --check "out/digest.md に今日の日付がある" \
  '0 9 * * mon-fri' -- '未対応のPRと直近のコミットをまとめてout/digest.mdに書く'
```

- `--tokens` は **run ごと**の上限です（`agent resume` の累積予算とは違い、毎回新しいタスクとして開始するため）。
- `--timeout` はモデル自身の協調的な時間予算（`AgentTask.time_budget_ms`）と、外側からの強制終了デッドラインの両方を兼ねます。モデルが自分で止まらない場合、`cron run-job` プロセス内のウォッチドッグがこの run の claim リース（`timeout` の2倍、最低60秒）が切れる少し前に自プロセスグループを `SIGKILL` します。run 行はその場では更新されません（プロセスごと落ちるため）が、次のスキャンの `reap_expired_leases` が `failed`(timeout) として拾います。
- `--max-tokens-per-day`（任意）は直近24時間の累積トークン使用量に対する上限です。5分おきの実行では per-run 予算だけでは実質無制限になるため。
- grant（`--read`/`--write`/`--allow-command`/`--allow-mcp`/`--network`/`--env`/`--sandbox`）は `agent run` と完全に同じ検証を通ります（`dsh-builtin/src/agent/grant.rs` を共有）。

### 承認が必要になったとき

無人実行なので**確認は出ません**。付与されていない権限が必要になった run は `needs-approval` 状態で終わり、incident が1件記録されます。

```sh
cron incidents                       # 未確認の一覧（job / kind / task id）
agent show <TASK_ID>                 # 何を求められたか（マスク済み）
cron edit digest --allow-command '...'   # 恒久的に許可
cron incidents ack <ID>              # incident を閉じ、ジョブの blocked を解除
```

`--allow-mcp` の値は `agent show` に出る承認キーそのまま（完全一致）です。ゼロから正しいキーを書けると仮定せず、一度実行させて incident から写すのが正しい導線です。

`hook:` で始まる承認キー（AI chat hooks の `ask`）は `--allow-*` では満たせません。incident の種別が別（`hook-ask`）に分かれます。ジョブ単位の回避策はありません — `--env NAME` は**名前だけ**を許可するもので値は持たないため、`--env AI_CHAT_HOOKS=off` は何も許可しません。直すには hook 定義自体（`ai-hooks.json`）を変えるか、`config.lisp` かジョブを実行する環境で `AI_CHAT_HOOKS=off` を設定してください（この場合ジョブ単位ではなく全体で hooks が止まります）。

`--allow-mcp` を持たないジョブは MCP サーバーに接続しません。`dsh -c` は対話サービスを起動しないため（`needs_interactive_services()` が false）、MCP grant を持つジョブだけが `cron run-job` 内で明示的に MCP 接続を張ります。

### notepad — ジョブ固有の記憶

毎 run が新しいタスク（新しい会話）として始まるため、notepad が唯一の連続性です。`cron_state_dir()/notepad/<job>.md` の実ファイルで、run 開始時に goal の前に「これは前回のメモであり指示や承認ではない」という区切り付きで挿入されます。

notepad のディレクトリはジョブの grant（`read_roots`/`write_roots`）に自動追加されるため、モデルは既存の `edit`/`str_replace` ツールでそのまま読み書きできます。**新しいツールは増やしていません。**

```sh
cron notepad digest            # 表示
cron notepad digest --clear    # 消去
```

### エージェント自身によるジョブ管理

`!` チャットと `agent run` は `cron_manage` という chat tool を持ち、上記の `cron` サブコマンドを人が打つのと同じことをツール呼び出しで行えます。`execute` 経由の `cron ...` は届きません（`cron` は builtin であって shell コマンドではないため）。

- `create` は**常に paused で登録**されます。人が `cron run --now` で確認してから `cron resume` するまで発火しません。
- grant（`--read`/`--write`/`--allow-command`/`--allow-mcp`/`--network`/`--env`/`--sandbox`）は**呼び出し中のタスク自身の grant を超えられません**。超えるリクエストは確認を挟まず即座に拒否されます。
- `create` 以外の書き込み系（`update`/`pause`/`resume`/`remove`/`run`/`ack`）は毎回人に確認します。無人タスク中はこれが `TaskStatus::InputRequired` になり、`cron incidents` ではなくタスク自身が保留になります。
- notepad 用の action はありません — notepad ディレクトリは既にジョブの grant に入っているため、既存の `read_file`/`edit` で足ります。

## 失敗モードと振る舞い

| 状況 | 振る舞い |
|---|---|
| API キー未設定 | `agent` に入る前に検出。`failed`(config) + incident。ack まで再試行しない |
| ネットワーク断など一時的な失敗 | `failed`(transient)。**連続3回**で初めて incident に昇格 |
| 別の agent タスクが実行中（flock 競合） | `skipped`(agent-busy)。連続3回で incident（LockStarvation）に昇格 |
| 同じジョブの前回 run がまだ実行中 | claim 段階で除外される（run 行は作られない）。history に何も残らない |
| 外部 tick とセッション runner が同時に claim | 片方だけが成功。SQLite の条件付き UPDATE が保証 |
| 外側 timeout で強制終了 | `failed`(timeout)。次に誰かが agent を触ったとき `recover_interrupted()` が `Interrupted` に落とし、cron は `reconcile` incident として拾う |
| ジョブのルートディレクトリが消えた・別物になった | `failed`(root-changed) + incident。自動では作り直さない |

`cron doctor`（`--json` あり）は上の表に載らない、事前に気づける不整合を報告する: 一致し得ないスケジュール、消えた `cwd`、API キー未設定、`--allow-mcp` を持つのに MCP サーバー未設定、`config.lisp` の `sched-add` 残存、一度も run が完了していない、に加えて **`--on` が `never` 以外のジョブが1件でもあれば「通知はまだ配線されていない」旨の note を1行**、**現在 claim を握っている（`running`）ジョブがあればその経過時間を warn** で出す。

## Lisp からの登録

`config.lisp` は毎起動評価されるため、`(cron-add "<name>" "<schedule>" "<command>" ["<notify>"])` は**同名なら upsert**（置き換え）します。エラーにも重複作成にもなりません。

```lisp
(cron-add "fetch" "5m" "git fetch --all")
(cron-add "prs" "10m" "gh pr list" "change")
```

対照的に **CLI の `cron add` は同名だとエラー**になり `--force` が必要です — 人がプロンプトで打つ場合はタイプミスの可能性の方が高いためです。

`(sched-add ...)` / `(sched-remove ...)` / `(sched-pause ...)` / `(sched-resume ...)` / `(sched-list)` は1リリース限定の非推奨エイリアスとして残っており、対応する `cron-*` へそのまま委譲します（`config.lisp` は最初のエラーで評価が打ち切られるため、いずれか1つでもいきなり未定義にすると alias・abbr・PATH 設定がまとめて消える事故になります）。

## 関連ドキュメント

- CLI の使用例は README の「Cron Jobs」節。
- AI エージェント自身が cron ジョブを追加・編集・デバッグするための手順は runtime skill `docs/ai/skills/dsh-cron/`（`scripts/install-runtime-skills.sh --target dsh --profile dsh-user` で導入）。
- 無人タスクの権限モデル・予算・SRT サンドボックスの詳細は `docs/agent.md`。
