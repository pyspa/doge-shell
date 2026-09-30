# Read Boundaries

- Start with `rg --files` or `rg -n`; do not open `README.md` or broad directories first.
- Open `README.md` only when the task depends on user-facing behavior, config examples, installation guidance, or public docs updates.
- `README.md` は約 2000 行。開くときは先に見出し索引を取り、該当範囲だけ offset 指定で読む: `rg -n '^#{1,3} ' README.md`（コードブロック内の `#` コメント行も拾うので、絵文字付きの `## ` 見出しを目安にする）。builtin 一覧・Lisp 関数・`config.lisp`・MCP 設定・AI 連携はそれぞれ独立した `##` 節にある。
- Read `module-map.md` only when crate ownership is unclear after targeted `rg`.
- Read `package-map.md` before choosing `cargo test -p ...` when the directory name may differ from the package name.
- Run `cargo test` for the whole workspace only when the change clearly crosses crate boundaries.
- Prefer `cargo check --workspace` over `cargo test` when you only need a broad compile confirmation.
- For investigation or review tasks, avoid editing and avoid broad validation until the likely files are narrowed down.
- refactor の分割シリーズ（`git log --oneline | rg 'refactor.*split'`）で、テストは各モジュールから
  `<module>/tests.rs` に分離済み。1000 行を超える `.rs` は今のところ**全部**そうした `*/tests.rs`
  で、非テストの本体側は 1000 行を超えない（超えたら `scripts/check-file-budget.py` の 800 行ルールに
  先に引っかかる）。読む前に `grep -n '^#\[cfg(test)\]' <file>` を打てば、その行がまだ本体内に
  テストを抱えているか（`mod tests { ... }` が続く）、単なる宣言（`mod tests;` で本体は別ファイル、
  例: `dsh/src/completion/dynamic.rs` → `dynamic/tests.rs`）かが分かる。宣言だけなら、その行以降は
  offset/limit で読み飛ばしてよい。
- `#[cfg(test)] use ...` はただの import で、モジュール宣言ではない。行番号を見ずに「テストだから」と
  読み飛ばさない。
- `completions/` は 500 個超の JSON（2MB 台）。全文検索するときは `rg --glob '!completions/**'` で
  除外し、特定コマンドの定義が要るときだけ `completions/<command>.json` を開く。正確な件数・サイズは
  `ls completions/*.json | wc -l` / `du -sh completions/` で確認する（この文書には焼き付けない — 数
  自体が動く）。
