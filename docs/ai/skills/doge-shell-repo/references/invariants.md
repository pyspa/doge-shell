# Invariants

短いが破りやすいルール。いずれも実際に事故った箇所だけを載せる。

## ディレクトリ変更
- cwd を変えるのは `ShellProxy::changepwd`（`dsh/src/proxy/mod.rs`）だけ。`std::env::set_current_dir` を直接呼ぶと `OLDPWD`・`path_history`（`z`）・`*on-chdir-hooks*`・`dir_stack[0]` が全部ずれる。
- `changepwd` は **chdir してから** hook / direnv で失敗しうる。`Err` を「何も起きなかった」と扱わないこと。呼び出し側は `get_current_dir()` と突き合わせて判定する（`dsh-builtin/src/dirstack.rs` の `apply` / `push_directory` が実例）。
- `dir_stack[0]` は常に現在ディレクトリ。`dirs -v` の番号と `cd -N` はこの前提で一致している。

## Environment の状態
- `EnvironmentSnapshot`（`dsh/src/lisp/mod.rs`）は config.lisp 失敗時のロールバック対象。**設定**（`keybindings`, `alias`, `abbreviations` …）は追加する。**ランタイム状態**（`dir_stack`, `scheduler`）は追加しない。
- `config.lisp` は `Repl::new` より前に走る。REPL 起動前に登録できる必要があるものは `Environment` に置く。

## キー入力
- キーを消費しないこと。未定義チョード（`Ctrl-x q`）は prefix を捨てて、**終端キーを通常ディスパッチする**。飲み込むと誤爆 `Ctrl-x` の直後の Enter でコマンドが黙って実行されなくなる。
- `determine_key_action`（`dsh/src/repl/key_action.rs`）は純粋関数のまま維持する。上書きは `handler.rs` の前段レイヤーで行う。
- `KeyAction` に variant を足したら `keybind/action_name.rs` の `ACTIONS` にも足す（網羅テストが落ちる）。

## 端末描画
- `print_prompt` は**新しいプロンプト**（OSC 133 A + pre-prompt hooks）。同じ行を描き直すだけなら `render::redraw_prompt` を使う。取り違えると OSC 133 A が D と対にならず、shell integration 対応端末で偽のコマンドブロックが開く。
- DECSTBM のスクロール領域は **スクロールしか** 守らない。`Clear(ClearType::FromCursorDown)` / `Clear(All)` は予約行も消すので、消したら `StatusLine::invalidate()` を呼ぶ（差分描画がスキップして空行のまま残る）。
- 画面を占有するもの（picker、補完グリッド、外部エディタ、前景コマンド）は `StatusLinePause` で囲む。子プロセスが実行され**終わる**まで pause を保持すること。
- プロンプト上への割り込み出力は `render::print_above_prompt` を通す。

## テストと実端末
- テストは**開発者の実端末を触ってはいけない**。`cargo test` の test binary は fd 0 にユーザーの tty をそのまま継承するので、そこへの書き込み・termios 変更・`tcsetpgrp` はすべて開発者の端末に届く。しかも test binary は終了時に何も戻さない。
- 実端末を変更するコードは `crate::terminal::terminal_control_enabled()` を通す（unit test ビルドでは常に false）。`flush_stdout_bytes` / `Drop for Repl` / `job_wait` の `owns_terminal` / PTY input proxy の stdin フォールバックが実例。
- `ctx.interactive` はガードにならない。`Context::new_safe` はこれを `isatty(STDIN_FILENO)` から作るので、端末から `cargo test` すると **true になる**。テストで `Context::new_safe(.., true)` を使うなら `ctx.interactive = false` を明示する（`dsh/src/shell/mod.rs` の job テストが手本）。
- 端末へ書く型は writer を引数に取る。`StatusLinePause::with_writer` がその形。`std::io::stdout()` をハードコードすると、テストが DECSTBM のスクロール領域（`ESC[1;Nr`）を開発者の端末に置き去りにする。**実際に起きた**: エコーが消え Enter が効かなくなるが、termios は正常なので `stty` では気付けない。
- PTY が要るテストは自前の pty slave を開く（`dsh/src/process/async_io.rs` のテストと `setup_pty_with` が手本）。`AsyncStdin::open_tty()` は `ttyname(0)` を解決するので実端末を読む。
- 端末汚染の検知は「escape sequence が実 stdout に出たか」で見る。termios 差分だけでは DECSTBM を取りこぼす。

## 出力履歴
- `OutputHistory` は `push_front`。**先頭が最新**。直前に push したものは `get(1)`、`.last()` は最古。
- `CommandBlock.command` は `blocks rerun` がそのまま評価器へ渡す。ラベルや接頭辞を混ぜない。表示用の加工は `OutputEntry.command`（`out` / `tm` が表示するだけ）に置く。

