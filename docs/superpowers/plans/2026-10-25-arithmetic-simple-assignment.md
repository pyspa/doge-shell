# Arithmetic simple assignment implementation plan

**Goal:** Replace an arithmetic target's old text without evaluating it for `=`.
**Spec:** `docs/superpowers/specs/2026-10-25-arithmetic-simple-assignment-design.md`.
**Scope:** doge-shell arithmetic evaluator and its unit/integration/contracts only.

## Task 1: Evidence and regression

- [x] Recheck current remote develop, PRs, CI, clean worktrees and instructions.
  Create a separate feature worktree from current develop, preserve PR #85 work.
- [x] Build current source offline; run isolated `X=abc; echo $((X = 7)); echo "$X"`.
  Expect current failure, compared with success `7\n7\n` for numeric/unset targets.
- [x] Add POSIX contract for overwrite and chained assignment in expansion.toml.
- [x] Add unit cases in arithmetic/eval.rs for old-value matrix, chained/RHS-once,
  compound invalid target, explicit invalid RHS read, failed RHS state, export
  state and compound RHS-before-current ordering. Reuse current parser/host.
- [x] Add integration tests in arithmetic_expansion.rs for overwrite+persist,
  export visibility, chained assignment, fatal explicit/compound reads.
- [x] Run focused new regression, record expected RED before production edits.

## Task 2: Minimal implementation and validation

- [x] Branch on AssignmentOp::Assign after RHS evaluation in eval_expr; use RHS
  directly, preserve resolve/apply for compound and common set_shell_var write.
- [x] Run arithmetic unit/integration tests and shell_contract; confirm exact
  output/state/errors. Correct failures by evidence, never relax assertions.
- [x] Run final doge-shell package tests with isolated HOME/XDG and stdin closed,
  offline dependencies; use already-supported runtime permissions for subprocess
  tests. Report baseline/infrastructure failures separately if encountered.
- [x] Run fmt, package all-target Clippy -D warnings, portability/runtime-authority/
  file-budget checks and diff check. No workspace scope increase without cause.
- [x] Independent review against spec, plan and final diff; address material
  findings with regression evidence, rerun only checks affected by changes.

## Task 3: Authorized publication

- [ ] Recheck remote develop and open PRs, review branch diff and author/committer.
  Commit as mopemope, push only feature branch without force, verify remote SHA.
- [ ] Create a separate Japanese draft PR targeting develop, attach/report URL.
- [ ] Verify CI against exact PR head through terminal state; fix introduced
  failures in scope. Keep PR draft; never merge or update develop directly.
- [ ] Confirm original checkout, prior PR #85 worktree and new worktree states.

## Self-review and limits

The first regression must fail on the rebuilt current source. A newly merged fix
or contradicting reproduction invalidates the plan. Keep old-value validation for
all actual reads. Do not alter side-effect ordering or introduce transactionality.
No live services or dependency installation needed locally. Runtime failure
injection and unrelated arithmetic extensions remain out of scope. Store logs and
resumption notes outside tracked source; report actual checks, not proposed checks.

## Verification outcome

Source-built reproduction and RED regression confirmed before the fix. Final
arithmetic unit tests: 21 passed; arithmetic integration: 39 passed; contracts:
28 passed. Full doge-shell package: library 2367 passed / 8 ignored and all
integration tests passed. All-target Clippy -D warnings, fmt, portability,
runtime authority, file budget and diff checks passed. Independent review found
no production defect. The initial export test used sh -c and hit the existing
confirmation guard (status 130); changing only the probe to printenv retained
exact output/status assertions and passed final tests. Optional extra compound
error-state tests were deferred because their unchanged path is outside this fix.
