# Invariants: cron

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。旧 `sched`（`dsh/src/scheduler/`）は削除済み — `cron`（`dsh/src/cron/`）に統合された。

- `Shell` は `!Send`（`Rc<RefCell<LispEngine>>`）。spawn したタスクから `eval_str` は呼べない。AI ジョブは `dsh -c "cron run-job <uuid>"` という別プロセスで実行する（`dsh/src/cron/run_job.rs`）。子プロセスに渡すのは UUID だけで、goal や grant は SQLite 経由で渡す。
- セッション内 runner（`dsh/src/cron/runner.rs`）はセッション内の due スキャンだけを担当し、実際の claim・実行は `dsh/src/cron/tick.rs` の `run_once` に一本化されている。外部 tick（`dsh -c "cron tick"`）も同じ関数を呼ぶ。
- 二重実行の防止は SQLite の `BEGIN IMMEDIATE` トランザクション + 条件付き `UPDATE`（`claimed_by`/`claimed_until`）で行う。in-process の `RwLock`/`AtomicBool` には依存しない — 複数セッション・外部 tick が同時に動く前提だから。
- claim は `next_run_at` を同じトランザクション内で前進させる。skip したときも必ず動かす。さもないと永久に due のままスピンする。
- pause から resume するときは `next_run_at` を貼り直す（`store.set_paused`/`set_all_paused`）。しないと溜まった分が一斉に発火する。
- `cron tick` はジョブの失敗を tick 自体の失敗にしない。既定では無出力・exit 0 を返す。ストアが開けない・スキーマが新しすぎるなど tick 自体が壊れたときだけ非 0。system cron がメール地獄にならないための設計。
- `(cron-add ...)` は name で upsert する。CLI の `cron add` は同名でエラー（`--force` で上書き）。`config.lisp` は毎起動評価されるため、upsert でないと 2 回目の起動で `already exists` エラーになり、そのエラーで `config.lisp` の残り（alias・abbr・PATH 設定）が丸ごと止まる。
- 非対話の `cron tick`（`dsh -c`）は `needs_interactive_services()` が false なので MCP 接続を張らない。`--allow-mcp` を持つ AI ジョブだけ `cron run-job` 内で明示的に `reload_mcp_config()` を呼ぶ。
- status_line が読む `Environment.cron_health`（`CronHealth` のキャッシュ）は cron runner がスキャンのたびに更新する。`status_line::compose()` 自体は I/O をしない。
- エージェント（`!` チャット / `agent run`）からの cron 操作は chat tool `cron_manage`（`dsh-builtin/src/chatgpt/tool/cron.rs` + `dsh/src/cron/tool.rs`）一本。`execute` 経由の `dsh -c "cron ..."` を許可リストに足して回避させない — builtin は `sh -c` の外なのでどのみち届かない。`cron_manage` の `create` は常に `--paused` を強制し、grant はタスク自身の `TaskGrant` を超えられない（`grant_exceeds_task`）。cron の argv 検証（`cli::parse_add`/`parse_edit`）を二重実装しない。
- AI ジョブの外側 timeout は `run_job.rs` 内のウォッチドッグスレッドが担う。`lease_secs(timeout)`（`store/claim.rs` と `dsh-types/src/cron/job.rs` が共有する同一関数）より数秒早く自プロセスグループへ `SIGKILL` する。2 つを別々の定数にしない — ずれるとウォッチドッグより先にリースが失効し、生きている run を「見捨てられた」と誤認する。
