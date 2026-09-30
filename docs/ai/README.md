# AI / Skill 運用メモ

このディレクトリは、このリポジトリでの AI 利用時の token 消費を減らすための運用情報をまとめる。

## 目的
- 常時読む文書を短くする。
- 詳細は必要時だけ読む。
- repo 固有知識を Skill と reference に分離する。

## 配置
- canonical Skill source: `docs/ai/skills/`
- Codex runtime skills: `~/.codex/skills/`
- doge-shell runtime skills: `~/.config/dogesh/skills/`
- Claude Code runtime skills: `~/.claude/skills/` (`CLAUDE_CONFIG_DIR` で上書き可)
- Claude Code project skills: `<repo>/.claude/skills/` (core 3件だけのcanonical symlink)
- doge-shell project skills: `<project>/.dogesh/skills/` (dogesh の `!` チャットだけが読む。installer の対象外で、リポジトリが自分で持つ)
- cross-agent project skills: `<repo>/.agents/skills/` (core 3件だけのcanonical symlink。installer の対象外)

## 使い分け
- `AGENTS.md`: この repo で最初に守る短いルールだけを書く。`CLAUDE.md` は `@AGENTS.md` を import するだけの薄いアダプタにし、内容は複製しない。
- `SKILL.md`: 別エージェントが作業を始めるための最短手順だけを書く。
- `references/`: 長い説明、モジュール一覧、チェックリストを置く。

## 導入
- sample Skill の配置には `scripts/install-runtime-skills.sh` を使う。
- `both` を指定すると Codex と doge-shell の両方へ入れる。
- 普段は `--list` / `--dry-run` / `--status` で対象を確認してから、必要な Skill だけ入れる。
- Codex runtime は原則 `--profile codex-core` で `doge-shell-repo` だけ入れ、領域別 Skill は repo-local source を必要時に読む。
- Skill を更新したら `--status` で状態を表示し、`--check-installed` を整合性ゲートに使う。stale なら同じ profile を再インストールする。`doctor skills` でも Codex/dogesh runtime の stale/missing を確認できる。

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
- `description` は dogesh のプロンプトでは 240 字で切られる（`skill_manage` が書き込める上限は 300 字）。trigger を先頭に置く。
- `allowed-tools` などの他ツール向け frontmatter キーは dogesh では無視される（強制しない）。
- `skill_manage` が書く内容の lint（拒否条件の一覧）は `docs/design/ai/skill.md` が正典。既存の canonical skill はこの検査の corpus テストを兼ねる（`cargo test -p dsh-builtin the_repositorys_own_skills_pass_the_lint`）。
- 変更後は `scripts/check-ai-guidance.sh` で軽量 lint する。

## 推奨 runtime Skill
- Codex 最小: `--profile codex-core` (`doge-shell-repo`)
- Codex よく使う構成: `--profile codex-common` (`doge-shell-repo`, `doge-shell-validation`, `doge-shell-investigation`, `doge-shell-chat-tools`)
- dogesh runtime 用: `--profile dogesh-common`
- Claude Code 用: `.claude/skills` はcore 3件だけ。領域別Skillは `scripts/agent-context.py` の `skills[].path` から直接読む。global installationは通常不要。`--profile claude-common` はglobal fallback用。
- 領域別: `doge-shell-parser-shell`, `doge-shell-process-pty`, `doge-shell-execution-semantics`, `doge-shell-repl-completion`, `doge-shell-completion-spec`, `doge-shell-prompt-terminal-ui`, `doge-shell-env-startup`, `doge-shell-lisp-config`, `doge-shell-history-frecency`, `doge-shell-command-palette-ai`, `doge-shell-builtin-commands`, `doge-shell-serve-web`, `doge-shell-notebook-markdown`, `doge-shell-safety-policy`
- Skill 自体を書き足すとき (製品利用者向けにも公開): `dsh-skill-authoring`
- 製品利用者向け: `--profile dogesh-user` (`dsh-cron`, `dsh-skill-authoring`, `dsh-chat`)

編集前の案内は `python3 scripts/agent-context.py --topic "<task>" --json`。`--path` は複数指定でき、`--changed` は作業ツリーの変更pathを使う。routing metadataは `agent-routing.json`、回帰ケースは `evals/routing-cases.json`。編集後の検証コマンドは `docs/ai/skills/doge-shell-repo/references/test-scope.md` で選ぶ（release バイナリが新しければ `doctor validate` の提案も使える）。Claude Codeでrust-analyzer Code Intelligenceが利用可能ならsymbol navigationを優先し、利用できなければ `rg` を使う。LSPはbuild/testの必須条件ではない。

## 製品利用者向け Skill

このディレクトリの Skill は原則「この repo を AI に編集させるための運用ルール」で、`doge-shell-*` は repo 開発者専用。`dsh-*` の 3 件だけが例外で、**doge-shell 製品自体の利用者**向け: `dsh-cron` は `!` チャットから cron ジョブを扱うための Skill、`dsh-skill-authoring` は利用者が自分用の Skill を書き足すための Skill、`dsh-chat` は `!` チャット自体の日常利用（モデル・メンション・MCP・承認・会話）のための Skill。開発チェックアウトの `--profile dogesh-common` には含めない（無関係な利用者は repo を開発しないし、repo 開発者の毎ターンのプロンプトに無関係な description を乗せたくない）。配布は `--target dogesh --profile dogesh-user` で明示的に行う。命名規則として、製品利用者向けは `dsh-*`、repo 開発者専用は `doge-shell-*` を使う。

## 製品側の AI 機能

doge-shell が製品として持つ AI 機能の設計文書は `docs/design/ai/` に置く（runtime Skill には入れない）。

## その他の文書

- `agent-adapters.md`: Codex / Claude Code / OpenCode ごとの対応表。
- `long-task-workflow.md`: 長時間タスクの `.agent/TASK.md` と独立レビューの手順。
