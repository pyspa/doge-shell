# AI / Skill 運用メモ

このディレクトリは、このリポジトリでの AI 利用時の token 消費を減らすための運用情報をまとめる。

## 目的
- 常時読む文書を短くする。
- 詳細は必要時だけ読む。
- repo 固有知識を Skill と reference に分離する。

## 配置
- canonical Skill source: `docs/ai/skills/`
- Codex runtime skills: `~/.codex/skills/`
- doge-shell runtime skills: `~/.config/dsh/skills/`

## 使い分け
- `AGENTS.md`: この repo で最初に守る短いルールだけを書く。
- `SKILL.md`: 別エージェントが作業を始めるための最短手順だけを書く。
- `references/`: 長い説明、モジュール一覧、チェックリストを置く。

## 導入
- sample Skill の配置には `scripts/install-runtime-skills.sh` を使う。
- `both` を指定すると Codex と doge-shell の両方へ入れる。
- 普段は `--list` と `--dry-run` で対象を確認してから、必要な Skill だけ入れる。

```bash
scripts/install-runtime-skills.sh --list
scripts/install-runtime-skills.sh --dry-run --target codex doge-shell-repo doge-shell-validation doge-shell-investigation doge-shell-chat-tools
scripts/install-runtime-skills.sh --target codex doge-shell-repo doge-shell-validation doge-shell-investigation doge-shell-chat-tools
```

## authoring ルール
- trigger 条件は frontmatter の `description` に集約する。
- `SKILL.md` 本文には長い「when to use」を書かない。
- バリエーションごとの詳細は `references/` に逃がす。
- shell / Rust / reference で済むなら、新しい長文ドキュメントを増やさない。
- 変更後は `scripts/check-ai-guidance.sh` で軽量 lint する。

## 推奨 runtime Skill
- 常用: `doge-shell-repo`, `doge-shell-validation`, `doge-shell-investigation`, `doge-shell-chat-tools`
- 領域別: `doge-shell-parser-shell`, `doge-shell-process-pty`, `doge-shell-repl-completion`, `doge-shell-prompt-terminal-ui`, `doge-shell-env-startup`, `doge-shell-history-frecency`, `doge-shell-safety-policy`
