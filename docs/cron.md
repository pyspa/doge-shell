# 永続 cron ジョブ

`cron` はシェルコマンドを、壁時計スケジュールで実行する永続的なジョブスケジューラです。旧 `sched`（セッション限り・インターバルのみ）を置き換えました。ジョブ定義は `$XDG_STATE_HOME/dogesh/cron/jobs.sqlite3` に永続化され、シェルの再起動をまたいで残ります。

## 実行モデル: 二系統駆動

ジョブは次の2つの経路のどちらか（両方でも構わない）で発火します。

1. **対話セッションが開いている**: 各セッションが自分の cron runner を持ち、次に due になるジョブまでの秒数だけ sleep しては due ジョブを claim します（アイドル時は最大60秒間隔でポーリング）。セッションを開くだけで有効で、追加のインストールは不要です。
2. **外部 tick**: `dogesh -c "cron tick"` を crontab / systemd timer / launchd から定期実行します。これがログアウト中もジョブを回す唯一の経路です。`cron setup`（`--crontab` / `--systemd` / `--launchd`、無指定なら OS を自動判定）が貼り付け可能な設定テキストを標準出力に印字します。**何もインストールしません** — 印字するだけです。

二系統が同時に同じジョブを見つけても二重実行しません。claim は SQLite の `BEGIN IMMEDIATE` トランザクション内で行う条件付き `UPDATE`（`claimed_by`/`claimed_until` 列）で、どちらか一方だけが成功します。claim と同時に `next_run_at` を次のスロットへ進めるので、claim に失敗した側は「due な仕事が無い」と判断してスキップします。

**1 run = 1 子プロセス**です。claim した runner（セッション内 runner または `cron tick`）は `dogesh -c "cron run-job <UUID>"` という別プロセスを起動するだけで、実行結果を待ちません。子プロセスに渡すのは run の UUID だけで、コマンド行は SQLite 経由で渡ります — 余計なシェルのパーサには入りません。

## スケジュール構文

`--schedule`（または `cron add` の位置引数）は3種類の書式を受け付けます。

| 形式 | 例 | 意味 |
|---|---|---|
| interval | `30s` / `5m` / `1h` | 前回実行からの相対時間。5秒〜24時間。 |
| cron 式 | `0 9 * * mon-fri` | 5フィールド（分 時 日 月 曜）、ローカル壁時計。 |
| macro | `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly` | 固定 cron 式の省略形。 |
| `@reboot` | `@reboot` | 対話セッションの runner 開始ごとに一度だけ発火。外部 tick からは発火しない。 |
| `@manual` | `@manual` | 明示的な `cron run` / `cron run --now` でのみ発火。スケジュール単独では発火しない。 |

cron 式は**必ずクォートする**こと。`cron add */5 * * * * git fetch` はシェルが `*/5` 以外のフィールドをファイル名として glob 展開してしまいます。`cron add` はこの典型的な間違いを検出し、直し方を含むエラーを返します。

day-of-month と day-of-week の両方を制限した場合は **OR** になります（Vixie cron の仕様）。`0 0 1 * mon` は「1日、かつ毎週月曜日」であって「1日かつ月曜日」ではありません。

DST（夏時間）境界をまたぐ場合: 存在しない時刻（春の繰り上げで飛ばされる時間帯）は繰り上げ後の最初の瞬間に、二重に存在する時刻（秋の繰り下げの重複）は早い方の瞬間に解決されます。

長時間ログアウトしていた・マシンが休止していたなどで発火を取りこぼした場合、**取りこぼした回数ぶん連続実行しません**。`--catchup`（既定1時間）より古い分は1回にまとめて次のスロットへ進みます。

`0 0 30 2 *` のように永久に一致しない式は、10年先まで探索して見つからなければ `None` を返し無限ループになりません。

## ジョブの永続化とストア

- 置き場所: `$XDG_STATE_HOME/dogesh/cron/`（`agent_state_dir()` の**兄弟**であり子ではない — `SafetyGuard::task_file_allowed` が `agent_state_dir()` 配下を無条件拒否するため、notepad をそちらには置けない）。
- SQLite（WAL + `synchronous=NORMAL`）。`jobs` / `runs` / `incidents` の3テーブル。`runs` と `history` はテーブルを分けない — 1 fire = 1 attempt なので状態列（`queued`/`running`/`succeeded`/`failed`/`skipped`/`needs-approval`/`cancelled`）で区別すれば足ります。
- マイグレーションは `PRAGMA user_version` + 番号付きステップ。**store が既知の最大バージョンより新しければ開くのを拒否**します（古い `dogesh` バイナリが新しいスキーマを壊さないため — 外部 tick は dogesh のアップグレードより長生きしうる）。
- `runs` はジョブあたり200行・30日を超えた分を定期的に刈り込みます。

## `cron tick` の終了コードと出力

**ジョブの失敗は tick 自体の失敗ではありません。** `cron tick` はジョブが失敗しても既定で **無出力・exit 0** を返します。そうしないと system cron が失敗のたびにメールを送り、運用者は結局 tick を無効化してしまいます。非0を返すのは tick 自体が壊れたとき（store が開けない・スキーマが新しすぎる）だけです。`--json` / `--verbose` / `--dry-run` / `--max N` があります。

## ジョブの種類

`cron` が実行するのはシェルコマンドのみです。`--agent` による AI ジョブは廃止されました。旧バージョンで作られた AI ジョブがストアに残っていても、新しい run は `failed`(config) として記録されるだけで実行されません。シェルコマンドとして作り直すか、不要なら `cron rm <name>` で削除してください（残すと incident が繰り返し起票されます）。

