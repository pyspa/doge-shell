# CLAUDE.md

このファイルは Claude Code 用の薄いアダプタです。共通ルールは `AGENTS.md` を単一ソースとして import します。内容をここに複製しないこと。

@AGENTS.md

## Claude Code 固有の手順

- リポジトリ作業では最初に Skill `doge-shell-repo` を使い、そこから狭い Skill / `references/` に降りる。`.claude/skills` は core 3件だけを公開し、domain Skill は `scripts/agent-context.py` が返す canonical path を直接読む（追加インストール不要）。
- rust-analyzer Code Intelligenceが使える場合はdefinition/reference/symbol navigationを広いファイル読みより優先する。使えない場合は `rg` を使う。
- `doctor` は CLI サブコマンドではなく shell builtin。`dogesh doctor validate` は失敗する。`./target/release/dogesh -c "doctor validate --json"` と呼ぶ。見ているのは `git status --short` の未コミット分だけ。`target/release/dogesh` が無い・ソースより古いときは使わず `test-scope.md` に従う。
- `target/debug` が無い状態からの最初の cargo コマンドは rusqlite の C ビルドを含むフルビルドになる。既定の 120 秒では足りないので `timeout` を 600000 にし、`cargo check -p <package>` で温めてから test に進む。
- 大きなファイル（特に 1000 行超の `tests.rs`）は全文を読まない。`rg -n 'fn <name>|^mod |^#\[cfg\(test\)\]' <file>` で位置を特定してから offset 指定で読む（詳細は `docs/ai/skills/doge-shell-repo/references/read-boundaries.md`）。
- ログの環境変数は `RUST_LOG` ではなく `DOGESH_LOG`。コミットメッセージは英語の Conventional Commits（チャットは日本語）。
- 安全な read-only コマンドは `.claude/settings.json` で許可済み。破壊的操作・外部送信・commit/push はユーザーの明示指示があるまで行わない。
