# pm add name completion plan

1. Revalidate latest develop/PRs/instructions and raw parser argument ownership;
   create isolated feature worktree, preserve all prior work.
2. Add public engine regressions in integrated/pm_name_tests.rs with actual temp
   directories: plain, spaces, Unicode, quote chars, interior/trailing/repeated
   backslashes; plain/single/double input encoding, slash/no-slash, encoded partial
   name queries and repeated requests. Assert exact raw names, insertion decode,
   negative query and unchanged metadata. Compile current source and record RED.
3. Decode raw path once with existing decode_literal_word, trim / only, derive
   basename. Decode current name query once before matches_prefix. Preserve raw
   candidate text and existing insertion formatting/cache/provider architecture.
4. Run new tests GREEN and existing path/quote/completion controls, then full
   doge-shell package offline under isolated HOME/XDG/TMPDIR without credentials.
   Run fmt, all-target Clippy -D warnings, portability/runtime-authority/file-budget.
5. Independent full-diff review, resolve material findings with tests and affected
   checks. Recheck develop/open PRs, commit mopemope author+committer, push feature
   branch only, verify remote head/identity, create Japanese draft PR to develop.
6. Attach/report PR URL; follow exact-head CI through terminal state, fix introduced
   failures only, report initial failures/retries. Keep draft/no merge and confirm
   original/prior/new worktrees preserved.

Self-review: public candidate names, not only counts; preserve literal spelling,
no double decoding, no live services/install/config changes or unrelated fixes.

Results: source-built public regression RED quoted plain returned plain' instead of plain. Final public matrix GREEN; full package library2375 passed/8 ignored and integrations all passed. All-target Clippy/fmt/static/diff passed. Independent no must-fix. No verification failures/retries after RED. Publication/CI next.
