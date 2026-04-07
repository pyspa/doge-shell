# Agent Guide

このリポジトリでは、トークン消費を抑えるために「最小探索・最小検証」を徹底すること。

## 基本方針
- チャットは日本語で行う。
- Python 実行は禁止。補助スクリプトは shell を使う。
- まず `rg --files` / `rg -n` で当たりを付け、必要なファイルだけ読む。
- `README.md` 全文を最初から読まない。ユーザー向け挙動、設定例、公開文書の更新時だけ必要箇所を開く。
- 変更後は関係する最小コマンドで検証し、無関係なワークスペース全体テストは最後に限定する。

## 探索順
1. `Cargo.toml` でクレート境界を確認する。
2. `rg -n "<symbol>|<feature>" dsh dsh-builtin dsh-openai dsh-types` で実装位置を絞る。
3. 迷ったら `docs/ai/skills/doge-shell-repo/references/module-map.md` を読む。

## 主要モジュール
- シェル本体: `dsh/src`
- REPL / 入力: `dsh/src/repl`, `dsh/src/input`
- パーサー: `dsh/src/parser`
- 補完: `dsh/src/completion`, `dsh/src/repl/completion`
- Lisp: `dsh/src/lisp`
- prompt / UI: `dsh/src/prompt`, `dsh/src/terminal`
- safety: `dsh/src/safety`
- builtin: `dsh-builtin/src`
- chat / tool / skills: `dsh-builtin/src/chatgpt`
- OpenAI client: `dsh-openai/src`

## 検証の最小単位
- `dsh-builtin` を触ったとき: `cargo test -p dsh-builtin`
- `dsh` 本体を触ったとき: `cargo test -p dsh`
- 複数クレートを跨いだときだけ: `cargo test`
- 広いビルド確認が必要なら: `cargo check --workspace`

## Skill 運用
- canonical source は `docs/ai/skills/` に置く。
- runtime 配置先は `~/.codex/skills/` と `~/.config/dsh/skills/`。
- 導入や更新は `scripts/install-runtime-skills.sh` を使う。
- Skill は frontmatter の `description` を短い要約兼トリガー文として書く。
