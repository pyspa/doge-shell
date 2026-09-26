# Invariants: 実行セマンティクス・ライフサイクル・所有権・ハーネス

[../invariants.md](../invariants.md) の索引から。process / shell semantics を触る前に読む。

## 所有権

- 稼働中の helper には論理 owner がちょうど1つ。`ExecutionResources -> Job -> detached reaper` へ移動はするが複製しない。consumer が必要な間は drop しない。`Shell` A の cleanup が `Shell` B の resource に触れない（`ProcessSubstitutionRegistry` は per-`Shell`。process-global に戻さない）。
- `Shell` drop は自分がまだ所有する helper group だけ kill する。reaper が回収済みのものは deregister 済みなので届かない。
- Process-substitution resource owner は exactly one of: `ExecutionResources` → reaper / registry → released/reaped。

## Process substitution lifecycle

- `<(...)` helper = producer, `>(...)` helper = consumer。
- Read direction: outer consumer completion permits producer termination (synchronous bounded reap foreground, detached grace reaper background)。
- Write direction: outer producer completion first closes parent write endpoint; helper consumer drains to EOF and is normally reaped without signal (detached natural reaper, prompt を block しない)。
- Output consumers remain asynchronous with respect to the next shell command.
- Normal shell exit may release Write-direction consumer ownership without signal (`detach_process_substitution_consumers_for_normal_exit`)。Abnormal `Shell::Drop` kills still-owned process-substitution groups (both directions)。

## ライフサイクル

- tree completion は strict: 全 stage `Completed` のときだけ完了。最終 stage の成功だけでは完了にしない。exit code は完了判定に使わない（non-zero も `Completed`）。
- `has_stopped_process`（いずれか `Stopped`）は SIGCONT 要否に使う。`is_fully_stopped`（`Stopped` あり + `Running` なし）は foreground-wait 終了と `Job.state` 要約に使う。`Completed` は neutral。
- `Job.state` は process tree から導出する。truth ではない。`refresh_lifecycle_state` の順序: 全完了 → 最終 stage の完了状態 / fully stopped → 実測の先頭 `Stopped` / それ以外 → `Running`。
- `Completed` stage は terminal。`Completed -> Running/Stopped` の遷移を作らない。
- `ECHILD` は wait の観測であって合成 `ProcessState` ではない。実ステータスを消費したコードだけが tree に記録する。後続の ECHILD 観測が exit code を捏造しない。
- `mark_stopped_processes_running` は `Stopped -> Running` のみ。`Completed` は触らない。

## Foreground stop / PTY ownership

- foreground `JobLaunchOutcome` は canonical tree から導出する。tail-only (`last_process_state`) を launch outcome に使わない。
- logical final status は completed tree にだけ存在する。incomplete tree の `final_exit_status()` は pipefail ON/OFF とも `None`。
- stopped FullProxy job は PTY/output ownership を保持するが terminal input proxy は持たない (`pty`/`pty_mode`/`pty_output_task` は残し `pty_input_task` は `None`)。
- `fg` resume は input proxy を exactly one 再生成する。既存 PTY session を継続し、新しい PTY は作らない。
- terminal raw mode は各 active FullProxy foreground interval にスコープする (`ForegroundPtyRawModeGuard` を再利用し、新しい raw-mode mechanism を作らない)。
- stopped/incomplete job を completed `OutputHistory` に記録しない。`final_exit_status().unwrap_or(0)` による synthetic success は禁止。
- completion-only I/O finalization (`capture_completed_output_and_history`) を stopped job に実行しない。

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
- FD close の検証に、drop 済み `OwnedFd` の stale `RawFd` 数値への `fcntl(F_GETFD)` + `EBADF` 期待を使わない。close 後の descriptor number は同一プロセス内の別 thread/test に即再利用され得るため、close/leak の証明にならない。FD leak は isolated child + bounded `RLIMIT_NOFILE` + repeated real operation の behavioral contract で検証する。
- property test で実 process を spawn しない。OS 依存（`waitpid`・`ECHILD`・`SIGSTOP` 配送・PGID・実 child exit）は deterministic test / Resource Harness 側。
- `cargo test` と nextest は process isolation が違う。nextest の `static Mutex` は binary を跨いだ exclusion にならない。targeted lane は `-j 1`。

## No-command pipeline stage

