# CLAUDE.md

このファイルは Claude Code 用の薄いアダプタです。共通ルールは `AGENTS.md` を単一ソースとして import します。内容をここに複製しないこと。

@AGENTS.md

## Claude Code 固有の手順

- リポジトリ作業では最初に Skill `doge-shell-repo` を使い、そこから狭い Skill / `references/` に降りる。canonical source は `docs/ai/skills/`。
- Skill は `.claude/skills` -> `../docs/ai/skills` の symlink で全件がプロジェクトスコープに入る。追加導入は不要。
  symlink が辿られず Skill 一覧に出ない場合だけ `scripts/install-runtime-skills.sh --target claude-project` で実体をコピーする（ユーザー全体に入れるなら `--target claude`。`--profile` を付けると 4 個に絞られるので付けない）。
- 検証は可能なら `doctor validate` の提案を優先し、なければ `docs/ai/skills/doge-shell-repo/references/test-scope.md` で最小コマンドを選ぶ。`dsh/` の Cargo package 名は `doge-shell`（`-p dsh` は存在しない）。
- `doctor` は CLI サブコマンドではなく shell builtin。`dsh doctor validate` は失敗する。`./target/release/dsh -c "doctor validate --json"` と呼ぶ。見ているのは `git status --short` の未コミット分だけで、release バイナリはソースより古いことがある。
- `target/debug` が無い状態からの最初の cargo コマンドは rusqlite の C ビルドを含むフルビルドになる。既定の 120 秒では足りないので `timeout` を 600000 にし、`cargo check -p <package>` で温めてから test に進む。
- 1000 行超のファイルは先に `grep -n '^#\[cfg(test)\]' <file>` を打ってから offset 指定で読む（`references/read-boundaries.md`）。全文を読まない。
- ログの環境変数は `RUST_LOG` ではなく `DSH_LOG`。コミットメッセージは英語の Conventional Commits（チャットは日本語）。
- 安全な read-only コマンドは `.claude/settings.json` で許可済み。破壊的操作・外部送信・commit/push はユーザーの明示指示があるまで行わない。
- `AGENTS.md` / `CLAUDE.md` / `docs/ai/` / Skill / installer / `.claude/` を変更したら `scripts/check-ai-guidance.sh` を実行する。
