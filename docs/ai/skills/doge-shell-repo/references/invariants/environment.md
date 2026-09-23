# Invariants: ディレクトリ変更・Environment の状態

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。

## ディレクトリ変更
- cwd を変えるのは `ShellProxy::changepwd`（`dsh/src/proxy/mod.rs`）だけ。`std::env::set_current_dir` を直接呼ぶと `OLDPWD`・`path_history`（`z`）・`*on-chdir-hooks*`・`dir_stack[0]` が全部ずれる。
- `changepwd` は **chdir してから** hook / direnv で失敗しうる。`Err` を「何も起きなかった」と扱わないこと。呼び出し側は `get_current_dir()` と突き合わせて判定する（`dsh-builtin/src/dirstack.rs` の `apply` / `push_directory` が実例）。
- `dir_stack[0]` は常に現在ディレクトリ。`dirs -v` の番号と `cd -N` はこの前提で一致している。

## Environment の状態
- `EnvironmentSnapshot`（`dsh/src/lisp/mod.rs`）は config.lisp 失敗時のロールバック対象。**設定**（`keybindings`, `alias`, `abbreviations` …）は追加する。**ランタイム状態**（`dir_stack`, `scheduler`）は追加しない。
- `config.lisp` は `Repl::new` より前に走る。REPL 起動前に登録できる必要があるものは `Environment` に置く。
- `variable_state.variables` / `exported_vars` の key は bare variable name のみ。
- sigil/brace 付きスペルは lookup/parser input syntax で、storage key にしない。
- shell variable mutation は Environment の canonical setter/remover を通し、derived state を同期する。
- raw map の bulk restore/apply 後は variable-derived projections を再構築する。
