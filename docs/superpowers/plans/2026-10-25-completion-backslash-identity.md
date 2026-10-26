# Completion backslash identity implementation plan

**Outcome:** Retain distinct literal names through both completion dedup paths.
**Spec:** `docs/superpowers/specs/2026-10-25-completion-backslash-identity-design.md`.
**Scope:** two key-normalization predicates and focused tests; no ghost fix.

## Task 1: Current evidence and RED

- [x] Refresh develop/PRs/CI; inspect AGENTS, repository and completion skills,
  invariants, candidate/raw-word authority and validation/platform guidance.
- [x] Create isolated fix-completion-backslash-identity worktree from latest
  develop without updating the original checkout or prior #85/#86 worktrees.
- [x] Add legacy regression using actual temporary file and directory names
  with zero/one/two trailing backslashes; assert raw collection before dedup.
- [x] Add legacy preference regression so a command wins only for the same exact
  basename, while a different backslash-suffixed name survives.
- [x] Add integrated regression matrix for file/directory/home/argument types,
  exact duplicates and ranking; pin slash alias behavior for backslash names.
- [x] Add public engine completion test for real files/directories, repeat/cached
  requests and unquoted/double-quoted insertion preserving backslashes.
- [x] Compile current production source and run new regressions; record expected
  missing candidates (RED), not build errors, before production changes.

## Task 2: Fix, GREEN and review

- [x] Change only trim_end_matches predicates from slash/backslash to slash in
  commands.rs and integrated/finalize.rs; explain literal-backslash ownership.
- [x] Run new tests GREEN, then completion library tests. Existing separator,
  priority, type, cache, quoting, spaces and Unicode regressions must remain green.
- [x] Run final full doge-shell package tests offline, isolated HOME/XDG and no
  provider credentials; retain existing subprocess permissions and stdin isolation.
- [x] Run package all-target Clippy -D warnings, fmt, portability, runtime authority,
  file budget and diff checks. No dependency installs or config changes.
- [x] Independent code review against whole diff/design/plan. Resolve material
  findings with failing regressions and rerun affected checks.

## Task 3: Publication and verification

- [ ] Recheck remote develop/open PRs, branch diff and mopemope author/committer.
  Commit and push only feature branch, verify remote SHA and GitHub identities.
- [ ] Create separate Japanese draft PR targeting develop; attach and report URL.
- [ ] Follow exact-head Linux/macOS CI to terminal result; fix introduced failures
  within scope, never merge or update develop directly.
- [ ] Confirm all original/prior worktrees preserved and new worktree clean.

## Self-review

Both merge paths must be tested: fixing only integrated completion leaves legacy
command/file fallback wrong. Real filesystem collection proves these are legal
names, not synthetic impossible candidates. Current slash aliases and ranking are
compatibility controls. Quoting belongs after dedup and must not be changed here.
No syntax/grammar change requires a shell contract addition; public completion and
full package tests cover this behavior. Linux-only local execution is not macOS
proof; report CI separately. Logs/resumption notes stay outside tracked source.

## Results

Three source-built regressions failed before the production fix and passed after.
Full package passed (library 2370 passed, 8 ignored; all integrations passed).
Initial restricted completion-only run failed cached_queries_decode_source_once_and_preserve_values
and skill_subcommands_complete_project_skill_names; both passed in the permission-enabled
isolated full run. Exact environment cause not established. Independent review
found no must-fix issue; minor suggestion to assert the full expected name set in
the public completion test deferred. macOS and exact-head CI pending publication.
