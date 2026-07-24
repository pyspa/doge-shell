# CLAUDE.md

このファイルは Claude Code 用の薄いアダプタです。共通ルールは `AGENTS.md` を単一ソースとして import します。内容をここに複製しないこと。

@AGENTS.md

## Claude Code 固有の手順

- リポジトリ作業では最初に Skill `doge-shell-repo` を使い、そこから狭い Skill / `references/` に降りる。canonical source は `docs/ai/skills/`。
- Skill が未導入なら `scripts/install-runtime-skills.sh --target claude --profile claude-common` で `~/.claude/skills/` に導入する。未導入のまま進める場合は repo-local の `docs/ai/skills/<skill>/SKILL.md` を直接読む。
- 検証は可能なら `doctor validate` の提案を優先し、なければ `docs/ai/skills/doge-shell-repo/references/test-scope.md` で最小コマンドを選ぶ。`dsh/` の Cargo package 名は `doge-shell`（`-p dsh` は存在しない）。
- 安全な read-only コマンドは `.claude/settings.json` で許可済み。破壊的操作・外部送信・commit/push はユーザーの明示指示があるまで行わない。
- `AGENTS.md` / `docs/ai/` / Skill / installer を変更したら `scripts/check-ai-guidance.sh` を実行する。
