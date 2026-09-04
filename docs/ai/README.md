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
- Claude Code runtime skills: `~/.claude/skills/` (`CLAUDE_CONFIG_DIR` で上書き可)
- Claude Code project skills: `<repo>/.claude/skills/` (`../docs/ai/skills` への symlink。全 Skill がそのまま見える)

## 使い分け
- `AGENTS.md`: この repo で最初に守る短いルールだけを書く。`CLAUDE.md` は `@AGENTS.md` を import するだけの薄いアダプタにし、内容は複製しない。
- `SKILL.md`: 別エージェントが作業を始めるための最短手順だけを書く。
- `references/`: 長い説明、モジュール一覧、チェックリストを置く。

## 導入
- sample Skill の配置には `scripts/install-runtime-skills.sh` を使う。
- `both` を指定すると Codex と doge-shell の両方へ入れる。
- 普段は `--list` / `--dry-run` / `--status` で対象を確認してから、必要な Skill だけ入れる。
- Codex runtime は原則 `--profile codex-core` で `doge-shell-repo` だけ入れ、領域別 Skill は repo-local source を必要時に読む。
- Skill を更新したら `--status` で状態を表示し、`--check-installed` を整合性ゲートに使う。stale なら同じ profile を再インストールする。`doctor skills` でも Codex/dsh runtime の stale/missing を確認できる。

```bash
scripts/install-runtime-skills.sh --list
scripts/install-runtime-skills.sh --dry-run --target codex --profile codex-core
scripts/install-runtime-skills.sh --status --target codex --profile codex-core
scripts/install-runtime-skills.sh --check-installed --target codex --profile codex-core
scripts/install-runtime-skills.sh --target codex --profile codex-core
doctor skills
```

`--status` は人が状態を見るための表示で、missing/stale でも終了コードは 0。
自動検査では `--check-installed` を使い、完全一致しない場合を失敗にする。

## authoring ルール
- trigger 条件は frontmatter の `description` に集約する。
- `SKILL.md` 本文には長い「when to use」を書かない。
- バリエーションごとの詳細は `references/` に逃がす。
- shell / Rust / reference で済むなら、新しい長文ドキュメントを増やさない。
- 失敗しやすい実装パターンを見つけたら、`task-map.md` か該当 Skill の `references/` へ短く戻す。
- 変更後は `scripts/check-ai-guidance.sh` で軽量 lint する。

## 推奨 runtime Skill
- Codex 最小: `--profile codex-core` (`doge-shell-repo`)
- Codex よく使う構成: `--profile codex-common` (`doge-shell-repo`, `doge-shell-validation`, `doge-shell-investigation`, `doge-shell-chat-tools`)
- dsh runtime 用: `--profile dsh-common`
- Claude Code 用: プロジェクト内では `.claude/skills` の symlink で全件が入るので導入不要。
  symlink が使えない環境だけ `--target claude-project`（リポジトリ内へコピー）か `--target claude`（`~/.claude/skills/` へコピー）を使う。
  `--profile claude-common` を付けると 4 個に絞られ、SKILL.md 間の相対リンクが切れるので通常は付けない。
- 領域別: `doge-shell-parser-shell`, `doge-shell-process-pty`, `doge-shell-repl-completion`, `doge-shell-completion-spec`, `doge-shell-prompt-terminal-ui`, `doge-shell-env-startup`, `doge-shell-lisp-config`, `doge-shell-history-frecency`, `doge-shell-command-palette-ai`, `doge-shell-builtin-commands`, `doge-shell-serve-web`, `doge-shell-notebook-markdown`, `doge-shell-safety-policy`
- Skill 自体を書き足すとき: `dsh-skill-authoring`

## 製品側の AI 機能

このディレクトリは「この repo を AI に編集させるときの運用ルール」で、doge-shell が製品として
持つ AI 機能（`!` チャット、MCP、ツール、コマンドパレットの AI アクション、`ai-commit`、
`safe-run`、ゴーストテキスト）の設計文書ではない。そちらの方針は
`docs/ai/skills/doge-shell-repo/references/ai-architecture.md` に置く。
