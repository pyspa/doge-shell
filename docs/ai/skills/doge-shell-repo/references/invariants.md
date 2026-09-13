# Invariants — 索引

短いが破りやすいルール。いずれも実際に事故った箇所だけを載せる。話題別に分割してある。
**必要な話題のファイルだけ**開く。

| 話題 | ファイル |
|---|---|
| ディレクトリ変更・`Environment` の状態 | [invariants/environment.md](invariants/environment.md) |
| キー入力・端末描画・テストと実端末・出力履歴 | [invariants/terminal.md](invariants/terminal.md) |
| cron | [invariants/cron.md](invariants/cron.md) |
| Completion 定義 | [invariants/completion.md](invariants/completion.md) |
| プラットフォーム | [invariants/platform.md](invariants/platform.md) |
| 安全判定 | [invariants/safety.md](invariants/safety.md) |
| 二重化しているもの（多数派が正解とは限らない） | [invariants/duplication.md](invariants/duplication.md) |

cwd 変更、`Environment` の状態、キー入力、端末描画、出力履歴、cron を触る前に該当ファイルを読む。