## スケジューラ
- `Shell` は `!Send`（`Rc<RefCell<LispEngine>>`）。spawn したタスクから `eval_str` は呼べない。
- `handle_background_tick` は `tokio::select!` の腕の中で await される。ここで重い処理をするとキー入力が固まる。
- 履歴同期の SQLite 読み込み・正規化と command timing の JSON 書き込みを REPL イベントループへ戻さない。`repl/background_io.rs` で予約し、完了後は世代を確認してメモリ上の snapshot だけを適用する。
- command history の reload は SQLite 保存未確認のローカル差分を snapshot へマージする。command timing は一時ファイルから atomic に公開し、`timing --clear` 後の古い snapshot を reset 世代で拒否する。
- 非ブロッキング性は時間閾値ではなく `repl::background_io::tests` の停止ワーカー・in-flight・完了 channel で検証する。世代競合は `command_timing::tests` と `history::*::background_reload_tests` が入口。
- REPL 終了時は raw mode と DECSTBM を解除してから background writer の完了を待つ。ファイル I/O 待機中に端末状態を保持しない。
- pause から resume するときは `next_run` を貼り直す。しないと溜まった分が一斉に発火する。

## Completion 定義
- 埋め込み元は `completions/` ただ 1 つ（`dsh/src/completion/json_loader.rs` の `#[folder = "../completions/"]`）。ディレクトリを増やさない。以前は `dsh/completions/` との二重管理で、root にだけ足した 4 ファイルが出荷バイナリに載っていなかった。
- provider 名のタイポは**どのテストも落ちない**。loader は `provider` をただの `String` として通し、`DynamicProviderId::parse` が `None` を返して候補が静かに 0 件になるだけ。`json_loader.rs` の `embedded_completion_definitions_are_valid` が唯一の防波堤なので、ここを弱めない。
- `ArgumentType` は `#[serde(tag = "type", content = "data")]`。`Choice` は newtype なので `data` は**文字列の配列**（`{"type":"Choice","data":["a","b"]}`）。`{"choices": [...]}` のようなオブジェクトで包むと deserialize が `invalid type: map, expected a sequence` で落ち、その定義だけ丸ごと無効になる。
- 新しい dynamic provider は 3 箇所を同時に更新する。`dsh-types/src/completion.rs` の `DYNAMIC_COMPLETION_PROVIDERS`（**ソート済み**・`binary_search` 前提）、`command-completion-schema.json` の Dynamic Type enum（**完全一致**で比較される）、`dsh/src/completion/dynamic/registry.rs` の `family_for` + family collector。検証は `cargo test -p dsh-types` と `cargo test -p doge-shell` の両方。
- `family_for`（`registry.rs`）の else は無条件 `External`。プレフィクスを足し忘れても「未登録」とは言われない。`dynamic/git.rs` の `_ =>` は `platform::collect` に落ちるので、「match アームが無い = 未対応」でもない。
- 動的補完には経路が 2 つある。宣言的 provider（JSON の `Dynamic`）と、コマンド名直結の `DYNAMIC_PROVIDER_SPECS`（`completion/integrated.rs`）。後者が先に走り、結果に前者が `extend` される。片方だけ直すと候補が重複するか、直したはずが効かない。
- JSON を**新規追加**しただけでは release ビルドが再実行されない（rust-embed は `include_bytes!` でファイル単位に依存を張るのでディレクトリの変化を追わない）。出荷前に `touch dsh/src/completion/json_loader.rs`。`output-schemas/` も同じ仕組み（`dsh/src/output_schema/loader.rs`）なので、スキーマ追加時は同様に loader を touch する。
- `completions/` はクレートディレクトリの外なので `cargo package -p doge-shell` には入らない。path 依存があり現状 publish できないため実害は無いが、crates.io 公開が必要になったら `dsh/` 配下へ戻す。

