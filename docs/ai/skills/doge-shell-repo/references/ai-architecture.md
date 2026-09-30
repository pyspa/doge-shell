# AI 機能の設計方針 — 入口

製品としての AI 機能（`!` チャット、MCP、ツール、コマンドパレットの AI アクション、`ai-commit`、
`safe-run`、ゴーストテキスト、AI chat hooks、Herdr 連携）の設計正典は `docs/design/ai/` にある。
runtime Skill には入れていない（codex-core profile のコピー対象を小さく保つため）。

最初に `docs/design/ai/README.md`（プロバイダ・エージェントループ・再実装禁止・既定 OFF）を読み、
話題が決まっていればそのファイルだけ開く:

| 話題 | ファイル |
|---|---|
| 安全ゲート（`SafetyGuard` / MCP trust / 承認キー） | `docs/design/ai/safety.md` |
| AI 機能・Herdr の環境変数 | `docs/design/ai/env-vars.md` |
| 製品 Skill（root・trust・lint・staging） | `docs/design/ai/skill.md` |
| AI chat hooks | `docs/design/ai/hooks.md` |
| 対話ジョブ・Tool Search | `docs/design/ai/interactive-jobs.md` |
| Herdr 連携 | `docs/design/ai/herdr.md` |
| 未解決の設計判断 | `docs/design/ai/open-questions.md` |

実装不変条件（事故ったルール）は `invariants/safety.md` が正典で、`docs/design/ai/safety.md` は
製品ポリシー側。重複させない。