- runtime expansion 後に command name がない stage も正式な pipeline stage。drop / rewire（`A | empty | C` → `A | C`）・`/bin/true` 置換は禁止。位置を保ったまま isolated re-exec helper（`NoCommandProcess` + `InternalExecKind::NoCommand`）で実行する。
- single-stage no-command は現在の shell で `execute_no_command()`（assignment は parent へ反映）。multi-stage の no-command member は helper snapshot 内だけで assignment を適用し、parent へ漏らさない。
- NoCommand の redirection は通常の parent pipeline wiring（`Job::launch_process` → `JobProcess::launch`）で exactly once 適用する。helper request に redirection を含めないし、helper 側で再適用しない。helper が受け取るのは確定済み stdio のみ。
- helper へ送るのは assignments + `last_command_substitution_status` のみ。command substitution は materialization 時に実行・authorize 済みで、helper で再実行・再 parse（`dogesh -c "<source>"`）しない。status 伝搬: substitution なし → 0、あり → 最後の substitution status。

## Background job completion / `$!` / `wait`

- Topic split: background `$!`/`wait`/ledger guidance lives in
  [invariants/execution-background.md](execution-background.md).

## Normal exit / detached asynchronous AND-OR lists

- explicit async AND-OR list は shell 実行中は shell-owned のまま（`wait_jobs` + `KnownAsyncLedger::Active`）。`$!`/`wait` ownership はその間ずっと有効。
- normal execution-environment exit（command-mode return・helper-plan return）でのみ active known async job を明示 release する。launch 直後ではない（`sleep 1 & wait $!` を壊すため）。
- `Shell::Drop` は abnormal/safety cleanup であり POSIX async exit semantics ではない。detach 済み job は table から消えている。残っているものは kill する。
- detachment は wait/signal しない。double-fork・`setsid`・`setpgid`・`killpg` を追加しない。process group setup は spawn boundary で完了済み。orphan/reparent は OS に任せる。
- detached job は parent-only resource を持たない（`OutputMonitor`・PTY task・`ExecutionResources` なし）。`validate_detach_safe` が fail-closed で検査する。transactional two-pass（全 validate→commit）で partial detach 禁止。
- noninteractive async stdout/stderr は caller fd を direct inherit（monitor なし）。interactive async output は monitor-managed capture を維持（prompt-safe rendering）。
- no-job-control async stdin default は `/dev/null`。`supports_job_control()` は `interactive` を含む enabled 判定。body 内の explicit `< file` は override する。
- helper execution environment も同じ normal-exit rule（`run_helper_plan` return 前に detach）。substitution outer group ownership・`ProcessSubstitutionRegistry` の Read 所有権は触らない（Write のみ release）。
- output-pipe EOF lifetime と parent shell process lifetime は別物。capture pipe を grandchild が保持する場合の EOF 待ちは shell wait ではない（`$(...)`・subshell capture の既知の挙動）。command-mode でも helper でも fd を閉じる処理を足さない。
- detach 対象は ledger `Active` + job id 一致のものだけ。unknown/stopped/session-owned job は `Drop` cleanup へ残す。completed async job も detach 対象外（`Drop` の kill は completed tree には no-op）。
- SIGHUP/`disown`/`wait -f` は scope 外。`!` chat jobs・`ProcessSubstitutionRegistry` shutdown は触らない（Write release 以外）。

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

## JobProcess metadata legality

- Command/Builtin own command-scoped redirects/env overrides.
- NoCommand owns assignments + its own redirects.
- SyntheticSource owns neither redirects nor env overrides.
- AsyncList outer node owns neither; its serialized inner stages own their own metadata.
- JobProcess must not expose generic mutation APIs that can attach command metadata to every variant.
- `build_stage_process` dispatches to `StageProcess` (`Builtin`/`Command` only) and attaches metadata at the single `finish_stage_process` site, so a new dispatch arm cannot silently skip the attach.
- Illegal metadata states are prevented at construction/API boundaries, not discarded at runtime.
- Never log-and-ignore unexpected execution metadata.

## 1件の execution / process bug を直すときの5点

1. Minimal reproducer 2. Opposite case 3. Adjacent execution context（`FOO=bar` builtin を直すなら external env prefix・assignment-only・pipeline・`&&`/`||`・`$?` まで見る） 4. Resource / lifecycle invariant 5. Regression sibling。