### incident が起きたとき

```sh
cron incidents                       # 未確認の一覧
cron incidents ack <ID>              # incident を閉じる。他に blocking な incident がなければジョブの blocked を解除
```

### notepad — ジョブ固有のメモ

`cron_state_dir()/notepad/<slug>.md` の実ファイルです（`<slug>` はジョブ名をファイル名安全にしたもの）。ジョブに関するメモを残すための場所で、実行には影響しません。

```sh
cron notepad digest            # 表示
cron notepad digest --clear    # 消去
```

### チャットからのジョブ管理

`!` チャットは `cron_manage` という chat tool を持ち、上記の `cron` サブコマンドを人が打つのと同じことをツール呼び出しで行えます。`execute` 経由の `cron ...` は届きません（`cron` は builtin であって shell コマンドではないため）。

- `create` は**常に paused で登録**されます。人が `cron run --now` で確認してから `cron resume` するまで発火しません。
- `create` 以外の書き込み系（`update`/`pause`/`resume`/`remove`/`run`/`ack`）は毎回人に確認します。
- notepad 用の action はありません。


## 実行結果の確認

`runs` テーブルには `stdout`/`stderr` 列（マスク済み、各 8KiB にクランプ）が最初からありましたが、それを読み出す口は `cron logs` を追加するまで存在しませんでした。

```sh
cron logs digest                # 最新の finished run の記録済み stdout/stderr を表示（各 8KiB clamp 済みの記録であり、実行時の全出力ではない）
cron logs digest --run <id>     # 特定の run（history の run 列、一意な prefix でも可）
cron logs digest --stdout       # stdout だけ（パイプ向け、見出し無し）
cron logs digest --json         # {"run": {...}, "stdout": "...", "stderr": "..."}
```

- `cron history` の `run` 列（先頭8文字）を `--run` にそのまま渡せます。
- 失敗した run では `reason` と `preview` を併記します。
- `cron show <job>` は最新 run（state / duration / preview）も表示します。

## 失敗モードと振る舞い

| 状況 | 振る舞い |
|---|---|
| コマンドの起動失敗（`sh` が spawn できない） | `failed`（exit 127、stderr に理由）。単発では incident にならない |
| 外側 timeout で強制終了 | `failed`(timeout) |
| 失敗が連続3回 | `failing` incident に昇格。成功で自動解消 |
| 同じジョブの前回 run がまだ実行中 | claim 段階で除外される（run 行は作られない）。history に何も残らない |
| 外部 tick とセッション runner が同時に claim | 片方だけが成功。SQLite の条件付き UPDATE が保証 |
| ジョブの `cwd` が消えた | run 時の `root-changed`/`transient` 判定は shell job では記録されない。`cron doctor` が `cwd-missing` として warn で報告する |

旧バージョンの AI ジョブ由来の行にだけ残っている `config`/`root-changed`/`transient` などの reason は、新しい shell job の実行では記録されません。

`cron doctor`（`--json` あり）は上の表に載らない、事前に気づける不整合を報告する: 一致し得ないスケジュール、消えた `cwd`、`config.lisp` の `sched-add` 残存、一度も run が完了していない、に加えて **`--on` が `never` 以外のジョブが1件でもあれば「通知はまだ配線されていない」旨の note を1行**、**現在 claim を握っている（`running`）ジョブがあればその経過時間を warn** で出す。

## Lisp からの登録

`config.lisp` は毎起動評価されるため、`(cron-add "<name>" "<schedule>" "<command>" ["<notify>"])` は**同名なら upsert**（置き換え）します。エラーにも重複作成にもなりません。

```lisp
(cron-add "fetch" "5m" "git fetch --all")
(cron-add "prs" "10m" "gh pr list" "change")
```

対照的に **CLI の `cron add` は同名だとエラー**になり `--force` が必要です — 人がプロンプトで打つ場合はタイプミスの可能性の方が高いためです。

`cwd`/`env` は**最初の登録時のスナップショットのまま**で、以降の upsert では更新されません。`cron-add` に `--cwd`/`--env` に相当する引数は無く、常に「このプロセスの現在の cwd/env」を使う設計だからです — `config.lisp` は `dogesh -c "cron tick"` や `dogesh -c "cron run-job <uuid>"` の中でも評価されるため、上書きを許すと外部 tick（多くの場合 crontab のごく限られた環境）が走るたびにジョブの実行環境が意図せず入れ替わってしまいます。schedule/command/notify など他のフィールドは通常どおり毎回上書きされます。cwd/env を変えたい場合は `cron edit --cwd`（env は手段が無いため `cron rm` して作り直す）を使ってください。

`(sched-add ...)` / `(sched-remove ...)` / `(sched-pause ...)` / `(sched-resume ...)` / `(sched-list)` は1リリース限定の非推奨エイリアスとして残っており、対応する `cron-*` へ委譲します（`config.lisp` は最初のエラーで評価が打ち切られるため、いずれか1つでもいきなり未定義にすると alias・abbr・PATH 設定がまとめて消える事故になります）。委譲の前に stderr が tty のときだけ非推奨警告を出します（`cron tick`/`cron run-job` のような無人実行では出しません — `cron tick` は既定で無出力・exit 0 が契約なので、この警告が外部 tick のたびに system cron のメールを起こしては本末転倒です）。

## 関連ドキュメント

- CLI の使用例は README の「Cron Jobs」節。
- チャットから cron ジョブを追加・編集・デバッグするための手順は runtime skill `docs/ai/skills/dsh-cron/`（`scripts/install-runtime-skills.sh --target dogesh --profile dogesh-user` で導入）。
