# Arithmetic simple assignment design

## Goal and scope

A simple arithmetic assignment `X = rhs` writes and returns the numeric RHS
without converting the previous value of X. Preserve the existing integer-only
variable-read policy, compound operations, error category, evaluation order,
short-circuiting, chained assignment, and logical environment/export authority.
No completion, capture, parser, dependency, or configuration changes.

## Current evidence and cause

Latest remote develop includes PR #85; no open PR covers arithmetic assignment.
`eval_expr` evaluates RHS, then unconditionally calls `resolve_variable` on the
target before `apply_assignment_op`; Assign discards that parsed current value.
Thus an irrelevant invalid old value aborts assignment. A fresh offline source
build reproduced status 1/empty stdout for `X=abc; echo $((X = 7)); echo "$X"`;
the numeric-target control and bash produced status 0 and `7\n7\n`. The focused
regression failed with actual status 1 versus expected 0 before production edits.
Bash accepts nonnumeric compound operands differently; that is not adopted here.

## Decision

After target validation and the existing single RHS evaluation, branch on Assign:
use RHS directly. Other operators retain target resolution followed by the existing
checked arithmetic helper. All paths retain the common `set_shell_var` write and
return. This keeps mutation/export/cache authority and ordering unchanged.
Do not make nonnumeric variables silently zero, modify grammar, evaluate RHS twice,
or add special cases to variable storage. Do not reorder compound assignment.

## Acceptance and review focus

- Overwrite nonnumeric, invalid-octal, expression-text and out-of-range old values;
  unset/empty/numeric targets still work; yield and persisted value match RHS.
- Chained assignments overwrite both targets; RHS side effects occur exactly once.
- Reading invalid X explicitly on RHS still fails; every compound operator still
  rejects invalid old X. Failed RHS leaves outer target unchanged; prior RHS side
  effects are not rolled back (existing semantics).
- Preserve RHS-before-current behavior of compound assignment, checked errors,
  lazy skipped branches, and exported/unexported state.
- End-to-end tests assert exit status, exact stdout and empty stderr on success.
- Linux and macOS via CI, with no new platform-specific code.

## Self-review

The minimal branch is preferable to passing a dummy current value to the helper:
it makes the absence of a read explicit. The helper's Assign arm may remain for
exhaustive enum matching; no enum or parser refactor is warranted. Protect against
an overly broad fix with negative tests for compound/RHS reads, and assert state
after errors at unit level. Tests of shell output alone cannot observe that state.
No unresolved decision requires user input within the authorized scope.
