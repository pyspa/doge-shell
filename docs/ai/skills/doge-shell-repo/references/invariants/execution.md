# Invariants: 実行セマンティクス・ライフサイクル・所有権・ハーネス

[../invariants.md](../invariants.md) の索引から。process / shell semantics を触る前に読む。

## 所有権

- 稼働中の helper / producer には論理 owner がちょうど1つ。`ExecutionResources -> Job -> detached reaper` へ移動はするが複製しない。consumer が必要な間は drop しない。`Shell` A の cleanup が `Shell` B の resource に触れない（`ProducerRegistry` は per-`Shell`。process-global に戻さない）。
- `Shell` drop は自分がまだ所有する producer group だけ kill する。reaper が回収済みのものは deregister 済みなので届かない。

## ライフサイクル

- tree completion は strict: 全 stage `Completed` のときだけ完了。最終 stage の成功だけでは完了にしない。exit code は完了判定に使わない（non-zero も `Completed`）。
- `has_stopped_process`（いずれか `Stopped`）は SIGCONT 要否に使う。`is_fully_stopped`（`Stopped` あり + `Running` なし）は foreground-wait 終了と `Job.state` 要約に使う。`Completed` は neutral。
- `Job.state` は process tree から導出する。truth ではない。`refresh_lifecycle_state` の順序: 全完了 → 最終 stage の完了状態 / fully stopped → 実測の先頭 `Stopped` / それ以外 → `Running`。
- `Completed` stage は terminal。`Completed -> Running/Stopped` の遷移を作らない。
- `ECHILD` は wait の観測であって合成 `ProcessState` ではない。実ステータスを消費したコードだけが tree に記録する。後続の ECHILD 観測が exit code を捏造しない。
- `mark_stopped_processes_running` は `Stopped -> Running` のみ。`Completed` は触らない。

## ハーネス運用

- shell semantics 変更 → `dsh/tests/spec/*.toml` に case を先に追加/更新し `cargo test -p doge-shell --test shell_contract`。
- process lifecycle 変更 → `dsh/src/process/job/lifecycle_property_tests.rs` の invariant を確認し `cargo test -p doge-shell --lib lifecycle_property`。
- FD / PID / PGID 所有権変更 → `dsh/tests/resource_contract.rs` + `cargo test -p doge-shell --test fd_ownership`。
- concurrency 所有権変更 → `resource_contract.rs` の明示 concurrency test。既存テストを parallel にしない（serial lock を外さない）。
- 既知 bug → XFAIL contract + `spec/xfail-allowlist.txt` への ID 追加。allowlist は exact match（新規の隠れ XFAIL も stale entry も落とす）。
- bug fix → XPASS（suite failure）を確認してから XFAIL を削除。XPASS を silent success にしない。
- process-heavy fix → `cargo nextest run -p doge-shell -P ci --stress-count 10 -j 1` の targeted stress。
- contract の期待値は現在の実装ではなく仕様上正しい挙動を書く。壊れていれば XFAIL。`exit 7` のような外部 helper は spec 内 token（`{{TRUE}}` 等）で書く。絶対 path・`..` escape は runner が reject する。
- race test に `sleep` ベースの readiness は使わない。pipe / marker / handshake + bounded timeout。
- property test で実 process を spawn しない。OS 依存（`waitpid`・`ECHILD`・`SIGSTOP` 配送・PGID・実 child exit）は deterministic test / Resource Harness 側。
- `cargo test` と nextest は process isolation が違う。nextest の `static Mutex` は binary を跨いだ exclusion にならない。targeted lane は `-j 1`。

## No-command pipeline stage

- runtime expansion 後に command name がない stage も正式な pipeline stage。drop / rewire（`A | empty | C` → `A | C`）・`/bin/true` 置換は禁止。位置を保ったまま isolated re-exec helper（`NoCommandProcess` + `InternalExecKind::NoCommand`）で実行する。
- single-stage no-command は現在の shell で `execute_no_command()`（assignment は parent へ反映）。multi-stage の no-command member は helper snapshot 内だけで assignment を適用し、parent へ漏らさない。
- NoCommand の redirection は通常の parent pipeline wiring（`Job::launch_process` → `JobProcess::launch`）で exactly once 適用する。helper request に redirection を含めないし、helper 側で再適用しない。helper が受け取るのは確定済み stdio のみ。
- helper へ送るのは assignments + `last_command_substitution_status` のみ。command substitution は materialization 時に実行・authorize 済みで、helper で再実行・再 parse（`dogesh -c "<source>"`）しない。status 伝搬: substitution なし → 0、あり → 最後の substitution status。

## Background job completion / `$!` / `wait`

