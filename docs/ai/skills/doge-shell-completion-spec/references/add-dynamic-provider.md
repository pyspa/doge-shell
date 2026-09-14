# 新しい dynamic provider を足す

まず provider が**固定シェイプ**（固定の実行ファイル + 固定引数、または固定パス読み取り + パーサ/ローダー関数）かどうかを判定する。動的な引数構築・JSON 応答の解釈・複数コマンドのマージ・ランタイム発見スコープが要るなら「固定シェイプでない」側の手順に従う。

### 固定シェイプの場合

`dsh/src/completion/dynamic/local.rs` の `LocalSpec` テーブル駆動経路に乗る。`family_for` の prefix も family モジュールの match アームも要らない。

1. `dsh-types/src/completion.rs` の `DYNAMIC_COMPLETION_PROVIDERS` に追加。**アルファベット順を守る**（`DynamicProviderId::parse` が `binary_search`）。順序を崩すと `dynamic_completion_providers_are_sorted_and_unique` が落ちる。
2. `command-completion-schema.json` の Dynamic Type の `provider` enum に**同じ順序**で追加。`json_loader.rs` の `command_completion_schema_uses_shared_dynamic_provider_list` が配列を完全一致で比較する。
3. `LocalSpec` の行を 1 つ足す。置き場所は、使うパーサ/ローダー関数と同じファイル——`dsh/src/completion/dynamic/specs.rs`（`CORE_LOCAL_SPECS`）か、`container.rs`/`dev.rs`/`linux.rs`/`project.rs` の `LOCAL_SPECS`。既存の関数が private のまま住んでいる場所に置くことで可視性を広げずに済み、Linux 専用パス文字列は所有ファイルを変えないので `scripts/portability-allowlist.txt` も無傷で済む。パーサ/ローダーが新規なら、それも同じファイルに書く。
4. `completions/<command>.json` の該当引数を `{"type":"Dynamic","data":{"provider":"..."}}` にする。
5. README の動的補完の一覧を更新（慣習）。
6. テスト。`local.rs` の `every_local_spec_names_a_registered_provider` / `local_spec_providers_are_unique` は誤字・重複だけを見る——実際に候補が返るかは `dynamic.rs`/family モジュールのテストで確認する。偽 CLI を置くなら `completion/integrated.rs` の `write_executable_script` + `engine_with_path` が手本。

検証: `cargo test -p dsh-types` と `cargo test -p doge-shell --lib completion`。OS 固有のソースを足したときは `scripts/check-portability.py` も。

### それ以外（動的な引数・JSON 解釈・複数コマンドのマージ・ランタイム発見スコープなど）

3 つの網羅性テストが別々に落ちるので、部分的に直すとテストを 3 回回すことになる。まとめて変更する。

1. `dsh-types/src/completion.rs` の `DYNAMIC_COMPLETION_PROVIDERS` に追加（上と同じ、アルファベット順）。
2. `command-completion-schema.json` の Dynamic Type の `provider` enum に**同じ順序**で追加（上と同じ）。
3. `dsh/src/completion/dynamic/registry.rs` の `family_for` にプレフィクスを追加。**忘れても黙って `External` になる**だけでエラーにならない。
4. その family のモジュール（`dynamic/{git,container,kubernetes,linux,dev,project,external}.rs`）の `collect` に match アームを追加。抜けると `every_registered_provider_has_a_dispatch_arm`（`dynamic.rs`）が落ちる。
5. 収集の実装。OS 固有のソース（`/proc`、`/etc/passwd`、`sysctl` …）を読むなら `#[cfg(not(target_os = "macos"))]` と `#[cfg(target_os = "macos")]` を対で書き、`../../doge-shell-repo/references/platform-support.md` を先に読む。片肺だともう一方の OS で静かに 0 件になる。サブプロセスを起こすなら `dynamic/runner.rs` 経由（ローカル 1500ms / リモート 5s のタイムアウト付き）。`cached_only` は `request.cache_policy.is_cached_only()` から各 `collect_*` へ横流しする定型に従う。**cached 専用の dispatch を新しく作らない**（AGENTS.md の設計境界）。
6. `completions/<command>.json` の該当引数を `{"type":"Dynamic","data":{"provider":"..."}}` にする。
7. README の動的補完の一覧を更新（慣習）。
8. テスト。偽 CLI を置くなら `completion/integrated.rs` の `write_executable_script` + `engine_with_path` が手本。

検証: `cargo test -p dsh-types` と `cargo test -p doge-shell --lib completion`。OS 固有のソースを足したときは `scripts/check-portability.py` も。

## 注意
- 補完には経路が 2 つある。コマンド名直結の `DYNAMIC_PROVIDER_SPECS`（`completion/integrated/providers.rs`）が先に走り、その結果に宣言的 provider の結果が `extend` される。既存コマンドに足すときは、そのコマンドが前者に載っていないか先に確認する。
- `dynamic/git.rs` の `_ =>` は `platform::collect` にフォールスルーする。match アームが無いことは未対応を意味しない。テーブル駆動の provider はそもそも family の match まで届かないので、なおさら「アームが無い = 未対応」ではない。
