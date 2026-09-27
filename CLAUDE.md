# CLAUDE.md

このファイルは Claude Code 用の薄いアダプタです。共通ルールは `AGENTS.md` を単一ソースとして import します。内容をここに複製しないこと。

@AGENTS.md

## Claude Code 固有の手順

- リポジトリ作業では最初に Skill `doge-shell-repo` を使い、そこから狭い Skill / `references/` に降りる。canonical source は `docs/ai/skills/`。
- 非自明な作業は `scripts/agent-context.py` で編集前のSkillを選ぶ。編集後の検証選定は `doctor validate` に従う。
- rust-analyzer Code Intelligenceが使える場合はdefinition/reference/symbol navigationを広いファイル読みより優先する。使えない場合は `rg` を使い、LSPの有無をbuild/testの条件にしない。
- `.claude/skills` はcore 3件だけを公開する。domain Skillは `scripts/agent-context.py` が返すcanonical pathを直接読む。project Skillの追加インストールは不要。
- 検証は可能なら `doctor validate` の提案を優先し、なければ `docs/ai/skills/doge-shell-repo/references/test-scope.md` で最小コマンドを選ぶ。`dsh/` の Cargo package 名は `doge-shell`（`-p dogesh` は存在しない）。
- `doctor` は CLI サブコマンドではなく shell builtin。`dogesh doctor validate` は失敗する。`./target/release/dogesh -c "doctor validate --json"` と呼ぶ。見ているのは `git status --short` の未コミット分だけで、release バイナリはソースより古いことがある。
- `target/debug` が無い状態からの最初の cargo コマンドは rusqlite の C ビルドを含むフルビルドになる。既定の 120 秒では足りないので `timeout` を 600000 にし、`cargo check -p <package>` で温めてから test に進む。
- 1000 行超のファイルは先に `grep -n '^#\[cfg(test)\]' <file>` を打ってから offset 指定で読む（詳細は `docs/ai/skills/doge-shell-repo/references/read-boundaries.md`）。全文を読まない。
- ログの環境変数は `RUST_LOG` ではなく `DOGESH_LOG`。コミットメッセージは英語の Conventional Commits（チャットは日本語）。
- 安全な read-only コマンドは `.claude/settings.json` で許可済み。破壊的操作・外部送信・commit/push はユーザーの明示指示があるまで行わない。
- `AGENTS.md` / `CLAUDE.md` / `docs/ai/` / Skill / installer / `.claude/` を変更したら `scripts/check-ai-guidance.sh` を実行する。
