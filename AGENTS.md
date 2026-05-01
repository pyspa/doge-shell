# Agent Guide

このリポジトリでは、トークン消費を抑えるために「最小探索・最小検証」を徹底すること。

## 基本方針
- チャットは日本語で行う。
- 補助スクリプトは shell / Python のどちらを使ってもよい。
- `Cargo.toml` と必要なら task map で範囲を絞り、`rg --files` / `rg -n` で当たりを付けてから必要なファイルだけ読む。
- 該当する Skill がある場合は先に使い、詳細は必要になってから `references/` を読む。
- `README.md` 全文を最初から読まない。ユーザー向け挙動、設定例、公開文書の更新時だけ必要箇所を開く。
- 変更後は関係する最小コマンドで検証し、無関係なワークスペース全体テストは最後に限定する。

## 探索順
1. `Cargo.toml` でクレート境界を確認する。
2. タスク種別が明確なら `docs/ai/skills/doge-shell-repo/references/task-map.md` で入口と検証候補を確認する。
3. `rg -n "<symbol>|<feature>" dsh dsh-builtin dsh-openai dsh-types` で実装位置を絞る。
4. package 名が曖昧なら `docs/ai/skills/doge-shell-repo/references/package-map.md` を読む。
5. 所有範囲が曖昧なら `docs/ai/skills/doge-shell-repo/references/module-map.md` を読む。

## 検証の最小単位
- `dsh-builtin` を触ったとき: `cargo test -p dsh-builtin`
- `dsh` 本体を触ったとき: `cargo test -p doge-shell`
- 複数クレートを跨いだときだけ: `cargo test`
- 広いビルド確認が必要なら: `cargo check --workspace`

## 参照の使い分け
- `task-map.md`: タスクごとの最初の読みに行く先と最小検証を決める。
- `package-map.md`: ディレクトリ名と Cargo package 名のズレを避ける。
- `module-map.md`: crate や主要ディレクトリの ownership を確認する。
- `read-boundaries.md`: README や workspace 全体 test を開く条件を確認する。

## Skill 運用
- canonical source は `docs/ai/skills/` に置く。
- runtime 配置先は `~/.codex/skills/` と `~/.config/dsh/skills/`。
- 導入や更新は `scripts/install-runtime-skills.sh` を使う。
- 普段は必要な skill だけ install する。引数なしの全件 install は初期セットアップ時だけ使う。
- Skill は frontmatter の `description` を短い要約兼トリガー文として書く。
- `AGENTS.md` / `docs/ai/` / Skill を変更したら `scripts/check-ai-guidance.sh` を実行する。
