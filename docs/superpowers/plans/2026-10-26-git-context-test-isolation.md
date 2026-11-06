# Git context test isolation plan

1. Refresh develop/#90/open PRs/CI/instructions and create isolated worktree.
2. Build existing tests and reproduce no-marker/history/prompt failures under an
   owned ancestor .git + TMPDIR. Preserve source until RED evidence recorded.
3. Extract private predicate-driven marker walk, private history lookup boundary,
   and prompt command fallback with identical production flow. Rewrite the three
   host-dependent tests against deterministic boundaries. Retain filesystem tests
   and add explicit ancestor/nearest public integration cases.
4. Run targeted cases under both normal and marker-bearing TMPDIR, then relevant
   context/prompt tests. Full doge-shell package with isolated HOME/XDG/TMPDIR,
   offline deps and no credentials; fmt/all-target clippy/static/diff checks.
5. Independent review; address material issues with regressions. Recheck latest
   develop/PRs; commit author+committer mopemope, push only feature branch, verify
   remote SHA; create Japanese draft PR to develop and attach URL.
6. Verify exact-head Linux/macOS CI terminal state, report initial failures/retries,
   preserve all worktrees and keep draft/no merge.

Self-review: no broad environmental policy in product code and no tautological
expected values. Assert both fallback and marker priority. No test skips, sleeps,
settings or dependency installation. No unrelated completion/CI fixes.

Results: existing3 tests RED under owned ancestor marker; final targeted9 pass with and without ancestor marker. Full doge-shell library2374 passed/8 ignored and all integrations passed. Independent no must-fix; prompt absent-marker-to-command connection statically reviewed and boundaries tested separately. Clippy/static results checked before publication.
