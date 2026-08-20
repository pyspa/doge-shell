# Read Boundaries

- Start with `rg --files` or `rg -n`; do not open `README.md` or broad directories first.
- Open `README.md` only when the task depends on user-facing behavior, config examples, installation guidance, or public docs updates.
- Read `module-map.md` only when crate ownership is unclear after targeted `rg`.
- Read `package-map.md` before choosing `cargo test -p ...` when the directory name may differ from the package name.
- Run `cargo test` for the whole workspace only when the change clearly crosses crate boundaries.
- Prefer `cargo check --workspace` over `cargo test` when you only need a broad compile confirmation.
- For investigation or review tasks, avoid editing and avoid broad validation until the likely files are narrowed down.
- 1000 行を超えるファイルを読む前に `grep -n '^#\[cfg(test)\]' <file>` を 1 回打ち、返った行番号を境界にして offset/limit を決める。`completion/dynamic.rs` は 6927 行中 1833 行、`completion/integrated.rs` は 5108 行中 2516 行がテスト。
- ただし `#[cfg(test)] mod tests;` は**宣言**でテスト本体は別ファイル（`dsh/src/prompt/mod.rs` が該当し、1174 行すべてが実装）。`#[cfg(test)] use ...` はただの import。行番号を見ずに「テストだから」と読み飛ばさない。
- `completions/` は 483 個の JSON（2.1MB）。全文検索するときは `rg --glob '!completions/**'` で除外し、特定コマンドの定義が要るときだけ `completions/<command>.json` を開く。
