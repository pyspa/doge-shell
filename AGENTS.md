# Agent Guide

このリポジトリでは、トークン消費を抑えるために「最小探索・最小検証」を徹底すること。

## 基本方針
- チャットは日本語で行う。
- 対応 OS は Linux と macOS の 2 つ。片方でしか動かない変更を入れない。`/proc` や `/etc/passwd` のような OS 固有のソースを読むときは、必ずもう一方の OS の腕も書く。
- 補助スクリプトは shell / Python のどちらを使ってもよい。repo-tracked な生成・整形は、目的が明確なときだけ行う。
- `Cargo.toml` と必要なら task map で範囲を絞り、`rg --files` / `rg -n` で当たりを付けてから必要なファイルだけ読む。
- 該当する Skill がある場合は先に使い、詳細は必要になってから `references/` を読む。
- `README.md` 全文を最初から読まない。ユーザー向け挙動、設定例、公開文書の更新時だけ必要箇所を開く。
- 変更後は関係する最小コマンドで検証し、無関係なワークスペース全体テストは最後に限定する。

## 作業タイプ別の最初の一手
- 実装修正: `docs/ai/skills/doge-shell-repo/references/task-map.md` で入口と検証候補を確認する。
- 検証選定: `doctor validate` が使える環境では提案を優先し、なければ `docs/ai/skills/doge-shell-repo/references/test-scope.md` で選ぶ。
- AI guidance / Skill 変更: `scripts/check-ai-guidance.sh` と runtime Skill の `--status` を使う。
- 失敗例の再発防止: 修正後に `task-map.md` か該当 Skill の `references/` へ短く戻す。

## 探索順
1. `Cargo.toml` でクレート境界を確認する。
2. タスク種別が明確なら `docs/ai/skills/doge-shell-repo/references/task-map.md` で入口と検証候補を確認する。
3. `rg -n "<symbol>|<feature>" dsh dsh-builtin dsh-openai dsh-types` で実装位置を絞る。
4. package 名が曖昧なら `docs/ai/skills/doge-shell-repo/references/package-map.md` を読む。
5. 所有範囲が曖昧なら `docs/ai/skills/doge-shell-repo/references/module-map.md` を読む。

## 検証の最小単位
- `doctor validate` が使える環境では変更ファイルに応じた候補を確認する。
- `dsh-builtin` を触ったとき: `cargo test -p dsh-builtin`
- `dsh` 本体を触ったとき: `cargo test -p doge-shell`
- 複数クレートを跨いだときだけ: `cargo test`
- 広いビルド確認が必要なら: `cargo check --workspace`
- OS 依存のコード・テスト・ビルド設定を触ったとき: `scripts/check-portability.py`
- 段階的な設計変更の完了時とリリース前: `./scripts/check.sh`

## 設計境界
- 動的補完 provider は `dsh/src/completion/dynamic/registry.rs` の `DynamicProviderId` へ一度だけ登録し、family collector と `CachePolicy` 経路を使う。cached 専用 dispatch を増やさない。
- `ShellProxy` は互換レイヤーとして固定し、新規メソッドを追加しない。builtin の新しい依存は `dsh-builtin/src/shell_capabilities.rs` の能力 trait へ追加する。
- `ShellProxy` または能力 trait を変更したら `scripts/check-shell-proxy-capabilities.py` を実行する。
- プラットフォーム分岐は `#[cfg(not(target_os = "macos"))]` と `#[cfg(target_os = "macos")]` の対で書き、共通ロジックは cfg の外の純粋関数に置く（`dsh/src/completion/generators/user.rs`）。片方だけ書くと、もう一方の OS ではその項目が消えるだけでコンパイルもテストも通る。
- OS ごとに違う定数表には `libc` と突き合わせるテストを付ける（`dsh/src/completion/generators/signal.rs`）。`cargo clippy` はそのホストの腕しか見ないので、macOS 側の実証は CI の macos ジョブだけ。

## 参照の使い分け
- `task-map.md`: タスクごとの最初の読みに行く先と最小検証を決める。
- `package-map.md`: ディレクトリ名と Cargo package 名のズレを避ける。
- `module-map.md`: crate や主要ディレクトリの ownership を確認する。
- `read-boundaries.md`: README や workspace 全体 test を開く条件を確認する。
- `invariants.md`: cwd 変更、`Environment` の状態、キー入力、端末描画、出力履歴、スケジューラを触る前に読む。
- `platform-support.md`: 対応 OS、プラットフォーム分岐の書き方、macOS 側の腕を Linux ホストで確認する手順。

## Skill 運用
- canonical source は `docs/ai/skills/` に置く。
- runtime 配置先は `~/.codex/skills/` と `~/.config/dsh/skills/`。
- 導入や更新は `scripts/install-runtime-skills.sh` を使う。
- 普段は必要な skill だけ install する。引数なしの全件 install は初期セットアップ時だけ使う。
- Codex runtime へ常時入れる Skill は原則 `doge-shell-repo` のみにし、領域別 Skill は `docs/ai/skills/<skill>/SKILL.md` を必要時に読む。
- Skill は frontmatter の `description` を短い要約兼トリガー文として書く。
- `AGENTS.md` / `docs/ai/` / Skill を変更したら `scripts/check-ai-guidance.sh` を実行する。
