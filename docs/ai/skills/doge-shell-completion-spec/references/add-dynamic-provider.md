# 新しい dynamic provider を足す

3 つの網羅性テストが別々に落ちるので、部分的に直すとテストを 3 回回すことになる。まとめて変更する。

1. `dsh-types/src/completion.rs` の `DYNAMIC_COMPLETION_PROVIDERS` に追加。**アルファベット順を守る**（`DynamicProviderId::parse` が `binary_search`）。順序を崩すと `dynamic_completion_providers_are_sorted_and_unique` が落ちる。
2. `command-completion-schema.json` の Dynamic Type の `provider` enum に**同じ順序**で追加。`json_loader.rs` の `command_completion_schema_uses_shared_dynamic_provider_list` が配列を完全一致で比較する。
3. `dsh/src/completion/dynamic/registry.rs` の `family_for` にプレフィクスを追加。**忘れても黙って `External` になる**だけでエラーにならない。
4. その family のモジュール（`dynamic/{git,container,kubernetes,linux,dev,project,external}.rs`）の `collect` に match アームを追加。抜けると `every_registered_provider_has_a_dispatch_arm`（`dynamic.rs`）が落ちる。
5. 収集の実装。サブプロセスを起こすなら `dynamic/runner.rs` 経由（ローカル 1500ms / リモート 5s のタイムアウト付き）。`cached_only` は `request.cache_policy.is_cached_only()` から各 `collect_*` へ横流しする定型に従う。**cached 専用の dispatch を新しく作らない**（AGENTS.md の設計境界）。
6. `completions/<command>.json` の該当引数を `{"type":"Dynamic","data":{"provider":"..."}}` にする。
7. README の動的補完の一覧を更新（慣習）。
8. テスト。偽 CLI を置くなら `completion/integrated.rs` の `write_executable_script` + `engine_with_path` が手本。

検証: `cargo test -p dsh-types` と `cargo test -p doge-shell --lib completion`。

## 注意
- 補完には経路が 2 つある。コマンド名直結の `DYNAMIC_PROVIDER_SPECS`（`completion/integrated.rs`）が先に走り、その結果に宣言的 provider の結果が `extend` される。既存コマンドに足すときは、そのコマンドが前者に載っていないか先に確認する。
- `dynamic/git.rs` の `_ =>` は `platform::collect` にフォールスルーする。match アームが無いことは未対応を意味しない。