- 非同期 AND-OR list は parent shell において associated PID を1つだけ持つ。doge-shell では managed AsyncList helper PID。nested internal command PID を外へ露出させない（helper が subshell environment そのもの・process-group leader・AND-OR 全体の status を持つ・parent が canonical に所有する waitable child）。
- `$!` は最後に登録された associated PID を展開する（`Environment.last_async_pid`）。PID の wait status を consume しても `$!` 自体は消さない（2回目の `wait $!` は値自体は展開できるが ledger から消えているため127）。
- wait ownership は `Shell.known_async`（`KnownAsyncLedger`）が持つ。`Environment` に ledger を入れない（parameter expansion と lifecycle ownership の分離）。subshell snapshot は `last_async_pid` の値だけ引き継ぎ、ledger は引き継がない。
- Active job は `wait_jobs` に住む。重い `Job` オブジェクトの削除は、monitor へ terminal retirement action を1回実行（running は bounded `drain_available`、canonical completed tree の reconciliation は `ReadyNow`、explicit ownership wait（`wait PID`・`fg`）は `drain_to_eof`）し、known async job なら final status を ledger に archive してから。completed `Job` の直接 `remove()` は禁止。全経路（`check_job_state`・`jobs`・notices・`fg`・`bg`・`wait`）は canonical finalizer を通す。
- running background monitor は bounded `drain_available` を使う。canonical completed tree の reconciliation は `ReadyNow`（await なし・timeout なし・O_NONBLOCK fd への直接 drain）。`ReadyNow` は monitor ownership がそこで終了するため pending fragment を publish する。explicit ownership wait（`wait PID`・`fg`）は `ToEof` を使う。descendant が pipe を保持していても reconciliation は EOF を待たない。`FinalizeDrain::Skip` は存在しない。
- drain error で status を失わない。exit status 確定と ledger archive を先に保証し、monitor error は diagnostic に留める。
- final status は canonical tail process の `ProcessState::shell_exit_code()` から取る（`job.state` の blind read 禁止）。signal 死は128+N。
- `ECHILD` は completion を捏造しない。canonical tree が `Completed` でなければ status を invent しない。`wait(-1)`/`waitpid(-1)` 禁止。ledger→Job→canonical PID set 経由でのみ待つ。
- `wait` semantics: 引数なし→全 known を待って0（個別 failure を反映しない）・全 consume。`wait PID` は active→take/termination-wait/finalize/consume、completed→即返却+consume、unknown→127（ alien PID を `waitpid` しない）。複数 operand は順に処理し最後の status。repeat は127。`-n/-p/-f` は usage error（exit 1）。`%spec`/非数値は 127 として扱い次の operand へ進む（code behavior）。
- wait 用は TerminationOnly policy（stop は completion 扱いせず待機継続）。foreground の stop 終了・SIGINT forward と混ぜない。`wait` 中の SIGINT は child へ forward せず builtin を interrupt して130、job は requeue・ledger は Active のまま。
- ledger は bounded（`CHILD_MAX` 相当、fallback 4096）。prune は oldest Completed から。Active は捨てない。PID reuse は new register が勝つ。completed retention は metadata/status のみで process resource を所有しない。
- `jobs`・completion notice・`fg`/`bg` は archive するが consume しない。consume するのは `wait` だけ。
- normal-exit detach（`Shell::detach_known_async_jobs_for_normal_exit`）は user command ではない。ledger status を consume せず `$?`/`foreground status` を書き換えない。detach failure は infrastructure failure（exit 1）で ownership を維持し、`Drop` cleanup が kill する。
- テストは `dsh/tests/wait_semantics.rs`。sandbox の SafetyGuard が nested shell（`sh`/`bash`）を deny するため、exact status には `false`(1)/`true`(0)/self-`kill`(143)/unknown-command(127) を使う。`sh -c 'exit N'` 前提にしない。

## Normal exit / detached asynchronous AND-OR lists

- explicit async AND-OR list は shell 実行中は shell-owned のまま（`wait_jobs` + `KnownAsyncLedger::Active`）。`$!`/`wait` ownership はその間ずっと有効。
- normal execution-environment exit（command-mode return・helper-plan return）でのみ active known async job を明示 release する。launch 直後ではない（`sleep 1 & wait $!` を壊すため）。
- `Shell::Drop` は abnormal/safety cleanup であり POSIX async exit semantics ではない。detach 済み job は table から消えている。残っているものは kill する。
- detachment は wait/signal しない。double-fork・`setsid`・`setpgid`・`killpg` を追加しない。process group setup は spawn boundary で完了済み。orphan/reparent は OS に任せる。
- detached job は parent-only resource を持たない（`OutputMonitor`・PTY task・`ExecutionResources` なし）。`validate_detach_safe` が fail-closed で検査する。transactional two-pass（全 validate→commit）で partial detach 禁止。
- noninteractive async stdout/stderr は caller fd を direct inherit（monitor なし）。interactive async output は monitor-managed capture を維持（prompt-safe rendering）。
- no-job-control async stdin default は `/dev/null`。`supports_job_control()` は `interactive` を含む enabled 判定。body 内の explicit `< file` は override する。
- helper execution environment も同じ normal-exit rule（`run_helper_plan` return 前に detach）。substitution outer group ownership・`ProducerRegistry` は触らない。
- output-pipe EOF lifetime と parent shell process lifetime は別物。capture pipe を grandchild が保持する場合の EOF 待ちは shell wait ではない（`$(...)`・subshell capture の既知の挙動）。command-mode でも helper でも fd を閉じる処理を足さない。
- detach 対象は ledger `Active` + job id 一致のものだけ。unknown/stopped/session-owned job は `Drop` cleanup へ残す。completed async job も detach 対象外（`Drop` の kill は completed tree には no-op）。
- SIGHUP/`disown`/`wait -n/-p/-f` は scope 外。`!` chat jobs・`ProducerRegistry` shutdown は触らない。

## Async AND-OR signal disposition

- job-control-disabled async AND-OR list starts with SIGINT/SIGQUIT ignored
- decision is captured at parent launch boundary via supports_job_control()
- request carries explicit PlanSignalPolicy
- race-free transition is spawn-mask block -> SIG_IGN install -> unblock
- parent process signal dispositions are never temporarily modified
- normal/job-control-enabled helpers keep existing signal semantics
- external commands inherit SIG_IGN; child_exec does not duplicate the async policy
- nested async lists re-evaluate their own job-control state
- async initial ignore is not "ignored on shell entry" metadata; future trap support must be able to override it

## 1件の execution / process bug を直すときの5点

1. Minimal reproducer 2. Opposite case 3. Adjacent execution context（`FOO=bar` builtin を直すなら external env prefix・assignment-only・pipeline・`&&`/`||`・`$?` まで見る） 4. Resource / lifecycle invariant 5. Regression sibling。
