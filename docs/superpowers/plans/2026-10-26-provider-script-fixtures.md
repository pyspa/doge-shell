# Provider capture fixture implementation plan

1. Refresh latest develop/PRs/CI/instructions; inspect exact prior failure and
   existing interpreter fixture patterns. Isolate a new feature worktree.
2. Extract capture_script_path with unchanged direct-exec body. Add deterministic
   test: create script with known stdout/stderr and exit7, hold writable File open,
   call helper and assert status/bytes. Compile current source and observe RED.
3. Change only test helper program to /bin/sh and pass script path in argv, no -c.
   Keep cwd/timeout/env/capture behavior. Do not change production or other fixture
   helpers. Run new regression and full task::provider_tests GREEN.
4. Run full dsh-builtin package offline under isolated HOME/XDG/TMPDIR without
   provider credentials; fmt, all-target Clippy -D warnings, portability,
   runtime-authority/file-budget/diff. Report every failure and actual retries.
5. Independent review of full diff, design and tests; resolve material findings.
   Self-review boundary: stream-capture helper only, no admission-test weakening.
6. Recheck develop/PRs, commit with mopemope author/committer, push only new branch,
   verify remote SHA and identity; create Japanese draft PR to develop, attach URL.
7. Follow exact-head Linux/macOS CI to terminal state, address introduced failures,
   keep draft/no merge, verify original and #85-#88 worktrees preserved.

No toolchain/dependency install, settings changes, broad flake cleanup or unrelated
completion edits. No retries/sleeps introduced in tests. Logs remain outside git.

Results: deterministic RED Text file busy confirmed. Initial GREEN build E0308 fixed by borrowing script argv as &str. Final provider16, builtin1266 passed/2 ignored; all-target clippy/fmt/portability/runtime-authority/file-budget/diff passed. Independent review identified the same compile issue, no semantic must-fix; corrected before final validation. Publish/CI pending.
