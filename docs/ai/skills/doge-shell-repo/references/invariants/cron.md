# Invariants: cron

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。旧 `sched`（`dsh/src/scheduler/`）は削除済み — `cron`（`dsh/src/cron/`）に統合された。

- `Shell` は `!Send`（`Rc<RefCell<LispEngine>>`）。spawn したタスクから `eval_str` は呼べない。AI ジョブは `dogesh -c "cron run-job <uuid>"` という別プロセスで実行する（`dsh/src/cron/run_job.rs`）。子プロセスに渡すのは UUID だけで、goal や grant は SQLite 経由で渡す。
- セッション内 runner（`dsh/src/cron/runner.rs`）はセッション内の due スキャンだけを担当し、実際の claim・実行は `dsh/src/cron/tick.rs` の `run_once` に一本化されている。外部 tick（`dogesh -c "cron tick"`）も同じ関数を呼ぶ。
- 二重実行の防止は SQLite の `BEGIN IMMEDIATE` トランザクション + 条件付き `UPDATE`（`claimed_by`/`claimed_until`）で行う。in-process の `RwLock`/`AtomicBool` には依存しない — 複数セッション・外部 tick が同時に動く前提だから。
- claim は `next_run_at` を同じトランザクション内で前進させる。skip したときも必ず動かす。さもないと永久に due のままスピンする。
- pause から resume するときは `next_run_at` を貼り直す（`store.set_paused`/`set_all_paused`）。しないと溜まった分が一斉に発火する。
- `cron tick` はジョブの失敗を tick 自体の失敗にしない。既定では無出力・exit 0 を返す。ストアが開けない・スキーマが新しすぎるなど tick 自体が壊れたときだけ非 0。system cron がメール地獄にならないための設計。
- `(cron-add ...)` は name で upsert する。CLI の `cron add` は同名でエラー（`--force` で上書き）。`config.lisp` は毎起動評価されるため、upsert でないと 2 回目の起動で `already exists` エラーになり、そのエラーで `config.lisp` の残り（alias・abbr・PATH 設定）が丸ごと止まる。
- status_line が読む `Environment.cron_health`（`CronHealth` のキャッシュ）は cron runner がスキャンのたびに更新する。`status_line::compose()` 自体は I/O をしない。
- `!` チャットからの cron 操作は chat tool `cron_manage`（`dsh-builtin/src/chatgpt/tool/cron.rs` + `dsh/src/cron/tool.rs`）一本。`execute` 経由の `dogesh -c "cron ..."` を許可リストに足して回避させない — builtin は `sh -c` の外なのでどのみち届かない。`cron_manage` の `create` は常に `--paused` を強制する。cron の argv 検証（`cli::parse_add`/`parse_edit`）を二重実装しない。
- `runs.stdout`/`stderr`（8KiB clamp、`cron logs` が読む）は `SELECT_RUN`（`store/rows.rs`）に**足さない**。この定数と `run_from_row` の `row.get(n)` は列順で結合しており、足すとコンパイルは通るが黙って別のフィールドを返す。本文は `run_output`（2 クエリ方式）が別途読む。
- `store::trigger()`（`cron run <job>` の非 `--now` 経路、`cron_manage(action=run)` も同じ）は paused/blocked を拒否する。`claim_due_jobs` が `enabled=1 AND blocked=0` を要求するため、拒否せず `next_run_at` だけ書くと一生発火しない予約が残り、「paused なら `next_run_at` は NULL」という他の書き込み経路（`set_paused`/`patch`）の不変条件も破る。`claim_one`（`cron run --now`）だけは両方を無視してよい — claim を直接取るのであってスキャン待ちではないため。
- `insert_job` の `preserve_run_state`（config.lisp 経由の upsert が渡す）は `cwd`/`env` も対象に含む。`cron_add`（`dsh/src/lisp/cron.rs`）は常にこのプロセスの現在の cwd/env を使う設計で、`config.lisp` は `dogesh -c "cron tick"`/`"cron run-job <uuid>"` の中でも評価されるため、対象に含めないと外部 tick（crontab の限定環境）が走るたびにジョブの実行環境が意図せず入れ替わる。
