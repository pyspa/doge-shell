---
name: doge-shell-completion-spec
description: Use when adding or editing doge-shell command completion JSON, a dynamic completion provider, comp-gen output, 補完定義, 補完 JSON, or provider 追加. Covers the completions/ schema, the provider registration path, and the smallest validation.
---

# Doge Shell Completion Spec

このリポジトリで最も頻度の高い作業。定義は `completions/<command>.json` ただ 1 つで、`dsh/src/completion/json_loader.rs` の `#[folder = "../completions/"]` がバイナリへ埋め込む。コピーを別ディレクトリに作らない。

- 既存の近いコマンドを 1 つだけ読んで形を真似る。全文検索するなら `rg --glob '!completions/**'` で除外してから、必要な `completions/<command>.json` を開く。
- 形と制約は [references/schema.md](references/schema.md) を読む。`command-completion-schema.json` を最初から読むより速い。
- dynamic provider を新規に足すときの同時更新箇所は [references/add-dynamic-provider.md](references/add-dynamic-provider.md)。
- provider 名のタイポは実行時に候補 0 件になるだけでログしか出ない。必ず `cargo test -p doge-shell --lib completion::json_loader` で確認する。
- 検証: JSON だけなら `cargo test -p doge-shell --lib completion::json_loader`。provider を足したら `cargo test -p dsh-types` も。`comp-gen --audit completions` は未知/未使用 provider の棚卸しに使う。
- 端末で確かめるなら release を作り直す。JSON の**新規追加**だけでは rust-embed が再ビルドを検知しないので、先に `touch dsh/src/completion/json_loader.rs`。
