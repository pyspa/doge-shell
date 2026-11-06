# Relative directory prefix implementation plan

1. Refresh develop/open PRs/CI/instructions, create isolated feature worktree.
2. Add subprocess-isolated regression under completion/path/relative_tests.rs;
   fixture cwd contains dir/child.txt, dir/folder/, nested/deep/leaf, empty/ and
   spaced directories, plus a cwd-only distractor. Assert exact strict and public
   shell-token outputs, directory-only and repeated/cache behavior. Run RED on
   unchanged source before production edits.
3. In path_completion_prefix_strict choose input on trailing separator first;
   otherwise nonempty parent or cwd. Preserve listing/cache/display authority,
   ranking, strict prefix and quoting. No fuzzy or terminal change.
4. Run focused GREEN, related path/quoting tests then full doge-shell package
   offline in isolated HOME/XDG without provider credentials; fmt, package
   all-target Clippy -D warnings, portability/runtime-authority/file-budget/diff.
5. Independent whole-diff review including design and tests; address material
   findings with evidence and affected verification. Report limitations honestly.
6. Refresh remote develop/PRs; commit as mopemope author+committer; push only feature
   branch, verify remote SHA and identities. Create separate Japanese draft PR to
   develop, attach/report URL. Follow exact-head Linux/macOS CI to terminal status,
   correct introduced failures, keep draft and never merge.
7. Confirm original and #85-#87 worktrees preserved, new branch clean.

Self-review: subprocess isolation is necessary because cwd is process-wide. Test
fixtures must distinguish parent listing from child listing and assert names, not
only counts. No installs/live services. Existing tests remain acceptance controls.
Do not pin future implementation to a fixed base revision.

Validation: RED confirmed dir/ None vs child. Final path6 passed, package library2371 passed/8 ignored and all integrations passed; clippy/fmt/static passed. Initial PTY diagnostic failure passed standalone. Three root-context failures saw shared /tmp/.git; dedicated TMPDIR full run passed. Independent review no must-fix; direct empty-string strict display compatibility test deferred. Publication/CI next.
