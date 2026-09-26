# Invariants: background job completion / `$!` / `wait`

[../invariants.md](../invariants.md) の索引から。background job / `$!` / `wait` を触る前に読む。

## Background job completion / `$!` / `wait`

- 非同期 AND-OR list は parent shell において associated PID を1つだけ持つ。doge-shell では managed AsyncList helper PID。nested internal command PID を外へ露出させない（helper=subshell 全体・process-group・status の所有者）。
- `$!` は最後に登録された associated PID を展開する（`Environment.last_async_pid`）。PID の wait status を consume しても `$!` 自体は消さない（2回目の `wait $!` は値自体は展開できるが ledger から消えているため127）。
- wait ownership は `Shell.known_async`（`KnownAsyncLedger`）が持つ。`Environment` に ledger を入れない（parameter expansion と lifecycle ownership の分離）。subshell snapshot は `last_async_pid` の値だけ引き継ぎ、ledger は引き継がない。
- Active job は `wait_jobs` に住む。重い `Job` オブジェクトの削除は、monitor へ terminal retirement action を1回実行（drain 種別は次項通り）し、known async job なら final status を ledger に archive してから。completed `Job` の直接 `remove()` は禁止。全経路（`check_job_state`・`jobs`・notices・`fg`・`bg`・`wait`）は canonical finalizer を通す。
- running background monitor は bounded `drain_available` を使う。canonical completed tree の reconciliation は `ReadyNow`（await なし・timeout なし・O_NONBLOCK fd への直接 drain）。`ReadyNow` は monitor ownership がそこで終了するため pending fragment を publish する。explicit ownership wait（`wait PID`・`fg`）は `ToEof` を使う。descendant が pipe を保持していても reconciliation は EOF を待たない。`FinalizeDrain::Skip` は存在しない。
- drain error で status を失わない。exit status 確定と ledger archive を先に保証し、monitor error は diagnostic に留める。
- OutputMonitor terminal rendering is best-effort presentation only.
- A renderer write/flush failure disables rendering for that monitor exactly once.
- Renderer failure never terminates a drain, changes wait status, or releases the capture-pipe reader early.
- After renderer failure, pipe drain, captured_output, and SharedOutputObserver continue normally.
- ReadyNow still reads until EOF/WouldBlock; ToEof still owns the reader until EOF.
- Actual read/readiness/framing errors retain their existing Result semantics.
- Lifecycle: `Job.state` は process tree から導出する lifecycle summary のまま（導出順序は「ライフサイクル」節通り）。pipefail status を `Job.state` に書き込まない。
- Logical pipeline exit status: policy は `Job::launch` 時に `ShellOptions` から snapshot し、`Job` が frozen で所有する。pipefail OFF → tail stage の `shell_exit_code()`、pipefail ON → 右端に最も近い non-zero `shell_exit_code()`（全成功なら 0）。finalization は live `ShellOptions` を読まない。
- final status は `Job::final_exit_status()`（`dsh/src/process/pipeline_status.rs`）の single resolver から取る（`job.state` の blind read 禁止）。signal 死は既存 `ProcessState::shell_exit_code()` の 128+N を使う。`NoCommand` stage も通常 stage として参加する。未完了 tree は `None`（status を捏造しない）。
- `ECHILD` は completion を捏造しない。canonical tree が `Completed` でなければ status を invent しない。`wait(-1)`/`waitpid(-1)` 禁止。ledger→Job→canonical PID set 経由でのみ待つ。
- `wait` semantics: 引数なし→全 known を待って0（個別 failure を反映しない）・全 consume。`wait PID` は active→take/termination-wait/finalize/consume、completed→即返却+consume、unknown→127（alien PID を `waitpid` しない）。複数 operand は順に処理し最後の status。repeat は127。`wait` operand は bare decimal=PID、`%`/`%+`/`%%`/`%-`/`%N`=job spec（選択規則→Active-job selection）。explicit `%N` のみ completed ledger fallback。`wait -f` は usage error（exit 1）。非数値・非 `%`・unknown jobspec は 127 として扱い次の operand へ進む（code behavior）。
- `wait -p VAR`: valid simple variable name を validate し、wait 開始前に destination を unset（値+export bit を除去）する。invocation の最終 returned status を実際に提供した known completion の canonical associated PID のみ publish（jobspec 時も PID を格納し job number は入れない）。actual child status 127 でも assign し、unknown/stale target の 127 では unset のまま。sequential operands は最終 status に対応する identity のみ publish（後続 NoCompletion が先行 completion を reset）。`wait -n` は selected `WaitCompletion` の PID を publish。bare `wait -p`（operand なし）は全 wait 後に status 0 で unset のまま。SIGINT / NoTargets は unset のまま。assignment は wait outcome 確定後に1回だけ行い、loop 内で逐次行わない。wait の ownership/consumption 規則は変えない。
- `wait -n` semantics: resolved known target set のうち1件の completion を待つ。no target→127（引数なし `wait` の0と違う）。already-completed ledger status は即返却。選択した status だけ consume し、unselected は後続 `wait` 用に retained。duplicate PID/jobspec は二重 consume しない。explicit ownership completion は `FinalizeDrain::ToEof`。unrelated child status を consume しない（`waitpid(-1)` 禁止）。stopped は completion でない。`ECHILD` から synthetic completion を作らない。stale Active ledger entry（table job なし）は削除し、他 valid target を待ち続ける。全 target が stale/unknown なら127。
- wait 用は TerminationOnly policy（stop は completion 扱いせず待機継続）。foreground の stop 終了・SIGINT forward と混ぜない。`wait`/`wait -n` 中の SIGINT は child へ forward せず builtin を interrupt して130、job は requeue・ledger は Active のまま。
- ledger は bounded（`CHILD_MAX` 相当、fallback 4096）。prune は oldest Completed から。Active は捨てない。PID reuse は new register が勝つ。completed retention は metadata/status のみで process resource を所有しない。
- `jobs`・completion notice・`fg`/`bg` は archive するが consume しない。consume するのは `wait` だけ。
- `jobs` は reconciliation より先に CLI を parse する。syntax valid 後に `check_job_state` で reconcile し、completed Job を直接 remove せず、ledger を consume しない。
- `jobs` default 出力は pid を含まない（`job`/`state`/`command`）。`-l`/`--list` は associated pid を足した long table。`-p`/`--pgid` は canonical process-group leader ID のみを table/header/prose なしで出す。`-p` で 0 件なら空 stdout + status 0。pgid が無い active Job は fail-closed。
- Active-job selection: table order is authority; 0 jobs=None/None, 1 job=both resolve to it, 2+=current last/previous second-last. `%`/`%+`/`%%`=current, `%-`=previous, `%N`=stable id; bare numeric is command-specific (wait=PID, fg/bg/jobs=legacy).
- jobs markers use the full table, not the filtered slice: current `+`, previous `-`, single job `+` only.
- shell.job completion shares the selection (single job still offers `%-`); notice markers are history, not selection authority.
- `bg` の explicit operand は全て、1 回 reconcile した mutation 前 active-table snapshot に対して stable job ID へ解決する。remove/requeue を跨いで `wait_jobs` index を持ち回らない。
- `bg` は明示 target を全件 best-effort で処理する。失敗 target があっても他 target を skip/rollback しない。1 件でも失敗したら builtin は non-zero。
- `bg` の選択 Job は毎回 `finalize_background_resume()` を通す。SIGCONT failure でも active ownership を requeue する。completion は archive するが `KnownAsyncLedger` を consume しない。
- `fg` is a status-bearing job-control builtin. Completed: return `Job::final_exit_status()` after canonical `ToEof` finalization. Stopped again: job final status remains `None`; `fg` invocation status is `128 +` observed stop signal; job is requeued and wait ownership remains `Active`. Running/incomplete + successful wait: infrastructure inconsistency; requeue ownership first, then error; never synthesize status 0. `fg` archives known-async completion but never consumes it; only `wait` consumes `KnownAsyncLedger` status. Do not read live `ShellOptions`. Do not derive pipeline status from `Job.state`.
- normal-exit detach（`Shell::detach_known_async_jobs_for_normal_exit`）は user command ではない。ledger status を consume せず `$?`/`foreground status` を書き換えない。detach failure は infrastructure failure（exit 1）で ownership を維持し、`Drop` cleanup が kill する。
- テストは `dsh/tests/wait_semantics.rs`。sandbox の SafetyGuard が nested shell（`sh`/`bash`）を deny するため、exact status には `false`(1)/`true`(0)/self-`kill`(143)/unknown-command(127) を使う。`sh -c 'exit N'` 前提にしない。
- A top-level managed async AND-OR helper is the associated PID and process-group leader; nested commands belong to that job group.
- Integration/contract cleanup for a deliberately long-lived async AND-OR job must terminate the owned process group, not only `$!`. Signaling `$!` as a positive PID kills only the helper and can leave helper-spawned descendants alive.
- Long-lived contract helpers must be explicitly reaped/terminated. Do not hide leaks by redirecting all inherited descriptors to /dev/null.
- Contract case `timeout_ms` currently bounds the primary dogesh child wait, not an indefinitely-held stdout/stderr EOF after that primary child exits. Therefore descendant FD ownership must remain correct.
