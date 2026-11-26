# Completion JSON Schema

正典は `command-completion-schema.json`。ここは実際に間違えやすい所だけ。

## トップレベル
- 必須は `command` のみ。**ファイル名の stem と一致**していなければテストが落ちる（`completions/git.json` なら `"command": "git"`）。
- 定義には `command` / `description` / `global_options` / `options` / `subcommands` / `arguments` / `short_option_clusters` を使う。`options` は `global_options` の旧名で、loader が merge する。新規は `global_options` を使う。
- 短いオプションをまとめられるコマンドでは `"short_option_clusters": true` を指定すると、入力中のフラグに次の一文字を追加する候補が出る。GNU 版と BSD 版で異なるフラグがある場合は、両方で使える文字を `"short_option_clusters": "bcdfns"` のように列挙する。既定値は false。

## option
- `short` は `-` 1 個で始まる（`-v`, `-f <FILE>`）。`long` は `-` か `+` で始まる（`--verbose`, `-Xmx`, `+short`）。裸の `-` / `--` は不可。
- 値を取るなら `takes_value: true` と `value_type` をセットで書く。
- `--color[=WHEN]` のように値を省略でき、次の単語を消費しないものは `value_attached_only: true` も指定する。
- 繰り返し指定できるoptionは `multiple: true`。省略時はfalseで、使用済みならshort/longの両表記を候補から除外する。ただしglobal/subcommand間で名前が衝突する場合は別名まで除外しない。argumentの `multiple` とは別の指定。

## argument
- `type` キーで型を書く（Rust 側のフィールド名は `arg_type` だが JSON では `type`）。
- 末尾の引数が繰り返せるなら `multiple: true`。

## 型（ArgumentType）
`#[serde(tag = "type", content = "data")]` なので、`data` の形は variant ごとに違う。

- data 不要: `{"type":"String"}` / `Directory` / `Number` / `Command` / `Environment` / `Url` / `Regex` / `Process` / `CommandWithArgs` / `User` / `Group` / `Signal` / `Interface`
- `{"type":"File","data":{"extensions":[".rs",".toml"]}}`（`extensions` は null 可）
- `{"type":"Choice","data":["a","b"]}` — **data は文字列の配列**。`{"choices":[...]}` のようにオブジェクトで包むと `invalid type: map, expected a sequence` で定義ごと無効になる（実際に、今は削除済みの旧 `sched.json` で起きた）
- `{"type":"Dynamic","data":{"provider":"git.branch","scope":"project"}}` — `scope` は任意
- `Script` は**組み込み定義では使用禁止**（テストが落ちる）。ユーザー生成の定義専用。

## provider
`data.provider` は `dsh-types/src/completion.rs` の `DYNAMIC_COMPLETION_PROVIDERS` に載っている文字列だけ。未知の名前はエラーにならず候補 0 件になる。一覧は `comp-gen --list-dynamic-providers` で出る。
