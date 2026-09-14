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
- `runs.stdout`/`stderr`（8KiB clamp、`cron logs` が読む）は `SELECT_RUN`（`store/rows.rs`）に**足さない**。この定数と `run_from_row` の `row.get(n)` は列順で結合しており、足すとコンパイルは通るが黙って別のフィールドを返す。本文は `run_output`（2 クエリ方式）が別途読む。
- `complete_run` の `agent_task_id` 列は `COALESCE(?9, agent_task_id)` で書く。`attach_agent_task`（run 開始時に task-id を先に記録する）の効果を、config/budget/lock などトークン消費前に打ち切る早期失敗パスの `agent_task_id: None` で消してはいけない — watchdog に `SIGKILL` された run はここでしか task-id を保持できず、消えると `cron logs` が agent ストアへ辿れなくなる。
- AI ジョブの `RunOutcome.stdout`（`dsh/src/agent/summary.rs` の `task_summary` が組み立てる）は `digest` の入力に使わない（`run_job.rs` の `agent_digest_input`）。要約文は毎回変わるので、そこを混ぜると `--on change` が実質 `--on always` になる。
- `store::trigger()`（`cron run <job>` の非 `--now` 経路、`cron_manage(action=run)` も同じ）は paused/blocked を拒否する。`claim_due_jobs` が `enabled=1 AND blocked=0` を要求するため、拒否せず `next_run_at` だけ書くと一生発火しない予約が残り、「paused なら `next_run_at` は NULL」という他の書き込み経路（`set_paused`/`patch`）の不変条件も破る。`claim_one`（`cron run --now`）だけは両方を無視してよい — claim を直接取るのであってスキャン待ちではないため。
- `insert_job` の `preserve_run_state`（config.lisp 経由の upsert が渡す）は `cwd`/`env` も対象に含む。`cron_add`（`dsh/src/lisp/cron.rs`）は常にこのプロセスの現在の cwd/env を使う設計で、`config.lisp` は `dsh -c "cron tick"`/`"cron run-job <uuid>"` の中でも評価されるため、対象に含めないと外部 tick（crontab の限定環境）が走るたびにジョブの実行環境が意図せず入れ替わる。
- AI ジョブの env スナップショット（`ClaimedRun.env`）は `agent_outcome`（`run_job.rs`）の先頭、他のスレッドを spawn する前に `apply_job_environment` で適用する。`std::env::set_var` と `Environment::set_system_env_var` の両方に書く — `--env` grant や sandbox 実行は生の `std::env` を読み、`resolved_config`（API キー解決）は `Environment::get_var` を先に試すため、片方だけでは経路によって古い値が残る。
- AI ジョブの失敗理由は `run_task`/`agent_outcome` が付与する `crate::agent::TaskFailure` marker（`anyhow::Error::new(marker).context(human_message)`）から `run_job.rs::failure_reason` が読む。`error.to_string()` の文字列一致で分岐しない。marker が付いていないエラーの既定値は `RunReason::Transient`（3連続で初めて incident 昇格）であって `StateUnusable`（1回で job を blocked にする）ではない。
