# Invariants: キー入力・端末描画・テストと実端末・出力履歴

[../invariants.md](../invariants.md) の索引から。短いが破りやすいルール。

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
