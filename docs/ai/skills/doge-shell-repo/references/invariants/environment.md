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
- AI/capture subprocesses must not inherit process-global environment
  implicitly.
- `command |!`, `ShellProxy::capture_command`, and interactive `execute`
  spawn `/bin/sh` with env_clear + Environment::child_process_env().
- `/bin/sh` is the internal interpreter boundary on supported Linux/macOS;
  it is not resolved through process-global or project-controlled PATH.
- persistent agent execution does not receive the shell's full exported
  environment. Its environment is the logical baseline plus explicitly
  granted TaskGrant.environment names.
- TaskGrant.environment lookup must use logical shell variables only.
  A logically unset variable must never be resurrected from std::env.
- agent sandbox runtime discovery (`srt`) uses logical command search paths,
  never process-global PATH.
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
- Prompt external tool probes resolve executables from the logical shell
  PATH snapshot, never process-global PATH.
- Prompt probe subprocesses use absolute resolved executable paths and
  env_clear + Environment::child_process_env().
- A prompt refresh tick uses one immutable runtime snapshot for PATH,
  exported child environment, cwd, and prompt-related logical variables.
- Prompt executable availability must not be stored in process-lifetime
  OnceLock state; PATH is runtime mutable.
- Prompt external-tool caches/backoff are invalidated when logical PATH
  generation changes.
- Prompt probe cache authority is the complete prompt runtime identity:
  logical PATH generation, snapshot cwd, exported child environment, and
  prompt-specific logical variables.
- A prompt probe result or failure may publish only when its launch epoch is
  still the current runtime epoch.
- Prompt runtime epoch changes monotonically whenever runtime identity
  changes, even when state later returns to a previously seen identity
  (ABA protection).
- At most one probe of each kind may be in flight for the same runtime epoch.
- An old probe completion must never clear or mutate a newer epoch's
  in-flight state, cache, or failure backoff.
- PATH assignment remains an explicit rescan boundary even when its textual
  value is unchanged.

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
- 実行時の PATH 変更（`add_path` 等）は logical PATH shell variable を Environment の canonical mutation path（`insert_path_entry` → `set_shell_var("PATH", ...)`）経由で更新する。`variable_state.paths` への直接書き込みは禁止。
- `variable_state.paths` は derived effective lookup projection であり、独立した書き込み可能な PATH ストアではない。読んで新値を組み立てる用途に限る。
- `add_path` 等の PATH helper は既存の export 属性を保持する（`set_and_export_shell_var` を使わない）。
- PATH 変更時の command-location cache 無効化と PATH 由来 completion cache の再活性化は `refresh_derived_state("PATH")` 経由でのみ行う。caller 側で個別に cache を触らない。
