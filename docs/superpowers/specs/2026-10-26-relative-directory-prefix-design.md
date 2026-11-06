# Relative directory prefix design

A strict ghost path prefix ending in / must list that directory, including a bare
relative directory such as dir/. Path::parent(dir/) is empty: the current code
checks that before checking the final separator and lists cwd instead. Verify this
with source-built regression tests before modifying production.

Select the input directory first when input ends in MAIN_SEPARATOR. Otherwise
select a nonempty parent, or cwd for a bare partial name. Use the existing directory
listing/cache and preserve source spelling, strict matching, only_dirs, sorting
and shell-token quote handling. No changes to fuzzy completion, provider routing,
cache authority, parser, terminal handling or unrelated settings.

Tests run in a child test process with an isolated cwd (no global cwd mutation in
the parallel test runner). Assert exact completions for dir/ and ./dir/, nested,
absolute, partial file prefix, empty/missing directory, directory-only, quoted and
escaped spaces. Repeat requests to exercise cache. Public shell-token entry must
return an append-only continuation. Build/test against current develop for RED.

Self-review: checking only nonempty parents would miss the actual defect. Moving
the separator check first also treats root and ./ consistently. Retain strict
file/dir distinction and do not change the fuzzy function's separate policy.
Linux/macOS must both run the same regressions in CI.