## プラットフォーム
- `nix` の Linux 拡張を使わない。`pipe2` は nix が macOS に出しておらず、これで `doge-shell` crate 全体がコンパイル不能だった（`a846e3c`）。cloexec な pipe は `std::io::pipe` が両 OS でくれる。
- `rustflags` は `[target.'cfg(target_os = "linux")']` の下に置く。`[build]` に書くと macOS の clang が `-fuse-ld=mold` を `invalid linker name` で拒否し、**リンクが全部落ちる**（`f866418`）。
- `/bin/true` と `/bin/false` は macOS に無い（`/usr/bin` にしかない）。テストからの絶対パス起動は `dsh/tests/common/mod.rs` の `true_path()` / `false_path()` / `first_existing()` を通す。`/etc/hostname` も macOS に無いので `/etc/hosts` を使う。`/tmp` は macOS で `/private/tmp` に解決されるので `canonicalize` して比べる（`ae2f192`）。
- macOS の `/etc/passwd` は**実在するのに実質空**。単一ユーザーモード用で、対話ユーザーは Open Directory にいる。ファイルの有無で分岐すると「読めたのに `root` しか出ない」になる（`f45fcc2`）。`/etc/group` は逆に macOS でも埋まっているが、Open Directory が足すグループは持たない。
- シグナル番号は 1-15 しか共通でない。`SIGUSR1` は Linux 10 / macOS 30、`SIGCHLD` は 17 / 20。番号表を共有せず per-OS に持ち、`libc` と突き合わせるテストを付ける（`59855e1`、`generators/signal.rs`）。
- `dirs::config_dir()` と `xdg::BaseDirectories` は Linux で同じ、macOS で**別のディレクトリ**（前者は `~/Library/Application Support`）。混ぜると installer が書いた場所を loader が読まない。実際に runtime skill が macOS でエージェントから見えなかった。config パスは `dsh-builtin/src/config_paths.rs`（`dsh` crate 内は `environment::get_config_file`）を通す。`scripts/check-portability.py` が直接呼び出しを禁止している。
- 片肺の `#[cfg]` は**何も落とさない**。もう一方の OS でその項目が存在しなくなるだけで、コンパイルもテストも通る。`scripts/check-portability.py` がファイル単位で見るのが唯一の自動防波堤で、関数単位は CI の macos ジョブが担う。

## 安全判定
- `SafetyGuard::check_jobs` は `Job.cmd`（**行全体。パイプラインもまとめて 1 本の文字列**）を見る。先頭トークンだけを分類すると `true | rm -rf ~` は `true`、`sudo rm -rf ~` は `sudo` になり、どちらもルールが無いので**全チェックを素通りする**。オペレータで区切り、ラッパーを覗いてから分類すること（`dsh_types::safety_policy::{split_command_segments, command_candidates}`）。
- 行の分割は**生文字列**に対してやる。`shell_words::split` は空白でしか切らないので `echo hi; rm -rf ~` は `["echo", "hi;", "rm", ...]` になり、トークン単位の分割では `;` が見えない。
- ラッパーのオプションは値を取る（`timeout 5 ...`、`nice -n 10 ...`、`chroot /new ...`）。「最初の非オプション引数が中身のコマンド」は**その値を拾う**。`command_candidates` は残りの非オプショントークンを全部候補にして fail-safe に倒している。
- **判定した行と実行する行を一致させる**。`sh -c` は行全体を実行するのに、dsh の文法は grouping・制御構文・heredoc を持たない。`get_jobs` は未消費の末尾を**警告するだけ**なので、安全判定側は `unconsumed_tail` と `compound_statement_keyword` で fail closed にする。`{ rm -rf ~; }` は `{` という名前のコマンドとして完全にパースされてしまう。
- コマンド置換（`` ` ``、`$(...)`、`<(...)`、`(...)`）は**判定より前に拒否する**。`shell::parse::parse_command` が評価するので、「安全か」を尋ねること自体が実行になる。
- 文字列をコードとして渡す経路は flag だけではない。stdin から読むシェル（`printf ... | sh`）と `eval` は flag を持たない（`execute.rs` の `hidden_code_source`）。

## 二重化しているもの（多数派が正解とは限らない）
- builtin の能力 trait は `dsh-builtin/src/shell_capabilities.rs` が**正**（`scripts/check-shell-proxy-capabilities.py` の検査対象）。`dsh-builtin/src/capability.rs` は旧世代で、利用ファイル数だけは多い。新しい依存は前者へ足す。
- **AI 機能の方針**は `ai-architecture.md` が正。エージェントループは 2 つだけ、共有方針は `dsh-openai/src/turn.rs`、安全ゲートは `SafetyGuard` 1 つ。新しい AI 経路を足す前にそこを読む。
- `SafetyLevel` は `dsh-types/src/safety_policy.rs` が**正**。`dsh/src/safety/mod.rs` はそこを re-export しているだけ。以前は 2 つの enum があり、値の読み先も 2 つ（`SAFETY_LEVEL` 変数と `policy_state.safety_level`）だったので、`(safety-level ...)` の二重書きだけが同期を保っていた。**単一ソースは `policy_state.safety_level`**、変数は表示用のコピー。
- `McpManager` の実体は `Environment.integration_state.mcp_manager` ただ 1 つ。以前 `!` チャットだけが自前の 2 個目を作って 300 秒キャッシュしていたので、`mcp connect` / `mcp disconnect` がチャットに効かず `mcp status` の表示と食い違った。builtin からは `AgentCommandPolicy::agent_mcp_manager` で受け取る。
- `dsh` と `dsh-builtin` は互いに依存できないので、両方で要る純粋なテキスト処理は `dsh-types` に置く。ANSI ストリップは `dsh-types/src/ansi.rs`、`{{name:default}}` の走査は `dsh-types/src/placeholder.rs` が**正**。以前これを各クレートで書いていて、名前検証のある側と無い側に分かれ、`docker ps --format '{{json .}}'` が片方だけ壊れた。コピーを作らない。
