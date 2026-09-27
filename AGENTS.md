# Agent Guide

このリポジトリでは、最小探索・最小検証を徹底する。

## 共通ルール

- チャットは日本語で行う。
- 対応 OS は Linux と macOS。片方だけで動く変更を入れない。
- 既存のユーザー変更を破棄しない。
- `Cargo.toml` でcrate境界を確認し、`rg --files` / `rg -n` で当たりを付けてから狭く読む。
- 該当Skillを先に使い、詳細な `references/` は必要になったときだけ読む。
- 利用可能で信頼できるsymbol navigationを優先する。使えない場合は `rg` とcompiler/test結果を使う。LSPを必須検証にしない。
- `README.md` 全文を最初から読まない。公開挙動・設定例の更新時だけ該当箇所を読む。
- repo-trackedな生成・整形は目的が明確なときだけ行う。
- 400行超の非テストファイルには `//!` module docを付け、800行超は分割を検討する。`scripts/check-file-budget.py` が検査する。

## 編集前のルーティング

- 非自明な実装・調査・refactorでは、まず `python3 scripts/agent-context.py --topic "<task>" --json` を使う。pathが既知なら `--path <repo-relative-path>` を追加できる。
- routerは編集前のSkillとreferenceの入口だけを示す。使えないときは `docs/ai/skills/doge-shell-repo/references/task-map.md` を読む。
- typo、明白な1ファイルdocs編集、変更内容を完全指定された単純修正ではrouterを省略してよい。
- 補完定義は `doge-shell-completion-spec` と `references/invariants/completion.md`、execution/job lifecycleは `doge-shell-execution-semantics` と `references/invariants/execution*.md` を読む。
- 製品側のAI機能では `docs/ai/skills/doge-shell-repo/references/ai-architecture.md` を先に読む。

## 設計境界

- AI機能、completion、environment、runtime subprocess、ShellProxy capabilityの既存authorityを迂回しない。詳細は関連Skill・invariantと `scripts/check-runtime-authority.py`、`scripts/check-shell-proxy-capabilities.py` にある。
- executionのwait/ownership境界は `scripts/check-execution-authority.py` が検査する。
- OS固有の変更は `docs/ai/skills/doge-shell-repo/references/platform-support.md` を読み、Linux/macOS両方の実装と `scripts/check-portability.py` を確認する。

## 編集後の検証

- `doctor validate` が使える環境では、変更ファイルに応じた提案を優先する。routerを検証コマンドのauthorityにしない。
- 使えない場合は `docs/ai/skills/doge-shell-repo/references/test-scope.md` から最小コマンドを選ぶ。
- `dsh/` はCargo package `doge-shell`、`dsh-builtin/` は `dsh-builtin`。複数crateに跨るときだけ広いtestを選ぶ。
- AI guidance / Skill変更では `scripts/check-ai-guidance.sh` を実行する。runtime Skillの `--check-installed` はコピーの新旧を検査し、`--status` は表示専用。
- OS依存コード・設定では `scripts/check-portability.py`、ShellProxy・能力trait変更では `scripts/check-shell-proxy-capabilities.py` を実行する。
- `Cargo.toml` / `README.md` / `LICENSE` 変更では `scripts/check-project-consistency.py` を実行する。
- 全体の `./scripts/check.sh` は段階的な設計変更の最後とリリース前に実行する。
- 長時間タスクで `.agent/TASK.md` が存在すれば再開前に読み、Current state / Decisions / Blockersを短く保つ。手順は `docs/ai/herdr-development.md` を参照する。
- 失敗例の再発防止は `task-map.md` または該当Skillのreferenceへ短く戻す。

## Skillの配置

- canonical sourceは `docs/ai/skills/`。Codex runtimeには原則 `doge-shell-repo` だけを常設し、領域別Skillは必要時に読む。
- `.agents/skills` と `.claude/skills` はcore 3件だけをcanonical directoryへのsymlinkとして公開する。domain Skillはrouterが返す `skills[].path` を直接読む。
- `AGENTS.md` / `CLAUDE.md` / `docs/ai/` / Skill / installer / `.claude/` を変えたら `scripts/check-ai-guidance.sh` を実行する。
