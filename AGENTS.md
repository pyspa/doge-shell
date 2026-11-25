# Agent Guide

日本語で応答する。既存のユーザー変更を保持し、Linux と macOS の両方をサポートする。

## 実装の入口

- 非自明な実装・調査・refactor は `python3 scripts/agent-context.py --topic "<依頼>"` で担当 Skill を探す。既知のパスは `--path <path>` で渡す。typo や変更箇所が明確な単純修正は対象に直接進む。
- 返された Skill と必要な reference だけを読む。振り分け不能なら `docs/ai/skills/doge-shell-repo/references/task-map.md` の該当節を検索する。同一タスクの探索結果はスコープや内容が変わるまで再利用する。
- 編集前に完了条件・変更範囲・検証方法を短く定める。再開時は今回の目標に合う `.agent/TASK.md` を使う。長期作業の手順は `docs/ai/long-task-workflow.md` にある。
- 定義・参照を辿るには利用可能な symbol navigation または `rg` を使う。crate 境界は `Cargo.toml` で確認する。ルート README は公開挙動・設定例の該当節だけ読む。

## 実装と検証

- 構文・実行の仕様は `dsh/src/shell.pest` と `dsh/tests/spec/*.toml`。仕様変更は契約ケースを先に追加する。補完定義は completion-spec、job lifecycle は execution-semantics の Skill と該当 invariant に従う。
- 製品 AI の設計は `docs/design/ai/README.md` が入口。AI、補完、環境、subprocess、wait、ShellProxy の操作は各領域の既存 authority を使う。OS 固有処理は `docs/ai/skills/doge-shell-repo/references/platform-support.md` に従う。
- 検証は `docs/ai/skills/doge-shell-repo/references/test-scope.md` で、変更した挙動を通るテストと必須 checker を選ぶ。対象テストの実行件数・結果・未検証範囲を報告する。`dsh/` の package 名は `doge-shell`。段階的設計変更の最終確認・リリース前は `./scripts/check.sh` を使う。
- 生成物・整形の差分は必要箇所に限る。非テスト Rust ファイルは 400 行超で `//!` module doc を付け、800 行超は分割を検討する。
- 開発用 Skill の正本は `docs/ai/skills/`。常設公開は core 3 件、Codex の既定インストールは doge-shell-repo のみ。繰り返す失敗は該当 Skill・reference と回帰ケースに戻す。ガイド・Skill・installer・アダプタ変更後は `scripts/check-ai-guidance.sh` を実行する。

## コミットと PR

- コミットの author・committer と PR 作成者はすべて `mopemope` とする。コミット前にこの checkout の `.githooks/prepare-commit-msg` を有効にする（標準のフックが未使用なら `git config --local core.hooksPath .githooks`、既存フックがあればそこから呼ぶ）。
- PR 作成は `scripts/agent-create-pr.sh BASE HEAD TITLE BODY_FILE [--draft]` を使う。名義を確認できないときは作成を中止する。別のアカウント・コネクタ・Web UI での再試行やフックの無効化は禁止。
