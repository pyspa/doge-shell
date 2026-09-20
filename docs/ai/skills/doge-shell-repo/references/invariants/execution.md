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

## 1件の execution / process bug を直すときの5点

1. Minimal reproducer 2. Opposite case 3. Adjacent execution context（`FOO=bar` builtin を直すなら external env prefix・assignment-only・pipeline・`&&`/`||`・`$?` まで見る） 4. Resource / lifecycle invariant 5. Regression sibling。
