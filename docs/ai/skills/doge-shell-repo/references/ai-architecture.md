# AI 機能の設計方針 — 索引

doge-shell が**製品として持つ** AI 機能の方針。`docs/ai/` の他の文書は「この repo を AI に
編集させるときの運用ルール」で、別物。ここは実装の話をする。

新しい AI 機能を足すとき、既存の AI 機能を直すときに最初に読む。

内容は話題別に `references/ai/` へ分割してある。**必要な話題のファイルだけ**開く
（全部読む必要はない — それぞれ独立して読める）。

| 話題 | ファイル | 内容 |
|---|---|---|
| 概論・プロバイダ・エージェントループ・再実装禁止・既定 OFF | [ai/README.md](ai/README.md) | §1-3, §5 |
| 安全ゲート（`SafetyGuard`） | [ai/safety.md](ai/safety.md) | §4 |
| 環境変数の正典 | [ai/env-vars.md](ai/env-vars.md) | §6 |
| Skill | [ai/skill.md](ai/skill.md) | §7 |
| AI chat hooks | [ai/hooks.md](ai/hooks.md) | §8 |
| 未解決の設計判断 | [ai/open-questions.md](ai/open-questions.md) | §9 |
| Herdr 連携・永続タスク (`agent`) | [ai/herdr.md](ai/herdr.md) | §10 |

新しい AI 機能を足す・既存の AI 機能を直すタスクでは、まず [ai/README.md](ai/README.md) を
読み、関係する話題のファイルだけ追加で開く。
