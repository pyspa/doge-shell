# Repository Guidelines

## プロジェクト構成
- Rust ワークスペース。主要クレート `dsh` (本体シェル) と補助クレート `dsh-builtin`, `dsh-frecency`, `dsh-types`, `dsh-openai` が `Cargo.toml` に列挙。
- エントリポイント: `dsh/src/main.rs`。REPL・パーサー・補完・Lisp 実装は `dsh/src` 以下の各モジュールに分割。
- 統合テストは `dsh/tests`、ユニットテストは各モジュール内に併設。補完スキーマやサンプルは `completions` / `dynamic-completions` に配置。

## ビルド・実行・テスト
```bash
cargo build                # デバッグビルド
cargo build --release      # 最適化ビルド
cargo run -p dsh -- --help # シェル起動オプションの確認
cargo run -p dsh --        # シェル本体の起動
cargo test                 # ワークスペース全体のテスト
cargo test -p dsh -- --nocapture # dsh クレート限定で出力を表示
cargo fmt                  # `rustfmt.toml` に沿った整形
cargo clippy --all-targets --all-features # 静的解析
```

## コーディングスタイル & 命名
- `rustfmt` 準拠 (4 スペース、Edition 2024)。PR 前に必ず `cargo fmt`。
- 命名: モジュール/関数は snake_case、型は UpperCamelCase、定数は SCREAMING_SNAKE_CASE。
- ロジックが複雑な箇所のみ短いコメントを付与。非同期は Tokio パターンに合わせる。
- 新しい機能は既存ディレクトリ構造に倣い、補完関連は `dsh/src/completion/` 配下に配置。

## テスト指針
- 標準の `cargo test` フレームワークを使用。公開 API やバグ修正には回帰テストを追加。
- テスト名は振る舞いを示す説明的な snake_case で統一 (例: `handles_ctrl_c_signal`)。
- I/O を伴うテストは可能ならモック・一時ディレクトリを利用し、副作用を隔離。

## コミット & PR ガイド
- コミットは Conventional Commits に従う: `<type>(<scope>): <subject>`、命令形・72 文字以内。例: `feat(parser): support here-doc`.
- PR には概要、動機、主要変更点、実行したコマンド (テスト/フォーマット) を記載し、関連 Issue があればリンク。
- 動作確認が必要な変更は再現手順やスクリーンショットを添付。レビュー可能な粒度でコミットを分割。

## セキュリティ・設定の注意
- API キーやトークンをリポジトリに含めない。必要な場合はローカル環境変数や個人設定ファイル (`~/.config/dsh/config.lisp` など) で管理。
- サードパーティ通信を伴う機能 (OpenAI/MCP 等) は、ネットワーク不可環境でもフェイルセーフに振る舞うか確認すること。

<!-- BACKLOG.MD MCP GUIDELINES START -->

<CRITICAL_INSTRUCTION>

## BACKLOG WORKFLOW INSTRUCTIONS

This project uses Backlog.md MCP for all task and project management activities.

**CRITICAL GUIDANCE**

- If your client supports MCP resources, read `backlog://workflow/overview` to understand when and how to use Backlog for this project.
- If your client only supports tools or the above request fails, call `backlog.get_workflow_overview()` tool to load the tool-oriented overview (it lists the matching guide tools).

- **First time working here?** Read the overview resource IMMEDIATELY to learn the workflow
- **Already familiar?** You should have the overview cached ("## Backlog.md Overview (MCP)")
- **When to read it**: BEFORE creating tasks, or when you're unsure whether to track work

These guides cover:
- Decision framework for when to create tasks
- Search-first workflow to avoid duplicates
- Links to detailed guides for task creation, execution, and completion
- MCP tools reference

You MUST read the overview resource to understand the complete workflow. The information is NOT summarized here.

</CRITICAL_INSTRUCTION>

<!-- BACKLOG.MD MCP GUIDELINES END -->
