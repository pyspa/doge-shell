# Signed arithmetic variable radix plan

1. Revalidate develop/PRs/CI and instructions; isolate worktree, preserve previous
   work. Inspect parser/evaluator, shell grammar and existing arithmetic contracts.
2. Add signed variable public POSIX contract, evaluator cases and integration
   coverage for radix signs, invalid digits, i64 bounds, assignments/compounds and
   error state. Compile current source and record RED before implementation.
3. Strip optional sign only for radix/digit selection; use signed from_str_radix
   for i64 conversion. Preserve unsigned parsing, unary AST, strict integer-only
   variables, checked math, assignment order, environment/export authority.
4. Focused arithmetic lib/integration and shell_contract GREEN; full doge-shell
   package offline with isolated HOME/XDG/TMPDIR and no provider credentials;
   fmt, all-target Clippy -D warnings, portability/runtime-authority/file-budget.
5. Independent full-diff review; fix material findings with regression evidence.
6. Refresh remote/PRs, commit as mopemope author+committer, push feature branch,
   verify remote SHA/identity, create separate Japanese draft PR to develop.
7. Verify exact-head Linux/macOS CI through terminal state, fix scope-related
   failures, retain initial failures/retries, keep draft/no merge, confirm all
   original and prior worktrees preserved and new branch clean.

Self-review: MIN requires signed-magnitude conversion, not checked_neg on an
already overflowing magnitude. Test numeric read failure leaves compound target
unchanged; simple assignment still ignores old invalid text. No dependency,
settings, completion, general expression or unrelated CI changes.

Verification: RED -010 read -10 instead of -8. Final arithmetic unit22, integration40, shell_contract28, full package library2372 passed/8 ignored and all integrations passed. Clippy/fmt/static/diff passed. Independent review no must-fix. No verification retry. Publication/CI next.
