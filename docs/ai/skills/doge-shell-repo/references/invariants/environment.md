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
- `Environment::variables` が全 shell variable value の唯一の storage。
- 起動時 process environment も `variables` に seed し、その name を `exported_vars` に入れる。
- `exported_vars` は export attribute のみで値を持たない。
- child environment は `variables` + `exported_vars` から materialize する。
- runtime shell state の fallback として `std::env` を再読込しない。
- system command lookup/completion の PATH authority も
  `Environment.variable_state.paths`。
- completion cache refresh が `std::env::PATH` を再読込してはいけない。
- PATH-derived async cache result は、refresh 開始時の PATH generation と
  scan id が current の場合だけ publish できる。scan id は同一 generation 内で
  古い scan の結果を後から publish できないようにする。
- runtime completion request は PATH snapshot の activation ticket を保持し、
  古い request が global cache を以前の PATH へ巻き戻さない。
- integrated top-level completion cache は Environment の logical PATH
  activation generation で scope する。
- `std::env` は bootstrap/process-global integration boundary に限定する。
- consumer が `Environment` を利用可能な runtime path では、shell variable
  の miss を `std::env::var` へ fallback しない。startup process environment
  は `Environment::new()` で import 済みなので、再読込するものが何もない。
- logical unset は final。process-global startup value を再発見しない。
  (`AI_CHAT_API_KEY` を `unset` しても、起動時 process env の同名値で
  `!` chat が復活してはいけない、等)
- shell で変更された value は runtime consumer が即座に見る
  (`set AI_CHAT_MODEL` が次の `!` turn に効く、等)。
- shell variable mutation は Environment の canonical setter/remover を通し、derived state を同期する。
- raw map の bulk restore/apply 後は variable-derived projections を再構築する。
- direnv restore state は root 登録時ではなく activation 時に capture する。
- active direnv root は exact previous logical state（value + export bit）を所有し、leave 時に `restore_shell_var_state` で復元する。
- cross-root transition は unload deepest-first → load shallowest-first。
- direnv read/load failure で allowed root を失わない。
- direnv PATH restore に process-global std::env を使わない。
- task discovery の external provider executable lookup は
  `Environment.variable_state.paths` が authority。
- provider subprocess は `Environment::child_process_env()` snapshot だけを受け取り、
  process-global environment を暗黙継承しない。
- task cache は project input metadata だけでなく runtime discovery identity でも scope する。
- `pm status` / `pm activate` の external provider executable lookup、
  特に mise lookup は logical `Environment.variable_state.paths`
  を authority とする。
- project provider subprocess は
  `Environment::child_process_env()` snapshot だけを受け取り、
  process-global environment を暗黙継承しない。
- 一つの provider operation 中では executable lookup / trust /
  provider query / activation で同一 runtime snapshot を使用する。

## Command resolution

- Runtime command search authority is Environment.variable_state.paths.
- A command name containing '/' bypasses PATH search.
- PATH search considers executable files only and preserves PATH order.
- Explicit pathnames are passed toward exec even when non-executable so exec diagnostics remain authoritative.
- command_cache stores successful absolute-PATH resolutions only.
- command-not-found is never cached.
- cached paths are revalidated before use.
- any relative/empty PATH element disables persistent command-location caching.
- assigning PATH invalidates remembered command locations even when the textual value is unchanged.
- command-scoped PATH=... applies to that external command's lookup and child environment only; it never mutates or populates the persistent shell command cache.
